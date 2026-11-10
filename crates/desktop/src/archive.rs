//! Archived sessions: hidden from the sidebar and the project tree, kept (messages, title and
//! all), listed in Settings ▸ Sessions to restore or delete, and still found by search. The
//! sidebar's Archive, the keyboard shortcut and Settings' Unarchive all land here, and so does
//! the opt-in auto-archive (Settings ▸ Sessions: `sessions.auto_archive`, idle days in
//! `sessions.auto_archive_days`), which hides unpinned sessions idle that long, at most once a
//! day; a session it hid comes back when it's used again.
//!
//! The store's schema (shared with the GPUI app) has no archived column, so the set lives in the
//! data folder's `archived-sessions.json`.
//!
//! Hermes: `hermes_cli/web_routers/sessions.py` (`archived=exclude|only|include`, PATCH
//! `archived`), `hermes_state_maintenance.py` (`maybe_auto_archive`, `archive_stale_sessions`:
//! pinned sessions exempt, `min_interval_hours` 24, sweep provenance un-hidden on reuse),
//! `hermes_cli/config_defaults.py` (`sessions.auto_archive` false, `auto_archive_days` 3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const FILE: &str = "archived-sessions.json";

/// Hermes's `min_interval_hours`: auto-archive sweeps at most this often.
const SWEEP_INTERVAL_MS: i64 = 24 * 3600 * 1000;

/// Hermes's `auto_archive_days` default.
pub const DEFAULT_IDLE_DAYS: f64 = 3.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// When it was archived (ms since the epoch).
    at: i64,
    /// Archived by the idle sweep, not by the user: using it again brings it back.
    #[serde(default)]
    auto: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    sessions: BTreeMap<String, Entry>,
    /// When auto-archive last swept (ms since the epoch).
    #[serde(default)]
    last_sweep: Option<i64>,
}

/// A session the sweep may consider: id, last activity (ms), pinned, running a turn.
pub struct Candidate<'a> {
    pub id: &'a str,
    pub last_active: i64,
    pub pinned: bool,
    pub running: bool,
}

pub struct Archive {
    path: PathBuf,
    state: Mutex<State>,
}

impl Archive {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let state = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, state: &State) {
        match serde_json::to_vec_pretty(state) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&self.path, bytes) {
                    tracing::warn!("saving {}: {error}", self.path.display());
                }
            }
            Err(error) => tracing::warn!("archived sessions: {error}"),
        }
    }

    pub fn is_archived(&self, id: &str) -> bool {
        self.state().sessions.contains_key(id)
    }

    /// Archives or restores a session by the user's hand. `true` when that changed anything.
    pub fn set(&self, id: &str, archived: bool, now: i64) -> bool {
        let mut state = self.state();
        let changed = if archived {
            state
                .sessions
                .insert(
                    id.to_string(),
                    Entry {
                        at: now,
                        auto: false,
                    },
                )
                .is_none()
        } else {
            state.sessions.remove(id).is_some()
        };
        if changed {
            self.save(&state);
        }
        changed
    }

    /// A deleted session leaves the set.
    pub fn forget(&self, id: &str) {
        let mut state = self.state();
        if state.sessions.remove(id).is_some() {
            self.save(&state);
        }
    }

    /// A session is used again: one the idle sweep hid comes back (`true`); one the user
    /// archived stays archived.
    pub fn reopened(&self, id: &str) -> bool {
        let mut state = self.state();
        if state.sessions.get(id).is_some_and(|entry| entry.auto) {
            state.sessions.remove(id);
            self.save(&state);
            return true;
        }
        false
    }

    /// The auto-archive sweep: unpinned, idle sessions not running a turn are archived, at most
    /// once per [`SWEEP_INTERVAL_MS`]. Returns the ids it archived.
    pub fn sweep(&self, candidates: &[Candidate<'_>], idle_days: f64, now: i64) -> Vec<String> {
        let mut state = self.state();
        if state
            .last_sweep
            .is_some_and(|last| now - last < SWEEP_INTERVAL_MS)
            || idle_days < 0.0
        {
            return Vec::new();
        }
        let cutoff = now - (idle_days * 86_400_000.0) as i64;
        let mut archived = Vec::new();
        for candidate in candidates {
            if candidate.pinned
                || candidate.running
                || candidate.last_active >= cutoff
                || state.sessions.contains_key(candidate.id)
            {
                continue;
            }
            state.sessions.insert(
                candidate.id.to_string(),
                Entry {
                    at: now,
                    auto: true,
                },
            );
            archived.push(candidate.id.to_string());
        }
        // Recorded even when nothing went, so the sweep doesn't repeat until the interval passes.
        state.last_sweep = Some(now);
        self.save(&state);
        archived
    }
}

/// How a session list treats archived sessions (`archived=` on `/api/profiles/sessions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Exclude,
    Only,
    Include,
}

impl Filter {
    /// The query value; anything unknown is Hermes's default, `exclude`.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("only") => Filter::Only,
            Some("include") => Filter::Include,
            _ => Filter::Exclude,
        }
    }

    pub fn keeps(self, archived: bool) -> bool {
        match self {
            Filter::Exclude => !archived,
            Filter::Only => archived,
            Filter::Include => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn candidate(id: &str, last_active: i64) -> Candidate<'_> {
        Candidate {
            id,
            last_active,
            pinned: false,
            running: false,
        }
    }

    #[test]
    fn archiving_survives_a_restart_and_deleting_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::load(dir.path());
        assert!(archive.set("a", true, 1));
        assert!(
            !archive.set("a", true, 2),
            "archiving twice changes nothing"
        );
        assert!(Archive::load(dir.path()).is_archived("a"));
        assert!(archive.set("a", false, 3));
        assert!(!Archive::load(dir.path()).is_archived("a"));
        archive.set("b", true, 4);
        archive.forget("b");
        assert!(!Archive::load(dir.path()).is_archived("b"));
    }

    #[test]
    fn the_sweep_hides_idle_unpinned_sessions_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::load(dir.path());
        let now = 100 * DAY;
        let mut pinned = candidate("pinned", now - 10 * DAY);
        pinned.pinned = true;
        let mut running = candidate("running", now - 10 * DAY);
        running.running = true;
        let all = [
            candidate("old", now - 4 * DAY),
            candidate("recent", now - DAY),
            pinned,
            running,
        ];
        assert_eq!(archive.sweep(&all, 3.0, now), ["old"]);
        assert!(archive.is_archived("old") && !archive.is_archived("recent"));
        // Within the day nothing more happens, even for a session now idle long enough.
        assert!(
            archive
                .sweep(&[candidate("recent", now - 5 * DAY)], 3.0, now + DAY / 2)
                .is_empty()
        );
        assert_eq!(
            archive.sweep(&[candidate("recent", now - 5 * DAY)], 3.0, now + DAY + 1),
            ["recent"]
        );
    }

    #[test]
    fn a_swept_session_comes_back_when_used_but_a_users_archive_stays() {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::load(dir.path());
        archive.sweep(&[candidate("idle", 0)], 3.0, 10 * DAY);
        archive.set("mine", true, 10 * DAY);
        assert!(archive.reopened("idle") && !archive.is_archived("idle"));
        assert!(!archive.reopened("mine") && archive.is_archived("mine"));
        assert!(!archive.reopened("never"));
    }

    #[test]
    fn list_filters() {
        assert_eq!(Filter::parse(None), Filter::Exclude);
        assert_eq!(Filter::parse(Some("bogus")), Filter::Exclude);
        assert!(Filter::parse(Some("only")).keeps(true) && !Filter::Only.keeps(false));
        assert!(Filter::Include.keeps(true) && Filter::Include.keeps(false));
        assert!(!Filter::Exclude.keeps(true));
    }
}
