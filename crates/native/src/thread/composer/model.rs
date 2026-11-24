//! The composer's model: what the footer shows and what a send does, for the open thread or for a
//! new one. One [`ComposerModel`] per open thread (or new-thread screen), made by the app model
//! beside the thread's own; it asks the [`Backend`] and never the core.
//!
//! What it keeps: the thread's controls as the core last reported them ([`Controls`]: the model,
//! effort and mode, whether a turn runs, the queue of prompts waiting behind it, how full the
//! context is), read in the background and read again when the core says something changed
//! (`TurnStarted`, `TurnEnded`, `Idle`, `QueueChanged`, `QueuedFailed` events, forwarded by the
//! app model); the model list the endpoints offer (asked when the picker opens); and, for a new
//! thread, the picks that have no session to apply to yet.
//!
//! A send is `Backend::send`: the core starts the turn, queues the prompt behind the running one,
//! or (Ctrl+Enter) steers it into the running turn. The user's row in the timeline is the thread
//! model's, shown when the turn with this prompt's admission id starts ([`ThreadModel::expect_prompt`]),
//! so a queued prompt appears when it runs and a refused send shows nothing. A refused send keeps
//! the text in the input (the view clears it only on [`ComposerEvent::Sent`]); a queued prompt the
//! core could not start comes back as [`ComposerEvent::Restore`].
//!
//! The newest read wins: events bump a generation and a read that started before the newest
//! event is read again, so a slow read cannot show a turn as idle after it started.
//!
//! t3code: `apps/web/src/components/chat/ChatComposer.tsx` (the send, the stop, the queue),
//! `QueuedRunsControl.tsx` (send now, edit, remove), `ComposerModelEffort.tsx`.
//!
//! Hermes: none (`hermes_core::turns` is the queue; the page's `prompt.submit` and the queue panel
//! are its counterparts).

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{AppContext as _, Context, Entity, EventEmitter, ImageFormat};
use hermes_core::CoreEvent;

use super::attach::{self, Attachment, MAX_ATTACHMENTS};
use super::data::{
    Controls, ModelChoices, Pick, Picks, QueueOp, QueuedItem, SendRequest, Sent, sendable,
};
use super::mention::{self, AppCommand, CommandSpec, Suggestion};
use crate::model::Backend;
use crate::model::backend::{AttachmentSource, CommandAnswer, CompletionScope};
use crate::thread::ThreadModel;

/// What the composer is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A stored thread.
    Thread(String),
    /// A new thread in the project with this id (nothing is stored until the first send).
    Draft(String),
}

/// The model picker's list.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Choices {
    /// Not asked yet.
    #[default]
    Idle,
    /// The endpoints are being asked.
    Loading,
    Ready(ModelChoices),
    /// Why they could not be listed.
    Failed(String),
}

/// The commands of the core's catalog.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Catalog {
    /// Not asked yet.
    #[default]
    Idle,
    Loading,
    Ready(Vec<CommandSpec>),
    Failed(String),
}

/// The `@` menu's entries for the word being typed.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Mentions {
    /// The word the entries are for (`@src/ma`).
    pub word: String,
    pub items: Vec<Suggestion>,
    /// The core is being asked.
    pub loading: bool,
    /// Why it could not be asked.
    pub error: Option<String>,
}

/// What the composer tells its view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    /// The send went through: clear the input if it still holds `text`.
    Sent { text: String },
    /// The send was refused: the text stays (or comes back if the input was emptied).
    Failed { text: String, why: String },
    /// A queued prompt could not start: put its text back.
    Restore { text: String, why: String },
    /// A queued prompt was taken out to be edited in the input.
    Edit { text: String },
    /// An edit ended (sent as a replacement, or cancelled).
    EditEnded,
    /// Something the user asked for did not work (a toast).
    Problem { title: String, why: String },
    /// Something to tell that is not a failure (a toast).
    Notice { title: String, why: String },
    /// A command answered while there is no thread to show it in (the new-thread screen).
    Answer { command: String, text: String },
    /// The user ran a command of the app (`/new`, `/model`, ...): the view or the frame does it.
    App(AppCommand),
    /// A new thread was made for the first send: the app model opens it and sends there.
    Created {
        session: String,
        request: SendRequest,
        picks: Picks,
    },
}

/// The composer of a thread or of the new-thread screen.
pub struct ComposerModel {
    target: Target,
    backend: Arc<dyn Backend>,
    thread: Option<Entity<ThreadModel>>,
    controls: Controls,
    /// The first read of the controls landed.
    loaded: bool,
    /// Bumped by every event that changes what a read would say.
    generation: u64,
    /// The generation a read in flight started at.
    reading: Option<u64>,
    sending: bool,
    /// Picks of a new thread, applied with its first send.
    picks: Picks,
    /// The model picked on the new-thread screen.
    draft_model: Option<(String, String)>,
    choices: Choices,
    counter: u64,
    /// The queued prompt being edited in the input.
    editing: Option<String>,
    /// Queued prompts this composer sent: the text, to give back if one cannot start.
    queued_texts: HashMap<String, String>,
    show_all_queued: bool,
    /// The files and images that go with the next prompt.
    attachments: Vec<Attachment>,
    /// Their thumbnails, made once (an image is decoded once, whatever the frame count).
    thumbs: HashMap<u64, Arc<gpui_kit::Image>>,
    /// Attachments being read off the UI thread.
    preparing: usize,
    next_attachment: u64,
    catalog: Catalog,
    mentions: Option<Mentions>,
    /// Bumped by every `@` request: an answer for an older word is dropped.
    mention_seq: u64,
}

impl EventEmitter<ComposerEvent> for ComposerModel {}

impl ComposerModel {
    /// The composer of `target`; `thread` is its timeline's model (to show the prompts). Reads
    /// the controls at once.
    pub fn new(
        target: Target,
        backend: Arc<dyn Backend>,
        thread: Option<Entity<ThreadModel>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self {
            target,
            backend,
            thread,
            controls: Controls::default(),
            loaded: false,
            generation: 0,
            reading: None,
            sending: false,
            picks: Picks::default(),
            draft_model: None,
            choices: Choices::Idle,
            counter: 0,
            editing: None,
            queued_texts: HashMap::new(),
            show_all_queued: false,
            attachments: Vec::new(),
            thumbs: HashMap::new(),
            preparing: 0,
            next_attachment: 0,
            catalog: Catalog::Idle,
            mentions: None,
            mention_seq: 0,
        };
        model.refresh(cx);
        model
    }

    // --- reading ------------------------------------------------------------------------------

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The thread's id, or `None` on the new-thread screen.
    pub fn session(&self) -> Option<&str> {
        match &self.target {
            Target::Thread(id) => Some(id),
            Target::Draft(_) => None,
        }
    }

    /// The footer's state as last read (with the picks made since, on a new thread).
    pub fn controls(&self) -> &Controls {
        &self.controls
    }

    /// Whether the first read of the controls landed.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// A turn runs in the thread (the button becomes Stop when there is nothing to send).
    pub fn running(&self) -> bool {
        self.controls.running
    }

    /// A send is on its way to the core.
    pub fn sending(&self) -> bool {
        self.sending
    }

    /// The prompts waiting behind the turn (the ones not already on screen).
    pub fn queue(&self) -> Vec<&QueuedItem> {
        self.controls.queue.iter().filter(|q| !q.shown).collect()
    }

    /// The queued prompt being edited, if any.
    pub fn editing(&self) -> Option<&str> {
        self.editing.as_deref()
    }

    /// Whether a long queue is shown whole.
    pub fn show_all_queued(&self) -> bool {
        self.show_all_queued
    }

    pub fn toggle_show_all_queued(&mut self, cx: &mut Context<Self>) {
        self.show_all_queued = !self.show_all_queued;
        cx.notify();
    }

    /// The model picker's list.
    pub fn choices(&self) -> &Choices {
        &self.choices
    }

    /// The picks a new thread's first send will carry.
    pub fn picks(&self) -> &Picks {
        &self.picks
    }

    /// Reads the controls again, in the background. One read at a time; an event during a read
    /// makes it read again when it lands.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.reading.is_some() {
            return;
        }
        let started = self.generation;
        self.reading = Some(started);
        let (backend, target) = (self.backend.clone(), self.target.clone());
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    match target {
                        Target::Thread(session) => backend.controls(&session),
                        Target::Draft(_) => backend.draft_controls(),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reading = None;
                if this.generation != started {
                    // Something happened while the core was asked: ask again.
                    this.refresh(cx);
                    return;
                }
                match read {
                    Ok(controls) => this.take_controls(controls, cx),
                    Err(why) => tracing::warn!("reading the composer's controls: {why}"),
                }
            });
        })
        .detach();
    }

    fn take_controls(&mut self, mut controls: Controls, cx: &mut Context<Self>) {
        if matches!(self.target, Target::Draft(_)) {
            // What was picked on the new-thread screen shows over the defaults.
            if let Some((provider, model)) = &self.draft_model {
                controls.provider = provider.clone();
                controls.model = model.clone();
                controls.provider_name = self.provider_name(provider).unwrap_or_default();
            }
            if let Some(effort) = &self.picks.effort {
                controls.effort = effort.clone().unwrap_or_default();
            }
            if let Some(mode) = self.picks.mode {
                controls.mode = mode;
            }
        }
        self.loaded = true;
        if self.controls != controls {
            self.controls = controls;
            cx.notify();
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.refresh(cx);
    }

    fn provider_name(&self, provider: &str) -> Option<String> {
        match &self.choices {
            Choices::Ready(choices) => choices
                .entries
                .iter()
                .find(|e| e.provider == provider)
                .map(|e| e.provider_name.clone()),
            _ => None,
        }
    }

    // --- the core's events --------------------------------------------------------------------

    /// Takes in one of the core's events (the ones for another thread are ignored).
    pub fn on_event(&mut self, event: &CoreEvent, cx: &mut Context<Self>) {
        let Target::Thread(session) = &self.target else {
            return;
        };
        if event.session_id() != session {
            return;
        }
        match event {
            CoreEvent::TurnStarted { admission_id, .. } => {
                self.queued_texts.remove(admission_id);
                self.controls.running = true;
                cx.notify();
                self.invalidate(cx);
            }
            // A model picked during the turn is applied after this event, with no event of its
            // own: the backend's read shows the pick until it is stored (`CoreBackend::controls`),
            // so the read this starts is right either side of that moment.
            CoreEvent::Idle { .. } => {
                self.controls.running = false;
                cx.notify();
                self.invalidate(cx);
            }
            CoreEvent::TurnEnded { .. } | CoreEvent::QueueChanged { .. } => self.invalidate(cx),
            CoreEvent::QueuedFailed {
                admission_id,
                error,
                ..
            } => {
                if let Some(text) = self.queued_texts.remove(admission_id) {
                    self.drop_expected(admission_id, cx);
                    cx.emit(ComposerEvent::Restore {
                        text,
                        why: error.clone(),
                    });
                }
                self.invalidate(cx);
            }
            _ => {}
        }
    }

    fn drop_expected(&mut self, admission_id: &str, cx: &mut Context<Self>) {
        if let Some(thread) = &self.thread {
            thread.update(cx, |thread, _| thread.forget_prompt(admission_id));
        }
    }

    // --- sending ------------------------------------------------------------------------------

    /// Sends `text` (the user pressed Enter, or Ctrl+Enter for `steer`) with the attachments.
    /// A text that is a command (`/usage`) runs it. On a stored thread the core starts a turn,
    /// queues it behind the running one, or steers it into it; on the new-thread screen a thread
    /// is made first. Nothing happens for white space with nothing attached, while a send is on
    /// its way, or while an attachment is still being read.
    pub fn send(&mut self, text: &str, steer: bool, cx: &mut Context<Self>) {
        let empty = !sendable(text) && self.attachments.is_empty();
        if self.sending || self.preparing > 0 || empty {
            return;
        }
        let original = text.to_string();
        let trimmed = text.trim().to_string();
        if let Some(id) = self.editing.take() {
            self.replace_queued(&id, &trimmed, original, cx);
            return;
        }
        if self.attachments.is_empty()
            && let Some((name, argument)) = mention::parse_command(&trimmed)
            && self.run_command(name, argument, &trimmed, steer, original.clone(), cx)
        {
            return;
        }
        self.send_prompt(trimmed, None, steer, original, cx);
    }

    /// Sends `text` as the prompt (shown as `display` when that is not it).
    fn send_prompt(
        &mut self,
        text: String,
        display: Option<String>,
        steer: bool,
        original: String,
        cx: &mut Context<Self>,
    ) {
        let request = SendRequest {
            text,
            admission_id: self.next_admission(),
            steer: steer && self.controls.running,
            display,
            attachments: self.attachments.clone(),
        };
        match self.target.clone() {
            Target::Thread(session) => {
                self.dispatch(&session, request, Picks::default(), original, cx)
            }
            Target::Draft(project) => {
                self.sending = true;
                cx.notify();
                let (backend, picks, model) = (
                    self.backend.clone(),
                    self.picks.clone(),
                    self.draft_model.clone(),
                );
                cx.spawn(async move |this, cx| {
                    let made = cx
                        .background_spawn(async move { backend.create_thread(&project, model) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        match made {
                            Ok(session) => {
                                // Still "sending": this screen holds the text until the new
                                // thread opens (and replaces this composer), and a second Enter
                                // meanwhile must not make a second thread.
                                cx.emit(ComposerEvent::Created {
                                    session,
                                    request,
                                    picks,
                                });
                            }
                            Err(why) => {
                                this.sending = false;
                                cx.emit(ComposerEvent::Failed {
                                    text: original,
                                    why,
                                })
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    // --- commands ---------------------------------------------------------------------------

    /// The command called `name` is ours to run (the app's) or the core's (a coding command, a
    /// report, a skill): runs it and returns true. False when the text is no command (the
    /// catalog is read and does not list the name): it is an ordinary prompt.
    fn run_command(
        &mut self,
        name: &str,
        argument: &str,
        typed: &str,
        steer: bool,
        original: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(app) = AppCommand::named(name) {
            cx.emit(ComposerEvent::App(app));
            cx.emit(ComposerEvent::Sent { text: original });
            return true;
        }
        if let Catalog::Ready(listed) = &self.catalog
            && !mention::merged(listed.clone())
                .iter()
                .any(|c| c.name == name)
        {
            return false;
        }
        self.sending = true;
        cx.notify();
        let (backend, session) = (self.backend.clone(), self.session().map(str::to_string));
        let (name, argument, typed) = (name.to_string(), argument.to_string(), typed.to_string());
        cx.spawn(async move |this, cx| {
            let (n, a) = (name.clone(), argument.clone());
            let answered = cx
                .background_spawn(async move { backend.run_command(session.as_deref(), &n, &a) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                match answered {
                    Ok(CommandAnswer::Output(text)) => {
                        this.show_answer(&typed, &text, cx);
                        cx.emit(ComposerEvent::Sent { text: original });
                    }
                    Ok(CommandAnswer::Send { message, display })
                    | Ok(CommandAnswer::Skill { message, display }) => {
                        this.send_prompt(message, Some(display), steer, original, cx)
                    }
                    Ok(CommandAnswer::NotACommand) => {
                        this.send_prompt(typed.clone(), None, steer, original, cx)
                    }
                    Err(why) => cx.emit(ComposerEvent::Problem {
                        title: format!("/{name} did not run"),
                        why,
                    }),
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Shows what a command answered: in the timeline of the thread, or, with no thread yet, as
    /// a notice.
    fn show_answer(&mut self, command: &str, text: &str, cx: &mut Context<Self>) {
        match &self.thread {
            Some(thread) => thread.update(cx, |t, cx| t.show_command_note(command, text, cx)),
            None => cx.emit(ComposerEvent::Answer {
                command: command.to_string(),
                text: text.to_string(),
            }),
        }
    }

    /// The catalog the `/` menu searches: the core's (once read), the reports and the app's.
    pub fn commands(&self) -> Vec<CommandSpec> {
        match &self.catalog {
            Catalog::Ready(listed) => mention::merged(listed.clone()),
            _ => mention::merged(Vec::new()),
        }
    }

    /// Whether the core's commands are still being read.
    pub fn commands_loading(&self) -> bool {
        matches!(self.catalog, Catalog::Idle | Catalog::Loading)
    }

    /// Reads the core's commands (once; again after a failure).
    pub fn load_commands(&mut self, cx: &mut Context<Self>) {
        if matches!(self.catalog, Catalog::Loading | Catalog::Ready(_)) {
            return;
        }
        self.catalog = Catalog::Loading;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx.background_spawn(async move { backend.commands() }).await;
            let _ = this.update(cx, |this, cx| {
                this.catalog = match listed {
                    Ok(commands) => Catalog::Ready(commands),
                    Err(why) => Catalog::Failed(why),
                };
                cx.notify();
            });
        })
        .detach();
    }

    // --- the `@` menu ---------------------------------------------------------------------------

    /// The entries of the `@` menu for the word being typed.
    pub fn mentions(&self) -> Option<&Mentions> {
        self.mentions.as_ref()
    }

    /// Asks the core for the entries of `word` (`@src/ma`). Newest wins: an answer for an older
    /// word is dropped when it lands. The old entries stay shown meanwhile, so the menu does not
    /// flicker as the word grows.
    pub fn request_mentions(&mut self, word: &str, cx: &mut Context<Self>) {
        if self.mentions.as_ref().is_some_and(|m| m.word == word) {
            return;
        }
        self.mention_seq += 1;
        let seq = self.mention_seq;
        let kept = self.mentions.take().map(|m| m.items).unwrap_or_default();
        self.mentions = Some(Mentions {
            word: word.to_string(),
            items: kept,
            loading: true,
            error: None,
        });
        cx.notify();
        let backend = self.backend.clone();
        let scope = match &self.target {
            Target::Thread(id) => CompletionScope::Thread(id.clone()),
            Target::Draft(project) => CompletionScope::Project(project.clone()),
        };
        let asked = word.to_string();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { backend.complete_path(&scope, &asked) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.mention_seq != seq {
                    return;
                }
                let Some(mentions) = this.mentions.as_mut() else {
                    return;
                };
                mentions.loading = false;
                match found {
                    Ok(items) => {
                        mentions.items = items;
                        mentions.error = None;
                    }
                    Err(why) => {
                        mentions.items.clear();
                        mentions.error = Some(why);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The `@` menu is not needed now.
    pub fn clear_mentions(&mut self, cx: &mut Context<Self>) {
        if self.mentions.take().is_some() {
            // Its answer, if one is on its way, is for nobody.
            self.mention_seq += 1;
            cx.notify();
        }
    }

    // --- attachments ----------------------------------------------------------------------------

    /// The attachments waiting to go with the next prompt.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// How many are being read.
    pub fn preparing(&self) -> usize {
        self.preparing
    }

    /// The thumbnail of an image attachment, ready to draw.
    pub fn thumb(&self, id: u64) -> Option<Arc<gpui_kit::Image>> {
        self.thumbs.get(&id).cloned()
    }

    /// Adds files or images (dropped, picked or pasted). They are read off the UI thread; what
    /// the core refuses (too large, not readable) comes back as [`ComposerEvent::Problem`] with
    /// the core's sentence, a file already attached as a notice, and what goes over
    /// [`MAX_ATTACHMENTS`] is left out.
    pub fn add_attachments(&mut self, sources: Vec<AttachmentSource>, cx: &mut Context<Self>) {
        if sources.is_empty() {
            return;
        }
        let room = MAX_ATTACHMENTS.saturating_sub(self.attachments.len() + self.preparing);
        if sources.len() > room {
            cx.emit(ComposerEvent::Problem {
                title: "Too many attachments".into(),
                why: attach::too_many().0,
            });
        }
        let sources: Vec<AttachmentSource> = sources.into_iter().take(room).collect();
        if sources.is_empty() {
            return;
        }
        self.preparing += sources.len();
        cx.notify();
        let backend = self.backend.clone();
        let count = sources.len();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    sources
                        .iter()
                        .map(|source| backend.prepare_attachment(source))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.preparing = this.preparing.saturating_sub(count);
                for prepared in read {
                    match prepared {
                        Ok(prepared) => this.take_prepared(prepared, cx),
                        Err(refusal) => cx.emit(ComposerEvent::Problem {
                            title: "Could not attach".into(),
                            why: refusal.0,
                        }),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn take_prepared(&mut self, prepared: attach::Prepared, cx: &mut Context<Self>) {
        if self.attachments.iter().any(|a| a.key == prepared.key) {
            cx.emit(ComposerEvent::Notice {
                title: "Already attached".into(),
                why: prepared.name,
            });
            return;
        }
        if self.attachments.len() >= MAX_ATTACHMENTS {
            cx.emit(ComposerEvent::Problem {
                title: "Too many attachments".into(),
                why: attach::too_many().0,
            });
            return;
        }
        self.next_attachment += 1;
        let attachment = prepared.into_attachment(self.next_attachment);
        self.keep_thumb(&attachment);
        self.attachments.push(attachment);
    }

    fn keep_thumb(&mut self, attachment: &Attachment) {
        if let Some(png) = &attachment.thumb {
            self.thumbs.insert(
                attachment.id,
                Arc::new(gpui_kit::Image::from_bytes(
                    ImageFormat::Png,
                    png.as_ref().clone(),
                )),
            );
        }
    }

    /// Puts attachments in the strip as they are (stories and tests: nothing is read).
    pub fn seed_attachments(&mut self, prepared: Vec<attach::Prepared>, cx: &mut Context<Self>) {
        for prepared in prepared {
            self.take_prepared(prepared, cx);
        }
        cx.notify();
    }

    /// Takes an attachment off the prompt.
    pub fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.attachments.len();
        self.attachments.retain(|a| a.id != id);
        self.thumbs.remove(&id);
        if self.attachments.len() != before {
            cx.notify();
        }
    }

    /// The images of the attachments as the thread's row shows them.
    fn row_images(attachments: &[Attachment]) -> Vec<crate::thread::data::Attachment> {
        attachments
            .iter()
            .filter(|a| a.kind == attach::Kind::Image)
            .filter_map(|a| {
                a.thumb.as_ref().map(|png| crate::thread::data::Attachment {
                    name: a.name.clone(),
                    mime: "image/png".into(),
                    bytes: png.clone(),
                })
            })
            .collect()
    }

    /// A new admission id for a prompt this composer hands the core.
    fn next_admission(&mut self) -> String {
        self.counter += 1;
        format!("native-{}-{}", crate::model::system_now_ms(), self.counter)
    }

    /// The first prompt of a thread made from the new-thread screen: sent once the app model has
    /// opened the thread, with the picks made there.
    pub fn send_first(&mut self, request: SendRequest, picks: Picks, cx: &mut Context<Self>) {
        let Target::Thread(session) = self.target.clone() else {
            return;
        };
        // A command's prompt gives back what was typed (`/plan x`), not what it loaded.
        let original = request.shown_text().to_string();
        self.dispatch(&session, request, picks, original, cx);
    }

    fn dispatch(
        &mut self,
        session: &str,
        request: SendRequest,
        picks: Picks,
        original: String,
        cx: &mut Context<Self>,
    ) {
        self.sending = true;
        if let Some(thread) = &self.thread {
            let id = request.admission_id.clone();
            let text = request.shown_text().to_string();
            let images = Self::row_images(&request.attachments);
            thread.update(cx, |thread, _| {
                thread.expect_prompt_with(&id, &text, images)
            });
        }
        cx.notify();
        let (backend, session) = (self.backend.clone(), session.to_string());
        let sent_request = request.clone();
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_spawn(async move { backend.send(&session, &sent_request, &picks) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                match sent {
                    Ok(how) => {
                        match how {
                            Sent::Started => {
                                this.controls.running = true;
                            }
                            Sent::Queued => {
                                this.controls.running = true;
                                this.queued_texts
                                    .insert(request.admission_id.clone(), request.text.clone());
                            }
                            Sent::Steered => {
                                // No turn starts for it: the row is shown now.
                                this.drop_expected(&request.admission_id, cx);
                                if let Some(thread) = &this.thread {
                                    thread.update(cx, |t, cx| {
                                        t.show_user_now(request.shown_text(), cx)
                                    });
                                }
                            }
                        }
                        // What went with the prompt is the thread's now; what was added while
                        // the send was on its way stays for the next one.
                        let sent: Vec<u64> = request.attachments.iter().map(|a| a.id).collect();
                        this.attachments.retain(|a| !sent.contains(&a.id));
                        this.thumbs.retain(|id, _| !sent.contains(id));
                        cx.emit(ComposerEvent::Sent { text: original });
                        this.invalidate(cx);
                    }
                    Err(why) => {
                        this.drop_expected(&request.admission_id, cx);
                        // The first send of a new thread fails in a composer that never held
                        // the attachments: they are given back.
                        if this.attachments.is_empty() && !request.attachments.is_empty() {
                            for attachment in &request.attachments {
                                this.keep_thumb(attachment);
                            }
                            this.attachments = request.attachments.clone();
                            // Their ids came from the other composer: the next one is past them.
                            let top = this.attachments.iter().map(|a| a.id).max().unwrap_or(0);
                            this.next_attachment = this.next_attachment.max(top);
                        }
                        cx.emit(ComposerEvent::Failed {
                            text: original,
                            why,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stops the running turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Target::Thread(session) = self.target.clone() else {
            return;
        };
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let stopped = cx
                .background_spawn(async move { backend.stop(&session) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = stopped {
                    cx.emit(ComposerEvent::Problem {
                        title: "Could not stop the turn".into(),
                        why,
                    });
                }
                this.invalidate(cx);
            });
        })
        .detach();
    }

    // --- the queue ----------------------------------------------------------------------------

    /// Takes a queued prompt out of the queue.
    pub fn remove_queued(&mut self, id: &str, cx: &mut Context<Self>) {
        self.queued_texts.remove(id);
        self.drop_expected(id, cx);
        if self.editing.as_deref() == Some(id) {
            self.editing = None;
            cx.emit(ComposerEvent::EditEnded);
        }
        self.queue_op(
            QueueOp::Remove(id.to_string()),
            "Could not remove the prompt",
            cx,
        );
    }

    /// Runs a queued prompt now, inside the running turn ("Send now").
    pub fn steer_queued(&mut self, id: &str, cx: &mut Context<Self>) {
        // It becomes a steer: its row is shown by the turn it joins, not by a turn of its own
        // (also for a prompt another front end queued: its text is the queue's).
        let listed = self
            .controls
            .queue
            .iter()
            .find(|q| q.id == id && !q.shown)
            .map(|q| q.text.clone());
        let text = self.queued_texts.remove(id).or(listed);
        self.drop_expected(id, cx);
        if let (Some(text), Some(thread)) = (text, &self.thread) {
            thread.update(cx, |t, cx| t.show_user_now(&text, cx));
        }
        self.queue_op(
            QueueOp::Steer(id.to_string()),
            "Could not send the prompt now",
            cx,
        );
    }

    /// Puts a queued prompt's text in the input to be changed; the next send replaces it.
    pub fn begin_edit(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.controls.queue.iter().find(|q| q.id == id) else {
            return;
        };
        self.editing = Some(id.to_string());
        cx.emit(ComposerEvent::Edit {
            text: item.text.clone(),
        });
        cx.notify();
    }

    /// Gives up editing a queued prompt (it stays as it was).
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.emit(ComposerEvent::EditEnded);
            cx.notify();
        }
    }

    fn replace_queued(&mut self, id: &str, text: &str, original: String, cx: &mut Context<Self>) {
        self.sending = true;
        cx.notify();
        let (backend, session) = (self.backend.clone(), self.session().map(str::to_string));
        // The edit is a new admission: its row shows when its turn starts, as any queued prompt's
        // (expected before the op, so a turn that starts it at once finds it).
        let new_id = self.next_admission();
        if let Some(thread) = &self.thread {
            thread.update(cx, |t, _| t.expect_prompt(&new_id, text));
        }
        let op = QueueOp::Replace {
            id: id.to_string(),
            text: text.to_string(),
            new_id: new_id.clone(),
        };
        let old = id.to_string();
        let edited = text.to_string();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => backend.queue_op(&session, &op),
                        None => Err("There is no thread to edit.".to_string()),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                match done {
                    Ok(()) => {
                        // The old admission will never start; the new one is ours to give back
                        // should it fail to start.
                        this.queued_texts.remove(&old);
                        this.drop_expected(&old, cx);
                        this.queued_texts.insert(new_id, edited);
                        cx.emit(ComposerEvent::EditEnded);
                        cx.emit(ComposerEvent::Sent { text: original });
                    }
                    Err(why) => {
                        // Still being edited: the text stays where it is.
                        this.drop_expected(&new_id, cx);
                        this.editing = Some(old);
                        cx.emit(ComposerEvent::Failed {
                            text: original,
                            why,
                        });
                    }
                }
                this.invalidate(cx);
            });
        })
        .detach();
    }

    fn queue_op(&mut self, op: QueueOp, title: &'static str, cx: &mut Context<Self>) {
        let (backend, session) = (self.backend.clone(), self.session().map(str::to_string));
        let Some(session) = session else {
            return;
        };
        // The item leaves the list at once; the read that follows says what the core has.
        match &op {
            QueueOp::Remove(id) | QueueOp::Steer(id) => {
                self.controls.queue.retain(|q| &q.id != id);
                cx.notify();
            }
            QueueOp::Replace { .. } => {}
        }
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move { backend.queue_op(&session, &op) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = done {
                    cx.emit(ComposerEvent::Problem {
                        title: title.into(),
                        why,
                    });
                }
                this.invalidate(cx);
            });
        })
        .detach();
    }

    // --- the footer's picks -------------------------------------------------------------------

    /// Asks the endpoints for their models (once; again after a failure).
    pub fn load_choices(&mut self, cx: &mut Context<Self>) {
        if matches!(self.choices, Choices::Loading | Choices::Ready(_)) {
            return;
        }
        self.choices = Choices::Loading;
        cx.notify();
        let (backend, session) = (self.backend.clone(), self.session().map(str::to_string));
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move { backend.model_choices(session.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.choices = match listed {
                    Ok(choices) => Choices::Ready(choices),
                    Err(why) => Choices::Failed(why),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Picks the model of the next turn (applied from the next turn, as the core defers a pick
    /// made while a turn runs). On the new-thread screen it also becomes the default for new
    /// threads.
    pub fn pick_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        // A read in flight started before the pick: it is read again, not shown.
        self.generation += 1;
        self.controls.provider = provider.to_string();
        self.controls.model = model.to_string();
        self.controls.provider_name = self
            .provider_name(provider)
            .unwrap_or_else(|| provider.to_string());
        cx.notify();
        let (backend, target) = (self.backend.clone(), self.target.clone());
        let (provider, model) = (provider.to_string(), model.to_string());
        if matches!(target, Target::Draft(_)) {
            self.draft_model = Some((provider.clone(), model.clone()));
        }
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    match target {
                        Target::Thread(session) => backend.pick(
                            &session,
                            &Pick::Model {
                                provider: provider.clone(),
                                model: model.clone(),
                            },
                        ),
                        Target::Draft(_) => backend.set_default_model(&provider, &model),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = done {
                    cx.emit(ComposerEvent::Problem {
                        title: "Could not change the model".into(),
                        why,
                    });
                    this.invalidate(cx);
                } else if this.session().is_some() {
                    // A pick made mid-turn is the core's to defer: read what it says.
                    this.invalidate(cx);
                }
            });
        })
        .detach();
    }

    /// Picks the reasoning effort (`None`: Settings' again).
    pub fn pick_effort(&mut self, effort: Option<&str>, cx: &mut Context<Self>) {
        self.generation += 1;
        self.controls.effort = effort.unwrap_or_default().to_string();
        cx.notify();
        let effort = effort.map(str::to_string);
        match self.target.clone() {
            Target::Draft(_) => self.picks.effort = Some(effort),
            Target::Thread(session) => self.apply_pick(session, Pick::Effort(effort), cx),
        }
    }

    /// Picks the permission mode.
    pub fn pick_mode(&mut self, mode: super::data::Mode, cx: &mut Context<Self>) {
        self.generation += 1;
        self.controls.mode = mode;
        cx.notify();
        match self.target.clone() {
            Target::Draft(_) => self.picks.mode = Some(mode),
            Target::Thread(session) => self.apply_pick(session, Pick::Mode(mode), cx),
        }
    }

    fn apply_pick(&mut self, session: String, pick: Pick, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move { backend.pick(&session, &pick) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = done {
                    cx.emit(ComposerEvent::Problem {
                        title: "Could not change that".into(),
                        why,
                    });
                }
                // What the core now says (a read that raced the pick is not the last word).
                this.invalidate(cx);
            });
        })
        .detach();
    }
}
