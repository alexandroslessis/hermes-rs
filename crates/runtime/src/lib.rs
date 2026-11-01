//! The agent runtime the desktop app talks to: create and open sessions, start a turn and
//! receive its events, cancel it, list a provider's models.

mod commands;
pub mod compress;
mod curator;
mod guards;
mod ops;
mod review;
mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

pub use curator::{CuratorModel, CuratorOutcome};
pub use hermes_tools::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse, ProcessNotice,
};
/// The operation note on a command report (`/rollback`, `/diff`) in the chat.
pub const COMMAND_NOTE: &str = "command";

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
        let mut models =
            hermes_llm::discovery::list_models(&self.client()?, &self.base_url).await?;
        // Ollama lists bare ids; its native API, beside `/v1`, has the context windows.
        let base = self.base_url.trim_end_matches('/');
        if let Some(root) = base.strip_suffix("/v1")
            && models.iter().any(|model| model.context_length.is_none())
        {
            let root = ApiClient::new_with_tls(root.to_string(), AuthMethod::NoAuth, None)?;
            hermes_llm::discovery::enrich_from_ollama(&root, &mut models).await;
        }
        Ok(models)
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

/// Estimated tokens per part of the request (characters / 4, like Hermes's breakdown).
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextBreakdown {
    pub system_prompt: usize,
    pub tool_definitions: usize,
    pub skills: usize,
    pub memory: usize,
    pub conversation: usize,
}

impl ContextBreakdown {
    pub fn total(&self) -> usize {
        self.system_prompt + self.tool_definitions + self.skills + self.memory + self.conversation
    }
}

pub struct Turn {
    pub events: mpsc::UnboundedReceiver<RuntimeEvent>,
    pub cancel: CancellationToken,
}

pub struct Runtime {
    store: Arc<Store>,
    memory: Arc<MemoryStore>,
    skills: Arc<SkillStore>,
    /// The dangerous-command policy every session shares.
    approvals: Arc<hermes_tools::approval::ApprovalStore>,
    /// Filesystem checkpoints every session shares (Hermes: opt-in).
    checkpoints: Arc<hermes_tools::checkpoints::Checkpoints>,
    /// The web tools every session shares.
    web: Arc<hermes_tools::web::Web>,
    config: RuntimeConfig,
    bridge: Arc<dyn UserBridge>,
    windows_version: String,
    sessions: Mutex<HashMap<String, Arc<OpenSession>>>,
    /// Hermes's `background_review.enabled`: review memory and skills after turns.
    background_review: Arc<AtomicBool>,
    /// When a turn last started or ended: the curator waits for the app to be idle.
    last_activity: Arc<Mutex<std::time::Instant>>,
    curator_running: AtomicBool,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, bridge: Arc<dyn UserBridge>) -> Result<Self> {
        let store = Store::open(&config.data_dir.join("sessions.db"))?;
        let memory = MemoryStore::new(config.data_dir.join("memories"), config.memory.clone());
        let skills = SkillStore::new(config.data_dir.join("skills"), config.skills.clone());
        skills.seed_bundled();
        let approvals = Arc::new(hermes_tools::approval::ApprovalStore::new(&config.data_dir));
        let checkpoints = Arc::new(hermes_tools::checkpoints::Checkpoints::new(
            config.data_dir.join("checkpoints"),
            false,
        ));
        Ok(Self {
            store: Arc::new(store),
            memory: Arc::new(memory),
            skills: Arc::new(skills),
            approvals,
            checkpoints,
            web: Arc::new(hermes_tools::web::Web::new(hermes_tools::web::cache_dir(
                &config.data_dir,
            ))),
            windows_version: windows_version(),
            config,
            bridge,
            sessions: Mutex::new(HashMap::new()),
            background_review: Arc::new(AtomicBool::new(true)),
            last_activity: Arc::new(Mutex::new(std::time::Instant::now())),
            curator_running: AtomicBool::new(false),
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
    /// the skill, and `/bundle-name instruction` the one that loads a bundle's skills (a bundle
    /// wins over a skill of the same name). `None` when `text` names neither.
    pub fn skill_command(&self, text: &str) -> Option<String> {
        let rest = text.trim().strip_prefix('/')?;
        let (name, instruction) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        self.skills
            .bundle_message(name, instruction)
            .or_else(|| self.skills.invocation_message(name, instruction))
    }

    /// Turns automatic background reviews (memory and skills, after turns) on or off.
    pub fn set_background_review(&self, enabled: bool) {
        self.background_review.store(enabled, Ordering::Relaxed);
    }

    /// The dangerous-command policy (Settings shows and edits it).
    pub fn approvals(&self) -> Arc<hermes_tools::approval::ApprovalStore> {
        self.approvals.clone()
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
        self.system_prompt_with(provider, model, context_length, cwd, true)
    }

    /// The frozen prompt; `with_memory` false leaves the memory snapshot out (Hermes's curator
    /// runs with `skip_memory`).
    fn system_prompt_with(
        &self,
        provider: &str,
        model: &str,
        context_length: Option<usize>,
        cwd: &Path,
        with_memory: bool,
    ) -> Result<String> {
        self.prompt_source()
            .build(provider, model, context_length, cwd, with_memory)
    }

    fn prompt_source(&self) -> PromptSource {
        PromptSource {
            data_dir: self.config.data_dir.clone(),
            shell: self.config.shell.clone(),
            windows_version: self.windows_version.clone(),
            store: self.store.clone(),
            memory: self.memory.clone(),
            skills: self.skills.clone(),
        }
    }

    /// Rebuilds a session's frozen prompt as it would be built now (after a compaction).
    fn prompt_rebuild(
        &self,
        open: &OpenSession,
        context_length: Option<usize>,
    ) -> compress::PromptRebuild {
        let source = self.prompt_source();
        let (provider, model, cwd) = {
            let record = open.record.lock();
            (
                record.provider.clone(),
                record.model.clone(),
                PathBuf::from(&record.cwd),
            )
        };
        Arc::new(move || source.build(&provider, &model, context_length, &cwd, true))
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
            approvals: Some(self.approvals.clone()),
            checkpoints: Some(self.checkpoints.clone()),
            web: Some(self.web.clone()),
        });
        let user_turns = conversation
            .messages()
            .iter()
            .filter(|m| m.role == rmcp::model::Role::User)
            .filter(|m| {
                m.content
                    .iter()
                    .any(|c| matches!(c, MessageContent::Text(_)))
            })
            .count();
        let open = Arc::new(OpenSession {
            record: Mutex::new(record),
            conversation: Mutex::new(conversation),
            tools: Arc::new(tools),
            running: Mutex::new(None),
            turn_state: Mutex::new(None),
            review: Mutex::new(None),
            nudges: Mutex::new(review::Nudges::resumed(user_turns)),
            context: Mutex::new(None),
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
            // Rewound past a compaction's handoff: the turns it summarized come back.
            compress::undo(&mut conversation);
            conversation.clone()
        };
        *open.context.lock() = None;
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
            persist: true,
            record_usage: true,
        };
        // A live turn supersedes a review still running from the last one (Hermes cancels it so
        // the two don't compete for the model).
        if let Some(review) = open.review.lock().take() {
            review.cancel();
        }
        let review_memory = text.is_some() && open.nudges.lock().tick_memory();
        let review_setup = ReviewSetup {
            store: self.store.clone(),
            bridge: self.bridge.clone(),
            llm: llm.clone(),
            model: model.clone(),
            context_length,
            effort_params: effort_params.clone(),
            enabled: self.background_review.clone(),
        };
        // A message with `@url:` references is added once its pages are fetched, in the turn's
        // task (fetching can take seconds).
        let deferred = text
            .filter(|t| !hermes_tools::references::url_targets(t).is_empty())
            .map(str::to_string);
        let first_event = match text {
            Some(_) if deferred.is_some() => Ok(None),
            Some(text) => {
                user_message(&open, text, context_length, &Default::default()).and_then(|user| {
                    runtime
                        .append(user.clone())
                        .map(|()| Some(AgentEvent::Message(user)))
                })
            }
            None => runtime
                .drop_trailing_errors()
                .and_then(|conversation| conversation.context("there is no failed turn to retry"))
                .map(|conversation| Some(AgentEvent::HistoryReplaced(conversation))),
        };
        let first_event = match first_event {
            Ok(event) => event,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let mut turn_start = open.conversation.lock().messages().len();
        let web = self.web.clone();
        let rebuild_prompt = self.prompt_rebuild(&open, context_length);
        let summarizer_llm = llm.clone();
        *self.last_activity.lock() = std::time::Instant::now();
        let last_activity = self.last_activity.clone();

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        if let Some(event) = first_event {
            let _ = events_tx.send(RuntimeEvent::Agent(event));
        }
        let max_iterations = self.config.max_iterations;
        let session_id = session_id.to_string();
        let turn_cancel = cancel.clone();
        tokio::spawn(async move {
            if let Some(text) = deferred {
                let mut pages = hermes_tools::references::FetchedPages::new();
                for url in hermes_tools::references::url_targets(&text) {
                    let page = tokio::select! {
                        page = web.fetch_for_reference(&url) => page,
                        _ = turn_cancel.cancelled() => Err("cancelled".into()),
                    };
                    pages.insert(url, page);
                }
                let added = user_message(&runtime.session, &text, context_length, &pages)
                    .and_then(|user| runtime.append(user.clone()).map(|()| user));
                match added {
                    Ok(user) if !turn_cancel.is_cancelled() => {
                        turn_start = runtime.session.conversation.lock().len();
                        let _ = events_tx.send(RuntimeEvent::Agent(AgentEvent::Message(user)));
                    }
                    outcome => {
                        *runtime.session.running.lock() = None;
                        *runtime.session.turn_state.lock() = None;
                        let status = match outcome {
                            Err(error) => TurnStatus::Failed(format!("{error:#}")),
                            Ok(_) => TurnStatus::Cancelled,
                        };
                        let _ = events_tx.send(RuntimeEvent::TurnFinished(status));
                        return;
                    }
                }
            }
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
                scratch_dir: runtime.session.tools.scratch_dir().to_path_buf(),
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
                Step::Operation(Arc::new(compress::AutoCompress {
                    state: state.clone(),
                    summarizer: compress::Summarizer {
                        llm: summarizer_llm,
                        model: model.clone(),
                        context_length,
                    },
                    rebuild_prompt,
                })),
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
            *last_activity.lock() = std::time::Instant::now();
            let finished = status == TurnStatus::Completed;
            let _ = events_tx.send(RuntimeEvent::TurnFinished(status));
            review_setup.after_turn(&runtime.session, turn_start, review_memory, finished);
        });
        Ok(Turn {
            events: events_rx,
            cancel,
        })
    }

    /// Hermes's `/refine [focus]`: reviews the conversation now, in the background, saving
    /// lessons to memory and skills. The result arrives through the bridge's `review_finished`.
    pub fn refine(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        focus: Option<String>,
    ) -> Result<()> {
        let open = self.open(session_id)?;
        anyhow::ensure!(
            open.running.lock().is_none(),
            "a turn is running in this session; refine it when it ends"
        );
        anyhow::ensure!(
            !open.conversation.lock().messages().is_empty(),
            "Nothing to refine yet — send a message first."
        );
        let setup = ReviewSetup {
            store: self.store.clone(),
            bridge: self.bridge.clone(),
            llm: provider.provider()?,
            model: open.record.lock().model.clone(),
            context_length: options.context_length,
            effort_params: options
                .effort
                .as_deref()
                .map(|effort| effort_request_params(&provider.base_url, effort)),
            enabled: self.background_review.clone(),
        };
        let skills = SessionTools::tool_names().contains(&"skill_manage");
        setup.start(&open, true, skills, focus, true);
        Ok(())
    }

    /// Hermes's `/compress [focus]`: summarizes the conversation now, between turns. Runs like a
    /// turn (it can be cancelled, and no turn starts meanwhile); the events end with
    /// `TurnFinished`, failed with "Nothing to compress yet." when there's nothing to summarize.
    pub fn compress(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        focus: Option<String>,
    ) -> Result<Turn> {
        let open = self.open(session_id)?;
        let cancel = CancellationToken::new();
        {
            let mut running = open.running.lock();
            anyhow::ensure!(
                running.is_none(),
                "a turn is running in this session; compress it when it ends"
            );
            *running = Some(cancel.clone());
        }
        let llm = match provider.provider() {
            Ok(llm) => llm,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let rebuild_prompt = self.prompt_rebuild(&open, options.context_length);
        let summarizer = compress::Summarizer {
            llm,
            model: open.record.lock().model.clone(),
            context_length: options.context_length,
        };
        let store = self.store.clone();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let turn_cancel = cancel.clone();
        tokio::spawn(async move {
            let _ = events_tx.send(RuntimeEvent::Agent(AgentEvent::Compacting));
            let conversation = open.conversation.lock().clone();
            let fixed = compress::estimate_text(&open.record.lock().system_prompt)
                + compress::tool_definition_tokens();
            let result = tokio::select! {
                result = compress::compact(&conversation, fixed, &summarizer, focus.as_deref(), None) => Some(result),
                _ = turn_cancel.cancelled() => None,
            };
            let status = match result {
                None => TurnStatus::Cancelled,
                Some(Ok(None)) => TurnStatus::Failed("Nothing to compress yet.".into()),
                Some(Err(error)) => TurnStatus::Failed(format!("{error:#}")),
                Some(Ok(Some(compacted))) => {
                    let prompt = rebuild_prompt().ok();
                    let conversation = compacted.conversation;
                    match open.apply_compaction(&store, conversation.clone(), prompt) {
                        Ok(()) => {
                            let _ = events_tx.send(RuntimeEvent::Agent(
                                AgentEvent::HistoryReplaced(conversation),
                            ));
                            let _ = events_tx.send(RuntimeEvent::Agent(AgentEvent::Compacted {
                                tokens: compacted.notice.tokens_after,
                            }));
                            TurnStatus::Completed
                        }
                        Err(error) => TurnStatus::Failed(format!("{error:#}")),
                    }
                }
            };
            *open.running.lock() = None;
            let _ = events_tx.send(RuntimeEvent::TurnFinished(status));
        });
        Ok(Turn {
            events: events_rx,
            cancel,
        })
    }

    /// What fills the context window, estimated (Hermes's `/context` breakdown): the frozen
    /// prompt split into its skills index, memory and the rest, the tool definitions, and the
    /// conversation the model sees.
    pub fn context_breakdown(&self, session_id: &str) -> Result<ContextBreakdown> {
        let open = self.open(session_id)?;
        let prompt = open.record.lock().system_prompt.clone();
        let skills = match (
            prompt.find("<available_skills>"),
            prompt.find("</available_skills>"),
        ) {
            (Some(start), Some(end)) if end > start => {
                compress::estimate_text(&prompt[start..end + "</available_skills>".len()])
            }
            _ => 0,
        };
        let snapshot = self.memory.snapshot();
        let memory = [snapshot.memory, snapshot.user]
            .into_iter()
            .flatten()
            .filter(|block| prompt.contains(block.as_str()))
            .map(|block| compress::estimate_text(&block))
            .sum::<usize>();
        let total = compress::estimate_text(&prompt);
        Ok(ContextBreakdown {
            system_prompt: total.saturating_sub(skills + memory),
            tool_definitions: compress::tool_definition_tokens(),
            skills,
            memory,
            conversation: compress::estimate_messages(open.conversation.lock().messages()),
        })
    }

    /// The filesystem checkpoints (Settings turns them on and off).
    pub fn checkpoints(&self) -> Arc<hermes_tools::checkpoints::Checkpoints> {
        self.checkpoints.clone()
    }

    /// Hermes's `/rollback [diff] [N] [file] [--all]`. A restore also undoes the last chat turn,
    /// so the conversation matches the files. Returns the conversation after it, which ends with
    /// the report.
    pub fn rollback(&self, session_id: &str, args: &str) -> Result<Vec<Message>> {
        let open = self.idle_session(session_id)?;
        let cwd = PathBuf::from(&open.record.lock().cwd);
        let home = dirs::home_dir().context("no home directory")?;
        let outcome = commands::rollback(&self.checkpoints, &cwd, &home, args);
        let mut report = outcome.report;
        if outcome.restored {
            let last_turn = open.conversation.lock().messages().iter().rposition(|m| {
                m.role == rmcp::model::Role::User
                    && m.is_user_visible()
                    && !m.is_tool_response()
                    && m.content
                        .iter()
                        .any(|c| matches!(c, MessageContent::Text(_)))
            });
            if let Some(index) = last_turn {
                self.rewind(session_id, index)?;
                report.push_str("\n\nChat turn undone to match restored file state.");
            }
        }
        self.add_command_output(&open, "rollback", &report)
    }

    /// Hermes's `/diff [working|staged|all|session] [--stat] [path…]`. Returns the conversation
    /// after it, which ends with the diff.
    pub fn diff(&self, session_id: &str, args: &str) -> Result<Vec<Message>> {
        let open = self.idle_session(session_id)?;
        let cwd = PathBuf::from(&open.record.lock().cwd);
        let home = dirs::home_dir().context("no home directory")?;
        let output = commands::diff(&self.checkpoints, &cwd, &home, args);
        self.add_command_output(&open, "diff", &output)
    }

    fn idle_session(&self, session_id: &str) -> Result<Arc<OpenSession>> {
        let open = self.open(session_id)?;
        anyhow::ensure!(
            open.running.lock().is_none(),
            "a turn is running in this session; wait for it to end"
        );
        Ok(open)
    }

    /// A command's report in the chat: shown to the user, never sent to the model.
    fn add_command_output(
        &self,
        open: &OpenSession,
        command: &str,
        text: &str,
    ) -> Result<Vec<Message>> {
        let mut message = Message::assistant()
            .with_text(text)
            .with_visibility(true, false)
            .with_generated_id_if_missing();
        message.metadata.set_operation_note(
            COMMAND_NOTE,
            "command",
            serde_json::Value::String(command.to_string()),
        );
        let session_id = open.record.lock().id.clone();
        let (index, messages) = {
            let mut conversation = open.conversation.lock();
            conversation.push(message.clone());
            (conversation.len() - 1, conversation.messages().clone())
        };
        self.store.upsert_message(&session_id, index, &message)?;
        Ok(messages)
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

/// The user's message as the model gets it: with its `@` references expanded (Hermes's
/// `preprocess_context_references`), the typed text kept for the chat to show. Refused when the
/// attached context would take over half the window.
fn user_message(
    open: &OpenSession,
    text: &str,
    context_length: Option<usize>,
    pages: &hermes_tools::references::FetchedPages,
) -> Result<Message> {
    let cwd = PathBuf::from(&open.record.lock().cwd);
    let home = dirs::home_dir().context("no home directory")?;
    let Some(expanded) = hermes_tools::references::expand(text, &cwd, &home, context_length, pages)
    else {
        return Ok(Message::user()
            .with_text(text)
            .with_generated_id_if_missing());
    };
    anyhow::ensure!(!expanded.blocked, "{}", expanded.warnings.join("\n"));
    let mut message = Message::user()
        .with_text(expanded.message)
        .with_generated_id_if_missing();
    message.metadata.set_operation_note(
        hermes_tools::references::NOTE,
        "typed",
        serde_json::Value::String(text.to_string()),
    );
    Ok(message)
}

/// What the frozen system prompt is built from; cloned into a compaction so it can rebuild it.
#[derive(Clone)]
struct PromptSource {
    data_dir: PathBuf,
    shell: ShellKind,
    windows_version: String,
    store: Arc<Store>,
    memory: Arc<MemoryStore>,
    skills: Arc<SkillStore>,
}

impl PromptSource {
    fn build(
        &self,
        provider: &str,
        model: &str,
        context_length: Option<usize>,
        cwd: &Path,
        with_memory: bool,
    ) -> Result<String> {
        let home = dirs::home_dir().context("no home directory")?;
        let soul = std::fs::read_to_string(self.data_dir.join("SOUL.md")).ok();
        let tool_names = SessionTools::tool_names();
        let skills_index = self.skills.index_prompt();
        // Memory as it is now; writes during the session don't change this prompt.
        let snapshot = self.memory.snapshot();
        let memory = [snapshot.memory, snapshot.user]
            .into_iter()
            .flatten()
            .reduce(|a, b| format!("{a}\n\n{b}"))
            .filter(|_| with_memory);
        // The scratch dir is per session but the prompt must name it before the id exists, so
        // sessions share the scratch root in the prompt and get their own subdirectory below.
        let scratch_root = self.data_dir.join("scratch");
        // The project this folder belongs to, so the agent knows its other folders.
        let projects = self.store.list_projects().unwrap_or_default();
        let project = hermes_store::project_for_path(&projects, &cwd.display().to_string());
        let prompt = build_system_prompt(&SystemPromptInput {
            model,
            provider,
            cwd,
            home: &home,
            scratch_dir: &scratch_root,
            shell: match self.shell {
                ShellKind::GitBash(_) => Shell::GitBash,
                ShellKind::PowerShell => Shell::PowerShell,
            },
            windows_version: &self.windows_version,
            tool_names: &tool_names,
            soul: soul.as_deref(),
            memory: memory.as_deref(),
            skills_index: skills_index.as_deref(),
            project: project.map(|p| (p.name.as_str(), p.folders.as_slice())),
            context_length,
            now: chrono::Local::now(),
        });
        Ok(prompt.full())
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

/// What a turn needs to start a background review when it ends.
struct ReviewSetup {
    store: Arc<Store>,
    bridge: Arc<dyn UserBridge>,
    llm: Arc<dyn Provider>,
    model: String,
    context_length: Option<usize>,
    effort_params: Option<HashMap<String, serde_json::Value>>,
    enabled: Arc<AtomicBool>,
}

impl ReviewSetup {
    /// Hermes's turn finalizer: counts the turn toward the skill nudge, then reviews memory
    /// and/or skills when one is due, the turn ended with an answer, and reviews are on.
    fn after_turn(
        self,
        session: &Arc<OpenSession>,
        turn_start: usize,
        review_memory: bool,
        finished: bool,
    ) {
        let (review_skills, answered) = {
            let conversation = session.conversation.lock();
            let messages = conversation.messages();
            let turn = messages.get(turn_start..).unwrap_or_default();
            let review_skills = session.nudges.lock().finish_turn(turn);
            let answered = messages.last().is_some_and(|last| {
                last.role == rmcp::model::Role::Assistant
                    && last.error_kind().is_none()
                    && last.content.iter().any(|content| {
                        matches!(content, MessageContent::Text(text) if !text.text.trim().is_empty())
                    })
            });
            (review_skills, answered)
        };
        if !finished || !answered || !(review_memory || review_skills) {
            return;
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.start(session, review_memory, review_skills, None, false);
    }

    fn start(
        self,
        session: &Arc<OpenSession>,
        memory: bool,
        skills: bool,
        focus: Option<String>,
        requested: bool,
    ) {
        let cancel = CancellationToken::new();
        if let Some(previous) = session.review.lock().replace(cancel.clone()) {
            previous.cancel();
        }
        tokio::spawn(review::run(review::ReviewJob {
            session: session.clone(),
            store: self.store,
            bridge: self.bridge,
            llm: self.llm,
            model: self.model,
            context_length: self.context_length,
            effort_params: self.effort_params,
            memory,
            skills,
            focus,
            requested,
            cancel,
        }));
    }
}
