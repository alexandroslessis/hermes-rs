//! Hermes's REST routes (`hermes_cli/web_routers/*`) the frontend needs, answered from the
//! hermes-rs data folder. Shapes follow the frontend's types (`apps/desktop/src/types/hermes.ts`).
//! Anything not listed here gets Hermes's "No such API endpoint" 404.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The Hermes commit the bundled frontend was built from (what `/api/health` reports).
pub const HERMES_COMMIT: &str = "c419cf7c";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// A query parameter, percent-decoded (`+` is a space).
pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| percent_decode(value))
    })
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Our built-in desktop plugin (`plugins/hermes-rs/plugin.js`).
const BUILTIN_PLUGIN: &str = include_str!("../plugins/hermes-rs/plugin.js");

/// Hermes's disk door for desktop plugins: `<data folder>/desktop-plugins/<name>/plugin.js`,
/// with ours written there on start.
pub fn desktop_plugins_dir(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("desktop-plugins");
    let ours = dir.join("hermes-rs");
    if std::fs::create_dir_all(&ours).is_ok() {
        let _ = std::fs::write(ours.join("plugin.js"), BUILTIN_PLUGIN);
    }
    dir
}

/// Hermes's config defaults for the keys this backend honours (`hermes_cli/config.py`'s
/// `DEFAULT_CONFIG`): what `/api/config/defaults` reports and `/api/config` starts from.
fn config_defaults() -> Value {
    json!({
        "agent": {"reasoning_effort": "medium"},
        "display": {
            "show_reasoning": true,
            "busy_input_mode": "queue",
            "interim_assistant_messages": true,
            "timestamps": false,
            "tool_progress": "all",
        },
        "approvals": {"mode": "manual"},
        "stt": {"enabled": false},
    })
}

/// `PUT /api/config` deep-merges onto what's stored, as Hermes's does.
fn deep_merge(target: &mut Value, changes: &Value) {
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

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub struct Rest {
    pub data_dir: PathBuf,
    plugins_dir: PathBuf,
    /// The Desktop's interface preferences, kept as the frontend writes them.
    ui_preferences: std::sync::Mutex<Value>,
    /// Hermes's config record as the Settings pages write it (`desktop-config.json`); the
    /// model and provider come from the hermes-rs settings instead.
    config: std::sync::Mutex<Value>,
}

impl Rest {
    pub fn new(data_dir: PathBuf) -> Self {
        let ui_preferences =
            read_json(&data_dir.join("desktop-ui-preferences.json")).unwrap_or_else(|| json!({}));
        let config = read_json(&data_dir.join("desktop-config.json")).unwrap_or_else(|| json!({}));
        Self {
            plugins_dir: desktop_plugins_dir(&data_dir),
            data_dir,
            ui_preferences: std::sync::Mutex::new(ui_preferences),
            config: std::sync::Mutex::new(config),
        }
    }

    /// `GET /api/config`: the stored record over the defaults (unless `include_defaults=false`),
    /// with the model from the hermes-rs settings.
    fn config(&self, query: Option<&str>) -> Option<Value> {
        let stored = self.config.lock().ok()?.clone();
        let mut record = if query_param(query, "include_defaults").as_deref() == Some("false") {
            json!({})
        } else {
            config_defaults()
        };
        deep_merge(&mut record, &stored);
        let model = default_model(&self.data_dir);
        if !model.model.is_empty() {
            record["model"] = json!({"default": model.model, "provider": model.provider});
        }
        Some(record)
    }

    fn save_config(&self, body: &[u8]) -> Option<Value> {
        let body: Value = serde_json::from_slice(body).ok()?;
        let mut changes = body.get("config").cloned().unwrap_or(body);
        // The model is the hermes-rs settings' (the model picker changes it).
        if let Some(changes) = changes.as_object_mut() {
            changes.remove("model");
        }
        let mut config = self.config.lock().ok()?;
        deep_merge(&mut config, &changes);
        let _ = std::fs::write(
            self.data_dir.join("desktop-config.json"),
            config.to_string(),
        );
        Some(json!({"ok": true}))
    }

    /// A string setting from the config record (`display.busy_input_mode`), defaults included.
    pub fn config_value(&self, key: &str) -> Value {
        let record = self.config(None).unwrap_or_default();
        key.split('.')
            .try_fold(&record, |value, part| value.get(part))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// `/api/desktop-plugins/*` (`hermes_cli/web_routers/desktop_plugins.py`): the folder, its
    /// listings and file reads, only inside it.
    fn desktop_plugins(&self, route: &str, path: Option<&str>) -> Option<(u16, Value)> {
        let root = self.plugins_dir.display().to_string();
        if route == "root" {
            return Some((200, json!({ "root": root })));
        }
        if route == "reconcile" {
            return Some((200, json!({ "touched": [] })));
        }
        let path = PathBuf::from(path?);
        let inside = path
            .canonicalize()
            .ok()
            .zip(self.plugins_dir.canonicalize().ok())
            .is_some_and(|(p, r)| p.starts_with(r));
        if !inside {
            return Some((
                403,
                json!({ "detail": "outside the desktop plugins folder" }),
            ));
        }
        match route {
            "list" => {
                let entries: Vec<Value> = std::fs::read_dir(&path)
                    .map(|dir| {
                        dir.flatten()
                            .map(|entry| {
                                json!({
                                    "name": entry.file_name().to_string_lossy(),
                                    "path": entry.path().display().to_string(),
                                    "isDirectory": entry.path().is_dir(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some((200, json!({ "entries": entries })))
            }
            "read" => match std::fs::read_to_string(&path) {
                Ok(text) => Some((
                    200,
                    json!({ "path": path.display().to_string(), "text": text }),
                )),
                Err(error) => Some((404, json!({ "detail": error.to_string() }))),
            },
            _ => None,
        }
    }

    fn profile(&self) -> Value {
        let model = default_model(&self.data_dir);
        json!({
            "name": "default",
            "path": self.data_dir.display().to_string(),
            "is_default": true,
            "has_env": false,
            "model": (!model.model.is_empty()).then_some(model.model),
            "provider": (!model.provider.is_empty()).then_some(model.provider),
            "skill_count": 0,
        })
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        let ok = |value: Value| Some((200, value));
        if let Some(route) = path.strip_prefix("/api/desktop-plugins/") {
            return self.desktop_plugins(route, query_param(query, "path").as_deref());
        }
        match (method, path) {
            ("POST", "/api/webapp/socket-ticket") => ok(json!({
                "ticket": uuid::Uuid::new_v4().simple().to_string(),
                "ttl_seconds": 60,
            })),
            ("GET", "/api/health") => ok(json!({
                "ok": true,
                "version": VERSION,
                "displayVersion": VERSION,
                "commit": HERMES_COMMIT,
                "auth_required": false,
            })),
            ("GET", "/api/status") => ok(json!({
                "active_sessions": 0,
                "config_path": self.data_dir.join("config.json").display().to_string(),
                "config_version": 1,
                "latest_config_version": 1,
                "env_path": "",
                "gateway_exit_reason": null,
                "gateway_health_url": null,
                "gateway_pid": null,
                "gateway_platforms": {},
                "gateway_running": false,
                "gateway_state": null,
                "gateway_updated_at": null,
                "hermes_home": self.data_dir.display().to_string(),
                "release_date": "",
                "version": VERSION,
            })),
            ("GET", "/api/profiles") => ok(json!({ "profiles": [self.profile()] })),
            ("GET", "/api/profiles/active") => {
                ok(json!({ "name": "default", "profile": self.profile() }))
            }
            ("GET", "/api/ui-preferences") => ok(self.ui_preferences.lock().ok()?.clone()),
            ("PATCH", "/api/ui-preferences") => {
                let patch: Value = serde_json::from_slice(body).unwrap_or_default();
                let mut preferences = self.ui_preferences.lock().ok()?;
                if let (Some(target), Some(changes)) =
                    (preferences.as_object_mut(), patch.as_object())
                {
                    for (key, value) in changes {
                        target.insert(key.clone(), value.clone());
                    }
                }
                let _ = std::fs::write(
                    self.data_dir.join("desktop-ui-preferences.json"),
                    preferences.to_string(),
                );
                ok(preferences.clone())
            }
            ("GET", "/api/config") => ok(self.config(query)?),
            ("PUT", "/api/config") => ok(self.save_config(body)?),
            ("GET", "/api/config/defaults") => ok(config_defaults()),
            // Where a new session starts when none is picked (what `session.create` uses too).
            ("GET", "/api/fs/default-cwd") => ok(json!({
                "cwd": dirs::home_dir().map(|home| home.display().to_string()),
                "branch": null,
            })),
            ("GET", "/api/model/info") => {
                let model = default_model(&self.data_dir);
                ok(json!({ "model": model.model, "provider": model.provider }))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_config_merges_over_the_defaults_and_keeps_the_model_out() {
        let dir =
            std::env::temp_dir().join(format!("hermes-desktop-rest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"provider":"ollama","model":"qwen"}"#,
        )
        .unwrap();
        let rest = Rest::new(dir.clone());
        let body = br#"{"config":{"display":{"show_reasoning":false},"model":{"default":"x"}}}"#;
        assert_eq!(
            rest.handle("PUT", "/api/config", None, body).unwrap().0,
            200
        );
        let (_, config) = rest.handle("GET", "/api/config", None, b"").unwrap();
        assert_eq!(config["display"]["show_reasoning"], false);
        // Untouched defaults stay, and the model is the hermes-rs settings'.
        assert_eq!(config["display"]["busy_input_mode"], "queue");
        assert_eq!(config["model"]["default"], "qwen");
        assert_eq!(rest.config_value("approvals.mode"), "manual");
        // Without defaults: only what was saved (plus the model).
        let (_, bare) = rest
            .handle("GET", "/api/config", Some("include_defaults=false"), b"")
            .unwrap();
        assert!(bare.get("approvals").is_none());
        // It survives a restart.
        let again = Rest::new(dir.clone());
        assert_eq!(again.config_value("display.show_reasoning"), false);
        let _ = std::fs::remove_dir_all(dir);
    }
}
