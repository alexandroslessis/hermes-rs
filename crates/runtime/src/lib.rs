//! The agent runtime the desktop app talks to: create and open sessions, start a turn and
//! receive its events, cancel it, list a provider's models.

mod guards;
mod ops;
mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_agent::events::AgentEvent;
use hermes_agent::inference::InferenceRunner;
use hermes_agent::machine::{StateMachine, Step};
use hermes_agent::operation::Emitter;
use hermes_agent::tool::{ToolOperation, ToolProvider};
use hermes_llm::api_client::{ApiClient, AuthMethod};
use hermes_llm::base::Provider;
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::discovery::DiscoveredModel;
use hermes_llm::model::ModelConfig;
use hermes_llm::openai_compatible::OpenAiCompatibleProvider;
use hermes_memory::{MemoryConfig, MemoryStore};
use hermes_prompts::{Shell, SystemPromptInput, build_system_prompt};
use hermes_store::{NewSession, SessionRecord, Store};
use hermes_tools::{
    SessionTools, SessionToolsConfig, ShellKind, SkillStore, SkillsConfig, TodoItem, UserBridge,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use hermes_tools::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse, ProcessNotice,
};
pub use ops::{EMPTY_TERMINAL_MESSAGE, SUMMARY_REQUEST, TRUNCATED_MESSAGE};
use session::{HermesEffect, OpenSession, TurnRuntime, TurnSession};

/// An OpenAI-compatible endpoint, e.g. OpenRouter at `https://openrouter.ai/api/v1`.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl ProviderConfig {
    fn client(&self) -> Result<ApiClient> {
        let auth = match &self.api_key {
            Some(key) if !key.is_empty() => AuthMethod::BearerToken(key.clone()),
            _ => AuthMethod::NoAuth,
        };
        ApiClient::new_with_tls(self.base_url.trim_end_matches('/').to_string(), auth, None)
    }

    fn provider(&self) -> Result<Arc<dyn Provider>> {
        Ok(Arc::new(OpenAiCompatibleProvider::new(
            self.id.clone(),
            self.client()?,
            String::new(),
        )))
    }

    /// Every model the endpoint serves, with context length, pricing and capabilities when known.
    pub async fn list_models(&self) -> Result<Vec<DiscoveredModel>> {
        Ok(hermes_llm::discovery::list_models(&self.client()?, &self.base_url).await?)
    }
}

/// Per-turn request settings.
#[derive(Debug, Clone, Default)]
pub struct TurnOptions {
    /// The model's context window, when known (sizes context-file caps and compaction).
    pub context_length: Option<usize>,
    /// Reasoning effort for thinking models: `none`, `minimal`, `low`, `medium`, `high` or
    /// `xhigh`. `None` sends nothing and leaves the provider's default (Hermes: unset stays unset).
    pub effort: Option<String>,
}

/// The request field that carries reasoning effort: OpenRouter normalizes it as
/// `reasoning: {effort}`; other OpenAI-compatible servers (vLLM, DeepSeek, LM Studio) take
/// `reasoning_effort`.
pub fn effort_request_params(base_url: &str, effort: &str) -> HashMap<String, serde_json::Value> {
    let openrouter = hermes_llm::discovery::catalog_provider_for(base_url) == "openrouter";
    if openrouter {
        HashMap::from([(
            "reasoning".to_string(),
            serde_json::json!({ "effort": effort }),
        )])
    } else {
        HashMap::from([("reasoning_effort".to_string(), serde_json::json!(effort))])
    }
}

pub struct RuntimeConfig {
    /// `%APPDATA%\hermes-rs`: sessions.db, SOUL.md, scratch/.
    pub data_dir: PathBuf,
    pub shell: ShellKind,
    /// Model calls allowed per user message (Hermes's iteration budget); when they run out the
    /// model gets one more call, without tools, to summarize.
    pub max_iterations: u32,
    /// `MEMORY.md` / `USER.md` limits and the write-approval gate.
    pub memory: MemoryConfig,
    /// Skill write approval and extra skill folders.
    pub skills: SkillsConfig,
}

impl RuntimeConfig {
    pub fn for_current_user() -> Result<Self> {
        // HERMES_RS_HOME points a run at another data folder (testing without touching the
        // real sessions and memory).
        let data_dir = match std::env::var_os("HERMES_RS_HOME") {
            Some(home) => PathBuf::from(home),
            None => dirs::data_dir()
                .context("no application data directory")?
                .join("hermes-rs"),
        };
        Ok(Self {
            data_dir,
            shell: hermes_tools::detect_shell(),
            max_iterations: 500,
            memory: MemoryConfig::default(),
            skills: SkillsConfig::default(),
        })
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Agent(AgentEvent),
    /// A `/steer` message the turn ended before delivering; Hermes sends it as the next message.
    SteerUndelivered(String),
    TurnFinished(TurnStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    /// The agent finished, or stopped because of a provider error (shown as a message).
    Completed,
    Cancelled,
    /// The runtime itself failed (database, invalid state).
    Failed(String),
}

pub struct Turn {
    pub events: mpsc::UnboundedReceiver<RuntimeEvent>,
    pub cancel: CancellationToken,
}

pub struct Runtime {
    store: Arc<Store>,
    memory: Arc<MemoryStore>,
    skills: Arc<SkillStore>,
    config: RuntimeConfig,
    bridge: Arc<dyn UserBridge>,
    windows_version: String,
    sessions: Mutex<HashMap<String, Arc<OpenSession>>>,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, bridge: Arc<dyn UserBridge>) -> Result<Self> {
        let store = Store::open(&config.data_dir.join("sessions.db"))?;
        let memory = MemoryStore::new(config.data_dir.join("memories"), config.memory.clone());
        let skills = SkillStore::new(config.data_dir.join("skills"), config.skills.clone());
        skills.seed_bundled();
        Ok(Self {
            store: Arc::new(store),
            memory: Arc::new(memory),
            skills: Arc::new(skills),
            windows_version: windows_version(),
            config,
            bridge,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The skills folder every session shares (the Skills screen edits it too).
    pub fn skills(&self) -> Arc<SkillStore> {
        self.skills.clone()
    }

    /// Hermes's skill slash commands: `/skill-name instruction` becomes the message that loads
    /// the skill. `None` when `text` isn't a command for an existing skill.
    pub fn skill_command(&self, text: &str) -> Option<String> {
        let rest = text.trim().strip_prefix('/')?;
        let (name, instruction) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        self.skills.invocation_message(name, instruction)
    }

    /// The memory store every session shares (the Memory screen edits it too).
    pub fn memory(&self) -> Arc<MemoryStore> {
        self.memory.clone()
    }

    fn scratch_dir(&self, session_id: &str) -> PathBuf {
        self.config.data_dir.join("scratch").join(session_id)
    }

    /// Creates a session and freezes its system prompt.
    pub fn create_session(
        &self,
        provider: &ProviderConfig,
        model: &str,
        context_length: Option<usize>,
        cwd: &Path,
    ) -> Result<SessionRecord> {
        let prompt = self.system_prompt(&provider.id, model, context_length, cwd)?;
        self.store.create_session(NewSession {
            cwd: cwd.display().to_string().replace('\\', "/"),
            provider: provider.id.clone(),
            model: model.to_string(),
            system_prompt: prompt,
        })
    }

    /// Switches a session to another model. Like Hermes, this rebuilds the frozen system
    /// prompt (model-family guidance differs), so the next request misses the prompt cache.
    pub fn set_session_model(
        &self,
        session_id: &str,
        model: &str,
        context_length: Option<usize>,
    ) -> Result<()> {
        let open = self.open(session_id)?;
        anyhow::ensure!(
            open.running.lock().is_none(),
            "the model can't change while a turn is running"
        );
        let (provider, cwd) = {
            let record = open.record.lock();
            (record.provider.clone(), PathBuf::from(&record.cwd))
        };
        let prompt = self.system_prompt(&provider, model, context_length, &cwd)?;
        self.store.set_model(session_id, model, &prompt)?;
        let mut record = open.record.lock();
        record.model = model.to_string();
        record.system_prompt = prompt;
        Ok(())
    }

    fn system_prompt(
        &self,
        provider: &str,
        model: &str,
        context_length: Option<usize>,
        cwd: &Path,
    ) -> Result<String> {
        let home = dirs::home_dir().context("no home directory")?;
        let soul = std::fs::read_to_string(self.config.data_dir.join("SOUL.md")).ok();
        let tool_names = SessionTools::tool_names();
        let skills_index = self.skills.index_prompt();
        // Memory as it is now; writes during the session don't change this prompt.
        let snapshot = self.memory.snapshot();
        let memory = [snapshot.memory, snapshot.user]
            .into_iter()
            .flatten()
            .reduce(|a, b| format!("{a}\n\n{b}"));
        // The scratch dir is per session but the prompt must name it before the id exists, so
        // sessions share the scratch root in the prompt and get their own subdirectory below.
        let scratch_root = self.config.data_dir.join("scratch");
        let prompt = build_system_prompt(&SystemPromptInput {
            model,
            provider,
            cwd,
            home: &home,
            scratch_dir: &scratch_root,
            shell: match self.config.shell {
                ShellKind::GitBash(_) => Shell::GitBash,
                ShellKind::PowerShell => Shell::PowerShell,
            },
            windows_version: &self.windows_version,
            tool_names: &tool_names,
            soul: soul.as_deref(),
            memory: memory.as_deref(),
            skills_index: skills_index.as_deref(),
            context_length,
            now: chrono::Local::now(),
        });
        Ok(prompt.full())
    }

    fn open(&self, session_id: &str) -> Result<Arc<OpenSession>> {
        if let Some(open) = self.sessions.lock().get(session_id) {
            return Ok(open.clone());
        }
        let record = self
            .store
            .session(session_id)?
            .with_context(|| format!("no session {session_id}"))?;
        let conversation = Conversation::new_unvalidated(self.store.load_messages(session_id)?);
        let tools = SessionTools::new(SessionToolsConfig {
            session_id: session_id.to_string(),
            cwd: PathBuf::from(&record.cwd),
            home: dirs::home_dir().context("no home directory")?,
            scratch_dir: self.scratch_dir(session_id),
            shell: self.config.shell.clone(),
            bridge: self.bridge.clone(),
            todos: restore_todos(&conversation),
            memory: Some(self.memory.clone()),
            history: Some(self.store.clone()),
            skills: Some(self.skills.clone()),
        });
        let open = Arc::new(OpenSession {
            record: Mutex::new(record),
            conversation: Mutex::new(conversation),
            tools: Arc::new(tools),
            running: Mutex::new(None),
            turn_state: Mutex::new(None),
        });
        Ok(self
            .sessions
            .lock()
            .entry(session_id.to_string())
            .or_insert(open)
            .clone())
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .open(session_id)?
            .conversation
            .lock()
            .messages()
            .clone())
    }

    /// Hermes's edit-and-resend: drops message `keep` and everything after it, so the next
    /// turn continues from the conversation as it was before that message. The todo list goes
    /// back to what it was then. Refused while a turn runs.
    pub fn rewind(&self, session_id: &str, keep: usize) -> Result<Vec<Message>> {
        let open = self.open(session_id)?;
        if open.running.lock().is_some() {
            anyhow::bail!("a turn is running in this session");
        }
        let conversation = {
            let mut conversation = open.conversation.lock();
            conversation.messages_mut().truncate(keep);
            conversation.clone()
        };
        self.store
            .replace_messages(session_id, conversation.messages())?;
        open.tools.reset_todos(restore_todos(&conversation));
        Ok(conversation.messages().clone())
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .get(session_id)
            .is_some_and(|open| open.running.lock().is_some())
    }

    /// Adds the user's message and runs the agent until it finishes, stops or is cancelled.
    pub fn start_turn(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: &str,
    ) -> Result<Turn> {
        self.begin_turn(session_id, provider, options, Some(text))
    }

    /// Hermes's `/retry`: drops the provider error that ended the last turn and runs the agent
    /// again from where it stopped, without a new user message.
    pub fn retry_turn(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
    ) -> Result<Turn> {
        self.begin_turn(session_id, provider, options, None)
    }

    fn begin_turn(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: Option<&str>,
    ) -> Result<Turn> {
        let TurnOptions {
            context_length,
            effort,
        } = options;
        let effort_params = effort
            .as_deref()
            .map(|effort| effort_request_params(&provider.base_url, effort));
        let open = self.open(session_id)?;
        let cancel = CancellationToken::new();
        {
            let mut running = open.running.lock();
            anyhow::ensure!(
                running.is_none(),
                "a turn is already running in this session"
            );
            *running = Some(cancel.clone());
        }
        let model = open.record.lock().model.clone();
        let llm = match provider.provider() {
            Ok(llm) => llm,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let state: ops::SharedTurnState = Arc::default();
        *open.turn_state.lock() = Some(state.clone());
        open.tools.begin_turn();
        let runtime = TurnRuntime {
            session: open.clone(),
            store: self.store.clone(),
            state: state.clone(),
        };
        let first_event = match text {
            Some(text) => {
                let user = Message::user()
                    .with_text(text)
                    .with_generated_id_if_missing();
                runtime
                    .append(user.clone())
                    .map(|()| AgentEvent::Message(user))
            }
            None => runtime
                .drop_trailing_errors()
                .and_then(|conversation| conversation.context("there is no failed turn to retry"))
                .map(AgentEvent::HistoryReplaced),
        };
        let first_event = match first_event {
            Ok(event) => event,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let _ = events_tx.send(RuntimeEvent::Agent(first_event));
        let max_iterations = self.config.max_iterations;
        let session_id = session_id.to_string();
        let turn_cancel = cancel.clone();
        tokio::spawn(async move {
            let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
            let forward_tx = events_tx.clone();
            let forward = tokio::spawn(async move {
                while let Some(event) = agent_rx.recv().await {
                    let _ = forward_tx.send(RuntimeEvent::Agent(event));
                }
            });
            let emit = Emitter::new(agent_tx, turn_cancel.clone());
            let tool_names: Vec<String> = SessionTools::tool_names()
                .into_iter()
                .map(str::to_string)
                .collect();
            let tools: Arc<dyn ToolProvider<TurnSession>> = Arc::new(ops::GuardedTools {
                inner: runtime.session.tools.clone(),
                state: state.clone(),
                result_limit: ops::result_size_limit(context_length),
            });
            let mut model_config = ModelConfig::new(&model).with_context_limit(context_length);
            if let Some(params) = effort_params {
                model_config = model_config.with_merged_request_params(params);
            }
            let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
                Step::Operation(Arc::new(ops::FrozenPrompt)),
                Step::Operation(Arc::new(ops::IterationBudget {
                    state: state.clone(),
                    max_iterations,
                })),
                Step::Operation(Arc::new(ops::ValidateToolCalls {
                    state: state.clone(),
                    tool_names: tool_names.clone(),
                })),
                Step::Operation(Arc::new(ToolOperation::new().with_provider(tools))),
                Step::Operation(Arc::new(ops::UnhandledToolCalls { tool_names })),
                Step::Operation(Arc::new(ops::DeliverSteer {
                    state: state.clone(),
                })),
                Step::Operation(Arc::new(ops::LengthContinuation {
                    state: state.clone(),
                })),
                Step::Operation(Arc::new(ops::EmptyResponseRecovery {
                    state: state.clone(),
                })),
                Step::Operation(Arc::new(ops::ExitOnError)),
                Step::Inference(Arc::new(InferenceRunner::new(llm, model_config))),
            ];
            let machine = StateMachine::new(steps, turn_cancel.clone());
            let result = machine.run(&runtime, &session_id, &emit).await;
            drop(machine);
            drop(emit);
            let _ = forward.await;
            *runtime.session.running.lock() = None;
            *runtime.session.turn_state.lock() = None;
            if let Some(text) = state.lock().pending_steer.take() {
                let _ = events_tx.send(RuntimeEvent::SteerUndelivered(text));
            }
            let status = match result {
                Ok(_) if turn_cancel.is_cancelled() => TurnStatus::Cancelled,
                Ok(_) => TurnStatus::Completed,
                Err(error) => {
                    tracing::error!("turn failed: {error:#}");
                    TurnStatus::Failed(format!("{error:#}"))
                }
            };
            let _ = events_tx.send(RuntimeEvent::TurnFinished(status));
        });
        Ok(Turn {
            events: events_rx,
            cancel,
        })
    }

    /// Hermes's `/steer`: a message for the running turn, delivered to the model right after its
    /// next tool results. `false` when no turn is running (send it as a normal message instead).
    pub fn steer(&self, session_id: &str, text: &str) -> bool {
        let state = self
            .sessions
            .lock()
            .get(session_id)
            .and_then(|open| open.turn_state.lock().clone());
        let Some(state) = state else {
            return false;
        };
        let mut state = state.lock();
        state.pending_steer = Some(match state.pending_steer.take() {
            Some(earlier) => format!("{earlier}\n\n{text}"),
            None => text.to_string(),
        });
        true
    }

    pub fn cancel(&self, session_id: &str) {
        if let Some(open) = self.sessions.lock().get(session_id) {
            if let Some(cancel) = open.running.lock().as_ref() {
                cancel.cancel();
            }
        }
    }

    /// Stops the session's shell and background processes and forgets its in-memory state.
    pub async fn close(&self, session_id: &str) {
        let open = self.sessions.lock().remove(session_id);
        if let Some(open) = open {
            if let Some(cancel) = open.running.lock().as_ref() {
                cancel.cancel();
            }
            open.tools.shutdown().await;
        }
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.close(session_id).await;
        self.store.delete_session(session_id)?;
        let _ = std::fs::remove_dir_all(self.scratch_dir(session_id));
        Ok(())
    }
}

/// The todo list from the last successful `todo_list` call, so it survives reopening a session.
fn restore_todos(conversation: &Conversation) -> Vec<TodoItem> {
    let todo_calls: std::collections::HashSet<&str> = conversation
        .messages()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(MessageContent::as_tool_request)
        .filter(|request| {
            request
                .tool_call
                .as_ref()
                .is_ok_and(|call| call.name == "todo_list")
        })
        .map(|request| request.id.as_str())
        .collect();
    conversation
        .messages()
        .iter()
        .rev()
        .flat_map(|message| message.content.iter().rev())
        .filter_map(|content| {
            let response = content.as_tool_response()?;
            todo_calls
                .contains(response.id.as_str())
                .then(|| content.as_tool_response_text())?
        })
        .find_map(|text| {
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            serde_json::from_value(value.get("todos")?.clone()).ok()
        })
        .unwrap_or_default()
}

/// "11 (build 26200)", from `ver`; Windows 11 still reports 10.0 so the build number decides.
fn windows_version() -> String {
    use std::os::windows::process::CommandExt as _;
    let output = std::process::Command::new("cmd")
        .args(["/C", "ver"])
        .creation_flags(0x0800_0000)
        .output();
    let text = output
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let build = text
        .split("Version ")
        .nth(1)
        .and_then(|version| version.split('.').nth(2))
        .and_then(|build| {
            build
                .trim_end_matches([']', '\r', '\n'])
                .parse::<u32>()
                .ok()
        });
    match build {
        Some(build) if build >= 22000 => format!("11 (build {build})"),
        Some(build) => format!("10 (build {build})"),
        None => "unknown version".into(),
    }
}
