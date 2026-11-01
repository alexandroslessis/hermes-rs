//! Hermes's `delegate_task` (`tools/delegate_tool*.py`, MIT) in its joined mode: each task gets
//! a subagent with its own conversation, terminal session and step budget; they run in parallel
//! and only their final summaries return, in this tool call. Children run the same turn pipeline
//! as the parent, with the parent's prompt (no memory) plus Hermes's subagent block, and can't
//! delegate, ask the user or write memory. Dangerous commands are denied (nobody can approve them).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use hermes_agent::events::AgentEvent;
use hermes_agent::inference::InferenceRunner;
use hermes_agent::machine::{StateMachine, Step};
use hermes_agent::operation::Emitter;
use hermes_agent::tool::{ToolOperation, ToolProvider};
use hermes_llm::base::Provider;
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::model::ModelConfig;
use hermes_store::Store;
use hermes_tools::{
    ClarifyOutcome, ClarifyRequest, ProcessNotice, SessionTools, UserBridge,
    approval::{ApprovalChoice, CommandApproval},
};
use parking_lot::Mutex;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Role, Tool};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::compress;
use crate::ops;
use crate::session::{HermesEffect, OpenSession, TurnRuntime, TurnSession};

/// `delegation.max_concurrent_children`.
pub const MAX_CONCURRENT_CHILDREN: usize = 10;
/// `delegation.max_iterations`: each child's own budget.
const CHILD_MAX_ITERATIONS: u32 = 250;
/// `DEFAULT_MAX_SUMMARY_CHARS`.
const MAX_SUMMARY_CHARS: usize = 24_000;
/// `DELEGATE_BLOCKED_TOOLS` that exist here.
pub const BLOCKED_TOOLS: &[&str] = &["delegate_task", "clarify", "memory"];

/// `_COMPLETION_INSTRUCTIONS`.
const COMPLETION_INSTRUCTIONS: &str = "\nComplete this task using the tools available to you. When finished, provide a clear, concise summary of:\n- What you did\n- What you found or accomplished\n- Any files you created or modified\n- Any issues encountered\n\nImportant workspace rule: Never assume a repository lives at /workspace/... or any other container-style path unless the task/context explicitly gives that path. If no exact local path is provided, discover it first before issuing git/workdir-specific commands.\n\nKeep your final summary tight: lead with outcomes, prefer bullet points over paragraphs, and don't replay your whole process. Your response is returned to the parent agent as a summary, and overlong summaries crowd out the parent's context window.";

/// `_build_child_system_prompt` (leaf role). The project context files are already in the base
/// prompt the child gets, so they aren't repeated here.
pub fn child_prompt_block(context: Option<&str>, workspace: &Path) -> String {
    let mut parts =
        vec!["You are a focused subagent working on a specific delegated task.".to_string()];
    if let Some(context) = context.filter(|c| !c.trim().is_empty()) {
        parts.push(format!("\nCONTEXT:\n{context}"));
    }
    parts.push(format!(
        "\nWORKSPACE PATH:\n{}\nUse this exact path for local repository/workdir operations unless the task explicitly says otherwise.",
        hermes_tools::display_path(workspace)
    ));
    parts.push(COMPLETION_INSTRUCTIONS.to_string());
    parts.join("\n")
}

/// Builds a child's own tools: its session id, working folder and scratch folder.
pub type ChildTools = Arc<dyn Fn(&str, &Path, &Path) -> SessionTools + Send + Sync>;
/// The parent's system prompt without memory (the base of every child's prompt).
pub type BasePrompt = Arc<dyn Fn() -> Result<String> + Send + Sync>;

/// Everything a turn needs to start subagents.
pub struct Delegator {
    pub parent: Arc<OpenSession>,
    pub store: Arc<Store>,
    pub llm: Arc<dyn Provider>,
    pub model: String,
    pub context_length: Option<usize>,
    pub effort_params: Option<HashMap<String, Value>>,
    pub supports_vision: bool,
    pub base_prompt: BasePrompt,
    pub child_tools: ChildTools,
    /// MCP servers' tools, offered to the parent and (as in Hermes) inherited by children.
    pub mcp: Arc<crate::mcp::McpManager>,
}

/// The parent's tools plus `delegate_task`.
pub struct DelegatingTools {
    pub inner: Arc<SessionTools>,
    pub delegator: Arc<Delegator>,
}

#[async_trait]
impl ToolProvider<TurnSession> for DelegatingTools {
    async fn tools(&self, session: &TurnSession) -> Result<Vec<Tool>> {
        let mut tools = self.inner.tools(session).await?;
        tools.extend(self.delegator.mcp.tools());
        Ok(tools)
    }

    async fn call(
        &self,
        session: &TurnSession,
        request_id: &str,
        call: CallToolRequestParams,
        emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        if call.name.starts_with("mcp__") {
            let arguments = call.arguments.unwrap_or_default();
            return Ok(self.delegator.mcp.call(&call.name, arguments).await);
        }
        if call.name != "delegate_task" {
            return self.inner.call(session, request_id, call, emit).await;
        }
        let args = Value::Object(call.arguments.unwrap_or_default());
        let result = self.delegator.delegate(&args, request_id, emit).await;
        let failed = result.get("success") == Some(&Value::Bool(false));
        let text = serde_json::to_string(&result).unwrap_or_default();
        Ok(if failed {
            CallToolResult::error(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(text)])
        })
    }
}

/// A child's tools: the session's own, minus the ones subagents can't use.
struct ChildToolProvider {
    inner: Arc<SessionTools>,
    mcp: Arc<crate::mcp::McpManager>,
}

#[async_trait]
impl ToolProvider<TurnSession> for ChildToolProvider {
    async fn tools(&self, session: &TurnSession) -> Result<Vec<Tool>> {
        let tools = self.inner.tools(session).await?;
        Ok(tools
            .into_iter()
            .filter(|tool| !BLOCKED_TOOLS.contains(&tool.name.as_ref()))
            .chain(self.mcp.tools())
            .collect())
    }

    async fn call(
        &self,
        session: &TurnSession,
        request_id: &str,
        call: CallToolRequestParams,
        emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        if call.name.starts_with("mcp__") {
            let arguments = call.arguments.unwrap_or_default();
            return Ok(self.mcp.call(&call.name, arguments).await);
        }
        if BLOCKED_TOOLS.contains(&call.name.as_ref()) {
            let text = json!({"success": false, "error": format!("{} is not available to subagents.", call.name)});
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                text.to_string(),
            )]));
        }
        self.inner.call(session, request_id, call, emit).await
    }
}

/// What a child can ask of the user: nothing. Questions go unanswered and dangerous commands are
/// denied (Hermes's default for subagents).
pub struct ChildBridge;

#[async_trait]
impl UserBridge for ChildBridge {
    async fn clarify(&self, _session_id: &str, _request: ClarifyRequest) -> ClarifyOutcome {
        ClarifyOutcome::Undelivered
    }

    fn process_finished(&self, _session_id: &str, _notice: ProcessNotice) {}

    async fn approve_command(
        &self,
        _session_id: &str,
        _request: CommandApproval,
    ) -> ApprovalChoice {
        ApprovalChoice::Deny
    }
}

/// A short label for a child's tool call in the progress line.
fn progress_line(name: &str, args: &serde_json::Map<String, Value>) -> String {
    let detail = [
        "command",
        "path",
        "query",
        "pattern",
        "urls",
        "image_url",
        "name",
    ]
    .iter()
    .find_map(|key| args.get(*key))
    .map(|value| match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
    .unwrap_or_default();
    let detail: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(80).collect();
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {detail}")
    }
}

fn capped(summary: &str) -> String {
    let total = summary.chars().count();
    if total <= MAX_SUMMARY_CHARS {
        return summary.to_string();
    }
    let head: String = summary.chars().take(MAX_SUMMARY_CHARS * 3 / 4).collect();
    let tail: String = summary
        .chars()
        .skip(total - MAX_SUMMARY_CHARS / 4)
        .collect();
    format!(
        "{head}\n\n[... {} chars of the summary omitted ...]\n\n{tail}",
        total - MAX_SUMMARY_CHARS
    )
}

impl Delegator {
    /// `delegate_task`: validates the tasks, runs one child per task in parallel, and returns
    /// `{"results": [...], "total_duration_seconds"}`.
    pub async fn delegate(&self, args: &Value, request_id: &str, emit: &Emitter) -> Value {
        // The legacy single-goal shape is accepted as a one-task batch.
        let tasks: Vec<Value> = match args.get("tasks") {
            Some(Value::Array(tasks)) => tasks.clone(),
            _ if args.get("goal").is_some() => vec![args.clone()],
            _ => {
                return json!({"success": false, "error": "tasks is required: a list of {goal, context} objects."});
            }
        };
        if tasks.is_empty() {
            return json!({"success": false, "error": "tasks must contain at least one task."});
        }
        if tasks.len() > MAX_CONCURRENT_CHILDREN {
            return json!({"success": false, "error": format!(
                "Too many tasks ({}): at most {MAX_CONCURRENT_CHILDREN} subagents run in parallel. Split the work into smaller batches.",
                tasks.len()
            )});
        }
        let mut specs = Vec::new();
        for (index, task) in tasks.iter().enumerate() {
            let goal = task
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if goal.is_empty() {
                return json!({"success": false, "error": format!("Task {index} has no goal.")});
            }
            let context = task
                .get("context")
                .and_then(Value::as_str)
                .map(str::to_string);
            specs.push((index, goal.to_string(), context));
        }
        let base_prompt = match (self.base_prompt)() {
            Ok(prompt) => prompt,
            Err(error) => return json!({"success": false, "error": format!("{error:#}")}),
        };
        let started = Instant::now();
        let runs = specs.into_iter().map(|(index, goal, context)| {
            let base_prompt = base_prompt.clone();
            async move {
                self.run_child(
                    index,
                    &goal,
                    context.as_deref(),
                    &base_prompt,
                    request_id,
                    emit,
                )
                .await
            }
        });
        let results = futures::future::join_all(runs).await;
        json!({
            "results": results,
            "total_duration_seconds": (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        })
    }

    async fn run_child(
        &self,
        index: usize,
        goal: &str,
        context: Option<&str>,
        base_prompt: &str,
        request_id: &str,
        parent_emit: &Emitter,
    ) -> Value {
        let started = Instant::now();
        let subagent_id = format!(
            "sa-{index}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let parent_id = self.parent.record.lock().id.clone();
        let workspace = self.parent.tools.cwd();
        let scratch = self
            .parent
            .tools
            .scratch_dir()
            .join("subagents")
            .join(&subagent_id);
        let tools = Arc::new((self.child_tools)(
            &format!("{parent_id}:{subagent_id}"),
            &workspace,
            &scratch,
        ));
        tools.begin_turn();
        tools.set_vision(self.supports_vision);
        let mut record = self.parent.record.lock().clone();
        record.system_prompt = format!(
            "{base_prompt}\n\n{}",
            child_prompt_block(context, &workspace)
        );
        let child = Arc::new(OpenSession {
            record: Mutex::new(record),
            conversation: Mutex::new(Conversation::new_unvalidated(vec![
                Message::user()
                    .with_text(goal)
                    .with_generated_id_if_missing(),
            ])),
            tools: tools.clone(),
            running: Mutex::new(None),
            turn_state: Mutex::new(None),
            review: Mutex::new(None),
            nudges: Mutex::new(Default::default()),
            context: Mutex::new(None),
        });
        let state: ops::SharedTurnState = Arc::default();
        // Usage counts toward the parent session (Hermes rolls children's cost up).
        let runtime = TurnRuntime {
            session: child.clone(),
            store: self.store.clone(),
            state: state.clone(),
            persist: false,
            record_usage: true,
        };
        let cancel = parent_emit.cancel_token().child_token();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(events_tx, cancel.clone());
        // The child's tool calls become progress lines under the parent's delegate_task card.
        let progress_emit = parent_emit.clone();
        let progress_id = request_id.to_string();
        let relay = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if let AgentEvent::Message(message) = event {
                    for request in message
                        .content
                        .iter()
                        .filter_map(MessageContent::as_tool_request)
                    {
                        if let Ok(call) = &request.tool_call {
                            progress_emit.emit(AgentEvent::SubagentProgress {
                                tool_call_id: progress_id.clone(),
                                task: index,
                                text: progress_line(
                                    &call.name,
                                    &call.arguments.clone().unwrap_or_default(),
                                ),
                            });
                        }
                    }
                }
            }
        });
        let tool_names: Vec<String> = SessionTools::tool_names()
            .into_iter()
            .filter(|name| !BLOCKED_TOOLS.contains(name))
            .map(str::to_string)
            .chain(self.mcp.tool_names())
            .collect();
        let provider: Arc<dyn ToolProvider<TurnSession>> = Arc::new(ops::GuardedTools {
            inner: Arc::new(ChildToolProvider {
                inner: tools.clone(),
                mcp: self.mcp.clone(),
            }),
            scratch_dir: scratch.clone(),
            state: state.clone(),
            result_limit: ops::result_size_limit(self.context_length),
        });
        let mut model_config = ModelConfig::new(&self.model)
            .with_context_limit(self.context_length)
            .with_vision_support(self.supports_vision);
        if let Some(params) = self.effort_params.clone() {
            model_config = model_config.with_merged_request_params(params);
        }
        let child_prompt = child.record.lock().system_prompt.clone();
        let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
            Step::Operation(Arc::new(ops::FrozenPrompt)),
            Step::Operation(Arc::new(ops::IterationBudget {
                state: state.clone(),
                max_iterations: CHILD_MAX_ITERATIONS,
            })),
            Step::Operation(Arc::new(ops::ValidateToolCalls {
                state: state.clone(),
                tool_names: tool_names.clone(),
            })),
            Step::Operation(Arc::new(ToolOperation::new().with_provider(provider))),
            Step::Operation(Arc::new(ops::UnhandledToolCalls { tool_names })),
            Step::Operation(Arc::new(ops::LengthContinuation {
                state: state.clone(),
            })),
            Step::Operation(Arc::new(ops::EmptyResponseRecovery {
                state: state.clone(),
            })),
            Step::Operation(Arc::new(ops::ExitOnError)),
            Step::Operation(Arc::new(compress::AutoCompress {
                state: state.clone(),
                summarizer: compress::Summarizer {
                    llm: self.llm.clone(),
                    model: self.model.clone(),
                    context_length: self.context_length,
                },
                rebuild_prompt: Arc::new(move || Ok(child_prompt.clone())),
            })),
            Step::Inference(Arc::new(InferenceRunner::new(
                self.llm.clone(),
                model_config,
            ))),
        ];
        let machine = StateMachine::new(steps, cancel.clone());
        let session_id = child.record.lock().id.clone();
        let outcome = machine.run(&runtime, &session_id, &emit).await;
        drop(machine);
        drop(emit);
        let _ = relay.await;
        tools.shutdown().await;

        let messages = child.conversation.lock().messages().clone();
        let replies: Vec<&Message> = messages
            .iter()
            .filter(|m| m.role == Role::Assistant && m.is_agent_visible())
            .collect();
        let last = messages.iter().rev().find(|m| m.role == Role::Assistant);
        let error = match &outcome {
            Err(error) => Some(format!("{error:#}")),
            Ok(_) => last.and_then(|m| m.error_kind().map(|_| m.as_concat_text())),
        };
        let summary = last
            .filter(|m| m.error_kind().is_none())
            .map(|m| m.as_concat_text())
            .unwrap_or_default();
        let truncated = state.lock().summary_requested;
        let (status, exit_reason) = if cancel.is_cancelled() {
            ("interrupted", "interrupted")
        } else if error.is_some() || summary.trim().is_empty() {
            ("failed", "error")
        } else if truncated {
            ("completed", "max_iterations")
        } else {
            ("completed", "completed")
        };
        // The tool trace: which tools ran and whether they failed.
        let failed_results: std::collections::HashSet<&str> = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(MessageContent::as_tool_response)
            .filter(|r| match &r.tool_result {
                Ok(result) => result.is_error == Some(true),
                Err(_) => true,
            })
            .map(|r| r.id.as_str())
            .collect();
        let tool_trace: Vec<Value> = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(MessageContent::as_tool_request)
            .filter_map(|r| {
                let call = r.tool_call.as_ref().ok()?;
                let status = if failed_results.contains(r.id.as_str()) {
                    "error"
                } else {
                    "ok"
                };
                Some(json!({"tool": call.name, "status": status}))
            })
            .collect();
        json!({
            "task_index": index,
            "status": status,
            "summary": capped(&summary),
            "error": error,
            "api_calls": replies.len(),
            "duration_seconds": (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
            "model": self.model,
            "exit_reason": exit_reason,
            "truncated": exit_reason == "max_iterations",
            "tool_trace": tool_trace,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_prompt_has_context_and_workspace() {
        let block = child_prompt_block(Some("the bug is in parser.rs"), Path::new(r"C:\proj"));
        assert!(block.starts_with("You are a focused subagent"));
        assert!(block.contains("CONTEXT:\nthe bug is in parser.rs"));
        assert!(block.contains("WORKSPACE PATH:\nC:/proj"));
        assert!(block.ends_with("parent's context window."));
    }

    #[test]
    fn progress_lines_are_short() {
        let mut args = serde_json::Map::new();
        args.insert("command".into(), json!("cargo   test\n --all"));
        assert_eq!(
            progress_line("terminal", &args),
            "terminal: cargo test --all"
        );
        assert_eq!(
            progress_line("todo_list", &serde_json::Map::new()),
            "todo_list"
        );
        let long = "x".repeat(30_000);
        assert!(capped(&long).contains("chars of the summary omitted"));
    }
}
