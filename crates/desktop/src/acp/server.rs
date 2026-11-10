//! The ACP agent: the methods an editor calls (`initialize`, `session/new`, `session/prompt`, …),
//! answered by driving this process's own [`Gateway`] the way the page's socket does, and the
//! gateway's page events turned into `session/update`s (`translate.rs`).
//!
//! One reader (the caller's `read_lines`), one writer (`rpc::Sender`), one event pump (reads the
//! gateway's outbox), and one task per request, so a long `session/prompt` never holds up a
//! `session/cancel`.
//!
//! Hermes: `acp_adapter/gateway_server.py` (`GatewayACPAgent`: `initialize`, `new_session`,
//! `prompt`, `cancel`, `_events`, `_project`, `_answer_permission`, `_answer_clarify`),
//! `acp_adapter/server.py` (`initialize`, `list_sessions`, `_replay_session_history`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use super::rpc::{self, Incoming, RpcError, Sender};
use super::translate::{self, Action};
use super::{controls, history, permissions, prompt};
use crate::gateway::Gateway;
use crate::hub::Outbox;

const NO_MODEL: &str = "Hermes has no model set up. Open the Hermes desktop app, go to \
    Settings \u{25b8} Providers, add one, then start a new thread.";

/// Files bigger than this aren't read for a diff's "before".
const SNAPSHOT_LIMIT: u64 = 1 << 20;

struct Turn {
    /// Answers the `session/prompt` that started the turn.
    done: Option<oneshot::Sender<Result<Value, RpcError>>>,
    translate: translate::Turn,
    /// Cards waiting for the editor's answer, by prompt id.
    asking: HashMap<String, tokio::task::JoinHandle<()>>,
}

struct Session {
    cwd: String,
    /// One prompt at a time per session.
    gate: Arc<tokio::sync::Mutex<()>>,
    turn: Option<Turn>,
    /// The title the editor was last told.
    title: Option<String>,
}

#[derive(Default)]
struct Inner {
    /// The editor takes forms (`elicitation/create`).
    forms: bool,
    /// The editor draws terminal cards from `_meta` (Zed's extension).
    terminal_cards: bool,
    sessions: HashMap<String, Session>,
}

/// What `/undo` and `/retry` came to.
enum Rewound {
    /// The answer to show; the prompt ends there.
    Reply(String),
    /// `/retry`: the dropped prompt, sent again as this turn's.
    Resend(String),
}

pub struct AcpServer {
    gateway: Arc<Gateway>,
    sender: Sender,
    /// Where the gateway's events for our sessions arrive.
    outbox: Outbox,
    inner: Mutex<Inner>,
}

fn param<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params(format!("{key} is required")))
}

/// The gateway's error as ours.
fn failure((_, message): (i64, String)) -> RpcError {
    if message.starts_with("No provider is configured") || message.starts_with("No model is chosen")
    {
        return RpcError::internal(NO_MODEL);
    }
    RpcError::internal(message)
}

impl AcpServer {
    /// Serves one client until it closes its input.
    pub async fn serve<R, W>(gateway: Arc<Gateway>, input: R, output: W)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (sender, writer) = Sender::writing_to(output);
        let (outbox, events) = mpsc::unbounded_channel::<String>();
        let server = Arc::new(Self {
            gateway,
            sender: sender.clone(),
            outbox,
            inner: Mutex::new(Inner::default()),
        });
        let pump = tokio::spawn(server.clone().pump(events));
        let reader = server.clone();
        rpc::read_lines(input, move |incoming| reader.clone().incoming(incoming)).await;
        // The editor is gone: stop what runs for it.
        tracing::info!("the client closed its input; shutting down");
        server.shutdown().await;
        sender.fail_all();
        pump.abort();
        drop(server);
        drop(sender);
        // Let the writer flush what is queued.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    }

    fn incoming(self: Arc<Self>, incoming: Result<Incoming, RpcError>) {
        match incoming {
            Err(error) => {
                tracing::warn!("a line from the client: {}", error.message);
                self.sender.respond(Value::Null, Err(error));
            }
            Ok(Incoming::Response { id, result }) => self.sender.resolve(&id, result),
            Ok(Incoming::Notification { method, params }) => {
                tokio::spawn(async move { self.notification(&method, &params).await });
            }
            Ok(Incoming::Request { id, method, params }) => {
                tokio::spawn(async move {
                    let result = self.request(&method, &params).await;
                    if let Err(error) = &result {
                        tracing::warn!("{method} failed: {}", error.message);
                    }
                    let opened = match (&result, method.as_str()) {
                        (Ok(answer), "session/new") => answer["sessionId"].as_str(),
                        (Ok(_), "session/load") => params["sessionId"].as_str(),
                        _ => None,
                    }
                    .map(str::to_string);
                    self.sender.respond(id, result);
                    // The editor registers a session when it has the answer: what it needs to know
                    // next goes after it.
                    if let Some(session) = opened {
                        self.after_open(&session).await;
                    }
                });
            }
        }
    }

    async fn notification(self: &Arc<Self>, method: &str, params: &Value) {
        match method {
            "session/cancel" => {
                if let Some(session) = params["sessionId"].as_str() {
                    self.cancel(session).await;
                }
            }
            other => tracing::debug!("ignoring the notification {other}"),
        }
    }

    async fn request(self: &Arc<Self>, method: &str, params: &Value) -> Result<Value, RpcError> {
        tracing::info!("request {method}");
        match method {
            "initialize" => Ok(self.initialize(params)),
            "session/new" => self.new_session(params).await,
            "session/load" => self.load_session(params).await,
            "session/list" => self.list_sessions(params).await,
            "session/set_mode" => self.set_mode(params).await,
            "session/set_config_option" => self.set_config_option(params).await,
            "session/prompt" => self.prompt(params).await,
            // Liveness probes some clients send: not ours, and not worth a warning.
            "ping" | "health" | "healthcheck" => {
                Err(RpcError::new(rpc::METHOD_NOT_FOUND, "Method not found"))
            }
            other => {
                crate::server::log_missing(&format!("ACP {other}"), &params.to_string());
                Err(RpcError::new(
                    rpc::METHOD_NOT_FOUND,
                    format!("Method not found: {other}"),
                ))
            }
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let client = &params["clientInfo"];
        tracing::info!(
            "initialize from {} {} (protocol {})",
            client["name"].as_str().unwrap_or("an unnamed client"),
            client["version"].as_str().unwrap_or(""),
            params["protocolVersion"],
        );
        let forms = params["clientCapabilities"]["elicitation"]
            .get("form")
            .is_some_and(|form| !form.is_null());
        self.lock().forms = forms;
        self.lock().terminal_cards =
            params["clientCapabilities"]["_meta"]["terminal_output"] == true;
        json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {"image": true, "audio": false, "embeddedContext": true},
                "mcpCapabilities": {"http": false, "sse": false},
                "sessionCapabilities": {"list": {}},
            },
            "agentInfo": {
                "name": "hermes-rs",
                "title": "Hermes (rs)",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "authMethods": [],
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A call on our own gateway, the answer as ours.
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.gateway
            .clone()
            .call(method, params, &self.outbox)
            .await
            .map_err(failure)
    }

    fn update(&self, session: &str, update: Value) {
        self.sender.notify(
            "session/update",
            json!({"sessionId": session, "update": update}),
        );
    }

    // ---- sessions ----

    async fn new_session(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let cwd = param(params, "cwd")?;
        if let Some(servers) = params["mcpServers"].as_array().filter(|s| !s.is_empty()) {
            let names: Vec<_> = servers.iter().filter_map(|s| s["name"].as_str()).collect();
            tracing::info!(
                "ignoring the editor's MCP servers {names:?}: they are set up in the desktop app"
            );
        }
        let created = self.call("session.create", json!({"cwd": cwd})).await?;
        let id = created["session_id"]
            .as_str()
            .ok_or_else(|| RpcError::internal("the new session has no id"))?
            .to_string();
        self.lock().sessions.insert(
            id.clone(),
            Session {
                cwd: cwd.to_string(),
                gate: Arc::default(),
                turn: None,
                title: None,
            },
        );
        tracing::info!("new session {id} in {cwd}");
        let mut answer = self.controls(&id).await;
        answer["sessionId"] = json!(id);
        Ok(answer)
    }

    /// `session/load`: the thread's history is replayed as updates *before* the answer (the editor
    /// takes them while it waits for it), then the session is ready for prompts.
    async fn load_session(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let id = param(params, "sessionId")?;
        let asked_cwd = params["cwd"].as_str().unwrap_or_default();
        let resumed = self
            .call("session.resume", json!({"session_id": id}))
            .await
            .map_err(|error| {
                if error.message.starts_with("Session not found") {
                    RpcError::not_found(error.message)
                } else {
                    error
                }
            })?;
        let cwd = resumed["info"]["cwd"]
            .as_str()
            .unwrap_or(asked_cwd)
            .to_string();
        if !asked_cwd.is_empty() && !same_folder(&cwd, asked_cwd) {
            // The frozen system prompt names the folder the thread began in; it stays that.
            tracing::info!("{id} was opened from {asked_cwd}; it stays in {cwd}");
        }
        let title = resumed["info"]["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        {
            let mut inner = self.lock();
            let session = inner
                .sessions
                .entry(id.to_string())
                .or_insert_with(|| Session {
                    cwd: cwd.clone(),
                    gate: Arc::default(),
                    turn: None,
                    title: None,
                });
            session.title = title;
        }
        let rows = resumed["messages"].as_array().cloned().unwrap_or_default();
        for update in history::replay(&rows, &cwd) {
            self.update(id, update);
        }
        tracing::info!("loaded session {id}: {} rows", rows.len());
        Ok(self.controls(id).await)
    }

    /// `session/list`: this folder's threads (all of them without a `cwd`), newest first, a page
    /// at a time. The cursor is the last session id of the page before.
    async fn list_sessions(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        const PAGE: usize = 50;
        let wanted = params["cwd"].as_str().filter(|c| !c.is_empty());
        let cursor = params["cursor"].as_str().filter(|c| !c.is_empty());
        let (_, listed) = self
            .gateway
            .rest(
                "GET",
                "/api/profiles/sessions",
                Some("limit=1000&archived=exclude"),
                &[],
            )
            .await
            .ok_or_else(|| RpcError::internal("the session list isn't available"))?;
        let mut rows: Vec<&Value> = listed["sessions"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    // A thread nobody wrote in isn't worth listing.
                    .filter(|row| row["message_count"].as_u64().unwrap_or(0) > 0)
                    .filter(|row| {
                        wanted.is_none_or(|cwd| {
                            same_folder(row["cwd"].as_str().unwrap_or_default(), cwd)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(cursor) = cursor {
            rows = match rows.iter().position(|row| row["id"] == cursor) {
                Some(at) => rows.split_off(at + 1),
                // An unknown cursor is the end, never the whole list again.
                None => Vec::new(),
            };
        }
        let more = rows.len() > PAGE;
        let page: Vec<Value> = rows
            .iter()
            .take(PAGE)
            .map(|row| {
                let updated = row["last_active"]
                    .as_f64()
                    .and_then(|secs| chrono::DateTime::from_timestamp(secs as i64, 0))
                    .map(|time| time.to_rfc3339());
                json!({
                    "sessionId": row["id"],
                    "cwd": row["cwd"],
                    "title": row["title"].as_str().filter(|t| !t.is_empty()),
                    "updatedAt": updated,
                })
            })
            .collect();
        let next = more
            .then(|| page.last().map(|last| last["sessionId"].clone()))
            .flatten();
        Ok(json!({"sessions": page, "nextCursor": next}))
    }

    /// What the editor can change in a thread: its modes, and its model and effort pickers.
    async fn controls(&self, session: &str) -> Value {
        // A provider that doesn't answer must not hold up opening a thread.
        let options = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            self.call("model.options", json!({"session_id": session})),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let mut config = Vec::new();
        if let Some(options) = options {
            config.push(controls::model_option(&options));
        }
        config.push(controls::effort_option(&self.gateway.effort(session)));
        config.push(controls::mode_option(
            self.gateway.runtime.approval_mode(session),
        ));
        json!({
            "modes": controls::modes(self.gateway.runtime.approval_mode(session)),
            "configOptions": config,
        })
    }

    /// `session/set_mode`: the session's approval mode (Auto, Manual, Accept edits, Plan, YOLO).
    async fn set_mode(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let id = param(params, "sessionId")?;
        let mode_id = param(params, "modeId")?;
        let mode = controls::parse_mode(mode_id)
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown mode {mode_id}")))?;
        self.session_handle(id).await?;
        let gateway = self.gateway.clone();
        let session = id.to_string();
        tokio::task::spawn_blocking(move || gateway.runtime.set_approval_mode(&session, mode))
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
            .map_err(|e| RpcError::internal(format!("{e:#}")))?;
        Ok(json!({}))
    }

    /// `session/set_config_option`: the model or the effort, through the calls the page's pickers
    /// make. A model picked mid-turn applies from the next one (the gateway defers it).
    async fn set_config_option(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let id = param(params, "sessionId")?;
        let option = param(params, "configId")?;
        let value = param(params, "value")?;
        self.session_handle(id).await?;
        match option {
            "model" => {
                let (provider, model) = controls::split_model(value)
                    .ok_or_else(|| RpcError::invalid_params(format!("Not a model: {value}")))?;
                self.call(
                    "config.set",
                    json!({
                        "session_id": id,
                        "key": "model",
                        "value": format!("{model} --provider {provider} --session"),
                    }),
                )
                .await?;
            }
            "mode" => {
                self.set_mode(&json!({"sessionId": id, "modeId": value}))
                    .await?;
            }
            "effort" => {
                let effort = controls::gateway_effort(value)
                    .ok_or_else(|| RpcError::invalid_params(format!("Not an effort: {value}")))?;
                self.call(
                    "config.set",
                    json!({"session_id": id, "key": "reasoning", "value": effort}),
                )
                .await?;
            }
            other => {
                return Err(RpcError::invalid_params(format!("Unknown option {other}")));
            }
        }
        let controls = self.controls(id).await;
        Ok(json!({"configOptions": controls["configOptions"]}))
    }

    /// `/undo [N]` and `/retry`: the conversation cut back to before the Nth last typed prompt
    /// (Hermes's `/undo` and `/retry`, over the gateway's `session.rewind`). `None` for any other
    /// text.
    async fn rewind_command(self: &Arc<Self>, id: &str, text: &str) -> Option<Rewound> {
        let (name, arg) = text
            .strip_prefix('/')?
            .split_once(char::is_whitespace)
            .map_or((text.trim_start_matches('/'), ""), |(n, a)| (n, a.trim()));
        let retry = match name {
            "undo" => false,
            "retry" => true,
            _ => return None,
        };
        let turns = if retry {
            1
        } else if arg.is_empty() {
            1
        } else {
            match arg.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => return Some(Rewound::Reply("Usage: /undo [N]".into())),
            }
        };
        let messages = match self.gateway.runtime.messages(id) {
            Ok(messages) => messages,
            Err(error) => return Some(Rewound::Reply(format!("{error:#}"))),
        };
        let prompts = crate::transcript::typed_prompts(&messages);
        if prompts.is_empty() {
            let what = if retry { "retry" } else { "undo" };
            return Some(Rewound::Reply(format!("Nothing to {what}.")));
        }
        if turns > prompts.len() {
            return Some(Rewound::Reply(format!(
                "Only {} turn{} to undo.",
                prompts.len(),
                if prompts.len() == 1 { "" } else { "s" }
            )));
        }
        let (row, prompt, image) = &prompts[prompts.len() - turns];
        if retry && *image {
            return Some(Rewound::Reply(
                "/retry: that prompt had an image; send it again yourself.".into(),
            ));
        }
        let answer = match self
            .call(
                "session.rewind",
                json!({"session_id": id, "target_message_id": row}),
            )
            .await
        {
            Ok(answer) => answer,
            Err(error) => return Some(Rewound::Reply(error.message)),
        };
        if retry {
            return Some(Rewound::Resend(prompt.clone()));
        }
        let count = answer["rewound_count"].as_u64().unwrap_or(0);
        Some(Rewound::Reply(format!(
            "Undid {turns} turn{} ({count} message{}).\nRemoved prompt: {prompt}\n\n\
             The messages above stay on screen in Zed; reopen the thread from the history to see \
             the shortened conversation.",
            if turns == 1 { "" } else { "s" },
            if count == 1 { "" } else { "s" },
        )))
    }

    /// After `session/new` or `session/load` is answered: the commands the editor can offer.
    async fn after_open(&self, session: &str) {
        let catalog = self.call("commands.catalog", json!({})).await;
        let Ok(catalog) = catalog else {
            return;
        };
        let mut commands: Vec<Value> = catalog["pairs"]
            .as_array()
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|pair| {
                        let name = pair[0].as_str()?.trim_start_matches('/');
                        (!name.is_empty()).then(|| {
                            json!({
                                "name": name,
                                "description": pair[1].as_str().unwrap_or_default(),
                                "input": {"hint": "arguments, if it takes any"},
                            })
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The editor refuses a command that isn't listed, so the gateway's own are added.
        for (name, description) in controls::GATEWAY_COMMANDS {
            if !commands.iter().any(|c| c["name"] == name) {
                commands.push(json!({
                    "name": name,
                    "description": description,
                    "input": {"hint": "arguments, if it takes any"},
                }));
            }
        }
        self.update(
            session,
            json!({"sessionUpdate": "available_commands_update", "availableCommands": commands}),
        );
    }

    /// The session's prompt gate and folder; one the editor never created or loaded in this
    /// process (a thread reopened without `session/load`) is read from the store.
    async fn session_handle(
        &self,
        id: &str,
    ) -> Result<(Arc<tokio::sync::Mutex<()>>, String), RpcError> {
        if let Some(session) = self.lock().sessions.get(id) {
            return Ok((session.gate.clone(), session.cwd.clone()));
        }
        let gateway = self.gateway.clone();
        let wanted = id.to_string();
        let record = tokio::task::spawn_blocking(move || {
            gateway.runtime.store().session(&wanted).ok().flatten()
        })
        .await
        .ok()
        .flatten()
        .ok_or_else(|| RpcError::invalid_params(format!("Unknown session {id}")))?;
        let mut inner = self.lock();
        let session = inner
            .sessions
            .entry(id.to_string())
            .or_insert_with(|| Session {
                cwd: record.cwd.clone(),
                gate: Arc::default(),
                turn: None,
                title: None,
            });
        Ok((session.gate.clone(), session.cwd.clone()))
    }

    async fn cancel(&self, session: &str) {
        let known = self.lock().sessions.contains_key(session);
        if !known {
            return;
        }
        if let Err(error) = self
            .call("session.interrupt", json!({"session_id": session}))
            .await
        {
            tracing::warn!("stopping {session}: {}", error.message);
        }
    }

    /// The editor went away: stop every turn, and every command the agent started for it.
    async fn shutdown(&self) {
        let sessions: Vec<String> = self.lock().sessions.keys().cloned().collect();
        for session in sessions {
            self.gateway.runtime.cancel(&session);
            self.gateway.runtime.stop_processes(&session).await;
        }
    }

    // ---- prompts ----

    async fn prompt(self: &Arc<Self>, params: &Value) -> Result<Value, RpcError> {
        let id = param(params, "sessionId")?;
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let (gate, cwd) = self.session_handle(id).await?;
        // The turn before this one (the editor cancels first, but don't count on it) finishes first.
        let _turn = gate.lock_owned().await;
        let prompt = prompt::from_blocks(&blocks);
        if prompt.is_empty() {
            return Ok(json!({"stopReason": "end_turn"}));
        }
        let mut text = prompt.text.clone();
        // Images go to the gateway first; the turn takes the ones attached to the session.
        for image in &prompt.images {
            self.call(
                "image.attach_bytes",
                json!({"session_id": id, "content_base64": image.base64, "filename": image.name}),
            )
            .await
            .map_err(|error| RpcError::invalid_params(error.message))?;
        }
        // `/undo [N]` and `/retry` rewind the conversation (an editor can't edit an older prompt:
        // Zed offers that to its own agent only).
        if prompt.images.is_empty() {
            match self.rewind_command(id, text.trim()).await {
                Some(Rewound::Reply(answer)) => {
                    self.update(
                        id,
                        json!({
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": answer},
                            "messageId": uuid::Uuid::new_v4().to_string(),
                        }),
                    );
                    return Ok(json!({"stopReason": "end_turn"}));
                }
                Some(Rewound::Resend(again)) => text = again,
                None => {}
            }
        }
        // A command (`/usage`, a skill, `/plan …`): answered here, or turned into the message the
        // turn sends. Anything the gateway doesn't know as a command goes to the model as typed.
        if prompt.images.is_empty() && text.trim_start().starts_with('/') {
            let answer = self
                .call(
                    "slash.exec",
                    json!({"session_id": id, "command": text.trim()}),
                )
                .await;
            if let Ok(answer) = answer {
                let sends = matches!(answer["type"].as_str(), Some("send" | "skill"));
                match (sends, answer["message"].as_str(), answer["output"].as_str()) {
                    (true, Some(message), _) => text = message.to_string(),
                    (_, _, Some(output)) => {
                        self.update(
                            id,
                            json!({
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": output},
                                "messageId": uuid::Uuid::new_v4().to_string(),
                            }),
                        );
                        return Ok(json!({"stopReason": "end_turn"}));
                    }
                    _ => {}
                }
            }
        }
        let (done, finished) = oneshot::channel();
        let terminal_cards = self.lock().terminal_cards;
        self.lock().sessions.get_mut(id).map(|session| {
            session.turn = Some(Turn {
                done: Some(done),
                translate: translate::Turn::new(&cwd).with_terminal_cards(terminal_cards),
                asking: HashMap::new(),
            });
        });
        let submitted = self
            .call(
                "prompt.submit",
                json!({
                    "session_id": id,
                    "text": text,
                    "submission_id": uuid::Uuid::new_v4().to_string(),
                }),
            )
            .await;
        if let Err(error) = submitted {
            if let Some(session) = self.lock().sessions.get_mut(id) {
                session.turn = None;
            }
            return Err(error);
        }
        // The turn ends with the gateway's `message.complete`. Should its loop ever end without
        // one (the runtime failing under it), the prompt must not wait forever: a session the
        // runtime no longer runs, two checks in a row, has no turn left to wait for.
        let mut finished = std::pin::pin!(finished);
        let mut idle = 0;
        let mut check = tokio::time::interval(std::time::Duration::from_secs(3));
        check.tick().await;
        loop {
            tokio::select! {
                answer = &mut finished => {
                    return answer.unwrap_or_else(|_| {
                        Err(RpcError::internal("the agent stopped before the turn ended"))
                    });
                }
                _ = check.tick() => {
                    idle = if self.gateway.runtime.is_running(id) { 0 } else { idle + 1 };
                    if idle >= 2 {
                        if let Some(session) = self.lock().sessions.get_mut(id) {
                            session.turn = None;
                        }
                        tracing::warn!("{id}: the turn stopped without a result");
                        return Err(RpcError::internal("The turn ended without a result."));
                    }
                }
            }
        }
    }

    // ---- the gateway's events ----

    async fn pump(self: Arc<Self>, mut events: mpsc::UnboundedReceiver<String>) {
        while let Some(frame) = events.recv().await {
            let Ok(frame) = serde_json::from_str::<Value>(&frame) else {
                continue;
            };
            if frame["method"] != "event" {
                continue;
            }
            let params = &frame["params"];
            let (Some(kind), Some(session)) =
                (params["type"].as_str(), params["session_id"].as_str())
            else {
                continue;
            };
            self.on_event(session, kind, &params["payload"]).await;
        }
    }

    /// The editor's thread title follows the session's: the first line of the first message at
    /// once, the title model's a moment after the first turn.
    fn note_title(&self, session: &str, title: Option<&str>) {
        let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) else {
            return;
        };
        let changed = {
            let mut inner = self.lock();
            let Some(known) = inner.sessions.get_mut(session) else {
                return;
            };
            let changed = known.title.as_deref() != Some(title);
            known.title = Some(title.to_string());
            changed
        };
        if changed {
            self.update(
                session,
                json!({
                    "sessionUpdate": "session_info_update",
                    "title": title,
                    "updatedAt": chrono::Utc::now().to_rfc3339(),
                }),
            );
        }
    }

    async fn on_event(self: &Arc<Self>, session: &str, kind: &str, payload: &Value) {
        if matches!(kind, "session.title" | "session.info") {
            self.note_title(session, payload["title"].as_str());
            return;
        }
        let actions = {
            let mut inner = self.lock();
            let Some(turn) = inner
                .sessions
                .get_mut(session)
                .and_then(|session| session.turn.as_mut())
            else {
                return;
            };
            turn.translate.event(kind, payload)
        };
        for action in actions {
            self.run(session, action).await;
        }
    }

    async fn run(self: &Arc<Self>, session: &str, action: Action) {
        match action {
            Action::Update(update) => self.update(session, update),
            Action::Snapshot { tool_id, path } => {
                let text = tokio::task::spawn_blocking(move || read_snapshot(&path))
                    .await
                    .ok()
                    .flatten();
                if let Some(turn) = self
                    .lock()
                    .sessions
                    .get_mut(session)
                    .and_then(|s| s.turn.as_mut())
                {
                    turn.translate.set_before(&tool_id, text);
                }
            }
            Action::Approval {
                prompt_id,
                command,
                description,
                choices,
            } => {
                let this = self.clone();
                let (sid, pid) = (session.to_string(), prompt_id.clone());
                let task = tokio::spawn(async move {
                    this.ask_approval(&sid, &pid, &command, &description, &choices)
                        .await;
                });
                self.remember_asking(session, prompt_id, task);
            }
            Action::Clarify {
                prompt_id,
                question,
                choices,
                multi_select,
            } => {
                let this = self.clone();
                let (sid, pid) = (session.to_string(), prompt_id.clone());
                let task = tokio::spawn(async move {
                    this.ask_clarify(&sid, &pid, &question, &choices, multi_select)
                        .await;
                });
                self.remember_asking(session, prompt_id, task);
            }
            Action::Finish(result) => {
                let turn = self
                    .lock()
                    .sessions
                    .get_mut(session)
                    .and_then(|s| s.turn.take());
                if let Some(mut turn) = turn {
                    for (_, task) in turn.asking.drain() {
                        task.abort();
                    }
                    if let Some(done) = turn.done.take() {
                        let _ = done.send(result);
                    }
                }
            }
        }
    }

    fn remember_asking(&self, session: &str, prompt_id: String, task: tokio::task::JoinHandle<()>) {
        match self
            .lock()
            .sessions
            .get_mut(session)
            .and_then(|s| s.turn.as_mut())
        {
            Some(turn) => {
                turn.asking.insert(prompt_id, task);
            }
            None => task.abort(),
        }
    }

    /// A flagged command: the editor's card, then the gateway's answer. Runs on its own task: the
    /// pump keeps reading while the user decides.
    async fn ask_approval(
        &self,
        session: &str,
        prompt_id: &str,
        command: &str,
        description: &str,
        choices: &[String],
    ) {
        let request =
            permissions::approval_request(session, prompt_id, command, description, choices);
        let answer = self
            .sender
            .request("session/request_permission", request.clone())
            .await;
        let choice = permissions::approval_choice(&answer, &request);
        self.update(
            session,
            permissions::closing_update(prompt_id, choice != "deny"),
        );
        let told = self
            .call(
                "approval.respond",
                json!({"session_id": session, "prompt_id": prompt_id, "choice": choice}),
            )
            .await;
        if let Err(error) = told {
            // Already settled: the turn was stopped while the card was open.
            tracing::debug!("approval {prompt_id}: {}", error.message);
        }
    }

    async fn ask_clarify(
        &self,
        session: &str,
        prompt_id: &str,
        question: &str,
        choices: &[String],
        multi: bool,
    ) {
        let mut answer = None;
        if self.lock().forms {
            let form = permissions::clarify_form(session, question, choices, multi);
            let response = self.sender.request("elicitation/create", form).await;
            answer = permissions::clarify_form_answer(&response);
            if answer.is_none() {
                tracing::info!("the editor didn't take the form; asking with a card");
                self.lock().forms = false;
            }
        }
        if answer.is_none() {
            let (request, answers) =
                permissions::clarify_card(session, prompt_id, question, choices);
            let response = self
                .sender
                .request("session/request_permission", request)
                .await;
            answer = permissions::clarify_card_answer(&response, &answers);
            self.update(
                session,
                json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": format!("clarify-{prompt_id}"),
                    "status": "completed",
                }),
            );
        }
        let Some(answer) = answer else {
            return;
        };
        let told = self
            .call(
                "clarify.respond",
                json!({"session_id": session, "prompt_id": prompt_id, "answer": answer}),
            )
            .await;
        if let Err(error) = told {
            tracing::debug!("clarify {prompt_id}: {}", error.message);
        }
    }
}

/// A file's text for a diff's "before": `None` for a file that isn't there, is binary or is big.
fn read_snapshot(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > SNAPSHOT_LIMIT {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// Two spellings of one folder: slashes, a trailing slash and case don't matter on Windows.
fn same_folder(a: &str, b: &str) -> bool {
    fn norm(path: &str) -> String {
        path.replace('\\', "/").trim_end_matches('/').to_lowercase()
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_compare_by_meaning_not_spelling() {
        assert!(same_folder("C:\\Users\\A\\proj", "c:/users/a/proj/"));
        assert!(!same_folder("C:/a", "C:/b"));
    }
}
