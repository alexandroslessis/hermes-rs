//! The agent runtime the desktop app talks to: create and open sessions, start a turn and
//! receive its events, cancel it, list a provider's models.

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
use hermes_prompts::{Shell, SystemPromptInput, build_system_prompt};
use hermes_store::{NewSession, SessionRecord, Store};
use hermes_tools::{SessionTools, SessionToolsConfig, ShellKind, TodoItem, UserBridge};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use hermes_tools::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse, ProcessNotice,
};
pub use ops::MAX_TURNS_MESSAGE;
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

pub struct RuntimeConfig {
    /// `%APPDATA%\hermes-rs`: sessions.db, SOUL.md, scratch/.
    pub data_dir: PathBuf,
    pub shell: ShellKind,
    /// Model calls allowed per user message before the agent stops and asks to continue.
    pub max_turns: u32,
}

impl RuntimeConfig {
    pub fn for_current_user() -> Result<Self> {
        let data_dir = dirs::data_dir()
            .context("no application data directory")?
            .join("hermes-rs");
        Ok(Self {
            data_dir,
            shell: hermes_tools::detect_shell(),
            max_turns: 150,
        })
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Agent(AgentEvent),
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
    config: RuntimeConfig,
    bridge: Arc<dyn UserBridge>,
    windows_version: String,
    sessions: Mutex<HashMap<String, Arc<OpenSession>>>,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, bridge: Arc<dyn UserBridge>) -> Result<Self> {
        let store = Store::open(&config.data_dir.join("sessions.db"))?;
        Ok(Self {
            store: Arc::new(store),
            windows_version: windows_version(),
            config,
            bridge,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
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
            memory: None,
            skills_index: None,
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
        });
        let open = Arc::new(OpenSession {
            record: Mutex::new(record),
            conversation: Mutex::new(conversation),
            tools: Arc::new(tools),
            running: Mutex::new(None),
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
        context_length: Option<usize>,
        text: &str,
    ) -> Result<Turn> {
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
        let runtime = TurnRuntime {
            session: open.clone(),
            store: self.store.clone(),
        };
        let user = Message::user()
            .with_text(text)
            .with_generated_id_if_missing();
        if let Err(error) = runtime.append(user.clone()) {
            *open.running.lock() = None;
            return Err(error);
        }

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let _ = events_tx.send(RuntimeEvent::Agent(AgentEvent::Message(user)));
        let max_turns = self.config.max_turns;
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
            let tool_names = SessionTools::tool_names()
                .into_iter()
                .map(str::to_string)
                .collect();
            let tools: Arc<dyn ToolProvider<TurnSession>> = runtime.session.tools.clone();
            let model_config = ModelConfig::new(&model).with_context_limit(context_length);
            let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
                Step::Operation(Arc::new(ops::FrozenPrompt)),
                Step::Operation(Arc::new(ToolOperation::new().with_provider(tools))),
                Step::Operation(Arc::new(ops::UnhandledToolCalls { tool_names })),
                Step::Operation(Arc::new(ops::EmptyResponse)),
                Step::Operation(Arc::new(ops::ExitOnError)),
                Step::Operation(Arc::new(ops::MaxTurns { max_turns })),
                Step::Inference(Arc::new(InferenceRunner::new(llm, model_config))),
            ];
            let machine = StateMachine::new(steps, turn_cancel.clone());
            let result = machine.run(&runtime, &session_id, &emit).await;
            drop(machine);
            drop(emit);
            let _ = forward.await;
            *runtime.session.running.lock() = None;
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
