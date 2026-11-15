//! The store's writer (Phase 13 CP13b): one thread owns the write connection of `sessions.db`
//! and commits what the app writes in batches, one transaction per batch, instead of one
//! transaction per streamed chunk on the caller's thread.
//!
//! How it works, in one place (the rules the rest of the code relies on):
//!
//! - **One queue, one order.** Every write goes through one channel to the writer thread, the
//!   queued ones ([`crate::Store::queue`]: a turn's messages and token usage) and the waiting ones
//!   (every other `Store` write method, which returns once its write is committed). The thread
//!   runs them in the order they were sent, so a waiting write never overtakes a queued one.
//! - **Batches.** A batch starts with the first write that arrives and closes after
//!   [`BATCH_WINDOW`] (5 ms), after [`MAX_BATCH`] writes, at a waiting write, or when someone
//!   flushes. It is one `BEGIN IMMEDIATE` ... `COMMIT`. Each write runs in its own savepoint
//!   inside it: a write that fails is rolled back alone, its error recorded, and the others in
//!   the batch are committed. If the `COMMIT` itself fails, the batch is rolled back and each
//!   write is retried in a transaction of its own, so one bad write never takes the others down;
//!   the same when a write's error ends the whole transaction (SQLite does on a full disk or an
//!   I/O error), so nothing runs outside the transaction and nothing runs twice.
//!   A queued upsert of a message that a later write in the same batch upserts again (a streamed
//!   reply growing chunk by chunk) is skipped: only the last version is written.
//! - **Reads see every write sent before them.** The read connection is separate (WAL lets it
//!   read while the writer writes), so every read method first waits until the writer has
//!   committed everything queued before it ([`Writer::settle`]): it asks the writer to close its
//!   batch now rather than wait out the window. Read-after-write therefore holds for every read,
//!   from any thread, without the caller remembering to flush.
//! - **Errors are reported, never swallowed.** A queued write that fails is logged at `error`
//!   level and kept, per session, until someone asks: the next [`crate::Store::queue`] for that
//!   session returns it (so a turn stops at its next write, as it did when writes were
//!   synchronous), and so does [`crate::Store::flush_session`] / [`crate::Store::flush`], which
//!   the runtime calls at every turn boundary before the page hears the turn ended.
//! - **Nothing lost on exit.** [`Writer::shutdown`] (also run by `Drop`) closes the channel and
//!   waits for the thread to commit everything queued. A process that is killed loses only the
//!   batch that was not committed yet (at most ~5 ms of writes); a turn boundary is never lost,
//!   because the runtime flushes it (commit, then the turn's end is reported). After shutdown,
//!   writes run on the caller's thread as before CP13b.
//!
//! Not from Hermes (its `hermes_state.py` writes synchronously, one connection per process).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use hermes_llm::conversation::message::Message;
use hermes_llm::conversation::token_usage::ProviderUsage;
use parking_lot::{Condvar, Mutex};
use rusqlite::Connection;

/// How long a batch stays open for more writes after its first one (the plan's "~5 ms"): long
/// enough to take a burst of streamed chunks in one commit, short enough that a crash loses
/// almost nothing.
pub const BATCH_WINDOW: Duration = Duration::from_millis(5);

/// The most writes in one transaction, so one batch never holds the database for long (another
/// process, the GPUI app, may be waiting to write).
pub const MAX_BATCH: usize = 256;

/// The most failures kept per session for its callers to collect; later ones are counted.
const MAX_FAILURES: usize = 64;

/// A write the runtime queues without waiting for it ([`crate::Store::queue`]).
pub enum StoreOp {
    /// Inserts or replaces the message at position `seq` (the turn path's per-chunk write).
    UpsertMessage {
        session_id: String,
        seq: usize,
        message: Arc<Message>,
    },
    /// Replaces the whole stored conversation (compaction, a repaired history, retry).
    ReplaceMessages {
        session_id: String,
        messages: Vec<Arc<Message>>,
    },
    /// One model call's token usage.
    RecordUsage {
        session_id: String,
        usage: ProviderUsage,
    },
    /// The frozen prompt, rewritten by compaction only.
    SetSystemPrompt {
        session_id: String,
        system_prompt: String,
    },
}

impl StoreOp {
    /// The session the write belongs to (its errors are reported to that session's callers).
    pub fn session_id(&self) -> &str {
        match self {
            StoreOp::UpsertMessage { session_id, .. }
            | StoreOp::ReplaceMessages { session_id, .. }
            | StoreOp::RecordUsage { session_id, .. }
            | StoreOp::SetSystemPrompt { session_id, .. } => session_id,
        }
    }

    /// What can be refused before the write is queued (a message without an id is a bug in the
    /// caller, refused at once as the synchronous write did).
    pub(crate) fn check(&self) -> Result<()> {
        let id = |message: &Message| {
            message
                .id
                .as_ref()
                .map(|_| ())
                .ok_or_else(|| anyhow!("messages must have an id before they are stored"))
        };
        match self {
            StoreOp::UpsertMessage { message, .. } => id(message),
            StoreOp::ReplaceMessages { messages, .. } => messages.iter().try_for_each(|m| id(m)),
            StoreOp::RecordUsage { .. } | StoreOp::SetSystemPrompt { .. } => Ok(()),
        }
    }

    fn apply(&self, conn: &Connection) -> Result<()> {
        match self {
            StoreOp::UpsertMessage {
                session_id,
                seq,
                message,
            } => crate::upsert_message_in(conn, session_id, *seq, message),
            StoreOp::ReplaceMessages {
                session_id,
                messages,
            } => crate::replace_messages_in(conn, session_id, messages),
            StoreOp::RecordUsage { session_id, usage } => {
                crate::record_usage_in(conn, session_id, usage)
            }
            StoreOp::SetSystemPrompt {
                session_id,
                system_prompt,
            } => crate::set_system_prompt_in(conn, session_id, system_prompt),
        }
    }
}

/// Any other write, as a closure over the write connection. `Fn`, not `FnOnce`: a batch whose
/// commit fails runs its writes again one by one.
pub(crate) type Call = Box<dyn Fn(&Connection) -> Result<()> + Send>;

pub(crate) enum Work {
    Op(StoreOp),
    Call(Call),
}

impl Work {
    fn run(&self, conn: &Connection) -> Result<()> {
        match self {
            Work::Op(op) => op.apply(conn),
            Work::Call(call) => call(conn),
        }
    }

    /// `(session, seq)` of a message upsert (what a later upsert can supersede).
    fn upsert_key(&self) -> Option<(&str, usize)> {
        match self {
            Work::Op(StoreOp::UpsertMessage {
                session_id, seq, ..
            }) => Some((session_id, *seq)),
            _ => None,
        }
    }
}

struct Job {
    /// Position in the queue (1, 2, ...): `done` counts up to it.
    seq: u64,
    /// The session the write touches; `None` for writes that touch no session (projects).
    session: Option<String>,
    work: Work,
    /// The caller waiting for this write (`None`: queued, its error is kept for later).
    reply: Option<mpsc::Sender<Result<()>>>,
}

enum Msg {
    Job(Job),
    /// Close the open batch now (a read or a flush is waiting for it).
    Flush,
}

struct Queue {
    sender: Option<mpsc::Sender<Msg>>,
    /// The last `seq` sent.
    enqueued: u64,
}

/// A queued write that failed, kept until a caller takes it. `session: None` is a failure of the
/// writer itself, reported to whoever asks first.
struct Failure {
    session: Option<String>,
    message: String,
    /// Later failures of the same session past [`MAX_FAILURES`], counted on its last entry.
    more: usize,
}

impl State {
    /// Keeps `failure`, at most [`MAX_FAILURES`] entries per session (the rest are counted), so
    /// one session's failures can never push another's out.
    fn record(&mut self, failure: Failure) {
        let same = |f: &&mut Failure| f.session == failure.session;
        if self.failures.iter_mut().filter(same).count() >= MAX_FAILURES {
            if let Some(last) = self.failures.iter_mut().rfind(same) {
                last.more += 1;
            }
            return;
        }
        self.failures.push(failure);
    }
}

#[derive(Default)]
struct State {
    /// Every job up to this `seq` is committed (or failed and recorded).
    done: u64,
    /// The thread has ended: nobody waits for it any more.
    stopped: bool,
    failures: Vec<Failure>,
    /// Batches committed and queued upserts skipped as superseded (for the tests).
    batches: u64,
    skipped: u64,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// The writer thread and its queue. See the module doc.
pub(crate) struct Writer {
    /// The write connection: the thread's, and the caller's after shutdown.
    write: Arc<Mutex<Connection>>,
    shared: Arc<Shared>,
    queue: Mutex<Queue>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Writer {
    pub(crate) fn start(write: Arc<Mutex<Connection>>) -> Self {
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::channel();
        let thread = {
            let (write, shared) = (write.clone(), shared.clone());
            std::thread::Builder::new()
                .name("hermes-store-writer".into())
                .spawn(move || run(receiver, &write, &shared))
        };
        let (sender, thread) = match thread {
            Ok(thread) => (Some(sender), Some(thread)),
            Err(error) => {
                // Writes then run on the caller's thread, as before the writer existed.
                tracing::error!("session database: no writer thread ({error}); writing inline");
                shared.state.lock().stopped = true;
                (None, None)
            }
        };
        Self {
            write,
            shared,
            queue: Mutex::new(Queue {
                sender,
                enqueued: 0,
            }),
            thread: Mutex::new(thread),
        }
    }

    /// The write connection (tests check its pragmas).
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.write
    }

    /// `(batches committed, queued upserts skipped as superseded)` so far.
    #[cfg(test)]
    pub(crate) fn stats(&self) -> (u64, u64) {
        let state = self.shared.state.lock();
        (state.batches, state.skipped)
    }

    /// Sends a write to the thread; with `wait`, returns once it is committed, with its own
    /// result. Without the thread (after shutdown), runs it here in a transaction of its own.
    pub(crate) fn submit(&self, session: Option<String>, work: Work, wait: bool) -> Result<()> {
        let (reply, answer) = if wait {
            let (reply, answer) = mpsc::channel();
            (Some(reply), Some(answer))
        } else {
            (None, None)
        };
        // The write the thread can't take: after shutdown, or (defensive: it never stops on its
        // own) when it is gone.
        let refused = {
            let mut queue = self.queue.lock();
            let seq = queue.enqueued + 1;
            let refused = match queue.sender.as_ref() {
                None => Some(work),
                Some(sender) => match sender.send(Msg::Job(Job {
                    seq,
                    session,
                    work,
                    reply,
                })) {
                    Ok(()) => None,
                    Err(mpsc::SendError(Msg::Job(job))) => Some(job.work),
                    Err(mpsc::SendError(Msg::Flush)) => None,
                },
            };
            match refused {
                None => queue.enqueued = seq,
                Some(_) => queue.sender = None,
            }
            refused
        };
        if let Some(work) = refused {
            // Not before the thread has written what was sent to it: a shutdown on another
            // thread may still be draining the queue, and this write comes after all of it.
            self.wait_stopped();
            return run_alone(&self.write.lock(), &work);
        }
        match answer {
            Some(answer) => answer
                .recv()
                .unwrap_or_else(|_| Err(anyhow!("the session database writer stopped"))),
            None => Ok(()),
        }
    }

    /// Waits until every write sent before this call is committed (or failed and recorded).
    /// Errors stay where they are, for [`Writer::take_failures`].
    pub(crate) fn settle(&self) {
        let target = {
            let queue = self.queue.lock();
            if self.shared.state.lock().done >= queue.enqueued {
                return;
            }
            if let Some(sender) = &queue.sender {
                let _ = sender.send(Msg::Flush);
            }
            queue.enqueued
        };
        let mut state = self.shared.state.lock();
        while state.done < target && !state.stopped {
            self.shared.changed.wait(&mut state);
        }
    }

    /// The failures recorded for `session` (every failure with `None`), taken: each is reported
    /// to one caller.
    pub(crate) fn take_failures(&self, session: Option<&str>) -> Result<()> {
        let failures = {
            let mut state = self.shared.state.lock();
            let all = std::mem::take(&mut state.failures);
            let (taken, kept): (Vec<_>, Vec<_>) = all.into_iter().partition(|failure| {
                failure.session.is_none()
                    || session.is_none_or(|session| failure.session.as_deref() == Some(session))
            });
            state.failures = kept;
            taken
        };
        if failures.is_empty() {
            return Ok(());
        }
        let count: usize = failures.iter().map(|failure| 1 + failure.more).sum();
        let first = failures
            .first()
            .map_or("see the log", |failure| failure.message.as_str());
        Err(anyhow!(
            "{count} write{} to the session database failed (the first: {first})",
            if count == 1 { "" } else { "s" }
        ))
    }

    /// Sends nothing more to the thread, lets it commit what is queued, and waits for it to end.
    /// Writes after this run on the caller's thread. Returns the failures nobody collected.
    pub(crate) fn shutdown(&self) -> Result<()> {
        let sender = self.queue.lock().sender.take();
        drop(sender);
        let thread = self.thread.lock().take();
        if let Some(thread) = thread
            && thread.join().is_err()
        {
            tracing::error!("session database: the writer thread panicked");
        }
        // A second caller (the exit hook and `Drop`, two threads) finds the handle taken: it
        // still returns only once the queue is drained.
        self.wait_stopped();
        self.take_failures(None)
    }

    /// Blocks until the writer thread has ended (at once if it never started). Only called once
    /// the channel is closed or broken, so the thread is on its way out.
    fn wait_stopped(&self) {
        let mut state = self.shared.state.lock();
        while !state.stopped {
            self.shared.changed.wait(&mut state);
        }
    }

    /// Forgets `session`'s recorded failures (it was deleted: they are moot). Unlike
    /// [`Writer::take_failures`], a failure of the writer itself (`session: None`) stays to be
    /// reported.
    pub(crate) fn discard_failures(&self, session: &str) {
        self.shared
            .state
            .lock()
            .failures
            .retain(|failure| failure.session.as_deref() != Some(session));
    }
}

/// The thread: collects a batch, commits it, reports, until the channel closes.
fn run(receiver: mpsc::Receiver<Msg>, write: &Mutex<Connection>, shared: &Shared) {
    /// Releases every waiter when the thread ends, normally or (a bug outside the per-batch
    /// `catch_unwind`) by a panic: then the jobs still in the channel are lost, which is
    /// recorded as a failure for the next flush, and later writes run on the caller's thread.
    struct Stopped<'a>(&'a Shared);
    impl Drop for Stopped<'_> {
        fn drop(&mut self) {
            let mut state = self.0.state.lock();
            state.stopped = true;
            if std::thread::panicking() {
                tracing::error!("session database: the writer thread stopped on a panic");
                state.record(Failure {
                    session: None,
                    message: "the session database writer stopped; queued writes were lost".into(),
                    more: 0,
                });
            }
            self.0.changed.notify_all();
        }
    }
    let _stopped = Stopped(shared);
    loop {
        let first = match receiver.recv() {
            Ok(Msg::Job(job)) => job,
            Ok(Msg::Flush) => continue,
            Err(mpsc::RecvError) => break,
        };
        let (batch, closed) = collect(&receiver, first);
        commit(write, shared, batch);
        if closed {
            break;
        }
    }
}

/// The batch that starts with `first`; `true` when the channel closed meanwhile.
fn collect(receiver: &mpsc::Receiver<Msg>, first: Job) -> (Vec<Job>, bool) {
    let waiting = first.reply.is_some();
    let mut batch = vec![first];
    if waiting {
        return (batch, false);
    }
    let deadline = Instant::now() + BATCH_WINDOW;
    while batch.len() < MAX_BATCH {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match receiver.recv_timeout(left) {
            Ok(Msg::Job(job)) => {
                let waiting = job.reply.is_some();
                batch.push(job);
                if waiting {
                    break;
                }
            }
            Ok(Msg::Flush) | Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => return (batch, true),
        }
    }
    (batch, false)
}

/// Writes one batch and reports each write's result: to its waiting caller, or (queued) to the
/// log and the failure list.
fn commit(write: &Mutex<Connection>, shared: &Shared, batch: Vec<Job>) {
    let last = batch.last().map_or(0, |job| job.seq);
    let skip = superseded(&batch);
    let results = {
        let conn = write.lock();
        match std::panic::catch_unwind(AssertUnwindSafe(|| apply(&conn, &batch, &skip))) {
            Ok(results) => results,
            Err(_) => {
                if !conn.is_autocommit() {
                    let _ = conn.execute_batch("ROLLBACK");
                }
                batch
                    .iter()
                    .map(|_| {
                        Some(Err(anyhow!(
                            "the session database writer hit an internal error; not saved"
                        )))
                    })
                    .collect()
            }
        }
    };
    let mut failures = Vec::new();
    let mut replies = Vec::new();
    for (job, result) in batch.into_iter().zip(results) {
        // `None`: superseded by a later upsert of the same message, which carries its result.
        let result = result.unwrap_or(Ok(()));
        match job.reply {
            Some(reply) => replies.push((reply, result)),
            None => {
                if let Err(error) = result {
                    tracing::error!(
                        "session database: a queued write for {} failed: {error:#}",
                        job.session.as_deref().unwrap_or("no session")
                    );
                    failures.push(Failure {
                        session: job.session,
                        message: format!("{error:#}"),
                        more: 0,
                    });
                }
            }
        }
    }
    {
        let mut state = shared.state.lock();
        state.done = last;
        state.batches += 1;
        state.skipped += skip.iter().filter(|skipped| **skipped).count() as u64;
        for failure in failures {
            state.record(failure);
        }
        shared.changed.notify_all();
    }
    // After `done` moved: a caller that reads right after its write returns sees no wait.
    for (reply, result) in replies {
        let _ = reply.send(result);
    }
}

/// Which queued upserts a later write in the batch makes pointless: a later upsert of the same
/// `(session, seq)`, with nothing in between that touches that session other than upserts of
/// other messages (a replace, a delete or a write of unknown scope keeps the earlier one).
fn superseded(batch: &[Job]) -> Vec<bool> {
    let mut skip = vec![false; batch.len()];
    for (i, job) in batch.iter().enumerate() {
        let Some((session, seq)) = job.work.upsert_key() else {
            continue;
        };
        if job.reply.is_some() {
            continue;
        }
        for later in &batch[i + 1..] {
            match (&later.session, later.work.upsert_key()) {
                (Some(_), Some((other, other_seq))) if other == session => {
                    if other_seq == seq {
                        skip[i] = true;
                        break;
                    }
                }
                (Some(other), _) if other != session => {}
                _ => break,
            }
        }
    }
    skip
}

/// Batches whose COMMIT failed and were retried write by write (for the tests; no test but
/// that one makes a commit fail).
#[cfg(test)]
static RETRIED_BATCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One transaction over the batch, a savepoint per write. `None` for a skipped write.
fn apply(conn: &Connection, batch: &[Job], skip: &[bool]) -> Vec<Option<Result<()>>> {
    if !conn.is_autocommit() {
        // Left open by an earlier panic: nothing in it was reported as saved.
        let _ = conn.execute_batch("ROLLBACK");
    }
    if let Err(error) = conn.execute_batch("BEGIN IMMEDIATE") {
        let message = format!("{error:#}");
        return batch
            .iter()
            .zip(skip)
            .map(|(_, skipped)| (!skipped).then(|| Err(anyhow!("{message}"))))
            .collect();
    }
    let mut results: Vec<Option<Result<()>>> = Vec::with_capacity(batch.len());
    for (job, skipped) in batch.iter().zip(skip) {
        if *skipped {
            results.push(None);
            continue;
        }
        let result = in_savepoint(conn, &job.work);
        if conn.is_autocommit() {
            // The write's error ended the whole transaction (SQLite rolls a transaction back
            // itself on some errors: a full disk, an I/O error, an interrupt), so the writes
            // before it are undone too and `ROLLBACK TO` had nothing to go back to. Going on
            // would run the rest outside any transaction, each `SAVEPOINT` committing on its
            // own, and the retry below would then run them a second time (a usage row counted
            // twice, a session insert answering "already exists"). Nothing of the batch is
            // saved: run every write again in a transaction of its own.
            let error = result.err().map_or_else(
                || "a write ended the batch's transaction".to_string(),
                |error| format!("{error:#}"),
            );
            return retry_each(conn, batch, skip, &error);
        }
        results.push(Some(result));
    }
    match conn.execute_batch("COMMIT") {
        Ok(()) => results,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            retry_each(conn, batch, skip, &error.to_string())
        }
    }
}

/// The batch's transaction is gone, nothing of it saved: each write again, alone.
fn retry_each(
    conn: &Connection,
    batch: &[Job],
    skip: &[bool],
    why: &str,
) -> Vec<Option<Result<()>>> {
    tracing::warn!("session database: a batch did not commit ({why}); retrying each write");
    #[cfg(test)]
    RETRIED_BATCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    batch
        .iter()
        .zip(skip)
        .map(|(job, skipped)| (!skipped).then(|| run_alone(conn, &job.work)))
        .collect()
}

fn in_savepoint(conn: &Connection, work: &Work) -> Result<()> {
    conn.execute_batch("SAVEPOINT store_op")?;
    match work.run(conn) {
        Ok(()) => Ok(conn.execute_batch("RELEASE store_op")?),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK TO store_op; RELEASE store_op");
            Err(error)
        }
    }
}

/// One write in a transaction of its own.
fn run_alone(conn: &Connection, work: &Work) -> Result<()> {
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match work.run(conn) {
        Ok(()) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error.into())
            }
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
