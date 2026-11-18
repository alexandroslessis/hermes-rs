//! Hermes's config record as the Settings pages write it: `desktop-config.json` in the data
//! folder, read over Hermes's defaults for the keys this backend honours. Switched-off toolsets
//! and skills, the approval mode, the budget profile, the memory sizes, the sessions' auto-archive
//! and the rest of Settings are kept here, one dotted key at a time. The model and provider are
//! the hermes-rs settings' instead (`providers.rs`, `config.json`).
//!
//! Hermes: `hermes_cli/config.py` (`DEFAULT_CONFIG`: the defaults), `hermes_cli/web_routers/
//! config.py` (`/api/config`: a `PUT` deep-merges onto what is stored).
//!
//! The page's REST routes over this (`/api/config`, `/api/config/defaults`, the schema) are
//! `crates/desktop/src/rest.rs`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

/// The model new sessions use, from the hermes-rs settings (`config.json`).
#[derive(Clone, Debug, Default)]
pub struct DefaultModel {
    pub provider: String,
    pub model: String,
}

pub fn default_model(data_dir: &Path) -> DefaultModel {
    let config: Value = std::fs::read(data_dir.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    DefaultModel {
        provider: config["provider"].as_str().unwrap_or_default().to_string(),
        model: config["model"].as_str().unwrap_or_default().to_string(),
    }
}

/// Hermes's config defaults for the keys this backend honours (`hermes_cli/config.py`'s
/// `DEFAULT_CONFIG`): what `/api/config/defaults` reports and `/api/config` starts from.
pub fn defaults() -> Value {
    json!({
        "agent": {"reasoning_effort": ""},
        "display": {
            "show_reasoning": true,
            "busy_input_mode": "steer",
            "interim_assistant_messages": true,
            "timestamps": false,
            "tool_progress": "all",
        },
        "approvals": {"mode": "manual"},
        // The providers a request moves to when the main one cannot serve it (daily quota used
        // up, server down, unknown model): `[{provider, model}, ...]`, in order, at most three
        // (`fallback.rs`). Empty: no fallback.
        "fallback_providers": [],
        // How the agent spends model requests and tokens (`hermes_runtime::budget`): `requests`
        // (the default, also for a config without the key), `balanced` (how it worked before
        // profiles) or `tokens`. Per-knob overrides sit beside it (`budget.max_iterations`...).
        // `review_mode` "" is the profile's own background review (Phase 7: `idle_once` under
        // `requests`, `nudges` otherwise); a Settings select can pick another.
        "budget": {"profile": "requests", "review_mode": ""},
        // On by default (Hermes's is opt-in): a coding harness keeps a way back from the agent's
        // edits, as Claude Code does. Settings ▸ Safety ▸ Checkpoints switches it.
        // The skills curator (backend v2 Phase 8): `auto_apply` applies its merge and repair
        // proposals at once, through the same checks as an approval; off, they wait in Capabilities
        // ▸ Skills ▸ Pending.
        "curator": {"auto_apply": false},
        "checkpoints": {"enabled": true},
        "stt": {"enabled": false},
        // Settings ▸ Sessions: hide sessions idle this many days (`archive.rs`); off by default.
        "sessions": {"auto_archive": false, "auto_archive_days": 3},
        // The built-in memory (MEMORY.md, USER.md), applied to new sessions (Hermes's
        // `memory.*`; `write_approval` is ours: stage the agent's writes for review).
        "memory": {
            "memory_enabled": true,
            "user_profile_enabled": true,
            "memory_char_limit": 2200,
            "user_char_limit": 1375,
            "project_char_limit": 1500,
            "write_approval": false,
        },
    })
}

/// `PUT /api/config` deep-merges onto what's stored, as Hermes's does.
pub fn deep_merge(target: &mut Value, changes: &Value) {
    match (target.as_object_mut(), changes.as_object()) {
        (Some(target), Some(changes)) => {
            for (key, value) in changes {
                match target.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        deep_merge(existing, value)
                    }
                    _ => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        _ => *target = changes.clone(),
    }
}

/// The config record: `desktop-config.json`, held in memory and written on every change.
pub struct ConfigRecord {
    data_dir: PathBuf,
    record: Mutex<Value>,
}

impl ConfigRecord {
    /// Reads the record from the data folder (empty when there is none).
    pub fn load(data_dir: &Path) -> Self {
        let record = std::fs::read(data_dir.join("desktop-config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({}));
        Self {
            data_dir: data_dir.to_path_buf(),
            record: Mutex::new(record),
        }
    }

    /// The stored record over the defaults (unless `with_defaults` is off), with the model from
    /// the hermes-rs settings. `None` when the record's lock is poisoned.
    pub fn merged(&self, with_defaults: bool) -> Option<Value> {
        let stored = self.record.lock().ok()?.clone();
        let mut record = if with_defaults { defaults() } else { json!({}) };
        deep_merge(&mut record, &stored);
        let model = default_model(&self.data_dir);
        if !model.model.is_empty() {
            record["model"] = json!({"default": model.model, "provider": model.provider});
        }
        Some(record)
    }

    /// Merges `changes` (a partial record) into the stored one and writes it. The model is the
    /// hermes-rs settings' (the model picker changes it), so a `model` key is dropped; budget
    /// settings the runtime would refuse (an unknown profile, a 0 cap) are dropped, and a `budget`
    /// that isn't an object (it would replace the stored one whole) is too. `false` when the
    /// record's lock is poisoned.
    pub fn save(&self, mut changes: Value) -> bool {
        if let Some(changes) = changes.as_object_mut() {
            changes.remove("model");
        }
        if let Some(changes) = changes.as_object_mut() {
            match changes.get_mut("budget") {
                Some(Value::Object(budget)) => budget.retain(|key, value| {
                    hermes_runtime::budget::check_config_value(key, value).is_ok()
                }),
                Some(_) => {
                    changes.remove("budget");
                }
                None => {}
            }
        }
        let Ok(mut record) = self.record.lock() else {
            return false;
        };
        deep_merge(&mut record, &changes);
        let _ = std::fs::write(
            self.data_dir.join("desktop-config.json"),
            record.to_string(),
        );
        true
    }

    /// One dotted key of the record (`display.in_app_tips`), as `config.set` writes it.
    pub fn set_value(&self, key: &str, value: Value) {
        let changes = key
            .rsplit('.')
            .fold(value, |inner, part| json!({ part: inner }));
        self.save(changes);
    }

    /// One dotted key of the record (`display.busy_input_mode`), defaults included; `null` when
    /// it isn't there.
    pub fn value(&self, key: &str) -> Value {
        let record = self.merged(true).unwrap_or_default();
        key.split('.')
            .try_fold(&record, |value, part| value.get(part))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_config_merges_over_the_defaults_and_keeps_the_model_out() {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"provider":"ollama","model":"qwen"}"#,
        )
        .unwrap();
        let record = ConfigRecord::load(dir.path());
        assert!(
            record.save(json!({"display": {"show_reasoning": false}, "model": {"default": "x"}}))
        );
        let config = record.merged(true).unwrap();
        assert_eq!(config["display"]["show_reasoning"], false);
        // Untouched defaults stay, and the model is the hermes-rs settings'.
        assert_eq!(config["display"]["busy_input_mode"], "steer");
        assert_eq!(config["model"]["default"], "qwen");
        assert_eq!(record.value("approvals.mode"), "manual");
        // Without defaults: only what was saved (plus the model).
        assert!(record.merged(false).unwrap().get("approvals").is_none());
        // It survives a restart.
        assert_eq!(
            ConfigRecord::load(dir.path()).value("display.show_reasoning"),
            false
        );
    }

    #[test]
    fn a_dotted_key_is_set_and_read_back() {
        let dir = hermes_testdir::tempdir().unwrap();
        let record = ConfigRecord::load(dir.path());
        record.set_value("projects.active_id", json!("p1"));
        record.set_value("terminal.sandbox_grants", json!(["C:\\a"]));
        assert_eq!(record.value("projects.active_id"), "p1");
        assert_eq!(record.value("nothing.here"), Value::Null);
        // The other keys of the group are kept.
        record.set_value("display.timestamps", json!(true));
        assert_eq!(record.value("display.tool_progress"), "all");
    }

    #[test]
    fn budget_values_the_runtime_would_refuse_are_dropped() {
        let dir = hermes_testdir::tempdir().unwrap();
        let record = ConfigRecord::load(dir.path());
        assert_eq!(record.value("budget.profile"), "requests");
        record.save(json!({"budget": {"profile": "tokens", "max_iterations": 0}}));
        assert_eq!(record.value("budget.profile"), "tokens");
        assert_eq!(record.value("budget.max_iterations"), Value::Null);
        record.save(json!({"budget": {"profile": "fast"}}));
        assert_eq!(record.value("budget.profile"), "tokens");
        // Not an object: it would replace the stored one whole, so it is dropped.
        record.save(json!({"budget": "balanced"}));
        assert_eq!(record.value("budget.profile"), "tokens");
        assert_eq!(record.value("budget.review_mode"), "");
        record.save(json!({"budget": {"review_mode": "agentic"}}));
        assert_eq!(record.value("budget.review_mode"), "agentic");
        record.save(json!({"budget": {"review_mode": "never"}}));
        assert_eq!(record.value("budget.review_mode"), "agentic");
    }
}
