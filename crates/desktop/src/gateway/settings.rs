//! Setup, profile and settings calls (`setup.status`, `config.get`, `config.set`, the model
//! picker) and the answers for features this backend doesn't have. Hermes:
//! `tui_gateway/server.py` (`config.*`, `setup.*`), `tui_gateway/methods_session.py`.
//!
//! The logic (Settings put on the runtime, the model picker's data, switching a model, a model's
//! window, the provider a session talks to, a turn's options) is `hermes_core::settings`; this
//! file is the page's JSON, the RPC errors and the `session.info` frames a change triggers.

use std::sync::Arc;

use hermes_core::settings::{ImportRouteError, ModelSwitch, RouteError, setup_status};
use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{ConfigGet, ConfigSet, Reply, RpcError};
use crate::providers::Settings;

impl Gateway {
    pub(super) fn setup_status(&self, settings: &Settings) -> Reply {
        let status = setup_status(settings);
        Ok(json!({
            "ok": true,
            "ready": status.ready,
            "provider_configured": status.ready,
            "inference_provider": status.provider,
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
        let status = setup_status(settings);
        Ok(json!({
            "ok": status.ready,
            "provider": status.provider,
            "model": settings.model,
            "profile": "default",
            "error": (!status.ready).then_some("No model is set up"),
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
                self.core
                    .pick_effort(id, Some(effort.clone()).filter(|e| !e.is_empty()));
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

impl Gateway {
    /// The Settings the runtime keeps in memory, from the stored config record: at startup and
    /// after every change (`Core::apply_settings`).
    pub(super) fn apply_settings(&self) {
        self.core.apply_settings();
        // The status bar and Settings each change the approval mode: open pages hear the new mode
        // at once.
        for id in self.hub.followed() {
            self.emit(&id, "session.info", self.session_info(&id));
        }
        // Auto-archive just switched on sweeps now (it waits a day between sweeps itself).
        self.core.sweep_archive();
    }

    /// Hermes's model picker: each provider and its models (listed live from the provider). For
    /// a session, its own model is the current one.
    pub(super) async fn model_options(
        &self,
        settings: &Settings,
        session_id: Option<&str>,
    ) -> Value {
        let options = self.core.model_options(settings, session_id).await;
        json!({
            "providers": options.providers.iter().map(|provider| json!({
                "slug": provider.id,
                "name": provider.name,
                "total_models": provider.models.len(),
                "models": provider.models,
                "is_current": provider.is_current,
                "is_user_defined": true,
                "api_url": provider.base_url,
                "authenticated": provider.authenticated,
            })).collect::<Vec<_>>(),
            "model": options.model,
            "provider": options.provider,
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
        let request = ModelSwitch {
            model,
            provider,
            session_only,
            session_id: params["session_id"].as_str().map(str::to_string),
        };
        let switched = self
            .core
            .switch_model(&request, settings, &|id| {
                self.emit(id, "session.info", self.session_info(id))
            })
            .map_err(RpcError::server)?;
        Ok(json!({"key": "model", "value": switched.model, "provider": switched.provider}))
    }

    /// The provider a session's turns go to.
    pub(super) fn provider_for(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, RpcError> {
        self.core
            .provider_for(id, settings)
            .map_err(|e| RpcError::server(e.to_string()))
    }

    /// What imported sessions talk to: the default endpoint and model.
    pub(super) fn import_route(
        &self,
        settings: &Settings,
    ) -> Result<(hermes_runtime::ProviderConfig, String), RpcError> {
        self.core
            .import_route(settings)
            .map_err(|e: ImportRouteError| RpcError::server(e.to_string()))
    }

    /// The provider a session talks to (its own, else the default).
    pub(super) fn session_provider(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, RpcError> {
        self.core
            .session_provider(id, settings)
            .map_err(|e| match e {
                RouteError::NoSession(id) => RpcError::invalid(format!("No session {id}.")),
                other => RpcError::server(other.to_string()),
            })
    }
}
