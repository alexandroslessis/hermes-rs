//! Setup, profile and settings calls (`setup.status`, `config.get`, `config.set`, the model
//! picker) and the answers for features this backend doesn't have. Hermes:
//! `tui_gateway/server.py` (`config.*`, `setup.*`), `tui_gateway/methods_session.py`.

use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{ConfigGet, ConfigSet, Reply, RpcError};
use crate::providers::Settings;

impl Gateway {
    pub(super) fn setup_status(&self, settings: &Settings) -> Reply {
        let provider = settings.active();
        let ready = provider.is_some_and(|p| p.ready()) && settings.model.is_some();
        Ok(json!({
            "ok": true,
            "ready": ready,
            "provider_configured": ready,
            "inference_provider": provider.map(|p| p.id.clone()),
            "profile": "default",
        }))
    }

    pub(super) fn profiles_list(&self, settings: &Settings) -> Reply {
        Ok(json!({"profiles": [{
            "name": "default",
            "path": self.data_dir.display().to_string(),
            "is_default": true,
            "model": settings.model,
            "provider": settings.provider,
        }]}))
    }

    pub(super) fn setup_runtime_check(&self, settings: &Settings) -> Reply {
        let provider = settings.active();
        let ok = provider.is_some_and(|p| p.ready()) && settings.model.is_some();
        Ok(json!({
            "ok": ok,
            "provider": provider.map(|p| p.id.clone()),
            "model": settings.model,
            "profile": "default",
            "error": (!ok).then_some("No model is set up"),
        }))
    }

    pub(super) fn config_get(&self, p: ConfigGet, settings: &Settings) -> Reply {
        Ok(json!({
            "value": p.key
                .as_deref()
                .map(|key| self.rest.config_value(key))
                .filter(|value| !value.is_null())
                .unwrap_or(json!("")),
            "model": settings.model,
            "provider": settings.provider,
        }))
    }

    pub(super) async fn config_set(self: &Arc<Self>, p: ConfigSet, settings: &Settings) -> Reply {
        if p.model {
            let (raw, s) = (p.raw.clone(), settings.clone());
            return self.blocking(move |g| g.switch_model(&raw, &s)).await?;
        }
        // A session's effort; any other key is a Settings value, kept in the config record.
        let key = p.key.as_str();
        let value = p.value.clone();
        match (key, p.session_id.as_deref()) {
            ("reasoning", Some(id)) => {
                let effort = value.as_str().unwrap_or_default().to_lowercase();
                // Nothing picked (an editor's "Default"): Settings' effort again.
                self.pick_effort(id, Some(effort.clone()).filter(|e| !e.is_empty()));
                self.emit(id, "session.info", self.session_info(id));
                Ok(json!({"key": key, "value": effort, "ok": true}))
            }
            ("", _) => Err(RpcError::invalid("key is required")),
            // `budget.profile` and the per-knob overrides: a bad value is refused here
            // (0 for a cap, an unknown profile), not stored and then ignored.
            (key, _) if key.starts_with("budget.") => {
                hermes_runtime::budget::check_config_value(&key["budget.".len()..], &value)
                    .map_err(RpcError::invalid)?;
                self.rest.set_config_value(key, value.clone());
                self.apply_settings();
                Ok(json!({"key": key, "value": value, "ok": true}))
            }
            _ => {
                let key = if key == "reasoning" {
                    "agent.reasoning_effort"
                } else {
                    key
                };
                self.rest.set_config_value(key, value.clone());
                self.apply_settings();
                Ok(json!({"key": key, "value": value, "ok": true}))
            }
        }
    }

    /// `pet.info`: polled on start and on every focus; features this backend doesn't have answer
    /// "off" so the page stops asking and shows nothing for them.
    pub(super) fn pet_info() -> Value {
        json!({"enabled": false})
    }

    pub(super) fn shared_metrics_status() -> Value {
        json!({"enabled": false, "send": false, "decided": true})
    }

    pub(super) fn free_tier_status() -> Value {
        json!({
            "has_guest": false,
            "enabled": false,
            "available": false,
            "notice_pending": false,
            "model": "",
            "label": "",
        })
    }

    pub(super) fn wake_status() -> Value {
        json!({
            "listening": false,
            "owned_by_caller": false,
            "phrase": "",
            "provider": "",
            "configured_surface": "",
            "input_device": {},
            "available": false,
            "hint": "Voice wake isn't available in hermes-rs.",
            "enabled": false,
            "audio_silent": true,
            "capture": "none",
            "local_input_available": false,
            "sample_rate": 16000,
            "frame_length": 512,
        })
    }
}

/// What a provider's listing (or the bundled catalog) says about a model.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ModelFacts {
    pub(super) context_length: Option<usize>,
    /// It accepts images.
    pub(super) vision: Option<bool>,
}

/// The window assumed for a model whose provider doesn't list one (Hermes's
/// `DEFAULT_FALLBACK_CONTEXT`).
pub(super) const FALLBACK_CONTEXT: usize = 256_000;

impl Gateway {
    /// The Settings the runtime keeps in memory, from the stored config record: at startup and
    /// after every change.
    pub(super) fn apply_settings(&self) {
        self.apply_approvals_mode(&self.rest.config_value("approvals.mode"));
        // Variables the terminal keeps although their names look like secrets (`terminal.
        // env_passthrough`; everything with KEY/TOKEN/SECRET/PASSWORD/AUTH... is dropped).
        hermes_tools::set_env_passthrough(
            self.rest
                .config_value("terminal.env_passthrough")
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        );
        // Each provider's daily request limit and where its day starts, for the request ledger
        // and its governor (`provider_limits`).
        self.runtime.ledger().configure(provider_limits(
            &Settings::load(&self.data_dir),
            &self.rest.config_value("requests.limits"),
        ));
        // The budget profile and its overrides (`budget.*`): knobs for the next turn; the prompt
        // wording and the skills block they pick apply to sessions made after this.
        self.runtime.set_budget(hermes_runtime::budget::from_config(
            &self.rest.config_value("budget"),
        ));
        // Backend v2 Phase 8: the curator's merge and repair proposals apply at once instead of
        // waiting in Pending (off by default).
        self.runtime
            .set_curator_auto_apply(self.rest.config_value("curator.auto_apply") == true);
        // File checkpoints (`/rollback`), before the turn's first write.
        self.runtime
            .checkpoints()
            .set_enabled(self.rest.config_value("checkpoints.enabled") != false);
        // Settings ▸ Memory: which files new sessions keep and get, their sizes, approvals.
        let memory = self.rest.config_value("memory");
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
        let off = crate::toolsets::disabled_toolsets(
            &self.rest.config_value(crate::toolsets::CONFIG_KEY),
        );
        self.runtime
            .set_disabled_tools(crate::toolsets::disabled_tools(&off));
        // Auto-archive just switched on sweeps now (it waits a day between sweeps itself).
        self.auto_archive();
    }

    /// Hermes's `approvals.mode` on the runtime's policy: manual asks, smart has a reviewer model
    /// judge first, off runs flagged commands (hardline ones never run).
    pub(super) fn apply_approvals_mode(&self, mode: &Value) {
        let approvals = self.runtime.approvals();
        let mode = mode.as_str().unwrap_or("manual");
        approvals.set_ask(mode != "off");
        approvals.set_smart(mode == "smart");
        // The status bar and Settings each change it: open pages hear the new mode at once.
        for id in self.hub.followed() {
            self.emit(&id, "session.info", self.session_info(&id));
        }
    }

    /// Hermes's model picker: each provider and its models (listed live from the provider). For
    /// a session, its own model is the current one.
    pub(super) async fn model_options(
        &self,
        settings: &Settings,
        session_id: Option<&str>,
    ) -> Value {
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
            let current = current_provider.as_deref() == Some(provider.id.as_str());
            let models = provider.model_ids().await;
            providers.push(json!({
                "slug": provider.id,
                "name": provider.name,
                "total_models": models.len(),
                "models": models,
                "is_current": current,
                "is_user_defined": true,
                "api_url": provider.base_url,
                "authenticated": provider.ready(),
            }));
        }
        json!({
            "providers": providers,
            "model": current_model,
            "provider": current_provider,
        })
    }

    /// The model picker's `config.set model "<model> --provider <id> [--session]"`: switches the
    /// session's model and, without `--session`, makes it the default for new sessions (as the
    /// GPUI app's picker does). A session keeps its provider; another one needs a new session.
    pub(super) fn switch_model(&self, params: &Value, settings: &Settings) -> Reply {
        let value = params["value"].as_str().unwrap_or_default();
        let mut words = value.split_whitespace();
        let model = words
            .next()
            .ok_or(RpcError::invalid("a model is required"))?
            .to_string();
        let (mut provider, mut session_only) = (None, false);
        while let Some(word) = words.next() {
            match word {
                "--provider" => provider = words.next().map(str::to_string),
                "--session" => session_only = true,
                _ => {}
            }
        }
        let provider = provider
            .or_else(|| settings.active().map(|p| p.id.clone()))
            .ok_or(RpcError::server("No provider is configured"))?;
        if settings.provider(&provider).is_none() {
            return Err(RpcError::server(format!("Unknown provider {provider}")));
        }
        let record = params["session_id"]
            .as_str()
            .and_then(|id| self.runtime.store().session(id).ok().flatten());
        if let Some(record) = &record {
            let changes = record.model != model || record.provider != provider;
            // Mid-reply: the pick applies from the next turn on (Hermes's deferred pick); it
            // shows as the session's model right away. Picking the running model again
            // withdraws an earlier pick. "Is a turn running" and keeping the pick are one step
            // of the session's actor: a turn ending in between gets the switch now instead,
            // and the actor drops a pick that turn left untaken, so it can't override this one.
            // (No actor: nothing runs and nothing was picked; none is started for this.)
            let pick = changes.then(|| (provider.clone(), model.clone()));
            let deferred = self
                .runtime
                .existing_session_handle(&record.id)
                .and_then(|handle| handle.set_pending_route(pick).ok())
                .unwrap_or(false);
            if deferred {
                self.emit(&record.id, "session.info", self.session_info(&record.id));
            } else if changes {
                self.route_session(&record.id, &provider, &model)?;
            }
        }
        if !session_only {
            crate::providers::save_default(&self.data_dir, &provider, &model)
                .map_err(|e| RpcError::server(format!("{e:#}")))?;
        }
        Ok(json!({"key": "model", "value": model, "provider": provider}))
    }

    /// Moves a session to `provider` / `model` (refused while a turn runs).
    pub(super) fn route_session(
        &self,
        id: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), RpcError> {
        let record = self.runtime.store().session(id).ok().flatten();
        let other_provider = record
            .as_ref()
            .is_some_and(|r| r.provider != provider)
            .then_some(provider);
        self.runtime
            .set_session_route(id, other_provider, model, None)
            .map_err(|e| RpcError::server(format!("{e:#}")))?;
        self.emit(id, "session.info", self.session_info(id));
        Ok(())
    }

    /// A model's context window, as its provider lists it (remembered once the provider has
    /// answered), else Hermes's fallback for an unknown window.
    /// A model's context window when it's known without asking the provider: the endpoint's
    /// setting, else what its model list said (cached by `context_length`). `None` lets the
    /// runtime assume its default; the 256k display fallback is never passed on.
    pub(super) fn known_context(&self, provider: &str, model: &str) -> Option<usize> {
        let settings = Settings::load(&self.data_dir);
        if let Some(length) = settings.provider(provider).and_then(|p| p.context_length) {
            return Some(length);
        }
        self.context_lengths
            .lock()
            .ok()?
            .get(&format!("{provider}\n{model}"))
            .and_then(|facts| facts.context_length)
    }

    pub(super) async fn context_length(&self, provider: &str, model: &str) -> usize {
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
    pub(super) async fn model_facts(&self, provider: &str, model: &str) -> ModelFacts {
        let key = format!("{provider}\n{model}");
        if let Some(known) = self
            .context_lengths
            .lock()
            .ok()
            .and_then(|lengths| lengths.get(&key).copied())
        {
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
        if let Ok(mut lengths) = self.context_lengths.lock() {
            lengths.insert(key, facts);
        }
        facts
    }

    /// Whether attached images go to the model as images (Hermes's `agent.image_input_mode`:
    /// `native` always, `text` never, `auto` when the model takes them). Otherwise the prompt
    /// names the image's path, for `vision_analyze`.
    pub(super) fn sends_images(&self, provider: &str, model: &str) -> bool {
        match self.rest.config_value("agent.image_input_mode").as_str() {
            Some("native") => true,
            Some("text") => false,
            _ => self
                .context_lengths
                .lock()
                .ok()
                .and_then(|facts| {
                    facts
                        .get(&format!("{provider}\n{model}"))
                        .and_then(|f| f.vision)
                })
                .unwrap_or(false),
        }
    }

    /// The provider a session's turns go to.
    pub(super) fn provider_for(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, RpcError> {
        let record = self
            .runtime
            .store()
            .session(id)
            .map_err(|e| RpcError::server(format!("{e:#}")))?
            .ok_or(RpcError::server(format!("Session {id} not found")))?;
        Ok(settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or(RpcError::server("No provider is configured"))?
            .runtime())
    }

    /// What imported sessions talk to: the default endpoint and model.
    pub(super) fn import_route(
        &self,
        settings: &Settings,
    ) -> Result<(hermes_runtime::ProviderConfig, String), RpcError> {
        let provider = settings.active().ok_or(RpcError::server(
            "Set up an endpoint first (Settings ▸ Providers): imported sessions use it."
                .to_string(),
        ))?;
        let model = settings
            .model
            .clone()
            .or_else(|| provider.default_model.clone())
            .ok_or(RpcError::server(
                "Choose a model first: imported sessions use it.",
            ))?;
        Ok((provider.runtime(), model))
    }

    /// The provider a session talks to (its own, else the default).
    pub(super) fn session_provider(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, RpcError> {
        let record = self
            .runtime
            .store()
            .session(id)
            .ok()
            .flatten()
            .ok_or(RpcError::invalid(format!("No session {id}.")))?;
        Ok(settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or(RpcError::server("No provider is configured"))?
            .runtime())
    }
}

/// `requests.status`: the request ledger's numbers for one provider, as the page reads them.
/// The daily request limits the ledger enforces, from two places: a provider's own settings
/// (`config.json`'s `daily_request_limit`, Settings ▸ Providers ▸ Custom endpoints) and the config
/// record's `requests.limits.<id>` (`daily_limit`, `reset_offset_minutes`). The record's
/// `daily_limit`, when it is a positive whole number, wins; null, 0 or nonsense there means "the
/// record sets none", so the provider's own limit (if any) stands. Its offset (at most ±24 h, else
/// 0 = UTC) says where the provider's day starts. Applied as a whole by `Ledger::configure`: a
/// provider in neither place has no limit.
fn provider_limits(
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

#[cfg(test)]
mod provider_limit_tests {
    use super::*;

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
}
