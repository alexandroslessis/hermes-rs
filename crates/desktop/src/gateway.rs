//! Hermes's `tui_gateway` protocol over the hermes-rs runtime: newline-delimited JSON-RPC 2.0
//! on a WebSocket, `gateway.ready` right after connecting, agent progress as `event`
//! notifications (`tui_gateway/ws.py`, `tui_gateway/server.py`). Result shapes follow the
//! contract the frontend is generated from (`apps/shared/src/gateway-contract.openrpc.json`).
//! A method this backend doesn't implement answers JSON-RPC's -32601, which the frontend treats
//! as "not supported here" and hides.
//!
//! Hermes: `tui_gateway/server.py`, `tui_gateway/methods_prompt.py`, `tui_gateway/methods_session.py`; what the page expects: `apps/desktop/src/api/canonical-protocol.ts`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use hermes_agent::events::AgentEvent;
use hermes_runtime::{Runtime, RuntimeEvent, TurnOptions, TurnStatus};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::commands;
use crate::hub::{Hub, Outbox};
use crate::providers::Settings;
use crate::transcript;

/// JSON-RPC's "method not found".
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_ERROR: i64 = -32000;

/// A session whose turn runs refuses edits, restores and moves (the frozen policy would change
/// under it). The page reads the typed reason off the error's `data` (`canonical-protocol.ts`
/// `failure()`, the move's "busy" notice) and matches the prose otherwise (`isSessionBusyError`).
const BUSY: &str = "session busy";

type RpcResult = Result<Value, (i64, String)>;

/// Hermes fences turn control and prompt answers by an execution generation. This backend runs
/// one agent per session in-process, so a single generation serves every turn.
pub const EXECUTION_GENERATION: u64 = 0;

/// The window assumed for a model whose provider doesn't list one (Hermes's
/// `DEFAULT_FALLBACK_CONTEXT`).
const FALLBACK_CONTEXT: usize = 256_000;

/// Each session's generated tokens and generation seconds (the status bar's tokens per second),
/// in the data folder.
const GENERATION_FILE: &str = "generation-stats.json";

/// Hermes's compaction status lines (`agent/conversation_compression.py` `COMPACTION_STATUS`,
/// `COMPACTION_DONE_STATUS`).
const COMPACTING_TEXT: &str = "🗜️ Compacting context — summarizing earlier conversation so I can continue...";
const COMPACTED_TEXT: &str = "✓ Context compaction complete — continuing turn...";

/// What a provider's listing (or the bundled catalog) says about a model.
#[derive(Debug, Clone, Copy, Default)]
struct ModelFacts {
    context_length: Option<usize>,
    /// It accepts images.
    vision: Option<bool>,
}

pub struct Gateway {
    pub runtime: Arc<Runtime>,
    pub rest: crate::rest::Rest,
    /// Background actions the page follows by name (the curator's run).
    actions: Arc<crate::maintenance::Actions>,
    data_dir: PathBuf,
    /// Changes when the process restarts, so reconnecting clients reset their replay state.
    replay_epoch: String,
    /// Event routes and open questions, shared with the runtime's bridge.
    hub: Arc<Hub>,
    /// Session id → its edit revision (Hermes's CAS value).
    revisions: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Session id → the reasoning effort picked in the composer.
    efforts: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Session id → prompts sent while its turn ran (`prompt.submit` params), run in order.
    queued: std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<Value>>>,
    /// Admission id → the started turn waiting for its `prompt.submit` reply to go out first.
    starts: std::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// Session id → the provider and model picked while its turn ran, applied when it ends.
    pending_routes: std::sync::Mutex<std::collections::HashMap<String, (String, String)>>,
    /// Sessions with a turn in flight, counting the queued prompts that follow it.
    live_turns: std::sync::Mutex<std::collections::HashSet<String>>,
    /// `provider\nmodel` → its context window, as the provider lists it (asked once).
    context_lengths: std::sync::Mutex<std::collections::HashMap<String, ModelFacts>>,
    /// Session id → output tokens generated and the seconds the model calls took (kept in
    /// `GENERATION_FILE`; the status bar's tokens per second).
    generation: std::sync::Mutex<std::collections::HashMap<String, (u64, f64)>>,
    /// Session id → images attached for its next prompt (`image.attach`, `image.attach_bytes`).
    pending_images: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
    /// File ticket → the file it opens (download links and media elements).
    file_tickets: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
    /// Branch → the session it was branched from (`branch.rs`).
    branches: crate::branch::Links,
    /// Session id → the git identity of its workspace (`workspace.rs`), kept beside the store.
    workspaces: std::sync::Arc<crate::workspace::Workspaces>,
    /// A branch request's idempotency key → the branch it made (a retried request gets it again).
    branch_keys: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Archived sessions and the auto-archive sweep (`archive.rs`).
    archive: crate::archive::Archive,
}

impl Gateway {
    pub fn new(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf) -> Self {
        let data_dir_for_stats = data_dir.clone();
        let gateway = Self {
            runtime,
            rest: crate::rest::Rest::new(data_dir.clone()),
            actions: Default::default(),
            data_dir,
            replay_epoch: uuid::Uuid::new_v4().to_string(),
            hub,
            revisions: Default::default(),
            efforts: Default::default(),
            queued: Default::default(),
            starts: Default::default(),
            pending_routes: Default::default(),
            live_turns: Default::default(),
            context_lengths: Default::default(),
            pending_images: Default::default(),
            file_tickets: Default::default(),
            branches: crate::branch::Links::load(&data_dir_for_stats),
            workspaces: std::sync::Arc::new(crate::workspace::Workspaces::load(&data_dir_for_stats)),
            archive: crate::archive::Archive::load(&data_dir_for_stats),
            branch_keys: Default::default(),
            generation: std::sync::Mutex::new(
                std::fs::read(data_dir_for_stats.join(GENERATION_FILE))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default(),
            ),
        };
        // The runtime keeps the approval policy in memory: Settings' choice applies from the start.
        gateway.apply_settings();
        crate::maintenance::schedule_curator(gateway.runtime.clone(), gateway.actions.clone());
        // So are the skills switched off in Capabilities ▸ Skills.
        crate::skills::apply_config(&gateway.runtime.skills(), &gateway.rest);
        gateway
    }

    /// Background work that needs the shared gateway: the hourly auto-archive check (it sweeps at
    /// most once a day, and only when Settings ▸ Sessions turned it on).
    pub fn schedule(this: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(this);
        handle.spawn(async move {
            let mut hourly = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                hourly.tick().await;
                let Some(this) = weak.upgrade() else { break };
                let _ = tokio::task::spawn_blocking(move || this.auto_archive()).await;
            }
        });
    }

    /// Settings ▸ Sessions' auto-archive (`archive.rs`): when it's on, sessions idle for
    /// `sessions.auto_archive_days` go to the archive.
    fn auto_archive(&self) {
        let sessions = self.rest.config_value("sessions");
        if sessions["auto_archive"] != true {
            return;
        }
        let days = sessions["auto_archive_days"]
            .as_f64()
            .filter(|d| *d >= 1.0)
            .unwrap_or(crate::archive::DEFAULT_IDLE_DAYS);
        let all = self.runtime.store().list_sessions(100_000).unwrap_or_default();
        let candidates: Vec<crate::archive::Candidate<'_>> = all
            .iter()
            .map(|s| crate::archive::Candidate {
                id: &s.id,
                last_active: s.updated_at,
                pinned: s.pinned,
                running: self.runtime.is_running(&s.id),
            })
            .collect();
        let swept = self.archive.sweep(&candidates, days, chrono::Utc::now().timestamp_millis());
        if !swept.is_empty() {
            tracing::info!("auto-archive: archived {} session(s) idle for {days} days or more", swept.len());
        }
    }

    /// The Settings the runtime keeps in memory, from the stored config record: at startup and
    /// after every change.
    fn apply_settings(&self) {
        self.apply_approvals_mode(&self.rest.config_value("approvals.mode"));
        // File checkpoints (`/rollback`), before the turn's first write.
        self.runtime
            .checkpoints()
            .set_enabled(self.rest.config_value("checkpoints.enabled") != false);
        // Settings ▸ Memory: which files new sessions keep and get, their sizes, approvals.
        let memory = self.rest.config_value("memory");
        let defaults = hermes_memory::MemoryConfig::default();
        let flag = |key: &str, default: bool| memory[key].as_bool().unwrap_or(default);
        let size = |key: &str, default: usize| {
            memory[key].as_u64().filter(|n| *n > 0).map_or(default, |n| n as usize)
        };
        self.runtime.memory().set_config(hermes_memory::MemoryConfig {
            memory_enabled: flag("memory_enabled", defaults.memory_enabled),
            user_profile_enabled: flag("user_profile_enabled", defaults.user_profile_enabled),
            memory_char_limit: size("memory_char_limit", defaults.memory_char_limit),
            user_char_limit: size("user_char_limit", defaults.user_char_limit),
            write_approval: flag("write_approval", defaults.write_approval),
        });
        // The tools switched off in Capabilities ▸ Tools.
        let off = crate::toolsets::disabled_toolsets(&self.rest.config_value(crate::toolsets::CONFIG_KEY));
        self.runtime.set_disabled_tools(crate::toolsets::disabled_tools(&off));
        // Auto-archive just switched on sweeps now (it waits a day between sweeps itself).
        self.auto_archive();
    }

    /// Hermes's `approvals.mode` on the runtime's policy: manual asks, smart has a reviewer model
    /// judge first, off runs flagged commands (hardline ones never run).
    fn apply_approvals_mode(&self, mode: &Value) {
        let approvals = self.runtime.approvals();
        let mode = mode.as_str().unwrap_or("manual");
        approvals.set_ask(mode != "off");
        approvals.set_smart(mode == "smart");
        // The status bar and Settings each change it: open pages hear the new mode at once.
        for id in self.hub.followed() {
            self.emit(&id, "session.info", self.session_info(&id));
        }
    }

    pub async fn connection(self: Arc<Self>, socket: WebSocket) {
        let (mut sink, mut stream) = socket.split();
        let (outbox, mut outgoing) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if sink.send(WsMessage::Text(frame.into())).await.is_err() {
                    break;
                }
            }
        });
        send(
            &outbox,
            json!({
                "jsonrpc": "2.0",
                "method": "event",
                "params": {"type": "gateway.ready", "payload": {
                    "skin": {},
                    "change_events": false,
                    "heartbeat": true,
                    "replay_epoch": self.replay_epoch,
                }},
            }),
        );
        while let Some(Ok(message)) = stream.next().await {
            let WsMessage::Text(text) = message else {
                continue;
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(frame) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let this = self.clone();
                let outbox = outbox.clone();
                // Each request on its own task: a slow one never holds up the heartbeat.
                tokio::spawn(async move { this.handle(frame, outbox).await });
            }
        }
        writer.abort();
    }

    async fn handle(self: Arc<Self>, frame: Value, outbox: Outbox) {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            // A response to a server→client request (approvals, clarify): not wired yet.
            return;
        };
        let id = frame.get("id").cloned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        tracing::debug!(target: "hermes_desktop::wire", "← {method} {}", params["session_id"]);
        if matches!(
            method,
            "session.create" | "session.resume" | "session.close"
        ) {
            tracing::info!("{method} {}", params["session_id"]);
        }
        let began = std::time::Instant::now();
        let result = self.clone().dispatch(method, &params, &outbox).await;
        if began.elapsed() > std::time::Duration::from_secs(5) {
            tracing::warn!("RPC {method} took {:.1?}", began.elapsed());
        }
        let Some(id) = id else {
            return;
        };
        let reply = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                if code == METHOD_NOT_FOUND {
                    crate::server::log_missing(&format!("RPC {method}"), &params.to_string());
                } else {
                    tracing::warn!("RPC {method} failed: {message}");
                }
                let mut error = json!({"code": code, "message": message});
                if message == BUSY {
                    // The page's canonical protocol and the move's notice read the refusal's
                    // reason (`data.reason`), not just the prose.
                    error["data"] = json!({"reason": "session_busy"});
                }
                json!({"jsonrpc": "2.0", "id": id, "error": error})
            }
        };
        send(&outbox, reply);
        // A started turn's events follow its admission receipt.
        if method == "prompt.submit" {
            if let Some(admission) = admission_id(&params) {
                let start = self
                    .starts
                    .lock()
                    .ok()
                    .and_then(|mut s| s.remove(&admission));
                if let Some(start) = start {
                    let _ = start.send(());
                }
            }
        }
    }

    async fn dispatch(self: Arc<Self>, method: &str, params: &Value, outbox: &Outbox) -> RpcResult {
        let settings = Settings::load(&self.data_dir);
        match method {
            "gateway.ping" => Ok(json!({"ok": true})),
            "client.capabilities" => Ok(json!({"server_requests": []})),
            "setup.status" => {
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
            "onboarding.state" => Ok(json!({"run": false, "eligible": false})),
            "profiles.list" => Ok(json!({"profiles": [{
                "name": "default",
                "path": self.data_dir.display().to_string(),
                "is_default": true,
                "model": settings.model,
                "provider": settings.provider,
            }]})),
            "projects.tree" | "projects.project_sessions" => {
                let projects = self.runtime.store().list_projects().unwrap_or_default();
                let preview = params["preview_limit"].as_u64().unwrap_or(3) as usize;
                let rows = self.session_rows();
                // The tree lists counts; one project's sessions come hydrated.
                let single = params["project_id"].as_str().map(str::to_string);
                let mut tree = tokio::task::spawn_blocking(move || {
                    crate::projects::tree(&projects, &rows, preview, single.is_some())
                })
                .await
                .unwrap_or_default();
                match params["project_id"].as_str() {
                    Some(id) => {
                        let project = tree["projects"]
                            .as_array()
                            .and_then(|all| all.iter().find(|p| p["id"] == id).cloned());
                        Ok(json!({"project": project}))
                    }
                    None => {
                        tree["active_id"] = self.rest.config_value("projects.active_id");
                        Ok(tree)
                    }
                }
            }
            "projects.list" => Ok(self.project_list()),
            "projects.create" => {
                let name = params["name"]
                    .as_str()
                    .unwrap_or("Project")
                    .trim()
                    .to_string();
                let mut folders: Vec<String> = params["folders"]
                    .as_array()
                    .map(|f| {
                        f.iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(primary) = params["primary_path"].as_str().filter(|p| !p.is_empty()) {
                    folders.retain(|f| f != primary);
                    folders.insert(0, primary.to_string());
                }
                let project = self
                    .runtime
                    .store()
                    .create_project(&name, &folders)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                if params["use"] == true {
                    self.rest
                        .set_config_value("projects.active_id", json!(project.id));
                }
                Ok(json!({"project": crate::projects::record(&project)}))
            }
            "projects.update" | "projects.add_folder" => {
                let id = params["id"].as_str().unwrap_or_default();
                let mut project = self
                    .runtime
                    .store()
                    .list_projects()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|p| p.id == id)
                    .ok_or((SERVER_ERROR, format!("No project {id}")))?;
                if let Some(name) = params["name"].as_str().filter(|n| !n.trim().is_empty()) {
                    project.name = name.trim().to_string();
                }
                if let Some(path) = params["path"].as_str().filter(|p| !p.is_empty()) {
                    project.folders.retain(|f| f != path);
                    if params["is_primary"] == true {
                        project.folders.insert(0, path.to_string());
                    } else {
                        project.folders.push(path.to_string());
                    }
                }
                self.runtime
                    .store()
                    .update_project(&project.id, &project.name, &project.folders)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                Ok(json!({"project": crate::projects::record(&project)}))
            }
            "projects.delete" => {
                let id = params["id"].as_str().unwrap_or_default();
                self.runtime
                    .store()
                    .delete_project(id)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                if self.rest.config_value("projects.active_id") == id {
                    self.rest
                        .set_config_value("projects.active_id", Value::Null);
                }
                Ok(self.project_list())
            }
            "projects.set_active" => {
                let id = params["id"].clone();
                self.rest.set_config_value("projects.active_id", id.clone());
                Ok(json!({"active_id": id}))
            }
            // No scanning for repositories yet: the tree shows the ones sessions ran in.
            "projects.discover_repos" => Ok(json!({"repos": []})),
            "projects.record_repos" => Ok(json!({"repos": [], "accepted": true})),
            "session.active_list" => Ok(json!({"sessions": self.active_sessions()})),
            "model.options" => Ok(self
                .model_options(&settings, params["session_id"].as_str())
                .await),
            "config.get" => Ok(json!({
                "value": params["key"]
                    .as_str()
                    .map(|key| self.rest.config_value(key))
                    .filter(|value| !value.is_null())
                    .unwrap_or(json!("")),
                "model": settings.model,
                "provider": settings.provider,
            })),
            "config.set" if params["key"] == "model" => self.switch_model(params, &settings),
            // A session's effort; any other key is a Settings value, kept in the config record.
            "config.set" => {
                let key = params["key"].as_str().unwrap_or_default();
                let value = params["value"].clone();
                match (key, params["session_id"].as_str()) {
                    ("reasoning", Some(id)) => {
                        let effort = value.as_str().unwrap_or_default().to_lowercase();
                        if let Ok(mut efforts) = self.efforts.lock() {
                            efforts.insert(id.to_string(), effort.clone());
                        }
                        self.emit(id, "session.info", self.session_info(id));
                        Ok(json!({"key": key, "value": effort, "ok": true}))
                    }
                    ("", _) => Err((INVALID_PARAMS, "key is required".to_string())),
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
            // Polled on start and on every focus; features this backend doesn't have answer
            // "off" so the page stops asking and shows nothing for them.
            "pet.info" => Ok(json!({"enabled": false})),
            "setup.runtime_check" => {
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
            "shared_metrics.status" => {
                Ok(json!({"enabled": false, "send": false, "decided": true}))
            }
            "free_tier.status" => Ok(json!({
                "has_guest": false,
                "enabled": false,
                "available": false,
                "notice_pending": false,
                "model": "",
                "label": "",
            })),
            "wake.status" => Ok(json!({
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
            })),
            "commands.catalog" => Ok(commands::catalog(&self.runtime.skills())),
            "complete.slash" => Ok(commands::complete(
                &self.runtime.skills(),
                params["text"].as_str().unwrap_or_default(),
            )),
            // Hermes's wording for "not a command here": the page then tries `command.dispatch`,
            // and reports the slash.exec error when that finds nothing either.
            // The page polls each session's goal; there are no goals here.
            "slash.exec"
                if params["command"]
                    .as_str()
                    .is_some_and(|c| c.trim_start_matches('/').starts_with("goal")) =>
            {
                Ok(json!({"type": "exec", "output": ""}))
            }
            "slash.exec" => {
                let command = params["command"].as_str().unwrap_or_default();
                let (name, arg) = command
                    .trim()
                    .trim_start_matches('/')
                    .split_once(char::is_whitespace)
                    .unwrap_or((command.trim().trim_start_matches('/'), ""));
                if let Some(answer) = self.coding_command(params, name, arg) {
                    return answer;
                }
                if let Some(answer) = self.builtin_command(params, command, &settings).await {
                    return answer;
                }
                commands::exec(&self.runtime, command).ok_or((
                    SERVER_ERROR,
                    "not a quick/plugin/bundle/skill command".to_string(),
                ))
            }
            "session.status" => {
                let id = session_id(params)?;
                let output = self.status_text(&id).await?;
                Ok(json!({"output": output}))
            }
            "command.dispatch" if self
                .coding_command(params, params["name"].as_str().unwrap_or_default(), "")
                .is_some() =>
            {
                let name = params["name"].as_str().unwrap_or_default();
                let arg = params["arg"].as_str().unwrap_or_default();
                self.coding_command(params, name, arg).expect("a coding command")
            }
            "command.dispatch" => commands::dispatch(
                &self.runtime,
                params["name"].as_str().unwrap_or_default(),
                params["arg"].as_str().unwrap_or_default(),
            )
            .ok_or((
                SERVER_ERROR,
                "not a quick/plugin/bundle/skill command".to_string(),
            )),
            "session.title" => {
                let id = session_id(params)?;
                if let Some(title) = params["title"].as_str() {
                    self.rename(&id, title)?;
                }
                let title = self
                    .runtime
                    .store()
                    .session(&id)
                    .ok()
                    .flatten()
                    .and_then(|record| record.title)
                    .unwrap_or_default();
                Ok(json!({"title": title, "session_key": id}))
            }
            "session.mutate" => self.mutate(params, &settings).await,
            "session.rewind" => self.rewind_turn(params),
            "rollback.list" | "rollback.diff" | "rollback.restore" => self.rollback(method, params),
            "session.export" => self.export_session(params),
            // The Import screen: sessions from the original Hermes (`hermes_import.rs`).
            "session.foreign.list" => {
                let source = params["source"].as_str().map(str::to_string);
                let offset = params["offset"].as_u64().unwrap_or(0) as usize;
                let data_dir = self.data_dir.clone();
                tokio::task::spawn_blocking(move || {
                    crate::hermes_import::list(&data_dir, source.as_deref(), offset)
                })
                .await
                .map_err(|e| (SERVER_ERROR, e.to_string()))
            }
            "session.foreign.preview" => {
                let id = params["id"].as_str().unwrap_or_default().to_string();
                let data_dir = self.data_dir.clone();
                tokio::task::spawn_blocking(move || crate::hermes_import::preview(&data_dir, &id))
                    .await
                    .map_err(|e| (SERVER_ERROR, e.to_string()))?
                    .map_err(|e| (INVALID_PARAMS, e))
            }
            "session.foreign.import" => {
                let id = params["id"].as_str().unwrap_or_default().to_string();
                let (provider, model) = self.import_route(&settings)?;
                let this = self.clone();
                tokio::task::spawn_blocking(move || {
                    let target = crate::hermes_import::Target {
                        runtime: &this.runtime,
                        provider,
                        model,
                        data_dir: &this.data_dir,
                    };
                    crate::hermes_import::import(&target, &id)
                })
                .await
                .map_err(|e| (SERVER_ERROR, e.to_string()))?
                .map_err(|e| (INVALID_PARAMS, e))
            }
            // `/branch` and the message menu's Branch (`branch.rs`).
            "session.branch" => self.branch(params, &settings, outbox, false, false),
            "session.branch_whole" => self.branch(params, &settings, outbox, true, true),
            "session.branch_stored" => self.branch(params, &settings, outbox, true, true),
            // `/btw`: a side question about the conversation, answered as `btw.complete`.
            "prompt.btw" => {
                let id = session_id(params)?;
                let question = params["text"].as_str().unwrap_or_default().trim().to_string();
                if question.is_empty() {
                    return Err((INVALID_PARAMS, "text required".to_string()));
                }
                let provider = self.session_provider(&id, &settings)?;
                let task_id = format!("btw_{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
                let this = self.clone();
                let task = task_id.clone();
                tokio::spawn(async move {
                    let text = match this.runtime.side_question(&id, &provider, &question).await {
                        Ok(answer) if !answer.is_empty() => answer,
                        Ok(_) => "error: the model gave no answer".to_string(),
                        Err(error) => format!("error: {error:#}"),
                    };
                    this.emit(&id, "btw.complete", json!({"task_id": task, "question": question, "text": text}));
                });
                Ok(json!({"task_id": task_id}))
            }
            // `/bg`: the prompt runs as a background subagent; its answer arrives as
            // `background.complete`, its progress as its subagent row's events.
            "prompt.background" => {
                let id = session_id(params)?;
                let text = params["text"].as_str().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    return Err((INVALID_PARAMS, "text required".to_string()));
                }
                let provider = self.session_provider(&id, &settings)?;
                let options = self.turn_options(&id);
                let mut task = self
                    .runtime
                    .background(&id, &provider, options, &text)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                let task_id = task.task_id.clone();
                let this = self.clone();
                tokio::spawn(async move {
                    while let Some(event) = task.events.recv().await {
                        if let AgentEvent::Subagent { kind, payload } = event {
                            this.emit(&id, kind, payload);
                        }
                    }
                    let text = task.done.await.unwrap_or_else(|e| format!("error: {e}"));
                    this.emit(&id, "background.complete", json!({"task_id": task.task_id, "text": text}));
                });
                Ok(json!({"task_id": task_id}))
            }
            // A file attached in the composer: copied into the session's folder, referenced as
            // `@file:` in the prompt (`attachments.rs`).
            "file.attach" => {
                let id = session_id(params)?;
                let record = self
                    .runtime
                    .store()
                    .session(&id)
                    .ok()
                    .flatten()
                    .ok_or((INVALID_PARAMS, format!("No session {id}.")))?;
                crate::attachments::attach_file(std::path::Path::new(&record.cwd), params)
                    .map_err(|e| (INVALID_PARAMS, e))
            }
            // Images held for the session's next prompt (the page's path when it doesn't upload
            // them with the prompt).
            "image.attach" | "image.attach_bytes" => {
                let id = session_id(params)?;
                let path = if method == "image.attach" {
                    let path = params["path"].as_str().unwrap_or_default().trim().to_string();
                    let file = std::path::Path::new(&path);
                    if !file.is_file() {
                        return Err((INVALID_PARAMS, format!("image not found: {path}")));
                    }
                    if !crate::attachments::is_image(file) {
                        return Err((INVALID_PARAMS, format!("unsupported image: {path}")));
                    }
                    path
                } else {
                    let (status, saved) =
                        crate::attachments::image_from_base64(&self.data_dir, params);
                    if status != 200 {
                        let detail = saved["detail"].as_str().unwrap_or("The image wasn't saved.");
                        return Err((INVALID_PARAMS, detail.to_string()));
                    }
                    saved["path"].as_str().unwrap_or_default().to_string()
                };
                let mut pending = self.pending_images.lock().unwrap_or_else(|e| e.into_inner());
                let images = pending.entry(id).or_default();
                images.push(path.clone());
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(json!({"attached": true, "path": path, "name": name, "count": images.len(),
                          "text": format!("[User attached image: {name}]")}))
            }
            "image.detach" => {
                let id = session_id(params)?;
                let path = params["path"].as_str().unwrap_or_default().trim();
                let mut pending = self.pending_images.lock().unwrap_or_else(|e| e.into_inner());
                let images = pending.entry(id).or_default();
                let before = images.len();
                images.retain(|p| p != path);
                Ok(json!({"detached": images.len() != before, "count": images.len()}))
            }
            // Activity over the last `days` (default 30): sessions touched and their messages.
            "insights.get" => {
                let days = params["days"].as_i64().filter(|d| *d > 0).unwrap_or(30);
                // (The store keeps milliseconds.)
                let since = chrono::Utc::now().timestamp_millis() - days * 86_400_000;
                let store = self.runtime.store();
                let counts = store.message_counts().unwrap_or_default();
                let recent: Vec<_> = store
                    .list_sessions(100_000)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| s.updated_at >= since)
                    .collect();
                let messages: usize = recent.iter().filter_map(|s| counts.get(&s.id)).sum();
                Ok(json!({"days": days, "sessions": recent.len(), "messages": messages}))
            }
            "session.compress" => self.compress_session(params, &settings).await,
            "session.delete" => {
                let id = session_id(params)?;
                self.runtime
                    .delete_session(&id)
                    .await
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                self.archive.forget(&id);
                Ok(json!({"deleted": id}))
            }
            // Nothing to release: a session lives in the store, not in a worker.
            "session.close" => Ok(json!({"closed": true})),
            "session.create" => self.create_session(params, &settings, outbox),
            "session.resume" | "session.history" => self.resume_session(params, outbox),
            "prompt.submit" => {
                // What the model takes (images, its window) is asked of the provider once, before
                // the turn starts.
                if let Ok(id) = session_id(params)
                    && let Some(record) = self.runtime.store().session(&id).ok().flatten()
                {
                    self.model_facts(&record.provider, &record.model).await;
                }
                self.submit(params, &settings, outbox, true)
            }
            // The page's answer to an approval card (`choice`) or a clarify question (`answer`),
            // named by the prompt id it was shown with.
            "approval.respond" | "clarify.respond" => {
                let prompt_id = params["prompt_id"]
                    .as_str()
                    .or(params["request_id"].as_str())
                    .ok_or((INVALID_PARAMS, "prompt_id is required".to_string()))?;
                let field = if method == "approval.respond" {
                    "choice"
                } else {
                    "answer"
                };
                let answer = params[field].as_str().unwrap_or_default().to_string();
                if self.hub.answer(prompt_id, answer) {
                    Ok(json!({"resolved": 1}))
                } else {
                    Err((
                        SERVER_ERROR,
                        format!(
                            "No pending {} request",
                            method.split('.').next().unwrap_or("prompt")
                        ),
                    ))
                }
            }
            "session.context_breakdown" => self.context_breakdown(&session_id(params)?).await,
            // The open approval cards, for a page that reconnected or switched back.
            "approval.pending" => {
                let id = session_id(params)?;
                let approvals: Vec<Value> = self
                    .hub
                    .open_prompts(&id)
                    .into_iter()
                    .filter(|prompt| prompt["kind"] == "approval")
                    .map(|prompt| {
                        json!({
                            "request_id": prompt["prompt_id"],
                            "command": prompt["command"],
                            "description": prompt["description"],
                            "choices": prompt["choices"],
                            "allow_permanent": true,
                            "allow_session": true,
                        })
                    })
                    .collect();
                Ok(json!({"approvals": approvals}))
            }
            // The session's background processes (the `process` tool's list).
            "process.list" => {
                let id = session_id(params)?;
                let processes: Vec<Value> = self
                    .runtime
                    .processes(&id)
                    .into_iter()
                    .map(|mut process| {
                        process["uptime_seconds"] = process["running_seconds"].clone();
                        process
                    })
                    .collect();
                Ok(json!({"processes": processes}))
            }
            "process.kill" => {
                let id = session_id(params)?;
                let process_id = params["process_id"].as_str().unwrap_or_default();
                let before = self
                    .runtime
                    .processes(&id)
                    .into_iter()
                    .find(|p| p["session_id"] == process_id);
                let Some(before) = before else {
                    return Ok(json!({"status": "not_found", "session_id": process_id}));
                };
                if before["status"] != "running" {
                    return Ok(
                        json!({"status": "already_exited", "session_id": process_id, "exit_code": before["exit_code"]}),
                    );
                }
                let after = self
                    .runtime
                    .kill_process(&id, process_id)
                    .await
                    .unwrap_or_default();
                Ok(json!({
                    "status": "killed",
                    "session_id": process_id,
                    "command": before["command"],
                    "exit_code": after["exit_code"],
                    "termination_source": "user",
                }))
            }
            "process.stop" => {
                let id = session_id(params)?;
                Ok(json!({"killed": self.runtime.stop_processes(&id).await}))
            }
            // The session's running subagents (`delegate_task` children): the page's rows follow
            // their `subagent.*` events and reconcile against this list.
            "subagent.list" => {
                let id = session_id(params)?;
                Ok(json!({"subagents": self.runtime.subagents().list(&id), "delegations": []}))
            }
            "subagent.tail" | "subagent.steer" | "subagent.interrupt" => {
                let id = session_id(params)?;
                let child = params["subagent_id"].as_str().unwrap_or_default().to_string();
                let subagents = self.runtime.subagents();
                Ok(match method {
                    "subagent.tail" => subagents.tail(&id, &child),
                    "subagent.interrupt" => {
                        json!({"found": subagents.interrupt(&id, &child), "subagent_id": child})
                    }
                    _ => {
                        let text = params["text"].as_str().unwrap_or_default().trim().to_string();
                        if text.is_empty() {
                            return Err((INVALID_PARAMS, "text is required".to_string()));
                        }
                        // (Hermes's `SteerStatus`: queued for the child's next tool results.)
                        let status = if subagents.steer(&id, &child, &text) { "queued" } else { "rejected" };
                        json!({"status": status, "subagent_id": child, "text": text})
                    }
                })
            }
            "delegation.status" => Ok(json!({
                "active": self.runtime.subagents().active(),
                "paused": self.runtime.subagents().paused(),
                "max_spawn_depth": 1,
                "max_concurrent_children": hermes_runtime::MAX_CONCURRENT_CHILDREN,
            })),
            "delegation.pause" => {
                let paused = params["paused"].as_bool().unwrap_or(true);
                self.runtime.subagents().set_paused(paused);
                Ok(json!({"paused": paused}))
            }
            "complete.path" => {
                let word = params["word"].as_str().unwrap_or_default().to_string();
                let root = params["cwd"]
                    .as_str()
                    .filter(|cwd| !cwd.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        let id = params["session_id"].as_str()?;
                        let record = self.runtime.store().session(id).ok().flatten()?;
                        Some(PathBuf::from(record.cwd))
                    })
                    .or_else(dirs::home_dir)
                    .unwrap_or_default();
                // A name search walks the project: off the async workers.
                let items =
                    tokio::task::spawn_blocking(move || crate::paths::complete(&word, &root))
                        .await
                        .unwrap_or_default();
                Ok(json!({"items": items}))
            }
            "session.steer" => {
                let id = session_id(params)?;
                let text = params["text"].as_str().unwrap_or_default();
                if self.runtime.steer(&id, text) {
                    Ok(json!({"status": "queued", "session_id": id, "text": text}))
                } else {
                    Ok(json!({"status": "rejected", "session_id": id, "text": text}))
                }
            }
            // The queue panel: delete a queued prompt, steer it into the running turn, or edit it
            // (the edit goes to the back, as a new admission).
            "prompt.cancel" => {
                let id = session_id(params)?;
                let admission = params["admission_id"].as_str().unwrap_or_default();
                let cancelled = self.dequeue(&id, admission).is_some();
                Ok(json!({"cancelled": cancelled, "admission_id": admission, "session_id": id}))
            }
            "prompt.steer_queued" => {
                let id = session_id(params)?;
                let admission = params["admission_id"].as_str().unwrap_or_default();
                if !self.runtime.is_running(&id) {
                    return Err((SERVER_ERROR, "session_idle".to_string()));
                }
                let queued = self
                    .dequeue(&id, admission)
                    .ok_or((SERVER_ERROR, "admission_started".to_string()))?;
                let text = queued["text"].as_str().unwrap_or_default();
                if !self.runtime.steer(&id, text) {
                    // The turn ended in between: it runs next after all.
                    self.enqueue(&id, queued.clone(), true);
                }
                Ok(json!({"status": "steered", "admission_id": admission, "session_id": id}))
            }
            "prompt.replace_queued" => {
                let id = session_id(params)?;
                let admission = params["admission_id"].as_str().unwrap_or_default();
                self.dequeue(&id, admission)
                    .ok_or((SERVER_ERROR, "admission_started".to_string()))?;
                let edited = json!({
                    "text": params["text"],
                    "submission_id": params["submission_id"],
                });
                let replacement = self.enqueue(&id, edited, false);
                Ok(json!({
                    "status": "queued",
                    "admission_id": replacement,
                    "replaced": admission,
                    "ref": {"session_id": id},
                }))
            }
            // Interrupt and send this instead: the text runs first once the turn has stopped.
            "session.redirect" => {
                let id = session_id(params)?;
                let text = params["text"].as_str().unwrap_or_default();
                if !self.runtime.is_running(&id) {
                    return Ok(json!({"status": "rejected", "session_id": id, "text": text}));
                }
                self.subscribe(&id, outbox);
                self.enqueue(&id, json!({"text": text, "shown": true}), true);
                self.runtime.cancel(&id);
                self.hub.cancel_session(&id);
                Ok(json!({"status": "redirected", "session_id": id, "text": text}))
            }
            "session.interrupt" => {
                let id = session_id(params)?;
                let running = self.runtime.is_running(&id);
                tracing::info!("stop requested in {id} (a turn was running: {running})");
                self.runtime.cancel(&id);
                self.hub.cancel_session(&id);
                Ok(json!({
                    "status": if running { "interrupted" } else { "not_interrupted" },
                    "interrupted": running,
                }))
            }
            _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
        }
    }

    fn active_sessions(&self) -> Vec<Value> {
        let Ok(sessions) = self.runtime.store().list_sessions(200) else {
            return Vec::new();
        };
        sessions
            .into_iter()
            .filter(|s| self.is_live(&s.id))
            .map(|s| {
                // The page holds a session busy only while it's listed as `working` (or
                // `waiting`, on the user: a card is open); any other status idles it on the next
                // poll, which blinked Stop and Steer every 1.5 s mid-turn.
                let status = if self.hub.open_prompts(&s.id).is_empty() {
                    "working"
                } else {
                    "waiting"
                };
                json!({
                    "id": s.id,
                    "session_key": s.id,
                    "current": false,
                    "title": s.title.unwrap_or_default(),
                    "preview": "",
                    "model": s.model,
                    "status": status,
                    "message_count": 0,
                    "started_at": s.created_at as f64 / 1000.,
                    "last_active": s.updated_at as f64 / 1000.,
                })
            })
            .collect()
    }

    /// Hermes's model picker: each provider and its models (listed live from the provider). For
    /// a session, its own model is the current one.
    async fn model_options(&self, settings: &Settings, session_id: Option<&str>) -> Value {
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
    fn switch_model(&self, params: &Value, settings: &Settings) -> RpcResult {
        let value = params["value"].as_str().unwrap_or_default();
        let mut words = value.split_whitespace();
        let model = words
            .next()
            .ok_or((INVALID_PARAMS, "a model is required".to_string()))?
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
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?;
        if settings.provider(&provider).is_none() {
            return Err((SERVER_ERROR, format!("Unknown provider {provider}")));
        }
        let record = params["session_id"]
            .as_str()
            .and_then(|id| self.runtime.store().session(id).ok().flatten());
        if let Some(record) = &record {
            let changes = record.model != model || record.provider != provider;
            if self.runtime.is_running(&record.id) {
                // Mid-reply: the pick applies from the next turn on (Hermes's deferred pick); it
                // shows as the session's model right away. Picking the running model again
                // withdraws an earlier pick.
                if let Ok(mut picks) = self.pending_routes.lock() {
                    if changes {
                        picks.insert(record.id.clone(), (provider.clone(), model.clone()));
                    } else {
                        picks.remove(&record.id);
                    }
                }
                self.emit(&record.id, "session.info", self.session_info(&record.id));
            } else if changes {
                self.route_session(&record.id, &provider, &model)?;
            }
        }
        if !session_only {
            crate::providers::save_default(&self.data_dir, &provider, &model)
                .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        }
        Ok(json!({"key": "model", "value": model, "provider": provider}))
    }

    /// Moves a session to `provider` / `model` (refused while a turn runs).
    fn route_session(&self, id: &str, provider: &str, model: &str) -> Result<(), (i64, String)> {
        let record = self.runtime.store().session(id).ok().flatten();
        let other_provider = record
            .as_ref()
            .is_some_and(|r| r.provider != provider)
            .then_some(provider);
        self.runtime
            .set_session_route(id, other_provider, model, None)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        self.emit(id, "session.info", self.session_info(id));
        Ok(())
    }

    /// Counts one model call's output toward the session's tokens per second.
    fn note_generation(&self, id: &str, output_tokens: u64, seconds: f64) {
        if output_tokens == 0 || seconds <= 0.0 {
            return;
        }
        if let Ok(mut generation) = self.generation.lock() {
            let entry = generation.entry(id.to_string()).or_insert((0, 0.0));
            entry.0 += output_tokens;
            entry.1 += seconds;
            // Kept across restarts (beside the store, whose schema the GPUI app shares).
            if let Ok(json) = serde_json::to_vec(&*generation) {
                let _ = std::fs::write(self.data_dir.join(GENERATION_FILE), json);
            }
        }
    }

    /// The status bar's usage (`session.usage` / `message.complete`'s `usage`): the session's token
    /// totals, how full the context is (the last call's input tokens against the model's window)
    /// and the average generation speed.
    fn usage_snapshot(&self, id: &str, last_input: Option<usize>) -> Value {
        let store = self.runtime.store();
        let totals = store.usage_totals(id).unwrap_or_default();
        let record = store.session(id).ok().flatten();
        let context_max = record
            .as_ref()
            .map(|r| {
                let key = format!("{}\n{}", r.provider, r.model);
                self.context_lengths
                    .lock()
                    .ok()
                    .and_then(|lengths| lengths.get(&key).and_then(|facts| facts.context_length))
                    .unwrap_or(FALLBACK_CONTEXT)
            })
            .unwrap_or(0);
        let used = last_input.unwrap_or(totals.last_input_tokens.max(0) as usize);
        let used = if context_max > 0 {
            used.min(context_max)
        } else {
            used
        };
        let avg_tps = self
            .generation
            .lock()
            .ok()
            .and_then(|g| g.get(id).copied())
            .filter(|(_, seconds)| *seconds > 0.0)
            .map(|(tokens, seconds)| tokens as f64 / seconds);
        let mut usage = json!({
            "model": record.as_ref().map(|r| r.model.clone()),
            "input": totals.input_tokens,
            "output": totals.output_tokens,
            "total": totals.input_tokens + totals.output_tokens,
            "calls": totals.calls,
            "avg_tps": avg_tps,
        });
        if context_max > 0 && used > 0 {
            usage["context_used"] = json!(used);
            usage["context_max"] = json!(context_max);
            usage["context_percent"] = json!((used * 100 / context_max).min(100));
            usage["context_source"] = json!("provider_usage");
            usage["context_estimated"] = json!(false);
        }
        usage
    }

    /// A model's context window, as its provider lists it (remembered once the provider has
    /// answered), else Hermes's fallback for an unknown window.
    /// A model's context window when it's known without asking the provider: the endpoint's
    /// setting, else what its model list said (cached by `context_length`). `None` lets the
    /// runtime assume its default; the 256k display fallback is never passed on.
    fn known_context(&self, provider: &str, model: &str) -> Option<usize> {
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

    /// What every turn of a session runs with: its model's window (so compression starts in
    /// time for a small local model) and the side jobs' models (Settings ▸ Model ▸ Auxiliary).
    fn turn_options(&self, session: &str) -> TurnOptions {
        let settings = Settings::load(&self.data_dir);
        let record = self.runtime.store().session(session).ok().flatten();
        let context = |provider: &str, model: &str| self.known_context(provider, model);
        // Only a picked effort is sent; unset leaves the provider's default (as Hermes).
        let effort = self
            .efforts
            .lock()
            .ok()
            .and_then(|efforts| efforts.get(session).cloned())
            .filter(|e| e != "none");
        let mut aux = crate::aux_models::runtime_models(&settings, &context);
        // "Inherit main model effort" (the Auxiliary page's default): compression and the
        // background review think as hard as the chat. The approval judge and titles keep
        // reasoning off unless picked, as Hermes runs them.
        for job in [&mut aux.compression, &mut aux.review].into_iter().flatten() {
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
        }
    }

    async fn context_length(&self, provider: &str, model: &str) -> usize {
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
    async fn model_facts(&self, provider: &str, model: &str) -> ModelFacts {
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
            .map(|m| ModelFacts { context_length: m.context_length, vision: m.supports_vision })
            .unwrap_or_default();
        if let Ok(mut lengths) = self.context_lengths.lock() {
            lengths.insert(key, facts);
        }
        facts
    }

    /// Whether attached images go to the model as images (Hermes's `agent.image_input_mode`:
    /// `native` always, `text` never, `auto` when the model takes them). Otherwise the prompt
    /// names the image's path, for `vision_analyze`.
    fn sends_images(&self, provider: &str, model: &str) -> bool {
        match self.rest.config_value("agent.image_input_mode").as_str() {
            Some("native") => true,
            Some("text") => false,
            _ => self
                .context_lengths
                .lock()
                .ok()
                .and_then(|facts| facts.get(&format!("{provider}\n{model}")).and_then(|f| f.vision))
                .unwrap_or(false),
        }
    }

    /// The context gauge (`session.context_breakdown`, Hermes's `agent/context_breakdown.py`):
    /// the frozen system prompt and the conversation, estimated at ~4 characters a token, against
    /// the model's window. How full it is comes from the last request's real input tokens when the
    /// session has made one, else from the estimate.
    async fn context_breakdown(&self, id: &str) -> RpcResult {
        let record = self
            .runtime
            .store()
            .session(id)
            .ok()
            .flatten()
            .ok_or((SERVER_ERROR, format!("Session not found: {id}")))?;
        let tokens = |chars: usize| chars.div_ceil(4);
        let system = tokens(record.system_prompt.len());
        let messages = self.runtime.messages(id).unwrap_or_default();
        let conversation = tokens(serde_json::to_string(&messages).map_or(0, |json| json.len()));
        // Every request also carries the tool definitions: the built-in tools and MCP servers'.
        let (builtin, mcp) = self.runtime.tool_definitions();
        let json_tokens = |tools: &[Value]| {
            if tools.is_empty() {
                0
            } else {
                tokens(serde_json::to_string(tools).map_or(0, |json| json.len()))
            }
        };
        let (tool_definitions, mcp_tools) = (json_tokens(&builtin), json_tokens(&mcp));
        let estimated_total = system + tool_definitions + mcp_tools + conversation;
        let context_max = self.context_length(&record.provider, &record.model).await;
        let measured = self
            .runtime
            .store()
            .usage_totals(id)
            .map_or(0, |usage| usage.last_input_tokens.max(0) as usize);
        let (mut used, source) = if measured > 0 {
            (measured, "provider_usage")
        } else {
            (estimated_total, "local_estimate")
        };
        if context_max > 0 {
            used = used.min(context_max);
        }
        let category = |id: &str, label: &str, color: &str, tokens: usize| json!({"id": id, "label": label, "color": color, "tokens": tokens});
        // Hermes's order; an empty category is left out, except the conversation.
        let categories: Vec<Value> = [
            (
                "system_prompt",
                "System prompt",
                "var(--context-usage-system)",
                system,
            ),
            (
                "tool_definitions",
                "Tool definitions",
                "var(--context-usage-tools)",
                tool_definitions,
            ),
            ("mcp", "MCP", "var(--context-usage-mcp)", mcp_tools),
            (
                "conversation",
                "Conversation",
                "var(--context-usage-conversation)",
                conversation,
            ),
        ]
        .into_iter()
        .filter(|(id, _, _, tokens)| *tokens > 0 || *id == "conversation")
        .map(|(id, label, color, tokens)| category(id, label, color, tokens))
        .collect();
        Ok(json!({
            "categories": categories,
            "estimated_total": estimated_total,
            "context_max": context_max,
            "context_used": used,
            "context_percent": if context_max > 0 { (used * 100 / context_max).min(100) } else { 0 },
            "context_source": source,
            "context_estimated": source != "provider_usage",
            "model": record.model,
        }))
    }

    /// The provider and model picked while the session's turn runs, if any.
    fn pending_route(&self, id: &str) -> Option<(String, String)> {
        self.pending_routes.lock().ok()?.get(id).cloned()
    }

    /// A model picked while the last turn ran, applied now that it has finished.
    fn apply_pending_route(&self, id: &str) {
        let pick = self
            .pending_routes
            .lock()
            .ok()
            .and_then(|mut picks| picks.remove(id));
        if let Some((provider, model)) = pick {
            if let Err((_, error)) = self.route_session(id, &provider, &model) {
                tracing::warn!("applying the model picked during the turn in {id}: {error}");
            }
        }
    }

    /// Hermes's canonical `session.mutate`: a session edit presenting the revision the page last
    /// saw. Edits here are one user's, so the revision is reported but not enforced.
    async fn mutate(&self, params: &Value, settings: &Settings) -> RpcResult {
        let id = session_id(params)?;
        let payload = &params["payload"];
        let mut receipt = match params["operation"].as_str().unwrap_or_default() {
            "rename" => {
                let title = payload["title"].as_str().unwrap_or_default();
                self.rename(&id, title)?;
                json!({"title": title.trim()})
            }
            "model" => {
                let model = payload["model"].as_str().unwrap_or_default();
                let provider = payload["provider"].as_str().unwrap_or_default();
                // The canonical pick is session policy: it never rewrites the defaults.
                // Picked mid-reply, it waits for the next turn, but shows as the session's model
                // at once (no "applies next turn" notice).
                self.switch_model(
                    &json!({"session_id": id, "value": format!("{model} --provider {provider} --session")}),
                    settings,
                )?;
                json!({"model": model, "provider": provider, "status": "applied"})
            }
            "reasoning" => {
                let effort = payload["effort"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                if let Ok(mut efforts) = self.efforts.lock() {
                    efforts.insert(id.clone(), effort.clone());
                }
                self.emit(&id, "session.info", self.session_info(&id));
                json!({"effort": effort, "status": "applied"})
            }
            // Restore / edit: drop the named user row and everything after it (the page then
            // submits the text again).
            "rewind" => {
                let row = payload["target_message_id"]
                    .as_u64()
                    .ok_or((INVALID_PARAMS, "target_message_id is required".to_string()))?;
                if self.runtime.is_running(&id) {
                    return Err((SERVER_ERROR, BUSY.to_string()));
                }
                let kept = self
                    .runtime
                    .rewind(&id, transcript::kept_before_row(row as usize))
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                json!({"status": "applied", "target_message_id": row, "message_count": kept.len()})
            }
            // Move to project (`session.workspace.move`, and the composer's folder picker on an
            // open chat — both become this mutation in the page's canonical protocol): the stored
            // cwd and git identity move together (`workspace.rs`), idle sessions only. The live
            // state is dropped so the next turn's tools and shell start in the new folder.
            "workspace" => {
                let requested = payload["cwd"]
                    .as_str()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .ok_or((INVALID_PARAMS, "cwd is required".to_string()))?;
                if self.runtime.is_running(&id) {
                    return Err((SERVER_ERROR, BUSY.to_string()));
                }
                let runtime = self.runtime.clone();
                let workspaces = self.workspaces.clone();
                let moved = id.clone();
                let requested = requested.to_string();
                let (cwd, branch, root) =
                    tokio::task::spawn_blocking(move || {
                        crate::workspace::move_session(runtime.store(), &workspaces, &moved, &requested)
                    })
                    .await
                    .map_err(|e| (SERVER_ERROR, format!("the move failed: {e}")))?
                    .map_err(|e| match e {
                        crate::workspace::MoveError::InvalidFolder(folder) => {
                            (INVALID_PARAMS, format!("No such folder: {folder}"))
                        }
                        crate::workspace::MoveError::Store(message) => (SERVER_ERROR, message),
                    })?;
                if let Err(e) = self.runtime.evict(&id).await {
                    tracing::warn!("dropping the moved session's live state: {e:#}");
                }
                self.emit(&id, "session.info", self.session_info(&id));
                json!({"cwd": cwd, "branch": branch, "git_repo_root": root})
            }
            // The keyboard shortcut's `session.archive` (`canonical-protocol.ts`).
            "archive" => {
                let archived = payload["archived"].as_bool().unwrap_or(true);
                self.set_archived(&id, archived)?;
                json!({"archived": archived})
            }
            other => {
                return Err((
                    SERVER_ERROR,
                    format!("{other} isn't available in hermes-rs yet"),
                ));
            }
        };
        receipt["session_id"] = json!(id);
        receipt["revision"] = json!(self.bump_revision(&id));
        receipt["execution_generation"] = json!(EXECUTION_GENERATION);
        Ok(receipt)
    }

    /// `session.rewind` (Desktop's typed `/undo` and `/retry`, `slash-session-commands.ts`): drop
    /// the named user row and everything after it, answering with the dropped prompt, which `/undo`
    /// puts back in the composer and `/retry` sends again. `replay_only` asks for a rewind whose
    /// reply was lost; this backend answers every rewind, so there is none.
    fn rewind_turn(&self, params: &Value) -> RpcResult {
        let id = session_id(params)?;
        let nothing = || json!({"session_id": id, "status": "nothing", "rewound_count": 0});
        if params["replay_only"] == true {
            return Ok(nothing());
        }
        let Some(row) = params["target_message_id"].as_u64() else {
            return Ok(nothing());
        };
        if self.runtime.is_running(&id) {
            return Err((SERVER_ERROR, "session busy".to_string()));
        }
        let error = |e: anyhow::Error| (SERVER_ERROR, format!("{e:#}"));
        let before = self.runtime.messages(&id).map_err(error)?;
        let keep = transcript::kept_before_row(row as usize);
        let Some(target) = before.get(keep) else {
            return Ok(nothing());
        };
        let content = target.as_concat_text();
        let kept = self.runtime.rewind(&id, keep).map_err(error)?;
        Ok(json!({
            "session_id": id,
            "status": "applied",
            "rewound_count": before.len() - kept.len(),
            "target_message": {"id": row, "content": content},
            "revision": self.bump_revision(&id),
            "execution_generation": EXECUTION_GENERATION,
        }))
    }

    /// `/rollback` (`slash-desktop-commands.ts`): the session folder's file checkpoints, a diff
    /// against one, restoring one (Hermes's `rollback.list|diff|restore`, from
    /// `tools/checkpoint_manager.py`). `hash` is a hash prefix or a number in the list.
    fn rollback(&self, method: &str, params: &Value) -> RpcResult {
        let id = session_id(params)?;
        let error = |e: String| (SERVER_ERROR, e);
        let checkpoints = self.runtime.checkpoints();
        if !checkpoints.enabled() {
            return match method {
                "rollback.list" => Ok(json!({"enabled": false, "checkpoints": []})),
                _ => Err(error(
                    "Checkpoints are off: Settings ▸ Safety ▸ Checkpoints.".into(),
                )),
            };
        }
        let workdir = self
            .runtime
            .checkpoint_dir(&id)
            .map_err(|e| error(format!("{e:#}")))?;
        let list = checkpoints.list(&workdir);
        if method == "rollback.list" {
            let rows: Vec<Value> = list
                .iter()
                .map(|c| json!({"hash": c.hash, "timestamp": c.timestamp, "message": c.reason}))
                .collect();
            return Ok(json!({"enabled": true, "checkpoints": rows}));
        }
        let wanted = params["hash"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let hash = match wanted.parse::<usize>() {
            Ok(n) if (1..=list.len()).contains(&n) => Some(list[n - 1].hash.clone()),
            _ if wanted.is_empty() => None,
            _ => list
                .iter()
                .find(|c| c.hash.starts_with(&wanted))
                .map(|c| c.hash.clone()),
        }
        .ok_or_else(|| error(format!("No checkpoint {wanted}; /rollback lists them.")))?;
        if method == "rollback.diff" {
            let diff = checkpoints.diff(&workdir, &hash).map_err(error)?;
            return Ok(json!({"stat": diff.stat, "diff": diff.diff}));
        }
        if self.runtime.is_running(&id) {
            return Err(error("session busy".into()));
        }
        let file = params["file_path"]
            .as_str()
            .filter(|f| !f.trim().is_empty());
        match self
            .runtime
            .restore_checkpoint(&id, &hash, file, params["all"] == true)
        {
            Ok((restored, removed)) => {
                let mut kept = Vec::new();
                if !restored.skipped_user_edits.is_empty() {
                    kept.push(format!(
                        "Kept your hand edits: {} (/rollback {} --all restores those too).",
                        restored.skipped_user_edits.join(", "),
                        &hash[..8.min(hash.len())]
                    ));
                }
                if !restored.skipped_oversize.is_empty() {
                    kept.push(format!(
                        "Kept (too large for checkpoints): {}.",
                        restored.skipped_oversize.join(", ")
                    ));
                }
                let reason = [vec![restored.reason.clone()], kept].concat().join("\n");
                // (After a whole-folder restore the page reloads the transcript itself.)
                Ok(json!({
                    "success": true,
                    "restored_to": restored.restored_to,
                    "reason": reason,
                    "directory": restored.directory.display().to_string(),
                    "file": restored.file,
                    "history_removed": removed,
                }))
            }
            Err(e) => Ok(json!({"success": false, "error": format!("{e:#}")})),
        }
    }

    /// Hermes's built-in commands that report on the session (`slash.exec` output, shown as a
    /// note in the chat): `/status`, `/context`, `/usage`, `/insights`, `/diff`, `/tools`,
    /// `/memory`, and `/refine`, which starts a review. `None` for anything else (skills).
    /// Hermes: `hermes_cli/commands.py`, `cli_commands_mixin.py`.
    /// The `/review` reviewer's goal and briefing: the session's last user and assistant messages
    /// (their text), and the skills it loaded (`snapshot_recent_messages`,
    /// `collect_parent_loaded_skills`).
    fn review_task(&self, id: &str, instructions: &str) -> (String, String) {
        use hermes_llm::conversation::message::MessageContent;
        let messages = self.runtime.messages(id).unwrap_or_default();
        let mut recent: Vec<(bool, String)> = messages
            .iter()
            .rev()
            .filter(|m| m.is_user_visible())
            .filter_map(|m| {
                let is_user = m.role == rmcp::model::Role::User;
                let text: Vec<&str> = m
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        MessageContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let text = text.join("\n").trim().to_string();
                // A command's prompt is shown as the command it was typed as.
                let text = commands::typed_command(&text).unwrap_or(text);
                (!text.is_empty()).then_some((is_user, text))
            })
            .take(hermes_prompts::coding::REVIEW_CONTEXT_MESSAGES)
            .collect();
        recent.reverse();
        let mut skills: Vec<String> = Vec::new();
        for request in messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(MessageContent::as_tool_request)
        {
            let Ok(call) = &request.tool_call else { continue };
            let args = call.arguments.clone().unwrap_or_default();
            // Whole-skill loads only (a reference file is a detail of the skill).
            if call.name == "skill_view" && args.get("file_path").is_none() {
                if let Some(name) = args.get("name").and_then(Value::as_str) {
                    if !skills.iter().any(|s| s == name) && skills.len() < 8 {
                        skills.push(name.to_string());
                    }
                }
            }
        }
        hermes_prompts::coding::review_task(&recent, instructions, &skills)
    }

    /// `/plan`, `/init`, `/learn`, `/review` (`commands::coding_command`): `None` for another
    /// command. `/init` writes the session's folder's AGENTS.md; `/review` needs a conversation.
    fn coding_command(&self, params: &Value, name: &str, arg: &str) -> Option<RpcResult> {
        let name = name.trim_start_matches('/');
        if !matches!(name, "plan" | "init" | "learn" | "review") {
            return None;
        }
        let record = session_id(params)
            .ok()
            .and_then(|id| self.runtime.store().session(&id).ok().flatten());
        let cwd = record.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
        if name == "review"
            && record
                .as_ref()
                .is_none_or(|r| self.runtime.messages(&r.id).map_or(true, |m| m.is_empty()))
        {
            return Some(Ok(json!({"type": "exec",
                "output": "Nothing to review yet — send a message first."})));
        }
        if name == "init" && cwd.is_empty() {
            return Some(Err((INVALID_PARAMS, "/init needs a session with a folder".to_string())));
        }
        let existing = (name == "init")
            .then(|| std::fs::read_to_string(std::path::Path::new(&cwd).join("AGENTS.md")).ok())
            .flatten()
            .map(|text| text.trim_start_matches('\u{feff}').to_string());
        commands::coding_command(name, arg, &cwd, existing.as_deref()).map(Ok)
    }

    async fn builtin_command(
        &self,
        params: &Value,
        command: &str,
        settings: &Settings,
    ) -> Option<RpcResult> {
        let command = command.trim().trim_start_matches('/');
        let (name, arg) = command
            .split_once(char::is_whitespace)
            .map(|(n, a)| (n, a.trim()))
            .unwrap_or((command, ""));
        let id = session_id(params).ok();
        let error = |e: anyhow::Error| (SERVER_ERROR, format!("{e:#}"));
        let need_id = || {
            id.clone()
                .ok_or((INVALID_PARAMS, "session_id is required".to_string()))
        };
        let output: Result<String, (i64, String)> = match name {
            // Every session of the original Hermes not imported yet (`hermes_import.rs`).
            "import-hermes" => match self.import_route(settings) {
                Ok((provider, model)) => {
                    let runtime = self.runtime.clone();
                    let data_dir = self.data_dir.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        let sources = crate::hermes_import::sources();
                        let target = crate::hermes_import::Target {
                            runtime: &runtime,
                            provider,
                            model,
                            data_dir: &data_dir,
                        };
                        (sources, crate::hermes_import::import_all(&target))
                    })
                    .await;
                    match found {
                        Ok((sources, _)) if sources.is_empty() => Ok(
                            "No Hermes data found here (looked in HERMES_HOME, %LOCALAPPDATA%\\hermes and ~\\.hermes)."
                                .to_string(),
                        ),
                        Ok((sources, (imported, already, failed))) => {
                            let mut text = format!(
                                "Imported {imported} session{} from Hermes ({} database{}); {already} already here.",
                                if imported == 1 { "" } else { "s" },
                                sources.len(),
                                if sources.len() == 1 { "" } else { "s" },
                            );
                            if !failed.is_empty() {
                                text.push_str(&format!("\nNot imported ({}):\n{}", failed.len(), failed.join("\n")));
                            }
                            Ok(text)
                        }
                        Err(e) => Err((SERVER_ERROR, e.to_string())),
                    }
                }
                Err(e) => Err(e),
            },
            "status" => match need_id() {
                Ok(id) => self.status_text(&id).await,
                Err(e) => Err(e),
            },
            "context" | "ctx" => match need_id() {
                Ok(id) => self.context_text(&id).await,
                Err(e) => Err(e),
            },
            "usage" => match need_id() {
                Ok(id) => {
                    let window = self.window_for(&id).await;
                    self.runtime.usage_text(&id, window).map_err(error)
                }
                Err(e) => Err(e),
            },
            "insights" => Ok(self.runtime.insights_text(arg)),
            "diff" => need_id().and_then(|id| self.runtime.diff_report(&id, arg).map_err(error)),
            "tools" => {
                let (builtin, mcp) = self.runtime.tool_definitions();
                let line = |t: &Value| {
                    let description = t["description"].as_str().unwrap_or_default();
                    let first = description.split(['.', '\n']).next().unwrap_or_default();
                    format!(
                        "  {}  {}",
                        t["name"].as_str().unwrap_or_default(),
                        first.trim()
                    )
                };
                let mut out = vec![format!("Tools ({}):", builtin.len())];
                out.extend(builtin.iter().map(line));
                if !mcp.is_empty() {
                    out.push(format!("\nMCP tools ({}):", mcp.len()));
                    out.extend(mcp.iter().map(line));
                }
                Ok(out.join("\n"))
            }
            "memory" => {
                let memory = self.runtime.memory();
                let pending = memory.pending();
                let gate = if memory.config().write_approval {
                    "on"
                } else {
                    "off"
                };
                Ok(if pending.is_empty() {
                    format!("No memory writes are waiting for approval (approval gate: {gate}).")
                } else {
                    let mut out = vec![format!(
                        "Memory writes waiting for approval (gate: {gate}):"
                    )];
                    out.extend(pending.iter().map(|w| format!("  {}  {}", w.id, w.summary)));
                    out.join("\n")
                })
            }
            "refine" => {
                match need_id().and_then(|id| Ok((self.provider_for(&id, settings)?, id))) {
                    Ok((provider, id)) => {
                        let focus = (!arg.is_empty()).then(|| arg.to_string());
                        self.runtime
                        .refine(&id, &provider, self.turn_options(&id), focus)
                        .map(|()| {
                            "Reviewing this conversation for lessons to save to memory and skills; \
                             the review runs in the background."
                                .to_string()
                        })
                        .map_err(error)
                    }
                    Err(e) => Err(e),
                }
            }
            _ => return None,
        };
        Some(output.map(|output| json!({"type": "exec", "output": output})))
    }

    /// The provider a session's turns go to.
    fn provider_for(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, (i64, String)> {
        let record = self
            .runtime
            .store()
            .session(id)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?
            .ok_or((SERVER_ERROR, format!("Session {id} not found")))?;
        Ok(settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?
            .runtime())
    }

    /// The session model's context window, as the gauge shows it.
    async fn window_for(&self, id: &str) -> Option<usize> {
        let record = self.runtime.store().session(id).ok().flatten()?;
        Some(self.context_length(&record.provider, &record.model).await)
    }

    /// `/status`: the session, its model and folder, tokens so far, the context in use.
    async fn status_text(&self, id: &str) -> Result<String, (i64, String)> {
        let store = self.runtime.store();
        let record = store
            .session(id)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?
            .ok_or((SERVER_ERROR, format!("Session {id} not found")))?;
        let totals = store.usage_totals(id).unwrap_or_default();
        let messages = store.message_count(id).unwrap_or(0);
        let window = self.context_length(&record.provider, &record.model).await;
        let used = self
            .runtime
            .context_breakdown(id)
            .map(|b| b.total())
            .unwrap_or(0);
        Ok([
            format!(
                "Session: {} ({})",
                record.title.as_deref().unwrap_or("Untitled"),
                record.id
            ),
            format!("Model: {} via {}", record.model, record.provider),
            format!("Folder: {}", record.cwd),
            format!("Messages: {messages}"),
            format!(
                "Tokens: {} in, {} out over {} calls",
                totals.input_tokens, totals.output_tokens, totals.calls
            ),
            format!(
                "Context: ~{used} of {window} ({}%)",
                (used * 100).checked_div(window).unwrap_or(0)
            ),
        ]
        .join("\n"))
    }

    /// `/context`: what the next request is made of, by part, against the model's window.
    async fn context_text(&self, id: &str) -> Result<String, (i64, String)> {
        let b = self
            .runtime
            .context_breakdown(id)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        let window = self.window_for(id).await.unwrap_or(FALLBACK_CONTEXT);
        let total = b.total();
        let row = |name: &str, n: usize| {
            format!(
                "  {name:<18}~{n:>8}  {:>3}%",
                (n * 100).checked_div(total).unwrap_or(0)
            )
        };
        Ok([
            format!(
                "Context: ~{total} of {window} tokens ({}% of the window)",
                (total * 100).checked_div(window).unwrap_or(0)
            ),
            row("System prompt", b.system_prompt),
            row("Tool definitions", b.tool_definitions),
            row("Skills", b.skills),
            row("Memory", b.memory),
            row("Conversation", b.conversation),
            "Estimates (characters / 4); /compress summarizes the conversation now.".into(),
        ]
        .join("\n"))
    }

    /// `/compress [focus]` (`session.compress`): summarizes the conversation now, as Hermes's
    /// context compression does when the window fills, and answers with the shorter transcript
    /// the page shows from then on.
    async fn compress_session(&self, params: &Value, settings: &Settings) -> RpcResult {
        let id = session_id(params)?;
        let error = |e: anyhow::Error| (SERVER_ERROR, format!("{e:#}"));
        let record = self
            .runtime
            .store()
            .session(&id)
            .map_err(error)?
            .ok_or((SERVER_ERROR, format!("Session {id} not found")))?;
        let provider = settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?
            .runtime();
        let before_messages = self.runtime.messages(&id).map_err(error)?.len();
        let before_tokens = self
            .runtime
            .context_breakdown(&id)
            .map(|b| b.total())
            .unwrap_or(0);
        let focus = params["focus_topic"]
            .as_str()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string);
        let mut turn = self
            .runtime
            .compress(&id, &provider, self.turn_options(&id), focus)
            .map_err(error)?;
        let mut failed = None;
        while let Some(event) = turn.events.recv().await {
            if let RuntimeEvent::TurnFinished(TurnStatus::Failed(e)) = event {
                failed = Some(e);
            }
        }
        // A conversation too short to summarize ends the runtime's compress turn with this
        // (`Runtime::compress`): nothing happened, nothing failed.
        if failed.as_deref() == Some("Nothing to compress yet.") {
            failed = None;
        }
        let after = self.runtime.messages(&id).map_err(error)?;
        let after_tokens = self
            .runtime
            .context_breakdown(&id)
            .map(|b| b.total())
            .unwrap_or(0);
        let compressed = failed.is_none() && after.len() < before_messages;
        let headline = match (&failed, compressed) {
            (Some(e), _) => format!("Compression failed: {e}"),
            (None, true) => format!("Compressed: {before_messages} → {} messages", after.len()),
            (None, false) => "Nothing to compress yet.".to_string(),
        };
        self.emit(&id, "session.info", self.session_info(&id));
        Ok(json!({
            "status": if failed.is_some() { "aborted" } else if compressed { "compressed" } else { "noop" },
            "removed": before_messages.saturating_sub(after.len()),
            "before_messages": before_messages,
            "after_messages": after.len(),
            "before_tokens": before_tokens,
            "after_tokens": after_tokens,
            "messages": transcript::messages(&after),
            "summary": {
                "headline": headline,
                "token_line": compressed.then(|| format!("~{before_tokens} → ~{after_tokens} tokens")),
                "aborted": failed.is_some(),
                "noop": failed.is_none() && !compressed,
            },
        }))
    }

    /// `session.export` (`/save [json|md|html] [name] [redact]`): the conversation as file content;
    /// the page saves it through a Save As dialog.
    fn export_session(&self, params: &Value) -> RpcResult {
        let id = session_id(params)?;
        let format = crate::export::Format::parse(params["format"].as_str().unwrap_or("json"))
            .ok_or((INVALID_PARAMS, "format is json, md or html".to_string()))?;
        let store = self.runtime.store();
        let error = |e: anyhow::Error| (SERVER_ERROR, format!("{e:#}"));
        let record = store
            .session(&id)
            .map_err(error)?
            .ok_or((SERVER_ERROR, format!("Session {id} not found")))?;
        let rows = transcript::messages(&store.load_messages(&id).map_err(error)?);
        let (content, filename) =
            crate::export::export(&record, &rows, format, params["redact"] == true);
        Ok(json!({"content": content, "filename": filename, "format": format.extension()}))
    }

    /// Renames a session; the page's lists hear it through Hermes's `session.title` event.
    /// Archives or restores a session (the sidebar, the shortcut, Settings ▸ Sessions). An
    /// archived session leaves the pins, as Hermes's does.
    fn set_archived(&self, id: &str, archived: bool) -> Result<(), (i64, String)> {
        let store = self.runtime.store();
        let record = store
            .session(id)
            .ok()
            .flatten()
            .ok_or((INVALID_PARAMS, format!("No session {id}.")))?;
        if archived && record.pinned {
            store.set_pinned(id, false).map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        }
        self.archive.set(id, archived, chrono::Utc::now().timestamp_millis());
        Ok(())
    }

    fn rename(&self, id: &str, title: &str) -> Result<(), (i64, String)> {
        let title = title.trim();
        self.runtime
            .store()
            .set_title(id, title)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        self.emit(
            id,
            "session.title",
            json!({"session_id": id, "title": title}),
        );
        Ok(())
    }

    /// The session's edit counter (Hermes's CAS revision), which the page presents with each edit.
    fn revision(&self, id: &str) -> u64 {
        self.revisions
            .lock()
            .ok()
            .and_then(|revisions| revisions.get(id).copied())
            .unwrap_or(1)
    }

    fn bump_revision(&self, id: &str) -> u64 {
        let Ok(mut revisions) = self.revisions.lock() else {
            return 1;
        };
        let revision = revisions.entry(id.to_string()).or_insert(1);
        *revision += 1;
        *revision
    }

    /// The session's reasoning effort: picked in the composer, else Settings' default.
    fn effort(&self, id: &str) -> String {
        self.efforts
            .lock()
            .ok()
            .and_then(|efforts| efforts.get(id).cloned())
            .or_else(|| {
                self.rest
                    .config_value("agent.reasoning_effort")
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    /// Whether the session has a turn in flight as the page sees it: the runtime's turn, or the
    /// gap between a turn and its queued follow-up (the page must not see the turn end there).
    fn is_live(&self, id: &str) -> bool {
        self.runtime.is_running(id) || self.live_turns.lock().is_ok_and(|turns| turns.contains(id))
    }

    fn session_info(&self, id: &str) -> Value {
        let record = self.runtime.store().session(id).ok().flatten();
        let running = self.is_live(id);
        match record {
            Some(record) => json!({
                // A pick made mid-reply shows at once; it takes effect from the next turn.
                "model": self.pending_route(id).map_or(record.model, |(_, model)| model),
                "provider": self.pending_route(id).map_or(record.provider, |(provider, _)| provider),
                "cwd": record.cwd,
                // The git identity a move recorded; the sidebar groups by the repo root.
                "git_branch": self.workspaces.identity(id).and_then(|(branch, _)| branch),
                "git_repo_root": self.workspaces.identity(id).and_then(|(_, root)| root),
                "title": record.title.unwrap_or_default(),
                "stored_session_id": record.id,
                "running": running,
                "version": crate::rest::VERSION,
                // The protocol this backend speaks; the page warns "out of date" without it.
                "desktop_protocol": "hermes-gateway-v1",
                "profile_name": "default",
                "reasoning_effort": self.effort(id),
                "revision": self.revision(id),
                "execution_generation": EXECUTION_GENERATION,
                // The queue panel's rows: prompts waiting for the running turn.
                "pending": self.pending_rows(id),
                // The status bar's numbers for a session opened before any turn of this run.
                "usage": self.usage_snapshot(id, None),
                // The status bar's approval mode (applied from an event naming its profile).
                "approval_mode": self.rest.config_value("approvals.mode"),
            }),
            None => json!({"stored_session_id": id, "running": running}),
        }
    }

    /// The session's queued prompts as Hermes's pending admissions, in the order they'll run.
    fn pending_rows(&self, id: &str) -> Vec<Value> {
        let Ok(queued) = self.queued.lock() else {
            return Vec::new();
        };
        queued
            .get(id)
            .map(|queue| {
                queue
                    .iter()
                    // A redirect or a missed steer is on the page already, as the bubble the
                    // composer drew: as a card too it would show twice.
                    .filter(|params| params["shown"] != true)
                    .map(|params| {
                        json!({
                            "admission_id": params["submission_id"],
                            "text": params["text"],
                            "status": "queued",
                            "controls": ["steer", "replace", "cancel"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Queues a prompt behind the running turn (at the front for a redirect or a steer the turn
    /// missed) and tells the page; returns its admission id.
    fn enqueue(&self, id: &str, mut params: Value, front: bool) -> String {
        let admission = admission_id(&params).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        params["session_id"] = json!(id);
        params["submission_id"] = json!(admission);
        if let Ok(mut queued) = self.queued.lock() {
            let queue = queued.entry(id.to_string()).or_default();
            if front {
                queue.push_front(params);
            } else {
                queue.push_back(params);
            }
        }
        self.emit(id, "session.info", self.session_info(id));
        admission
    }

    /// Takes a queued prompt out by its admission id (`None` when it already ran or never was).
    fn dequeue(&self, id: &str, admission: &str) -> Option<Value> {
        let taken = self.queued.lock().ok().and_then(|mut queued| {
            let queue = queued.get_mut(id)?;
            let index = queue
                .iter()
                .position(|params| params["submission_id"] == admission)?;
            queue.remove(index)
        });
        if taken.is_some() {
            self.emit(id, "session.info", self.session_info(id));
        }
        taken
    }

    fn subscribe(&self, session_id: &str, outbox: &Outbox) {
        self.hub.subscribe(session_id, outbox);
    }

    /// What imported sessions talk to: the default endpoint and model.
    fn import_route(
        &self,
        settings: &Settings,
    ) -> Result<(hermes_runtime::ProviderConfig, String), (i64, String)> {
        let provider = settings
            .active()
            .ok_or((SERVER_ERROR, "Set up an endpoint first (Settings ▸ Providers): imported sessions use it.".to_string()))?;
        let model = settings
            .model
            .clone()
            .or_else(|| provider.default_model.clone())
            .ok_or((SERVER_ERROR, "Choose a model first: imported sessions use it.".to_string()))?;
        Ok((provider.runtime(), model))
    }

    /// The provider a session talks to (its own, else the default).
    fn session_provider(
        &self,
        id: &str,
        settings: &Settings,
    ) -> Result<hermes_runtime::ProviderConfig, (i64, String)> {
        let record = self
            .runtime
            .store()
            .session(id)
            .ok()
            .flatten()
            .ok_or((INVALID_PARAMS, format!("No session {id}.")))?;
        Ok(settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?
            .runtime())
    }

    /// `session.branch` (up to a message: `through_message_id`, else `count` of the page's
    /// rows), `session.branch_whole` and `session.branch_stored` (all of it, `whole`): a new
    /// session starting with the parent's conversation, listed under it (`branch.rs`).
    fn branch(
        &self,
        params: &Value,
        settings: &Settings,
        outbox: &Outbox,
        whole: bool,
        omit_messages: bool,
    ) -> RpcResult {
        let parent = params["parent_session_id"]
            .as_str()
            .or(params["session_id"].as_str())
            .filter(|id| !id.is_empty())
            .ok_or((INVALID_PARAMS, "session_id is required".to_string()))?
            .to_string();
        let record = self
            .runtime
            .store()
            .session(&parent)
            .ok()
            .flatten()
            .ok_or((INVALID_PARAMS, format!("No session {parent}.")))?;
        let key = params["idempotency_key"].as_str().filter(|k| !k.is_empty());
        let existing = key.and_then(|key| {
            self.branch_keys
                .lock()
                .ok()
                .and_then(|keys| keys.get(key).cloned())
        });
        let (child, kept) = match existing {
            // A retried request: the branch it already made.
            Some(child) => {
                let kept = self.runtime.messages(&child).unwrap_or_default();
                (child, kept)
            }
            None => {
                let messages = self
                    .runtime
                    .messages(&parent)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                let keep = if whole {
                    messages.len()
                } else if let Some(row) = params["through_message_id"].as_u64() {
                    row as usize
                } else if let Some(count) = params["count"].as_u64().filter(|c| *c > 0) {
                    // The page's rows: through the count-th one.
                    transcript::messages(&messages)
                        .get(count as usize - 1)
                        .and_then(|row| row["row_id"].as_u64())
                        .map_or(messages.len(), |row| row as usize)
                } else {
                    messages.len()
                };
                let kept = crate::branch::cut(&messages, keep);
                if kept.is_empty() {
                    return Err((INVALID_PARAMS, "nothing to branch — send a message first".to_string()));
                }
                let provider = self.session_provider(&parent, settings)?;
                let cwd = params["cwd"]
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .unwrap_or(&record.cwd)
                    .to_string();
                let made = self
                    .runtime
                    .create_session(&provider, &record.model, None, std::path::Path::new(&cwd))
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                self.runtime
                    .store()
                    .replace_messages(&made.id, &kept)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                let taken: Vec<String> = self
                    .runtime
                    .store()
                    .list_sessions(1000)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|s| s.title)
                    .collect();
                let title = params["name"]
                    .as_str()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::branch::title(record.title.as_deref(), &taken));
                let _ = self.runtime.store().set_title(&made.id, &title);
                self.branches.link(&made.id, &parent);
                if let (Some(key), Ok(mut keys)) = (key, self.branch_keys.lock()) {
                    keys.insert(key.to_string(), made.id.clone());
                }
                (made.id, kept)
            }
        };
        self.subscribe(&child, outbox);
        let title = self
            .runtime
            .store()
            .session(&child)
            .ok()
            .flatten()
            .and_then(|r| r.title)
            .unwrap_or_default();
        let mut answer = json!({
            "session_id": child,
            "stored_session_id": child,
            "title": title,
            "parent": parent,
            "message_count": kept.len(),
            "info": self.session_info(&child),
            "revision": self.revision(&child),
            "execution_generation": EXECUTION_GENERATION,
        });
        if omit_messages {
            answer["messages_omitted"] = json!(true);
        } else {
            answer["messages"] = json!(transcript::messages(&kept));
        }
        Ok(answer)
    }

    fn create_session(&self, params: &Value, settings: &Settings, outbox: &Outbox) -> RpcResult {
        let provider_id = params["provider"]
            .as_str()
            .map(str::to_string)
            .or_else(|| settings.active().map(|p| p.id.clone()))
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?;
        let provider = settings
            .provider(&provider_id)
            .ok_or((SERVER_ERROR, format!("Unknown provider {provider_id}")))?;
        let model = params["model"]
            .as_str()
            .map(str::to_string)
            .or_else(|| settings.model.clone())
            .ok_or((SERVER_ERROR, "No model is chosen".to_string()))?;
        let cwd = params["cwd"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("C:/"));
        let record = self
            .runtime
            .create_session(&provider.runtime(), &model, None, &cwd)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        if let Some(title) = params["title"].as_str().filter(|t| !t.is_empty()) {
            let _ = self.runtime.store().set_title(&record.id, title);
        }
        // A branch the page builds itself: the messages it starts with, and its parent.
        let seeded: Vec<hermes_llm::conversation::message::Message> = params["messages"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let text = row["content"].as_str().filter(|t| !t.trim().is_empty())?;
                        let message = match row["role"].as_str() {
                            Some("assistant") => {
                                hermes_llm::conversation::message::Message::assistant().with_text(text)
                            }
                            _ => hermes_llm::conversation::message::Message::user().with_text(text),
                        };
                        Some(message.with_generated_id_if_missing())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !seeded.is_empty() {
            self.runtime
                .store()
                .replace_messages(&record.id, &seeded)
                .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        }
        if let Some(parent) = params["parent_session_id"].as_str().filter(|p| !p.is_empty()) {
            self.branches.link(&record.id, parent);
        }
        self.subscribe(&record.id, outbox);
        Ok(json!({
            "session_id": record.id,
            "stored_session_id": record.id,
            "message_count": seeded.len(),
            "messages": transcript::messages(&seeded),
            "info": self.session_info(&record.id),
            "revision": self.revision(&record.id),
            "execution_generation": EXECUTION_GENERATION,
        }))
    }

    fn resume_session(&self, params: &Value, outbox: &Outbox) -> RpcResult {
        let id = session_id(params)?;
        // Hermes's wording: the page drops an id it doesn't know to a fresh draft.
        if self.runtime.store().session(&id).ok().flatten().is_none() {
            return Err((SERVER_ERROR, format!("Session not found: {id}")));
        }
        let messages = self.runtime.messages(&id).map_err(|e| {
            tracing::warn!("resuming {id}: {e:#}");
            (SERVER_ERROR, format!("{e:#}"))
        })?;
        let transcript = transcript::messages(&messages);
        self.subscribe(&id, outbox);
        Ok(json!({
            "session_id": id,
            "stored_session_id": id,
            "message_count": transcript.len(),
            "count": transcript.len(),
            "messages": transcript,
            "info": self.session_info(&id),
            "running": self.runtime.is_running(&id),
            // Questions still waiting: the page shows their cards again.
            "prompts": self.hub.open_prompts(&id),
            // The page's edit and turn-control fences.
            "revision": self.revision(&id),
            "execution_generation": EXECUTION_GENERATION,
            // Prompts still queued behind the running turn (the queue panel).
            "pending_submissions": self
                .pending_rows(&id)
                .into_iter()
                .map(|mut row| {
                    row["user"] = row["text"].clone();
                    row
                })
                .collect::<Vec<_>>(),
        }))
    }

    /// `prompt.submit`: starts a turn and streams it as Hermes's events.
    /// `gated`: the page awaits this call's reply, which goes out before the turn's events.
    fn submit(
        self: Arc<Self>,
        params: &Value,
        settings: &Settings,
        outbox: &Outbox,
        gated: bool,
    ) -> RpcResult {
        let id = session_id(params)?;
        let text = match &params["text"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let admission_id = admission_id(params).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // A session the idle sweep archived is back in use: back in the sidebar too.
        self.archive.reopened(&id);
        if self.runtime.is_running(&id) {
            // A rewind can't wait behind the turn: the page interrupts and retries on "busy".
            if params["confirm_truncate"] == true {
                return Err((SERVER_ERROR, "session busy".to_string()));
            }
            // Anything else waits its turn (Hermes's queued admission, listed in the panel).
            self.subscribe(&id, outbox);
            let admission_id = self.enqueue(&id, params.clone(), false);
            return Ok(json!({
                "status": "queued",
                "admission_id": admission_id,
                "ref": {"session_id": id},
            }));
        }
        // Restore / edit / regenerate: drop that user turn and everything after it first.
        // The cut is a stored row (the transcript's `row_id`), else the n-th typed message.
        if params["confirm_truncate"] == true {
            let messages = self
                .runtime
                .messages(&id)
                .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
            let keep = match (
                params["truncate_before_row_id"].as_u64(),
                params["truncate_before_user_ordinal"].as_u64(),
            ) {
                (Some(row), _) => {
                    Some(transcript::kept_before_row(row as usize).min(messages.len()))
                }
                (None, Some(ordinal)) => Some(
                    transcript::user_turn_index(&messages, ordinal as usize)
                        .unwrap_or(messages.len()),
                ),
                (None, None) => None,
            };
            if let Some(keep) = keep {
                self.runtime
                    .rewind(&id, keep)
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
            }
        }
        let record = self
            .runtime
            .store()
            .session(&id)
            .ok()
            .flatten()
            .ok_or((INVALID_PARAMS, format!("Unknown session {id}")))?;
        // An untitled session takes its first message's first line as its title until the title
        // model's arrives (after this turn); a loaded skill counts as the command that loaded it.
        let shown = commands::typed_command(&text).unwrap_or_else(|| text.clone());
        let titling = record.title.is_none().then(|| {
            let chosen = settings.aux_models.titles.as_ref().and_then(|choice| {
                Some((
                    settings.provider(&choice.provider)?.runtime(),
                    choice.model.clone(),
                    choice.effort.clone(),
                ))
            });
            let fallback = settings
                .provider(&record.provider)
                .map(|p| (p.runtime(), record.model.clone(), None));
            (chosen.or(fallback), shown.clone())
        });
        if record.title.is_none() {
            let line = shown
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            let title: String = line.chars().take(60).collect();
            if !title.is_empty() {
                let _ = self.runtime.store().set_title(&id, &title);
                self.emit(
                    &id,
                    "session.title",
                    json!({"session_id": id, "title": title}),
                );
            }
        }
        let provider = settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?
            .runtime();
        // The turn's rows start here (its receipt names them).
        let first_row = self.runtime.messages(&id).map(|m| m.len()).unwrap_or(0);
        let (record_provider, record_model) = (record.provider.clone(), record.model.clone());
        let options = self.turn_options(&id);
        // Images attached in the composer (`attachments: [{path, mime}]`, uploaded first through
        // `/api/chat/image-upload`): their bytes go with the prompt.
        // Images attached through `image.attach` go with this prompt too.
        let held: Vec<Value> = self
            .pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .map(|path| json!({"path": path}))
            .collect();
        let mut listed = params["attachments"].as_array().cloned().unwrap_or_default();
        listed.extend(held);
        let images = crate::attachments::image_bytes(&Value::Array(listed))
            .map_err(|e| (INVALID_PARAMS, e))?;
        let started = if let Some(instructions) = commands::review_instructions(&text) {
            // `/review`: the turn starts an independent reviewer subagent on the recent
            // conversation (Hermes's `start_review`), then the model presents its findings.
            let (goal, context) = self.review_task(&id, instructions);
            let arguments = json!({"goal": goal, "context": context});
            self.runtime.start_turn_with_call(
                &id,
                &provider,
                options,
                &text,
                "delegate_task",
                arguments.as_object().cloned().unwrap_or_default(),
            )
        } else if images.is_empty() {
            self.runtime.start_turn(&id, &provider, options, &text)
        } else {
            self.runtime
                .start_turn_with_images(&id, &provider, options, &text, images)
        };
        let mut turn = started.map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        self.subscribe(&id, outbox);
        let id_for_receipt = id.clone();
        // Only a request that named its admission is released by name after its receipt (a
        // generated one is never looked up, and its turn would wait forever).
        let named = self::admission_id(params).is_some();
        let started = (gated && named).then(|| {
            let (start, started) = tokio::sync::oneshot::channel();
            if let Ok(mut starts) = self.starts.lock() {
                starts.insert(admission_id.clone(), start);
            }
            started
        });
        self.set_live(&id, true);
        let this = self.clone();
        let outbox = outbox.clone();
        let admission = admission_id.clone();
        tokio::spawn(async move {
            if let Some(started) = started {
                let _ = started.await;
            }
            let mut stream = transcript::TurnStream::default();
            // message.start first: for a queued prompt it names the admission, and the page turns
            // that queue card into the user's bubble (patches/0001) before the session.info below
            // (whose `pending` no longer lists the prompt) retires the card. The page holds the
            // turn as live from `running: true` until `running: false`.
            this.emit(
                &id,
                "message.start",
                json!({"turn_id": admission, "admission_id": admission}),
            );
            this.emit(&id, "session.info", this.session_info(&id));
            // While the turn runs, a `running: true` heartbeat (Hermes's backend sends one, and
            // `gateway.ready` says it does) keeps the page holding the turn as live through quiet
            // stretches: a long think, a retry, a wait on the provider.
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(5));
            heartbeat.tick().await;
            // The model's context window, for the gauge (asked once per model, off this path).
            {
                let this = this.clone();
                let (provider, model) = (record_provider.clone(), record_model.clone());
                tokio::spawn(async move { this.context_length(&provider, &model).await });
            }
            // When the current model call began: the turn's start, then each tool result (tool
            // time isn't generation time). A call's output tokens over its time are its speed.
            let mut call_started = std::time::Instant::now();
            let mut last_input: Option<usize> = None;
            loop {
                let event = tokio::select! {
                    event = turn.events.recv() => event,
                    _ = heartbeat.tick() => {
                        this.emit(&id, "session.info", this.session_info(&id));
                        this.emit(&id, "session.usage", json!({"usage": this.usage_snapshot(&id, last_input)}));
                        continue;
                    }
                };
                let Some(event) = event else {
                    this.end_live(&id);
                    break;
                };
                match event {
                    RuntimeEvent::Agent(AgentEvent::Usage(usage)) => {
                        let output = usage.usage.output_tokens.unwrap_or(0).max(0) as u64;
                        // The provider's own timing when it reports one, else the call's.
                        let seconds = usage
                            .stats
                            .as_ref()
                            .and_then(|stats| stats.elapsed_ms)
                            .map_or_else(
                                || call_started.elapsed().as_secs_f64(),
                                |ms| ms as f64 / 1000.0,
                            );
                        this.note_generation(&id, output, seconds);
                        last_input = usage
                            .usage
                            .input_tokens
                            .map(|tokens| tokens.max(0) as usize);
                        call_started = std::time::Instant::now();
                        this.emit(
                            &id,
                            "session.usage",
                            json!({"usage": this.usage_snapshot(&id, last_input)}),
                        );
                    }
                    // A subagent's activity: the page's live subagent rows.
                    RuntimeEvent::Agent(AgentEvent::Subagent { kind, payload }) => {
                        this.emit(&id, kind, payload);
                    }
                    // Compression before a model call can take minutes on a local model; the
                    // page shows its "summarizing" indicator for the `compacting` /
                    // `compacted` pair, else the turn looks hung (Hermes:
                    // `tui_gateway/server.py` `_status_update`, `agent/conversation_compression.py`).
                    RuntimeEvent::Agent(AgentEvent::Compacting) => {
                        this.emit(&id, "status.update", json!({"kind": "compacting", "text": COMPACTING_TEXT}));
                    }
                    RuntimeEvent::Agent(AgentEvent::Compacted { .. }) => {
                        this.emit(&id, "status.update", json!({"kind": "compacted", "text": COMPACTED_TEXT}));
                    }
                    RuntimeEvent::Agent(AgentEvent::CompactionFailed(error)) => {
                        let text = match error {
                            Some(error) => format!("Context compression failed ({error}); continuing with the full conversation."),
                            None => "Context compression couldn't shorten the conversation; continuing.".to_string(),
                        };
                        this.emit(&id, "status.update", json!({"kind": "compacted", "text": text}));
                    }
                    RuntimeEvent::Agent(event) => {
                        if matches!(&event, AgentEvent::Message(m) if m.is_tool_response()) {
                            call_started = std::time::Instant::now();
                        }
                        for (name, payload) in stream.agent_event(event) {
                            this.emit(&id, name, payload);
                        }
                    }
                    // A steer the turn ended before reading: it runs next, as Hermes does.
                    RuntimeEvent::SteerUndelivered(text) => {
                        this.enqueue(&id, json!({"text": text, "shown": true}), true);
                    }
                    RuntimeEvent::TurnFinished(status) => {
                        let (mut status, mut error) = match status {
                            TurnStatus::Completed => ("complete", None),
                            TurnStatus::Cancelled => ("interrupted", None),
                            TurnStatus::Failed(error) => ("error", Some(error)),
                        };
                        let stored = this.runtime.messages(&id).unwrap_or_default();
                        // A turn that ended on a provider error carries it as the conversation's
                        // last message (ops::ExitOnError yields it). The page's error card keys
                        // off the failure status, so the turn is reported like Hermes's failed
                        // turn result: status "error", the provider's text, and the structured
                        // descriptor the card names the fix from.
                        let failure = (status == "complete")
                            .then(|| crate::transcript::message_error(&stored))
                            .flatten();
                        if let Some((_kind, text)) = &failure {
                            status = "error";
                            error = Some(text.clone());
                        }
                        let surface = failure.map(|(kind, text)| {
                            let (provider, model) = this
                                .runtime
                                .store()
                                .session(&id)
                                .ok()
                                .flatten()
                                .map(|record| (record.provider, record.model))
                                .unwrap_or_default();
                            crate::transcript::error_surface(kind, &text, &provider, &model)
                        });
                        // A turn that also streamed output keeps it: the text is the user's, not
                        // the error string (Hermes's partial failure settle).
                        let partial = status == "error" && !stream.text().trim().is_empty();
                        this.emit(
                            &id,
                            "message.complete",
                            json!({
                                "text": stream.text(),
                                "status": status,
                                "error": error,
                                "error_surface": surface,
                                "partial": partial,
                                // The rows this turn wrote, so the page can restore or edit them.
                                "persisted_turn": transcript::persisted_turn(&stored, first_row, &admission),
                                "usage": this.usage_snapshot(&id, last_input),
                            }),
                        );
                        // With a prompt queued, the session stays live into its turn: a
                        // `running: false` in between would end the turn on the page.
                        if !this.has_queued(&id) {
                            this.end_live(&id);
                        }
                        if let Some((Some((provider, model, effort)), opening)) = titling {
                            if status == "complete" {
                                this.title_session(&id, &provider, &model, effort.as_deref(), &opening).await;
                            }
                        }
                        break;
                    }
                }
            }
            this.apply_pending_route(&id);
            this.run_queued(&id, &outbox);
        });
        // The canonical protocol's admission receipt: which session took the prompt, under the
        // id the page gave this send (it drops its optimistic bubble on any other id or status).
        Ok(json!({
            "status": "started",
            "admission_id": admission_id,
            "ref": {"session_id": id_for_receipt},
        }))
    }

    /// Starts the session's next queued prompt, once its turn has finished.
    fn run_queued(self: &Arc<Self>, id: &str, outbox: &Outbox) {
        let next = self
            .queued
            .lock()
            .ok()
            .and_then(|mut queued| queued.get_mut(id).and_then(|q| q.pop_front()));
        let Some(params) = next else {
            return;
        };
        let settings = Settings::load(&self.data_dir);
        if let Err((_, error)) = self.clone().submit(&params, &settings, outbox, false) {
            tracing::warn!("queued prompt in {id}: {error}");
            self.emit(
                id,
                "message.complete",
                json!({"text": "", "status": "error", "error": error}),
            );
            self.end_live(id);
        }
    }

    /// Whether prompts wait behind the session's turn.
    fn has_queued(&self, id: &str) -> bool {
        self.queued
            .lock()
            .is_ok_and(|queued| queued.get(id).is_some_and(|q| !q.is_empty()))
    }

    /// Marks the session as having a turn in flight: from the start of a turn through any queued
    /// prompts that follow it. The page hears `running: false` only when that ends.
    fn set_live(&self, id: &str, live: bool) {
        if let Ok(mut turns) = self.live_turns.lock() {
            if live {
                turns.insert(id.to_string());
            } else {
                turns.remove(id);
            }
        }
    }

    /// The turn (and its queue) is over: the page settles it.
    fn end_live(&self, id: &str) {
        self.set_live(id, false);
        let mut info = self.session_info(id);
        info["running"] = json!(false);
        self.emit(id, "session.info", info);
    }

    /// Hermes's auto-title: the title model names the session from its opening message, and the
    /// page hears the new title through `session.info`.
    async fn title_session(
        &self,
        id: &str,
        provider: &hermes_runtime::ProviderConfig,
        model: &str,
        effort: Option<&str>,
        opening: &str,
    ) {
        match hermes_runtime::generate_title_with_effort(provider, model, effort, opening).await {
            Ok(Some(title)) => {
                let _ = self.runtime.store().set_title(id, &title);
                self.emit(
                    id,
                    "session.title",
                    json!({"session_id": id, "title": title}),
                );
                self.emit(id, "session.info", self.session_info(id));
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("titling {id}: {error:#}"),
        }
    }

    /// Sends a session event to the socket following that session.
    fn emit(&self, session_id: &str, event: &str, payload: Value) {
        self.hub.emit(session_id, event, payload);
    }

    /// The app's data folder.
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// The file a page path names, relative to the session's folder when it names a session.
    pub fn file_path(&self, path: &str, session_id: Option<&str>) -> PathBuf {
        let cwd = session_id
            .filter(|id| !id.is_empty())
            .and_then(|id| self.runtime.store().session(id).ok().flatten())
            .map(|record| PathBuf::from(record.cwd));
        crate::files::resolve(path, cwd.as_deref())
    }

    /// Whether `ticket` was issued for `file` (`/api/files/ticket`).
    pub fn ticket_allows(&self, ticket: &str, file: &std::path::Path) -> bool {
        self.file_tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(ticket)
            .is_some_and(|issued| issued == file)
    }

    /// REST routes this backend serves: `(status, body)`, or `None` for Hermes's 404.
    pub async fn rest(
        &self,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        if (method, path) == ("GET", "/api/profiles/sessions") {
            return Some((200, self.session_list(query)));
        }
        // Settings ▸ Providers ▸ Custom endpoints (`endpoints.rs`).
        if let Some(rest) = path.strip_prefix("/api/providers/custom-endpoints") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let answer = match (method, rest.trim_start_matches('/')) {
                ("GET", "") => (200, crate::endpoints::list(&self.data_dir)),
                ("POST", "") => crate::endpoints::save(&self.data_dir, &body),
                ("POST", "validate") => crate::endpoints::validate(&self.data_dir, &body).await,
                ("POST", id) if id.ends_with("/activate") => {
                    let id = crate::rest::percent_decode(id.trim_end_matches("/activate"));
                    crate::endpoints::activate(&self.data_dir, &id).await
                }
                ("DELETE", id) if !id.is_empty() => {
                    let id = crate::rest::percent_decode(id);
                    crate::endpoints::delete(&self.data_dir, &id).await
                }
                _ => return None,
            };
            if method != "GET" {
                // A changed endpoint may have a new window or models.
                if let Ok(mut lengths) = self.context_lengths.lock() {
                    lengths.clear();
                }
            }
            return Some(answer);
        }
        // The Star map: learned skills and memories, viewed, edited and deleted (`learning.rs`).
        if let Some(route) = path.strip_prefix("/api/learning/") {
            let (skills, memory) = (self.runtime.skills(), self.runtime.memory());
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let id = crate::rest::query_param(query, "id")
                .or_else(|| body["id"].as_str().map(str::to_string))
                .unwrap_or_default();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let (method, route) = (method.to_string(), route.to_string());
            return tokio::task::spawn_blocking(move || match (method.as_str(), route.as_str()) {
                ("GET", "graph") => Some((200, crate::learning::graph(&skills, &memory))),
                ("GET", "node") => Some(crate::learning::detail(&skills, &memory, &id)),
                ("DELETE", "node") => Some(crate::learning::delete(&skills, &memory, &id)),
                ("PUT", "node") => Some(crate::learning::edit(&skills, &memory, &id, &content)),
                _ => None,
            })
            .await
            .ok()
            .flatten();
        }
        // Capabilities ▸ Tools (`toolsets.rs`).
        if let Some(route) = path.strip_prefix("/api/tools/") {
            let off = || crate::toolsets::disabled_toolsets(&self.rest.config_value(crate::toolsets::CONFIG_KEY));
            return match (method, route) {
                ("GET", "toolsets") => Some((200, crate::toolsets::list(&off()))),
                ("GET", "terminal/backends") => Some((200, crate::toolsets::terminal_backends())),
                ("GET", rest) if rest.starts_with("toolsets/") && rest.ends_with("/config") => {
                    let name = rest.trim_start_matches("toolsets/").trim_end_matches("/config");
                    Some((200, crate::toolsets::config(&crate::rest::percent_decode(name))))
                }
                ("PUT", rest) if rest.starts_with("toolsets/") && !rest[9..].contains('/') => {
                    let name = crate::rest::percent_decode(&rest[9..]);
                    let body: Value = serde_json::from_slice(body).unwrap_or_default();
                    let enabled = body["enabled"].as_bool().unwrap_or(true);
                    match crate::toolsets::switch(&off(), &name, enabled) {
                        Some(next) => {
                            self.rest.set_config_value(crate::toolsets::CONFIG_KEY, json!(next));
                            self.runtime.set_disabled_tools(crate::toolsets::disabled_tools(&next));
                            Some((200, json!({"ok": true, "name": name, "enabled": enabled})))
                        }
                        None => Some((404, json!({"detail": format!("No toolset {name}.")}))),
                    }
                }
                _ => None,
            };
        }
        // The composer's attachments (`attachments.rs`).
        if (method, path) == ("POST", "/api/chat/image-upload") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let data_dir = self.data_dir.clone();
            return tokio::task::spawn_blocking(move || {
                crate::attachments::upload_image(&data_dir, &body)
            })
            .await
            .ok();
        }
        // The side panel's file preview and spot editor (`files.rs`). Paths are relative to the
        // session's folder when the page names a session.
        let param = |name: &str| crate::rest::query_param(query, name);
        if (method, path) == ("GET", "/api/fs/read-data-url") {
            let file = self.file_path(&param("path").unwrap_or_default(), param("session_id").as_deref());
            if crate::files::is_sensitive(&file) {
                return Some((403, json!({"detail": "That file holds credentials and isn't shown."})));
            }
            let file = file.display().to_string();
            return tokio::task::spawn_blocking(move || crate::attachments::read_data_url(&file))
                .await
                .ok();
        }
        if (method, path) == ("GET", "/api/fs/read-text") {
            let file = self.file_path(&param("path").unwrap_or_default(), param("session_id").as_deref());
            return tokio::task::spawn_blocking(move || crate::files::read_text(&file))
                .await
                .ok();
        }
        if (method, path) == ("GET", "/api/fs/git-root") {
            let file = self.file_path(&param("path").unwrap_or_default(), None);
            return tokio::task::spawn_blocking(move || (200, crate::files::git_root(&file)))
                .await
                .ok();
        }
        if method == "POST"
            && matches!(path, "/api/fs/write-text" | "/api/files/mkdir" | "/api/files/ticket")
        {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let named = body["path"].as_str().unwrap_or_default();
            if named.trim().is_empty() {
                return Some((400, json!({"detail": "path is required"})));
            }
            let file = self.file_path(named, body["session_id"].as_str());
            return match path {
                "/api/fs/write-text" => {
                    let content = body["content"].as_str().unwrap_or_default().to_string();
                    tokio::task::spawn_blocking(move || crate::files::write_text(&file, &content))
                        .await
                        .ok()
                }
                "/api/files/mkdir" => {
                    tokio::task::spawn_blocking(move || crate::files::mkdir(&file)).await.ok()
                }
                // A download link or media element can't send headers: the page asks for a
                // ticket bound to the file first (Hermes's `issue_file_ticket`).
                _ => {
                    let ticket = uuid::Uuid::new_v4().simple().to_string();
                    let mut tickets = self.file_tickets.lock().unwrap_or_else(|e| e.into_inner());
                    // (Old tickets go: a page makes a few per preview.)
                    if tickets.len() > 512 {
                        tickets.clear();
                    }
                    tickets.insert(ticket.clone(), file);
                    Some((200, json!({"ticket": ticket})))
                }
            };
        }
        // Sign-in providers (Nous Portal, Claude, Codex): none here.
        // The profiles window's SOUL.md editor (profiles.rs): the default profile's persona, read
        // and written in the data folder. The multi-profile routes (create, rename, delete,
        // export, import) aren't ported — a single-profile backend has no second profile.
        if let Some(name) = path
            .strip_prefix("/api/profiles/")
            .and_then(|rest| rest.strip_suffix("/soul"))
        {
            let name = crate::rest::percent_decode(name);
            if method == "GET" {
                let data_dir = self.data_dir.clone();
                return tokio::task::spawn_blocking(move || crate::profiles::soul(&data_dir, &name))
                    .await
                    .ok();
            }
            if method == "PUT" {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                let content = body["content"].as_str().unwrap_or_default().to_string();
                let data_dir = self.data_dir.clone();
                return tokio::task::spawn_blocking(move || {
                    crate::profiles::save_soul(&data_dir, &name, &content)
                })
                .await
                .ok();
            }
        }
        if (method, path) == ("GET", "/api/providers/oauth") {
            return Some((200, json!({"providers": []})));
        }
        // Hermes's catalog of provider keys (`.env`): none here, an endpoint keeps its own key.
        // The Providers page waits for it before showing any view.
        if (method, path) == ("GET", "/api/env") {
            return Some((200, json!({})));
        }
        // The sidebar's search: the sessions whose messages match, one hit each, newest first
        // (Hermes's `/api/sessions/search`, over the store's full-text index).
        if (method, path) == ("GET", "/api/sessions/search") {
            let text = crate::rest::query_param(query, "q").unwrap_or_default();
            let store = self.runtime.store();
            // (The store matches the listed roles only.)
            let roles = ["user".to_string(), "assistant".to_string()];
            let hits = if text.trim().is_empty() {
                Vec::new()
            } else {
                store
                    .search(&hermes_store::SearchQuery {
                        query: text.trim(),
                        roles: &roles,
                        exclude_sessions: &[],
                        after_ms: None,
                        before_ms: None,
                        sort: hermes_store::SearchSort::Newest,
                        limit: 200,
                    })
                    .unwrap_or_default()
            };
            let mut seen = std::collections::HashSet::new();
            let results: Vec<Value> = hits
                .into_iter()
                .filter(|hit| seen.insert(hit.session_id.clone()))
                .filter_map(|hit| {
                    let record = store.session(&hit.session_id).ok().flatten()?;
                    Some(json!({
                        "session_id": hit.session_id,
                        "lineage_root": hit.session_id,
                        "snippet": hit.snippet,
                        "role": hit.role,
                        "model": record.model,
                        "source": "desktop",
                        "title": record.title,
                        "session_started": record.created_at as f64 / 1000.,
                        "last_active": record.updated_at as f64 / 1000.,
                    }))
                })
                .take(50)
                .collect();
            return Some((200, json!({"results": results})));
        }
        // Settings ▸ Advanced ▸ Logs: this run's log (the app logs to its own log file).
        if (method, path) == ("GET", "/api/logs") {
            let lines = crate::rest::query_param(query, "lines")
                .and_then(|l| l.parse().ok())
                .unwrap_or(200usize);
            let file = self.data_dir.join("logs").join("desktop.log");
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            let tail: Vec<&str> = all[all.len().saturating_sub(lines)..].to_vec();
            return Some((
                200,
                json!({"file": file.display().to_string(), "lines": tail}),
            ));
        }
        // Command Center ▸ Maintenance: the curator and the memory files.
        match (method, path) {
            ("GET", "/api/curator") => {
                return Some((
                    200,
                    crate::maintenance::curator_status(&self.runtime.skills()),
                ));
            }
            ("PUT", "/api/curator/paused") => {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                let paused = body["paused"] == true;
                return Some((
                    200,
                    crate::maintenance::set_curator_paused(&self.runtime.skills(), paused),
                ));
            }
            ("POST", "/api/curator/run") => {
                let started =
                    crate::maintenance::run_curator(self.runtime.clone(), self.actions.clone());
                return Some((
                    200,
                    json!({"ok": true, "pid": 0, "name": crate::maintenance::CURATOR_RUN,
                           "already_running": !started}),
                ));
            }
            ("GET", "/api/memory") => {
                return Some((200, crate::maintenance::memory_status(&self.data_dir)));
            }
            ("POST", "/api/memory/reset") => {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                return Some(crate::maintenance::reset_memory(&self.data_dir, &body));
            }
            _ => {}
        }
        if let Some(name) = path
            .strip_prefix("/api/actions/")
            .filter(|_| method == "GET")
        {
            let name = crate::rest::percent_decode(name.trim_end_matches("/status"));
            let limit = crate::rest::query_param(query, "lines")
                .and_then(|l| l.parse().ok())
                .unwrap_or(200);
            return Some((200, self.actions.status(&name, limit)));
        }
        if path == "/api/skills" || path.starts_with("/api/skills/") {
            let store = self.runtime.skills();
            return crate::skills::route(&store, &self.rest, method, path, query, body);
        }
        // Settings ▸ Model: the default model (the hermes-rs settings') and the side jobs' models
        // (`aux_models.rs`).
        if (method, path) == ("GET", "/api/model/auxiliary") {
            return Some((200, crate::aux_models::list(&self.data_dir)));
        }
        if (method, path) == ("POST", "/api/model/set") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            match body["scope"].as_str() {
                None | Some("main") => {}
                Some("auxiliary") => return Some(crate::aux_models::assign(&self.data_dir, &body)),
                Some(scope) => {
                    return Some((400, json!({"detail": format!("Unknown model scope {scope}")})));
                }
            }
            let (Some(provider), Some(model)) = (body["provider"].as_str(), body["model"].as_str())
            else {
                return Some((400, json!({"detail": "provider and model are required"})));
            };
            if Settings::load(&self.data_dir).provider(provider).is_none() {
                return Some((
                    400,
                    json!({"detail": format!("Unknown provider {provider}")}),
                ));
            }
            return Some(
                match crate::providers::save_default(&self.data_dir, provider, model) {
                    Ok(()) => (
                        200,
                        json!({"ok": true, "provider": provider, "model": model}),
                    ),
                    Err(error) => (500, json!({"detail": format!("{error:#}")})),
                },
            );
        }
        if (method, path) == ("PUT", "/api/config") {
            let answer = self.rest.handle(method, path, query, body);
            self.apply_settings();
            return answer;
        }
        // A `file:` link opened from the page (patches/0013, `open_file.rs`): its default app,
        // or Explorer with it selected when it's a program.
        if (method, path) == ("POST", "/api/hermes-rs/open-file") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let link = body["url"].as_str().unwrap_or_default().to_string();
            let done = tokio::task::spawn_blocking(move || {
                crate::open_file::plan(&link).and_then(|open| {
                    crate::open_file::run(&open)?;
                    Ok(matches!(open, crate::open_file::Open::Reveal(_)))
                })
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            return Some(match done {
                Ok(revealed) => (200, json!({"ok": true, "revealed": revealed})),
                Err(error) => (400, json!({"detail": error})),
            });
        }
        // "Choose a folder" (patches/0002): Windows' own folder dialog, opened by this process,
        // which is the desktop window's. Cancelling answers no paths.
        if (method, path) == ("POST", "/api/hermes-rs/select-folder") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let title = body["title"].as_str().unwrap_or_default().to_string();
            let start = body["defaultPath"].as_str().unwrap_or_default().to_string();
            let picked = tokio::task::spawn_blocking(move || {
                let mut dialog = rfd::FileDialog::new();
                if !title.is_empty() {
                    dialog = dialog.set_title(&title);
                }
                if !start.is_empty() && Path::new(&start).is_dir() {
                    dialog = dialog.set_directory(&start);
                }
                dialog.pick_folder()
            })
            .await
            .ok()
            .flatten();
            let paths: Vec<String> = picked
                .map(|p| p.display().to_string())
                .into_iter()
                .collect();
            return Some((200, json!({"paths": paths})));
        }
        // "Save as" for `/save` (patches/0008): Windows' own dialog, then the file written here.
        if (method, path) == ("POST", "/api/hermes-rs/save-file") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let title = body["title"].as_str().unwrap_or_default().to_string();
            let default_path = body["defaultPath"].as_str().unwrap_or_default().to_string();
            let filters: Vec<(String, Vec<String>)> = body["filters"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    let name = f["name"].as_str().unwrap_or("File").to_string();
                    let extensions = f["extensions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    (name, extensions)
                })
                .collect();
            let saved = tokio::task::spawn_blocking(move || {
                let mut dialog = rfd::FileDialog::new();
                if !title.is_empty() {
                    dialog = dialog.set_title(&title);
                }
                let default = Path::new(&default_path);
                if let Some(name) = default.file_name() {
                    dialog = dialog.set_file_name(name.to_string_lossy());
                }
                if let Some(dir) = default.parent().filter(|d| d.is_dir()) {
                    dialog = dialog.set_directory(dir);
                }
                for (name, extensions) in &filters {
                    dialog = dialog.add_filter(name, extensions);
                }
                let path = dialog.save_file()?;
                Some(std::fs::write(&path, content).map(|()| path))
            })
            .await
            .ok()
            .flatten();
            return Some((
                200,
                match saved {
                    None => json!({"saved": false}),
                    Some(Ok(path)) => json!({"saved": true, "path": path.display().to_string()}),
                    Some(Err(error)) => json!({"saved": false, "error": error.to_string()}),
                },
            ));
        }
        // The file browser and the web folder picker (`/api/fs/list`).
        if (method, path) == ("GET", "/api/fs/list") {
            let folder = crate::rest::query_param(query, "path").unwrap_or_default();
            let listing = tokio::task::spawn_blocking(move || crate::paths::list_dir(&folder))
                .await
                .unwrap_or_else(|_| json!({"entries": [], "error": "read-error"}));
            return Some((200, listing));
        }
        // The coding rail and the review pane (`git_review.rs`; the folder is `path`).
        if path.starts_with("/api/git/") {
            let (method, path) = (method.to_string(), path.to_string());
            let (query, body) = (query.map(str::to_string), body.to_vec());
            return tokio::task::spawn_blocking(move || {
                crate::git_review::route(&method, &path, query.as_deref(), &body)
            })
            .await
            .ok()
            .flatten();
        }
        if (method, path) == ("GET", "/api/model/options") {
            let settings = Settings::load(&self.data_dir);
            let session = crate::rest::query_param(query, "session_id");
            return Some((200, self.model_options(&settings, session.as_deref()).await));
        }
        if let Some(id) = path
            .strip_prefix("/api/sessions/")
            .and_then(|rest| rest.strip_suffix("/messages"))
        {
            if method == "GET" {
                return Some(self.stored_messages(&crate::rest::percent_decode(id), query));
            }
        }
        if let Some(id) = path.strip_prefix("/api/sessions/") {
            let id = crate::rest::percent_decode(id);
            if !id.contains('/') && id != "search" {
                return self.session_route(method, &id, body).await;
            }
        }
        self.rest.handle(method, path, query, body)
    }

    /// `/api/sessions/{id}`: a session's row, its sidebar flags (`PATCH`), deleting it.
    async fn session_route(&self, method: &str, id: &str, body: &[u8]) -> Option<(u16, Value)> {
        let store = self.runtime.store();
        let Some(record) = store.session(id).ok().flatten() else {
            return Some((404, json!({"detail": format!("Session {id} not found")})));
        };
        match method {
            "GET" => {
                let count = store.message_count(id).unwrap_or(0);
                Some((200, self.session_row(&record, count)))
            }
            "PATCH" => {
                let changes: Value = serde_json::from_slice(body).unwrap_or_default();
                if let Some(archived) = changes["archived"].as_bool()
                    && let Err((_, error)) = self.set_archived(id, archived)
                {
                    return Some((500, json!({"detail": error})));
                }
                if let Some(pinned) = changes["pinned"].as_bool() {
                    if let Err(error) = store.set_pinned(id, pinned) {
                        return Some((500, json!({"detail": format!("{error:#}")})));
                    }
                }
                if let Some(title) = changes["title"].as_str() {
                    let _ = store.set_title(id, title.trim());
                }
                // `unread` is the page's own bookkeeping; nothing to keep for it.
                Some((200, json!({"ok": true})))
            }
            "DELETE" => match self.runtime.delete_session(id).await {
                Ok(()) => {
                    self.archive.forget(id);
                    Some((200, json!({"ok": true})))
                }
                Err(error) => Some((500, json!({"detail": format!("{error:#}")}))),
            },
            _ => None,
        }
    }

    /// `/api/sessions/{id}/messages`: the stored transcript, a page of it (`SessionMessagesResponse`).
    fn stored_messages(&self, id: &str, query: Option<&str>) -> (u16, Value) {
        let messages = match self.runtime.messages(id) {
            Ok(messages) => transcript::messages(&messages),
            Err(error) => return (404, json!({"detail": format!("{error:#}")})),
        };
        let param = |name: &str| {
            crate::rest::query_param(query, name).and_then(|v| v.parse::<usize>().ok())
        };
        let limit = param("limit").unwrap_or(messages.len().max(1));
        let offset = param("offset").unwrap_or(0);
        let latest = crate::rest::query_param(query, "order").as_deref() == Some("latest");
        // `latest` pages count back from the newest message; the page still reads oldest first.
        let (start, end) = if latest {
            let end = messages.len().saturating_sub(offset);
            (end.saturating_sub(limit), end)
        } else {
            (
                offset.min(messages.len()),
                (offset + limit).min(messages.len()),
            )
        };
        let page: Vec<Value> = messages[start..end]
            .iter()
            .map(|m| {
                let mut row = m.clone();
                row["content"] = m["text"].clone();
                row
            })
            .collect();
        (
            200,
            json!({
                "session_id": id,
                "profile": "default",
                "pagination": {
                    "limit": limit,
                    "offset": offset,
                    "order": if latest { "latest" } else { "oldest" },
                    "returned": page.len(),
                },
                "messages": page,
            }),
        )
    }

    /// One session as the sidebar lists it (`SessionInfo`). `message_count` must be true: the page
    /// treats a resume that returns no messages for a row it counted as a lost transcript.
    fn session_row(&self, s: &hermes_store::SessionRecord, message_count: usize) -> Value {
        json!({
            "id": s.id,
            "title": s.title,
            "cwd": s.cwd,
            "model": s.model,
            "source": "desktop",
            "preview": s.title,
            "started_at": s.created_at as f64 / 1000.,
            "last_active": s.updated_at as f64 / 1000.,
            "ended_at": null,
            "message_count": message_count,
            "input_tokens": 0,
            "output_tokens": 0,
            "is_active": self.runtime.is_running(&s.id),
            "pinned": s.pinned,
            "archived": self.archive.is_archived(&s.id),
            "profile": "default",
            // The git identity a move recorded (the sidebar groups by the repo root); a session
            // that never moved carries none and groups by its cwd.
            "git_branch": self.workspaces.identity(&s.id).and_then(|(branch, _)| branch),
            "git_repo_root": self.workspaces.identity(&s.id).and_then(|(_, root)| root),
            // A branch is listed under the session it came from.
            "parent_session_id": self.branches.parent(&s.id),
        })
    }

    /// Every session the sidebar's project tree shows (archived ones aren't), newest first.
    fn session_rows(&self) -> Vec<Value> {
        let all = self.runtime.store().list_sessions(1000).unwrap_or_default();
        let counts = self.runtime.store().message_counts().unwrap_or_default();
        all.iter()
            .filter(|s| !self.archive.is_archived(&s.id))
            .map(|s| self.session_row(s, counts.get(&s.id).copied().unwrap_or(0)))
            .collect()
    }

    /// `projects.list`: the user's projects and the active one.
    fn project_list(&self) -> Value {
        let projects: Vec<Value> = self
            .runtime
            .store()
            .list_projects()
            .unwrap_or_default()
            .iter()
            .map(crate::projects::record)
            .collect();
        json!({"projects": projects, "active_id": self.rest.config_value("projects.active_id")})
    }

    /// `/api/profiles/sessions`: the sidebar's list (`PaginatedSessions`).
    fn session_list(&self, query: Option<&str>) -> Value {
        let param = |name: &str| {
            crate::rest::query_param(query, name).and_then(|v| v.parse::<usize>().ok())
        };
        let limit = param("limit").unwrap_or(50);
        let offset = param("offset").unwrap_or(0);
        // `archived=exclude` (the sidebar, default), `only` (Settings ▸ Sessions), `include`.
        let filter = crate::archive::Filter::parse(crate::rest::query_param(query, "archived").as_deref());
        let all: Vec<_> = self
            .runtime
            .store()
            .list_sessions(1000)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| filter.keeps(self.archive.is_archived(&s.id)))
            .collect();
        let counts = self.runtime.store().message_counts().unwrap_or_default();
        let sessions: Vec<Value> = all
            .iter()
            .skip(offset)
            .take(limit)
            .map(|s| self.session_row(s, counts.get(&s.id).copied().unwrap_or(0)))
            .collect();
        json!({
            "sessions": sessions,
            "total": all.len(),
            "limit": limit,
            "offset": offset,
            "profile_totals": {"default": all.len()},
        })
    }
}

/// The page's id for a `prompt.submit` (its admission id).
fn admission_id(params: &Value) -> Option<String> {
    params["submission_id"]
        .as_str()
        .or(params["input_id"].as_str())
        .map(str::to_string)
}

fn session_id(params: &Value) -> Result<String, (i64, String)> {
    params["session_id"]
        .as_str()
        .map(str::to_string)
        .ok_or((INVALID_PARAMS, "session_id is required".to_string()))
}

fn send(outbox: &Outbox, frame: Value) {
    let _ = outbox.send(frame.to_string());
}
