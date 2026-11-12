//! The writes waiting for the user: the gateway's `staging.list`, `staging.apply` and
//! `staging.reject`, and the same three as REST (`GET /api/staging`, `POST /api/staging/<id>/apply`,
//! `POST /api/staging/<id>/reject`). While a session is tainted (it has read untrusted content),
//! the agent's memory and skill writes are kept in `staging/` instead of being saved
//! (`hermes_tools::staging`); these are how the user reviews them. The Memory and Skills screens
//! show them in CP6.3; until then nothing in the page calls these.
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
use hermes_tools::staging::{Applied, StagedItem, display_line, display_text};
use serde_json::{Value, json};

/// Why a staging request failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// No such item.
    Missing(String),
    /// The item can't be applied (it stays listed).
    Conflict(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Failure::Missing(m) | Failure::Conflict(m) => m,
        }
    }
}

/// The text fields go through `display_text` / `display_line` again here: the file on disk is
/// read back, and what the approval screen shows must not hide characters whatever wrote it.
fn item_json(item: &StagedItem) -> Value {
    json!({
        "id": item.id,
        "kind": item.kind.name(),
        "summary": item.summary(),
        "diff": display_text(&item.diff),
        "reason": display_line(&item.reason),
        "session_id": display_line(&item.session_id),
        "sources": item.sources.iter().map(|s| display_line(s)).collect::<Vec<_>>(),
        "created": item.created,
        "status": item.status.name(),
        "error": item.error.as_deref().map(display_text),
    })
}

fn missing(id: &str) -> Failure {
    Failure::Missing(format!(
        "No staged item {id}. It may already have been applied or rejected."
    ))
}

/// `staging.list`: every waiting write, oldest first.
pub async fn list(runtime: &Runtime) -> Value {
    let store = runtime.staging();
    let items = tokio::task::spawn_blocking(move || store.list())
        .await
        .unwrap_or_default();
    json!({"items": items.iter().map(item_json).collect::<Vec<_>>(), "count": items.len()})
}

/// `staging.apply`.
pub async fn apply(runtime: &Runtime, id: &str) -> Result<Value, Failure> {
    let store = runtime.staging();
    let owned = id.to_string();
    let item = tokio::task::spawn_blocking(move || store.get(&owned))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| missing(id))?;
    match runtime.apply_staged(id).await {
        Applied::Done(message) => Ok(json!({
            "id": item.id, "kind": item.kind.name(), "applied": true, "message": message,
        })),
        Applied::Failed { error, .. } => Err(Failure::Conflict(format!(
            "Could not apply this change: {error} It stays in the list; reject it if it is no longer wanted."
        ))),
        Applied::Missing => Err(missing(id)),
    }
}

/// `staging.reject`.
pub async fn reject(runtime: &Runtime, id: &str) -> Result<Value, Failure> {
    let store = runtime.staging();
    let owned = id.to_string();
    let item = tokio::task::spawn_blocking(move || store.reject(&owned))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| missing(id))?;
    Ok(json!({"id": item.id, "kind": item.kind.name(), "rejected": true}))
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
