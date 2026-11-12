//! The governor: decides from today's request count whether a kind of request may go out.
//!
//! Not from Hermes (backend behaviour is ours, AGENTS.md rule 1; `docs/backend/PHASES.md`
//! Phase 2): the user's provider allows a fixed number of requests per day, so when the quota runs
//! low the agent stops spending it on things the user didn't ask for, and keeps the rest for the
//! conversation. With no limit known (not set in Settings and none learned from a 429) it allows
//! everything.
//!
//! | requests left | main turn | `btw`, `compress` | `delegate`, `approval` | `review`, `curator`, `title` |
//! |---|---|---|---|---|
//! | 20% or more | allowed | allowed | allowed | allowed |
//! | under 20% | allowed | allowed | allowed | denied (deferred) |
//! | under 5% | allowed, with a warning | allowed | denied | denied |
//! | none | refused | refused | refused | refused |
//!
//! A denied background job is not an error: the caller skips it and tries again next time. The
//! main turn is only ever refused at zero, with a message saying when the quota resets.

use hermes_llm::observer::Purpose;

use crate::ledger::{Ledger, Usage};

/// Below this share of the quota left, background jobs are deferred.
const LOW_PERCENT: u64 = 20;
/// Below this share left, delegation and the approval judge stop too, and turns are warned.
const CRITICAL_PERCENT: u64 = 5;

/// What the governor says about one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Allowed, but the user should know the quota is nearly gone.
    Warn(String),
    /// Don't send it; the text says why.
    Deny(String),
}

impl Verdict {
    pub fn is_denied(&self) -> bool {
        matches!(self, Verdict::Deny(_))
    }
}

/// The rule table above, for today's `usage` of `provider`.
pub fn decide(usage: &Usage, purpose: Purpose, provider: &str) -> Verdict {
    let Some(limit) = usage.limit.filter(|limit| *limit > 0) else {
        return Verdict::Allow;
    };
    let limit = u64::from(limit);
    let left = limit.saturating_sub(usage.used);
    let under = |percent: u64| left * 100 < limit * percent;
    if left == 0 {
        return Verdict::Deny(format!(
            "The daily request limit of {provider} is used up ({} of {limit}). It resets at {}.",
            usage.used,
            reset_text(usage.resets_at)
        ));
    }
    let background = matches!(purpose, Purpose::Review | Purpose::Curator | Purpose::Title);
    let optional = background || matches!(purpose, Purpose::Delegate | Purpose::Approval);
    if under(CRITICAL_PERCENT) {
        if optional {
            return Verdict::Deny(format!(
                "Only {left} of {limit} requests are left today on {provider}; {} is skipped.",
                purpose.as_str()
            ));
        }
        if purpose == Purpose::Main {
            return Verdict::Warn(format!(
                "Only {left} of {limit} requests are left today on {provider} (resets at {}).",
                reset_text(usage.resets_at)
            ));
        }
    } else if under(LOW_PERCENT) && background {
        return Verdict::Deny(format!(
            "{left} of {limit} requests are left today on {provider}; background {} waits.",
            purpose.as_str()
        ));
    }
    Verdict::Allow
}

fn reset_text(resets_at: i64) -> String {
    chrono::DateTime::from_timestamp(resets_at, 0)
        .map(|t| t.format("%H:%M UTC on %Y-%m-%d").to_string())
        .unwrap_or_else(|| "midnight".into())
}

/// Asks the ledger how much of today is used and applies [`decide`].
#[derive(Clone)]
pub struct Governor {
    ledger: Ledger,
}

impl Governor {
    pub fn new(ledger: Ledger) -> Self {
        Self { ledger }
    }

    /// May a request of `purpose` go to `provider` now?
    pub fn check(&self, provider: &str, purpose: Purpose) -> Verdict {
        decide(&self.ledger.usage(provider), purpose, provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(used: u64, limit: Option<u32>) -> Usage {
        Usage {
            used,
            limit,
            resets_at: 1_794_528_000,
            ..Default::default()
        }
    }

    fn verdicts(used: u64, limit: u32) -> Vec<(Purpose, Verdict)> {
        Purpose::ALL
            .into_iter()
            .map(|purpose| (purpose, decide(&usage(used, Some(limit)), purpose, "p")))
            .collect()
    }

    fn only_denied(list: &[(Purpose, Verdict)]) -> Vec<Purpose> {
        list.iter()
            .filter(|(_, v)| v.is_denied())
            .map(|(p, _)| *p)
            .collect()
    }

    fn verdict_for(list: &[(Purpose, Verdict)], purpose: Purpose) -> Verdict {
        list.iter().find(|(p, _)| *p == purpose).unwrap().1.clone()
    }

    #[test]
    fn without_a_limit_everything_is_allowed() {
        for used in [0, 800, 100_000] {
            for purpose in Purpose::ALL {
                assert_eq!(decide(&usage(used, None), purpose, "p"), Verdict::Allow);
                assert_eq!(decide(&usage(used, Some(0)), purpose, "p"), Verdict::Allow);
            }
        }
    }

    #[test]
    fn the_background_edge_is_exactly_twenty_percent() {
        assert!(only_denied(&verdicts(0, 800)).is_empty());
        // 20% left (160 of 800) is not "below 20%"; 159 is.
        assert!(only_denied(&verdicts(640, 800)).is_empty());
        assert_eq!(
            only_denied(&verdicts(641, 800)),
            vec![Purpose::Review, Purpose::Curator, Purpose::Title]
        );
    }

    #[test]
    fn below_twenty_percent_only_background_jobs_wait() {
        let list = verdicts(700, 800); // 12.5% left
        assert_eq!(
            only_denied(&list),
            vec![Purpose::Review, Purpose::Curator, Purpose::Title]
        );
        for purpose in [
            Purpose::Main,
            Purpose::Delegate,
            Purpose::Approval,
            Purpose::Compress,
            Purpose::Btw,
        ] {
            assert_eq!(verdict_for(&list, purpose), Verdict::Allow, "{purpose:?}");
        }
    }

    #[test]
    fn below_five_percent_delegation_and_the_judge_stop_and_the_main_turn_is_warned() {
        let list = verdicts(765, 800); // 35 left = 4.4%
        assert_eq!(
            only_denied(&list),
            vec![
                Purpose::Review,
                Purpose::Curator,
                Purpose::Title,
                Purpose::Approval,
                Purpose::Delegate
            ]
        );
        let main = verdict_for(&list, Purpose::Main);
        assert!(matches!(&main, Verdict::Warn(text) if text.contains("35 of 800")), "{main:?}");
        // The user's own side questions and the compression that keeps a turn going still work.
        for purpose in [Purpose::Btw, Purpose::Compress] {
            assert_eq!(verdict_for(&list, purpose), Verdict::Allow);
        }
        // 5% exactly (40 of 800) is not below it.
        assert!(!only_denied(&verdicts(760, 800)).contains(&Purpose::Delegate));
    }

    #[test]
    fn at_zero_even_the_main_turn_is_refused_with_the_reset_time() {
        for used in [800, 801, 5000] {
            for (purpose, verdict) in verdicts(used, 800) {
                let Verdict::Deny(text) = verdict else {
                    panic!("{purpose:?} at {used} of 800 should be denied");
                };
                assert!(text.contains("used up") && text.contains("resets at"), "{text}");
                assert!(text.contains("2026-11-13"), "{text}");
            }
        }
    }

    #[test]
    fn the_main_turn_is_only_refused_at_zero() {
        for used in 0..800 {
            let verdict = decide(&usage(used, Some(800)), Purpose::Main, "p");
            assert!(!verdict.is_denied(), "refused at {used} of 800");
        }
    }

    #[test]
    fn the_governor_reads_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path().join("requests.json"));
        let governor = Governor::new(ledger.clone());
        let now = 1_794_484_800;
        assert_eq!(governor.check("p", Purpose::Review), Verdict::Allow);
        ledger.set_limit("p", Some(10));
        for _ in 0..9 {
            ledger.record_at("p", Purpose::Main, now);
        }
        // The counts are applied by the ledger thread; wait for it (the request path never waits).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ledger.usage_at("p", now).used < 9 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let usage = ledger.usage_at("p", now);
        assert_eq!((usage.used, usage.limit), (9, Some(10)));
        assert!(decide(&usage, Purpose::Review, "p").is_denied());
        assert_eq!(decide(&usage, Purpose::Btw, "p"), Verdict::Allow);
    }
}
