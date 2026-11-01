//! The core tools an agent session gets, served through goose-agent's `ToolProvider` trait.
//!
//! One [`SessionTools`] exists per agent session and lives across turns: it owns the session's
//! persistent shell, background processes, todo list and the file-read state that guards
//! `write_file`. Tool results are JSON text, like Hermes's.

mod clarify;
mod files;
mod job;
mod paths;
mod process;
mod recall;
mod search;
mod shell;
mod todo;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::operation::Emitter;
use hermes_agent::tool::ToolProvider;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use serde_json::{Value, json};

pub use clarify::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse,
};
pub use process::ProcessNotice;
pub use shell::{ShellKind, detect_shell};
pub use todo::{TodoItem, TodoStatus};

/// What the tools need from the app: answers to `clarify`, and somewhere to deliver
/// background-process completion notices.
#[async_trait]
pub trait UserBridge: Send + Sync {
    async fn clarify(&self, session_id: &str, request: ClarifyRequest) -> ClarifyOutcome;
    fn process_finished(&self, session_id: &str, notice: ProcessNotice);
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
}

impl SessionTools {
    pub fn new(config: SessionToolsConfig) -> Self {
        Self {
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
        }
    }

    /// Called when a turn starts: per-turn tool state resets.
    pub fn begin_turn(&self) {
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
            "clarify" => clarify::clarify(self, args).await,
            other => ToolOutput::error(format!("Unknown tool '{other}'.")),
        }
    }
}

impl SessionTools {
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

pub(crate) fn display_path(path: &Path) -> String {
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
        });
        (dir, tools)
    }

    pub fn tools_in() -> (tempfile::TempDir, SessionTools) {
        tools_with_bridge(Arc::new(TestBridge::default()))
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
        Ok(self.dispatch(&call.name, args).await.into_result())
    }
}
