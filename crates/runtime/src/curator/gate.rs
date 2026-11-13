//! The per-cluster curator's own limits, besides the quota governor (which stops background jobs
//! below 20% of the day's requests): an hourly cap and a back-off, as the review scheduler has
//! (`review_schedule.rs`). The curator runs weekly, so these only matter when the user presses Run
//! now again and again, or the model endpoint keeps failing.
//!
//! - **Hourly cap.** At most [`RUNS_PER_HOUR`] runs that send a request start per hour.
//! - **Back-off.** A run in which a request failed (a network error, a 429, a timeout) makes the
//!   next run wait [`BACKOFF_FIRST`], doubling per failure in a row up to [`BACKOFF_MAX`]; a run
//!   whose requests all got an answer resets it (a reply unreadable twice is remembered as judged
//!   in `seen.rs` instead, so it isn't asked again either). A run the governor held back sent
//!   nothing and changes nothing here.
//!
//! In memory: a restart opens a new window. A crash loop still can't burst: the scheduled run
//! saves its `last_run_at` (`curator_state`) *before* its first request, so after a restart the
//! schedule waits its week again, and the jobs a run judged are on disk (`seen.rs`); only the
//! user's own Run now starts a run outside the schedule.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Runs that send a request, per hour.
pub const RUNS_PER_HOUR: usize = 2;
/// The wait after the first failed run.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(3600);
/// The longest wait.
pub const BACKOFF_MAX: Duration = Duration::from_secs(24 * 3600);
const HOUR: Duration = Duration::from_secs(3600);

#[derive(Default)]
pub struct Gate {
    started: VecDeque<Instant>,
    failures: u32,
    retry_after: Option<Instant>,
}

impl Gate {
    /// A run is about to send its first request: `Ok` records it, `Err` says why not now.
    pub fn admit(&mut self, now: Instant) -> Result<(), String> {
        if let Some(until) = self.retry_after
            && until > now
        {
            return Err(format!(
                "backing off after a failed run; next try in {} minutes",
                (until - now).as_secs().div_ceil(60)
            ));
        }
        while self
            .started
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            self.started.pop_front();
        }
        if self.started.len() >= RUNS_PER_HOUR {
            let wait = self
                .started
                .front()
                .map(|t| HOUR.saturating_sub(now.duration_since(*t)))
                .unwrap_or_default();
            return Err(format!(
                "{RUNS_PER_HOUR} runs already started this hour; next try in {} minutes",
                wait.as_secs().div_ceil(60)
            ));
        }
        self.started.push_back(now);
        Ok(())
    }

    /// A run that was admitted ended: `failed` when a request of it failed.
    pub fn finished(&mut self, now: Instant, failed: bool) {
        if failed {
            self.failures += 1;
            let wait = BACKOFF_FIRST
                .saturating_mul(1u32 << (self.failures - 1).min(10))
                .min(BACKOFF_MAX);
            self.retry_after = Some(now + wait);
        } else {
            self.failures = 0;
            self.retry_after = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hourly_cap_frees_as_the_window_passes() {
        let mut gate = Gate::default();
        let t0 = Instant::now();
        assert!(gate.admit(t0).is_ok());
        assert!(gate.admit(t0 + Duration::from_secs(60)).is_ok());
        let refused = gate.admit(t0 + Duration::from_secs(120)).unwrap_err();
        assert!(refused.contains("2 runs already started"), "{refused}");
        assert!(gate.admit(t0 + HOUR).is_ok(), "the first one aged out");
    }

    #[test]
    fn failures_back_off_and_double_and_a_good_run_resets() {
        let mut gate = Gate::default();
        let t0 = Instant::now();
        gate.admit(t0).unwrap();
        gate.finished(t0, true);
        assert!(gate.admit(t0 + Duration::from_secs(1800)).unwrap_err().contains("30 minutes"));
        gate.admit(t0 + HOUR).unwrap();
        gate.finished(t0 + HOUR, true);
        // Two failures in a row: two hours.
        assert!(gate.admit(t0 + HOUR + Duration::from_secs(3600)).is_err());
        let later = t0 + HOUR + Duration::from_secs(2 * 3600);
        let mut fresh = Gate::default();
        fresh.finished(t0, true);
        for _ in 0..30 {
            fresh.finished(t0, true);
        }
        assert!(fresh.retry_after.unwrap() <= t0 + BACKOFF_MAX);
        gate.started.clear();
        gate.admit(later).unwrap();
        gate.finished(later, false);
        gate.admit(later + Duration::from_secs(1)).unwrap();
    }
}
