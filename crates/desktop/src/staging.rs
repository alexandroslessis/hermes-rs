//! The writes waiting for the user: the gateway's `staging.list`, `staging.apply` and
//! `staging.reject`, and the same three as REST (`GET /api/staging`, `POST /api/staging/<id>/apply`,
//! `POST /api/staging/<id>/reject`). While a session is tainted (it has read untrusted content),
//! the agent's memory and skill writes are kept in `staging/` instead of being saved
//! (`hermes_tools::staging`); these are how the user reviews them. The logic is
//! `hermes_core::staging`; this file is the page's JSON.
//!
//! **Ours, not Hermes's contract** (Hermes has no taint tracking): the page ignores a method it
//! doesn't know, and an older page never calls these. The shapes are kept small and stable:
//!
//! - `staging.list` -> `{"items": [item...], "count": n}`; an item is `{id, kind: "memory"|"skill",
//!   summary, diff, reason, session_id, sources: [str], created (ms), status: "pending"|"stale",
//!   error: str|null}`. The write's payload stays on disk (the diff is what the user reviews; it
//!   is whole, and control, zero-width and bidi characters in it are shown as `<U+XXXX>`).
//! - `staging.apply {id}` -> `{id, kind, applied: true, message}`. The write goes through the memory
//!   or skill store like a direct one, with its limits, threat scan and checks that what it was
//!   reviewed against is unchanged. When it can't be applied the answer is an error that says why
//!   ("... has changed since this was staged"), and the item stays listed with that error.
//! - `staging.reject {id}` -> `{id, kind, rejected: true}`.
//! - An unknown id, or one that can't be a staged item's, is an error ("No staged item ..."); a
//!   missing `id` is `-32602`. REST errors are `{"detail": ...}`: 404 unknown, 409 couldn't apply.

use hermes_runtime::Runtime;
use serde_json::{Value, json};

pub use hermes_core::staging::Failure;

/// `staging.list`: every waiting write, oldest first.
pub async fn list(runtime: &Runtime) -> Value {
    let items = hermes_core::staging::list(runtime).await;
    json!({
        "items": items.iter().map(|item| json!({
            "id": item.id,
            "kind": item.kind,
            "summary": item.summary,
            "diff": item.diff,
            "reason": item.reason,
            "session_id": item.session_id,
            "sources": item.sources,
            "created": item.created,
            "status": item.status,
            "error": item.error,
        })).collect::<Vec<_>>(),
        "count": items.len(),
    })
}

/// `staging.apply`.
pub async fn apply(runtime: &Runtime, id: &str) -> Result<Value, Failure> {
    let done = hermes_core::staging::apply(runtime, id).await?;
    Ok(json!({
        "id": done.id, "kind": done.kind, "applied": true, "message": done.message,
    }))
}

/// `staging.reject`.
pub async fn reject(runtime: &Runtime, id: &str) -> Result<Value, Failure> {
    let done = hermes_core::staging::reject(runtime, id).await?;
    Ok(json!({"id": done.id, "kind": done.kind, "rejected": true}))
}

/// The REST mirror: `None` for a path that isn't ours.
pub async fn route(runtime: &Runtime, method: &str, path: &str) -> Option<(u16, Value)> {
    let rest = path.strip_prefix("/api/staging")?;
    let answer = match (method, rest.trim_matches('/')) {
        ("GET", "") => return Some((200, list(runtime).await)),
        ("POST", tail) => match tail.split_once('/') {
            Some((id, "apply")) => apply(runtime, id).await,
            Some((id, "reject")) => reject(runtime, id).await,
            _ => return None,
        },
        _ => return None,
    };
    Some(match answer {
        Ok(body) => (200, body),
        Err(Failure::Missing(m)) => (404, json!({"detail": m})),
        Err(Failure::Conflict(m)) => (409, json!({"detail": m})),
    })
}
