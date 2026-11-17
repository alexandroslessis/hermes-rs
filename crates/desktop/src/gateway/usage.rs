//! What the model was asked and what it cost: the request ledger's numbers (`requests.status`),
//! the activity summary (`insights.get`), the writes a tainted session left for review
//! (`staging.*`), and each session's generation speed. `requests.status` and `staging.*` are ours,
//! not Hermes's.

use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{InsightsGet, Reply, RequestsStatus, RpcError, StagingId};
use super::*;
use crate::providers::Settings;

impl Gateway {
    /// Today's model requests for a provider (the active one by default): used, limit, when the
    /// count starts over, and the split by purpose.
    pub(super) fn requests_status_call(&self, p: RequestsStatus, settings: &Settings) -> Reply {
        let provider = p
            .provider
            .or_else(|| settings.active().map(|p| p.id.clone()));
        match provider {
            Some(provider) => Ok(requests_status(&self.runtime, &provider)),
            None => Err(RpcError::invalid("No provider is configured")),
        }
    }

    /// `staging.apply` / `staging.reject` (`staging.rs`).
    pub(super) async fn staging_decide(self: &Arc<Self>, p: StagingId, apply: bool) -> Reply {
        let Some(id) = p.id.as_deref() else {
            return Err(RpcError::invalid("id is required"));
        };
        let answer = if apply {
            crate::staging::apply(&self.runtime, id).await
        } else {
            crate::staging::reject(&self.runtime, id).await
        };
        answer.map_err(|failure| RpcError::server(failure.message().to_string()))
    }

    /// Activity over the last `days` (default 30): sessions touched and their messages.
    pub(super) async fn insights(self: &Arc<Self>, p: InsightsGet) -> Reply {
        let days = p.days;
        // (The store keeps milliseconds.)
        let since = chrono::Utc::now().timestamp_millis() - days * 86_400_000;
        // Every session and its message count: off the async workers.
        let (sessions, messages) = self
            .blocking(move |g| {
                let store = g.runtime.store();
                let counts = store.message_counts().unwrap_or_default();
                let recent: Vec<_> = store
                    .list_sessions(100_000)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| s.updated_at >= since)
                    .collect();
                let messages: usize = recent.iter().filter_map(|s| counts.get(&s.id)).sum();
                (recent.len(), messages)
            })
            .await?;
        Ok(json!({"days": days, "sessions": sessions, "messages": messages}))
    }
}

impl Gateway {
    /// Counts one model call's output toward the session's tokens per second.
    pub(super) fn note_generation(&self, id: &str, output_tokens: u64, seconds: f64) {
        if output_tokens == 0 || seconds <= 0.0 {
            return;
        }
        if let Ok(mut generation) = self.generation.lock() {
            let entry = generation.entry(id.to_string()).or_insert((0, 0.0));
            entry.0 += output_tokens;
            entry.1 += seconds;
            // Kept across restarts (beside the store, whose schema the GPUI app shares).
            if let Ok(json) = serde_json::to_vec(&*generation) {
                let _ = std::fs::write(self.data_dir.join(GENERATION_FILE), json);
            }
        }
    }

    /// The status bar's usage (`session.usage` / `message.complete`'s `usage`): the session's token
    /// totals, how full the context is (the last call's input tokens against the model's window)
    /// and the average generation speed.
    pub(super) fn usage_snapshot(&self, id: &str, last_input: Option<usize>) -> Value {
        let store = self.runtime.store();
        let totals = store.usage_totals(id).unwrap_or_default();
        let record = store.session(id).ok().flatten();
        let context_max = record
            .as_ref()
            .map(|r| {
                let key = format!("{}\n{}", r.provider, r.model);
                self.context_lengths
                    .lock()
                    .ok()
                    .and_then(|lengths| lengths.get(&key).and_then(|facts| facts.context_length))
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
            .lock()
            .ok()
            .and_then(|g| g.get(id).copied())
            .filter(|(_, seconds)| *seconds > 0.0)
            .map(|(tokens, seconds)| tokens as f64 / seconds);
        let mut usage = json!({
            "model": record.as_ref().map(|r| r.model.clone()),
            "input": totals.input_tokens,
            "output": totals.output_tokens,
            "total": totals.input_tokens + totals.output_tokens,
            "calls": totals.calls,
            "avg_tps": avg_tps,
        });
        if context_max > 0 && used > 0 {
            usage["context_used"] = json!(used);
            usage["context_max"] = json!(context_max);
            usage["context_percent"] = json!((used * 100 / context_max).min(100));
            usage["context_source"] = json!("provider_usage");
            usage["context_estimated"] = json!(false);
        }
        // The status bar's allowance gauge (`plan_gauge`, which the page draws as "<label> <n>%"
        // with the reset time in its tooltip) carries the day's request quota when there is one.
        if let Some(record) = record.as_ref() {
            let quota = self.runtime.ledger().usage(&record.provider);
            if let Some(limit) = quota.limit.filter(|limit| *limit > 0) {
                let used_bp = (quota.used.saturating_mul(10_000) / u64::from(limit)).min(10_000);
                usage["plan_gauge"] = json!({
                    "id": "requests",
                    "label": format!("Requests {}/{limit}", quota.used),
                    "used_bp": used_bp,
                    "resets_at_ms": quota.resets_at * 1000,
                });
            }
        }
        usage
    }
}

fn requests_status(runtime: &hermes_runtime::Runtime, provider: &str) -> Value {
    let usage = runtime.ledger().usage(provider);
    json!({
        "provider": provider,
        "used": usage.used,
        "limit": usage.limit,
        "resets_at": usage.resets_at,
        "by_purpose": usage.by_purpose,
        "rate_limited": usage.rate_limited,
    })
}
