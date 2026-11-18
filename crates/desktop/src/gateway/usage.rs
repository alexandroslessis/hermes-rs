//! What the model was asked and what it cost, as the page reads it: the request ledger's numbers
//! (`requests.status`), the activity summary (`insights.get`), the writes a tainted session left
//! for review (`staging.*`), and the status bar's usage (`session.usage`). `requests.status` and
//! `staging.*` are ours, not Hermes's. The numbers are `hermes_core::usage`; this file is the JSON.

use std::sync::Arc;

use hermes_core::usage::UsageSnapshot;
use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{InsightsGet, Reply, RequestsStatus, RpcError, StagingId};
use crate::providers::Settings;

impl Gateway {
    /// Today's model requests for a provider (the active one by default): used, limit, when the
    /// count starts over, and the split by purpose.
    pub(super) fn requests_status_call(&self, p: RequestsStatus, settings: &Settings) -> Reply {
        let provider = p
            .provider
            .or_else(|| settings.active().map(|p| p.id.clone()));
        match provider {
            Some(provider) => {
                let usage = self.core.requests_status(&provider);
                Ok(json!({
                    "provider": provider,
                    "used": usage.used,
                    "limit": usage.limit,
                    "resets_at": usage.resets_at,
                    "by_purpose": usage.by_purpose,
                    "rate_limited": usage.rate_limited,
                }))
            }
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
        // Every session and its message count: off the async workers.
        let insights = self.blocking(move |g| g.core.insights(days)).await?;
        Ok(json!({"days": days, "sessions": insights.sessions, "messages": insights.messages}))
    }
}

impl Gateway {
    /// The status bar's usage (`session.usage` / `message.complete`'s `usage`): the session's token
    /// totals, how full the context is (the last call's input tokens against the model's window)
    /// and the average generation speed.
    pub(super) fn usage_snapshot(&self, id: &str, last_input: Option<usize>) -> Value {
        usage_json(&self.core.usage_snapshot(id, last_input))
    }
}

fn usage_json(snapshot: &UsageSnapshot) -> Value {
    let totals = &snapshot.totals;
    let mut usage = json!({
        "model": snapshot.model,
        "input": totals.input_tokens,
        "output": totals.output_tokens,
        "total": totals.input_tokens + totals.output_tokens,
        "calls": totals.calls,
        "avg_tps": snapshot.avg_tps,
    });
    if let Some(context) = snapshot.context {
        usage["context_used"] = json!(context.used);
        usage["context_max"] = json!(context.max);
        usage["context_percent"] = json!(context.percent());
        usage["context_source"] = json!("provider_usage");
        usage["context_estimated"] = json!(false);
    }
    // The status bar's allowance gauge (`plan_gauge`, which the page draws as "<label> <n>%"
    // with the reset time in its tooltip) carries the day's request quota when there is one.
    if let Some(quota) = snapshot.quota {
        usage["plan_gauge"] = json!({
            "id": "requests",
            "label": format!("Requests {}/{}", quota.used, quota.limit),
            "used_bp": quota.used_bp,
            "resets_at_ms": quota.resets_at_ms,
        });
    }
    usage
}
