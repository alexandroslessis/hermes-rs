//! Sessions: the list (with what the sidebar shows beside each: running, waiting for the user,
//! archived, its git identity, its parent), creating one, renaming, deleting, archiving and the
//! idle sweep, branching, moving to another folder, stopping a turn, and rewinding to a typed
//! message.
//!
//! Hermes: `tui_gateway/methods_session.py` (create, resume, title, delete), `tui_gateway/
//! methods_session_branch.py` (branch; see `branch.rs` for how ours differs),
//! `hermes_cli/web_routers/sessions.py` (the list, `archived=`), `gateway/
//! session_mutation_policy.py` (the workspace move).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_llm::conversation::message::Message;
use hermes_runtime::ProviderConfig;
use hermes_store::SessionRecord;

use crate::events::CoreEvent;
use crate::workspace::MoveError;
use crate::{Core, archive, branch, transcript, workspace};

/// One session as a list shows it.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub record: SessionRecord,
    pub message_count: usize,
    /// A turn holds the session now.
    pub running: bool,
    /// A turn or the queued prompts after it are in flight (what a front end shows as busy).
    pub live: bool,
    /// A question waits for the user.
    pub waiting: bool,
    pub archived: bool,
    /// The branch and repository root a move recorded; `None` for a session that never moved
    /// (it groups by its folder alone).
    pub git_branch: Option<String>,
    pub git_repo_root: Option<String>,
    /// The session it was branched from.
    pub parent: Option<String>,
}

/// A new session.
pub struct NewSession {
    pub provider: ProviderConfig,
    pub model: String,
    pub cwd: PathBuf,
    pub title: Option<String>,
    /// The messages it starts with (a branch a front end built itself).
    pub seeded: Vec<Message>,
    /// The session it comes from, listed under it.
    pub parent: Option<String>,
}

/// Where a branch cuts its parent's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPoint {
    /// All of it.
    Whole,
    /// Up to and including this stored row.
    ThroughRow(usize),
    /// Through the `n`-th visible entry (1-based): a message's row, or all of it when that entry
    /// is a tool call.
    ThroughEntry(usize),
}

/// A branch to make.
pub struct BranchRequest {
    pub parent: String,
    pub point: BranchPoint,
    /// Its folder; the parent's when `None`.
    pub cwd: Option<String>,
    /// Its title; `<parent> (branch)` when `None`.
    pub name: Option<String>,
    /// A retried request with the same key gets the branch the first one made.
    pub idempotency_key: Option<String>,
}

/// A branch made (or found again by its key).
pub struct Branch {
    pub child: String,
    /// The messages it starts with.
    pub kept: Vec<Message>,
    /// The parent's record (the branch's messages are shown with its provider and model).
    pub parent: SessionRecord,
}

/// Why a branch wasn't made.
#[derive(Debug)]
pub enum BranchError<E> {
    /// No such parent session (the id).
    UnknownSession(String),
    /// Nothing to branch: the cut keeps no message.
    Empty,
    /// The parent's provider couldn't be resolved (the caller's error).
    Provider(E),
    /// Reading or writing the store failed.
    Failed(anyhow::Error),
}

/// Why a rewind was refused.
#[derive(Debug)]
pub enum RewindError {
    /// A turn runs in the session.
    Busy,
    Failed(anyhow::Error),
}

/// What a rewind to a typed message did.
pub struct Rewound {
    /// How many stored messages it dropped.
    pub removed: usize,
    /// What the user had typed there (without the context the index attached).
    pub typed: String,
}

impl Core {
    /// Whether the session has a turn in flight as a front end shows it: the runtime's turn, or
    /// the gap between a turn and its queued follow-up.
    pub fn is_live(&self, id: &str) -> bool {
        self.runtime
            .existing_session_handle(id)
            .is_some_and(|handle| handle.is_live())
    }

    /// One stored session as a list shows it.
    pub fn summary(&self, record: SessionRecord, message_count: usize) -> SessionSummary {
        let identity = self.workspaces.identity(&record.id);
        let (git_branch, git_repo_root) = identity.unwrap_or_default();
        SessionSummary {
            running: self.runtime.is_running(&record.id),
            live: self.is_live(&record.id),
            waiting: self.questions.is_waiting(&record.id),
            archived: self.archive.is_archived(&record.id),
            parent: self.branches.parent(&record.id),
            git_branch,
            git_repo_root,
            message_count,
            record,
        }
    }

    /// The sessions (up to 1,000, newest first) the filter keeps. Blocking (store reads).
    pub fn sessions(&self, filter: archive::Filter) -> Vec<SessionSummary> {
        let store = self.runtime.store();
        let all = store.list_sessions(1000).unwrap_or_default();
        let counts = store.message_counts().unwrap_or_default();
        all.into_iter()
            .filter(|s| filter.keeps(self.archive.is_archived(&s.id)))
            .map(|s| {
                let count = counts.get(&s.id).copied().unwrap_or(0);
                self.summary(s, count)
            })
            .collect()
    }

    /// The sessions with a turn in flight (among the newest 200). Blocking.
    pub fn live_sessions(&self) -> Vec<SessionRecord> {
        let Ok(sessions) = self.runtime.store().list_sessions(200) else {
            return Vec::new();
        };
        sessions
            .into_iter()
            .filter(|s| self.is_live(&s.id))
            .collect()
    }

    /// Tells the front ends the session's title changed.
    pub fn announce_title(&self, id: &str, title: &str) {
        self.publish(CoreEvent::TitleChanged {
            session_id: id.to_string(),
            title: title.to_string(),
        });
    }

    /// Renames a session (trimmed) and tells the front ends. Blocking.
    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        self.runtime.store().set_title(id, title)?;
        self.announce_title(id, title);
        Ok(())
    }

    /// Deletes a session: its rows, its live state and its turn; it leaves the archive.
    pub async fn delete_session(self: &Arc<Self>, id: &str) -> Result<()> {
        self.runtime.delete_session(id).await?;
        self.forget_archived(id).await
    }

    /// A deleted session leaves the archive (a file write: off the async workers).
    pub async fn forget_archived(self: &Arc<Self>, id: &str) -> Result<()> {
        let (this, id) = (self.clone(), id.to_string());
        tokio::task::spawn_blocking(move || this.archive.forget(&id))
            .await
            .context("the archive's thread stopped")
    }

    /// Archives or restores a session (the sidebar, a shortcut, Settings ▸ Sessions). An
    /// archived session leaves the pins, as Hermes's does. `Ok(false)`: no such session.
    /// Blocking.
    pub fn set_archived(&self, id: &str, archived: bool) -> Result<bool> {
        let store = self.runtime.store();
        let Some(record) = store.session(id).ok().flatten() else {
            return Ok(false);
        };
        if archived && record.pinned {
            store.set_pinned(id, false)?;
        }
        self.archive
            .set(id, archived, chrono::Utc::now().timestamp_millis());
        Ok(true)
    }

    /// A session is used again: one the idle sweep hid comes back.
    pub fn reopened(&self, id: &str) {
        self.archive.reopened(id);
    }

    /// The idle sweep (Settings ▸ Sessions' auto-archive): unpinned sessions idle `days` or more,
    /// not running a turn, go to the archive, at most once a day. Returns the ids it archived.
    /// Blocking.
    pub fn auto_archive(&self, days: f64) -> Vec<String> {
        let all = self
            .runtime
            .store()
            .list_sessions(100_000)
            .unwrap_or_default();
        let candidates: Vec<archive::Candidate<'_>> = all
            .iter()
            .map(|s| archive::Candidate {
                id: &s.id,
                last_active: s.updated_at,
                pinned: s.pinned,
                running: self.runtime.is_running(&s.id),
            })
            .collect();
        let swept = self
            .archive
            .sweep(&candidates, days, chrono::Utc::now().timestamp_millis());
        if !swept.is_empty() {
            tracing::info!(
                "auto-archive: archived {} session(s) idle for {days} days or more",
                swept.len()
            );
        }
        swept
    }

    /// Creates a session (its system prompt is built and frozen now), with its title, its seeded
    /// messages and its parent. Blocking.
    pub fn create_session(&self, new: NewSession) -> Result<(SessionRecord, Vec<Message>)> {
        let record = self
            .runtime
            .create_session(&new.provider, &new.model, None, &new.cwd)?;
        if let Some(title) = new.title.as_deref().filter(|t| !t.is_empty()) {
            let _ = self.runtime.store().set_title(&record.id, title);
        }
        if !new.seeded.is_empty() {
            self.runtime
                .store()
                .replace_messages(&record.id, &new.seeded)?;
        }
        if let Some(parent) = new.parent.as_deref().filter(|p| !p.is_empty()) {
            self.branches.link(&record.id, parent);
            // Seeded from the parent's rows: as careful as the parent (Phase 6).
            if !new.seeded.is_empty() {
                self.runtime.copy_taint(parent, &record.id);
            }
        }
        Ok((record, new.seeded))
    }

    /// `/branch`: a new session starting with the parent's conversation up to `point`, listed
    /// under it (`branch.rs`). `provider` resolves the parent's provider, asked only when a new
    /// branch is made. Blocking.
    pub fn branch<E>(
        &self,
        request: BranchRequest,
        provider: impl FnOnce() -> std::result::Result<ProviderConfig, E>,
    ) -> std::result::Result<Branch, BranchError<E>> {
        let parent_id = request.parent;
        let store = self.runtime.store();
        let parent = store
            .session(&parent_id)
            .ok()
            .flatten()
            .ok_or_else(|| BranchError::UnknownSession(parent_id.clone()))?;
        let key = request.idempotency_key.filter(|k| !k.is_empty());
        let existing = key.as_ref().and_then(|key| {
            self.branch_keys
                .lock()
                .ok()
                .and_then(|keys| keys.get(key).cloned())
        });
        // A retried request: the branch it already made.
        if let Some(child) = existing {
            let kept = self.runtime.messages(&child).unwrap_or_default();
            return Ok(Branch {
                child,
                kept,
                parent,
            });
        }
        let messages = self
            .runtime
            .messages(&parent_id)
            .map_err(BranchError::Failed)?;
        let keep = match request.point {
            BranchPoint::Whole => messages.len(),
            BranchPoint::ThroughRow(row) => row,
            BranchPoint::ThroughEntry(n) => transcript::entries(&messages)
                .get(n.saturating_sub(1))
                .and_then(transcript::Entry::row_id)
                .unwrap_or(messages.len()),
        };
        let kept = branch::cut(&messages, keep);
        if kept.is_empty() {
            return Err(BranchError::Empty);
        }
        let provider = provider().map_err(BranchError::Provider)?;
        let cwd = request
            .cwd
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| parent.cwd.clone());
        let made = self
            .runtime
            .create_session(&provider, &parent.model, None, std::path::Path::new(&cwd))
            .map_err(BranchError::Failed)?;
        store
            .replace_messages(&made.id, &kept)
            .map_err(BranchError::Failed)?;
        // The branch carries the parent's untrusted text, so its taint too (Phase 6).
        self.runtime.copy_taint(&parent_id, &made.id);
        let taken: Vec<String> = store
            .list_sessions(1000)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.title)
            .collect();
        let title = request
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| branch::title(parent.title.as_deref(), &taken));
        let _ = store.set_title(&made.id, &title);
        self.branches.link(&made.id, &parent_id);
        if let (Some(key), Ok(mut keys)) = (key, self.branch_keys.lock()) {
            keys.insert(key, made.id.clone());
        }
        Ok(Branch {
            child: made.id,
            kept,
            parent,
        })
    }

    /// Moves an idle session to another folder (already resolved): the stored cwd and git
    /// identity move together, and the live state is dropped so the next turn's tools and shell
    /// start there. Returns `(cwd, branch, repo root)`.
    pub async fn move_session(
        &self,
        id: &str,
        folder: PathBuf,
    ) -> std::result::Result<(String, Option<String>, Option<String>), MoveError> {
        if self.runtime.is_running(id) {
            return Err(MoveError::Busy);
        }
        let runtime = self.runtime.clone();
        let workspaces = self.workspaces.clone();
        let moved = id.to_string();
        let result = tokio::task::spawn_blocking(move || {
            workspace::move_session(runtime.store(), &workspaces, &moved, &folder)
        })
        .await
        .map_err(|e| MoveError::Stopped(e.to_string()))??;
        if let Err(e) = self.runtime.evict(id).await {
            tracing::warn!("dropping the moved session's live state: {e:#}");
        }
        Ok(result)
    }

    /// Stops the session's turn and drops its open questions (their cards close, an approval is
    /// denied). Returns whether a turn was running.
    pub fn stop(&self, id: &str) -> bool {
        let running = self.runtime.is_running(id);
        self.runtime.cancel(id);
        self.questions.cancel_session(id);
        running
    }

    /// Drops the typed message at stored `row` and everything after it (`/undo`, `/retry`,
    /// restore). `Ok(None)` when there is no message there. Blocking.
    pub fn rewind_to_row(
        &self,
        id: &str,
        row: usize,
    ) -> std::result::Result<Option<Rewound>, RewindError> {
        if self.runtime.is_running(id) {
            return Err(RewindError::Busy);
        }
        let before = self.runtime.messages(id).map_err(RewindError::Failed)?;
        let keep = transcript::kept_before_row(row);
        let Some(target) = before.get(keep) else {
            return Ok(None);
        };
        // What the user typed: without the skills and memory the index attached to it.
        let typed = hermes_runtime::preinject::user_text(target);
        let kept = self.runtime.rewind(id, keep).map_err(RewindError::Failed)?;
        Ok(Some(Rewound {
            removed: before.len() - kept.len(),
            typed,
        }))
    }
}

#[cfg(test)]
mod tests;
