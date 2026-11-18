//! What the model was asked and what it cost: a session's token totals and how full its context
//! is (the status bar), the generation speed, the request ledger's numbers for a provider
//! (`requests.status`), and the activity summary (`insights.get`). `requests.status` is ours, not
//! Hermes's.
//!
//! Typed; the page's `session.usage` / `requests.status` / `insights.get` JSON is
//! `crates/desktop/src/gateway/usage.rs`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use hermes_runtime::ledger::Usage as RequestUsage;
use hermes_store::UsageTotals;

use crate::Core;
use crate::settings::FALLBACK_CONTEXT;

/// Each session's generated tokens and generation seconds (the status bar's tokens per second),
/// in the data folder.
pub const GENERATION_FILE: &str = "generation-stats.json";

/// Output tokens generated and the seconds the model calls took, per session; kept in
/// [`GENERATION_FILE`].
pub(crate) struct Generation(Mutex<HashMap<String, (u64, f64)>>);

impl Generation {
    pub(crate) fn load(data_dir: &Path) -> Self {
        Self(Mutex::new(
            std::fs::read(data_dir.join(GENERATION_FILE))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
        ))
    }
}

/// How full a session's context is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUse {
    pub used: usize,
    pub max: usize,
}

impl ContextUse {
    /// Percent full, at most 100.
    pub fn percent(&self) -> usize {
        (self.used * 100 / self.max).min(100)
    }
}

/// The status bar's allowance gauge: today's request quota of the session's provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaGauge {
    pub used: u64,
    pub limit: u32,
    /// Basis points (1/100 of a percent) of the quota used, at most 10,000.
    pub used_bp: u64,
    /// When the count starts over, unix milliseconds.
    pub resets_at_ms: i64,
}

/// A session's usage as the status bar shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    /// The session's model.
    pub model: Option<String>,
    pub totals: UsageTotals,
    /// Average generation speed, tokens per second.
    pub avg_tps: Option<f64>,
    /// `None` until a call has told how many input tokens the context holds.
    pub context: Option<ContextUse>,
    /// `None` when the provider has no daily limit.
    pub quota: Option<QuotaGauge>,
}

/// Activity over a number of days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insights {
    pub sessions: usize,
    pub messages: usize,
}

impl Core {
    /// Counts one model call's output toward the session's tokens per second.
    pub fn note_generation(&self, id: &str, output_tokens: u64, seconds: f64) {
        if output_tokens == 0 || seconds <= 0.0 {
            return;
        }
        if let Ok(mut generation) = self.generation.0.lock() {
            let entry = generation.entry(id.to_string()).or_insert((0, 0.0));
            entry.0 += output_tokens;
            entry.1 += seconds;
            // Kept across restarts (beside the store, whose schema the GPUI app shares).
            if let Ok(json) = serde_json::to_vec(&*generation) {
                let _ = std::fs::write(self.data_dir.join(GENERATION_FILE), json);
            }
        }
    }

    /// The status bar's usage: the session's token totals, how full the context is (the last
    /// call's input tokens, `last_input`, against the model's window) and the average generation
    /// speed.
    pub fn usage_snapshot(&self, id: &str, last_input: Option<usize>) -> UsageSnapshot {
        let store = self.runtime.store();
        let totals = store.usage_totals(id).unwrap_or_default();
        let record = store.session(id).ok().flatten();
        let context_max = record
            .as_ref()
            .map(|r| {
                self.models
                    .get(&r.provider, &r.model)
                    .and_then(|facts| facts.context_length)
                    .unwrap_or(FALLBACK_CONTEXT)
            })
            .unwrap_or(0);
        let used = last_input.unwrap_or(totals.last_input_tokens.max(0) as usize);
        let used = if context_max > 0 {
            used.min(context_max)
        } else {
            used
        };
        let avg_tps = self
            .generation
            .0
            .lock()
            .ok()
            .and_then(|g| g.get(id).copied())
            .filter(|(_, seconds)| *seconds > 0.0)
            .map(|(tokens, seconds)| tokens as f64 / seconds);
        // The allowance gauge carries the day's request quota when there is one.
        let quota = record.as_ref().and_then(|record| {
            let quota = self.runtime.ledger().usage(&record.provider);
            let limit = quota.limit.filter(|limit| *limit > 0)?;
            Some(QuotaGauge {
                used: quota.used,
                limit,
                used_bp: (quota.used.saturating_mul(10_000) / u64::from(limit)).min(10_000),
                resets_at_ms: quota.resets_at * 1000,
            })
        });
        UsageSnapshot {
            model: record.map(|r| r.model),
            totals,
            avg_tps,
            context: (context_max > 0 && used > 0).then_some(ContextUse {
                used,
                max: context_max,
            }),
            quota,
        }
    }

    /// Today's model requests for a provider: used, limit, when the count starts over, and the
    /// split by purpose.
    pub fn requests_status(&self, provider: &str) -> RequestUsage {
        self.runtime.ledger().usage(provider)
    }

    /// Activity over the last `days`: sessions touched and their messages. Blocking (every
    /// session and its message count).
    pub fn insights(&self, days: i64) -> Insights {
        // (The store keeps milliseconds.)
        let since = chrono::Utc::now().timestamp_millis() - days * 86_400_000;
        let store = self.runtime.store();
        let counts = store.message_counts().unwrap_or_default();
        let recent: Vec<_> = store
            .list_sessions(100_000)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.updated_at >= since)
            .collect();
        let messages: usize = recent.iter().filter_map(|s| counts.get(&s.id)).sum();
        Insights {
            sessions: recent.len(),
            messages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_percent_is_capped() {
        assert_eq!(ContextUse { used: 50, max: 200 }.percent(), 25);
        assert_eq!(
            ContextUse {
                used: 500,
                max: 200
            }
            .percent(),
            100
        );
    }
}
