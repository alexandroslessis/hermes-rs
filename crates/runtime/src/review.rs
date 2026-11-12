//! Hermes's background memory/skill review (`agent/background_review.py`, MIT): after a turn,
//! the conversation is replayed with a review prompt to a copy of the agent, which may save
//! memories and improve skills. The copy has the session's frozen prompt and tool list (so its
//! requests reuse the prompt cache), may only dispatch the review tools, and its messages never
//! join the session. `/refine` runs the same review on request.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use hermes_agent::inference::InferenceRunner;
use hermes_agent::machine::{StateMachine, Step};
use hermes_agent::operation::Emitter;
use hermes_agent::tool::{ToolOperation, ToolProvider};
use hermes_llm::base::Provider;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::model::ModelConfig;
use hermes_store::Store;
use hermes_tools::{ReviewTools, SessionTools, UserBridge};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::ops;
use crate::session::{HermesEffect, OpenSession, ReviewHandle, TurnRuntime, TurnSession};

/// How often a paused review checks whether the turn that paused it has ended.
const PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// Model calls one review may make (Hermes's `_REVIEW_MAX_ITERATIONS`).
const REVIEW_MAX_ITERATIONS: u32 = 16;
/// User turns between memory reviews (Hermes's `memory.nudge_interval`).
pub const MEMORY_NUDGE_INTERVAL: u32 = 10;
/// Model calls without a `skill_manage` before a skill review (`skills.creation_nudge_interval`).
pub const SKILL_NUDGE_INTERVAL: u32 = 10;

/// Hermes's nudge counters for one session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Nudges {
    pub turns_since_memory: u32,
    pub iters_since_skill: u32,
}

impl Nudges {
    /// Counters for a session reopened with `user_turns` turns behind it: the memory counter
    /// resumes where the turn count leaves it, as Hermes hydrates it from history.
    pub fn resumed(user_turns: usize) -> Self {
        Self {
            turns_since_memory: (user_turns as u32) % MEMORY_NUDGE_INTERVAL,
            iters_since_skill: 0,
        }
    }

    /// A user message starts a turn (`_tick_memory_nudge`): `true` when this turn's end reviews
    /// memory.
    pub fn tick_memory(&mut self) -> bool {
        self.turns_since_memory += 1;
        if self.turns_since_memory >= MEMORY_NUDGE_INTERVAL {
            self.turns_since_memory = 0;
            return true;
        }
        false
    }

    /// Counts a finished turn's model calls toward the skill review, resetting on every call that
    /// used `skill_manage` (and the memory counter on `memory`). `true` when skills are due.
    pub fn finish_turn(&mut self, turn: &[Message]) -> bool {
        for message in turn {
            if message.role != rmcp::model::Role::Assistant || !message.is_agent_visible() {
                continue;
            }
            self.iters_since_skill += 1;
            for name in tool_names(message) {
                match name.as_str() {
                    "skill_manage" => self.iters_since_skill = 0,
                    "memory" => self.turns_since_memory = 0,
                    _ => {}
                }
            }
        }
        if self.iters_since_skill >= SKILL_NUDGE_INTERVAL {
            self.iters_since_skill = 0;
            return true;
        }
        false
    }
}

fn tool_names(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(MessageContent::as_tool_request)
        .filter_map(|request| request.tool_call.as_ref().ok())
        .map(|call| call.name.to_string())
        .collect()
}

/// Everything one review needs, captured when it's started.
pub struct ReviewJob {
    pub session: Arc<OpenSession>,
    pub store: Arc<Store>,
    pub bridge: Arc<dyn UserBridge>,
    pub llm: Arc<dyn Provider>,
    pub model: String,
    pub context_length: Option<usize>,
    pub effort_params: Option<HashMap<String, Value>>,
    pub memory: bool,
    pub skills: bool,
    pub focus: Option<String>,
    /// Requested by the user (`/refine`): memory keeps every operation, and the result is
    /// reported even when nothing changed.
    pub requested: bool,
    /// The review's slot in its session (`OpenSession::review`): its cancel token, and the flag
    /// that says whether it has made its first request.
    pub handle: ReviewHandle,
    /// The budget knobs of the turn that started the review.
    pub budget: crate::budget::BudgetKnobs,
}

/// Runs a review and reports what it changed to the app.
pub async fn run(job: ReviewJob) {
    let session = job.session.clone();
    let session_id = session.record.lock().id.clone();
    let (bridge, requested, cancel) = (
        job.bridge.clone(),
        job.requested,
        job.handle.cancel.clone(),
    );
    let result = review(job).await;
    // Done: free the slot, unless a newer review took it (taking it cancels this one).
    if !cancel.is_cancelled() {
        *session.review.lock() = None;
    }
    match result {
        Ok(_) if cancel.is_cancelled() => {}
        Ok(actions) => {
            if !actions.is_empty() || requested {
                bridge.review_finished(&session_id, actions, requested);
            }
        }
        Err(error) => tracing::warn!("background memory/skill review failed: {error:#}"),
    }
}

async fn review(job: ReviewJob) -> Result<Vec<String>> {
    let ReviewJob {
        session,
        store,
        llm,
        model,
        context_length,
        effort_params,
        memory,
        skills,
        focus,
        requested,
        handle,
        budget,
        ..
    } = job;
    let cancel = handle.cancel.clone();
    // The conversation as it is when no turn is running, plus the review prompt; the copy never
    // touches the session. A turn that starts first pauses the review here (before its first
    // request): the snapshot is taken under the `running` lock, which `begin_turn` holds while it
    // claims the session, so the review never copies a conversation a turn is appending to (a
    // half-finished tool exchange, or a user message with no answer yet).
    let mut conversation = loop {
        {
            let running = session.running.lock();
            if running.is_none() {
                handle
                    .started
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                break session.conversation.lock().clone();
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(Vec::new()),
            _ = tokio::time::sleep(PAUSE_POLL) => {}
        }
    };
    let reviewed = conversation.messages().len();
    let prompt = hermes_prompts::review::review_message(memory, skills, focus.as_deref());
    conversation.push(
        Message::user()
            .with_text(prompt)
            .with_generated_id_if_missing(),
    );
    let fork = Arc::new(OpenSession {
        record: Mutex::new(session.record.lock().clone()),
        conversation: Mutex::new(conversation),
        tools: session.tools.clone(),
        running: Mutex::new(None),
        turn_state: Mutex::new(None),
        review: Mutex::new(None),
        nudges: Mutex::new(Nudges::default()),
        context: Mutex::new(None),
    });
    let session_id = fork.record.lock().id.clone();
    let state: ops::SharedTurnState = Arc::default();
    let runtime = TurnRuntime {
        session: fork.clone(),
        store,
        state: state.clone(),
        persist: false,
        record_usage: true,
    };

    // Nothing shows the review's events; they're drained and dropped.
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
    let emit = Emitter::new(events_tx, cancel.clone());
    let tool_names: Vec<String> = SessionTools::tool_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let tools: Arc<dyn ToolProvider<TurnSession>> = Arc::new(ops::GuardedTools {
        inner: Arc::new(ReviewTools::new(session.tools.clone(), memory, requested)),
        scratch_dir: session.tools.scratch_dir().to_path_buf(),
        state: state.clone(),
        result_limit: ops::result_size_limit(context_length, &budget),
        disabled: Default::default(),
    });
    let mut model_config = ModelConfig::new(&model).with_context_limit(context_length);
    if let Some(params) = effort_params {
        model_config = model_config.with_merged_request_params(params);
    }
    let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
        Step::Operation(Arc::new(ops::FrozenPrompt)),
        Step::Operation(Arc::new(ops::IterationBudget {
            state: state.clone(),
            max_iterations: REVIEW_MAX_ITERATIONS,
        })),
        Step::Operation(Arc::new(ops::ValidateToolCalls::new(
            state.clone(),
            tool_names.clone(),
            &budget,
        ))),
        Step::Operation(Arc::new(ToolOperation::new().with_provider(tools))),
        Step::Operation(Arc::new(ops::UnhandledToolCalls {
            tool_names,
            disabled: Default::default(),
        })),
        Step::Operation(Arc::new(ops::LengthContinuation::new(state.clone(), &budget))),
        Step::Operation(Arc::new(ops::EmptyResponseRecovery {
            state: state.clone(),
        })),
        Step::Operation(Arc::new(ops::ExitOnError)),
        Step::Inference(Arc::new(
            InferenceRunner::new(llm, model_config)
                .with_empty_response_retries(budget.empty_response_retries),
        )),
    ];
    let machine = StateMachine::new(steps, cancel.clone());
    let result = machine.run(&runtime, &session_id, &emit).await;
    drop(machine);
    drop(emit);
    let _ = drain.await;
    result?;
    let messages = fork.conversation.lock().messages().clone();
    Ok(summarize(messages.get(reviewed..).unwrap_or_default()))
}

/// Hermes's `summarize_background_review_actions`: one line per change the review's memory and
/// skill calls made, without repeats.
pub fn summarize(messages: &[Message]) -> Vec<String> {
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    for request in messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(MessageContent::as_tool_request)
    {
        if let Ok(call) = &request.tool_call {
            let args = Value::Object(call.arguments.clone().unwrap_or_default());
            calls.insert(request.id.clone(), (call.name.to_string(), args));
        }
    }
    let mut actions: Vec<String> = Vec::new();
    for content in messages.iter().flat_map(|message| &message.content) {
        let Some(response) = content.as_tool_response() else {
            continue;
        };
        let Some((name, args)) = calls.get(&response.id) else {
            continue;
        };
        let Some(data) = content
            .as_tool_response_text()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        if data.get("success") != Some(&Value::Bool(true)) {
            continue;
        }
        for line in action_lines(name, args, &data) {
            if !actions.contains(&line) {
                actions.push(line);
            }
        }
    }
    actions
}

fn action_lines(name: &str, args: &Value, data: &Value) -> Vec<String> {
    let text =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    if data.get("staged") == Some(&Value::Bool(true)) {
        // A change held for approval (unattended removals, or write approval being on).
        let what = if name == "memory" { "Memory" } else { "Skill" };
        return vec![format!("{what} change waiting for your approval")];
    }
    match name {
        "memory" => {
            let label = match text(args, "target").as_deref() {
                Some("user") => "User profile",
                _ => "Memory",
            };
            vec![format!("{label} updated")]
        }
        "skill_manage" => {
            let ops: Vec<Value> = match args.get("operations") {
                Some(Value::Array(ops)) => ops.clone(),
                _ => vec![args.clone()],
            };
            let results: Vec<Value> = match data.get("results") {
                Some(Value::Array(results)) => results.clone(),
                _ => vec![data.clone()],
            };
            let default_name = text(args, "name").unwrap_or_default();
            ops.iter()
                .zip(&results)
                .filter(|(_, result)| result.get("success") == Some(&Value::Bool(true)))
                .filter_map(|(op, _)| {
                    let verb = match text(op, "action")?.as_str() {
                        "create" => "created",
                        "patch" => "patched",
                        "edit" => "rewritten",
                        "write_file" => "written",
                        "remove_file" => "removed",
                        "delete" => "deleted",
                        _ => return None,
                    };
                    let skill = text(op, "name").unwrap_or_else(|| default_name.clone());
                    let path = text(op, "file_path")
                        .map(|p| format!(" ({p})"))
                        .unwrap_or_default();
                    Some(format!("Skill '{skill}' {verb}{path}"))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::conversation::message::Message;
    use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
    use serde_json::json;

    fn call(id: &str, name: &str, args: Value) -> Message {
        let params = CallToolRequestParams::new(name.to_string())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        Message::assistant().with_tool_request(id, Ok(params))
    }

    fn result(id: &str, value: Value) -> Message {
        Message::user().with_tool_response(
            id,
            Ok(CallToolResult::success(vec![ContentBlock::text(
                value.to_string(),
            )])),
        )
    }

    #[test]
    fn the_summary_lists_what_changed() {
        let messages = vec![
            call("a", "skill_view", json!({"name": "deploy"})),
            result("a", json!({"success": true, "content": "..."})),
            call(
                "b",
                "skill_manage",
                json!({"operations": [
                    {"action": "patch", "name": "deploy"},
                    {"action": "write_file", "name": "deploy", "file_path": "references/ci.md"}
                ]}),
            ),
            result(
                "b",
                json!({"success": true, "results": [{"success": true}, {"success": true}]}),
            ),
            call("c", "memory", json!({"target": "user", "operations": []})),
            result("c", json!({"success": true})),
            call("d", "memory", json!({"target": "memory", "operations": []})),
            result("d", json!({"success": true, "staged": true})),
            call(
                "e",
                "skill_manage",
                json!({"operations": [{"action": "create", "name": "x"}]}),
            ),
            result("e", json!({"success": false, "error": "no"})),
        ];
        assert_eq!(
            summarize(&messages),
            vec![
                "Skill 'deploy' patched",
                "Skill 'deploy' written (references/ci.md)",
                "User profile updated",
                "Memory change waiting for your approval",
            ]
        );
    }

    #[test]
    fn nudges_follow_hermes() {
        let mut nudges = Nudges::resumed(19);
        assert_eq!(nudges.turns_since_memory, 9);
        assert!(nudges.tick_memory());
        assert_eq!(nudges.turns_since_memory, 0);

        let calls: Vec<Message> = (0..9)
            .map(|i| call(&i.to_string(), "terminal", json!({})))
            .collect();
        assert!(!nudges.finish_turn(&calls));
        assert_eq!(nudges.iters_since_skill, 9);
        // A skill_manage call resets the count.
        assert!(!nudges.finish_turn(&[call("s", "skill_manage", json!({}))]));
        assert_eq!(nudges.iters_since_skill, 0);
        assert!(
            nudges.finish_turn(
                &(0..10)
                    .map(|i| call(&i.to_string(), "read_file", json!({})))
                    .collect::<Vec<_>>()
            )
        );
        assert_eq!(nudges.iters_since_skill, 0);
    }
}
