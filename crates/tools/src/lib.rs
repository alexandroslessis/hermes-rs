//! The core tools an agent session gets, served through goose-agent's `ToolProvider` trait.
//!
//! One [`SessionTools`] exists per agent session and lives across turns: it owns the session's
//! persistent shell, background processes, todo list and the file-read state that guards
//! `write_file`. Tool results are JSON text, like Hermes's.

pub mod approval;
mod bundles;
pub mod checkpoints;
mod clarify;
pub mod curator;
mod files;
mod hints;
mod job;
pub mod lint;
mod paths;
mod process;
mod recall;
pub mod redact;
pub mod references;
mod review;
mod search;
mod shell;
mod skill_manage;
pub mod skills;
mod todo;
pub mod vision;
pub mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::operation::Emitter;
use hermes_agent::tool::ToolProvider;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use serde_json::{Value, json};

/// How long a flagged command waits for the user (Hermes's `approvals.timeout`, 300 s); then
/// it's denied.
const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub use bundles::{Bundle, describe_bundle_invocation};
pub use clarify::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse,
};
pub use process::ProcessNotice;
pub use review::ReviewTools;
pub use shell::{ShellKind, detect_shell};
pub use skill_manage::{PendingSkillWrite, SkillOp, WriteOrigin};
pub use skills::{SkillCommit, SkillEntry, SkillStore, SkillsConfig};
pub use todo::{TodoItem, TodoStatus};

/// What the tools need from the app: answers to `clarify`, and somewhere to deliver
/// background-process completion notices.
#[async_trait]
pub trait UserBridge: Send + Sync {
    async fn clarify(&self, session_id: &str, request: ClarifyRequest) -> ClarifyOutcome;
    fn process_finished(&self, session_id: &str, notice: ProcessNotice);
    /// A background review of the session finished: what it changed (Hermes's "Self-improvement
    /// review" summary). `requested` is true for `/refine`, which reports even when nothing changed.
    fn review_finished(&self, _session_id: &str, _actions: Vec<String>, _requested: bool) {}
    /// A command matched a dangerous pattern: ask the user. Without an answer the command
    /// doesn't run.
    async fn approve_command(
        &self,
        _session_id: &str,
        _request: approval::CommandApproval,
    ) -> approval::ApprovalChoice {
        approval::ApprovalChoice::Deny
    }
}

pub struct SessionToolsConfig {
    pub session_id: String,
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// Per-session scratch space; also where truncated terminal output is saved in full.
    pub scratch_dir: PathBuf,
    pub shell: ShellKind,
    pub bridge: Arc<dyn UserBridge>,
    /// Restored from the last `todo_list` result in the conversation when reopening a session.
    pub todos: Vec<TodoItem>,
    /// `MEMORY.md` / `USER.md`, shared by every session (the `memory` tool).
    pub memory: Option<Arc<hermes_memory::MemoryStore>>,
    /// Past conversations (the `session_search` tool).
    pub history: Option<Arc<hermes_store::Store>>,
    /// The skills folder (`skills_list`, `skill_view`, `skill_manage`).
    pub skills: Option<Arc<SkillStore>>,
    /// The dangerous-command policy (asking, and the always-allowed patterns).
    pub approvals: Option<Arc<approval::ApprovalStore>>,
    /// Filesystem checkpoints taken before file changes (`/rollback`).
    pub checkpoints: Option<Arc<checkpoints::Checkpoints>>,
    /// `web_search` / `web_extract` (the keyless vendor ring, shared by every session).
    pub web: Option<Arc<web::Web>>,
}

pub struct SessionTools {
    session_id: String,
    home: PathBuf,
    scratch_dir: PathBuf,
    bridge: Arc<dyn UserBridge>,
    shell: shell::ShellSession,
    processes: process::ProcessRegistry,
    files: files::FileState,
    todos: todo::TodoList,
    memory: Option<Arc<hermes_memory::MemoryStore>>,
    /// Failed memory consolidations this turn (Hermes caps them per turn).
    memory_failures: parking_lot::Mutex<hermes_memory::TurnFailures>,
    history: Option<Arc<hermes_store::Store>>,
    skills: Option<Arc<SkillStore>>,
    approvals: Option<Arc<approval::ApprovalStore>>,
    /// Patterns the user approved for this session.
    pub(crate) session_approved: parking_lot::Mutex<std::collections::HashSet<String>>,
    /// Directories whose AGENTS.md / CLAUDE.md the model has been shown.
    hints: parking_lot::Mutex<hints::HintTracker>,
    checkpoints: Option<Arc<checkpoints::Checkpoints>>,
    web: Option<Arc<web::Web>>,
    /// Projects already checkpointed this turn (one snapshot per project per turn).
    turn_checkpoints: parking_lot::Mutex<std::collections::HashSet<String>>,
    /// Hermes's `/yolo`: flagged commands run without asking (hardline ones never do).
    yolo: std::sync::atomic::AtomicBool,
    /// The smart-approval reviewer for this turn's model.
    guard: parking_lot::Mutex<Option<Arc<dyn approval::ApprovalGuard>>>,
    /// Whether the session's model accepts images (set per turn; `vision_analyze` needs it).
    vision: std::sync::atomic::AtomicBool,
}

impl SessionTools {
    pub fn new(config: SessionToolsConfig) -> Self {
        Self {
            hints: parking_lot::Mutex::new(hints::HintTracker::new(&config.cwd, &config.home)),
            shell: shell::ShellSession::new(config.shell, config.cwd, config.scratch_dir.clone()),
            processes: process::ProcessRegistry::default(),
            files: files::FileState::default(),
            todos: todo::TodoList::new(config.todos),
            session_id: config.session_id,
            home: config.home,
            scratch_dir: config.scratch_dir,
            bridge: config.bridge,
            memory: config.memory,
            memory_failures: Default::default(),
            history: config.history,
            skills: config.skills,
            approvals: config.approvals,
            checkpoints: config.checkpoints,
            web: config.web,
            turn_checkpoints: Default::default(),
            vision: Default::default(),
            yolo: Default::default(),
            guard: Default::default(),
            session_approved: Default::default(),
        }
    }

    /// Hermes's checkpoint trigger: the project a `write_file` / `patch` touches, or the folder a
    /// destructive terminal command runs in, is snapshotted once per turn before the change.
    /// Returns the file a write targets, for the agent-write ledger.
    fn checkpoint_before(&self, tool: &str, args: &Value) -> Option<PathBuf> {
        let checkpoints = self.checkpoints.as_ref().filter(|c| c.enabled())?;
        let mut taken = self.turn_checkpoints.lock();
        match tool {
            "write_file" | "patch" => {
                let path = self.resolve(args.get("path")?.as_str()?);
                let workdir = checkpoints.working_dir_for(&path, &self.home);
                checkpoints.ensure(&workdir, &format!("before {tool}"), &self.home, &mut taken);
                Some(path)
            }
            "terminal" => {
                let command = args.get("command")?.as_str()?;
                if checkpoints::is_destructive_command(command) {
                    let cwd = match args.get("workdir").and_then(Value::as_str) {
                        Some(dir) => self.resolve(dir),
                        None => self.cwd(),
                    };
                    let short: String = command.chars().take(60).collect();
                    checkpoints.ensure(
                        &cwd,
                        &format!("before terminal: {short}"),
                        &self.home,
                        &mut taken,
                    );
                }
                None
            }
            _ => None,
        }
    }

    /// `/yolo` for this session.
    pub fn set_yolo(&self, on: bool) {
        self.yolo.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn yolo(&self) -> bool {
        self.yolo.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The reviewer smart approvals use (set per turn, for its model).
    pub fn set_guard(&self, guard: Option<Arc<dyn approval::ApprovalGuard>>) {
        *self.guard.lock() = guard;
    }

    /// Whether the model of the coming turn accepts images.
    pub fn set_vision(&self, supported: bool) {
        self.vision
            .store(supported, std::sync::atomic::Ordering::Relaxed);
    }

    /// Hermes's native `vision_analyze`: the image goes into the tool result, which the model
    /// sees as an image.
    async fn vision_analyze(&self, args: &Value) -> CallToolResult {
        if !self.vision.load(std::sync::atomic::Ordering::Relaxed) {
            return ToolOutput::error(vision::NO_VISION_MESSAGE).into_result();
        }
        let Some(source) = args
            .get("image_url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return ToolOutput::error("image_url is required").into_result();
        };
        let question = args.get("question").and_then(Value::as_str).unwrap_or("");
        let region = match args.get("region") {
            None | Some(Value::Null) => None,
            Some(Value::Array(values)) if values.len() == 4 => {
                let numbers: Vec<u32> = values
                    .iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()))
                    .collect();
                match numbers.as_slice() {
                    &[x1, y1, x2, y2] => Some([x1, y1, x2, y2]),
                    _ => {
                        return ToolOutput::error(
                            "region must be four non-negative integers [x1, y1, x2, y2]",
                        )
                        .into_result();
                    }
                }
            }
            Some(_) => {
                return ToolOutput::error("region must be four integers [x1, y1, x2, y2]")
                    .into_result();
            }
        };
        let bytes = match vision::load_source(source, &self.cwd(), &self.home).await {
            Ok(bytes) => bytes,
            Err(error) => return ToolOutput::error(error).into_result(),
        };
        let prepared = tokio::task::spawn_blocking(move || {
            vision::prepare(
                &bytes,
                region,
                vision::EMBED_MAX_DIMENSION,
                vision::EMBED_TARGET_BYTES,
            )
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        match prepared {
            Ok(image) => CallToolResult::success(vec![
                ContentBlock::text(vision::native_result_text(question, image.note.as_deref())),
                ContentBlock::image(image.data, image.mime),
            ]),
            Err(error) => ToolOutput::error(error).into_result(),
        }
    }

    /// Called when a turn starts: per-turn tool state resets.
    pub fn begin_turn(&self) {
        self.turn_checkpoints.lock().clear();
        *self.memory_failures.lock() = hermes_memory::TurnFailures::default();
    }

    pub fn cwd(&self) -> PathBuf {
        self.shell.cwd()
    }

    /// Per-session scratch space (oversized tool results are spilled here).
    pub fn scratch_dir(&self) -> &Path {
        &self.scratch_dir
    }

    pub fn todos(&self) -> Vec<TodoItem> {
        self.todos.items()
    }

    pub fn reset_todos(&self, items: Vec<TodoItem>) {
        self.todos.reset(items);
    }

    pub fn tool_names() -> Vec<&'static str> {
        hermes_prompts::tools::tool_names().collect()
    }

    fn resolve(&self, path: &str) -> PathBuf {
        paths::resolve(path, &self.cwd(), &self.home)
    }

    /// How a path appears in results: relative to the working directory when inside it.
    fn shown(&self, path: &Path) -> String {
        let cwd = self.cwd();
        let lower = |p: &Path| display_path(p).to_lowercase();
        let (full, base) = (display_path(path), lower(&cwd));
        if lower(path).starts_with(&format!("{}/", base.trim_end_matches('/'))) {
            full[base.trim_end_matches('/').len() + 1..].to_string()
        } else {
            full
        }
    }

    /// Kills the shell and every background process. Called when the session is closed.
    pub async fn shutdown(&self) {
        self.shell.shutdown().await;
        self.processes.kill_all().await;
    }

    async fn dispatch(&self, name: &str, args: Value) -> ToolOutput {
        let read_path = (name == "read_file")
            .then(|| {
                args.get("path")
                    .and_then(Value::as_str)
                    .map(|p| self.resolve(p))
            })
            .flatten();
        let mut output = self.dispatch_unredacted(name, args).await;
        // Hermes masks secrets in what file reads, searches and process logs hand the model.
        match name {
            "read_file" => {
                let secret_file = read_path.as_deref().is_some_and(redact::is_secret_file);
                redact_field(
                    &mut output.value,
                    "content",
                    redact::Source::File { secret_file },
                );
            }
            "search_files" => {
                if let Some(Value::Array(matches)) = output.value.get_mut("matches") {
                    for entry in matches {
                        let secret_file = entry
                            .get("path")
                            .and_then(Value::as_str)
                            .is_some_and(|p| redact::is_secret_file(Path::new(p)));
                        let source = redact::Source::File { secret_file };
                        redact_field(entry, "content", source);
                        if let Some(Value::Array(lines)) = entry.get_mut("context") {
                            for line in lines {
                                if let Value::String(text) = line {
                                    *text = redact::redact(text, source);
                                } else {
                                    redact_field(line, "content", source);
                                }
                            }
                        }
                    }
                }
            }
            "process_manage" => {
                redact_field(&mut output.value, "output", redact::Source::Output);
            }
            _ => {}
        }
        output
    }

    async fn dispatch_unredacted(&self, name: &str, args: Value) -> ToolOutput {
        match name {
            "read_file" => files::read_file(self, args).await,
            "write_file" => files::write_file(self, args).await,
            "patch" => files::patch(self, args).await,
            "search_files" => search::search_files(self, args).await,
            "terminal" => shell::terminal(self, args).await,
            "process_manage" => process::process_manage(self, args).await,
            "todo_list" => todo::todo_list(self, args),
            "memory" => self.memory_tool(&args),
            "session_search" => self.session_search(&args),
            "skills_list" | "skill_view" | "skill_manage" => self.skills_tool(name, &args),
            "clarify" => clarify::clarify(self, args).await,
            "web_search" | "web_extract" => self.web_tool(name, &args).await,
            "delegate_task" => ToolOutput::error("delegate_task is not available here."),
            other => ToolOutput::error(format!("Unknown tool '{other}'.")),
        }
    }
}

impl SessionTools {
    /// The session's background processes, as the `process` tool lists them.
    pub fn process_list(&self) -> Vec<Value> {
        self.processes.status_list()
    }

    async fn web_tool(&self, name: &str, args: &Value) -> ToolOutput {
        let Some(web) = &self.web else {
            return ToolOutput::error("Web tools are not available in this session.");
        };
        let value = if name == "web_search" {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("");
            web.search(query, args.get("limit").and_then(Value::as_i64))
                .await
        } else {
            let urls = args
                .get("urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            web.extract(&urls, args.get("char_limit").and_then(Value::as_i64))
                .await
        };
        let failed = value.get("success") == Some(&Value::Bool(false));
        ToolOutput {
            value,
            is_error: failed,
        }
    }

    fn skills_tool(&self, name: &str, args: &Value) -> ToolOutput {
        let Some(skills) = &self.skills else {
            return ToolOutput::error("Skills are not available in this session.");
        };
        let result = match name {
            "skills_list" => skills.list_tool(args),
            "skill_view" => skills.view_tool(args),
            _ => skills.manage_tool(args),
        };
        let failed = result.get("success") == Some(&Value::Bool(false));
        ToolOutput {
            value: result,
            is_error: failed,
        }
    }

    /// Hermes's terminal approval guard. `Ok` means run it (carrying the pattern when the user
    /// just approved it); `Err` is the blocked result for the model.
    pub(crate) async fn check_command(&self, command: &str) -> Result<Option<String>, ToolOutput> {
        let Some(store) = &self.approvals else {
            return Ok(None);
        };
        let gate = approval::gate(command, store, &self.session_approved.lock());
        let blocked = |message: String| {
            ToolOutput::error_with(
                message,
                json!({"output": "", "exit_code": -1, "status": "blocked"}),
            )
        };
        let request = match gate {
            approval::Gate::Run { approved } => return Ok(approved),
            approval::Gate::Blocked(message) => return Err(blocked(message)),
            approval::Gate::Ask(request) => request,
        };
        let key = request.description.clone();
        if self.yolo() {
            return Ok(None);
        }
        // Smart approvals: the reviewer decides first; only an uncertain verdict reaches the user.
        let guard = self.guard.lock().clone();
        if let Some(guard) = guard.filter(|_| store.smart()) {
            match guard.assess(&request.command, &request.description).await {
                approval::Verdict::Approve => return Ok(Some(key)),
                approval::Verdict::Deny => {
                    return Err(blocked(approval::smart_denied_message(
                        &request.description,
                    )));
                }
                approval::Verdict::Escalate => {}
            }
        }
        let answer = tokio::time::timeout(
            APPROVAL_TIMEOUT,
            self.bridge.approve_command(&self.session_id, request),
        )
        .await;
        match answer {
            Ok(approval::ApprovalChoice::Once) => Ok(Some(key)),
            Ok(approval::ApprovalChoice::Session) => {
                self.session_approved.lock().insert(key.clone());
                Ok(Some(key))
            }
            Ok(approval::ApprovalChoice::Always) => {
                store.allow_always(&key);
                Ok(Some(key))
            }
            Ok(approval::ApprovalChoice::Deny) => Err(blocked(approval::denied_message())),
            Err(_) => Err(blocked(approval::timeout_message())),
        }
    }

    fn memory_tool(&self, args: &Value) -> ToolOutput {
        let Some(memory) = &self.memory else {
            return ToolOutput::error(
                "Memory is not available. It may be disabled in config or this environment.",
            );
        };
        let result = memory.tool(args, &mut self.memory_failures.lock());
        let failed = result.get("success") == Some(&Value::Bool(false));
        ToolOutput {
            value: result,
            is_error: failed,
        }
    }
}

/// A tool result: JSON shown to the model, flagged as an error when the call failed.
pub(crate) struct ToolOutput {
    value: Value,
    is_error: bool,
}

impl ToolOutput {
    pub(crate) fn ok(value: Value) -> Self {
        Self {
            value,
            is_error: false,
        }
    }

    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self {
            value: json!({"success": false, "error": message.into()}),
            is_error: true,
        }
    }

    pub(crate) fn error_with(message: impl Into<String>, mut extra: Value) -> Self {
        extra["success"] = json!(false);
        extra["error"] = json!(message.into());
        Self {
            value: extra,
            is_error: true,
        }
    }

    fn into_result(self) -> CallToolResult {
        let text = serde_json::to_string(&self.value).unwrap_or_else(|_| self.value.to_string());
        if self.is_error {
            CallToolResult::error(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(text)])
        }
    }
}

/// Deserialize tool arguments, turning a mismatch into a message the model can act on.
pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: Value,
) -> Result<T, ToolOutput> {
    serde_json::from_value(args)
        .map_err(|error| ToolOutput::error(format!("Invalid arguments for {tool}: {error}")))
}

pub fn display_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Default)]
    pub struct TestBridge {
        pub notices: parking_lot::Mutex<Vec<ProcessNotice>>,
    }

    #[async_trait]
    impl UserBridge for TestBridge {
        async fn clarify(&self, _session_id: &str, _request: ClarifyRequest) -> ClarifyOutcome {
            ClarifyOutcome::Undelivered
        }

        fn process_finished(&self, _session_id: &str, notice: ProcessNotice) {
            self.notices.lock().push(notice);
        }
    }

    pub fn tools_with_bridge(bridge: Arc<dyn UserBridge>) -> (tempfile::TempDir, SessionTools) {
        let dir = tempfile::tempdir().unwrap();
        let tools = SessionTools::new(SessionToolsConfig {
            session_id: "ses_test".into(),
            cwd: dir.path().to_path_buf(),
            home: dir.path().to_path_buf(),
            scratch_dir: dir.path().join(".scratch"),
            shell: detect_shell(),
            bridge,
            todos: Vec::new(),
            memory: None,
            history: None,
            skills: None,
            approvals: None,
            checkpoints: None,
            web: None,
        });
        (dir, tools)
    }

    pub fn tools_in() -> (tempfile::TempDir, SessionTools) {
        tools_with_bridge(Arc::new(TestBridge::default()))
    }

    #[tokio::test]
    async fn file_reads_and_searches_mask_secrets() {
        let (dir, tools) = tools_in();
        std::fs::write(
            dir.path().join(".env"),
            "DB_PASSWORD=hunter2hunter2\nOPENAI_KEY=sk-abcdefghijklmnop1234\n",
        )
        .unwrap();
        let read = tools.dispatch("read_file", json!({"path": ".env"})).await;
        let content = read.value["content"].as_str().unwrap();
        assert!(
            !content.contains("hunter2") && !content.contains("sk-abcdefghij"),
            "{content}"
        );
        assert!(content.contains("«redacted:sk-…»"), "{content}");
        let found = tools
            .dispatch(
                "search_files",
                json!({"pattern": "OPENAI", "target": "content", "path": "."}),
            )
            .await;
        assert!(
            !found.value.to_string().contains("sk-abcdefghij"),
            "{}",
            found.value
        );
    }
}

#[async_trait]
impl<S: Send + Sync + 'static> ToolProvider<S> for SessionTools {
    async fn tools(&self, _session: &S) -> anyhow::Result<Vec<Tool>> {
        hermes_prompts::tools::tool_names()
            .map(|name| {
                let schema = hermes_prompts::tools::tool_schema(name)?;
                Ok(Tool::new(
                    schema.name,
                    schema.description,
                    Arc::new(schema.parameters),
                ))
            })
            .collect()
    }

    async fn call(
        &self,
        _session: &S,
        _request_id: &str,
        call: CallToolRequestParams,
        _emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        let args = Value::Object(call.arguments.unwrap_or_else(JsonObject::new));
        if call.name == "vision_analyze" {
            return Ok(self.vision_analyze(&args).await);
        }
        let written = self.checkpoint_before(&call.name, &args);
        let mut result = self.dispatch(&call.name, args.clone()).await.into_result();
        if let (Some(path), Some(checkpoints)) = (written, &self.checkpoints)
            && result.is_error != Some(true)
        {
            checkpoints.record_write(&path, &self.home);
        }
        // Hermes appends a newly visited subdirectory's context file to the result.
        let hints = self.hints.lock().check(&call.name, &args, &self.cwd());
        if let Some(hints) = hints
            && let Some(ContentBlock::Text(text)) = result.content.first_mut()
        {
            text.text.push_str(&hints);
        }
        Ok(result)
    }
}

/// Masks the secrets in one string field of a tool result.
fn redact_field(value: &mut Value, field: &str, source: redact::Source) {
    if let Some(Value::String(text)) = value.get_mut(field) {
        *text = redact::redact(text, source);
    }
}
