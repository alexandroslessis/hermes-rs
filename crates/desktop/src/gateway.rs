//! Hermes's `tui_gateway` protocol over the hermes-rs runtime: newline-delimited JSON-RPC 2.0
//! on a WebSocket, `gateway.ready` right after connecting, agent progress as `event`
//! notifications (`tui_gateway/ws.py`, `tui_gateway/server.py`). Result shapes follow the
//! contract the frontend is generated from (`apps/shared/src/gateway-contract.openrpc.json`).
//! A method this backend doesn't implement answers JSON-RPC's -32601, which the frontend treats
//! as "not supported here" and hides.

use std::path::PathBuf;
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

type RpcResult = Result<Value, (i64, String)>;

/// Hermes fences turn control and prompt answers by an execution generation. This backend runs
/// one agent per session in-process, so a single generation serves every turn.
pub const EXECUTION_GENERATION: u64 = 0;

/// The window assumed for a model whose provider doesn't list one (Hermes's
/// `DEFAULT_FALLBACK_CONTEXT`).
const FALLBACK_CONTEXT: usize = 256_000;

pub struct Gateway {
    pub runtime: Arc<Runtime>,
    pub rest: crate::rest::Rest,
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
    context_lengths: std::sync::Mutex<std::collections::HashMap<String, Option<usize>>>,
    /// Session id → output tokens generated and the seconds the model calls took, this run
    /// (the status bar's tokens per second).
    generation: std::sync::Mutex<std::collections::HashMap<String, (u64, f64)>>,
}

impl Gateway {
    pub fn new(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf) -> Self {
        let gateway = Self {
            runtime,
            rest: crate::rest::Rest::new(data_dir.clone()),
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
            generation: Default::default(),
        };
        // The runtime keeps the approval policy in memory: Settings' choice applies from the start.
        gateway.apply_approvals_mode(&gateway.rest.config_value("approvals.mode"));
        gateway
    }

    /// Hermes's `approvals.mode` on the runtime's policy: manual asks, smart has a reviewer model
    /// judge first, off runs flagged commands (hardline ones never run).
    fn apply_approvals_mode(&self, mode: &Value) {
        let approvals = self.runtime.approvals();
        let mode = mode.as_str().unwrap_or("manual");
        approvals.set_ask(mode != "off");
        approvals.set_smart(mode == "smart");
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
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
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
                        if key == "approvals.mode" {
                            self.apply_approvals_mode(&value);
                        }
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
            "slash.exec" => commands::exec(
                &self.runtime,
                params["command"].as_str().unwrap_or_default(),
            )
            .ok_or((
                SERVER_ERROR,
                "not a quick/plugin/bundle/skill command".to_string(),
            )),
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
            "session.mutate" => self.mutate(params, &settings),
            "session.delete" => {
                let id = session_id(params)?;
                self.runtime
                    .delete_session(&id)
                    .await
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                Ok(json!({"deleted": id}))
            }
            // Nothing to release: a session lives in the store, not in a worker.
            "session.close" => Ok(json!({"closed": true})),
            "session.create" => self.create_session(params, &settings, outbox),
            "session.resume" | "session.history" => self.resume_session(params, outbox),
            "prompt.submit" => self.submit(params, &settings, outbox, true),
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
            // hermes-rs keeps no registry of a turn's subagents yet: none to list.
            "subagent.list" => Ok(json!({"subagents": [], "delegations": []})),
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
            let models: Vec<String> = if provider.ready() {
                provider
                    .runtime()
                    .list_models()
                    .await
                    .map(|models| models.into_iter().map(|m| m.id).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
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
                    .and_then(|lengths| lengths.get(&key).copied().flatten())
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
    async fn context_length(&self, provider: &str, model: &str) -> usize {
        let key = format!("{provider}\n{model}");
        if let Some(known) = self
            .context_lengths
            .lock()
            .ok()
            .and_then(|lengths| lengths.get(&key).copied())
        {
            return known.unwrap_or(FALLBACK_CONTEXT);
        }
        let settings = Settings::load(&self.data_dir);
        let listed = match settings.provider(provider) {
            Some(provider) if provider.ready() => provider.runtime().list_models().await.ok(),
            _ => None,
        };
        // A provider that didn't answer is asked again next time; one that answered without the
        // model's window is not.
        let Some(models) = listed else {
            return FALLBACK_CONTEXT;
        };
        let length = models
            .into_iter()
            .find(|m| m.id == model)
            .and_then(|m| m.context_length);
        if let Ok(mut lengths) = self.context_lengths.lock() {
            lengths.insert(key, length);
        }
        length.unwrap_or(FALLBACK_CONTEXT)
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
    fn mutate(&self, params: &Value, settings: &Settings) -> RpcResult {
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
                    return Err((SERVER_ERROR, "session busy".to_string()));
                }
                let kept = self
                    .runtime
                    .rewind(&id, transcript::kept_before_row(row as usize))
                    .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
                json!({"status": "applied", "target_message_id": row, "message_count": kept.len()})
            }
            "archive" => {
                return Err((
                    SERVER_ERROR,
                    "Archiving isn't available in hermes-rs; delete the session instead.".into(),
                ));
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

    /// Renames a session; the page's lists hear it through Hermes's `session.title` event.
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
        self.subscribe(&record.id, outbox);
        Ok(json!({
            "session_id": record.id,
            "stored_session_id": record.id,
            "message_count": 0,
            "messages": [],
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
                ))
            });
            let fallback = settings
                .provider(&record.provider)
                .map(|p| (p.runtime(), record.model.clone()));
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
        let mut turn = self
            .runtime
            .start_turn(
                &id,
                &provider,
                TurnOptions {
                    // Only a picked effort is sent; unset leaves the provider's default (as Hermes).
                    effort: self
                        .efforts
                        .lock()
                        .ok()
                        .and_then(|efforts| efforts.get(&id).cloned())
                        .filter(|e| e != "none"),
                    ..TurnOptions::default()
                },
                &text,
            )
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        self.subscribe(&id, outbox);
        let id_for_receipt = id.clone();
        let started = gated.then(|| {
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
                        this.note_generation(&id, output, call_started.elapsed().as_secs_f64());
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
                        let (status, error) = match status {
                            TurnStatus::Completed => ("complete", None),
                            TurnStatus::Cancelled => ("interrupted", None),
                            TurnStatus::Failed(error) => ("error", Some(error)),
                        };
                        let stored = this.runtime.messages(&id).unwrap_or_default();
                        this.emit(
                            &id,
                            "message.complete",
                            json!({
                                "text": stream.text(),
                                "status": status,
                                "error": error,
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
                        if let Some((Some((provider, model)), opening)) = titling {
                            if status == "complete" {
                                this.title_session(&id, &provider, &model, &opening).await;
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
        opening: &str,
    ) {
        match hermes_runtime::generate_title(provider, model, opening).await {
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
        // Settings ▸ Model ▸ Main model: the default model (the hermes-rs settings').
        if (method, path) == ("GET", "/api/model/auxiliary") {
            let settings = Settings::load(&self.data_dir);
            return Some((
                200,
                json!({
                    "main": {"provider": settings.provider, "model": settings.model},
                    "tasks": [],
                }),
            ));
        }
        if (method, path) == ("POST", "/api/model/set") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let (Some(provider), Some(model)) = (body["provider"].as_str(), body["model"].as_str())
            else {
                return Some((400, json!({"detail": "provider and model are required"})));
            };
            if body["scope"].as_str().is_some_and(|scope| scope != "main") {
                return Some((
                    400,
                    json!({"detail": "Only the main model can be set in hermes-rs"}),
                ));
            }
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
            self.apply_approvals_mode(&self.rest.config_value("approvals.mode"));
            return answer;
        }
        // The coding rail's git reads (the folder is `path`; a non-repository reads as null).
        if method == "GET" && matches!(path, "/api/git/status" | "/api/git/review/rev-parse") {
            let folder = PathBuf::from(crate::rest::query_param(query, "path").unwrap_or_default());
            let reference = crate::rest::query_param(query, "ref");
            let status = path == "/api/git/status";
            let answer = tokio::task::spawn_blocking(move || {
                if status {
                    crate::git::repo_status(&folder).unwrap_or(Value::Null)
                } else {
                    json!({"sha": crate::git::rev_parse(&folder, reference.as_deref())})
                }
            })
            .await
            .unwrap_or(Value::Null);
            return Some((200, answer));
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
                let count = store.message_counts().ok().and_then(|c| c.get(id).copied());
                Some((200, self.session_row(&record, count.unwrap_or(0))))
            }
            "PATCH" => {
                let changes: Value = serde_json::from_slice(body).unwrap_or_default();
                if changes["archived"] == true {
                    return Some((
                        400,
                        json!({"detail": "Archiving isn't available in hermes-rs; delete the session instead."}),
                    ));
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
                Ok(()) => Some((200, json!({"ok": true}))),
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
            "profile": "default",
        })
    }

    /// Every session as the sidebar lists it, newest first.
    fn session_rows(&self) -> Vec<Value> {
        let all = self.runtime.store().list_sessions(1000).unwrap_or_default();
        let counts = self.runtime.store().message_counts().unwrap_or_default();
        all.iter()
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
        let all = self.runtime.store().list_sessions(1000).unwrap_or_default();
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
