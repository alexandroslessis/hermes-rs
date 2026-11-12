//! The request ledger: how many requests went to each provider today, and why.
//!
//! Not from Hermes (backend behaviour is ours, AGENTS.md rule 1; `docs/backend/PHASES.md`
//! Phase 2): the user's provider allows a fixed number of requests per day, so every request is
//! counted per provider, per day and per [`Purpose`]. `PORTING.md` marks it `diverged`.
//!
//! ## How it works
//!
//! - [`hermes_llm::observer`] reports each HTTP attempt to [`LedgerObserver`], which only *sends a
//!   message* to the ledger thread (a channel send: no lock, no file I/O on the request's task).
//! - One plain OS thread owns the state and the file `data_dir/requests.json` (a side file: the
//!   store schema is frozen, ground rule 6). It applies messages, and writes the file at most
//!   every [`FLUSH_EVERY`] when something changed, and once more when the ledger is dropped. This
//!   is the "actor" of the plan, in Python terms a worker thread draining a `queue.Queue`.
//! - Reads ([`Ledger::snapshot`]) ask the thread over a oneshot channel and await the answer.
//!
//! ## The file
//!
//! ```json
//! { "version": 1,
//!   "providers": { "<provider id>": {
//!       "reset_offset_minutes": 0,
//!       "days": { "2026-11-12": { "by_purpose": { "main": 41, "review": 2 }, "rate_limited": 1 } } } } }
//! ```
//!
//! A day is a calendar date in the provider's reset time zone, given as a fixed UTC offset in
//! minutes (default 0 = UTC; named zones with daylight saving come with the provider settings in
//! CP2.2). Days older than [`KEEP_DAYS`] are dropped. A file that can't be read is set aside as
//! `requests.json.bad` and counting starts afresh.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hermes_llm::observer::{Outcome, Purpose, RequestContext, RequestMeta, RequestObserver};
use serde::{Deserialize, Serialize};

/// How long a change waits before the file is written.
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// Days of history kept per provider.
pub const KEEP_DAYS: i64 = 31;
const FILE_VERSION: u32 = 1;

/// Counts for one provider day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayCounts {
    /// Requests by purpose name (`main`, `review`, ...).
    pub by_purpose: BTreeMap<String, u64>,
    /// Requests the provider answered with HTTP 429 (they are also counted in `by_purpose`).
    pub rate_limited: u64,
}

impl DayCounts {
    pub fn total(&self) -> u64 {
        self.by_purpose.values().sum()
    }
}

/// One provider's history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDays {
    /// Where the provider's day starts, as minutes east of UTC.
    pub reset_offset_minutes: i32,
    /// The daily request limit the user set for this provider (Settings); `None` = not set.
    /// Settings are its only source (applied at start and on every change), so it is not written
    /// to the file: a limit removed from Settings must not live on in `requests.json`.
    #[serde(skip)]
    pub daily_limit: Option<u32>,
    /// The limit the provider's own 429 told us most recently ("800 of 800"); used when none is
    /// set.
    pub learned_limit: Option<u32>,
    /// `YYYY-MM-DD` -> counts.
    pub days: BTreeMap<String, DayCounts>,
}

/// Everything in `requests.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub version: u32,
    pub providers: BTreeMap<String, ProviderDays>,
}

impl Snapshot {
    /// Today's counts for `provider` at unix time `now` (zeros when nothing was counted yet).
    pub fn today(&self, provider: &str, now: i64) -> DayCounts {
        self.providers
            .get(provider)
            .and_then(|p| p.days.get(&day_key(now, p.reset_offset_minutes)).cloned())
            .unwrap_or_default()
    }
}

/// One provider's entry in Settings' `requests.limits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderLimit {
    pub provider: String,
    /// `None` = no limit set.
    pub daily_limit: Option<u32>,
    /// Where the provider's day starts, as minutes east of UTC.
    pub reset_offset_minutes: i32,
}

/// What a provider has used today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Requests made today (all purposes).
    pub used: u64,
    /// The daily limit, from Settings or learned from a 429; `None` when unknown.
    pub limit: Option<u32>,
    /// Unix time when today's count starts over (the provider's next midnight).
    pub resets_at: i64,
    pub by_purpose: BTreeMap<String, u64>,
    pub rate_limited: u64,
}

/// The next midnight, as unix time, in a zone `offset_minutes` east of UTC.
fn next_reset(now: i64, offset_minutes: i32) -> i64 {
    let local = now + i64::from(offset_minutes) * 60;
    (local.div_euclid(86_400) + 1) * 86_400 - i64::from(offset_minutes) * 60
}

/// The calendar date (`YYYY-MM-DD`) of unix time `secs` in a zone `offset_minutes` east of UTC.
pub fn day_key(secs: i64, offset_minutes: i32) -> String {
    chrono::DateTime::from_timestamp(secs + i64::from(offset_minutes) * 60, 0)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".into())
}

impl Snapshot {
    fn record(&mut self, provider: &str, purpose: Purpose, now: i64) {
        let entry = self.providers.entry(provider.to_string()).or_default();
        let key = day_key(now, entry.reset_offset_minutes);
        *entry
            .days
            .entry(key)
            .or_default()
            .by_purpose
            .entry(purpose.as_str().to_string())
            .or_insert(0) += 1;
        prune(entry, now);
    }

    fn record_rate_limited(&mut self, provider: &str, now: i64) {
        let entry = self.providers.entry(provider.to_string()).or_default();
        let key = day_key(now, entry.reset_offset_minutes);
        entry.days.entry(key).or_default().rate_limited += 1;
    }

    fn set_limit(&mut self, provider: &str, limit: Option<u32>) {
        self.providers.entry(provider.to_string()).or_default().daily_limit = limit;
    }

    /// Remembers the limit the provider reported last (a provider may raise or lower it). A limit
    /// set in Settings still wins while it is set.
    fn learn_limit(&mut self, provider: &str, limit: u32) {
        if limit > 0 {
            self.providers
                .entry(provider.to_string())
                .or_default()
                .learned_limit = Some(limit);
        }
    }

    /// Settings' whole `requests.limits`: every listed provider gets its limit and day start, and
    /// every other provider goes back to no configured limit and a UTC day.
    fn configure(&mut self, limits: &[ProviderLimit]) {
        for (id, entry) in self.providers.iter_mut() {
            if !limits.iter().any(|limit| &limit.provider == id) {
                entry.daily_limit = None;
                entry.reset_offset_minutes = 0;
            }
        }
        for limit in limits {
            let entry = self.providers.entry(limit.provider.clone()).or_default();
            entry.daily_limit = limit.daily_limit;
            entry.reset_offset_minutes = limit.reset_offset_minutes;
        }
    }

    fn set_reset_offset(&mut self, provider: &str, minutes: i32) {
        self.providers
            .entry(provider.to_string())
            .or_default()
            .reset_offset_minutes = minutes;
    }
}

/// Drops days older than [`KEEP_DAYS`] (relative to the provider's own today).
fn prune(entry: &mut ProviderDays, now: i64) {
    let cutoff = day_key(now - (KEEP_DAYS - 1) * 86_400, entry.reset_offset_minutes);
    entry.days.retain(|day, _| day.as_str() >= cutoff.as_str());
}

/// The daily limit a provider's 429 message states as "N of N" ("...daily request limit reached
/// (800 of 800)"): `Some(N)` when both numbers are the same, `None` otherwise.
pub fn daily_limit_in(message: &str) -> Option<u32> {
    // Thousands separators ("1,000 of 1,000") are dropped first, so the numbers stay whole.
    let chars: Vec<char> = message.chars().collect();
    let message: String = chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            !(c == ','
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        .map(|(_, &c)| c)
        .collect();
    let words: Vec<&str> = message
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '.' | ';' | ':'))
        .filter(|word| !word.is_empty())
        .collect();
    words.windows(3).find_map(|w| {
        let (used, limit) = (w[0].parse::<u32>().ok()?, w[2].parse::<u32>().ok()?);
        (w[1].eq_ignore_ascii_case("of") && used == limit && limit > 0).then_some(limit)
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

enum Message {
    Record { provider: String, purpose: Purpose, at: i64 },
    RateLimited { provider: String, at: i64 },
    ResetOffset { provider: String, minutes: i32 },
    SetLimit { provider: String, limit: Option<u32> },
    LearnLimit { provider: String, limit: u32 },
    Configure(Vec<ProviderLimit>),
    Snapshot(tokio::sync::oneshot::Sender<Snapshot>),
    /// Write now, then answer.
    Flush(mpsc::Sender<()>),
    Shutdown,
}

struct Inner {
    tx: mpsc::Sender<Message>,
    /// The counts, written only by the ledger thread; readers (the governor, the status method)
    /// take the read lock for a moment. The request path never touches it.
    shared: Arc<parking_lot::RwLock<Snapshot>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The last handle is gone (the runtime is closing): write what is pending and stop. The
        // join blocks the dropping thread for at most one file write; that happens once, at exit.
        // (The ledger thread holds no `Ledger`, so this never runs on it; the id check keeps a
        // future change from turning that into a self-join deadlock.) A panicked thread's join
        // error is ignored: its counts are lost, the app goes on.
        let _ = self.tx.send(Message::Shutdown);
        let thread = self.thread.lock().ok().and_then(|mut t| t.take());
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// A handle to the ledger thread; clones share it.
#[derive(Clone)]
pub struct Ledger {
    inner: Arc<Inner>,
}

impl Ledger {
    /// Opens (or creates) the ledger stored at `path`.
    pub fn open(path: PathBuf) -> Self {
        Self::open_with(path, FLUSH_EVERY)
    }

    /// [`Self::open`] with another flush interval (tests).
    pub fn open_with(path: PathBuf, flush_every: Duration) -> Self {
        let shared = Arc::new(parking_lot::RwLock::new(load(&path)));
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("hermes-ledger".into())
            .spawn({
                let shared = shared.clone();
                move || run(rx, shared, &path, flush_every)
            })
            .ok();
        Self {
            inner: Arc::new(Inner {
                tx,
                shared,
                thread: Mutex::new(thread),
            }),
        }
    }

    /// Counts one request now. Never blocks.
    pub fn record(&self, provider: &str, purpose: Purpose) {
        self.record_at(provider, purpose, unix_now());
    }

    /// [`Self::record`] at a given unix time (tests).
    pub fn record_at(&self, provider: &str, purpose: Purpose, at: i64) {
        let _ = self.inner.tx.send(Message::Record {
            provider: provider.to_string(),
            purpose,
            at,
        });
    }

    /// Counts a request the provider answered with 429.
    pub fn record_rate_limited(&self, provider: &str) {
        let _ = self.inner.tx.send(Message::RateLimited {
            provider: provider.to_string(),
            at: unix_now(),
        });
    }

    /// Where `provider`'s day starts, minutes east of UTC.
    pub fn set_reset_offset(&self, provider: &str, minutes: i32) {
        let _ = self.inner.tx.send(Message::ResetOffset {
            provider: provider.to_string(),
            minutes,
        });
    }

    /// The daily request limit the user set for `provider` (`None` clears it).
    pub fn set_limit(&self, provider: &str, limit: Option<u32>) {
        let _ = self.inner.tx.send(Message::SetLimit {
            provider: provider.to_string(),
            limit,
        });
    }

    /// Applies Settings' `requests.limits` as a whole (see [`ProviderLimit`]): providers not
    /// listed lose any configured limit and day start.
    pub fn configure(&self, limits: Vec<ProviderLimit>) {
        let _ = self.inner.tx.send(Message::Configure(limits));
    }

    /// A limit the provider itself reported (the latest one is kept).
    pub fn learn_limit(&self, provider: &str, limit: u32) {
        let _ = self.inner.tx.send(Message::LearnLimit {
            provider: provider.to_string(),
            limit,
        });
    }

    /// Today's usage of `provider`, read without waiting for the ledger thread (it may lag the
    /// latest few requests by a moment).
    pub fn usage(&self, provider: &str) -> Usage {
        self.usage_at(provider, unix_now())
    }

    /// [`Self::usage`] at a given unix time (tests).
    pub fn usage_at(&self, provider: &str, now: i64) -> Usage {
        let shared = self.inner.shared.read();
        let entry = shared.providers.get(provider);
        let offset = entry.map_or(0, |p| p.reset_offset_minutes);
        let today = entry
            .and_then(|p| p.days.get(&day_key(now, offset)))
            .cloned()
            .unwrap_or_default();
        Usage {
            used: today.total(),
            limit: entry.and_then(|p| p.daily_limit.or(p.learned_limit)),
            resets_at: next_reset(now, offset),
            by_purpose: today.by_purpose,
            rate_limited: today.rate_limited,
        }
    }

    /// Runs `future` as a request context that counts into this ledger (a task's own purpose,
    /// for work started outside a turn: titles, the curator, /btw...).
    pub async fn scoped<F: std::future::Future>(
        &self,
        purpose: Purpose,
        session_id: Option<&str>,
        future: F,
    ) -> F::Output {
        hermes_llm::observer::scope(self.context(purpose, session_id), future).await
    }

    /// The counts as they are now (including requests still waiting for the next file write).
    pub async fn snapshot(&self) -> Snapshot {
        let (reply, answer) = tokio::sync::oneshot::channel();
        if self.inner.tx.send(Message::Snapshot(reply)).is_err() {
            return Snapshot::default();
        }
        answer.await.unwrap_or_default()
    }

    /// Writes the file now and waits for it. For tests and orderly exits; blocks the caller.
    pub fn flush_blocking(&self) {
        let (reply, answer) = mpsc::channel();
        if self.inner.tx.send(Message::Flush(reply)).is_ok() {
            let _ = answer.recv_timeout(Duration::from_secs(10));
        }
    }

    /// What `hermes_llm` calls for each request: counts it for the context's provider.
    pub fn observer(&self) -> Arc<dyn RequestObserver> {
        Arc::new(LedgerObserver {
            ledger: self.clone(),
        })
    }

    /// A request context that counts into this ledger; wrap a task's future with
    /// [`hermes_llm::observer::scope`].
    pub fn context(&self, purpose: Purpose, session_id: Option<&str>) -> RequestContext {
        RequestContext {
            purpose,
            session_id: session_id.map(str::to_string),
            observer: self.observer(),
        }
    }
}

struct LedgerObserver {
    ledger: Ledger,
}

impl RequestObserver for LedgerObserver {
    fn on_request(&self, meta: &RequestMeta<'_>) {
        self.ledger.record(meta.provider, meta.purpose);
    }

    fn on_response(&self, meta: &RequestMeta<'_>, outcome: Outcome) {
        if outcome == Outcome::Status(429) {
            self.ledger.record_rate_limited(meta.provider);
        }
    }
}

fn load(path: &Path) -> Snapshot {
    let parsed = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Snapshot>(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Snapshot::default(),
        // Exists but can't be read (locked, permissions): set it aside too, so the next write
        // doesn't silently replace a month of counts.
        Err(error) => Err(error.to_string()),
    };
    match parsed {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!("{} is unreadable ({error}); starting a new ledger", path.display());
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Snapshot::default()
        }
    }
}

/// Writes the file; `false` when that failed (the caller keeps the state dirty and tries again).
fn save(path: &Path, snapshot: &Snapshot) -> bool {
    let mut snapshot = snapshot.clone();
    snapshot.version = FILE_VERSION;
    let Ok(bytes) = serde_json::to_vec_pretty(&snapshot) else {
        return false;
    };
    // Write-then-rename: a crash mid-write leaves the old file, not half of a new one. On Windows
    // `std::fs::rename` is `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, so it replaces the old file;
    // it fails only while another process holds it open without delete sharing (a scanner, an
    // editor), and then the next interval tries again.
    let temp = path.with_extension("json.tmp");
    let written = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = &written {
        tracing::warn!("saving {} failed: {error}", path.display());
    }
    written.is_ok()
}

/// The ledger thread: applies messages, writes the file when something changed and the last write
/// is older than `flush_every`.
fn run(
    rx: mpsc::Receiver<Message>,
    shared: Arc<parking_lot::RwLock<Snapshot>>,
    path: &Path,
    flush_every: Duration,
) {
    let mut dirty = false;
    let mut due: Option<std::time::Instant> = None;
    loop {
        let message = match due {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Message::Shutdown),
            },
            None => Some(rx.recv().unwrap_or(Message::Shutdown)),
        };
        match message {
            Some(Message::Record { provider, purpose, at }) => {
                shared.write().record(&provider, purpose, at);
                dirty = true;
            }
            Some(Message::RateLimited { provider, at }) => {
                shared.write().record_rate_limited(&provider, at);
                dirty = true;
            }
            Some(Message::ResetOffset { provider, minutes }) => {
                shared.write().set_reset_offset(&provider, minutes);
                dirty = true;
            }
            Some(Message::SetLimit { provider, limit }) => {
                shared.write().set_limit(&provider, limit);
                dirty = true;
            }
            Some(Message::LearnLimit { provider, limit }) => {
                shared.write().learn_limit(&provider, limit);
                dirty = true;
            }
            Some(Message::Configure(limits)) => {
                shared.write().configure(&limits);
                dirty = true;
            }
            Some(Message::Snapshot(reply)) => {
                let _ = reply.send(shared.read().clone());
            }
            Some(Message::Flush(reply)) => {
                if dirty && save(path, &shared.read()) {
                    dirty = false;
                    due = None;
                }
                let _ = reply.send(());
            }
            Some(Message::Shutdown) => {
                if dirty {
                    save(path, &shared.read());
                }
                return;
            }
            None => {
                // The debounce interval ran out (a failed write stays dirty: another interval).
                if dirty && save(path, &shared.read()) {
                    dirty = false;
                }
                due = None;
            }
        }
        if dirty && due.is_none() {
            due = Some(std::time::Instant::now() + flush_every);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-12 12:00:00 UTC
    const NOON: i64 = 1_794_484_800;

    #[test]
    fn a_day_is_a_calendar_date_in_the_providers_zone() {
        assert_eq!(day_key(NOON, 0), "2026-11-12");
        // 23:30 UTC on the 12th is already the 13th in UTC+2 and still the 12th in UTC-5.
        let late = NOON + 11 * 3600 + 30 * 60;
        assert_eq!(day_key(late, 0), "2026-11-12");
        assert_eq!(day_key(late, 120), "2026-11-13");
        assert_eq!(day_key(late, -300), "2026-11-12");
        // Just after midnight UTC, UTC-5 is still on the previous day.
        let after_midnight = NOON + 12 * 3600 + 60;
        assert_eq!(day_key(after_midnight, 0), "2026-11-13");
        assert_eq!(day_key(after_midnight, -300), "2026-11-12");
    }

    #[test]
    fn counts_roll_over_at_the_providers_midnight_not_utcs() {
        let mut state = Snapshot::default();
        state.set_reset_offset("p", 120); // UTC+2
        let before = NOON + 9 * 3600 + 30 * 60; // 21:30 UTC = 23:30 local, the 12th
        let after = before + 3600; // 22:30 UTC = 00:30 local, the 13th
        state.record("p", Purpose::Main, before);
        state.record("p", Purpose::Main, after);
        state.record("p", Purpose::Review, after);
        assert_eq!(state.today("p", before).total(), 1);
        let today = state.today("p", after);
        assert_eq!(today.by_purpose["main"], 1);
        assert_eq!(today.by_purpose["review"], 1);
        assert_eq!(state.providers["p"].days.len(), 2);
        // Another provider is counted separately.
        assert_eq!(state.today("other", after).total(), 0);
    }

    #[test]
    fn days_older_than_a_month_are_dropped() {
        let mut state = Snapshot::default();
        for day in 0..40 {
            state.record("p", Purpose::Main, NOON + day * 86_400);
        }
        let days = &state.providers["p"].days;
        assert_eq!(days.len() as i64, KEEP_DAYS);
        assert!(days.contains_key(&day_key(NOON + 39 * 86_400, 0)));
        assert!(!days.contains_key(&day_key(NOON, 0)));
        assert!(days.contains_key(&day_key(NOON + 9 * 86_400, 0)));
    }

    #[test]
    fn a_429_is_counted_beside_the_request() {
        let mut state = Snapshot::default();
        state.record("p", Purpose::Main, NOON);
        state.record_rate_limited("p", NOON);
        let today = state.today("p", NOON);
        assert_eq!((today.total(), today.rate_limited), (1, 1));
    }

    #[tokio::test]
    async fn changes_are_written_after_the_debounce_and_on_drop_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.json");
        let ledger = Ledger::open_with(path.clone(), Duration::from_millis(100));
        ledger.record_at("p", Purpose::Main, NOON);
        ledger.record_at("p", Purpose::Main, NOON);
        // Visible at once through the thread, written to the file a moment later.
        assert_eq!(ledger.snapshot().await.today("p", NOON).total(), 2);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let on_disk: Snapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.today("p", NOON).total(), 2);
        assert_eq!(on_disk.version, FILE_VERSION);

        // A change right before the ledger closes is written by the drop.
        ledger.record_at("p", Purpose::Title, NOON);
        drop(ledger);
        let reopened = Ledger::open(path.clone());
        let today = reopened.snapshot().await.today("p", NOON);
        assert_eq!((today.by_purpose["main"], today.by_purpose["title"]), (2, 1));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[tokio::test]
    async fn a_burst_of_requests_from_many_threads_loses_none() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with(dir.path().join("requests.json"), Duration::from_secs(60));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let ledger = ledger.clone();
                std::thread::spawn(move || {
                    for _ in 0..250 {
                        ledger.record_at("p", Purpose::Main, NOON);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(ledger.snapshot().await.today("p", NOON).total(), 2000);
    }

    #[tokio::test]
    async fn an_unreadable_file_is_set_aside_and_counting_starts_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.json");
        std::fs::write(&path, "{ not json").unwrap();
        let ledger = Ledger::open(path.clone());
        assert_eq!(ledger.snapshot().await, Snapshot::default());
        assert!(dir.path().join("requests.json.bad").exists());
        ledger.record_at("p", Purpose::Main, NOON);
        ledger.flush_blocking();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_failed_write_is_tried_again_at_the_next_interval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.json");
        // The temp file's name is taken by a folder: writing fails until it is gone.
        let blocker = path.with_extension("json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        let ledger = Ledger::open_with(path.clone(), Duration::from_millis(50));
        ledger.record_at("p", Purpose::Main, NOON);
        ledger.flush_blocking();
        assert!(!path.exists());
        std::fs::remove_dir(&blocker).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let on_disk: Snapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.today("p", NOON).total(), 1);
    }

    #[tokio::test]
    async fn a_file_that_exists_but_cannot_be_read_is_set_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.json");
        // A folder in the file's place: reading fails with something other than "not found".
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("marker"), "x").unwrap();
        let ledger = Ledger::open(path.clone());
        assert_eq!(ledger.snapshot().await, Snapshot::default());
        assert!(dir.path().join("requests.json.bad").join("marker").exists());
    }

    #[test]
    fn the_daily_limit_is_read_from_an_n_of_n_message() {
        assert_eq!(
            daily_limit_in("Rate limit exceeded: user daily request limit reached (800 of 800). Retry after 19919 seconds."),
            Some(800)
        );
        assert_eq!(daily_limit_in("You have used 50 of 50 requests"), Some(50));
        assert_eq!(daily_limit_in("limit reached (1,000 of 1,000), retry later"), Some(1000));
        // Not "N of N": a partial count, other numbers, no numbers.
        assert_eq!(daily_limit_in("(790 of 800)"), None);
        assert_eq!(daily_limit_in("retry after 30 seconds"), None);
        assert_eq!(daily_limit_in("0 of 0"), None);
    }

    #[test]
    fn a_configured_limit_wins_over_a_learned_one_and_the_latest_learned_one_is_kept() {
        let mut state = Snapshot::default();
        state.learn_limit("p", 800);
        state.learn_limit("p", 900);
        state.learn_limit("p", 0);
        assert_eq!(state.providers["p"].learned_limit, Some(900));
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path().join("requests.json"));
        ledger.learn_limit("p", 800);
        ledger.set_limit("p", Some(500));
        ledger.flush_blocking();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ledger.usage_at("p", NOON).limit != Some(500) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ledger.usage_at("p", NOON).limit, Some(500));
        ledger.set_limit("p", None);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ledger.usage_at("p", NOON).limit != Some(800) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ledger.usage_at("p", NOON).limit, Some(800));
    }

    #[test]
    fn settings_replace_the_configured_limits_and_are_not_written_to_the_file() {
        let mut state = Snapshot::default();
        state.learn_limit("learned", 800);
        let limit = |provider: &str, daily_limit, reset_offset_minutes| ProviderLimit {
            provider: provider.into(),
            daily_limit,
            reset_offset_minutes,
        };
        state.configure(&[limit("a", Some(100), 120), limit("b", Some(50), 0)]);
        assert_eq!(state.providers["a"].daily_limit, Some(100));
        assert_eq!(state.providers["a"].reset_offset_minutes, 120);
        // `a` removed from Settings: its limit and day start go; `b` stays; learned ones stay.
        state.configure(&[limit("b", Some(50), 0)]);
        assert_eq!(state.providers["a"].daily_limit, None);
        assert_eq!(state.providers["a"].reset_offset_minutes, 0);
        assert_eq!(state.providers["b"].daily_limit, Some(50));
        assert_eq!(state.providers["learned"].learned_limit, Some(800));
        // The file never carries a configured limit (Settings is its only source).
        let written = serde_json::to_value(&state).unwrap();
        assert!(written["providers"]["b"].get("daily_limit").is_none(), "{written}");
        assert_eq!(written["providers"]["learned"]["learned_limit"], 800);
    }

    #[test]
    fn the_reset_is_the_providers_next_midnight() {
        // NOON UTC: the next UTC midnight is 12 h later; in UTC+2 it is 10 h later (22:00 UTC).
        assert_eq!(next_reset(NOON, 0), NOON + 12 * 3600);
        assert_eq!(next_reset(NOON, 120), NOON + 10 * 3600);
        assert_eq!(next_reset(NOON, -300), NOON + 17 * 3600);
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path().join("requests.json"));
        assert_eq!(ledger.usage_at("none", NOON).resets_at, NOON + 12 * 3600);
        assert_eq!(ledger.usage_at("none", NOON).limit, None);
    }

    #[tokio::test]
    async fn the_observer_counts_requests_and_429s_for_the_providers_in_the_context() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path().join("requests.json"));
        let observer = ledger.observer();
        let meta = |provider: &'static str, purpose| RequestMeta {
            provider,
            model: Some("m"),
            purpose,
            session_id: None,
            streaming: false,
        };
        observer.on_request(&meta("a", Purpose::Main));
        observer.on_response(&meta("a", Purpose::Main), Outcome::Status(429));
        observer.on_request(&meta("b", Purpose::Curator));
        observer.on_response(&meta("b", Purpose::Curator), Outcome::Status(200));
        let snapshot = ledger.snapshot().await;
        let now = unix_now();
        assert_eq!(
            (snapshot.today("a", now).total(), snapshot.today("a", now).rate_limited),
            (1, 1)
        );
        assert_eq!(snapshot.today("b", now).by_purpose["curator"], 1);
        assert_eq!(snapshot.today("b", now).rate_limited, 0);
    }
}
