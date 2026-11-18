//! A prompt and the turn it starts: [`Core::submit`] runs the prompt now, or queues it behind the
//! session's running turn; the turn's loop streams its events ([`CoreEvent`]s), its end, its title
//! and the next queued prompt. The queue panel's operations (list, remove, steer into the running
//! turn, replace, redirect) are here too.
//!
//! How a front end takes part: the events (the reply, tool calls, `TurnStarted`, `TurnEnded`,
//! `QueueChanged`, `Idle`) reach every listener. What a front end needs beyond an event (the
//! Tauri page's `session.info` and `session.usage` frames, which read the store) is a
//! [`TurnHooks`] object given with the prompt: the loop calls it on a blocking thread, in order
//! with the events, at the points where the page's frames go out. Its settings methods name the
//! provider, the turn's options and the title model (Settings stay the front end's until CP1.3
//! moves them). The native UI passes [`NoHooks`] and a resolver of its own.
//!
//! The queue is the session actor's (`PageState::queue`, a list of JSON values in the runtime):
//! each item is a [`SubmitTurn`] written with serde under the page's field names
//! (`submission_id`, `text`, `shown`), which is what the actor looks prompts up by.
//!
//! Hermes: `tui_gateway/methods_prompt.py` (`prompt.submit`, the queued admissions, steer,
//! redirect), `tui_gateway/server.py` (the heartbeat, the auto-title after the first turn).

use std::sync::Arc;
use std::time::{Duration, Instant};

use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::message::MessageErrorKind;
use hermes_runtime::{ProviderConfig, RuntimeEvent, TurnOptions, TurnStatus};
use hermes_store::SessionRecord;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::Core;
use crate::events::CoreEvent;
use crate::stream::TurnStream;
use crate::transcript::{self, PersistedTurn};

/// How often a running turn tells the front end it is still running (Hermes's heartbeat).
const HEARTBEAT: Duration = Duration::from_secs(5);

/// A prompt to run in a session. Queued, it is kept as JSON under these (the page's) names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubmitTurn {
    /// The front end's id for this send (Hermes's admission id); one is made when `None`.
    #[serde(
        rename = "submission_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub admission_id: Option<String>,
    /// What the user typed.
    #[serde(default)]
    pub text: String,
    /// Images attached in the composer (paths of files already saved): their bytes go with the
    /// prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Restore, edit or regenerate: the session's conversation is cut before the prompt runs
    /// (refused with [`SubmitError::Busy`] while a turn runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncate: Option<Truncate>,
    /// Already on screen as the user's bubble (a redirect, a steer the turn missed): the queue
    /// panel lists no card for it.
    #[serde(default)]
    pub shown: bool,
    /// The front end releases the turn's events itself, once it has answered the request
    /// ([`TurnHooks::gate`]; the page's admission receipt goes out first).
    #[serde(skip)]
    pub gated: bool,
}

/// Where a restore, edit or regenerate cuts the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Truncate {
    /// Before a stored row (the transcript's `row_id`).
    BeforeRow(usize),
    /// Before the n-th message the user typed (0-based).
    BeforeUserMessage(usize),
    /// Nothing named: no cut (the busy check still applies).
    Nothing,
}

/// What [`Core::submit`] did with a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// The turn started.
    Started { admission_id: String },
    /// A turn was running: the prompt waits behind it.
    Queued { admission_id: String },
}

/// Why a prompt didn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// A cut can't wait behind the running turn: stop it and send again.
    Busy,
    /// Something in the request is wrong (an unknown session, an image that can't be read).
    Invalid(String),
    /// The turn couldn't start (no provider, the store, the runtime).
    Failed(String),
}

impl SubmitError {
    /// The error's text (Hermes's `session busy` for [`SubmitError::Busy`]).
    pub fn message(&self) -> &str {
        match self {
            SubmitError::Busy => "session busy",
            SubmitError::Invalid(text) | SubmitError::Failed(text) => text,
        }
    }
}

/// How a turn ended, as the user sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    /// Stopped by the user.
    Interrupted,
    /// The provider or the runtime failed.
    Error,
}

/// A finished turn: what it said, how it ended, the rows it stored.
#[derive(Debug, Clone)]
pub struct TurnEnd {
    pub session_id: String,
    pub admission_id: String,
    /// The runtime's status (a provider error is `Completed` here, with `failure` set).
    pub status: TurnStatus,
    pub outcome: Outcome,
    /// The error text (the runtime's, or the provider's from the last message).
    pub error: Option<String>,
    /// A provider error the turn ended on, and which provider and model it was.
    pub failure: Option<(MessageErrorKind, String)>,
    pub failed_provider: String,
    pub failed_model: String,
    /// The reply (or, when nothing streamed, the account of the error).
    pub text: String,
    /// An error after some output: the output is kept.
    pub partial: bool,
    /// The rows the turn wrote.
    pub persisted: PersistedTurn,
}

/// The settings a turn of a session runs with (the front end's Settings).
pub struct TurnSettings {
    /// The session's provider (else the default); `None` when nothing is configured.
    pub provider: Option<ProviderConfig>,
    /// Settings ▸ Model ▸ Auxiliary names a title model.
    pub titles_configured: bool,
    /// The model that names a new session: the auxiliary one, else the session's own; with an
    /// effort only for the auxiliary one.
    pub title_model: Option<(ProviderConfig, String, Option<String>)>,
}

/// What a front end does at a turn's milestones besides showing events (see the module doc).
/// Every method runs on a blocking thread (it may read the store), in order with the events.
pub trait TurnHooks: Send + Sync {
    /// The provider and title model for a turn of this session.
    fn settings(&self, record: &SessionRecord) -> TurnSettings;
    /// The options every turn of the session runs with (its window, effort, side models).
    fn options(&self, session_id: &str) -> TurnOptions;
    /// The front end follows the session's events from now on.
    fn follow(&self, _session_id: &str) {}
    /// For a gated prompt: what the turn's events wait for (`None`: they go at once).
    fn gate(&self, _admission_id: &str) -> Option<oneshot::Receiver<()>> {
        None
    }
    /// The turn started (after `TurnStarted`).
    fn started(&self, _session_id: &str, _provider: &str, _model: &str) {}
    /// The session's queue, title or state changed.
    fn changed(&self, _session_id: &str) {}
    /// Every few seconds while the turn runs. `last_input`: the last call's input tokens.
    fn heartbeat(&self, _session_id: &str, _last_input: Option<usize>) {}
    /// A model call reported its usage: `output_tokens` over `seconds` of generation.
    fn generated(
        &self,
        _session_id: &str,
        _output_tokens: u64,
        _seconds: f64,
        _last_input: Option<usize>,
    ) {
    }
    /// The turn ended (after `TurnEnded`).
    fn ended(&self, _end: &TurnEnd, _last_input: Option<usize>) {}
    /// A queued prompt couldn't start.
    fn queued_failed(&self, _session_id: &str, _error: &str) {}
    /// The turn and the prompts queued behind it are done (after `Idle`).
    fn idle(&self, _session_id: &str) {}
    /// Between a turn and the next queued prompt (a model picked mid-turn applies here).
    fn between_turns(&self, _session_id: &str) {}
}

/// Hooks with nothing to add, over a fixed provider and options (tests, simple front ends).
pub struct NoHooks {
    pub provider: Option<ProviderConfig>,
    pub options: TurnOptions,
}

impl TurnHooks for NoHooks {
    fn settings(&self, _record: &SessionRecord) -> TurnSettings {
        TurnSettings {
            provider: self.provider.clone(),
            titles_configured: false,
            title_model: None,
        }
    }

    fn options(&self, _session_id: &str) -> TurnOptions {
        self.options.clone()
    }
}

/// What the loop needs to title a new session after its first turn.
struct Titling {
    model: Option<(ProviderConfig, String, Option<String>)>,
    opening: String,
}

impl Core {
    /// Runs a prompt: at once, or queued behind the running turn. Blocking (store reads, a cut,
    /// the attached images): call it off the async workers.
    pub fn submit(
        self: &Arc<Self>,
        session_id: &str,
        turn: SubmitTurn,
        hooks: Arc<dyn TurnHooks>,
    ) -> Result<Submitted, SubmitError> {
        let id = session_id.to_string();
        let admission_id = turn
            .admission_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // A session the idle sweep archived is back in use: back in the sidebar too.
        self.reopened(&id);
        if self.runtime.is_running(&id) {
            // A cut can't wait behind the turn: the front end stops it and sends again.
            if turn.truncate.is_some() {
                return Err(SubmitError::Busy);
            }
            // Anything else waits its turn (Hermes's queued admission, listed in the panel).
            hooks.follow(&id);
            if let Some(admission_id) = self.enqueue_as(&id, turn.clone(), false, true, &*hooks) {
                return Ok(Submitted::Queued { admission_id });
            }
            // The turn ended in between: this prompt starts now.
        }
        if let Some(cut) = turn.truncate {
            self.cut(&id, cut)?;
        }
        let record = self
            .runtime
            .store()
            .session(&id)
            .ok()
            .flatten()
            .ok_or_else(|| SubmitError::Invalid(format!("Unknown session {id}")))?;
        let settings = hooks.settings(&record);
        let titling = self.provisional_title(&record, &turn.text, &settings);
        let provider = settings
            .provider
            .ok_or_else(|| SubmitError::Failed("No provider is configured".into()))?;
        // The turn's rows start here (its end names them).
        let first_row = self.runtime.messages(&id).map(|m| m.len()).unwrap_or(0);
        let options = hooks.options(&id);
        // Images attached in the composer, and those held for this prompt (`image.attach`).
        let mut images = turn.images.clone();
        images.extend(
            self.runtime
                .existing_session_handle(&id)
                .and_then(|handle| handle.take_images().ok())
                .unwrap_or_default(),
        );
        let images = crate::attachments::image_bytes(&images).map_err(SubmitError::Invalid)?;
        let started = if let Some(instructions) = crate::commands::review_instructions(&turn.text) {
            // `/review`: the turn starts an independent reviewer subagent on the recent
            // conversation (Hermes's `start_review`), then the model presents its findings.
            let (goal, context) = self.review_task(&id, instructions);
            let arguments = serde_json::json!({"goal": goal, "context": context});
            self.runtime.start_turn_with_call(
                &id,
                &provider,
                options,
                &turn.text,
                "delegate_task",
                arguments.as_object().cloned().unwrap_or_default(),
            )
        } else if images.is_empty() {
            self.runtime.start_turn(&id, &provider, options, &turn.text)
        } else {
            self.runtime
                .start_turn_with_images(&id, &provider, options, &turn.text, images)
        };
        let running = started.map_err(|e| SubmitError::Failed(format!("{e:#}")))?;
        hooks.follow(&id);
        // Only a request that named its admission is released by name (a generated one is never
        // looked up, and its turn would wait forever).
        let gate = (turn.gated && turn.admission_id.is_some())
            .then(|| hooks.gate(&admission_id))
            .flatten();
        self.set_live(&id, true);
        let run = TurnRun {
            core: self.clone(),
            hooks,
            session_id: id,
            admission_id: admission_id.clone(),
            provider: record.provider,
            model: record.model,
            first_row,
            titling,
        };
        self.handle.spawn(run.run(running, gate));
        Ok(Submitted::Started { admission_id })
    }

    /// Restore / edit / regenerate: drops that user turn and everything after it.
    fn cut(&self, id: &str, cut: Truncate) -> Result<(), SubmitError> {
        let messages = self
            .runtime
            .messages(id)
            .map_err(|e| SubmitError::Failed(format!("{e:#}")))?;
        let keep = match cut {
            Truncate::BeforeRow(row) => Some(transcript::kept_before_row(row).min(messages.len())),
            Truncate::BeforeUserMessage(ordinal) => {
                Some(transcript::user_turn_index(&messages, ordinal).unwrap_or(messages.len()))
            }
            Truncate::Nothing => None,
        };
        if let Some(keep) = keep {
            self.runtime
                .rewind(id, keep)
                .map_err(|e| SubmitError::Failed(format!("{e:#}")))?;
        }
        Ok(())
    }

    /// An untitled session takes its first message's first line as its title until the title
    /// model's arrives (after this turn); a loaded skill counts as the command that loaded it.
    /// Under `requests` with no auxiliary title model the title comes from the message itself and
    /// no title request is made (Phase 4). Returns what the loop needs to ask the title model.
    fn provisional_title(
        &self,
        record: &SessionRecord,
        text: &str,
        settings: &TurnSettings,
    ) -> Option<Titling> {
        let shown = crate::commands::typed_command(text).unwrap_or_else(|| text.to_string());
        let heuristic_titles = self.runtime.budget().titles
            == hermes_runtime::budget::TitleMode::Heuristic
            && !settings.titles_configured;
        let titling = (record.title.is_none() && !heuristic_titles).then(|| Titling {
            model: settings.title_model.clone(),
            opening: shown.clone(),
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
                let _ = self.runtime.store().set_title(&record.id, &title);
                self.announce_title(&record.id, &title);
            }
        }
        titling
    }

    /// The session's queued prompts, in the order they'll run.
    pub fn queue(&self, session_id: &str) -> Vec<SubmitTurn> {
        let Some(handle) = self.runtime.existing_session_handle(session_id) else {
            return Vec::new();
        };
        handle.page().queue.iter().filter_map(from_queue).collect()
    }

    /// Whether prompts wait behind the session's turn.
    pub fn has_queued(&self, session_id: &str) -> bool {
        self.runtime
            .existing_session_handle(session_id)
            .is_some_and(|handle| !handle.page().queue.is_empty())
    }

    /// Queues a prompt behind the running turn (at the front for a redirect or a steer the turn
    /// missed); returns its admission id. Blocking.
    pub fn enqueue(
        &self,
        session_id: &str,
        turn: SubmitTurn,
        front: bool,
        hooks: &dyn TurnHooks,
    ) -> String {
        self.enqueue_as(session_id, turn, front, false, hooks)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    /// [`Self::enqueue`], `only_if_running`: checked and queued in one step of the session's
    /// actor, so a turn ending in between can't leave the prompt queued with nobody to run it;
    /// `None` when no turn runs.
    fn enqueue_as(
        &self,
        id: &str,
        mut turn: SubmitTurn,
        front: bool,
        only_if_running: bool,
        hooks: &dyn TurnHooks,
    ) -> Option<String> {
        let admission = turn
            .admission_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        turn.admission_id = Some(admission.clone());
        let item = serde_json::to_value(&turn).unwrap_or_default();
        let queued = self
            .runtime
            .session_handle(id)
            .map_err(|e| format!("{e:#}"))
            .and_then(|handle| {
                handle
                    .enqueue(item, front, only_if_running)
                    .map_err(|e| e.to_string())
            });
        match queued {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => tracing::warn!("queueing a prompt in {id}: {error}"),
        }
        self.queue_changed(id, hooks);
        Some(admission)
    }

    /// Takes a queued prompt out by its admission id (`None` when it already ran or never was).
    /// Blocking.
    pub fn remove_queued(
        &self,
        session_id: &str,
        admission_id: &str,
        hooks: &dyn TurnHooks,
    ) -> Option<SubmitTurn> {
        let taken = self
            .runtime
            .existing_session_handle(session_id)
            .and_then(|handle| handle.dequeue(admission_id).ok().flatten());
        let taken = taken.as_ref().and_then(from_queue);
        if taken.is_some() {
            self.queue_changed(session_id, hooks);
        }
        taken
    }

    /// Moves a queued prompt into the running turn as a steer. `Err(SteerQueued::Idle)` when no
    /// turn runs, `Err(SteerQueued::Started)` when the prompt is no longer queued. Blocking.
    pub fn steer_queued(
        &self,
        session_id: &str,
        admission_id: &str,
        hooks: &dyn TurnHooks,
    ) -> Result<(), SteerQueued> {
        if !self.runtime.is_running(session_id) {
            return Err(SteerQueued::Idle);
        }
        let queued = self
            .remove_queued(session_id, admission_id, hooks)
            .ok_or(SteerQueued::Started)?;
        if !self.runtime.steer(session_id, &queued.text) {
            // The turn ended in between: it runs next after all.
            self.enqueue(session_id, queued, true, hooks);
        }
        Ok(())
    }

    /// Replaces a queued prompt's text: the edit goes to the back as a new admission (named
    /// `new_admission`, else a new id). `None` when the prompt is no longer queued. Blocking.
    pub fn replace_queued(
        &self,
        session_id: &str,
        admission_id: &str,
        text: String,
        new_admission: Option<String>,
        hooks: &dyn TurnHooks,
    ) -> Option<String> {
        self.remove_queued(session_id, admission_id, hooks)?;
        let edited = SubmitTurn {
            admission_id: new_admission,
            text,
            ..SubmitTurn::default()
        };
        Some(self.enqueue(session_id, edited, false, hooks))
    }

    /// Interrupt and send this instead: the text runs first once the turn has stopped (its open
    /// questions close; an approval is denied). `false` when no turn runs. Blocking.
    pub fn redirect(&self, session_id: &str, text: &str, hooks: &dyn TurnHooks) -> bool {
        if !self.runtime.is_running(session_id) {
            return false;
        }
        hooks.follow(session_id);
        let turn = SubmitTurn {
            text: text.to_string(),
            shown: true,
            ..SubmitTurn::default()
        };
        self.enqueue(session_id, turn, true, hooks);
        self.stop(session_id);
        true
    }

    /// A `/steer` message into the running turn; `false` when none runs.
    pub fn steer(&self, session_id: &str, text: &str) -> bool {
        self.runtime.steer(session_id, text)
    }

    fn queue_changed(&self, session_id: &str, hooks: &dyn TurnHooks) {
        self.publish(CoreEvent::QueueChanged {
            session_id: session_id.to_string(),
        });
        hooks.changed(session_id);
    }

    /// Starts the session's next queued prompt, once its turn has finished. Blocking.
    fn run_queued(self: &Arc<Self>, id: &str, hooks: Arc<dyn TurnHooks>) {
        let next = self
            .runtime
            .existing_session_handle(id)
            .and_then(|handle| handle.take_next().ok().flatten());
        let Some(item) = next else {
            return;
        };
        let Some(turn) = from_queue(&item) else {
            tracing::warn!("a queued prompt in {id} could not be read: {item}");
            self.publish(CoreEvent::QueueChanged {
                session_id: id.to_string(),
            });
            self.end_live(id, &*hooks);
            return;
        };
        let admission_id = turn.admission_id.clone().unwrap_or_default();
        if let Err(error) = self.submit(id, turn, hooks.clone()) {
            let error = error.message();
            tracing::warn!("queued prompt in {id}: {error}");
            // The front end that sent it learns why it never ran (the hooks are the page's).
            self.publish(CoreEvent::QueuedFailed {
                session_id: id.to_string(),
                admission_id,
                error: error.to_string(),
            });
            hooks.queued_failed(id, error);
            self.end_live(id, &*hooks);
        }
    }

    /// Marks the session as having a turn in flight: from the start of a turn through any queued
    /// prompts that follow it.
    fn set_live(&self, id: &str, live: bool) {
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

    /// The turn (and its queue) is over. Blocking.
    fn end_live(&self, id: &str, hooks: &dyn TurnHooks) {
        self.set_live(id, false);
        self.publish(CoreEvent::Idle {
            session_id: id.to_string(),
        });
        hooks.idle(id);
    }

    /// The `/review` reviewer's goal and briefing: the session's last user and assistant messages
    /// (their text), and the skills it loaded (`snapshot_recent_messages`,
    /// `collect_parent_loaded_skills`). Blocking.
    pub fn review_task(&self, id: &str, instructions: &str) -> (String, String) {
        crate::commands::review_task(&self.runtime.messages(id).unwrap_or_default(), instructions)
    }
}

/// Why [`Core::steer_queued`] didn't steer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerQueued {
    /// No turn runs.
    Idle,
    /// The prompt is no longer queued (it started, or was removed).
    Started,
}

fn from_queue(item: &serde_json::Value) -> Option<SubmitTurn> {
    serde_json::from_value(item.clone()).ok()
}

/// One running turn's loop.
struct TurnRun {
    core: Arc<Core>,
    hooks: Arc<dyn TurnHooks>,
    session_id: String,
    admission_id: String,
    provider: String,
    model: String,
    first_row: usize,
    titling: Option<Titling>,
}

impl TurnRun {
    /// Runs `work` with the hooks on a blocking thread; `None` when the thread died.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Arc<Core>, &dyn TurnHooks) -> T + Send + 'static,
    ) -> Option<T> {
        let (core, hooks) = (self.core.clone(), self.hooks.clone());
        tokio::task::spawn_blocking(move || work(&core, &*hooks))
            .await
            .ok()
    }

    /// [`Core::end_live`] from the loop. Should the thread die, the session is still settled.
    async fn end_live(&self) {
        let id = self.session_id.clone();
        if self
            .blocking(move |core, hooks| core.end_live(&id, hooks))
            .await
            .is_none()
        {
            self.core.set_live(&self.session_id, false);
        }
    }

    async fn run(mut self, mut turn: hermes_runtime::Turn, gate: Option<oneshot::Receiver<()>>) {
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        let id = self.session_id.clone();
        let mut stream = TurnStream::new(&id);
        // TurnStarted first: for a queued prompt it names the admission (the page turns that
        // queue card into the user's bubble), before the hook's session state (whose queue no
        // longer lists the prompt).
        self.core.publish(CoreEvent::TurnStarted {
            session_id: id.clone(),
            admission_id: self.admission_id.clone(),
        });
        let (session, provider, model) = (id.clone(), self.provider.clone(), self.model.clone());
        self.blocking(move |_, hooks| hooks.started(&session, &provider, &model))
            .await;
        // While the turn runs, a heartbeat keeps the front end holding the turn as live through
        // quiet stretches: a long think, a retry, a wait on the provider.
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.tick().await;
        // When the current model call began: the turn's start, then each tool result (tool time
        // isn't generation time). A call's output tokens over its time are its speed.
        let mut call_started = Instant::now();
        let mut last_input: Option<usize> = None;
        loop {
            let event = tokio::select! {
                event = turn.events.recv() => event,
                _ = heartbeat.tick() => {
                    let session = id.clone();
                    self.blocking(move |_, hooks| hooks.heartbeat(&session, last_input)).await;
                    continue;
                }
            };
            let Some(event) = event else {
                self.end_live().await;
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
                    call_started = Instant::now();
                    let session = id.clone();
                    self.blocking(move |_, hooks| {
                        hooks.generated(&session, output, seconds, last_input)
                    })
                    .await;
                    self.core.publish(CoreEvent::Usage {
                        session_id: id.clone(),
                        input_tokens: usage.usage.input_tokens.map(i64::from),
                        output_tokens: usage.usage.output_tokens.map(i64::from),
                    });
                }
                // A subagent's activity (Hermes's `subagent.*` rows).
                RuntimeEvent::Agent(AgentEvent::Subagent { kind, payload }) => {
                    self.core.publish(CoreEvent::Subagent {
                        session_id: id.clone(),
                        kind,
                        payload,
                    });
                }
                // The reply, reasoning, tool calls and status lines (a fallback, a taint warning,
                // compression, which can take minutes on a local model).
                RuntimeEvent::Agent(event) => {
                    if matches!(&event, AgentEvent::Message(m) if m.is_tool_response()) {
                        call_started = Instant::now();
                    }
                    for event in stream.agent_event(event) {
                        self.core.publish(event);
                    }
                }
                // The provider's daily request quota is nearly used up.
                RuntimeEvent::Notice(text) => {
                    self.core.publish(stream.notice(text));
                }
                // A steer the turn ended before reading: it runs next, as Hermes does.
                RuntimeEvent::SteerUndelivered(text) => {
                    let session = id.clone();
                    self.blocking(move |core, hooks| {
                        let turn = SubmitTurn {
                            text,
                            shown: true,
                            ..SubmitTurn::default()
                        };
                        core.enqueue(&session, turn, true, hooks)
                    })
                    .await;
                }
                RuntimeEvent::TurnFinished(status) => {
                    let end = self.ending(status, &stream).await;
                    self.core.publish(CoreEvent::TurnEnded {
                        session_id: id.clone(),
                        status: end.status.clone(),
                        end: Box::new(end.clone()),
                    });
                    let complete = end.outcome == Outcome::Complete;
                    self.blocking(move |_, hooks| hooks.ended(&end, last_input))
                        .await;
                    // With a prompt queued, the session stays live into its turn: an idle in
                    // between would end the turn on the page.
                    if !self.core.has_queued(&id) {
                        self.end_live().await;
                    }
                    if let Some(Titling {
                        model: Some((provider, model, effort)),
                        opening,
                    }) = self.titling.take()
                        && complete
                    {
                        self.title(&provider, &model, effort.as_deref(), &opening)
                            .await;
                    }
                    break;
                }
            }
        }
        // Both read the store (and the next prompt's submit writes it): off the async workers.
        let session = id.clone();
        let (core, hooks) = (self.core.clone(), self.hooks.clone());
        let _ = tokio::task::spawn_blocking(move || {
            hooks.between_turns(&session);
            core.run_queued(&session, hooks);
        })
        .await;
    }

    /// The finished turn as the user sees it: a provider error from the last message counts as
    /// a failure (Hermes's failed turn), with the provider and model that failed.
    async fn ending(&self, status: TurnStatus, stream: &TurnStream) -> TurnEnd {
        let (mut outcome, mut error) = match &status {
            TurnStatus::Completed => (Outcome::Complete, None),
            TurnStatus::Cancelled => (Outcome::Interrupted, None),
            TurnStatus::Failed(error) => (Outcome::Error, Some(error.clone())),
        };
        // The stored conversation, and the failed session's provider: store reads.
        let session = self.session_id.clone();
        let completed = outcome == Outcome::Complete;
        let (stored, failed_route) = self
            .blocking(move |core, _| {
                let stored = core.runtime.messages(&session).unwrap_or_default();
                let route = (completed && transcript::message_error(&stored).is_some())
                    .then(|| core.runtime.store().session(&session).ok().flatten())
                    .flatten()
                    .map(|record| (record.provider, record.model));
                (stored, route)
            })
            .await
            .unwrap_or_default();
        // A turn that ended on a provider error carries it as the conversation's last message
        // (ops::ExitOnError yields it): the turn is reported like Hermes's failed turn.
        let failure = completed
            .then(|| transcript::message_error(&stored))
            .flatten();
        if let Some((_kind, text)) = &failure {
            outcome = Outcome::Error;
            error = Some(text.clone());
        }
        let (failed_provider, failed_model) = failed_route.unwrap_or_default();
        // A turn that also streamed output keeps it: the text is the user's, not the error string
        // (Hermes's partial failure settle). One that streamed nothing carries the account.
        let text = match (&failure, stream.text().trim().is_empty()) {
            (Some((kind, detail)), true) => {
                transcript::turn_error_text(*kind, detail, &failed_provider)
            }
            _ => stream.text().to_string(),
        };
        let partial = outcome == Outcome::Error && !stream.text().trim().is_empty();
        TurnEnd {
            session_id: self.session_id.clone(),
            admission_id: self.admission_id.clone(),
            status,
            outcome,
            error,
            failure,
            failed_provider,
            failed_model,
            text,
            partial,
            persisted: transcript::persisted_turn(&stored, self.first_row),
        }
    }

    /// Hermes's auto-title: the title model names the session from its opening message.
    async fn title(
        &self,
        provider: &ProviderConfig,
        model: &str,
        effort: Option<&str>,
        opening: &str,
    ) {
        let id = &self.session_id;
        let runtime = self.core.runtime();
        // A title is optional: skipped when the daily request quota is running low.
        if let hermes_runtime::governor::Verdict::Deny(why) = runtime
            .governor()
            .check(&provider.id, hermes_llm::observer::Purpose::Title)
        {
            tracing::info!("titling {id} skipped: {why}");
            return;
        }
        let titled = runtime.ledger().scoped(
            hermes_llm::observer::Purpose::Title,
            Some(id),
            hermes_runtime::generate_title_with_effort(provider, model, effort, opening),
        );
        match titled.await {
            Ok(Some(title)) => {
                let (session, new_title) = (id.clone(), title.clone());
                self.blocking(move |core, _| core.runtime.store().set_title(&session, &new_title))
                    .await;
                self.core.announce_title(id, &title);
                let session = id.clone();
                self.blocking(move |_, hooks| hooks.changed(&session)).await;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("titling {id}: {error:#}"),
        }
    }
}

#[cfg(test)]
mod tests;
