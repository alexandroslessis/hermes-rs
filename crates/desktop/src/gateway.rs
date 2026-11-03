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
use hermes_runtime::{Runtime, RuntimeEvent, TurnOptions, TurnStatus};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::hub::{Hub, Outbox};
use crate::providers::Settings;
use crate::transcript;

/// JSON-RPC's "method not found".
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_ERROR: i64 = -32000;

type RpcResult = Result<Value, (i64, String)>;

pub struct Gateway {
    pub runtime: Arc<Runtime>,
    pub rest: crate::rest::Rest,
    data_dir: PathBuf,
    /// Changes when the process restarts, so reconnecting clients reset their replay state.
    replay_epoch: String,
    /// Event routes and open questions, shared with the runtime's bridge.
    hub: Arc<Hub>,
}

impl Gateway {
    pub fn new(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf) -> Self {
        Self {
            runtime,
            rest: crate::rest::Rest::new(data_dir.clone()),
            data_dir,
            replay_epoch: uuid::Uuid::new_v4().to_string(),
            hub,
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
        let result = self.clone().dispatch(method, &params, &outbox).await;
        let Some(id) = id else {
            return;
        };
        let reply = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                if code == METHOD_NOT_FOUND {
                    tracing::info!(target: "hermes_desktop::missing", "RPC {method} {params}");
                } else {
                    tracing::warn!("RPC {method} failed: {message}");
                }
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        send(&outbox, reply);
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
            "projects.tree" => Ok(json!({"projects": []})),
            "session.active_list" => Ok(json!({"sessions": self.active_sessions()})),
            "model.options" => Ok(self.model_options(&settings).await),
            "config.get" => Ok(json!({
                "value": match params["key"].as_str() {
                    Some("approvals.mode") => "manual",
                    _ => "",
                },
                "model": settings.model,
                "provider": settings.provider,
            })),
            "session.create" => self.create_session(params, &settings, outbox),
            "session.resume" | "session.history" => self.resume_session(params, outbox),
            "prompt.submit" => self.submit(params, &settings, outbox),
            // The page's answer to an approval card (`choice`) or a clarify question (`answer`),
            // named by the prompt id it was shown with.
            "approval.respond" | "clarify.respond" => {
                let prompt_id = params["prompt_id"]
                    .as_str()
                    .or(params["request_id"].as_str())
                    .ok_or((INVALID_PARAMS, "prompt_id is required".to_string()))?;
                let field = if method == "approval.respond" { "choice" } else { "answer" };
                let answer = params[field].as_str().unwrap_or_default().to_string();
                if self.hub.answer(prompt_id, answer) {
                    Ok(json!({"resolved": 1}))
                } else {
                    Err((SERVER_ERROR, format!("No pending {} request", method.split('.').next().unwrap_or("prompt"))))
                }
            }
            "session.interrupt" => {
                let id = session_id(params)?;
                let running = self.runtime.is_running(&id);
                self.runtime.cancel(&id);
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
            .filter(|s| self.runtime.is_running(&s.id))
            .map(|s| {
                json!({
                    "id": s.id,
                    "session_key": s.id,
                    "current": false,
                    "title": s.title.unwrap_or_default(),
                    "preview": "",
                    "model": s.model,
                    "status": "running",
                    "message_count": 0,
                    "started_at": s.created_at as f64 / 1000.,
                    "last_active": s.updated_at as f64 / 1000.,
                })
            })
            .collect()
    }

    /// Hermes's model picker: each provider and its models (listed live from the provider).
    async fn model_options(&self, settings: &Settings) -> Value {
        let mut providers = Vec::new();
        for provider in &settings.providers {
            let current = settings.provider.as_deref() == Some(provider.id.as_str());
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
            "model": settings.model,
            "provider": settings.provider,
        })
    }

    fn session_info(&self, id: &str) -> Value {
        let record = self.runtime.store().session(id).ok().flatten();
        let running = self.runtime.is_running(id);
        match record {
            Some(record) => json!({
                "model": record.model,
                "provider": record.provider,
                "cwd": record.cwd,
                "title": record.title.unwrap_or_default(),
                "stored_session_id": record.id,
                "running": running,
                "version": crate::rest::VERSION,
                // The protocol this backend speaks; the page warns "out of date" without it.
                "desktop_protocol": "hermes-gateway-v1",
                "profile_name": "default",
            }),
            None => json!({"stored_session_id": id, "running": running}),
        }
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
        }))
    }

    fn resume_session(&self, params: &Value, outbox: &Outbox) -> RpcResult {
        let id = session_id(params)?;
        let messages = self
            .runtime
            .messages(&id)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
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
        }))
    }

    /// `prompt.submit`: starts a turn and streams it as Hermes's events.
    fn submit(self: Arc<Self>, params: &Value, settings: &Settings, outbox: &Outbox) -> RpcResult {
        let id = session_id(params)?;
        let text = match &params["text"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let admission_id = params["submission_id"]
            .as_str()
            .or(params["input_id"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if self.runtime.is_running(&id) {
            return if self.runtime.steer(&id, &text) {
                Ok(json!({"status": "steered"}))
            } else {
                Err((SERVER_ERROR, "session busy".to_string()))
            };
        }
        let record = self
            .runtime
            .store()
            .session(&id)
            .ok()
            .flatten()
            .ok_or((INVALID_PARAMS, format!("Unknown session {id}")))?;
        // An untitled session takes its first message's first line as its title.
        if record.title.is_none() {
            let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            let title: String = line.chars().take(60).collect();
            if !title.is_empty() {
                let _ = self.runtime.store().set_title(&id, &title);
            }
        }
        let provider = settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or((SERVER_ERROR, "No provider is configured".to_string()))?
            .runtime();
        let mut turn = self
            .runtime
            .start_turn(&id, &provider, TurnOptions::default(), &text)
            .map_err(|e| (SERVER_ERROR, format!("{e:#}")))?;
        self.subscribe(&id, outbox);
        let id_for_receipt = id.clone();
        let this = self.clone();
        tokio::spawn(async move {
            let mut stream = transcript::TurnStream::default();
            this.emit(&id, "message.start", json!({}));
            while let Some(event) = turn.events.recv().await {
                match event {
                    RuntimeEvent::Agent(event) => {
                        for (name, payload) in stream.agent_event(event) {
                            this.emit(&id, name, payload);
                        }
                    }
                    RuntimeEvent::SteerUndelivered(_) => {}
                    RuntimeEvent::TurnFinished(status) => {
                        let (status, error) = match status {
                            TurnStatus::Completed => ("complete", None),
                            TurnStatus::Cancelled => ("interrupted", None),
                            TurnStatus::Failed(error) => ("error", Some(error)),
                        };
                        this.emit(
                            &id,
                            "message.complete",
                            json!({"text": stream.text(), "status": status, "error": error}),
                        );
                        break;
                    }
                }
            }
        });
        // The canonical protocol's admission receipt: which session took the prompt, under the
        // id the page gave this send (it drops its optimistic bubble on any other id or status).
        Ok(json!({
            "status": "started",
            "admission_id": admission_id,
            "ref": {"session_id": id_for_receipt},
        }))
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
        self.rest.handle(method, path, query, body)
    }

    /// `/api/profiles/sessions`: the sidebar's list (`PaginatedSessions`).
    fn session_list(&self, query: Option<&str>) -> Value {
        let param = |name: &str| {
            crate::rest::query_param(query, name).and_then(|v| v.parse::<usize>().ok())
        };
        let limit = param("limit").unwrap_or(50);
        let offset = param("offset").unwrap_or(0);
        let all = self.runtime.store().list_sessions(1000).unwrap_or_default();
        let sessions: Vec<Value> = all
            .iter()
            .skip(offset)
            .take(limit)
            .map(|s| {
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
                    "message_count": 1,
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "is_active": self.runtime.is_running(&s.id),
                    "pinned": s.pinned,
                    "profile": "default",
                })
            })
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

fn session_id(params: &Value) -> Result<String, (i64, String)> {
    params["session_id"]
        .as_str()
        .map(str::to_string)
        .ok_or((INVALID_PARAMS, "session_id is required".to_string()))
}

fn send(outbox: &Outbox, frame: Value) {
    let _ = outbox.send(frame.to_string());
}
