//! The prompt and the turn it starts: `prompt.submit` and the queue behind a running turn,
//! steering, redirecting, stopping, the page's answers to approval and clarify cards, and the
//! side questions (`/btw`) and background prompts (`/bg`). Hermes: `tui_gateway/methods_prompt.py`.
//!
//! The turn itself (submit, the queue, the turn's loop, the title) is `hermes_core::turns`; the
//! stream's frames come from the core's events through the hub. What stays here is the page's
//! side: the params as a `SubmitTurn`, the admission receipt and its gate, and [`PageTurn`], the
//! turn's hooks that send the frames needing the store (`session.info` with the queue panel's
//! rows, `session.usage`, `message.complete`).

use std::sync::{Arc, Mutex};

use hermes_agent::events::AgentEvent;
use hermes_core::turns::{
    Outcome, SteerQueued, SubmitError, SubmitTurn, Submitted, Truncate, TurnEnd, TurnHooks,
    TurnSettings,
};
use hermes_store::SessionRecord;
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
        let id = session_id(&params)?;
        let turn = submit_turn(&params);
        let hooks = PageTurn::new(self, Some(outbox.clone()), Some(settings.clone()));
        let session = id.clone();
        let submitted = self
            .blocking(move |g| g.core.submit(&session, turn, hooks))
            .await?;
        // The canonical protocol's admission receipt: which session took the prompt, under the
        // id the page gave this send (it drops its optimistic bubble on any other id or status).
        match submitted {
            Ok(Submitted::Started { admission_id }) => Ok(json!({
                "status": "started",
                "admission_id": admission_id,
                "ref": {"session_id": id},
            })),
            Ok(Submitted::Queued { admission_id }) => Ok(json!({
                "status": "queued",
                "admission_id": admission_id,
                "ref": {"session_id": id},
            })),
            Err(error) => Err(submit_error(error)),
        }
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
        if self.core.answer(prompt_id, answer) {
            Ok(json!({"resolved": 1}))
        } else {
            Err(RpcError::server(format!("No pending {kind} request")))
        }
    }

    /// The open approval cards, for a page that reconnected or switched back.
    pub(super) fn approval_pending(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        let approvals: Vec<Value> = self
            .core
            .open_questions(&id)
            .into_iter()
            .filter_map(|question| match question.ask {
                hermes_core::Ask::Approval {
                    command,
                    description,
                    choices,
                } => Some(json!({
                    "request_id": question.prompt_id,
                    "command": command,
                    "description": description,
                    "choices": choices,
                    "allow_permanent": true,
                    "allow_session": true,
                })),
                hermes_core::Ask::Clarify { .. } => None,
            })
            .collect();
        Ok(json!({"approvals": approvals}))
    }

    pub(super) fn session_steer(&self, p: SessionSteer) -> Reply {
        let id = p.session.id()?;
        let text = p.text;
        if self.core.steer(&id, &text) {
            Ok(json!({"status": "queued", "session_id": id, "text": text}))
        } else {
            Ok(json!({"status": "rejected", "session_id": id, "text": text}))
        }
    }

    /// The queue panel: delete a queued prompt, steer it into the running turn, or edit it
    /// (the edit goes to the back, as a new admission).
    pub(super) fn prompt_cancel(self: &Arc<Self>, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        let hooks = PageTurn::new(self, None, None);
        let cancelled = self.core.remove_queued(&id, admission, &*hooks).is_some();
        Ok(json!({"cancelled": cancelled, "admission_id": admission, "session_id": id}))
    }

    pub(super) fn prompt_steer_queued(self: &Arc<Self>, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        let hooks = PageTurn::new(self, None, None);
        match self.core.steer_queued(&id, admission, &*hooks) {
            Ok(()) => Ok(json!({"status": "steered", "admission_id": admission, "session_id": id})),
            Err(SteerQueued::Idle) => Err(RpcError::server("session_idle")),
            Err(SteerQueued::Started) => Err(RpcError::server("admission_started")),
        }
    }

    pub(super) fn prompt_replace_queued(self: &Arc<Self>, p: QueueRef) -> Reply {
        let id = p.session.id()?;
        let admission = p.admission_id.as_str();
        let hooks = PageTurn::new(self, None, None);
        let replacement = self
            .core
            .replace_queued(
                &id,
                admission,
                text_of(&p.text),
                p.submission_id.as_str().map(str::to_string),
                &*hooks,
            )
            .ok_or(RpcError::server("admission_started"))?;
        Ok(json!({
            "status": "queued",
            "admission_id": replacement,
            "replaced": admission,
            "ref": {"session_id": id},
        }))
    }

    /// Interrupt and send this instead: the text runs first once the turn has stopped.
    pub(super) fn session_redirect(self: &Arc<Self>, p: SessionSteer, outbox: &Outbox) -> Reply {
        let id = p.session.id()?;
        let text = p.text;
        let hooks = PageTurn::new(self, Some(outbox.clone()), None);
        if !self.core.redirect(&id, &text, &*hooks) {
            return Ok(json!({"status": "rejected", "session_id": id, "text": text}));
        }
        Ok(json!({"status": "redirected", "session_id": id, "text": text}))
    }

    pub(super) fn session_interrupt(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        let running = self.core.stop(&id);
        tracing::info!("stop requested in {id} (a turn was running: {running})");
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
        self.core.is_live(id)
    }

    /// The session's queued prompts as Hermes's pending admissions, in the order they'll run.
    pub(super) fn pending_rows(&self, id: &str) -> Vec<Value> {
        self.core
            .queue(id)
            .into_iter()
            // A redirect or a missed steer is on the page already, as the bubble the composer
            // drew: as a card too it would show twice.
            .filter(|turn| !turn.shown)
            .map(|turn| {
                json!({
                    "admission_id": turn.admission_id,
                    "text": turn.text,
                    "status": "queued",
                    "controls": ["steer", "replace", "cancel"],
                })
            })
            .collect()
    }

    pub(super) fn subscribe(&self, session_id: &str, outbox: &Outbox) {
        self.hub.subscribe(session_id, outbox);
    }

    /// The turn (and its queue) is over: the page settles it.
    fn settled(&self, id: &str) {
        let mut info = self.session_info(id);
        info["running"] = json!(false);
        self.emit(id, "session.info", info);
    }

    /// Sends a session event to the socket following that session.
    pub(super) fn emit(&self, session_id: &str, event: &str, payload: Value) {
        self.hub.emit(session_id, event, payload);
    }
}

/// `prompt.submit`'s params as the core's prompt.
fn submit_turn(params: &Value) -> SubmitTurn {
    // Restore / edit / regenerate: the cut is a stored row (the transcript's `row_id`), else the
    // n-th typed message.
    let truncate = (params["confirm_truncate"] == true).then(|| {
        match (
            params["truncate_before_row_id"].as_u64(),
            params["truncate_before_user_ordinal"].as_u64(),
        ) {
            (Some(row), _) => Truncate::BeforeRow(row as usize),
            (None, Some(ordinal)) => Truncate::BeforeUserMessage(ordinal as usize),
            (None, None) => Truncate::Nothing,
        }
    });
    SubmitTurn {
        admission_id: admission_id(params),
        text: text_of(&params["text"]),
        // Images attached in the composer (`attachments: [{path, mime}]`, uploaded first through
        // `/api/chat/image-upload`): their bytes go with the prompt.
        images: crate::attachments::image_paths(&params["attachments"]),
        truncate,
        shown: params["shown"] == true,
        // The page awaits this call's reply, which goes out before the turn's events.
        gated: true,
    }
}

/// A prompt's text as the page sent it (anything but a string as its JSON).
fn text_of(text: &Value) -> String {
    match text {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn submit_error(error: SubmitError) -> RpcError {
    match error {
        SubmitError::Busy => RpcError::server("session busy"),
        SubmitError::Invalid(text) => RpcError::invalid(text),
        SubmitError::Failed(text) => RpcError::server(text),
    }
}

/// A turn as the page follows it: the socket that sent the prompt, and the frames that need the
/// store, sent from the core's turn hooks (on blocking threads, in order with the events).
pub(super) struct PageTurn {
    gateway: Arc<Gateway>,
    outbox: Option<Outbox>,
    /// The Settings `prompt.submit` was answered with: its turn's provider comes from them; a
    /// queued prompt's turn reads them again when it starts.
    settings: Mutex<Option<Settings>>,
}

impl PageTurn {
    pub(super) fn new(
        gateway: &Arc<Gateway>,
        outbox: Option<Outbox>,
        settings: Option<Settings>,
    ) -> Arc<Self> {
        Arc::new(Self {
            gateway: gateway.clone(),
            outbox,
            settings: Mutex::new(settings),
        })
    }

    fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| Settings::load(&self.gateway.data_dir))
    }

    fn info(&self, id: &str) {
        self.gateway
            .emit(id, "session.info", self.gateway.session_info(id));
    }
}

impl TurnHooks for PageTurn {
    fn settings(&self, record: &SessionRecord) -> TurnSettings {
        let settings = self.settings();
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
        TurnSettings {
            provider: settings
                .provider(&record.provider)
                .or(settings.active())
                .map(|p| p.runtime()),
            titles_configured: settings.aux_models.titles.is_some(),
            title_model: chosen.or(fallback),
        }
    }

    fn options(&self, session_id: &str) -> TurnOptions {
        self.gateway.turn_options(session_id)
    }

    fn follow(&self, session_id: &str) {
        if let Some(outbox) = &self.outbox {
            self.gateway.subscribe(session_id, outbox);
        }
    }

    /// The turn's events wait for the `prompt.submit` receipt (released by its admission id in
    /// `Gateway::handle` / `call`).
    fn gate(&self, admission_id: &str) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let (start, started) = tokio::sync::oneshot::channel();
        if let Ok(mut starts) = self.gateway.starts.lock() {
            starts.insert(admission_id.to_string(), start);
        }
        Some(started)
    }

    /// After `message.start`: the session's row and usage (the page holds the turn as live from
    /// `running: true` until `running: false`), and the model's context window for the gauge
    /// (asked once per model, off this path).
    fn started(&self, session_id: &str, provider: &str, model: &str) {
        self.info(session_id);
        let (this, provider, model) = (
            self.gateway.clone(),
            provider.to_string(),
            model.to_string(),
        );
        tokio::spawn(async move { this.context_length(&provider, &model).await });
    }

    fn changed(&self, session_id: &str) {
        self.info(session_id);
    }

    /// A `running: true` heartbeat (Hermes's backend sends one, and `gateway.ready` says it does)
    /// keeps the page holding the turn as live through quiet stretches.
    fn heartbeat(&self, session_id: &str, last_input: Option<usize>) {
        let info = self.gateway.session_info(session_id);
        let usage = self.gateway.usage_snapshot(session_id, last_input);
        self.gateway.emit(session_id, "session.info", info);
        self.gateway
            .emit(session_id, "session.usage", json!({"usage": usage}));
    }

    /// The status bar's tokens per second: `note_generation` saves a file and the snapshot reads
    /// the store (in this order, as before).
    fn generated(
        &self,
        session_id: &str,
        output_tokens: u64,
        seconds: f64,
        last_input: Option<usize>,
    ) {
        self.gateway
            .note_generation(session_id, output_tokens, seconds);
        let snapshot = self.gateway.usage_snapshot(session_id, last_input);
        self.gateway
            .emit(session_id, "session.usage", json!({"usage": snapshot}));
    }

    /// `message.complete`, like Hermes's turn result. A failed turn carries the error, the
    /// structured descriptor the card names the fix from, and (when nothing streamed) the account
    /// as its text: the page's completion path appends the turn's message only when the frame's
    /// text is non-empty, so an empty frame would end the turn in silence.
    fn ended(&self, end: &TurnEnd, last_input: Option<usize>) {
        let usage = self.gateway.usage_snapshot(&end.session_id, last_input);
        let status = match end.outcome {
            Outcome::Complete => "complete",
            Outcome::Interrupted => "interrupted",
            Outcome::Error => "error",
        };
        let surface = end.failure.as_ref().map(|(kind, text)| {
            crate::transcript::error_surface(*kind, text, &end.failed_provider, &end.failed_model)
        });
        self.gateway.emit(
            &end.session_id,
            "message.complete",
            json!({
                "text": end.text,
                "status": status,
                "error": end.error,
                "error_surface": surface,
                "partial": end.partial,
                // The rows this turn wrote, so the page can restore or edit them.
                "persisted_turn": transcript::persisted_turn_json(&end.persisted, &end.admission_id),
                "usage": usage,
            }),
        );
    }

    fn queued_failed(&self, session_id: &str, error: &str) {
        self.gateway.emit(
            session_id,
            "message.complete",
            json!({"text": "", "status": "error", "error": error}),
        );
    }

    fn idle(&self, session_id: &str) {
        self.gateway.settled(session_id);
    }

    /// A model picked while the turn ran applies now.
    fn between_turns(&self, session_id: &str) {
        self.gateway.apply_pending_route(session_id);
    }
}
