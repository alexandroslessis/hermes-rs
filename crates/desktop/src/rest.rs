//! Hermes's REST routes (`hermes_cli/web_routers/*`) the frontend needs, answered from the
//! hermes-rs data folder. Shapes follow the frontend's types (`apps/desktop/src/types/hermes.ts`).
//! Anything not listed here gets Hermes's "No such API endpoint" 404.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The Hermes commit this build ports (`HERMES_COMMIT` at the repository root: the one place
/// it's set; the webapp build and `scripts/hermes-update-report.ps1` read it too).
pub const HERMES_COMMIT: &str = include_str!("../../../HERMES_COMMIT");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use hermes_core::config::{DefaultModel, default_model};

/// A query parameter, percent-decoded (`+` is a space).
pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| percent_decode(value))
    })
}

pub use hermes_core::files::percent_decode;

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

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Hermes's machine-level UI preferences store (`apps/desktop/electron/ui-preferences.ts`):
/// `{version, generation, revision, values}` in `ui-preferences.json`. A write merges per key
/// (null deletes); a `seed` write (a window's first contact) only adds missing keys.
const UI_PREFERENCES_FILE: &str = "ui-preferences.json";
const UI_PREFERENCE_PREFIXES: [&str; 3] =
    ["hermes.desktop.", "hermes-desktop-", "hermes-onboarding-"];

#[derive(Default)]
struct UiPreferences {
    exists: bool,
    generation: String,
    revision: u64,
    values: serde_json::Map<String, Value>,
}

impl UiPreferences {
    fn read(path: &Path) -> Self {
        let Some(doc) = read_json(path) else {
            return Self::default();
        };
        Self {
            exists: true,
            generation: doc["generation"].as_str().unwrap_or_default().to_string(),
            revision: doc["revision"].as_u64().unwrap_or(0),
            values: doc["values"]
                .as_object()
                .map(|values| {
                    values
                        .iter()
                        .filter(|(_, v)| v.is_string())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn snapshot(&self) -> Value {
        let mut snapshot =
            json!({"exists": self.exists, "revision": self.revision, "values": self.values});
        if self.exists {
            snapshot["generation"] = json!(self.generation);
        }
        snapshot
    }

    fn document(&self) -> String {
        let doc = json!({"version": 1, "generation": self.generation, "revision": self.revision, "values": self.values});
        serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
    }

    /// Applies a write; `Ok(false)` when nothing changed. Refused whole when a key or value
    /// isn't one Hermes accepts.
    fn write(&mut self, write: &Value) -> Result<bool, String> {
        let changes = write["values"]
            .as_object()
            .ok_or("ui preferences write needs a values object")?;
        for (key, value) in changes {
            if !UI_PREFERENCE_PREFIXES.iter().any(|p| key.starts_with(p)) || key.len() > 256 {
                return Err(format!("unsupported preference key: {key}"));
            }
            if !(value.is_null() || value.is_string()) {
                return Err(format!("unsupported value for {key}"));
            }
        }
        let seed = write["seed"] == true;
        let mut values = self.values.clone();
        for (key, value) in changes {
            if seed {
                if !value.is_null() && !values.contains_key(key) {
                    values.insert(key.clone(), value.clone());
                }
            } else if value.is_null() {
                values.remove(key);
            } else {
                values.insert(key.clone(), value.clone());
            }
        }
        if values == self.values && (self.exists || values.is_empty()) {
            return Ok(false);
        }
        self.values = values;
        self.revision += 1;
        if self.generation.is_empty() {
            self.generation = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
        }
        self.exists = true;
        Ok(true)
    }
}

/// Settings' tab order (Hermes's `_CATEGORY_ORDER`); other categories follow alphabetically.
const CATEGORY_ORDER: [&str; 15] = [
    "general",
    "agent",
    "terminal",
    "display",
    "delegation",
    "memory",
    "compression",
    "security",
    "browser",
    "voice",
    "tts",
    "stt",
    "logging",
    "discord",
    "auxiliary",
];

/// Hermes's `/api/config/schema` for the keys this backend keeps: each default as a field
/// (`_build_schema_from_config`: type from the value, category from the first path part),
/// with Hermes's select options where it has them.
fn config_schema() -> serde_json::Map<String, Value> {
    fn walk(prefix: &str, value: &Value, out: &mut serde_json::Map<String, Value>) {
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                let full = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                walk(&full, value, out);
            }
            return;
        }
        let kind = match value {
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::Array(_) => "list",
            _ => "string",
        };
        let description = prefix
            .replace('.', " → ")
            .replace('_', " ")
            .split(' ')
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| {
                        first
                            .to_uppercase()
                            .chain(chars.flat_map(char::to_lowercase))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<String>>()
            .join(" ");
        let category = prefix
            .split('.')
            .next()
            .filter(|_| prefix.contains('.'))
            .unwrap_or("general");
        let mut field = json!({"type": kind, "description": description, "category": category});
        let select = |description: &str, options: &[&str]| json!({"type": "select", "description": description, "options": options});
        let overrides = match prefix {
            "display.busy_input_mode" => Some(select(
                "Input behavior while agent is running",
                &["interrupt", "queue", "steer"],
            )),
            "budget.profile" => Some(select(
                "How the agent spends model requests and tokens. Requests: the fewest requests per task (whole files per read, bigger tool results, tighter retry caps, fewer subagents, late compression). Balanced: how it worked before profiles. Tokens: the smallest context (smaller tool results, early compression, skill names only). Takes effect on the next message; wording in the agent's instructions, and the skill list, change for new chats.",
                &["requests", "balanced", "tokens"],
            )),
            "budget.review_mode" => Some(select(
                "When the agent reviews a session to learn from it (memory, skills). Empty: the profile's choice. idle_once: one request when the chat has been idle 10 minutes or you switch away, only after corrections or other signals (the Requests profile's). nudges: the tool-using review every 10 turns, and after corrections (Balanced and Tokens). agentic: the tool-using review every 10 turns only, as before (kept for comparison).",
                &["", "idle_once", "nudges", "agentic"],
            )),
            "fallback_providers" => Some(json!({"description": "Providers to use, in order, when the main one cannot answer a request: its daily quota is used up, it is down, or it does not have the model. Each one you add receives your conversation when that happens, and you are told when it does. Pick providers you have set up under Providers; a wrong key on the main provider never moves to a fallback."})),
            "approvals.mode" => Some(select(
                "Dangerous command approval mode",
                &["manual", "smart", "off"],
            )),
            "agent.reasoning_effort" => Some(select(
                "Reasoning effort for thinking models",
                &["", "none", "minimal", "low", "medium", "high", "xhigh"],
            )),
            "display.tool_progress" => Some(select(
                "Tool progress display",
                &["off", "new", "all", "verbose"],
            )),
            "memory.memory_enabled" => Some(
                json!({"description": "Keep the agent's notes (MEMORY.md) and give them to new sessions"}),
            ),
            "memory.user_profile_enabled" => Some(
                json!({"description": "Keep what the agent learns about you (USER.md) and give it to new sessions"}),
            ),
            "memory.memory_char_limit" => {
                Some(json!({"description": "Largest size of MEMORY.md, in characters"}))
            }
            "memory.user_char_limit" => {
                Some(json!({"description": "Largest size of USER.md, in characters"}))
            }
            "memory.project_char_limit" => Some(json!({"description": "Largest size of one project's memory (entries the agent keeps for a git repository, given to chats in that repository only), in characters; 200 to 20000"})),
            "memory.write_approval" => {
                Some(json!({"description": "Ask before the agent saves to memory"}))
            }
            "curator.auto_apply" => Some(json!({"description": "Apply the skills curator's proposals (merging near-duplicate skills, repairing skills that often go wrong) without asking, for skills the background review created; a change to a skill you or a chat wrote still waits for you. Off: every proposal waits in Capabilities, Skills, Pending for your approval. Applies to the Requests profile's curator."})),
            _ => None,
        };
        if let (Some(field), Some(overrides)) = (field.as_object_mut(), overrides) {
            field.extend(overrides.as_object().cloned().unwrap_or_default());
        }
        out.insert(prefix.to_string(), field);
    }
    let mut out = serde_json::Map::new();
    walk("", &hermes_core::config::defaults(), &mut out);
    out
}

pub struct Rest {
    pub data_dir: PathBuf,
    plugins_dir: PathBuf,
    /// The Desktop's interface preferences (theme, layout), in Hermes's store format.
    ui_preferences: std::sync::Mutex<UiPreferences>,
    /// Hermes's config record as the Settings pages write it (`desktop-config.json`; the core
    /// owns it, `hermes_core::config`); the model and provider come from the hermes-rs settings
    /// instead.
    config: std::sync::Arc<hermes_core::config::ConfigRecord>,
}

impl Rest {
    /// A page backend with a config record of its own (tests).
    pub fn new(data_dir: PathBuf) -> Self {
        let config = std::sync::Arc::new(hermes_core::config::ConfigRecord::load(&data_dir));
        Self::with_config(data_dir, config)
    }

    /// A page backend over the core's config record (`Core::config`).
    pub fn with_config(
        data_dir: PathBuf,
        config: std::sync::Arc<hermes_core::config::ConfigRecord>,
    ) -> Self {
        let mut ui_preferences = UiPreferences::read(&data_dir.join(UI_PREFERENCES_FILE));
        // The sidebar opens on the Projects view (a seed: a choice made in its menu stays).
        if let Ok(true) = ui_preferences.write(&json!({
            "seed": true,
            "values": {"hermes.desktop.agentsGroupedByWorkspace": "true"},
        })) {
            let _ = std::fs::write(
                data_dir.join(UI_PREFERENCES_FILE),
                ui_preferences.document(),
            );
        }
        Self {
            plugins_dir: desktop_plugins_dir(&data_dir),
            data_dir,
            ui_preferences: std::sync::Mutex::new(ui_preferences),
            config,
        }
    }

    /// The config record (the core's), for the calls that take it whole.
    pub fn config_record(&self) -> &hermes_core::config::ConfigRecord {
        &self.config
    }

    /// `GET /api/config`: the stored record over the defaults (unless `include_defaults=false`),
    /// with the model from the hermes-rs settings.
    fn config(&self, query: Option<&str>) -> Option<Value> {
        let with_defaults = query_param(query, "include_defaults").as_deref() != Some("false");
        self.config.merged(with_defaults)
    }

    fn save_config(&self, body: &[u8]) -> Option<Value> {
        let body: Value = serde_json::from_slice(body).ok()?;
        let changes = body.get("config").cloned().unwrap_or(body);
        self.config.save(changes).then(|| json!({"ok": true}))
    }

    /// One dotted key of the config record (`display.in_app_tips`), as `config.set` writes it.
    pub fn set_config_value(&self, key: &str, value: Value) {
        self.config.set_value(key, value);
    }

    /// A string setting from the config record (`display.busy_input_mode`), defaults included.
    pub fn config_value(&self, key: &str) -> Value {
        self.config.value(key)
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
                "commit": &HERMES_COMMIT.trim()[..8],
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
            ("GET", "/api/ui-preferences") => ok(self.ui_preferences.lock().ok()?.snapshot()),
            ("PATCH", "/api/ui-preferences") => {
                let write: Value = serde_json::from_slice(body).unwrap_or_default();
                let mut preferences = self.ui_preferences.lock().ok()?;
                match preferences.write(&write) {
                    Ok(true) => {
                        let _ = std::fs::write(
                            self.data_dir.join(UI_PREFERENCES_FILE),
                            preferences.document(),
                        );
                        ok(preferences.snapshot())
                    }
                    Ok(false) => ok(preferences.snapshot()),
                    Err(detail) => Some((400, json!({ "detail": detail }))),
                }
            }
            ("GET", "/api/config") => ok(self.config(query)?),
            ("PUT", "/api/config") => ok(self.save_config(body)?),
            ("GET", "/api/config/defaults") => ok(hermes_core::config::defaults()),
            ("GET", "/api/config/schema") => ok(json!({
                "fields": config_schema(),
                "category_order": CATEGORY_ORDER,
            })),
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
        // Removed when the test ends, even when it fails (docs/PITFALLS.md).
        let tmp = hermes_testdir::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
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
        assert_eq!(config["display"]["busy_input_mode"], "steer");
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
    }

    #[test]
    fn ui_preferences_merge_seed_and_survive_a_restart() {
        // Removed when the test ends, even when it fails (docs/PITFALLS.md).
        let tmp = hermes_testdir::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let rest = Rest::new(dir.clone());
        let (_, empty) = rest
            .handle("GET", "/api/ui-preferences", None, b"")
            .unwrap();
        // A fresh store holds only the seeded Projects view.
        assert_eq!(
            empty["values"],
            json!({"hermes.desktop.agentsGroupedByWorkspace": "true"})
        );
        let theme = br#"{"values":{"hermes.desktop.theme":"dark","hermes-desktop-mode-v1":"x","hermes.desktop.agentsGroupedByWorkspace":"false"}}"#;
        let (_, written) = rest
            .handle("PATCH", "/api/ui-preferences", None, theme)
            .unwrap();
        assert_eq!(written["revision"], 2);
        assert_eq!(written["values"]["hermes.desktop.theme"], "dark");
        // A seed never overwrites; null deletes; foreign keys are refused whole.
        let seed = br#"{"seed":true,"values":{"hermes.desktop.theme":"light"}}"#;
        rest.handle("PATCH", "/api/ui-preferences", None, seed)
            .unwrap();
        let delete = br#"{"values":{"hermes-desktop-mode-v1":null}}"#;
        rest.handle("PATCH", "/api/ui-preferences", None, delete)
            .unwrap();
        let bad = br#"{"values":{"other":"1"}}"#;
        assert_eq!(
            rest.handle("PATCH", "/api/ui-preferences", None, bad)
                .unwrap()
                .0,
            400
        );
        let (_, again) = Rest::new(dir.clone())
            .handle("GET", "/api/ui-preferences", None, b"")
            .unwrap();
        // The user's choice of view survives the restart's seed.
        assert_eq!(
            again["values"],
            json!({"hermes.desktop.theme": "dark", "hermes.desktop.agentsGroupedByWorkspace": "false"})
        );
        assert_eq!(again["generation"], written["generation"]);
    }

    #[test]
    fn the_budget_profile_defaults_to_requests_and_is_a_select() {
        // Removed when the test ends, even when it fails (docs/PITFALLS.md).
        let tmp = hermes_testdir::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let rest = Rest::new(dir);
        // Nothing saved: the default.
        assert_eq!(rest.config_value("budget.profile"), "requests");
        let schema = config_schema();
        assert_eq!(schema["budget.profile"]["type"], "select");
        assert_eq!(
            schema["budget.profile"]["options"],
            json!(["requests", "balanced", "tokens"])
        );
        // The Settings page saves with PUT: a good profile is kept, bad values are dropped.
        let put = |body: &str| rest.handle("PUT", "/api/config", None, body.as_bytes()).unwrap().0;
        assert_eq!(put(r#"{"config":{"budget":{"profile":"tokens","max_iterations":0}}}"#), 200);
        assert_eq!(rest.config_value("budget.profile"), "tokens");
        assert_eq!(rest.config_value("budget.max_iterations"), Value::Null);
        assert_eq!(put(r#"{"config":{"budget":{"profile":"fast"}}}"#), 200);
        assert_eq!(rest.config_value("budget.profile"), "tokens");
        // Not an object: it would replace the stored one whole, so it is dropped.
        assert_eq!(put(r#"{"config":{"budget":"balanced"}}"#), 200);
        assert_eq!(rest.config_value("budget.profile"), "tokens");
        // The background review (Phase 7): the profile's own by default, a select, validated.
        assert_eq!(rest.config_value("budget.review_mode"), "");
        assert_eq!(schema["budget.review_mode"]["type"], "select");
        assert_eq!(
            schema["budget.review_mode"]["options"],
            json!(["", "idle_once", "nudges", "agentic"])
        );
        assert_eq!(put(r#"{"config":{"budget":{"review_mode":"agentic"}}}"#), 200);
        assert_eq!(rest.config_value("budget.review_mode"), "agentic");
        assert_eq!(put(r#"{"config":{"budget":{"review_mode":"never"}}}"#), 200);
        assert_eq!(rest.config_value("budget.review_mode"), "agentic");
    }

    #[test]
    fn the_schema_describes_every_default() {
        let schema = config_schema();
        assert_eq!(schema["display.busy_input_mode"]["type"], "select");
        assert_eq!(schema["display.show_reasoning"]["type"], "boolean");
        assert_eq!(schema["display.show_reasoning"]["category"], "display");
        assert_eq!(
            schema["display.show_reasoning"]["description"],
            "Display → Show Reasoning"
        );
    }
}
