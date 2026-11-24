//! The app-level model: the one GPUI entity the views read the projects and threads from, and the
//! only thing that talks to the backend ([`Backend`]: `hermes_core::Core`, or a fixture). Views
//! never call the runtime or the core (docs/native-ui/README.md, "Architecture"): they read this
//! model, and ask it to do things.
//!
//! What it holds: the last [`Snapshot`] (projects and threads), which thread is open
//! ([`Selection`]), and the marks the core's list does not have (a thread that finished while the
//! user was in another is *unread*, one whose last turn failed is *failed*; both for this run, as
//! t3code's `lastVisitedAt` is per browser).
//!
//! How it works, and why the list never waits:
//! - **Edits are optimistic.** [`AppModel::apply`] changes the snapshot at once
//!   ([`Snapshot::apply`]), then runs the [`Op`] on GPUI's background executor, then re-reads the
//!   list so the core's answer wins; a failure shows a toast and the re-read puts the old state
//!   back. Edits reach the backend **one at a time, in the order the user made them** (a queue):
//!   two quick renames of one thread must not land in the store the other way round. No handler or `render` ever blocks (ground rule 7: the core's store, file and git work
//!   happens on the background thread inside [`Backend`]).
//! - **One re-read at a time**, coalesced: events that change the list (a turn started or ended, a
//!   title) set a dirty flag while a read is in flight; a read that arrives while an edit is still
//!   running is dropped (the edit's own re-read follows).
//! - **Core events** arrive on a channel a GPUI task drains ([`AppModel::on_event`]): turn
//!   start and end set the working, unread and failed marks, and a few become toasts
//!   ([`ModelEvent::Toast`]): a failed turn, a queued prompt that could not start, the curator's
//!   result, a background review's changes. The workspace subscribes and shows them.
//!
//! Start-up: [`AppModel::starting`] is an empty model that shows nothing yet;
//! [`AppModel::connect`] gives it its backend (the app builds the `Core` on a background thread
//! and calls it when ready, so the window opens at once).
//!
//! The open thread (Phase 4): opening a thread makes its [`ThreadModel`] (its history is read a
//! page at a time from the backend), and the core's events for that thread are handed to it
//! before the model looks at them itself. One thread model at a time: a thread that is not open
//! keeps streaming in the core and shows what it stored when it is opened.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use gpui_kit::{AppContext as _, Context, Entity, EventEmitter, Subscription, Task};
use hermes_core::CoreEvent;
use hermes_runtime::TurnStatus;

use crate::design::toast::{Kind, ToastSpec};
use crate::sidebar::data::{Marks, Op, Snapshot};
use crate::thread::{ThreadEvent, ThreadModel};

pub mod backend;
pub mod fixture;

pub use backend::{Backend, CoreBackend, FixtureBackend};

/// What is open in the main area.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    /// Nothing (the "pick a thread" state).
    #[default]
    None,
    /// A stored thread.
    Thread(String),
    /// A new thread not yet sent (nothing is stored until the first message, Phase 5), in the
    /// project with this id.
    Draft(String),
}

/// Where the model is in starting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The backend is being opened; nothing to show yet.
    Starting,
    Ready,
    /// The backend could not be opened (why).
    Failed(String),
}

/// What the model tells the views beyond "something changed".
#[derive(Clone, Debug, PartialEq)]
pub enum ModelEvent {
    /// A notice to show; `open` is a thread its action opens.
    Toast {
        spec: ToastSpec,
        open: Option<String>,
    },
}

/// The projects and threads, the selection and the marks.
pub struct AppModel {
    backend: Option<Arc<dyn Backend>>,
    snapshot: Snapshot,
    selected: Selection,
    unread: HashSet<String>,
    failed: HashSet<String>,
    /// Threads the events say are working or waiting: they stay so across a re-read that ran
    /// before the core caught up, until their end event.
    working: HashSet<String>,
    waiting: HashSet<String>,
    phase: Phase,
    clock: fn() -> i64,
    reading: bool,
    dirty: bool,
    /// Edits made and not answered by the backend yet (the first one is running).
    queue: VecDeque<Op>,
    /// A list was read (or given) at least once.
    loaded: bool,
    events_task: Option<Task<()>>,
    /// The open thread's model.
    thread: Option<Entity<ThreadModel>>,
    /// The clock the open thread's durations use.
    thread_clock: Option<fn() -> i64>,
    /// What the open thread tells (a branch was made).
    thread_subscription: Option<Subscription>,
    /// A thread made by "Branch from here", opened as soon as the list has it.
    open_when_listed: Option<String>,
}

impl EventEmitter<ModelEvent> for AppModel {}

/// Milliseconds since the epoch.
pub fn system_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl AppModel {
    /// A model whose backend is still being opened.
    pub fn starting() -> Self {
        Self {
            backend: None,
            snapshot: Snapshot::default(),
            selected: Selection::None,
            unread: HashSet::new(),
            failed: HashSet::new(),
            working: HashSet::new(),
            waiting: HashSet::new(),
            phase: Phase::Starting,
            clock: system_now_ms,
            reading: false,
            dirty: false,
            queue: VecDeque::new(),
            loaded: false,
            events_task: None,
            thread: None,
            thread_clock: None,
            thread_subscription: None,
            open_when_listed: None,
        }
    }

    /// A model over `backend`, read at once (a fixture answers on the spot; the real core on its
    /// background read).
    pub fn with_backend(backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        let mut model = Self::starting();
        model.connect(backend, cx);
        model
    }

    /// A model over `backend` that already shows `snapshot` (what the backend holds): nothing
    /// waits for the first read. For fixtures, the gallery and tests.
    pub fn with_snapshot(
        backend: Arc<dyn Backend>,
        snapshot: Snapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self::starting();
        model.snapshot = snapshot;
        model.loaded = true;
        model.attach(backend, cx);
        model.phase = Phase::Ready;
        model
    }

    /// The fixture model (fake projects and threads, the fixed clock).
    pub fn fixture(cx: &mut Context<Self>) -> Self {
        let snapshot = fixture::snapshot();
        let backend = FixtureBackend::new(snapshot.clone());
        Self::with_snapshot(backend, snapshot, cx).with_clock(|| fixture::NOW_MS)
    }

    /// Starts with this thread open (the stories build their state this way: a story is rebuilt
    /// on every frame, so it must not notify anyone).
    pub fn with_open(mut self, id: &str) -> Self {
        self.selected = Selection::Thread(id.to_string());
        self
    }

    /// Starts with these threads marked finished-and-unseen.
    pub fn with_unread(mut self, ids: &[&str]) -> Self {
        self.unread = ids.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Starts with these threads marked failed.
    pub fn with_failed(mut self, ids: &[&str]) -> Self {
        self.failed = ids.iter().map(|s| s.to_string()).collect();
        self
    }

    /// The clock the relative times use.
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// Gives a starting model its backend: reads the list and follows the core's events.
    pub fn connect(&mut self, backend: Arc<dyn Backend>, cx: &mut Context<Self>) {
        self.phase = Phase::Ready;
        self.attach(backend, cx);
        self.refresh(cx);
        cx.notify();
    }

    /// Follows the backend's events and uses it for edits, without reading the list.
    fn attach(&mut self, backend: Arc<dyn Backend>, cx: &mut Context<Self>) {
        if let Some(events) = backend.events() {
            self.events_task = Some(cx.spawn(async move |this, cx| {
                let mut events = events;
                while let Some(event) = events.recv().await {
                    if this
                        .update(cx, |this, cx| this.on_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        self.backend = Some(backend);
    }

    /// The backend could not be opened.
    pub fn fail(&mut self, why: String, cx: &mut Context<Self>) {
        self.phase = Phase::Failed(why);
        cx.notify();
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn selected(&self) -> &Selection {
        &self.selected
    }

    /// Now, for relative times.
    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// The marks the status of a thread needs besides the core's list.
    pub fn marks(&self) -> Marks<'_> {
        Marks {
            unread: Some(&self.unread),
            failed: Some(&self.failed),
        }
    }

    /// Marks a thread finished-and-unseen, or not (fixtures and the stories; a core event does it
    /// in the app).
    pub fn set_unread(&mut self, id: &str, unread: bool, cx: &mut Context<Self>) {
        if unread {
            self.unread.insert(id.to_string());
        } else {
            self.unread.remove(id);
        }
        cx.notify();
    }

    /// Marks a thread whose last turn failed, or not.
    pub fn set_failed(&mut self, id: &str, failed: bool, cx: &mut Context<Self>) {
        if failed {
            self.failed.insert(id.to_string());
        } else {
            self.failed.remove(id);
        }
        cx.notify();
    }

    pub fn unread(&self) -> &HashSet<String> {
        &self.unread
    }

    pub fn failed(&self) -> &HashSet<String> {
        &self.failed
    }

    /// The id of the open thread, if one is open.
    pub fn open_thread(&self) -> Option<&str> {
        match &self.selected {
            Selection::Thread(id) => Some(id),
            _ => None,
        }
    }

    /// The open thread's model (once [`Self::open`] made it).
    pub fn thread(&self) -> Option<&Entity<ThreadModel>> {
        self.thread.as_ref()
    }

    /// The clock the open thread's durations use (fixtures: a fixed one).
    pub fn with_thread_clock(mut self, clock: fn() -> i64) -> Self {
        self.thread_clock = Some(clock);
        self
    }

    /// Makes the open thread's model if it is not the selection's yet, or drops it when nothing
    /// is open.
    fn sync_thread(&mut self, cx: &mut Context<Self>) {
        let wanted = self.open_thread().map(str::to_string);
        let current = self
            .thread
            .as_ref()
            .map(|t| t.read(cx).session().to_string());
        if wanted == current {
            return;
        }
        self.thread = match (wanted, self.backend.clone()) {
            (Some(id), Some(backend)) => {
                let clock = self.thread_clock;
                Some(cx.new(|cx| {
                    let model = ThreadModel::new(&id, backend, cx);
                    match clock {
                        Some(clock) => model.with_clock(clock),
                        None => model,
                    }
                }))
            }
            _ => None,
        };
        self.thread_subscription = self.thread.as_ref().map(|thread| {
            cx.subscribe(thread, |this, _, event: &ThreadEvent, cx| {
                if let ThreadEvent::Branched(id) = event {
                    this.open_when_listed = Some(id.clone());
                    this.refresh(cx);
                }
            })
        });
    }

    /// The project the main area is about: the open thread's, or the draft's.
    pub fn current_project(&self) -> Option<&str> {
        match &self.selected {
            Selection::Thread(id) => self.snapshot.thread(id).map(|t| t.project.as_str()),
            Selection::Draft(project) => Some(project),
            Selection::None => None,
        }
    }

    // --- choosing ---------------------------------------------------------------------------

    /// Opens a thread (it is seen: no longer unread).
    pub fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.snapshot.thread(id).is_none() {
            return;
        }
        self.unread.remove(id);
        self.selected = Selection::Thread(id.to_string());
        self.sync_thread(cx);
        cx.notify();
    }

    /// Starts a new thread in a project (a draft: nothing is stored until it is sent).
    pub fn new_thread(&mut self, project: &str, cx: &mut Context<Self>) {
        self.selected = Selection::Draft(project.to_string());
        self.sync_thread(cx);
        cx.notify();
    }

    /// Closes whatever is open.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = Selection::None;
        self.sync_thread(cx);
        cx.notify();
    }

    // --- reading ----------------------------------------------------------------------------

    /// Re-reads the list from the backend, in the background. Reads do not pile up: one runs at a
    /// time and a request made meanwhile runs once more after it.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if self.reading {
            self.dirty = true;
            return;
        }
        self.reading = true;
        cx.spawn(async move |this, cx| {
            loop {
                let reader = backend.clone();
                let result = cx.background_spawn(async move { reader.snapshot() }).await;
                let again = this.update(cx, |this, cx| {
                    match result {
                        Ok(snapshot) if this.queue.is_empty() => this.install(snapshot, cx),
                        // An edit is still running: its own re-read follows.
                        Ok(_) => {}
                        // Nothing was ever read: the main area says why instead of
                        // showing a first-run hero over a list it could not read.
                        Err(why) if !this.loaded => this.fail(why, cx),
                        Err(why) => this.toast(
                            ToastSpec::new(Kind::Error, "Could not read your threads")
                                .description(why),
                            None,
                            cx,
                        ),
                    }
                    let again = std::mem::take(&mut this.dirty);
                    if !again {
                        this.reading = false;
                    }
                    again
                });
                match again {
                    Ok(true) => continue,
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn install(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.loaded = true;
        if self.phase != Phase::Ready {
            self.phase = Phase::Ready;
            cx.notify();
        }
        // What the user has open may have been removed from another place (an archive, another
        // window): the main area must not show a thread that is gone.
        if let Selection::Thread(id) = &self.selected
            && snapshot.thread(id).is_none()
        {
            self.selected = Selection::None;
            self.sync_thread(cx);
        }
        let ids: HashSet<&str> = snapshot.threads.iter().map(|t| t.id.as_str()).collect();
        self.unread.retain(|id| ids.contains(id.as_str()));
        self.failed.retain(|id| ids.contains(id.as_str()));
        self.working.retain(|id| ids.contains(id.as_str()));
        self.waiting.retain(|id| ids.contains(id.as_str()));
        let mut snapshot = snapshot;
        for thread in &mut snapshot.threads {
            thread.working |= self.working.contains(&thread.id);
            thread.waiting |= self.waiting.contains(&thread.id);
        }
        if snapshot != self.snapshot {
            self.snapshot = snapshot;
            cx.notify();
        }
        // A branch just made: open it once the list has it.
        if let Some(id) = self.open_when_listed.take() {
            if self.snapshot.thread(&id).is_some() {
                self.open(&id, cx);
            } else {
                self.open_when_listed = Some(id);
            }
        }
    }

    // --- editing ----------------------------------------------------------------------------

    /// Does an edit: the list changes now, the backend does it in the background, the list is read
    /// again when it has. A failure is a toast and the list goes back to what the backend says.
    pub fn apply(&mut self, op: Op, cx: &mut Context<Self>) {
        self.snapshot.apply(&op);
        if let Op::Archive { id } | Op::Delete { id } = &op
            && self.selected == Selection::Thread(id.clone())
        {
            self.selected = Selection::None;
            self.sync_thread(cx);
        }
        cx.notify();
        if self.backend.is_none() {
            return;
        }
        self.queue.push_back(op);
        if self.queue.len() == 1 {
            self.run_queue(cx);
        }
    }

    /// Runs the queued edits, oldest first, each after the last has answered; re-reads the list
    /// when the queue is empty.
    fn run_queue(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(op)) = (self.backend.clone(), self.queue.front().cloned()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let done = op.clone();
            let result = cx.background_spawn(async move { backend.run(&done) }).await;
            let _ = this.update(cx, |this, cx| {
                this.queue.pop_front();
                if let Err(why) = result {
                    this.toast(
                        ToastSpec::new(Kind::Error, failure_title(&op)).description(why),
                        None,
                        cx,
                    );
                }
                if this.queue.is_empty() {
                    this.refresh(cx);
                } else {
                    this.run_queue(cx);
                }
            });
        })
        .detach();
    }

    /// Edits not answered yet (tests).
    pub fn pending_edits(&self) -> usize {
        self.queue.len()
    }

    // --- events -----------------------------------------------------------------------------

    fn toast(&mut self, spec: ToastSpec, open: Option<String>, cx: &mut Context<Self>) {
        cx.emit(ModelEvent::Toast { spec, open });
    }

    fn title_of(&self, session: &str) -> String {
        self.snapshot
            .thread(session)
            .map_or_else(|| "A thread".to_string(), |t| t.display_title().to_string())
    }

    fn set_flags(&mut self, session: &str, working: Option<bool>, waiting: Option<bool>) {
        let mark = |set: &mut HashSet<String>, on: bool| {
            if on {
                set.insert(session.to_string());
            } else {
                set.remove(session);
            }
        };
        if let Some(on) = working {
            mark(&mut self.working, on);
        }
        if let Some(on) = waiting {
            mark(&mut self.waiting, on);
        }
        if let Some(thread) = self.snapshot.threads.iter_mut().find(|t| t.id == session) {
            if let Some(working) = working {
                thread.working = working;
            }
            if let Some(waiting) = waiting {
                thread.waiting = waiting;
            }
        }
    }

    /// Takes in one core event (the channel's task calls it; tests call it directly). The open
    /// thread sees its own events first.
    pub fn on_event(&mut self, event: CoreEvent, cx: &mut Context<Self>) {
        if let Some(thread) = &self.thread
            && thread.read(cx).session() == event.session_id()
        {
            thread.update(cx, |thread, cx| thread.on_event(&event, cx));
        }
        match event {
            CoreEvent::TurnStarted { session_id, .. } => {
                self.failed.remove(&session_id);
                self.set_flags(&session_id, Some(true), None);
                self.refresh(cx);
                cx.notify();
            }
            CoreEvent::TurnEnded {
                session_id,
                status,
                end,
            } => {
                self.set_flags(&session_id, Some(false), None);
                let failed = match &status {
                    TurnStatus::Cancelled => false,
                    TurnStatus::Failed(_) => true,
                    TurnStatus::Completed => end.error.is_some(),
                };
                if failed {
                    self.failed.insert(session_id.clone());
                    let why = match &status {
                        TurnStatus::Failed(why) => Some(why.clone()),
                        _ => end.error.clone(),
                    };
                    let title = self.title_of(&session_id);
                    let mut spec = ToastSpec::new(Kind::Error, "A turn failed")
                        .description(match why {
                            Some(why) => format!("{title}: {why}"),
                            None => title,
                        })
                        .action("Open thread");
                    spec.ttl = None;
                    self.toast(spec, Some(session_id.clone()), cx);
                } else if status == TurnStatus::Completed
                    && self.open_thread() != Some(session_id.as_str())
                {
                    self.unread.insert(session_id.clone());
                }
                self.refresh(cx);
                cx.notify();
            }
            CoreEvent::Idle { session_id } => {
                self.set_flags(&session_id, Some(false), None);
                self.refresh(cx);
                cx.notify();
            }
            CoreEvent::ApprovalRequested { session_id, .. }
            | CoreEvent::ClarifyRequested { session_id, .. } => {
                self.set_flags(&session_id, None, Some(true));
                cx.notify();
            }
            CoreEvent::Settled { session_id, .. } => {
                self.set_flags(&session_id, None, Some(false));
                cx.notify();
            }
            CoreEvent::TitleChanged { session_id, title } => {
                if let Some(thread) = self
                    .snapshot
                    .threads
                    .iter_mut()
                    .find(|t| t.id == session_id)
                {
                    thread.title = title;
                    cx.notify();
                }
            }
            CoreEvent::QueuedFailed {
                session_id, error, ..
            } => {
                let title = self.title_of(&session_id);
                let spec = ToastSpec::new(Kind::Warning, "A queued prompt could not start")
                    .description(format!("{title}: {error}"))
                    .action("Open thread");
                self.toast(spec, Some(session_id), cx);
            }
            CoreEvent::CuratorFinished { ok, lines } => {
                let summary = lines.first().cloned().unwrap_or_default();
                let spec = if ok {
                    ToastSpec::new(Kind::Success, "The skills curator finished")
                } else {
                    ToastSpec::new(Kind::Error, "The skills curator failed")
                };
                let spec = if summary.is_empty() {
                    spec
                } else {
                    spec.description(summary)
                };
                self.toast(spec, None, cx);
            }
            CoreEvent::ReviewFinished {
                session_id,
                actions,
                requested,
            } => {
                if !actions.is_empty() || requested {
                    let title = self.title_of(&session_id);
                    let what = if actions.is_empty() {
                        "Nothing needed saving.".to_string()
                    } else {
                        actions.join("; ")
                    };
                    let spec = ToastSpec::new(Kind::Success, "Review finished")
                        .description(format!("{title}: {what}"));
                    self.toast(spec, Some(session_id), cx);
                }
            }
            _ => {}
        }
    }
}

/// The toast title when an edit fails.
fn failure_title(op: &Op) -> &'static str {
    match op {
        Op::Rename { .. } => "Could not rename the thread",
        Op::Pin { .. } => "Could not pin the thread",
        Op::Archive { .. } => "Could not archive the thread",
        Op::Delete { .. } => "Could not delete the thread",
        Op::Move { .. } => "Could not move the thread",
        Op::AddProject { .. } => "Could not add the project",
        Op::RenameProject { .. } => "Could not rename the project",
        Op::RemoveProject { .. } => "Could not remove the project",
    }
}
