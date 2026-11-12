//! The agent runtime the desktop app talks to: create and open sessions, start a turn and
//! receive its events, cancel it, list a provider's models.

pub mod budget;
pub mod changes;
mod commands;
pub mod compress;
mod curator;
mod delegate;
mod guards;
pub mod governor;
pub mod ledger;
pub mod mcp;
mod ops;
mod review;
mod session;
pub mod subagents;
pub mod titles;
mod turnlock;

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
use hermes_llm::observer::Purpose;
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
        Ok(ApiClient::new_with_tls(self.base_url.trim_end_matches('/').to_string(), auth, None)?
            .with_provider_id(self.id.clone()))
    }

    /// A provider with the default retry count (side jobs; turns use [`Self::provider_with`]).
    fn provider(&self) -> Result<Arc<dyn Provider>> {
        self.provider_with(budget::BudgetKnobs::BALANCED.http_retries)
    }

    /// A provider that retries a failed HTTP request `http_retries` times (the budget knob).
    fn provider_with(&self, http_retries: usize) -> Result<Arc<dyn Provider>> {
        Ok(Arc::new(
            OpenAiCompatibleProvider::new(self.id.clone(), self.client()?, String::new())
                .with_max_retries(http_retries),
        ))
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
    /// The model accepts images (its discovered input modalities): attached images and
    /// `vision_analyze` results are sent to it.
    pub supports_vision: bool,
    /// Models for side jobs (Hermes's `auxiliary.*`); each falls back to the turn's model.
    pub aux: AuxModels,
}

/// A model for a side job, on its own provider.
#[derive(Debug, Clone)]
pub struct AuxModel {
    pub provider: ProviderConfig,
    pub model: String,
    pub context_length: Option<usize>,
    /// The job's own reasoning effort (Hermes's `auxiliary.<task>.reasoning_effort`); `None`
    /// keeps the job's default (the provider's, or off for the approval judge).
    pub effort: Option<String>,
}

/// Hermes's `auxiliary.compression` and `auxiliary.background_review` (the curator's model is
/// chosen with [`CuratorModel`]).
#[derive(Debug, Clone, Default)]
pub struct AuxModels {
    pub compression: Option<AuxModel>,
    pub review: Option<AuxModel>,
    /// Judges flagged commands under smart approvals (`auxiliary.approval`).
    pub approval: Option<AuxModel>,
}

/// Hermes's smart-approval reviewer (`_smart_approve`): one word from a model, reasoning off.
struct SmartGuard {
    llm: Arc<dyn Provider>,
    model: String,
    base_url: String,
    /// The approval model's own effort (Settings ▸ Model ▸ Auxiliary); off when unset.
    effort: Option<String>,
    /// The quota governor and the id of the provider the judge calls; `None` = no accounting.
    governor: Option<(governor::Governor, String)>,
}

#[async_trait::async_trait]
impl hermes_tools::approval::ApprovalGuard for SmartGuard {
    async fn assess(&self, command: &str, description: &str) -> hermes_tools::approval::Verdict {
        use hermes_tools::approval::{self, Verdict};
        // One word, but a reasoning model needs room to think first when effort is on.
        let thinking = self
            .effort
            .as_deref()
            .is_some_and(|effort| effort != "none");
        let config = ModelConfig::new(&self.model)
            .with_max_tokens(Some(if thinking { 4096 } else { 64 }))
            .with_temperature(Some(0.0))
            .with_merged_request_params(effort_request_params(
                &self.base_url,
                self.effort.as_deref().unwrap_or("none"),
            ));
        // Low on quota: don't spend a request on the judge, ask the user instead.
        if let Some((governor, provider)) = &self.governor
            && let governor::Verdict::Deny(why) = governor.check(provider, Purpose::Approval)
        {
            tracing::info!("smart approval skipped: {why}");
            return Verdict::Escalate;
        }
        let request =
            [Message::user().with_text(approval::smart_user_prompt(command, description))];
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            hermes_llm::observer::with_purpose(
                Purpose::Approval,
                self.llm
                    .complete(&config, approval::SMART_SYSTEM_PROMPT, &request, &[]),
            ),
        )
        .await;
        match reply {
            Ok(Ok((message, _))) => {
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        MessageContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let verdict = approval::parse_verdict(&text);
                tracing::info!("smart approval: {verdict:?} for {description}");
                verdict
            }
            _ => {
                tracing::warn!("smart approval: reviewer failed or timed out, escalating");
                Verdict::Escalate
            }
        }
    }
}

/// Who writes compaction summaries: the auxiliary compression model, else the session's.
/// `window` stays the session model's (it decides when to compress).
fn summarizer_for(
    aux: &AuxModels,
    llm: &Arc<dyn Provider>,
    model: &str,
    context_length: Option<usize>,
    budget: &budget::BudgetKnobs,
) -> Result<compress::Summarizer> {
    let session = compress::Summarizer {
        llm: llm.clone(),
        model: model.to_string(),
        context_length,
        window: context_length,
        request_params: None,
        compress_at: budget.compress_at,
        fallback: None,
        deadline: None,
    };
    Ok(match &aux.compression {
        // A local server that is down must not cost the compaction: one retry at most (a
        // refused connection answers at once, backing off would stall the turn for seconds),
        // then the session's model writes the summary, as it does with no aux model.
        Some(aux) => compress::Summarizer {
            llm: aux.provider.provider_with(budget.http_retries.min(1))?,
            model: aux.model.clone(),
            context_length: aux.context_length,
            window: context_length,
            request_params: aux_effort_params(aux),
            compress_at: budget.compress_at,
            fallback: Some(Box::new(session)),
            // A hung server (connection taken, no answer) is bounded too.
            deadline: Some(compress::AUX_SUMMARY_DEADLINE),
        },
        None => session,
    })
}

/// The tools' share of the budget knobs (`hermes_tools` doesn't know the profiles).
fn tool_limits(budget: &budget::BudgetKnobs) -> hermes_tools::ToolLimits {
    hermes_tools::ToolLimits {
        read_file_max_lines: budget.read_file_max_lines,
        search_context_factor: budget.search_context_factor,
    }
}

/// A side job's request fields for its chosen effort, if it has one.
fn aux_effort_params(aux: &AuxModel) -> Option<HashMap<String, serde_json::Value>> {
    aux.effort
        .as_deref()
        .map(|effort| effort_request_params(&aux.provider.base_url, effort))
}

/// Hermes's `generate_title`: a 3–7 word name for a session from its opening message, written by
/// `model` (reasoning turned off). `None` when the model gave nothing usable.
pub async fn generate_title(
    provider: &ProviderConfig,
    model: &str,
    opening: &str,
) -> Result<Option<String>> {
    generate_title_with_effort(provider, model, None, opening).await
}

/// [`generate_title`] with the title model's own reasoning effort (`auxiliary.title_generation`);
/// `None` turns reasoning off, as Hermes does for titles.
pub async fn generate_title_with_effort(
    provider: &ProviderConfig,
    model: &str,
    effort: Option<&str>,
    opening: &str,
) -> Result<Option<String>> {
    use hermes_prompts::titles;
    let llm = provider.provider()?;
    let snippet: String = opening
        .chars()
        .take(titles::MAX_TITLE_INPUT_CHARS)
        .collect();
    let config = ModelConfig::new(model)
        .with_max_tokens(Some(if effort.is_some_and(|e| e != "none") {
            // A thinking model spends tokens before the title.
            4096
        } else {
            titles::TITLE_MAX_TOKENS
        }))
        .with_merged_request_params(effort_request_params(
            &provider.base_url,
            effort.unwrap_or("none"),
        ));
    let (reply, _) = llm
        .complete(
            &config,
            titles::TITLE_PROMPT,
            &[Message::user().with_text(snippet)],
            &[],
        )
        .await
        .map_err(|error| anyhow::anyhow!("title request failed: {error}"))?;
    let text: String = reply
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    Ok(titles::parse_title(&text))
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
    /// The budget knobs a run starts with (iteration budget, retries, compression thresholds,
    /// limits; see [`budget`]): the default profile, `requests`. The desktop replaces them from
    /// the `budget` config ([`Runtime::set_budget`]); every turn copies the current ones once as
    /// it starts.
    pub budget: budget::BudgetKnobs,
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
            budget: budget::BudgetKnobs::default(),
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
    /// Something the user should know before the turn goes on (the daily request quota is nearly
    /// used up); the turn is not affected.
    Notice(String),
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

/// A `/bg` task (`Runtime::background`): its id, its subagent's events, and its answer.
pub struct Background {
    pub task_id: String,
    pub events: mpsc::UnboundedReceiver<AgentEvent>,
    pub done: tokio::task::JoinHandle<String>,
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
    /// Counts every model request per provider and purpose (`requests.json`).
    ledger: ledger::Ledger,
    /// The configured MCP servers (Settings), shared by every session.
    mcp: Arc<mcp::McpManager>,
    config: RuntimeConfig,
    bridge: Arc<dyn UserBridge>,
    windows_version: String,
    sessions: Mutex<HashMap<String, Arc<OpenSession>>>,
    /// Hermes's `background_review.enabled`: review memory and skills after turns.
    background_review: Arc<AtomicBool>,
    /// When a turn last started or ended: the curator waits for the app to be idle.
    last_activity: Arc<Mutex<std::time::Instant>>,
    /// The `delegate_task` children running now (the page lists, steers and stops them).
    subagents: Arc<subagents::Subagents>,
    /// Tools the user switched off (Capabilities ▸ Tools); each turn takes them as it starts.
    disabled_tools: Mutex<std::collections::HashSet<String>>,
    /// The knobs now (the config's `budget`): each turn and each new session takes them as it
    /// starts.
    budget: Mutex<budget::BudgetKnobs>,
    curator_running: AtomicBool,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, bridge: Arc<dyn UserBridge>) -> Result<Self> {
        let store = Store::open(&config.data_dir.join("sessions.db"))?;
        let memory = MemoryStore::new(config.data_dir.join("memories"), config.memory.clone());
        let skills = Arc::new(SkillStore::new(
            config.data_dir.join("skills"),
            config.skills.clone(),
        ));
        // The files now; their git history off the startup path.
        skills.seed_bundled_in_background();
        let approvals = Arc::new(hermes_tools::approval::ApprovalStore::new(&config.data_dir));
        let checkpoints = Arc::new(hermes_tools::checkpoints::Checkpoints::new(
            config.data_dir.join("checkpoints"),
            false,
        ));
        let ledger = ledger::Ledger::open(config.data_dir.join("requests.json"));
        let initial_budget = config.budget;
        Ok(Self {
            store: Arc::new(store),
            memory: Arc::new(memory),
            skills,
            approvals,
            checkpoints,
            mcp: Arc::default(),
            web: Arc::new(hermes_tools::web::Web::new(hermes_tools::web::cache_dir(
                &config.data_dir,
            ))),
            windows_version: windows_version(),
            ledger,
            config,
            bridge,
            sessions: Mutex::new(HashMap::new()),
            background_review: Arc::new(AtomicBool::new(true)),
            last_activity: Arc::new(Mutex::new(std::time::Instant::now())),
            subagents: Arc::default(),
            disabled_tools: Mutex::new(Default::default()),
            budget: Mutex::new(initial_budget),
            curator_running: AtomicBool::new(false),
        })
    }

    /// The budget knobs as they are now. A turn copies them once as it starts; a session's
    /// system prompt takes its budget wording from them when it is made.
    pub fn budget(&self) -> budget::BudgetKnobs {
        *self.budget.lock()
    }

    /// Replaces the budget knobs (the `budget` config changed). Applies from the next turn;
    /// what changes the prompt or the tool list applies from the next session, because a
    /// session's prompt is frozen (prompt caching).
    pub fn set_budget(&self, knobs: budget::BudgetKnobs) {
        *self.budget.lock() = knobs;
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The request ledger: today's model requests per provider and purpose.
    pub fn ledger(&self) -> &ledger::Ledger {
        &self.ledger
    }

    /// The quota governor over the ledger (see [`governor`]).
    pub fn governor(&self) -> governor::Governor {
        governor::Governor::new(self.ledger.clone())
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
        self.set_session_route(session_id, None, model, context_length)
    }

    /// Switches a session to another model, on another provider when `provider` names one (the
    /// next turn is sent there). Rebuilds the frozen prompt like [`Self::set_session_model`].
    pub fn set_session_route(
        &self,
        session_id: &str,
        provider: Option<&str>,
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
            (
                provider.map_or_else(|| record.provider.clone(), str::to_string),
                PathBuf::from(&record.cwd),
            )
        };
        let prompt = self.system_prompt(&provider, model, context_length, &cwd)?;
        self.store
            .set_provider_model(session_id, &provider, model, &prompt)?;
        let mut record = open.record.lock();
        record.provider = provider;
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
        self.prompt_source(self.budget())
            .build(provider, model, context_length, cwd, with_memory)
    }

    /// Builds a throwaway system prompt on a background thread, so what's set up on first use
    /// (the threat-scan patterns, the skills index) is ready before the first new session.
    pub fn warm_up(&self) {
        let source = self.prompt_source(self.budget());
        std::thread::spawn(move || {
            if let Some(home) = dirs::home_dir() {
                let _ = source.build("warm-up", "warm-up", None, &home, true);
            }
        });
    }

    /// `budget`: the knobs whose wording the prompt takes (a new session: the current ones; a
    /// turn's rebuilds: the turn's own copy, so a turn never mixes two profiles).
    fn prompt_source(&self, budget: budget::BudgetKnobs) -> PromptSource {
        PromptSource {
            data_dir: self.config.data_dir.clone(),
            shell: self.config.shell.clone(),
            windows_version: self.windows_version.clone(),
            store: self.store.clone(),
            memory: self.memory.clone(),
            skills: self.skills.clone(),
            budget,
        }
    }

    /// A subagent's base prompt: the session's, built now without memory (Hermes builds children
    /// with `skip_memory`).
    fn child_base_prompt(
        &self,
        open: &OpenSession,
        context_length: Option<usize>,
        budget: budget::BudgetKnobs,
    ) -> delegate::BasePrompt {
        let source = self.prompt_source(budget);
        let (provider, model, cwd) = {
            let record = open.record.lock();
            (
                record.provider.clone(),
                record.model.clone(),
                PathBuf::from(&record.cwd),
            )
        };
        Arc::new(move || source.build(&provider, &model, context_length, &cwd, false))
    }

    /// How a subagent's own tools are made: the session's kinds of tools, no memory and no one to
    /// ask (`ChildBridge`).
    fn child_tools(&self) -> delegate::ChildTools {
        let shell = self.config.shell.clone();
        let store = self.store.clone();
        let skills = self.skills.clone();
        let approvals = self.approvals.clone();
        let checkpoints = self.checkpoints.clone();
        let data_dir = self.config.data_dir.clone();
        let web = self.web.clone();
        Arc::new(move |session_id, cwd, scratch_dir| {
            SessionTools::new(SessionToolsConfig {
                session_id: session_id.to_string(),
                cwd: cwd.to_path_buf(),
                home: dirs::home_dir().unwrap_or_default(),
                data_dir: data_dir.clone(),
                scratch_dir: scratch_dir.to_path_buf(),
                shell: shell.clone(),
                bridge: Arc::new(delegate::ChildBridge),
                todos: Vec::new(),
                memory: None,
                history: Some(store.clone()),
                skills: Some(skills.clone()),
                approvals: Some(approvals.clone()),
                checkpoints: Some(checkpoints.clone()),
                web: Some(web.clone()),
            })
        })
    }

    /// Rebuilds a session's frozen prompt as it would be built now (after a compaction), with
    /// the budget wording of the turn's knobs: compaction already rewrites the prompt, so it
    /// follows the profile the session now runs under (no per-session profile is stored).
    fn prompt_rebuild(
        &self,
        open: &OpenSession,
        context_length: Option<usize>,
        budget: budget::BudgetKnobs,
    ) -> compress::PromptRebuild {
        let source = self.prompt_source(budget);
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
            data_dir: self.config.data_dir.clone(),
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

    /// `open`, with the conversation brought up to date when it is idle: the desktop app and an
    /// editor's agent are separate processes on one store, and either may have written to the
    /// thread since this one loaded it.
    fn open_fresh(&self, session_id: &str) -> Result<Arc<OpenSession>> {
        let open = self.open(session_id)?;
        if open.running.lock().is_none() {
            self.reload_if_stale(&open, session_id);
        }
        Ok(open)
    }

    /// Reloads the conversation from the store when it no longer matches it (another process
    /// wrote): same count and same last message means nothing changed.
    fn reload_if_stale(&self, open: &OpenSession, session_id: &str) {
        let (len, last) = {
            let conversation = open.conversation.lock();
            let messages = conversation.messages();
            (messages.len(), messages.last().and_then(|m| m.id.clone()))
        };
        let (Ok(count), Ok(stored_last)) = (
            self.store.message_count(session_id),
            self.store.last_message_id(session_id),
        ) else {
            return;
        };
        if count == len && stored_last == last {
            return;
        }
        let Ok(messages) = self.store.load_messages(session_id) else {
            return;
        };
        tracing::info!(
            "session {session_id}: the stored conversation changed elsewhere ({len} -> {} messages); reloaded",
            messages.len()
        );
        let conversation = Conversation::new_unvalidated(messages);
        open.tools.reset_todos(restore_todos(&conversation));
        *open.conversation.lock() = conversation;
        *open.context.lock() = None;
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .open_fresh(session_id)?
            .conversation
            .lock()
            .messages()
            .clone())
    }

    /// Hermes's edit-and-resend: drops message `keep` and everything after it, so the next
    /// turn continues from the conversation as it was before that message. The todo list goes
    /// back to what it was then. Refused while a turn runs.
    pub fn rewind(&self, session_id: &str, keep: usize) -> Result<Vec<Message>> {
        let open = self.open_fresh(session_id)?;
        if open.running.lock().is_some() {
            anyhow::bail!("a turn is running in this session");
        }
        // Not under another window's running turn either: rows are keyed by position, so cutting
        // them while it appends would leave a mix of both.
        let _lock = turnlock::TurnLock::acquire(&self.config.data_dir, session_id)?;
        self.reload_if_stale(&open, session_id);
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

    /// The tool definitions every request carries, as sent: the built-in tools and the
    /// connected MCP servers' tools (for a context breakdown).
    pub fn tool_definitions(&self) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let builtin = hermes_prompts::tools::tool_names()
            .filter_map(|name| hermes_prompts::tools::tool_schema(name).ok())
            .map(|schema| {
                serde_json::json!({
                    "name": schema.name,
                    "description": schema.description,
                    "parameters": schema.parameters,
                })
            })
            .collect();
        let mcp = self
            .mcp
            .tools()
            .iter()
            .filter_map(|tool| serde_json::to_value(tool).ok())
            .collect();
        (builtin, mcp)
    }

    /// The session's background processes (empty when it isn't open in this process).
    pub fn processes(&self, session_id: &str) -> Vec<serde_json::Value> {
        self.sessions
            .lock()
            .get(session_id)
            .map(|open| open.tools.process_list())
            .unwrap_or_default()
    }

    /// Kills one of the session's background processes: its final status (`None` when the
    /// session isn't open here).
    pub async fn kill_process(
        &self,
        session_id: &str,
        process_id: &str,
    ) -> Option<serde_json::Value> {
        let tools = self
            .sessions
            .lock()
            .get(session_id)
            .map(|open| open.tools.clone())?;
        Some(tools.kill_process(process_id).await)
    }

    /// Kills all of the session's background processes; how many were running.
    pub async fn stop_processes(&self, session_id: &str) -> usize {
        let tools = self
            .sessions
            .lock()
            .get(session_id)
            .map(|open| open.tools.clone());
        match tools {
            Some(tools) => tools.kill_all_processes().await,
            None => 0,
        }
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
        self.begin_turn(session_id, provider, options, Some(text), Vec::new(), None)
    }

    /// [`Self::start_turn`] whose first step is a tool call made for the user (Hermes's scripted
    /// prelude: `/review` starts its reviewer with `delegate_task`); the model goes on from its
    /// result.
    pub fn start_turn_with_call(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: &str,
        tool: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Turn> {
        let call =
            rmcp::model::CallToolRequestParams::new(tool.to_string()).with_arguments(arguments);
        self.begin_turn(
            session_id,
            provider,
            options,
            Some(text),
            Vec::new(),
            Some(call),
        )
    }

    /// [`Self::start_turn`] with images the user attached (raw file bytes, any common format).
    pub fn start_turn_with_images(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: &str,
        images: Vec<Vec<u8>>,
    ) -> Result<Turn> {
        self.begin_turn(session_id, provider, options, Some(text), images, None)
    }

    /// Hermes's `/retry`: drops the provider error that ended the last turn and runs the agent
    /// again from where it stopped, without a new user message.
    pub fn retry_turn(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
    ) -> Result<Turn> {
        self.begin_turn(session_id, provider, options, None, Vec::new(), None)
    }

    fn begin_turn(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: Option<&str>,
        images: Vec<Vec<u8>>,
        prelude: Option<rmcp::model::CallToolRequestParams>,
    ) -> Result<Turn> {
        let TurnOptions {
            context_length,
            effort,
            supports_vision,
            aux,
        } = options;
        let effort_params = effort
            .as_deref()
            .map(|effort| effort_request_params(&provider.base_url, effort));
        let open = self.open(session_id)?;
        let cancel = CancellationToken::new();
        // The daily request quota: refused only when it is used up, warned when nearly so.
        let quota_notice = match self.governor().check(&provider.id, Purpose::Main) {
            governor::Verdict::Deny(why) => anyhow::bail!("{why}"),
            governor::Verdict::Warn(text) => Some(text),
            governor::Verdict::Allow => None,
        };
        {
            let mut running = open.running.lock();
            anyhow::ensure!(
                running.is_none(),
                "a turn is already running in this session"
            );
            let lock = turnlock::TurnLock::acquire(&self.config.data_dir, session_id)?;
            *running = Some(session::RunGuard::new(cancel.clone(), Some(lock)));
        }
        // Another process may have added to the conversation since it was loaded.
        self.reload_if_stale(&open, session_id);
        let model = open.record.lock().model.clone();
        // The budget knobs, read once: everything this turn builds takes them from this copy.
        let budget = self.budget();
        let models = provider.provider_with(budget.http_retries).and_then(|llm| {
            let summarizer = summarizer_for(&aux, &llm, &model, context_length, &budget)?;
            let review = match &aux.review {
                Some(aux) => (
                    aux.provider.provider_with(budget.http_retries)?,
                    aux.model.clone(),
                    aux.context_length,
                    aux_effort_params(aux),
                ),
                None => (
                    llm.clone(),
                    model.clone(),
                    context_length,
                    effort_params.clone(),
                ),
            };
            Ok((llm, summarizer, review))
        });
        let (llm, summarizer, review_model) = match models {
            Ok(models) => models,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let state: ops::SharedTurnState = Arc::default();
        *open.turn_state.lock() = Some(state.clone());
        open.tools.begin_turn();
        open.tools.set_vision(supports_vision);
        open.tools.set_limits(tool_limits(&budget));
        let governor = self.governor();
        // Smart approvals judge with the approval model, else the turn's.
        let guard: Option<Arc<dyn hermes_tools::approval::ApprovalGuard>> = match &aux.approval {
            Some(aux) => aux.provider.provider().ok().map(|llm| {
                Arc::new(SmartGuard {
                    llm,
                    model: aux.model.clone(),
                    base_url: aux.provider.base_url.clone(),
                    effort: aux.effort.clone(),
                    governor: Some((governor.clone(), aux.provider.id.clone())),
                }) as Arc<dyn hermes_tools::approval::ApprovalGuard>
            }),
            // Under `requests` there is no fallback to the main model: a flagged command asks.
            None if !budget.side_jobs_may_use_main_model() => None,
            None => provider.provider().ok().map(|llm| {
                Arc::new(SmartGuard {
                    llm,
                    model: open.record.lock().model.clone(),
                    base_url: provider.base_url.clone(),
                    effort: None,
                    governor: Some((governor.clone(), provider.id.clone())),
                }) as Arc<dyn hermes_tools::approval::ApprovalGuard>
            }),
        };
        open.tools.set_guard(guard);
        let runtime = TurnRuntime {
            session: open.clone(),
            store: self.store.clone(),
            state: state.clone(),
            persist: true,
            record_usage: true,
        };
        // A review still running from the last turn is left alone (Hermes cancels it, so the two
        // don't compete for the model; here it would waste the requests it already made, Phase 4):
        // one that hasn't made its first request waits for this turn to end, one that has is on
        // its own copy of the conversation and finishes. See `session::ReviewHandle`.
        let review_memory = text.is_some() && open.nudges.lock().tick_memory();
        let review_provider_id = aux
            .review
            .as_ref()
            .map_or_else(|| provider.id.clone(), |aux| aux.provider.id.clone());
        // The background review runs on the auxiliary review model when one is set.
        let (review_llm, review_name, review_context, review_effort) = review_model;
        let review_setup = ReviewSetup {
            store: self.store.clone(),
            bridge: self.bridge.clone(),
            llm: review_llm,
            model: review_name,
            context_length: review_context,
            effort_params: review_effort,
            enabled: self.background_review.clone(),
            ledger: self.ledger.clone(),
            provider_id: review_provider_id,
            budget,
        };
        // A message with `@url:` references or images is added once its pages are fetched and
        // its images prepared, in the turn's task (that can take seconds).
        let deferred = text
            .filter(|t| !images.is_empty() || !hermes_tools::references::url_targets(t).is_empty())
            .map(|t| (t.to_string(), images));
        let first_events = match text {
            Some(_) if deferred.is_some() => Ok(Vec::new()),
            Some(text) => user_message(&open, text, context_length, &Default::default(), &[])
                .and_then(|user| {
                    runtime.append(user.clone())?;
                    let mut events = vec![AgentEvent::Message(user)];
                    // A scripted first step: the assistant's tool call, which the turn's tool
                    // step runs before any model call.
                    if let Some(call) = prelude {
                        let id =
                            format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                        let request = Message::assistant()
                            .with_tool_request(id, Ok(call))
                            .with_generated_id_if_missing();
                        runtime.append(request.clone())?;
                        events.push(AgentEvent::Message(request));
                    }
                    Ok(events)
                }),
            None => runtime
                .drop_trailing_errors()
                .and_then(|conversation| conversation.context("there is no failed turn to retry"))
                .map(|conversation| vec![AgentEvent::HistoryReplaced(conversation)]),
        };
        let first_events = match first_events {
            Ok(events) => events,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let mut turn_start = open.conversation.lock().messages().len();
        let web = self.web.clone();
        let rebuild_prompt = self.prompt_rebuild(&open, context_length, budget);
        let delegator = self.delegator(
            &open,
            &provider.id,
            llm.clone(),
            &model,
            context_length,
            effort_params.clone(),
            supports_vision,
            summarizer.clone(),
            budget,
        );
        let self_mcp = self.mcp.clone();
        // The tools switched off as this turn starts (a switch applies from the next turn).
        let disabled = Arc::new(self.disabled_tools.lock().clone());
        *self.last_activity.lock() = std::time::Instant::now();
        let last_activity = self.last_activity.clone();

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        for event in first_events {
            let _ = events_tx.send(RuntimeEvent::Agent(event));
        }
        if let Some(text) = quota_notice {
            let _ = events_tx.send(RuntimeEvent::Notice(text));
        }
        let session_id = session_id.to_string();
        let turn_cancel = cancel.clone();
        // A turn that panics (a bug in a tool, say) must still end: otherwise the session stays
        // "running", Stop finds nothing to stop, and every new prompt waits behind it forever.
        let guard_session = runtime.session.clone();
        let guard_events = events_tx.clone();
        // Every model call of the turn is a `main` request in the ledger (the context is a
        // task-local, so it is set around this task; tasks the turn spawns need their own).
        let request_context = self.ledger.context(hermes_llm::observer::Purpose::Main, Some(&session_id));
        // A daily-limit 429 that ends the turn teaches the ledger the provider's limit.
        let (learn_ledger, learn_provider) = (self.ledger.clone(), provider.id.clone());
        let turn = tokio::spawn(hermes_llm::observer::scope(request_context, async move {
            if let Some((text, images)) = deferred {
                let session = runtime.session.clone();
                let attachments =
                    tokio::task::spawn_blocking(move || attach_images(&session, images))
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
                let mut pages = hermes_tools::references::FetchedPages::new();
                for url in hermes_tools::references::url_targets(&text) {
                    let page = tokio::select! {
                        page = web.fetch_for_reference(&url) => page,
                        _ = turn_cancel.cancelled() => Err("cancelled".into()),
                    };
                    pages.insert(url, page);
                }
                let added = attachments
                    .and_then(|attachments| {
                        user_message(
                            &runtime.session,
                            &text,
                            context_length,
                            &pages,
                            &attachments,
                        )
                    })
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
                .chain(self_mcp.tool_names())
                .collect();
            let tools: Arc<dyn ToolProvider<TurnSession>> = Arc::new(ops::GuardedTools {
                inner: Arc::new(delegate::DelegatingTools {
                    inner: runtime.session.tools.clone(),
                    delegator,
                }),
                scratch_dir: runtime.session.tools.scratch_dir().to_path_buf(),
                state: state.clone(),
                result_limit: ops::result_size_limit(context_length, &budget),
                disabled: disabled.clone(),
            });
            let mut model_config = ModelConfig::new(&model)
                .with_context_limit(context_length)
                .with_vision_support(supports_vision);
            if let Some(params) = effort_params {
                model_config = model_config.with_merged_request_params(params);
            }
            let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
                Step::Operation(Arc::new(ops::FrozenPrompt)),
                Step::Operation(Arc::new(ops::IterationBudget {
                    state: state.clone(),
                    max_iterations: budget.max_iterations,
                })),
                Step::Operation(Arc::new(ops::ValidateToolCalls::new(
                    state.clone(),
                    tool_names.clone(),
                    &budget,
                ))),
                Step::Operation(Arc::new(ToolOperation::new().with_provider(tools))),
                Step::Operation(Arc::new(ops::UnhandledToolCalls {
                    tool_names,
                    disabled: disabled.clone(),
                })),
                Step::Operation(Arc::new(ops::DeliverSteer {
                    state: state.clone(),
                })),
                Step::Operation(Arc::new(ops::LengthContinuation::new(
                    state.clone(),
                    &budget,
                ))),
                Step::Operation(Arc::new(ops::EmptyResponseRecovery {
                    state: state.clone(),
                })),
                Step::Operation(Arc::new(ops::ExitOnError)),
                Step::Operation(Arc::new(compress::AutoCompress {
                    state: state.clone(),
                    summarizer: summarizer.clone(),
                    rebuild_prompt,
                })),
                Step::Inference(Arc::new(
                    InferenceRunner::new(llm, model_config)
                        .with_empty_response_retries(budget.empty_response_retries),
                )),
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
            let limit_error = {
                let conversation = runtime.session.conversation.lock();
                conversation.messages().last().and_then(|last| {
                    last.content
                        .iter()
                        .find_map(|content| content.as_error())
                        .map(|error| error.message.clone())
                })
            };
            if let Some(text) = limit_error
                && hermes_llm::retry::is_usage_limit(&text)
                && let Some(limit) = ledger::daily_limit_in(&text)
            {
                learn_ledger.learn_limit(&learn_provider, limit);
            }
            *last_activity.lock() = std::time::Instant::now();
            let finished = status == TurnStatus::Completed;
            let _ = events_tx.send(RuntimeEvent::TurnFinished(status));
            review_setup.after_turn(&runtime.session, turn_start, review_memory, finished);
        }));
        tokio::spawn(async move {
            if let Err(error) = turn.await
                && error.is_panic()
            {
                tracing::error!("turn crashed: {error}");
                *guard_session.running.lock() = None;
                *guard_session.turn_state.lock() = None;
                let _ = guard_events.send(RuntimeEvent::TurnFinished(TurnStatus::Failed(
                    "The turn stopped on an internal error (details in the log).".into(),
                )));
            }
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
        // The budget knobs, read once for this review.
        let budget = self.budget();
        let (llm, model, context_length, effort_params) = match &options.aux.review {
            Some(aux) => (
                aux.provider.provider_with(budget.http_retries)?,
                aux.model.clone(),
                aux.context_length,
                aux_effort_params(aux),
            ),
            None => (
                provider.provider_with(budget.http_retries)?,
                open.record.lock().model.clone(),
                options.context_length,
                options
                    .effort
                    .as_deref()
                    .map(|effort| effort_request_params(&provider.base_url, effort)),
            ),
        };
        let setup = ReviewSetup {
            store: self.store.clone(),
            bridge: self.bridge.clone(),
            llm,
            model,
            context_length,
            effort_params,
            enabled: self.background_review.clone(),
            ledger: self.ledger.clone(),
            provider_id: options
                .aux
                .review
                .as_ref()
                .map_or_else(|| provider.id.clone(), |aux| aux.provider.id.clone()),
            budget,
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
            let lock = turnlock::TurnLock::acquire(&self.config.data_dir, session_id)?;
            *running = Some(session::RunGuard::new(cancel.clone(), Some(lock)));
        }
        self.reload_if_stale(&open, session_id);
        // The budget knobs, read once for this compression.
        let budget = self.budget();
        let llm = match provider.provider_with(budget.http_retries) {
            Ok(llm) => llm,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let rebuild_prompt = self.prompt_rebuild(&open, options.context_length, budget);
        let model = open.record.lock().model.clone();
        let summarizer = match summarizer_for(&options.aux, &llm, &model, options.context_length, &budget) {
            Ok(summarizer) => summarizer,
            Err(error) => {
                *open.running.lock() = None;
                return Err(error);
            }
        };
        let store = self.store.clone();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let turn_cancel = cancel.clone();
        let context = self.ledger.context(Purpose::Compress, Some(session_id));
        tokio::spawn(hermes_llm::observer::scope(context, async move {
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
        }));
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

    /// The MCP servers (Settings configures them; their tools join every turn).
    pub fn mcp(&self) -> Arc<mcp::McpManager> {
        self.mcp.clone()
    }

    /// Hermes's `/yolo`: turns approval prompts off (or on) for one session. Returns the new state.
    pub fn toggle_yolo(&self, session_id: &str) -> Result<bool> {
        let open = self.open(session_id)?;
        let on = !open.tools.yolo();
        open.tools.set_yolo(on);
        Ok(on)
    }

    /// The session's approval mode (its own, else Settings'); see [`hermes_tools::ApprovalMode`].
    pub fn approval_mode(&self, session_id: &str) -> hermes_tools::ApprovalMode {
        self.sessions
            .lock()
            .get(session_id)
            .map_or(hermes_tools::ApprovalMode::AcceptEdits, |open| {
                open.tools.mode()
            })
    }

    pub fn set_approval_mode(
        &self,
        session_id: &str,
        mode: hermes_tools::ApprovalMode,
    ) -> Result<()> {
        self.open(session_id)?.tools.set_mode(mode);
        Ok(())
    }

    pub fn yolo(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .get(session_id)
            .is_some_and(|open| open.tools.yolo())
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
        if outcome.restored && self.undo_last_turn(&open, session_id)? > 0 {
            report.push_str("\n\nChat turn undone to match restored file state.");
        }
        self.add_command_output(&open, "rollback", &report)
    }

    /// Drops the last typed turn (after a whole-folder restore, so the conversation matches the
    /// files); how many messages it removed.
    fn undo_last_turn(&self, open: &OpenSession, session_id: &str) -> Result<usize> {
        let (last_turn, total) = {
            let conversation = open.conversation.lock();
            let messages = conversation.messages();
            let last = messages.iter().rposition(|m| {
                m.role == rmcp::model::Role::User
                    && m.is_user_visible()
                    && !m.is_tool_response()
                    && m.content
                        .iter()
                        .any(|c| matches!(c, MessageContent::Text(_)))
            });
            (last, messages.len())
        };
        let Some(index) = last_turn else {
            return Ok(0);
        };
        self.rewind(session_id, index)?;
        Ok(total - index)
    }

    /// The checkpoint folder of a session: its working folder's project root (Hermes's
    /// `_get_working_dir_for`).
    pub fn checkpoint_dir(&self, session_id: &str) -> Result<PathBuf> {
        let open = self.open(session_id)?;
        let cwd = PathBuf::from(&open.record.lock().cwd);
        let home = dirs::home_dir().context("no home directory")?;
        Ok(self.checkpoints.working_dir_for(&cwd, &home))
    }

    /// Restores a session's files to a checkpoint (one file, or the folder: `all` overwrites hand
    /// edits too), and after a whole-folder restore undoes the last chat turn, as `/rollback`
    /// does. The restore, and how many messages were dropped.
    pub fn restore_checkpoint(
        &self,
        session_id: &str,
        hash: &str,
        file: Option<&str>,
        all: bool,
    ) -> Result<(hermes_tools::checkpoints::Restored, usize)> {
        let open = self.idle_session(session_id)?;
        let workdir = self.checkpoint_dir(session_id)?;
        let restored = self
            .checkpoints
            .restore(&workdir, hash, file, !all && file.is_none())
            .map_err(|e| anyhow::anyhow!(e))?;
        let removed = if file.is_none() {
            self.undo_last_turn(&open, session_id)?
        } else {
            0
        };
        Ok((restored, removed))
    }

    /// Hermes's `/diff [working|staged|all|session] [--stat] [path…]`. Returns the conversation
    /// after it, which ends with the diff.
    pub fn diff(&self, session_id: &str, args: &str) -> Result<Vec<Message>> {
        let open = self.idle_session(session_id)?;
        let output = self.diff_report(session_id, args)?;
        self.add_command_output(&open, "diff", &output)
    }

    /// `/diff`'s report, without adding it to the conversation (the desktop shows it as a note).
    pub fn diff_report(&self, session_id: &str, args: &str) -> Result<String> {
        let open = self.open(session_id)?;
        let cwd = PathBuf::from(&open.record.lock().cwd);
        let home = dirs::home_dir().context("no home directory")?;
        Ok(commands::diff(&self.checkpoints, &cwd, &home, args))
    }

    /// The review pane: what this session changed, per file.
    pub fn changes(&self, session_id: &str) -> Result<changes::Changes> {
        let open = self.open(session_id)?;
        let (cwd, started_ms) = {
            let record = open.record.lock();
            (PathBuf::from(&record.cwd), record.created_at)
        };
        let home = dirs::home_dir().context("no home directory")?;
        let files = changes::session_files(open.conversation.lock().messages(), &cwd);
        let scope = changes::SessionScope { files, started_ms };
        Ok(changes::changes(&self.checkpoints, &cwd, &home, &scope))
    }

    /// The review pane's Revert: one file back to how it was before the session's first edit.
    pub fn revert_file(&self, session_id: &str, file: &str) -> Result<()> {
        let open = self.idle_session(session_id)?;
        let (cwd, started_ms) = {
            let record = open.record.lock();
            (PathBuf::from(&record.cwd), record.created_at)
        };
        let home = dirs::home_dir().context("no home directory")?;
        changes::revert_file(&self.checkpoints, &cwd, &home, file, started_ms)
            .map_err(anyhow::Error::msg)
    }

    /// Hermes's `/undo [N]`: removes the last N exchanges (default 1). Returns the conversation
    /// after it (ending with the report) and the first removed message as the user typed it, for
    /// the composer.
    pub fn undo(&self, session_id: &str, args: &str) -> Result<(Vec<Message>, Option<String>)> {
        let open = self.idle_session(session_id)?;
        let turns = match args.trim() {
            "" => 1,
            arg => match arg.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => {
                    let report = format!("Invalid count \"{arg}\" — use /undo or /undo N.");
                    return Ok((self.add_command_output(&open, "undo", &report)?, None));
                }
            },
        };
        let starts: Vec<usize> = open
            .conversation
            .lock()
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.role == rmcp::model::Role::User
                    && m.is_user_visible()
                    && !m.is_tool_response()
                    && m.content
                        .iter()
                        .any(|c| matches!(c, MessageContent::Text(_)))
            })
            .map(|(index, _)| index)
            .collect();
        let Some(&start) = starts
            .iter()
            .rev()
            .nth(turns.min(starts.len()).saturating_sub(1))
        else {
            return Ok((
                self.add_command_output(&open, "undo", "Nothing to undo.")?,
                None,
            ));
        };
        let undone = turns.min(starts.len());
        let (removed, typed) = {
            let conversation = open.conversation.lock();
            let message = &conversation.messages()[start];
            let typed = hermes_tools::references::typed_text(message)
                .map(str::to_string)
                .unwrap_or_else(|| message.as_concat_text());
            // Counted without command reports (/usage, /diff, …), which aren't the conversation.
            let removed = conversation.messages()[start..]
                .iter()
                .filter(|m| m.metadata.operation_note(COMMAND_NOTE, "command").is_none())
                .count();
            (removed, typed)
        };
        self.rewind(session_id, start)?;
        let report = format!(
            "↩️ Undid {undone} turn{} ({removed} message{}). The message is back in the box to edit and resend.",
            if undone == 1 { "" } else { "s" },
            if removed == 1 { "" } else { "s" },
        );
        Ok((
            self.add_command_output(&open, "undo", &report)?,
            Some(typed),
        ))
    }

    /// Hermes's `/usage`: the session's token use, cost, context and its estimated breakdown.
    pub fn usage(&self, session_id: &str, context_length: Option<usize>) -> Result<Vec<Message>> {
        let open = self.idle_session(session_id)?;
        let report = self.usage_text(session_id, context_length)?;
        self.add_command_output(&open, "usage", &report)
    }

    /// `/usage`'s report, without adding it to the conversation.
    pub fn usage_text(&self, session_id: &str, context_length: Option<usize>) -> Result<String> {
        let open = self.open(session_id)?;
        let totals = self.store.usage_totals(session_id)?;
        let model = open.record.lock().model.clone();
        let compressions = open
            .conversation
            .lock()
            .messages()
            .iter()
            .filter(|m| compress::notice(m).is_some())
            .count();
        let breakdown = self.context_breakdown(session_id)?;
        Ok(commands::usage_report(
            &model,
            &totals,
            context_length,
            compressions,
            &breakdown,
        ))
    }

    /// Hermes's `/insights [--days N]`: activity across sessions.
    pub fn insights(&self, session_id: &str, args: &str) -> Result<Vec<Message>> {
        let open = self.idle_session(session_id)?;
        let report = self.insights_text(args);
        self.add_command_output(&open, "insights", &report)
    }

    /// `/insights`'s report, without adding it to a conversation.
    pub fn insights_text(&self, args: &str) -> String {
        commands::insights_report(&self.store, args)
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

    /// What a session's subagents run with (`delegate_task`, `/bg`).
    #[allow(clippy::too_many_arguments)]
    fn delegator(
        &self,
        open: &Arc<OpenSession>,
        provider_id: &str,
        llm: Arc<dyn Provider>,
        model: &str,
        context_length: Option<usize>,
        effort_params: Option<HashMap<String, serde_json::Value>>,
        supports_vision: bool,
        summarizer: compress::Summarizer,
        budget: budget::BudgetKnobs,
    ) -> Arc<delegate::Delegator> {
        Arc::new(delegate::Delegator {
            parent: open.clone(),
            store: self.store.clone(),
            llm,
            model: model.to_string(),
            context_length,
            effort_params,
            supports_vision,
            base_prompt: self.child_base_prompt(open, context_length, budget),
            child_tools: self.child_tools(),
            summarizer,
            mcp: self.mcp.clone(),
            subagents: self.subagents.clone(),
            disabled: Arc::new(self.disabled_tools.lock().clone()),
            quota: Some((self.governor(), provider_id.to_string())),
            budget,
        })
    }

    /// Hermes's `/bg <prompt>`: the prompt runs as a background subagent of the session (its own
    /// conversation and tools; the session's own conversation is untouched, and it may be busy).
    /// It is listed, followed, steered and stopped like any subagent; its events arrive on the
    /// receiver, and the task ends with its answer (or `error: …`).
    pub fn background(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: TurnOptions,
        text: &str,
    ) -> Result<Background> {
        // The user's own request: only held back when the daily quota is used up.
        if let governor::Verdict::Deny(why) = self.governor().check(&provider.id, Purpose::Main) {
            anyhow::bail!("{why}");
        }
        let open = self.open(session_id)?;
        // The budget knobs, read once for the whole task.
        let budget = self.budget();
        let llm = provider.provider_with(budget.http_retries)?;
        let model = open.record.lock().model.clone();
        let summarizer =
            summarizer_for(&options.aux, &llm, &model, options.context_length, &budget)?;
        let effort_params = options
            .effort
            .as_deref()
            .map(|effort| effort_request_params(&provider.base_url, effort));
        let delegator = self.delegator(
            &open,
            &provider.id,
            llm,
            &model,
            options.context_length,
            effort_params,
            options.supports_vision,
            summarizer,
            budget,
        );
        let task_id = format!("bg_{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let (events_tx, events) = mpsc::unbounded_channel();
        let emit = Emitter::new(events_tx, CancellationToken::new());
        let (goal, id) = (text.trim().to_string(), task_id.clone());
        let context = self.ledger.context(Purpose::Delegate, Some(session_id));
        let done = tokio::spawn(hermes_llm::observer::scope(context, async move {
            let result = delegator.background(&goal, &id, &emit).await;
            match result["status"].as_str() {
                Some("completed") => result["summary"].as_str().unwrap_or_default().to_string(),
                Some("interrupted") => "error: stopped".to_string(),
                _ => format!(
                    "error: {}",
                    result["error"]
                        .as_str()
                        .unwrap_or("the background task failed")
                ),
            }
        }));
        Ok(Background {
            task_id,
            events,
            done,
        })
    }

    /// Hermes's `/btw <question>`: a question about the conversation answered from a snapshot of
    /// it, in one request without tools (`agent/side_question.py`'s one-shot path). The
    /// conversation is untouched, and it may be mid-turn.
    pub async fn side_question(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        question: &str,
    ) -> Result<String> {
        use hermes_prompts::side::{self, Line};
        anyhow::ensure!(!question.trim().is_empty(), "a side question needs text");
        let messages = self.messages(session_id)?;
        let model = self
            .store
            .session(session_id)?
            .map(|record| record.model)
            .context("no such session")?;
        let mut lines = Vec::new();
        for message in messages.iter().filter(|m| m.is_agent_visible()) {
            let text: String = message
                .content
                .iter()
                .filter_map(|c| match c {
                    MessageContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let tools: Vec<String> = message
                .content
                .iter()
                .filter_map(MessageContent::as_tool_request)
                .filter_map(|r| r.tool_call.as_ref().ok().map(|c| c.name.to_string()))
                .collect();
            let results: Vec<String> = message
                .content
                .iter()
                .filter(|c| c.as_tool_response().is_some())
                .map(compress::response_text)
                .collect();
            match message.role {
                rmcp::model::Role::Assistant => {
                    if !tools.is_empty() {
                        lines.push(Line::CalledTools(tools));
                    }
                    lines.push(Line::Said {
                        label: "ASSISTANT",
                        text,
                    });
                }
                rmcp::model::Role::User if !results.is_empty() => {
                    lines.push(Line::Said {
                        label: "TOOL RESULT",
                        text: results.join("\n"),
                    });
                }
                rmcp::model::Role::User => lines.push(Line::Said {
                    label: "USER",
                    text,
                }),
            }
        }
        let llm = provider.provider()?;
        let config = ModelConfig::new(&model).with_max_tokens(Some(2048));
        let (reply, _) = self
            .ledger
            .scoped(
                Purpose::Btw,
                Some(session_id),
                llm.complete(
                    &config,
                    side::INSTRUCTIONS,
                    &[Message::user()
                        .with_text(side::user_input(&side::transcript(&lines), question))],
                    &[],
                ),
            )
            .await
            .map_err(|error| anyhow::anyhow!("side question failed: {error}"))?;
        let answer: String = reply
            .content
            .iter()
            .filter_map(|content| match content {
                MessageContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        Ok(answer.trim().to_string())
    }

    /// Tools the user switched off (Capabilities ▸ Tools), from the next turn on: not offered to
    /// the model (nor to subagents), and refused if called anyway.
    pub fn set_disabled_tools(&self, tools: impl IntoIterator<Item = String>) {
        *self.disabled_tools.lock() = tools.into_iter().collect();
    }

    /// The `delegate_task` children running now: list, tail, steer, stop (`subagents.rs`).
    pub fn subagents(&self) -> &subagents::Subagents {
        &self.subagents
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

    /// Replaces the running turn's undelivered steer (`None` withdraws it). `false` when no turn
    /// is running.
    pub fn set_steer(&self, session_id: &str, text: Option<&str>) -> bool {
        let state = self
            .sessions
            .lock()
            .get(session_id)
            .and_then(|open| open.turn_state.lock().clone());
        let Some(state) = state else {
            return false;
        };
        state.lock().pending_steer = text.map(str::to_string);
        true
    }

    pub fn cancel(&self, session_id: &str) {
        if let Some(open) = self.sessions.lock().get(session_id) {
            if let Some(cancel) = open.running.lock().as_ref() {
                cancel.cancel();
            }
        }
    }

    /// A committed workspace move: drop the session's cached in-memory state so the next turn
    /// rebuilds it from the stored record — the tools' cwd, the shell and its environment follow
    /// the new folder then (Hermes's `_evict_cached_agent(live.route)` and
    /// `drop_session_terminal` on a committed workspace move). The transcript is the store's
    /// (every append is persisted), so nothing is lost. Idle sessions only.
    pub async fn evict(&self, session_id: &str) -> Result<()> {
        // The idle check and the removal under one hold of the map: a turn can't start between them.
        let open = {
            let mut sessions = self.sessions.lock();
            let Some(open) = sessions.get(session_id).cloned() else {
                return Ok(());
            };
            if open.running.lock().is_some() {
                anyhow::bail!("a turn is running in this session");
            }
            sessions.remove(session_id);
            open
        };
        open.tools.shutdown().await;
        Ok(())
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
/// An image the user attached: what the model gets, and where the original was saved.
struct Attachment {
    image: hermes_tools::vision::PreparedImage,
    path: PathBuf,
}

/// Prepares attached images for the model and saves the originals in the session's scratch
/// folder, where tools (`vision_analyze` with a region, `read_file`) can reach them.
fn attach_images(open: &OpenSession, images: Vec<Vec<u8>>) -> Result<Vec<Attachment>> {
    let folder = open.tools.scratch_dir().join("attachments");
    std::fs::create_dir_all(&folder)?;
    images
        .into_iter()
        .map(|bytes| {
            let image = hermes_tools::vision::prepare(
                &bytes,
                None,
                hermes_tools::vision::EMBED_MAX_DIMENSION,
                hermes_tools::vision::ATTACHMENT_TARGET_BYTES,
            )
            .map_err(anyhow::Error::msg)?;
            let path = hermes_tools::vision::save(&folder, &bytes)?;
            Ok(Attachment { image, path })
        })
        .collect()
}

fn user_message(
    open: &OpenSession,
    text: &str,
    context_length: Option<usize>,
    pages: &hermes_tools::references::FetchedPages,
    images: &[Attachment],
) -> Result<Message> {
    let cwd = PathBuf::from(&open.record.lock().cwd);
    let home = dirs::home_dir().context("no home directory")?;
    let expanded = hermes_tools::references::expand(text, &cwd, &home, context_length, pages);
    if let Some(expanded) = &expanded {
        anyhow::ensure!(!expanded.blocked, "{}", expanded.warnings.join("\n"));
    }
    if expanded.is_none() && images.is_empty() {
        return Ok(Message::user()
            .with_text(text)
            .with_generated_id_if_missing());
    }
    let mut body = expanded.map_or_else(|| text.to_string(), |e| e.message);
    // Where each image was saved, so the model can zoom in with `vision_analyze` (or work with
    // the file when it can't see images).
    for attachment in images {
        body.push_str(&format!(
            "\n\n[Attached image: {}]",
            hermes_tools::display_path(&attachment.path)
        ));
    }
    let mut message = Message::user().with_text(body);
    for attachment in images {
        message = message.with_image(attachment.image.data.clone(), attachment.image.mime.clone());
    }
    let mut message = message.with_generated_id_if_missing();
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
    /// The knobs when this source was made: a new session's prompt takes its budget wording
    /// and skills block from them, and keeps them (the prompt is frozen).
    budget: budget::BudgetKnobs,
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
        let skills_index = self
            .skills
            .index_prompt_with(self.budget.skills_index == budget::SkillsIndex::NamesOnly);
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
            budget_guidance: match self.budget.profile {
                // Balanced is the prompt from before profiles, byte for byte.
                budget::BudgetProfile::Balanced => None,
                budget::BudgetProfile::Requests => Some(hermes_prompts::BUDGET_GUIDANCE_REQUESTS),
                budget::BudgetProfile::Tokens => Some(hermes_prompts::BUDGET_GUIDANCE_TOKENS),
            },
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
    /// Counts the review's requests and holds it back when the quota is low.
    ledger: ledger::Ledger,
    /// The provider the review model is on.
    provider_id: String,
    /// The turn's budget knobs (the review loop's limits).
    budget: budget::BudgetKnobs,
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
        // Low on quota: a background review waits for another day (one the user asked for with
        // /refine counts as their own work and is only held back when nothing is left).
        let asked_as = if requested { Purpose::Main } else { Purpose::Review };
        if let governor::Verdict::Deny(why) =
            governor::Governor::new(self.ledger.clone()).check(&self.provider_id, asked_as)
        {
            tracing::info!("review skipped: {why}");
            return;
        }
        let (mut memory, mut skills) = (memory, skills);
        let handle = {
            // Under `running` (lock order: running, then review): a paused review takes its
            // snapshot and sets `started` under this lock, so it can't start between the check
            // below and its cancellation.
            let _running = session.running.lock();
            let mut slot = session.review.lock();
            if let Some(previous) = slot.as_ref() {
                // One already past its first request finishes (cancelling it would waste what it
                // spent); a newly due review is skipped, except one the user asked for with
                // /refine, which replaces it. The skipped review's turns aren't lost: every
                // review reads the whole conversation, so the next one due sees them.
                if previous.has_started() && !requested {
                    tracing::info!("review skipped: one is still running");
                    return;
                }
                if !previous.has_started() {
                    // A paused /refine stays (it covers memory and skills, and the user waits
                    // for its report); a paused due review is replaced by this one, which will
                    // see the same conversation, and takes over what it was to cover.
                    if previous.requested && !requested {
                        tracing::info!("review skipped: a /refine is waiting to run");
                        return;
                    }
                    memory |= previous.memory;
                    skills |= previous.skills;
                }
                previous.cancel.cancel();
            }
            let handle = session::ReviewHandle::new(memory, skills, requested);
            *slot = Some(handle.clone());
            handle
        };
        let session_id = session.record.lock().id.clone();
        let context = self.ledger.context(Purpose::Review, Some(&session_id));
        tokio::spawn(hermes_llm::observer::scope(context, review::run(review::ReviewJob {
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
            budget: self.budget,
            handle,
        })));
    }
}

/// The smart-approval verdict `model` gives for a flagged command (for checks and tests).
pub async fn smart_review(
    provider: &ProviderConfig,
    model: &str,
    command: &str,
    description: &str,
) -> Result<hermes_tools::approval::Verdict> {
    use hermes_tools::approval::ApprovalGuard as _;
    let guard = SmartGuard {
        llm: provider.provider()?,
        model: model.to_string(),
        base_url: provider.base_url.clone(),
        effort: None,
        governor: None,
    };
    Ok(guard.assess(command, description).await)
}
