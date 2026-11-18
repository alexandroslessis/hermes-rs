//! Settings applied to the runtime, and what turns, sessions and the model picker ask of them:
//! the config record's keys put on the runtime's policies (approvals, memory sizes, the budget,
//! checkpoints, the terminal's sandbox, the request ledger's limits, the switched-off tools), a
//! model's context window and vision support as its provider lists them, the provider a session
//! talks to, the options a turn runs with, the model picker and switching a model.
//!
//! Hermes: `tui_gateway/server.py` (`config.*`, `setup.*`), `tui_gateway/methods_session.py`.
//! Backend, ours (it may deviate from Hermes): which keys exist and what they put on the runtime is
//! `desktop-config.json` (`config.rs`) and `config.json` (`providers.rs`).
//!
//! Typed calls; the page's `config.get`, `config.set`, `setup.*` and `model.options` JSON, the
//! `session.info` frames a change triggers and the RPC errors are `crates/desktop/src/gateway/
//! settings.rs`. Where a call used to emit a frame mid-way (a deferred model pick), it takes a
//! callback so the order stays the same.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use hermes_runtime::{ProviderConfig, TurnOptions};
use serde_json::Value;

use crate::Core;
use crate::providers::{ProviderSettings, Settings};

/// The window assumed for a model whose provider doesn't list one (Hermes's
/// `DEFAULT_FALLBACK_CONTEXT`).
pub use crate::commands::FALLBACK_CONTEXT;

/// What a provider's listing (or the bundled catalog) says about a model.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelFacts {
    pub context_length: Option<usize>,
    /// It accepts images.
    pub vision: Option<bool>,
}

/// `provider\nmodel` → its facts, as the provider lists them (asked once).
#[derive(Default)]
pub(crate) struct ModelCache(Mutex<HashMap<String, ModelFacts>>);

fn cache_key(provider: &str, model: &str) -> String {
    format!("{provider}\n{model}")
}

impl ModelCache {
    pub(crate) fn get(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        self.0
            .lock()
            .ok()?
            .get(&cache_key(provider, model))
            .copied()
    }

    fn insert(&self, provider: &str, model: &str, facts: ModelFacts) {
        if let Ok(mut facts_by_model) = self.0.lock() {
            facts_by_model.insert(cache_key(provider, model), facts);
        }
    }

    fn clear(&self) {
        if let Ok(mut facts) = self.0.lock() {
            facts.clear();
        }
    }
}

/// Whether the default provider is usable (`setup.status`, `setup.runtime_check`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupStatus {
    /// A callable provider and a model are set.
    pub ready: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Whether the active provider is callable and a model is set.
pub fn setup_status(settings: &Settings) -> SetupStatus {
    let provider = settings.active();
    SetupStatus {
        ready: provider.is_some_and(|p| p.ready()) && settings.model.is_some(),
        provider: provider.map(|p| p.id.clone()),
        model: settings.model.clone(),
    }
}

/// One provider in the model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<String>,
    pub is_current: bool,
    pub base_url: String,
    pub authenticated: bool,
}

/// The model picker: each provider and its models, and the current choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOptions {
    pub providers: Vec<PickerProvider>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// Why finding a provider for a session failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// The store could not be read.
    Store(String),
    /// No such session.
    NoSession(String),
    /// No provider is configured.
    NoProvider,
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::Store(text) => f.write_str(text),
            RouteError::NoSession(id) => write!(f, "Session {id} not found"),
            RouteError::NoProvider => f.write_str("No provider is configured"),
        }
    }
}

impl std::error::Error for RouteError {}

/// Why imported sessions have nothing to talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportRouteError {
    NoEndpoint,
    NoModel,
}

impl std::fmt::Display for ImportRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ImportRouteError::NoEndpoint => {
                "Set up an endpoint first (Settings ▸ Providers): imported sessions use it."
            }
            ImportRouteError::NoModel => "Choose a model first: imported sessions use it.",
        })
    }
}

impl std::error::Error for ImportRouteError {}

/// A model pick: the model picker's `config.set model "<model> --provider <id> [--session]"`.
#[derive(Debug, Clone)]
pub struct ModelSwitch {
    pub model: String,
    /// `None`: the default provider.
    pub provider: Option<String>,
    /// Only the session's; else the default for new sessions too.
    pub session_only: bool,
    pub session_id: Option<String>,
}

/// What a model pick settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitched {
    pub model: String,
    pub provider: String,
}

/// The daily request limits the ledger enforces, from two places: a provider's own settings
/// (`config.json`'s `daily_request_limit`, Settings ▸ Providers ▸ Custom endpoints) and the config
/// record's `requests.limits.<id>` (`daily_limit`, `reset_offset_minutes`). The record's
/// `daily_limit`, when it is a positive whole number, wins; null, 0 or nonsense there means "the
/// record sets none", so the provider's own limit (if any) stands. Its offset (at most ±24 h, else
/// 0 = UTC) says where the provider's day starts. Applied as a whole by `Ledger::configure`: a
/// provider in neither place has no limit.
pub fn provider_limits(
    settings: &Settings,
    record: &Value,
) -> Vec<hermes_runtime::ledger::ProviderLimit> {
    use hermes_runtime::ledger::ProviderLimit;
    let mut merged: std::collections::BTreeMap<String, ProviderLimit> = settings
        .providers
        .iter()
        .map(|p| {
            let limit = ProviderLimit {
                provider: p.id.clone(),
                daily_limit: p.daily_request_limit.filter(|n| *n > 0),
                reset_offset_minutes: 0,
            };
            (p.id.clone(), limit)
        })
        .collect();
    for (provider, limit) in record.as_object().into_iter().flatten() {
        let entry = merged
            .entry(provider.clone())
            .or_insert_with(|| ProviderLimit {
                provider: provider.clone(),
                daily_limit: None,
                reset_offset_minutes: 0,
            });
        if let Some(n) = limit["daily_limit"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
        {
            entry.daily_limit = Some(n);
        }
        entry.reset_offset_minutes = limit["reset_offset_minutes"]
            .as_i64()
            .filter(|m| m.abs() <= 24 * 60)
            .unwrap_or(0) as i32;
    }
    merged.into_values().collect()
}

/// `terminal.sandbox_grants`: the non-empty strings of a list, trimmed (anything else: none).
pub fn sandbox_grants(value: &Value) -> Vec<PathBuf> {
    value
        .as_array()
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| folder.as_str())
                .filter(|folder| !folder.trim().is_empty())
                .map(|folder| PathBuf::from(folder.trim()))
                .collect()
        })
        .unwrap_or_default()
}

impl Core {
    /// The Settings the runtime keeps in memory, from the stored config record: at startup and
    /// after every change. (Auto-archive, which a change may switch on, is `sweep_archive`; a
    /// front end that shows the approval mode refreshes after this.)
    pub fn apply_settings(&self) {
        let config = &self.config;
        self.apply_approvals_mode(&config.value("approvals.mode"));
        // Variables the terminal keeps although their names look like secrets (`terminal.
        // env_passthrough`; everything with KEY/TOKEN/SECRET/PASSWORD/AUTH... is dropped).
        hermes_tools::set_env_passthrough(
            config
                .value("terminal.env_passthrough")
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        );
        // The shell's OS sandbox (`terminal.sandbox`: "off", the default, or "low": a
        // low-integrity token, backend v2 Phase 15). Config record only, no Settings field, so
        // it takes effect at startup (a hand edit of desktop-config.json needs a restart); the
        // level is read at each spawn, so it applies to the next command.
        // Under "low" the configured Git Bash is replaced by PowerShell (Git Bash can't start at
        // low integrity; each result says "sandbox=low: ran in PowerShell, not bash"), and the
        // workspace, scratch and cache folders are labelled Low (`sandbox-labels.json`; undone
        // with `/sandbox remove-labels`).
        hermes_tools::set_sandbox(hermes_tools::Sandbox::from_config(
            &config.value("terminal.sandbox"),
        ));
        // Folders the user lets sandboxed commands write to (`terminal.sandbox_grants`: a list of
        // absolute paths; config record only): labelled Low by the next `low` command, with the
        // same record and refusals as the workspace.
        hermes_tools::set_sandbox_grants(sandbox_grants(&config.value("terminal.sandbox_grants")));
        // Each provider's daily request limit and where its day starts, for the request ledger
        // and its governor (`provider_limits`).
        self.runtime.ledger().configure(provider_limits(
            &Settings::load(&self.data_dir),
            &config.value("requests.limits"),
        ));
        // The budget profile and its overrides (`budget.*`): knobs for the next turn; the prompt
        // wording and the skills block they pick apply to sessions made after this.
        self.runtime
            .set_budget(hermes_runtime::budget::from_config(&config.value("budget")));
        // Backend v2 Phase 8: the curator's merge and repair proposals apply at once instead of
        // waiting in Pending (off by default).
        self.runtime
            .set_curator_auto_apply(config.value("curator.auto_apply") == true);
        // File checkpoints (`/rollback`), before the turn's first write.
        self.runtime
            .checkpoints()
            .set_enabled(config.value("checkpoints.enabled") != false);
        // Settings ▸ Memory: which files new sessions keep and get, their sizes, approvals.
        let memory = config.value("memory");
        let defaults = hermes_memory::MemoryConfig::default();
        let flag = |key: &str, default: bool| memory[key].as_bool().unwrap_or(default);
        let size = |key: &str, default: usize| {
            memory[key]
                .as_u64()
                .filter(|n| *n > 0)
                .map_or(default, |n| n as usize)
        };
        self.runtime
            .memory()
            .set_config(hermes_memory::MemoryConfig {
                memory_enabled: flag("memory_enabled", defaults.memory_enabled),
                user_profile_enabled: flag("user_profile_enabled", defaults.user_profile_enabled),
                memory_char_limit: size("memory_char_limit", defaults.memory_char_limit),
                user_char_limit: size("user_char_limit", defaults.user_char_limit),
                // Held to its bounds like a budget knob: anything outside (or not a whole
                // number) is the default, so a typo can't make the project block huge or empty.
                project_char_limit: memory["project_char_limit"]
                    .as_u64()
                    .map(|n| n as usize)
                    .filter(|n| {
                        (hermes_memory::PROJECT_LIMIT_MIN..=hermes_memory::PROJECT_LIMIT_MAX)
                            .contains(n)
                    })
                    .unwrap_or(defaults.project_char_limit),
                write_approval: flag("write_approval", defaults.write_approval),
            });
        // The tools switched off in Capabilities ▸ Tools.
        let off = crate::toolsets::disabled_toolsets(&config.value(crate::toolsets::CONFIG_KEY));
        self.runtime
            .set_disabled_tools(crate::toolsets::disabled_tools(&off));
    }

    /// Settings ▸ Sessions' auto-archive (`archive.rs`): when it's on, sessions idle for
    /// `sessions.auto_archive_days` go to the archive (at most once a day). Blocking.
    pub fn sweep_archive(&self) {
        let sessions = self.config.value("sessions");
        if sessions["auto_archive"] != true {
            return;
        }
        let days = sessions["auto_archive_days"]
            .as_f64()
            .filter(|d| *d >= 1.0)
            .unwrap_or(crate::archive::DEFAULT_IDLE_DAYS);
        self.auto_archive(days);
    }

    /// Hermes's `approvals.mode` on the runtime's policy: manual asks, smart has a reviewer model
    /// judge first, off runs flagged commands (hardline ones never run).
    fn apply_approvals_mode(&self, mode: &Value) {
        let approvals = self.runtime.approvals();
        let mode = mode.as_str().unwrap_or("manual");
        approvals.set_ask(mode != "off");
        approvals.set_smart(mode == "smart");
    }

    /// The effort picked for this session, if any (not Settings' default). Kept by the session's
    /// actor (`PageState::effort`).
    pub fn picked_effort(&self, id: &str) -> Option<String> {
        self.runtime.existing_session_handle(id)?.page().effort
    }

    /// The session's reasoning effort: picked in the composer, else Settings' default.
    pub fn effort(&self, id: &str) -> String {
        self.picked_effort(id)
            .or_else(|| {
                self.config
                    .value("agent.reasoning_effort")
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    /// Picks the session's effort (`None`: Settings' again). A session may get one before it
    /// was ever loaded (its actor starts here; nothing is read).
    pub fn pick_effort(&self, id: &str, effort: Option<String>) {
        let picked = match &effort {
            Some(_) => self
                .runtime
                .session_handle(id)
                .map_err(|e| format!("{e:#}")),
            None => match self.runtime.existing_session_handle(id) {
                Some(handle) => Ok(handle),
                None => return,
            },
        }
        .and_then(|handle| handle.set_effort(effort).map_err(|e| e.to_string()));
        if let Err(error) = picked {
            tracing::warn!("picking the effort of {id}: {error}");
        }
    }

    /// The provider and model picked while the session's turn runs, if any.
    pub fn pending_route(&self, id: &str) -> Option<(String, String)> {
        self.runtime
            .existing_session_handle(id)?
            .page()
            .pending_route
    }

    /// What every turn of a session runs with: its model's window (so compression starts in
    /// time for a small local model) and the side jobs' models (Settings ▸ Model ▸ Auxiliary),
    /// the effort, vision support and the fallback chain (Settings ▸ Model ▸ Fallbacks).
    pub fn turn_options(&self, session: &str) -> TurnOptions {
        let settings = Settings::load(&self.data_dir);
        let record = self.runtime.store().session(session).ok().flatten();
        let context = |provider: &str, model: &str| self.known_context(provider, model);
        // Only a picked effort is sent; unset leaves the provider's default (as Hermes).
        let effort = self
            .picked_effort(session)
            // "ultra" is Hermes's own step above max: the wire has no more than max.
            .map(|e| if e == "ultra" { "max".to_string() } else { e });
        let mut aux = crate::aux_models::runtime_models(&settings, &context);
        // "Inherit main model effort" (the Auxiliary page's default): compression and the
        // background review think as hard as the chat. The approval judge and titles keep
        // reasoning off unless picked, as Hermes runs them.
        for job in [&mut aux.compression, &mut aux.review]
            .into_iter()
            .flatten()
        {
            if job.effort.is_none() {
                job.effort = effort.clone();
            }
        }
        TurnOptions {
            context_length: record.as_ref().and_then(|r| context(&r.provider, &r.model)),
            effort,
            supports_vision: record
                .as_ref()
                .is_some_and(|r| self.sends_images(&r.provider, &r.model)),
            aux,
            // Settings ▸ Model ▸ Fallbacks (`fallback.rs`): the providers a request moves to when
            // the main one cannot serve it.
            fallback: crate::fallback::resolve(
                &self.config.value("fallback_providers"),
                &settings,
                record
                    .as_ref()
                    .map(|r| (r.provider.as_str(), r.model.as_str())),
                &context,
                &|provider, model| self.sends_images(provider, model),
            ),
        }
    }

    /// The model picker: each provider and its models (listed live from the provider). For a
    /// session, its own model is the current one.
    pub async fn model_options(
        &self,
        settings: &Settings,
        session_id: Option<&str>,
    ) -> ModelOptions {
        let record = session_id.and_then(|id| self.runtime.store().session(id).ok().flatten());
        let (current_provider, current_model) = match &record {
            Some(record) => match self.pending_route(&record.id) {
                Some((provider, model)) => (Some(provider), Some(model)),
                None => (Some(record.provider.clone()), Some(record.model.clone())),
            },
            None => (settings.provider.clone(), settings.model.clone()),
        };
        let mut providers = Vec::new();
        for provider in &settings.providers {
            providers.push(PickerProvider {
                is_current: current_provider.as_deref() == Some(provider.id.as_str()),
                models: provider.model_ids().await,
                id: provider.id.clone(),
                name: provider.name.clone(),
                base_url: provider.base_url.clone(),
                authenticated: provider.ready(),
            });
        }
        ModelOptions {
            providers,
            model: current_model,
            provider: current_provider,
        }
    }

    /// Switches the session's model and, without `session_only`, makes it the default for new
    /// sessions (as the GPUI app's picker does). A session keeps its provider; another one needs
    /// a new session. `changed` is called with the session's id when its model changed or a pick
    /// was deferred (the page's `session.info`), at the point the change happens. Blocking.
    pub fn switch_model(
        &self,
        request: &ModelSwitch,
        settings: &Settings,
        changed: &dyn Fn(&str),
    ) -> Result<ModelSwitched, String> {
        let provider = request
            .provider
            .clone()
            .or_else(|| settings.active().map(|p| p.id.clone()))
            .ok_or("No provider is configured".to_string())?;
        if settings.provider(&provider).is_none() {
            return Err(format!("Unknown provider {provider}"));
        }
        let model = request.model.clone();
        let record = request
            .session_id
            .as_deref()
            .and_then(|id| self.runtime.store().session(id).ok().flatten());
        if let Some(record) = &record {
            let moves = record.model != model || record.provider != provider;
            // Mid-reply: the pick applies from the next turn on (Hermes's deferred pick); it
            // shows as the session's model right away. Picking the running model again
            // withdraws an earlier pick. "Is a turn running" and keeping the pick are one step
            // of the session's actor: a turn ending in between gets the switch now instead,
            // and the actor drops a pick that turn left untaken, so it can't override this one.
            // (No actor: nothing runs and nothing was picked; none is started for this.)
            let pick = moves.then(|| (provider.clone(), model.clone()));
            let deferred = self
                .runtime
                .existing_session_handle(&record.id)
                .and_then(|handle| handle.set_pending_route(pick).ok())
                .unwrap_or(false);
            if deferred {
                changed(&record.id);
            } else if moves {
                self.route_session(&record.id, &provider, &model)?;
                changed(&record.id);
            }
        }
        if !request.session_only {
            crate::providers::save_default(&self.data_dir, &provider, &model)
                .map_err(|e| format!("{e:#}"))?;
        }
        Ok(ModelSwitched { model, provider })
    }

    /// Moves a session to `provider` / `model` (refused while a turn runs).
    pub fn route_session(&self, id: &str, provider: &str, model: &str) -> Result<(), String> {
        let record = self.runtime.store().session(id).ok().flatten();
        let other_provider = record
            .as_ref()
            .is_some_and(|r| r.provider != provider)
            .then_some(provider);
        self.runtime
            .set_session_route(id, other_provider, model, None)
            .map_err(|e| format!("{e:#}"))
    }

    /// A model picked while the last turn ran, applied now that it has finished. Returns the
    /// session's id when it was moved (its info changed).
    pub fn apply_pending_route(&self, id: &str) -> bool {
        let pick = self
            .runtime
            .existing_session_handle(id)
            .and_then(|handle| handle.take_pending_route().ok().flatten());
        let Some((provider, model)) = pick else {
            return false;
        };
        match self.route_session(id, &provider, &model) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!("applying the model picked during the turn in {id}: {error}");
                false
            }
        }
    }

    /// A model's context window when it's known without asking the provider: the endpoint's
    /// setting, else what its model list said (cached by `model_facts`). `None` lets the
    /// runtime assume its default; the 256k display fallback is never passed on.
    pub fn known_context(&self, provider: &str, model: &str) -> Option<usize> {
        let settings = Settings::load(&self.data_dir);
        if let Some(length) = settings.provider(provider).and_then(|p| p.context_length) {
            return Some(length);
        }
        self.models.get(provider, model)?.context_length
    }

    /// A model's context window, as its provider lists it (remembered once the provider has
    /// answered), else Hermes's fallback for an unknown window.
    pub async fn context_length(&self, provider: &str, model: &str) -> usize {
        let settings = Settings::load(&self.data_dir);
        if let Some(length) = settings.provider(provider).and_then(|p| p.context_length) {
            return length;
        }
        self.model_facts(provider, model)
            .await
            .context_length
            .unwrap_or(FALLBACK_CONTEXT)
    }

    /// The model's window and whether it takes images, as its provider lists it (filled from the
    /// bundled catalog), asked once.
    pub async fn model_facts(&self, provider: &str, model: &str) -> ModelFacts {
        if let Some(known) = self.models.get(provider, model) {
            return known;
        }
        let settings = Settings::load(&self.data_dir);
        let listed = match settings.provider(provider) {
            Some(provider) if provider.ready() => provider.runtime().list_models().await.ok(),
            _ => None,
        };
        // A provider that didn't answer is asked again next time; one that answered without the
        // model is not.
        let Some(models) = listed else {
            return ModelFacts::default();
        };
        let facts = models
            .into_iter()
            .find(|m| m.id == model)
            .map(|m| ModelFacts {
                context_length: m.context_length,
                vision: m.supports_vision,
            })
            .unwrap_or_default();
        self.models.insert(provider, model, facts);
        facts
    }

    /// Forgets what the providers said about their models (an endpoint changed).
    pub fn forget_model_facts(&self) {
        self.models.clear();
    }

    /// Whether attached images go to the model as images (Hermes's `agent.image_input_mode`:
    /// `native` always, `text` never, `auto` when the model takes them). Otherwise the prompt
    /// names the image's path, for `vision_analyze`.
    pub fn sends_images(&self, provider: &str, model: &str) -> bool {
        match self.config.value("agent.image_input_mode").as_str() {
            Some("native") => true,
            Some("text") => false,
            _ => self
                .models
                .get(provider, model)
                .and_then(|f| f.vision)
                .unwrap_or(false),
        }
    }

    /// The provider a session's turns go to (its own, else the default one).
    pub fn provider_for(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<ProviderConfig, RouteError> {
        let record = self
            .runtime
            .store()
            .session(id)
            .map_err(|e| RouteError::Store(format!("{e:#}")))?
            .ok_or_else(|| RouteError::NoSession(id.to_string()))?;
        Ok(provider_of(settings, &record.provider)?.runtime())
    }

    /// The provider a session talks to (its own, else the default); a store that can't be read
    /// counts as no session.
    pub fn session_provider(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<ProviderConfig, RouteError> {
        let record = self
            .runtime
            .store()
            .session(id)
            .ok()
            .flatten()
            .ok_or_else(|| RouteError::NoSession(id.to_string()))?;
        Ok(provider_of(settings, &record.provider)?.runtime())
    }

    /// What imported sessions talk to: the default endpoint and model.
    pub fn import_route(
        &self,
        settings: &Settings,
    ) -> Result<(ProviderConfig, String), ImportRouteError> {
        let provider = settings.active().ok_or(ImportRouteError::NoEndpoint)?;
        let model = settings
            .model
            .clone()
            .or_else(|| provider.default_model.clone())
            .ok_or(ImportRouteError::NoModel)?;
        Ok((provider.runtime(), model))
    }
}

/// A session's own provider, else the default one.
fn provider_of<'a>(settings: &'a Settings, id: &str) -> Result<&'a ProviderSettings, RouteError> {
    settings
        .provider(id)
        .or(settings.active())
        .ok_or(RouteError::NoProvider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(json: Value) -> Settings {
        serde_json::from_value(json).unwrap()
    }

    fn limit_of(
        limits: &[hermes_runtime::ledger::ProviderLimit],
        id: &str,
    ) -> Option<(Option<u32>, i32)> {
        limits
            .iter()
            .find(|l| l.provider == id)
            .map(|l| (l.daily_limit, l.reset_offset_minutes))
    }

    #[test]
    fn endpoint_limits_and_the_config_record_merge() {
        let s = settings(json!({"providers": [
            {"id": "a", "name": "A", "base_url": "http://a", "daily_request_limit": 800},
            {"id": "b", "name": "B", "base_url": "http://b"},
            {"id": "c", "name": "C", "base_url": "http://c", "daily_request_limit": 50},
        ]}));
        let record = json!({
            "a": {"reset_offset_minutes": 120},
            "b": {"daily_limit": 300},
            "c": {"daily_limit": null},
            "d": {"daily_limit": 10, "reset_offset_minutes": 99999},
        });
        let limits = provider_limits(&s, &record);
        // The endpoint's limit with the record's day start.
        assert_eq!(limit_of(&limits, "a"), Some((Some(800), 120)));
        // Only the record has one.
        assert_eq!(limit_of(&limits, "b"), Some((Some(300), 0)));
        // A null in the record leaves the endpoint's limit.
        assert_eq!(limit_of(&limits, "c"), Some((Some(50), 0)));
        // A provider only in the record; an offset beyond a day is UTC.
        assert_eq!(limit_of(&limits, "d"), Some((Some(10), 0)));
        // The record's limit wins over the endpoint's.
        let limits = provider_limits(&s, &json!({"a": {"daily_limit": 5}}));
        assert_eq!(limit_of(&limits, "a"), Some((Some(5), 0)));
    }

    #[test]
    fn no_providers_and_no_record_means_no_limits() {
        assert!(provider_limits(&Settings::default(), &Value::Null).is_empty());
        let limits = provider_limits(&Settings::default(), &json!({"x": {"daily_limit": 7}}));
        assert_eq!(limit_of(&limits, "x"), Some((Some(7), 0)));
    }

    #[test]
    fn a_hand_edited_bad_limit_does_not_lose_the_providers() {
        for bad in [
            json!("800"),
            json!(-1),
            json!(5_000_000_000u64),
            json!(0),
            json!(1.5),
        ] {
            let s = settings(json!({"providers": [
                {"id": "a", "name": "A", "base_url": "http://a", "daily_request_limit": bad},
            ]}));
            assert_eq!(s.providers.len(), 1, "{bad}");
            assert_eq!(s.providers[0].daily_request_limit, None, "{bad}");
        }
    }

    #[test]
    fn sandbox_grants_are_read_from_the_config_list() {
        assert_eq!(
            sandbox_grants(&json!([" D:\\data ", "", 3, "E:\\x"])),
            [PathBuf::from("D:\\data"), PathBuf::from("E:\\x")]
        );
        assert!(sandbox_grants(&json!("D:\\data")).is_empty());
        assert!(sandbox_grants(&Value::Null).is_empty());
    }

    #[test]
    fn setup_is_ready_with_a_callable_provider_and_a_model() {
        // A local endpoint needs no key.
        let ready = settings(json!({"providers": [
            {"id": "local", "name": "L", "base_url": "http://127.0.0.1:1/v1"}], "model": "m"}));
        let status = setup_status(&ready);
        assert!(status.ready);
        assert_eq!(status.provider.as_deref(), Some("local"));
        // No model: not ready. No provider: not ready.
        let no_model = settings(json!({"providers": [
            {"id": "local", "name": "L", "base_url": "http://127.0.0.1:1/v1"}]}));
        assert!(!setup_status(&no_model).ready);
        assert!(!setup_status(&Settings::default()).ready);
    }
}
