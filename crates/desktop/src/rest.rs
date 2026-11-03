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

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
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

pub struct Rest {
    pub data_dir: PathBuf,
    plugins_dir: PathBuf,
    /// The Desktop's interface preferences, kept as the frontend writes them.
    ui_preferences: std::sync::Mutex<Value>,
}

impl Rest {
    pub fn new(data_dir: PathBuf) -> Self {
        let ui_preferences = std::fs::read(data_dir.join("desktop-ui-preferences.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({}));
        Self {
            plugins_dir: desktop_plugins_dir(&data_dir),
            data_dir,
            ui_preferences: std::sync::Mutex::new(ui_preferences),
        }
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
            return Some((403, json!({ "detail": "outside the desktop plugins folder" })));
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
                Ok(text) => Some((200, json!({ "path": path.display().to_string(), "text": text }))),
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
            ("GET", "/api/profiles/active") => ok(json!({ "name": "default", "profile": self.profile() })),
            ("GET", "/api/ui-preferences") => ok(self.ui_preferences.lock().ok()?.clone()),
            ("PATCH", "/api/ui-preferences") => {
                let patch: Value = serde_json::from_slice(body).unwrap_or_default();
                let mut preferences = self.ui_preferences.lock().ok()?;
                if let (Some(target), Some(changes)) = (preferences.as_object_mut(), patch.as_object()) {
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
            ("GET", "/api/model/info") => {
                let model = default_model(&self.data_dir);
                ok(json!({ "model": model.model, "provider": model.provider }))
            }
            _ => None,
        }
    }
}
