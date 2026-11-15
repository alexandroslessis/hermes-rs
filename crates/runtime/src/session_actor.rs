//! The per-session actor (backend v2 Phase 13 CP13d): one thread per session owns that session's
//! control state, and everything else asks it through a [`SessionHandle`]. C++ terms: a worker
//! thread with a message queue that owns its data; Python terms: a thread reading a
//! `queue.Queue` of commands, the only one that touches the object they change.
//!
//! What the actor owns (`SessionState`): which [`OpenSession`] is loaded; the **run slot** (the
//! turn or `/compress` holding the session, its cancel token, the turn's steer box), claimed and
//! freed through [`crate::session::RunGuard`]; the agentic review's slot and the paused review's
//! start; Hermes's review counters; and [`PageState`], what the desktop gateway keeps for the
//! session between the page's calls (queued prompts, live, the model picked mid-turn, the effort
//! pick, the revision, images held for the next prompt). The session's *data* (record,
//! conversation, last request's size) stays on `OpenSession` under its own locks: the running
//! turn writes it per streamed chunk and is its only writer while it holds the run slot; a
//! review's or a subagent's copy has no actor. Each command is handled to the end before the next
//! one is read, in the order the commands were sent, so a check and the change it allows (is a
//! turn running? then queue the prompt) are one step that nothing can come between.
//!
//! Stop is the exception to "everything goes through the queue": `ClaimRun` publishes the turn's
//! cancel token and [`SessionHandle::cancel`] cancels it from the status at once, even while
//! commands wait ahead of it.
//!
//! Rules (read `docs/PITFALLS.md` "The session actor" before changing anything here):
//! - **The actor never blocks.** A handler only changes `SessionState`: no file, store, network or
//!   process work, no `.await`, no waiting on another handle (its own or another session's), no
//!   lock that a caller may hold while it waits for an answer. So a caller's wait for an answer is
//!   bounded by in-memory work, like taking a mutex, and may be done from any thread: an async
//!   worker, a blocking thread, the GPUI app's own threads.
//! - **No re-entrancy.** A command sent from the actor's own thread would wait for itself; such a
//!   call is refused with [`Gone::Reentrant`] (and a debug assertion).
//! - **Nothing hangs.** A command whose actor has stopped answers [`Gone::Stopped`]: the send
//!   fails, or the reply's sender is dropped with the command (a handler that panicked, a thread
//!   that ended). A handler's panic is caught and logged; the actor goes on with the next command.
//!   An actor whose loop died anyway is replaced by the registry on the session's next use
//!   (its last published turn is stopped), so a session never stays unusable.
//! - **Reads without a round trip.** After every command the actor publishes a [`SessionStatus`]
//!   (one `Arc`, swapped whole, so its fields are consistent with each other). It reflects every
//!   command whose answer the reader already has. Every command in the app is waited for for
//!   that reason (fire-and-forget is test-only); [`SessionHandle::barrier`] waits for everything
//!   sent before it. A read that must be ordered against another caller's change asks a command
//!   instead (`ReviewSlotTaken`, the `only_if_running` forms).
//! - **One outside lock.** `ClaimReview` runs the review scheduler's busy check, which takes its
//!   `states` lock: nobody may wait for an actor while holding `states` (the scheduler never does).
//! - **A thread, not a task.** `Runtime::start_turn` and the other claimers are synchronous and
//!   are called from one-thread tokio runtimes (the `turns` tests, the GPUI app): a task-based
//!   actor could not run while that thread waits for its answer.
//! - **Unbounded queue.** Commands are sent at the pace of user actions and turn boundaries (never
//!   per streamed chunk), and senders include code that must not wait (drops, other locks held):
//!   a bounded queue would only add a way to block.
//!
//! Hermes: no counterpart (Hermes keeps this state on its Python session objects, guarded by the
//! GIL and a few `threading.Lock`s); ours.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::ops::SharedTurnState;
use crate::review::Nudges;
use crate::session::{OpenSession, ReviewHandle};
use hermes_llm::conversation::message::Message;

/// The actor thread's stack: its handlers are small, in-memory functions.
const ACTOR_STACK: usize = 256 * 1024;

thread_local! {
    /// Set on an actor's own thread: a call to any handle from there would wait for an actor
    /// that cannot answer while it waits (itself), so it is refused.
    static ON_ACTOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Why a command was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gone {
    /// The session's actor has stopped (its thread ended), or the command's handler panicked
    /// (details in the log).
    Stopped,
    /// The call came from an actor's own thread (a bug: see the module's rules).
    Reentrant,
}

impl fmt::Display for Gone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gone::Stopped => f.write_str(
                "the session's state stopped answering (details in the log); reopen the session",
            ),
            Gone::Reentrant => f.write_str("a session's state was asked from its own thread"),
        }
    }
}

impl std::error::Error for Gone {}

/// A command refused because a turn (or `/compress`) holds the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

/// Where a command's answer goes: a waiting thread or an awaiting task. Every command in the
/// app is waited for (so its effect is in the status the caller reads next); tests also send
/// unanswered ones.
pub(crate) enum Reply<T> {
    Sync(mpsc::SyncSender<T>),
    Async(tokio::sync::oneshot::Sender<T>),
    #[cfg(test)]
    Ignore,
}

impl<T: Send + 'static> Reply<T> {
    /// The answer, to be delivered once the status that includes it is published.
    fn with(self, value: T) -> Deliver {
        Box::new(move || match self {
            Reply::Sync(sender) => {
                let _ = sender.send(value);
            }
            Reply::Async(sender) => {
                let _ = sender.send(value);
            }
            #[cfg(test)]
            Reply::Ignore => {}
        })
    }
}

/// An answer waiting to be sent: the actor publishes the new status first, so a caller that has
/// its answer also sees the state it describes.
type Deliver = Box<dyn FnOnce() + Send>;

/// What the actor can be asked. Each variant is handled in [`SessionState::handle`].
pub(crate) enum Command {
    /// The session was loaded: keep `open` unless one is loaded already (two callers loading at
    /// once keep the first), and answer the one kept.
    /// `nudges` are the review counters for the loaded conversation (`Nudges::resumed`): they
    /// start over with each load, as they did when they lived on the loaded session.
    /// The answer is the one kept and, when another was loaded first, the caller's own copy back,
    /// so that copy (its tools) is dropped on the caller's thread, never the actor's.
    Attach {
        open: Arc<OpenSession>,
        nudges: Nudges,
        reply: Reply<(Arc<OpenSession>, Option<Arc<OpenSession>>)>,
    },
    /// Forget the loaded session (close, delete, a workspace move). `only_idle`: refused, with
    /// [`Busy`], while a turn runs (the check and the drop are one step).
    Detach {
        only_idle: bool,
        reply: Reply<Result<Option<Arc<OpenSession>>, Busy>>,
    },
    /// A turn (or `/compress`) takes the session's run slot: refused with [`Busy`] while another
    /// holds it. Answers the claim's number. The claim's cancel token and the turn's loop state
    /// (where `/steer` text waits) are published with it, so Stop and steering need no command.
    ClaimRun {
        cancel: CancellationToken,
        turn_state: Option<SharedTurnState>,
        reply: Reply<Result<u64, Busy>>,
    },
    /// Claim `claim` ended (its [`crate::session::RunGuard`] was dropped). A number that is not
    /// the slot's (already released) changes nothing.
    Release { claim: u64, reply: Reply<()> },
    /// A paused review asks whether no turn runs: the claim count now if so (see `ConfirmIdle`).
    IdleEpoch { reply: Reply<Option<u64>> },
    /// A paused review copied the conversation after `IdleEpoch` answered `epoch`: `true`, and
    /// `started` set, when no turn has claimed the session since (so the copy was taken while
    /// none ran); `false` means copy again later.
    ConfirmIdle {
        epoch: u64,
        started: Arc<AtomicBool>,
        reply: Reply<bool>,
    },
    /// The agentic review's slot (`ReviewSetup::start`): one review per session, a paused one
    /// replaced (its job taken over), a started one left to finish. `evidence_busy` is the
    /// scheduler's check, run here so the check and the claim are one step (the scheduler's own
    /// claim asks `ReviewSlotTaken` after setting its flag: one of the two sees the other).
    ClaimReview {
        memory: bool,
        skills: bool,
        requested: bool,
        evidence_busy: Box<dyn FnOnce() -> bool + Send>,
        reply: Reply<ReviewClaim>,
    },
    /// The review whose `started` flag this is has ended: its slot is freed (a newer review that
    /// replaced it keeps the slot).
    ReleaseReview {
        started: Arc<AtomicBool>,
        reply: Reply<()>,
    },
    /// Whether an agentic review holds the slot (the scheduler's side of the pair above).
    ReviewSlotTaken { reply: Reply<bool> },
    /// A user message starts a turn: counts it toward the memory review (`Nudges::tick_memory`).
    TickMemory { reply: Reply<bool> },
    /// A turn finished with these messages: counts them toward the skill review
    /// (`Nudges::finish_turn`).
    FinishTurn {
        turn: Vec<Arc<Message>>,
        reply: Reply<bool>,
    },
    /// The page's queue (`prompt.submit` while a turn runs): `params` at the back, or the front
    /// for a redirect or a steer the turn missed. `only_if_running`: only while a turn holds the
    /// session (the check and the queueing are one step); answers whether it was queued.
    Enqueue {
        params: serde_json::Value,
        front: bool,
        only_if_running: bool,
        reply: Reply<bool>,
    },
    /// Takes the queued prompt whose `submission_id` is `admission` out of the queue.
    Dequeue {
        admission: String,
        reply: Reply<Option<serde_json::Value>>,
    },
    /// Takes the front of the queue (a turn's end runs it).
    TakeNext {
        reply: Reply<Option<serde_json::Value>>,
    },
    /// Sets `PageState::live`.
    SetLive { live: bool, reply: Reply<()> },
    /// A model picked mid-turn: kept for the turn's end (`None` withdraws an earlier pick), but
    /// only while a turn holds the session; answers whether it was kept (if not, the caller
    /// switches the model now: the check and the keeping are one step, and an earlier pick the
    /// ended turn has not taken yet is dropped, so it can't override this newer one).
    SetPendingRoute {
        route: Option<(String, String)>,
        reply: Reply<bool>,
    },
    /// Takes the pick made during the turn (its end applies it).
    TakePendingRoute {
        reply: Reply<Option<(String, String)>>,
    },
    /// Counts an edit; answers the new revision.
    BumpRevision { reply: Reply<u64> },
    /// Holds an image for the next prompt; answers how many are held.
    AttachImage { path: String, reply: Reply<usize> },
    /// Drops a held image; answers whether one was dropped and how many are left.
    DetachImage {
        path: String,
        reply: Reply<(bool, usize)>,
    },
    /// Takes the held images (a prompt is sent with them).
    TakeImages { reply: Reply<Vec<String>> },
    /// Sets the effort picked for the session (`None`: back to Settings').
    SetEffort {
        effort: Option<String>,
        reply: Reply<()>,
    },
    /// Answers once every command sent before it has been handled.
    Barrier { reply: Reply<()> },
    /// Test only: a handler that panics.
    #[cfg(test)]
    Panic { reply: Reply<()> },
    /// Test only: runs `work` on the actor's thread (to check the re-entrancy refusal).
    #[cfg(test)]
    OnActor {
        work: Box<dyn FnOnce() + Send>,
        reply: Reply<()>,
    },
    /// Test only: the actor's loop ends at once, as if its thread had died.
    #[cfg(test)]
    Exit,
}

/// What the desktop gateway keeps for a session between the page's calls (moved from its
/// per-session maps). The runtime only stores it: its meaning is the protocol's.
#[derive(Clone)]
pub struct PageState {
    /// Prompts sent while the turn ran (`prompt.submit` params), run one per turn's end, front
    /// first.
    pub queue: VecDeque<serde_json::Value>,
    /// The page holds the session's turn as live: from a turn's start through the queued
    /// prompts that follow it (`running` in `session.info` is this or a running turn).
    pub live: bool,
    /// The provider and model picked while the turn ran, applied when it ends.
    pub pending_route: Option<(String, String)>,
    /// The reasoning effort picked in the composer for this session (`None`: Settings').
    pub effort: Option<String>,
    /// The session's edit counter (Hermes's CAS revision), starting at 1.
    pub revision: u64,
    /// Images attached for the next prompt (`image.attach`, `image.attach_bytes`): file paths.
    pub images: Vec<String>,
}

impl Default for PageState {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            live: false,
            pending_route: None,
            effort: None,
            revision: 1,
            images: Vec::new(),
        }
    }
}

/// What `ClaimReview` decided.
pub(crate) enum ReviewClaim {
    /// Start this review (its memory / skills cover what a replaced paused one was to cover).
    Start(ReviewHandle),
    /// Not started; the reason is logged as "review skipped: <reason>".
    Skipped(&'static str),
}

/// The run slot as published: who holds it, and how to reach that turn without a command.
#[derive(Clone)]
pub(crate) struct RunInfo {
    pub(crate) claim: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) turn_state: Option<SharedTurnState>,
}

/// What the actor publishes after every command (see the module doc).
#[derive(Default)]
pub struct SessionStatus {
    /// The loaded session's data, if loaded.
    pub open: Option<Arc<OpenSession>>,
    /// The turn (or `/compress`) holding the session.
    pub(crate) run: Option<RunInfo>,
    /// The agentic review in its slot (paused or running).
    pub(crate) review: Option<ReviewHandle>,
    /// The page's state for the session.
    pub page: PageState,
}

/// The state one actor owns.
#[derive(Default)]
struct SessionState {
    open: Option<Arc<OpenSession>>,
    run: Option<RunInfo>,
    /// Run claims made so far: a claim's number, and a paused review's "has a turn come and
    /// gone since?" check.
    claims: u64,
    review: Option<ReviewHandle>,
    /// Hermes's nudge counters, which decide when a turn is followed by a review.
    nudges: Nudges,
    page: PageState,
}

impl SessionState {
    fn handle(&mut self, command: Command) -> Deliver {
        match command {
            Command::Attach {
                open,
                nudges,
                reply,
            } => match &self.open {
                Some(kept) => reply.with((kept.clone(), Some(open))),
                None => {
                    self.nudges = nudges;
                    self.open = Some(open.clone());
                    reply.with((open, None))
                }
            },
            Command::TickMemory { reply } => reply.with(self.nudges.tick_memory()),
            Command::FinishTurn { turn, reply } => reply.with(self.nudges.finish_turn(&turn)),
            Command::Detach { only_idle, reply } => {
                if only_idle && self.run.is_some() {
                    reply.with(Err(Busy))
                } else {
                    reply.with(Ok(self.open.take()))
                }
            }
            Command::ClaimRun {
                cancel,
                turn_state,
                reply,
            } => {
                if self.run.is_some() {
                    return reply.with(Err(Busy));
                }
                self.claims += 1;
                let claim = self.claims;
                self.run = Some(RunInfo {
                    claim,
                    cancel,
                    turn_state,
                });
                reply.with(Ok(claim))
            }
            Command::Release { claim, reply } => {
                if self.run.as_ref().is_some_and(|run| run.claim == claim) {
                    self.run = None;
                }
                reply.with(())
            }
            Command::IdleEpoch { reply } => {
                let idle = self.run.is_none().then_some(self.claims);
                reply.with(idle)
            }
            Command::ConfirmIdle {
                epoch,
                started,
                reply,
            } => {
                let still = self.run.is_none() && self.claims == epoch;
                if still {
                    started.store(true, Ordering::SeqCst);
                }
                reply.with(still)
            }
            Command::ClaimReview {
                memory,
                skills,
                requested,
                evidence_busy,
                reply,
            } => reply.with(self.claim_review(memory, skills, requested, evidence_busy)),
            Command::ReleaseReview { started, reply } => {
                if self
                    .review
                    .as_ref()
                    .is_some_and(|review| Arc::ptr_eq(&review.started, &started))
                {
                    self.review = None;
                }
                reply.with(())
            }
            Command::ReviewSlotTaken { reply } => reply.with(self.review.is_some()),
            Command::Enqueue {
                params,
                front,
                only_if_running,
                reply,
            } => {
                if only_if_running && self.run.is_none() {
                    return reply.with(false);
                }
                if front {
                    self.page.queue.push_front(params);
                } else {
                    self.page.queue.push_back(params);
                }
                reply.with(true)
            }
            Command::Dequeue { admission, reply } => {
                let index = self
                    .page
                    .queue
                    .iter()
                    .position(|params| params["submission_id"] == admission.as_str());
                let taken = index.and_then(|index| self.page.queue.remove(index));
                reply.with(taken)
            }
            Command::TakeNext { reply } => reply.with(self.page.queue.pop_front()),
            Command::SetLive { live, reply } => {
                self.page.live = live;
                reply.with(())
            }
            Command::SetPendingRoute { route, reply } => {
                if self.run.is_none() {
                    // The caller switches (or keeps) the model now: a pick made during the turn
                    // that just ended, not yet taken by its end, must not be applied after this
                    // newer one (the last pick wins).
                    self.page.pending_route = None;
                    return reply.with(false);
                }
                self.page.pending_route = route;
                reply.with(true)
            }
            Command::TakePendingRoute { reply } => reply.with(self.page.pending_route.take()),
            Command::AttachImage { path, reply } => {
                self.page.images.push(path);
                reply.with(self.page.images.len())
            }
            Command::DetachImage { path, reply } => {
                let before = self.page.images.len();
                self.page.images.retain(|held| *held != path);
                let left = self.page.images.len();
                reply.with((left != before, left))
            }
            Command::TakeImages { reply } => reply.with(std::mem::take(&mut self.page.images)),
            Command::BumpRevision { reply } => {
                self.page.revision += 1;
                reply.with(self.page.revision)
            }
            Command::SetEffort { effort, reply } => {
                self.page.effort = effort;
                reply.with(())
            }
            Command::Barrier { reply } => reply.with(()),
            #[cfg(test)]
            Command::Panic { reply } => {
                let _reply = reply;
                panic!("a test handler panicked");
            }
            #[cfg(test)]
            Command::OnActor { work, reply } => {
                work();
                reply.with(())
            }
            // Handled by the loop before it gets here.
            #[cfg(test)]
            Command::Exit => Box::new(|| {}),
        }
    }

    /// `ClaimReview` (moved from `ReviewSetup::start`, where it ran under the `running` and
    /// `review` locks): the paused review sets `started` only through `ConfirmIdle`, so the check
    /// of `started` here and the cancel that follows are one step with respect to it.
    fn claim_review(
        &mut self,
        memory: bool,
        skills: bool,
        requested: bool,
        evidence_busy: Box<dyn FnOnce() -> bool + Send>,
    ) -> ReviewClaim {
        // One review per session at a time (Phase 7): an evidence review holds it.
        if evidence_busy() {
            return ReviewClaim::Skipped("an evidence review of the session is running");
        }
        let (mut memory, mut skills) = (memory, skills);
        if let Some(previous) = self.review.as_ref() {
            // One already past its first request finishes (cancelling it would waste what it
            // spent); a newly due review is skipped, except one the user asked for with /refine,
            // which replaces it. The skipped review's turns aren't lost: every review reads the
            // whole conversation, so the next one due sees them.
            if previous.has_started() && !requested {
                return ReviewClaim::Skipped("one is still running");
            }
            if !previous.has_started() {
                // A paused /refine stays (it covers memory and skills, and the user waits for its
                // report); a paused due review is replaced by this one, which will see the same
                // conversation, and takes over what it was to cover.
                if previous.requested && !requested {
                    return ReviewClaim::Skipped("a /refine is waiting to run");
                }
                memory |= previous.memory;
                skills |= previous.skills;
            }
            previous.cancel.cancel();
        }
        let handle = ReviewHandle::new(memory, skills, requested);
        self.review = Some(handle.clone());
        ReviewClaim::Start(handle)
    }

    fn status(&self) -> SessionStatus {
        SessionStatus {
            open: self.open.clone(),
            run: self.run.clone(),
            review: self.review.clone(),
            page: self.page.clone(),
        }
    }
}

struct Inner {
    id: String,
    commands: mpsc::Sender<Command>,
    status: Arc<RwLock<Arc<SessionStatus>>>,
    /// Cleared when the actor's loop ends, however it ends (see [`SessionRegistry::get`]).
    alive: Arc<AtomicBool>,
}

/// One session's actor, as callers hold it (cheap to clone). The actor's thread ends when the
/// last handle is dropped, after the commands already sent.
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<Inner>,
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionHandle")
            .field("id", &self.inner.id)
            .finish()
    }
}

impl SessionHandle {
    /// Starts the actor of session `id` on its own thread.
    fn spawn(id: &str) -> std::io::Result<Self> {
        let (commands, inbox) = mpsc::channel::<Command>();
        let status: Arc<RwLock<Arc<SessionStatus>>> = Arc::default();
        let published = status.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let living = alive.clone();
        // The id's end tells sessions apart in a debugger's thread list.
        let short: String = id
            .chars()
            .skip(id.chars().count().saturating_sub(8))
            .collect();
        std::thread::Builder::new()
            .name(format!("hermes-session-{short}"))
            .stack_size(ACTOR_STACK)
            .spawn(move || run(SessionState::default(), inbox, published, living))?;
        Ok(Self {
            inner: Arc::new(Inner {
                id: id.to_string(),
                commands,
                status,
                alive,
            }),
        })
    }

    /// Whether the actor's loop still runs (it ends only once every handle is gone, unless
    /// something outside a handler panicked on its thread: a bug, logged).
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// The session's id.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The status as of the last command handled (see the module doc for what it reflects).
    pub fn status(&self) -> Arc<SessionStatus> {
        self.inner.status.read().clone()
    }

    /// The loaded session's data, if it is loaded.
    pub fn open(&self) -> Option<Arc<OpenSession>> {
        self.status().open.clone()
    }

    /// Sends a command and waits for its answer on this thread (bounded: handlers are in-memory
    /// work). Never call it from an actor's thread (refused).
    pub(crate) fn ask<T: Send + 'static>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, Gone> {
        if ON_ACTOR.with(|on| on.get()) {
            debug_assert!(false, "a session actor called a session handle");
            return Err(Gone::Reentrant);
        }
        let (sender, answer) = mpsc::sync_channel(1);
        self.inner
            .commands
            .send(command(Reply::Sync(sender)))
            .map_err(|_| Gone::Stopped)?;
        answer.recv().map_err(|_| Gone::Stopped)
    }

    /// [`Self::ask`] for async code: the task waits, the worker does not.
    pub(crate) async fn ask_async<T: Send + 'static>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, Gone> {
        let (sender, answer) = tokio::sync::oneshot::channel();
        self.inner
            .commands
            .send(command(Reply::Async(sender)))
            .map_err(|_| Gone::Stopped)?;
        answer.await.map_err(|_| Gone::Stopped)
    }

    /// Sends a command whose answer nobody waits for (tests). `false` when the actor has stopped.
    #[cfg(test)]
    pub(crate) fn tell(&self, command: Command) -> bool {
        self.inner.commands.send(command).is_ok()
    }

    /// Keeps `open` as the loaded session (with its review counters) unless another caller
    /// loaded it first; the one kept.
    pub(crate) fn attach(
        &self,
        open: Arc<OpenSession>,
        nudges: Nudges,
    ) -> Result<Arc<OpenSession>, Gone> {
        let (kept, refused) = self.ask(|reply| Command::Attach {
            open,
            nudges,
            reply,
        })?;
        // A copy loaded by a caller that lost the race is dropped here, not on the actor.
        drop(refused);
        Ok(kept)
    }

    /// Forgets the loaded session (and answers it); `only_idle` refuses with [`Busy`] while a turn
    /// runs.
    pub fn detach(&self, only_idle: bool) -> Result<Result<Option<Arc<OpenSession>>, Busy>, Gone> {
        self.ask(|reply| Command::Detach { only_idle, reply })
    }

    /// Returns once every command sent before it has been handled (and is in [`Self::status`]).
    pub fn barrier(&self) -> Result<(), Gone> {
        self.ask(|reply| Command::Barrier { reply })
    }

    /// The page's state as of the last command.
    pub fn page(&self) -> PageState {
        self.status().page.clone()
    }

    /// Queues a prompt for the page (see `Command::Enqueue`); whether it was queued.
    pub fn enqueue(
        &self,
        params: serde_json::Value,
        front: bool,
        only_if_running: bool,
    ) -> Result<bool, Gone> {
        self.ask(|reply| Command::Enqueue {
            params,
            front,
            only_if_running,
            reply,
        })
    }

    /// Takes a queued prompt out by its admission id.
    pub fn dequeue(&self, admission: &str) -> Result<Option<serde_json::Value>, Gone> {
        let admission = admission.to_string();
        self.ask(|reply| Command::Dequeue { admission, reply })
    }

    /// Takes the queue's front.
    pub fn take_next(&self) -> Result<Option<serde_json::Value>, Gone> {
        self.ask(|reply| Command::TakeNext { reply })
    }

    /// Keeps a model picked mid-turn (see `Command::SetPendingRoute`); `false` when no turn runs.
    pub fn set_pending_route(&self, route: Option<(String, String)>) -> Result<bool, Gone> {
        self.ask(|reply| Command::SetPendingRoute { route, reply })
    }

    /// Holds an image for the next prompt; how many are held.
    pub fn attach_image(&self, path: &str) -> Result<usize, Gone> {
        let path = path.to_string();
        self.ask(|reply| Command::AttachImage { path, reply })
    }

    /// Drops a held image: whether one was dropped, and how many are left.
    pub fn detach_image(&self, path: &str) -> Result<(bool, usize), Gone> {
        let path = path.to_string();
        self.ask(|reply| Command::DetachImage { path, reply })
    }

    /// Takes the held images.
    pub fn take_images(&self) -> Result<Vec<String>, Gone> {
        self.ask(|reply| Command::TakeImages { reply })
    }

    /// Counts an edit of the session; the new revision.
    pub fn bump_revision(&self) -> Result<u64, Gone> {
        self.ask(|reply| Command::BumpRevision { reply })
    }

    /// Sets the effort picked for the session (`None`: Settings' again).
    pub fn set_effort(&self, effort: Option<String>) -> Result<(), Gone> {
        self.ask(|reply| Command::SetEffort { effort, reply })
    }

    /// Takes the model picked during the turn.
    pub fn take_pending_route(&self) -> Result<Option<(String, String)>, Gone> {
        self.ask(|reply| Command::TakePendingRoute { reply })
    }

    /// Sets whether the page holds the session's turn as live.
    pub fn set_live(&self, live: bool) -> Result<(), Gone> {
        self.ask(|reply| Command::SetLive { live, reply })
    }

    /// The page's "a turn is in flight": a turn runs, or the page holds it live (between a turn
    /// and its queued follow-up). Both read from one published status.
    pub fn is_live(&self) -> bool {
        let status = self.status();
        status.run.is_some() || status.page.live
    }

    /// Whether a turn (or `/compress`) holds the session.
    pub fn is_running(&self) -> bool {
        self.status().run.is_some()
    }

    /// Stop: cancels the running turn's token directly, never behind queued commands (the
    /// priority path). Nothing runs: nothing happens.
    pub fn cancel(&self) {
        if let Some(run) = &self.status().run {
            run.cancel.cancel();
        }
    }

    /// The running turn's loop state (where `/steer` text waits), if a turn runs.
    pub(crate) fn turn_state(&self) -> Option<SharedTurnState> {
        self.status().run.as_ref()?.turn_state.clone()
    }

    /// Whether an agentic review holds the review slot (published; for a decision that must
    /// see a concurrent claim, ask `ReviewSlotTaken`).
    pub fn reviewing(&self) -> bool {
        self.status().review.is_some()
    }
}

/// The actor's loop: one command at a time, in the order sent, until every handle is gone.
fn run(
    mut state: SessionState,
    inbox: mpsc::Receiver<Command>,
    status: Arc<RwLock<Arc<SessionStatus>>>,
    alive: Arc<AtomicBool>,
) {
    /// Clears `alive` when the loop ends, by its normal end or by unwinding.
    struct Ended(Arc<AtomicBool>);
    impl Drop for Ended {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
            if std::thread::panicking() {
                tracing::error!("a session actor's thread panicked outside a handler; it stopped");
            }
        }
    }
    let _ended = Ended(alive);
    ON_ACTOR.with(|on| on.set(true));
    while let Ok(command) = inbox.recv() {
        #[cfg(test)]
        if matches!(command, Command::Exit) {
            // Test only: the loop ends as if its thread had died.
            return;
        }
        // A panicking handler drops its command, and with it the reply's sender: the caller gets
        // `Gone::Stopped` instead of waiting. Handlers change the state only in their last step,
        // so the state they leave is whole.
        let deliver =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.handle(command)));
        *status.write() = Arc::new(state.status());
        match deliver {
            // Delivering drops the answer here when its caller has gone (an awaiting task that was
            // dropped): caught too, so a panicking drop can't end the actor.
            Ok(deliver) => {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(deliver)).is_err() {
                    tracing::error!(
                        "delivering a session actor's answer panicked; the actor goes on"
                    );
                }
            }
            Err(_) => tracing::error!("a session actor's command panicked; the actor goes on"),
        }
    }
}

/// Every session's handle, by id: the one map keyed by session that stays (Phase 13 CP13d).
#[derive(Default)]
pub struct SessionRegistry {
    handles: Mutex<HashMap<String, SessionHandle>>,
}

impl SessionRegistry {
    /// The session's handle, if its actor runs (a read never starts one). An actor whose loop
    /// died (a bug outside its handlers, logged) is dropped from the registry here, and the turn
    /// it last published is stopped (Stop could no longer reach it through the registry): the
    /// session's next use starts a fresh actor instead of every call answering
    /// [`Gone::Stopped`] for the rest of the process. What that actor kept for the page is lost.
    pub fn get(&self, id: &str) -> Option<SessionHandle> {
        let mut handles = self.handles.lock();
        let handle = handles.get(id)?;
        if handle.is_alive() {
            return Some(handle.clone());
        }
        let dead = handles.remove(id)?;
        drop(handles);
        dead.cancel();
        None
    }

    /// The session's handle, starting its actor on first use (no store or file work); a dead
    /// one is replaced (see [`Self::get`]).
    pub fn get_or_spawn(&self, id: &str) -> std::io::Result<SessionHandle> {
        let mut handles = self.handles.lock();
        let dead = match handles.get(id) {
            Some(handle) if handle.is_alive() => return Ok(handle.clone()),
            Some(_) => handles.remove(id),
            None => None,
        };
        let handle = SessionHandle::spawn(id)?;
        handles.insert(id.to_string(), handle.clone());
        drop(handles);
        if let Some(dead) = dead {
            dead.cancel();
        }
        Ok(handle)
    }

    /// Removes the session's handle (it was deleted). Its actor ends once the commands already
    /// sent are handled and the last other holder (a turn still ending) lets go.
    pub fn remove(&self, id: &str) -> Option<SessionHandle> {
        self.handles.lock().remove(id)
    }

    /// Every live handle now.
    pub fn all(&self) -> Vec<SessionHandle> {
        self.handles
            .lock()
            .values()
            .filter(|handle| handle.is_alive())
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[path = "session_actor/tests.rs"]
mod tests;
