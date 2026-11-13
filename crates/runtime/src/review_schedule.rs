//! When the evidence review (`review_once.rs`) runs: Phase 7 of `docs/backend/PHASES.md`, CP7.3.
//!
//! Not from Hermes (it reviews every 10 user turns or 10 model calls, `review.rs`); `PORTING.md`
//! marks the review `diverged`. Under `review_mode = idle_once` (the `requests` profile) a session
//! is reviewed **once, when it goes quiet**, and only when its learning signals (`signals.rs`)
//! reached the `review_threshold` knob (or the user said "remember this" / `/learn`):
//!
//! - **Idle.** Every session has a timer, armed when activity ends (a turn ends, a rewind, the
//!   page opens the session) for `review_idle_minutes` (default 10) and disarmed while a turn
//!   runs. A timer is a sleeping tokio task holding only a weak handle: it never spins and never
//!   keeps anything alive (the app exits with it pending; dropping the `Runtime` aborts it).
//! - **Closed or switched away.** The page switching to another session (the gateway sees a
//!   `session.resume` of another id on the same socket) and an editor closing a thread (ACP
//!   `session/close` → `Runtime::close`) arm the timer for [`LEFT_DELAY`] instead: coming back
//!   within it is activity again, so flicking between chats starts nothing. Closing the window ends
//!   the app; nothing is started then (see "exit" below).
//! - **One at a time.** A session has at most one review in flight, of either kind: a trigger
//!   while the evidence review runs, or while an agentic review (`/refine`, or a `nudges` review)
//!   is running or paused, is dropped (the signals stay unreviewed and are picked up next time);
//!   the agentic review skips while an evidence review runs, and `/refine` is refused.
//! - **Never cancelled by a turn.** The review works on what it read at its start (signals,
//!   conversation, skills, memory) and writes only memory and skills, so a turn that starts
//!   meanwhile runs beside it (Phase 4's rule for the agentic review). Deleting the session aborts
//!   it.
//! - **What it covers.** The evidence and its highest `seq` are taken when the review starts;
//!   signals recorded while it runs get higher numbers and stay unreviewed. Signals are marked
//!   reviewed only after a reply was read and its operations applied, staged or refused (CP7.2).
//! - **Back-off.** A review that failed without sending anything (the model's window too small
//!   for a packet) would fail the same way: the session waits [`BACKOFF_NO_REQUEST`]. One that
//!   failed after a request (a network error, a reply unreadable twice, a daily-limit 429, which
//!   also teaches the ledger the limit) waits [`BACKOFF_FIRST`], doubling up to
//!   [`BACKOFF_MAX`]. A review the quota governor held back costs nothing and is tried at the next
//!   idle. That back-off is per process; what survives a restart is the signals file's count of
//!   reviews started that never covered the signals (`SessionSignals::retry_wait`): the first is
//!   redone freely, from the second on the session waits an hour, doubling to a day, so an app
//!   that crashes (or is closed) mid-review again and again can't send a review per start.
//! - **Hourly cap.** At most `review_per_hour` evidence reviews start per hour, across sessions; a
//!   session over the cap is re-armed for when the window frees (timers that all went off at once,
//!   say on waking from sleep, start at most that many). In memory: a restart opens a new window,
//!   but after a restart only sessions the page opens again are armed. The governor (Phase 2) is asked
//!   before every request on top of that (background work stops below 20% of the daily quota).
//! - **Exit and restart.** Signals are on disk (CP7.1) and nothing is marked reviewed before the
//!   reply was applied, so an app that exits mid-review loses only that request: the review is
//!   redone later. After a restart, a session with an unreviewed score at the threshold is
//!   reviewed when it next goes idle in this process, i.e. after the page opens it (or a turn, or
//!   a switch away from it); a session never opened again keeps its signals. There is no sweep
//!   over old sessions at startup, so an app start can't send a burst of requests.
//! - **Which model.** The gateway installs a route resolver ([`RouteFn`]: the session's provider
//!   and its turn options, auxiliary review model included, as a turn would get them now); without
//!   one (the runtime alone, tests) the last turn's route in this process is used.
//!
//! What it learned is reported through `UserBridge::review_finished` (the desktop logs it, as for
//! the agentic review; staged items show in Settings ▸ Memory / Capabilities ▸ Skills ▸ Pending).
//! No page event: Hermes's page has none for a background review's result.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use hermes_llm::conversation::message::Message;
use hermes_llm::observer::Purpose;
use hermes_memory::MemoryStore;
use hermes_store::Store;
use hermes_tools::UserBridge;
use hermes_tools::index::LocalIndex;
use hermes_tools::skills::SkillStore;
use hermes_tools::taint::Taint;
use parking_lot::Mutex;
use tokio::task::AbortHandle;

use crate::budget::{BudgetKnobs, ReviewMode};
use crate::review_once::{self, ReviewOnceSettings, ReviewOutcome, ReviewStatus};
use crate::session::OpenSession;
use crate::{ProviderConfig, TurnOptions, governor, ledger, signals};

/// How long a review waits after the user switched away from (or closed) a session: coming back
/// within it is activity, which re-arms the full idle time.
pub const LEFT_DELAY: Duration = Duration::from_secs(3);
/// A review that failed before sending anything waits this long before the next try.
pub const BACKOFF_NO_REQUEST: Duration = Duration::from_secs(24 * 3600);
/// A review that failed after sending a request waits this long, doubling per failure in a row.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(3600);
pub const BACKOFF_MAX: Duration = Duration::from_secs(24 * 3600);
const HOUR: Duration = Duration::from_secs(3600);

/// The provider and turn options a session's review should use now (the gateway's: the
/// session's provider, its window, the auxiliary review model). Called on a blocking thread.
pub type RouteFn = Arc<dyn Fn(&str) -> Option<(ProviderConfig, TurnOptions)> + Send + Sync>;

/// What the scheduler knows about one session (for tests, logs and a status line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReviewScheduleStatus {
    /// A timer is waiting to start the session's review.
    pub timer_armed: bool,
    /// The session's evidence review is running now.
    pub reviewing: bool,
    /// Evidence reviews started for the session in this process.
    pub attempts: u32,
    /// Failed reviews in a row.
    pub failures: u32,
    /// The next try waits for the back-off.
    pub backed_off: bool,
    /// How the last review ended.
    pub last: Option<ReviewOutcome>,
}

#[derive(Default)]
struct SessionState {
    /// The armed timer and its number (a timer that fires checks it is still the current one).
    timer: Option<(u64, AbortHandle)>,
    /// An evidence review holds the session (set by [`Scheduler::claim`]).
    busy: bool,
    /// The running scheduled review's task (aborted when the session is deleted).
    task: Option<AbortHandle>,
    /// The route of the session's last turn in this process.
    route: Option<(ProviderConfig, TurnOptions)>,
    attempts: u32,
    failures: u32,
    retry_after: Option<Instant>,
    last: Option<ReviewOutcome>,
    /// `nudges`: when the signal score last started an agentic review, and the session's
    /// `reviewed_seq` then (see [`Scheduler::nudge_due`]).
    nudged: Option<(Instant, u64)>,
}

/// What a review needs from the runtime: shared handles, so a timer can run one without the
/// `Runtime` itself.
pub(crate) struct Env {
    pub store: Arc<Store>,
    pub memory: Arc<MemoryStore>,
    pub skills: Arc<SkillStore>,
    pub index: Option<Arc<LocalIndex>>,
    pub ledger: ledger::Ledger,
    pub signals: signals::SignalStore,
    pub data_dir: PathBuf,
    pub bridge: Arc<dyn UserBridge>,
    /// The runtime's knobs now.
    pub budget: Arc<Mutex<BudgetKnobs>>,
    /// Hermes's `background_review.enabled`.
    pub enabled: Arc<AtomicBool>,
    /// The runtime's open sessions.
    pub open: Arc<Mutex<HashMap<String, Arc<OpenSession>>>>,
}

/// A session as the review reads it: from its open state, else from the store.
struct View {
    messages: Vec<Message>,
    model: String,
    cwd: PathBuf,
    taint: Taint,
    /// The repository the session works in (its tools' project, or the one its stored folder is
    /// in): whose skills the review may read and write besides the user's own.
    project: Option<PathBuf>,
}

impl Env {
    fn open_session(&self, session_id: &str) -> Option<Arc<OpenSession>> {
        self.open.lock().get(session_id).cloned()
    }

    fn turn_running(&self, session_id: &str) -> bool {
        self.open_session(session_id)
            .is_some_and(|open| open.running.lock().is_some())
    }

    async fn view(&self, session_id: &str) -> Result<View> {
        if let Some(open) = self.open_session(session_id) {
            let messages = open.conversation.lock().messages().clone();
            let model = open.record.lock().model.clone();
            return Ok(View {
                messages,
                model,
                cwd: open.tools.cwd(),
                taint: open.tools.taint(),
                project: open.tools.project_root(),
            });
        }
        // Closed (switched away, or after a restart): what the store and the taint file say.
        let (store, data_dir, id) = (
            self.store.clone(),
            self.data_dir.clone(),
            session_id.to_string(),
        );
        tokio::task::spawn_blocking(move || -> Result<View> {
            let record = store
                .session(&id)?
                .with_context(|| format!("no session {id}"))?;
            let messages = store.load_messages(&id)?;
            let taint = hermes_tools::taint::load(&data_dir, &id)
                .map(|sources| Taint {
                    turn: false,
                    session: true,
                    sources,
                })
                .unwrap_or_default();
            let cwd = PathBuf::from(record.cwd);
            Ok(View {
                messages,
                model: record.model,
                project: hermes_tools::project_skills::find_root(&cwd),
                cwd,
                taint,
            })
        })
        .await?
    }

    /// The evidence review of a session (CP7.2): see `Runtime::review_once`. The caller holds the
    /// session's claim.
    pub(crate) async fn review_once(
        &self,
        session_id: &str,
        provider: &ProviderConfig,
        options: &TurnOptions,
        settings: ReviewOnceSettings,
    ) -> Result<ReviewOutcome> {
        // What the review covers is fixed here, at its start.
        let evidence: Vec<signals::Signal> = self
            .signals
            .snapshot(session_id)
            .await
            .evidence()
            .into_iter()
            .cloned()
            .collect();
        if evidence.is_empty() {
            return Ok(ReviewOutcome::nothing());
        }
        let view = self.view(session_id).await?;
        let budget = *self.budget.lock();
        let (llm, model, base_url, context_length, effort_params, provider_id) =
            match &options.aux.review {
                Some(aux) => (
                    aux.provider.one_shot_provider(budget.http_retries)?,
                    aux.model.clone(),
                    aux.provider.base_url.clone(),
                    aux.context_length,
                    crate::aux_effort_params(aux),
                    aux.provider.id.clone(),
                ),
                None => (
                    provider.one_shot_provider(budget.http_retries)?,
                    view.model.clone(),
                    provider.base_url.clone(),
                    options.context_length,
                    options
                        .effort
                        .as_deref()
                        .map(|effort| crate::effort_request_params(&provider.base_url, effort)),
                    provider.id.clone(),
                ),
            };
        if let governor::Verdict::Deny(why) =
            governor::Governor::new(self.ledger.clone()).check(&provider_id, Purpose::Review)
        {
            tracing::info!("review skipped: {why}");
            return Ok(ReviewOutcome::skipped(why));
        }
        let query = hermes_tools::index::IndexQuery {
            message: review_once::topic(&view.messages, &evidence),
            last_assistant: None,
            project: hermes_tools::index::project_name(&view.cwd),
            project_root: view.project.clone(),
        };
        let (memory, skills, index) =
            (self.memory.clone(), self.skills.clone(), self.index.clone());
        let messages = view.messages;
        let (cwd, project, scratch) = (
            view.cwd.clone(),
            view.project.clone(),
            self.data_dir.join("scratch"),
        );
        // Both resolve paths and read files: off the async workers.
        let (input, default_scope) = tokio::task::spawn_blocking(move || {
            let scope =
                review_once::evidence_scope(&messages, &cwd, project.as_deref(), &scratch);
            let input = review_once::gather(&messages, &memory, &skills, index.as_deref(), query);
            (input, scope)
        })
        .await?;
        // A project skill in the packet (loaded, or found by the related-skills lookup) is the
        // repository's text going to a model that can write: held like a tainted session's.
        let packet_project_skill = input
            .loaded
            .iter()
            .chain(&input.related)
            .find(|skill| skill.project)
            .map(|skill| hermes_tools::taint::project_skill_source(&skill.name));
        // A session that read something the user didn't write may carry an injection into the
        // evidence: everything it would write waits for the user (Phase 6).
        let hold = if view.taint.is_tainted() {
            Some(review_once::Hold {
                reason: view
                    .taint
                    .reason()
                    .unwrap_or_else(|| "this session read untrusted content".into()),
                sources: view.taint.sources.clone(),
            })
        } else if let Some(source) = packet_project_skill {
            Some(review_once::Hold {
                reason: format!("the review read {source}"),
                sources: vec![source],
            })
        } else if settings.require_approval {
            Some(review_once::Hold {
                reason: "review.require_approval is on".into(),
                sources: Vec::new(),
            })
        } else {
            None
        };
        let job = review_once::OnceJob {
            model: review_once::ReviewModel {
                llm,
                model,
                base_url,
                context_length,
                effort_params,
            },
            settings,
            input,
            apply: review_once::ApplyContext {
                memory: self.memory.clone(),
                skills: self.skills.clone(),
                staging: hermes_tools::staging::StagingStore::new(&self.data_dir),
                session_id: session_id.to_string(),
                hold,
                shown: Default::default(),
                project: view.project.clone(),
                default_scope,
            },
            store: self.store.clone(),
        };
        // On disk before the request: a review cut off by an exit or a crash counts as a try in
        // the next run of the app too (`SessionSignals::retry_wait`); a reply read resets it.
        self.signals.review_started(session_id);
        let context = self.ledger.context(Purpose::Review, Some(session_id));
        let outcome =
            hermes_llm::observer::scope(context, review_once::run_review_once(job, evidence)).await;
        if let Some(seq) = outcome.reviewed_seq {
            self.signals.mark_reviewed(session_id, seq);
        }
        // A daily-limit 429 teaches the ledger the provider's limit, as a turn's does.
        if let ReviewStatus::Failed(text) = &outcome.status
            && hermes_llm::retry::is_usage_limit(text)
            && let Some(limit) = ledger::daily_limit_in(text)
        {
            self.ledger.learn_limit(&provider_id, limit);
        }
        Ok(outcome)
    }
}

/// The review scheduler (see the module doc). Cheap to clone.
#[derive(Clone)]
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
}

struct Shared {
    env: Env,
    states: Mutex<HashMap<String, SessionState>>,
    /// When the evidence reviews of the last hour started (the hourly cap).
    started: Mutex<VecDeque<Instant>>,
    route: Mutex<Option<RouteFn>>,
    next_timer: std::sync::atomic::AtomicU64,
    /// The runtime is going away ([`Scheduler::shutdown`]): nothing is armed or started any more.
    closed: AtomicBool,
}

/// A session's review slot, held while its evidence review runs; dropping it frees the slot.
pub(crate) struct Claim {
    shared: Arc<Shared>,
    session_id: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(state) = self.shared.states.lock().get_mut(&self.session_id) {
            state.busy = false;
            state.task = None;
        }
    }
}

impl Scheduler {
    pub fn new(env: Env) -> Self {
        Self {
            shared: Arc::new(Shared {
                env,
                states: Mutex::new(HashMap::new()),
                started: Mutex::new(VecDeque::new()),
                route: Mutex::new(None),
                next_timer: Default::default(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn env(&self) -> &Env {
        &self.shared.env
    }

    /// Installs (or removes) the route resolver.
    pub fn set_route(&self, route: Option<RouteFn>) {
        *self.shared.route.lock() = route;
    }

    /// A turn started: its provider and options are the fallback route for the session's review,
    /// and no review timer runs while it does.
    pub fn turn_started(&self, session_id: &str, provider: &ProviderConfig, options: &TurnOptions) {
        let mut states = self.shared.states.lock();
        let state = states.entry(session_id.to_string()).or_default();
        state.route = Some((provider.clone(), options.clone()));
        if let Some((_, timer)) = state.timer.take() {
            timer.abort();
        }
    }

    /// Activity ended (a turn ended, a rewind, the page opened the session): the review waits for
    /// `review_idle_minutes` of quiet from now.
    pub fn idle_from_now(&self, session_id: &str) {
        let minutes = self.shared.env.budget.lock().review_idle_minutes;
        self.arm(session_id, Duration::from_secs_f64(minutes.max(0.0) * 60.0));
    }

    /// The user switched away from the session or closed it: its review runs after
    /// [`LEFT_DELAY`] (or the idle time, when that is shorter).
    pub fn left(&self, session_id: &str) {
        let minutes = self.shared.env.budget.lock().review_idle_minutes;
        let idle = Duration::from_secs_f64(minutes.max(0.0) * 60.0);
        self.arm(session_id, LEFT_DELAY.min(idle));
    }

    /// The session was deleted: no timer, no review, nothing remembered.
    pub fn forget(&self, session_id: &str) {
        if let Some(state) = self.shared.states.lock().remove(session_id) {
            if let Some((_, timer)) = state.timer {
                timer.abort();
            }
            if let Some(task) = state.task {
                task.abort();
            }
        }
    }

    /// Every timer and scheduled review stops (the runtime is going away). Nothing is lost: the
    /// signals are on disk and an interrupted review marked nothing reviewed.
    pub fn shutdown(&self) {
        // Set before the drain: a timer past its checks (it already dropped its handle, so it
        // can't be aborted) sees it before it starts a review.
        self.shared.closed.store(true, Ordering::SeqCst);
        for (_, state) in self.shared.states.lock().drain() {
            if let Some((_, timer)) = state.timer {
                timer.abort();
            }
            if let Some(task) = state.task {
                task.abort();
            }
        }
    }

    pub fn status(&self, session_id: &str) -> ReviewScheduleStatus {
        let states = self.shared.states.lock();
        let Some(state) = states.get(session_id) else {
            return ReviewScheduleStatus::default();
        };
        ReviewScheduleStatus {
            timer_armed: state.timer.is_some(),
            reviewing: state.busy,
            attempts: state.attempts,
            failures: state.failures,
            backed_off: state.retry_after.is_some_and(|t| Instant::now() < t),
            last: state.last.clone(),
        }
    }

    /// Whether an evidence review holds the session.
    pub fn is_busy(&self, session_id: &str) -> bool {
        self.shared
            .states
            .lock()
            .get(session_id)
            .is_some_and(|s| s.busy)
    }

    /// Takes the session's review slot: `None` when an evidence review holds it, or an agentic
    /// review (`/refine`, `nudges`) is running or waiting to.
    pub fn claim(&self, session_id: &str) -> Option<Claim> {
        Shared::claim(&self.shared, session_id, true)
    }

    /// `nudges`: whether the signal score may start an agentic review of the session now. A
    /// review the score started that hasn't covered the signals yet (still running, or it failed:
    /// the session's `reviewed_seq` is where it was) holds the trigger back for
    /// [`BACKOFF_FIRST`], so a failing review isn't started again after every turn.
    pub fn nudge_due(&self, session_id: &str, reviewed_seq: u64) -> bool {
        let mut states = self.shared.states.lock();
        let state = states.entry(session_id.to_string()).or_default();
        if let Some((at, seq)) = state.nudged
            && seq == reviewed_seq
            && at.elapsed() < BACKOFF_FIRST
        {
            return false;
        }
        state.nudged = Some((Instant::now(), reviewed_seq));
        true
    }

    fn arm(&self, session_id: &str, delay: Duration) {
        if self.shared.closed.load(Ordering::SeqCst)
            || self.shared.env.budget.lock().review_mode != ReviewMode::IdleOnce
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let number = self.shared.next_timer.fetch_add(1, Ordering::Relaxed);
        let weak = Arc::downgrade(&self.shared);
        let id = session_id.to_string();
        let mut states = self.shared.states.lock();
        let state = states.entry(id.clone()).or_default();
        if let Some((_, old)) = state.timer.take() {
            old.abort();
        }
        let task = handle.spawn(async move {
            tokio::time::sleep(delay).await;
            Shared::fire(weak, id, number).await;
        });
        state.timer = Some((number, task.abort_handle()));
    }
}

impl Shared {
    /// `create`: a caller asking now (`Runtime::review_once`) may claim a session the scheduler
    /// hasn't seen; a timer may not (a session deleted, or a runtime shut down, after its timer
    /// went off has no state any more, and must not get one back with a review running).
    fn claim(this: &Arc<Self>, session_id: &str, create: bool) -> Option<Claim> {
        {
            let mut states = this.states.lock();
            if this.closed.load(Ordering::SeqCst) {
                return None;
            }
            let state = if create {
                states.entry(session_id.to_string()).or_default()
            } else {
                states.get_mut(session_id)?
            };
            if state.busy {
                return None;
            }
            state.busy = true;
        }
        let claim = Claim {
            shared: this.clone(),
            session_id: session_id.to_string(),
        };
        // An agentic review takes its slot (`OpenSession::review`) after checking ours, under
        // its own lock, so one of the two always sees the other.
        // (The slot's lock is released before `claim` drops, which takes ours: no lock is ever
        // held while waiting for the other in the opposite order.)
        let agentic = this
            .env
            .open_session(session_id)
            .is_some_and(|open| open.review.lock().is_some());
        (!agentic).then_some(claim)
    }

    /// The route for the session's review: the resolver's, else its last turn's.
    async fn route(this: &Arc<Self>, session_id: &str) -> Option<(ProviderConfig, TurnOptions)> {
        let resolver = this.route.lock().clone();
        if let Some(resolver) = resolver {
            let id = session_id.to_string();
            if let Ok(Some(route)) = tokio::task::spawn_blocking(move || resolver(&id)).await {
                return Some(route);
            }
        }
        this.states
            .lock()
            .get(session_id)
            .and_then(|s| s.route.clone())
    }

    /// Reserves a start in the hourly window, or says how long until one frees.
    fn reserve_hourly(&self, per_hour: u32) -> Result<Instant, Duration> {
        let now = Instant::now();
        let mut started = self.started.lock();
        while started
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            started.pop_front();
        }
        if started.len() >= per_hour.max(1) as usize {
            let oldest = *started.front().expect("non-empty");
            return Err(HOUR.saturating_sub(now.duration_since(oldest)) + Duration::from_secs(1));
        }
        started.push_back(now);
        Ok(now)
    }

    /// Gives a reserved start back (the review sent nothing).
    fn release_hourly(&self, at: Instant) {
        let mut started = self.started.lock();
        if let Some(i) = started.iter().position(|t| *t == at) {
            started.remove(i);
        }
    }

    /// A timer went off: start the session's review if it is still due.
    async fn fire(weak: Weak<Self>, session_id: String, number: u64) {
        let Some(this) = weak.upgrade() else {
            return;
        };
        if this.closed.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut states = this.states.lock();
            match states.get_mut(&session_id) {
                Some(state) if state.timer.as_ref().is_some_and(|(n, _)| *n == number) => {
                    state.timer = None;
                }
                // Replaced or forgotten meanwhile.
                _ => return,
            }
        }
        let env = &this.env;
        let budget = *env.budget.lock();
        if budget.review_mode != ReviewMode::IdleOnce || !env.enabled.load(Ordering::Relaxed) {
            return;
        }
        // A turn is running: its end arms the timer again.
        if env.turn_running(&session_id) {
            return;
        }
        let backed_off = this
            .states
            .lock()
            .get(&session_id)
            .and_then(|s| s.retry_after)
            .filter(|t| Instant::now() < *t);
        if let Some(until) = backed_off {
            tracing::info!(
                "review of {session_id} not tried: backing off for {:?} after a failure",
                until.saturating_duration_since(Instant::now())
            );
            return;
        }
        let signals = env.signals.snapshot(&session_id).await;
        if !signals.triggered_at(budget.review_threshold) {
            return;
        }
        // Earlier tries that never covered the signals, in this run or an earlier one (a crash
        // loop mid-review must not send a review per start).
        if let Some(wait) = signals.retry_wait(chrono::Utc::now().timestamp_millis()) {
            tracing::info!(
                "review of {session_id} not tried: {} earlier tries didn't finish; waiting {wait:?}",
                signals.review_tries
            );
            return;
        }
        let reserved = match this.reserve_hourly(budget.review_per_hour) {
            Ok(at) => at,
            Err(wait) => {
                tracing::info!(
                    "review of {session_id} deferred: {} reviews already started this hour",
                    budget.review_per_hour
                );
                Scheduler {
                    shared: this.clone(),
                }
                .arm(&session_id, wait);
                return;
            }
        };
        let Some(claim) = Shared::claim(&this, &session_id, false) else {
            tracing::info!("review of {session_id} dropped: a review of it is running");
            this.release_hourly(reserved);
            return;
        };
        let Some((provider, options)) = Shared::route(&this, &session_id).await else {
            tracing::info!("review of {session_id} not run: no provider known for it");
            this.release_hourly(reserved);
            return;
        };
        let shared = this.clone();
        let id = session_id.clone();
        let mut states = this.states.lock();
        let Some(state) = states.get_mut(&session_id) else {
            // Deleted meanwhile.
            drop(states);
            this.release_hourly(reserved);
            return;
        };
        state.attempts += 1;
        let task = tokio::spawn(async move {
            let _claim = claim;
            let outcome = shared
                .env
                .review_once(&id, &provider, &options, ReviewOnceSettings::default())
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!("review of {id} could not run: {error:#}");
                    ReviewOutcome::failed(format!("{error:#}"))
                });
            Shared::finished(&shared, &id, outcome, reserved);
        });
        state.task = Some(task.abort_handle());
    }

    /// Books the outcome: back-off on a failure, the report on a change.
    fn finished(this: &Arc<Self>, session_id: &str, outcome: ReviewOutcome, reserved: Instant) {
        tracing::info!(
            "review of {session_id}: {:?}, {} request(s), {} op(s)",
            outcome.status,
            outcome.requests,
            outcome.ops.len()
        );
        if outcome.requests == 0 {
            this.release_hourly(reserved);
        }
        {
            let mut states = this.states.lock();
            if let Some(state) = states.get_mut(session_id) {
                match &outcome.status {
                    ReviewStatus::Done => {
                        state.failures = 0;
                        state.retry_after = None;
                    }
                    ReviewStatus::Failed(_) => {
                        state.failures += 1;
                        let wait = if outcome.requests == 0 {
                            BACKOFF_NO_REQUEST
                        } else {
                            BACKOFF_FIRST
                                .saturating_mul(1 << (state.failures - 1).min(10))
                                .min(BACKOFF_MAX)
                        };
                        state.retry_after = Some(Instant::now() + wait);
                    }
                    ReviewStatus::NothingToReview | ReviewStatus::Skipped(_) => {}
                }
                state.last = Some(outcome.clone());
            }
        }
        let actions = outcome.actions();
        if !actions.is_empty() {
            this.env.bridge.review_finished(session_id, actions, false);
        }
    }
}
