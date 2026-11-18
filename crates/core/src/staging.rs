//! The writes waiting for the user. While a session is tainted (it has read untrusted content),
//! the agent's memory and skill writes are kept in `staging/` instead of being saved
//! (`hermes_tools::staging`); these calls are how the user reviews them: list, apply, reject.
//!
//! **Ours, not Hermes's contract** (Hermes has no taint tracking): the page ignores a method it
//! doesn't know, and an older page never calls these. The page's shapes (`staging.list`,
//! `staging.apply`, `staging.reject` and the REST mirror) are `crates/desktop/src/staging.rs`.
//!
//! - Apply: the write goes through the memory or skill store like a direct one, with its limits,
//!   threat scan and checks that what it was reviewed against is unchanged. When it can't be
//!   applied the item stays listed with the error.
//! - An unknown id, or one that can't be a staged item's, is [`Failure::Missing`].

use hermes_runtime::Runtime;
use hermes_tools::staging::{Applied, StagedItem, display_line, display_text};

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

/// A waiting write as the user reviews it. The text fields go through `display_text` /
/// `display_line` again here: the file on disk is read back, and what the approval screen shows
/// must not hide characters whatever wrote it. The write's payload stays on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    /// `"memory"` or `"skill"`.
    pub kind: &'static str,
    pub summary: String,
    pub diff: String,
    pub reason: String,
    pub session_id: String,
    pub sources: Vec<String>,
    /// Milliseconds since the epoch.
    pub created: i64,
    /// `"pending"` or `"stale"`.
    pub status: &'static str,
    pub error: Option<String>,
}

fn view(item: &StagedItem) -> Item {
    Item {
        id: item.id.clone(),
        kind: item.kind.name(),
        summary: item.summary(),
        diff: display_text(&item.diff),
        reason: display_line(&item.reason),
        session_id: display_line(&item.session_id),
        sources: item.sources.iter().map(|s| display_line(s)).collect(),
        created: item.created,
        status: item.status.name(),
        error: item.error.as_deref().map(display_text),
    }
}

/// A write that went through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub id: String,
    pub kind: &'static str,
    pub message: String,
}

fn missing(id: &str) -> Failure {
    Failure::Missing(format!(
        "No staged item {id}. It may already have been applied or rejected."
    ))
}

/// Every waiting write, oldest first.
pub async fn list(runtime: &Runtime) -> Vec<Item> {
    let store = runtime.staging();
    let items = tokio::task::spawn_blocking(move || store.list())
        .await
        .unwrap_or_default();
    items.iter().map(view).collect()
}

/// Applies a waiting write.
pub async fn apply(runtime: &Runtime, id: &str) -> Result<Done, Failure> {
    let store = runtime.staging();
    let owned = id.to_string();
    let item = tokio::task::spawn_blocking(move || store.get(&owned))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| missing(id))?;
    match runtime.apply_staged(id).await {
        Applied::Done(message) => Ok(Done {
            id: item.id,
            kind: item.kind.name(),
            message,
        }),
        Applied::Failed { error, .. } => Err(Failure::Conflict(format!(
            "Could not apply this change: {error} It stays in the list; reject it if it is no longer wanted."
        ))),
        Applied::Missing => Err(missing(id)),
    }
}

/// Drops a waiting write.
pub async fn reject(runtime: &Runtime, id: &str) -> Result<Done, Failure> {
    let store = runtime.staging();
    let owned = id.to_string();
    let item = tokio::task::spawn_blocking(move || store.reject(&owned))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| missing(id))?;
    Ok(Done {
        id: item.id,
        kind: item.kind.name(),
        message: String::new(),
    })
}
