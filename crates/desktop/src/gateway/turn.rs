//! The prompt and the turn it starts: `prompt.submit` and the queue behind a running turn,
//! steering, redirecting, stopping, the page's answers to approval and clarify cards, and the
//! side questions (`/btw`) and background prompts (`/bg`). Hermes: `tui_gateway/methods_prompt.py`.

use std::sync::Arc;

use hermes_agent::events::AgentEvent;
use serde_json::{Value, json};

use super::rpc::{QueueRef, Reply, Respond, RpcError, SessionRef, SessionSteer, SessionText};
use super::*;
use super::{Gateway, session_id};
use crate::hub::Outbox;
use crate::providers::Settings;

impl Gateway {
    pub(super) async fn prompt_submit(
        self: &Arc<Self>,
        params: Value,
        settings: &Settings,
        outbox: &Outbox,
    ) -> Reply {
        // What the model takes (images, its window) is asked of the provider once, before
        // the turn starts.
        if let Ok(id) = session_id(&params)
            && let Some(record) = self
                .blocking(move |g| g.runtime.store().session(&id).ok().flatten())
                .await?
        {
            self.model_facts(&record.provider, &record.model).await;
        }
        let (s, o) = (settings.clone(), outbox.clone());
        self.blocking(move |g| g.clone().submit(&params, &s, &o, true))
            .await?
    }

    /// `/btw`: a side question about the conversation, answered as `btw.complete`.
    pub(super) fn prompt_btw(self: &Arc<Self>, p: SessionText, settings: &Settings) -> Reply {
        let id = p.session.id()?;
        let question = p.text;
        if question.is_empty() {
            return Err(RpcError::invalid("text required"));
        }
        let provider = self.session_provider(&id, settings)?;
        let task_id = format!("btw_{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let this = self.clone();
        let task = task_id.clone();
        tokio::spawn(async move {
            let text = match this.runtime.side_question(&id, &provider, &question).await {
                Ok(answer) if !answer.is_empty() => answer,
                Ok(_) => "error: the model gave no answer".to_string(),
                Err(error) => format!("error: {error:#}"),
            };
            this.emit(
                &id,
                "btw.complete",
                json!({"task_id": task, "question": question, "text": text}),
            );
        });
        Ok(json!({"task_id": task_id}))
    }

    /// `/bg`: the prompt runs as a background subagent; its answer arrives as
    /// `background.complete`, its progress as its subagent row's events.
    pub(super) fn prompt_background(
        self: &Arc<Self>,
        p: SessionText,
        settings: &Settings,
    ) -> Reply {
        let id = p.session.id()?;
        let text = p.text;
        if text.is_empty() {
            return Err(RpcError::invalid("text required"));
        }
        let provider = self.session_provider(&id, settings)?;
        let options = self.turn_options(&id);
        let mut task = self
            .runtime
            .background(&id, &provider, options, &text)
            .map_err(RpcError::failed)?;
        let task_id = task.task_id.clone();
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(event) = task.events.recv().await {
                if let AgentEvent::Subagent { kind, payload } = event {
                    this.emit(&id, kind, payload);
                }
            }
            let text = task.done.await.unwrap_or_else(|e| format!("error: {e}"));
            this.emit(
                &id,
                "background.complete",
                json!({"task_id": task.task_id, "text": text}),
            );
        });
        Ok(json!({"task_id": task_id}))
    }

    /// The page's answer to an approval card (`choice`) or a clarify question (`answer`), named
    /// by the prompt id it was shown with. `kind` is "approval" or "clarify".
    pub(super) fn respond(&self, p: Respond, kind: &str) -> Reply {
        let prompt_id = p
            .prompt_id
            .as_deref()
            .ok_or(RpcError::invalid("prompt_id is required"))?;
        let answer = if kind == "approval" {
            p.choice
        } else {
            p.answer
        };
        if self.hub.answer(prompt_id, answer) {
            Ok(json!({"resolved": 1}))
        } else {
            Err(RpcError::server(format!("No pending {kind} request")))
        }
    }

    /// The open approval cards, for a page that reconnected or switched back.
    pub(super) fn approval_pending(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
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

    pub(super) fn session_steer(&self, p: SessionSteer) -> Reply {
        let id = p.session.id()?;
        let text = p.text;
        if self.runtime.steer(&id, &text) {
            Ok(json!({"status": "queued", "session_id": id, "text": text}))
        } else {
            Ok(json!({"status": "rejected", "session_id": id, "text": text}))
        }
    }

    /// The queue panel: delete a queued prompt, steer it into the running turn, or edit it
    /// (the edit goes to the back, as a new admission).
    pub(super) fn prompt_cancel(&self, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        let cancelled = self.dequeue(&id, admission).is_some();
        Ok(json!({"cancelled": cancelled, "admission_id": admission, "session_id": id}))
    }

    pub(super) fn prompt_steer_queued(&self, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        if !self.runtime.is_running(&id) {
            return Err(RpcError::server("session_idle"));
        }
        let queued = self
            .dequeue(&id, admission)
            .ok_or(RpcError::server("admission_started"))?;
        let text = queued["text"].as_str().unwrap_or_default();
        if !self.runtime.steer(&id, text) {
            // The turn ended in between: it runs next after all.
            self.enqueue(&id, queued.clone(), true);
        }
        Ok(json!({"status": "steered", "admission_id": admission, "session_id": id}))
    }

    pub(super) fn prompt_replace_queued(&self, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        self.dequeue(&id, admission)
            .ok_or(RpcError::server("admission_started"))?;
        let edited = json!({
            "text": p.text,
            "submission_id": p.submission_id,
        });
        let replacement = self.enqueue(&id, edited, false);
        Ok(json!({
            "status": "queued",
            "admission_id": replacement,
            "replaced": admission,
            "ref": {"session_id": id},
        }))
    }

    /// Interrupt and send this instead: the text runs first once the turn has stopped.
    pub(super) fn session_redirect(&self, p: SessionSteer, outbox: &Outbox) -> Reply {
        let id = p.session.id()?;
        let text = p.text;
        if !self.runtime.is_running(&id) {
            return Ok(json!({"status": "rejected", "session_id": id, "text": text}));
        }
        self.subscribe(&id, outbox);
        self.enqueue(&id, json!({"text": text, "shown": true}), true);
        self.runtime.cancel(&id);
        self.hub.cancel_session(&id);
        Ok(json!({"status": "redirected", "session_id": id, "text": text}))
    }

    pub(super) fn session_interrupt(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        let running = self.runtime.is_running(&id);
        tracing::info!("stop requested in {id} (a turn was running: {running})");
        self.runtime.cancel(&id);
        self.hub.cancel_session(&id);
        Ok(json!({
            "status": if running { "interrupted" } else { "not_interrupted" },
            "interrupted": running,
        }))
    }
}

impl Gateway {
    /// What every turn of a session runs with: its model's window (so compression starts in
    /// time for a small local model) and the side jobs' models (Settings ▸ Model ▸ Auxiliary).
    pub(super) fn turn_options(&self, session: &str) -> TurnOptions {
        let settings = Settings::load(&self.data_dir);
        let record = self.runtime.store().session(session).ok().flatten();
        let context = |provider: &str, model: &str| self.known_context(provider, model);
        // Only a picked effort is sent; unset leaves the provider's default (as Hermes).
        let effort = self
            .picked_effort(session)
            // "ultra" is Hermes's own step above max: the wire has no more than max.
            .map(|e| if e == "ultra" { "max".to_string() } else { e });
        let mut aux = crate::aux_models::runtime_models(&settings, &context);
        // "Inherit main model effort" (the Auxiliary page's default): compression and the
        // background review think as hard as the chat. The approval judge and titles keep
        // reasoning off unless picked, as Hermes runs them.
        for job in [&mut aux.compression, &mut aux.review]
            .into_iter()
            .flatten()
        {
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
            // Settings ▸ Model ▸ Fallbacks (`fallback.rs`): the providers a request moves to when
            // the main one cannot serve it.
            fallback: crate::fallback::resolve(
                &self.rest.config_value("fallback_providers"),
                &settings,
                record
                    .as_ref()
                    .map(|r| (r.provider.as_str(), r.model.as_str())),
                &context,
                &|provider, model| self.sends_images(provider, model),
            ),
        }
    }

    /// Whether the session has a turn in flight as the page sees it: the runtime's turn, or the
    /// gap between a turn and its queued follow-up (the page must not see the turn end there).
    pub(super) fn is_live(&self, id: &str) -> bool {
        self.runtime
            .existing_session_handle(id)
            .is_some_and(|handle| handle.is_live())
    }

    /// The session's queued prompts as Hermes's pending admissions, in the order they'll run.
    /// (The queue is the session actor's: `PageState::queue`.)
    pub(super) fn pending_rows(&self, id: &str) -> Vec<Value> {
        let Some(handle) = self.runtime.existing_session_handle(id) else {
            return Vec::new();
        };
        handle
            .page()
            .queue
            .iter()
            // A redirect or a missed steer is on the page already, as the bubble the composer
            // drew: as a card too it would show twice.
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
    }

    /// Queues a prompt behind the running turn (at the front for a redirect or a steer the turn
    /// missed) and tells the page; returns its admission id.
    pub(super) fn enqueue(&self, id: &str, params: Value, front: bool) -> String {
        self.enqueue_as(id, params, front, false)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    /// [`Self::enqueue`] at the back, only while a turn holds the session (checked and queued
    /// in one step of its actor, so a turn ending in between can't leave the prompt queued with
    /// nobody to run it); `None` when no turn runs.
    pub(super) fn enqueue_while_running(&self, id: &str, params: Value) -> Option<String> {
        self.enqueue_as(id, params, false, true)
    }

    pub(super) fn enqueue_as(
        &self,
        id: &str,
        mut params: Value,
        front: bool,
        only_if_running: bool,
    ) -> Option<String> {
        let admission = admission_id(&params).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        params["session_id"] = json!(id);
        params["submission_id"] = json!(admission);
        let queued = self
            .runtime
            .session_handle(id)
            .map_err(|e| format!("{e:#}"))
            .and_then(|handle| {
                handle
                    .enqueue(params, front, only_if_running)
                    .map_err(|e| e.to_string())
            });
        match queued {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => tracing::warn!("queueing a prompt in {id}: {error}"),
        }
        self.emit(id, "session.info", self.session_info(id));
        Some(admission)
    }

    /// Takes a queued prompt out by its admission id (`None` when it already ran or never was).
    pub(super) fn dequeue(&self, id: &str, admission: &str) -> Option<Value> {
        let taken = self
            .runtime
            .existing_session_handle(id)
            .and_then(|handle| handle.dequeue(admission).ok().flatten());
        if taken.is_some() {
            self.emit(id, "session.info", self.session_info(id));
        }
        taken
    }

    pub(super) fn subscribe(&self, session_id: &str, outbox: &Outbox) {
        self.hub.subscribe(session_id, outbox);
    }

    /// `prompt.submit`: starts a turn and streams it as Hermes's events.
    /// `gated`: the page awaits this call's reply, which goes out before the turn's events.
    pub(super) fn submit(
        self: Arc<Self>,
        params: &Value,
        settings: &Settings,
        outbox: &Outbox,
        gated: bool,
    ) -> Reply {
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
                return Err(RpcError::server("session busy"));
            }
            // Anything else waits its turn (Hermes's queued admission, listed in the panel).
            self.subscribe(&id, outbox);
            if let Some(admission_id) = self.enqueue_while_running(&id, params.clone()) {
                return Ok(json!({
                    "status": "queued",
                    "admission_id": admission_id,
                    "ref": {"session_id": id},
                }));
            }
            // The turn ended in between: this prompt starts now.
        }
        // Restore / edit / regenerate: drop that user turn and everything after it first.
        // The cut is a stored row (the transcript's `row_id`), else the n-th typed message.
        if params["confirm_truncate"] == true {
            let messages = self
                .runtime
                .messages(&id)
                .map_err(|e| RpcError::server(format!("{e:#}")))?;
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
                    .map_err(|e| RpcError::server(format!("{e:#}")))?;
            }
        }
        let record = self
            .runtime
            .store()
            .session(&id)
            .ok()
            .flatten()
            .ok_or(RpcError::invalid(format!("Unknown session {id}")))?;
        // An untitled session takes its first message's first line as its title until the title
        // model's arrives (after this turn); a loaded skill counts as the command that loaded it.
        let shown = commands::typed_command(&text).unwrap_or_else(|| text.clone());
        // Under `requests` with no auxiliary title model the title comes from the message itself
        // and no title request is made (Phase 4); an auxiliary model, or another profile, keeps
        // the model's title.
        let heuristic_titles = self.runtime.budget().titles
            == hermes_runtime::budget::TitleMode::Heuristic
            && settings.aux_models.titles.is_none();
        let titling = (record.title.is_none() && !heuristic_titles).then(|| {
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
            let heuristic = if heuristic_titles {
                hermes_runtime::titles::heuristic_title(&shown)
            } else {
                String::new()
            };
            let title: String = if heuristic.is_empty() {
                line.chars().take(60).collect()
            } else {
                heuristic
            };
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
            .ok_or(RpcError::server("No provider is configured"))?
            .runtime();
        // The turn's rows start here (its receipt names them).
        let first_row = self.runtime.messages(&id).map(|m| m.len()).unwrap_or(0);
        let (record_provider, record_model) = (record.provider.clone(), record.model.clone());
        let options = self.turn_options(&id);
        // Images attached in the composer (`attachments: [{path, mime}]`, uploaded first through
        // `/api/chat/image-upload`): their bytes go with the prompt.
        // Images attached through `image.attach` go with this prompt too.
        let held: Vec<Value> = self
            .runtime
            .existing_session_handle(&id)
            .and_then(|handle| handle.take_images().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|path| json!({"path": path}))
            .collect();
        let mut listed = params["attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        listed.extend(held);
        let images = crate::attachments::image_bytes(&Value::Array(listed))
            .map_err(|e| RpcError::invalid(e))?;
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
        let mut turn = started.map_err(|e| RpcError::server(format!("{e:#}")))?;
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
            // The session's row and usage: store reads (the prompt was just queued), off the
            // async workers.
            let session = id.clone();
            if let Ok(info) = this.blocking(move |g| g.session_info(&session)).await {
                this.emit(&id, "session.info", info);
            }
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
                        // Store reads: off the async workers.
                        let session = id.clone();
                        if let Ok((info, usage)) = this
                            .blocking(move |g| {
                                (g.session_info(&session), g.usage_snapshot(&session, last_input))
                            })
                            .await
                        {
                            this.emit(&id, "session.info", info);
                            this.emit(&id, "session.usage", json!({"usage": usage}));
                        }
                        continue;
                    }
                };
                let Some(event) = event else {
                    this.end_live_off_worker(&id).await;
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
                        last_input = usage
                            .usage
                            .input_tokens
                            .map(|tokens| tokens.max(0) as usize);
                        call_started = std::time::Instant::now();
                        // `note_generation` saves a file and the snapshot reads the store: off
                        // the async workers (in this order, as before).
                        let session = id.clone();
                        if let Ok(snapshot) = this
                            .blocking(move |g| {
                                g.note_generation(&session, output, seconds);
                                g.usage_snapshot(&session, last_input)
                            })
                            .await
                        {
                            this.emit(&id, "session.usage", json!({"usage": snapshot}));
                        }
                    }
                    // A subagent's activity: the page's live subagent rows.
                    RuntimeEvent::Agent(AgentEvent::Subagent { kind, payload }) => {
                        this.emit(&id, kind, payload);
                    }
                    // A status line for the transcript: the page keeps `fallback` ones as a system
                    // line (Phase 6 uses it for "ran after reading untrusted content").
                    RuntimeEvent::Agent(AgentEvent::Status { kind, text }) => {
                        this.emit(&id, "status.update", json!({"kind": kind, "text": text}));
                    }
                    // Live progress from inside a running tool call (the terminal's output
                    // tail): the running row shows the message beside its timer, and the
                    // clicked-open row renders it as its output while the call works
                    // (patches/0017). The payload is the contract's ToolProgressPayload.
                    RuntimeEvent::Agent(AgentEvent::ToolProgress {
                        tool_call_id,
                        name,
                        progress,
                        message,
                    }) => {
                        this.emit(
                            &id,
                            "tool.progress",
                            json!({
                                "tool_id": tool_call_id,
                                "name": name,
                                "progress": progress,
                                "total": Value::Null,
                                "percent": Value::Null,
                                "message": message,
                            }),
                        );
                    }
                    // Compression before a model call can take minutes on a local model; the
                    // page shows its "summarizing" indicator for the `compacting` /
                    // `compacted` pair, else the turn looks hung (Hermes:
                    // `tui_gateway/server.py` `_status_update`, `agent/conversation_compression.py`).
                    RuntimeEvent::Agent(AgentEvent::Compacting) => {
                        this.emit(
                            &id,
                            "status.update",
                            json!({"kind": "compacting", "text": COMPACTING_TEXT}),
                        );
                    }
                    RuntimeEvent::Agent(AgentEvent::Compacted { .. }) => {
                        this.emit(
                            &id,
                            "status.update",
                            json!({"kind": "compacted", "text": COMPACTED_TEXT}),
                        );
                    }
                    RuntimeEvent::Agent(AgentEvent::CompactionFailed(error)) => {
                        let text = match error {
                            Some(error) => format!(
                                "Context compression failed ({error}); continuing with the full conversation."
                            ),
                            None => {
                                "Context compression couldn't shorten the conversation; continuing."
                                    .to_string()
                            }
                        };
                        this.emit(
                            &id,
                            "status.update",
                            json!({"kind": "compacted", "text": text}),
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
                    // The provider's daily request quota is nearly used up (our own notice: the page
                    // shows `status.update` kinds it knows and ignores the rest; CP2.3 adds the display).
                    RuntimeEvent::Notice(text) => {
                        this.emit(&id, "status.update", json!({"kind": "quota", "text": text}));
                    }
                    // A steer the turn ended before reading: it runs next, as Hermes does.
                    RuntimeEvent::SteerUndelivered(text) => {
                        // `enqueue` sends the session's info (store reads): off the async workers.
                        let session = id.clone();
                        let _ = this
                            .blocking(move |g| {
                                g.enqueue(&session, json!({"text": text, "shown": true}), true)
                            })
                            .await;
                    }
                    RuntimeEvent::TurnFinished(status) => {
                        let (mut status, mut error) = match status {
                            TurnStatus::Completed => ("complete", None),
                            TurnStatus::Cancelled => ("interrupted", None),
                            TurnStatus::Failed(error) => ("error", Some(error)),
                        };
                        // The stored conversation, and the failed session's provider: store reads,
                        // off the async workers.
                        let session = id.clone();
                        let completed = status == "complete";
                        let (stored, failed_route) = this
                            .blocking(move |g| {
                                let stored = g.runtime.messages(&session).unwrap_or_default();
                                let route = (completed
                                    && crate::transcript::message_error(&stored).is_some())
                                .then(|| g.runtime.store().session(&session).ok().flatten())
                                .flatten()
                                .map(|record| (record.provider, record.model));
                                (stored, route)
                            })
                            .await
                            .unwrap_or_default();
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
                        // Which provider and model failed: the card names them.
                        let (provider, model) = failed_route.unwrap_or_default();
                        let surface = failure.as_ref().map(|(kind, text)| {
                            crate::transcript::error_surface(*kind, text, &provider, &model)
                        });
                        // A turn that also streamed output keeps it: the text is the user's, not
                        // the error string (Hermes's partial failure settle). One that streamed
                        // nothing carries the account instead — the page's completion path
                        // appends the turn's message only when the frame's text is non-empty, so
                        // an empty frame would end the turn in silence (no card, no bubble), and
                        // its partial=false strips the account from the bubble: the card is the
                        // only thing shown.
                        let text = match (&failure, stream.text().trim().is_empty()) {
                            (Some((kind, detail)), true) => {
                                crate::transcript::turn_error_text(*kind, detail, &provider)
                            }
                            _ => stream.text().to_string(),
                        };
                        let partial = status == "error" && !stream.text().trim().is_empty();
                        let session = id.clone();
                        let usage = this
                            .blocking(move |g| g.usage_snapshot(&session, last_input))
                            .await
                            .unwrap_or(Value::Null);
                        this.emit(
                            &id,
                            "message.complete",
                            json!({
                                "text": text,
                                "status": status,
                                "error": error,
                                "error_surface": surface,
                                "partial": partial,
                                // The rows this turn wrote, so the page can restore or edit them.
                                "persisted_turn": transcript::persisted_turn(&stored, first_row, &admission),
                                "usage": usage,
                            }),
                        );
                        // With a prompt queued, the session stays live into its turn: a
                        // `running: false` in between would end the turn on the page.
                        if !this.has_queued(&id) {
                            this.end_live_off_worker(&id).await;
                        }
                        if let Some((Some((provider, model, effort)), opening)) = titling {
                            if status == "complete" {
                                this.title_session(
                                    &id,
                                    &provider,
                                    &model,
                                    effort.as_deref(),
                                    &opening,
                                )
                                .await;
                            }
                        }
                        break;
                    }
                }
            }
            // Both read the store (and the next prompt's submit writes it): off the async
            // workers.
            let (session, queued_outbox) = (id.clone(), outbox.clone());
            let _ = this
                .blocking(move |g| {
                    g.apply_pending_route(&session);
                    g.run_queued(&session, &queued_outbox);
                })
                .await;
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
    pub(super) fn run_queued(self: &Arc<Self>, id: &str, outbox: &Outbox) {
        let next = self
            .runtime
            .existing_session_handle(id)
            .and_then(|handle| handle.take_next().ok().flatten());
        let Some(params) = next else {
            return;
        };
        let settings = Settings::load(&self.data_dir);
        if let Err(error) = self.clone().submit(&params, &settings, outbox, false) {
            let error = error.message();
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
    pub(super) fn has_queued(&self, id: &str) -> bool {
        self.runtime
            .existing_session_handle(id)
            .is_some_and(|handle| !handle.page().queue.is_empty())
    }

    /// Marks the session as having a turn in flight: from the start of a turn through any queued
    /// prompts that follow it. The page hears `running: false` only when that ends.
    pub(super) fn set_live(&self, id: &str, live: bool) {
        // The session's actor holds it (`PageState::live`); clearing it never starts one.
        let handle = if live {
            self.runtime.session_handle(id).ok()
        } else {
            self.runtime.existing_session_handle(id)
        };
        if let Some(Err(error)) = handle.map(|handle| handle.set_live(live)) {
            tracing::warn!("marking {id}'s turn live={live}: {error}");
        }
    }

    /// The turn (and its queue) is over: the page settles it.
    pub(super) fn end_live(&self, id: &str) {
        self.set_live(id, false);
        let mut info = self.session_info(id);
        info["running"] = json!(false);
        self.emit(id, "session.info", info);
    }

    /// [`Self::end_live`] from async code: `session_info` reads the store. Should the thread die,
    /// the session is still marked as settled.
    pub(super) async fn end_live_off_worker(self: &Arc<Self>, id: &str) {
        let session = id.to_string();
        if self.blocking(move |g| g.end_live(&session)).await.is_err() {
            self.set_live(id, false);
        }
    }

    /// Hermes's auto-title: the title model names the session from its opening message, and the
    /// page hears the new title through `session.info`.
    pub(super) async fn title_session(
        self: &Arc<Self>,
        id: &str,
        provider: &hermes_runtime::ProviderConfig,
        model: &str,
        effort: Option<&str>,
        opening: &str,
    ) {
        // A title is optional: skipped when the daily request quota is running low.
        if let hermes_runtime::governor::Verdict::Deny(why) = self
            .runtime
            .governor()
            .check(&provider.id, hermes_llm::observer::Purpose::Title)
        {
            tracing::info!("titling {id} skipped: {why}");
            return;
        }
        let titled = self.runtime.ledger().scoped(
            hermes_llm::observer::Purpose::Title,
            Some(id),
            hermes_runtime::generate_title_with_effort(provider, model, effort, opening),
        );
        match titled.await {
            Ok(Some(title)) => {
                let (session, new_title) = (id.to_string(), title.clone());
                let _ = self
                    .blocking(move |g| g.runtime.store().set_title(&session, &new_title))
                    .await;
                self.emit(
                    id,
                    "session.title",
                    json!({"session_id": id, "title": title}),
                );
                let session = id.to_string();
                if let Ok(info) = self.blocking(move |g| g.session_info(&session)).await {
                    self.emit(id, "session.info", info);
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("titling {id}: {error:#}"),
        }
    }

    /// Sends a session event to the socket following that session.
    pub(super) fn emit(&self, session_id: &str, event: &str, payload: Value) {
        self.hub.emit(session_id, event, payload);
    }
}
