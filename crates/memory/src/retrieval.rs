//! What the local index, the Star map and the curator read from `memory.db` beyond the prompt
//! (Backend v2 Phase 9, CP9.3). Not from Hermes: see `PORTING.md`, `diverged`.
//!
//! - **The retrievable rows** ([`MemoryStore::tier_index_rows`]): every live `project` and
//!   `archive` row of every repository, with the scope it belongs to. `hermes_tools::index` puts
//!   them in `index.db` with that scope and filters on it in SQL, so a repository's project memory
//!   is only ever read for a session in that repository. Rows the agent removed or rewrote
//!   (`removed` set, or `superseded_by`) are not returned, and rows the user deleted are gone.
//! - **A generation** ([`MemoryStore::tier_generation`]): a counter in `meta`, bumped by every
//!   change to a non-`core` row, by any process. The index compares it with the one it indexed, so
//!   a write made by another process (the ACP binary, the GPUI app) is seen too.
//! - **Hits** ([`MemoryStore::record_hits`]): an entry that was put in front of the model gets
//!   `hits + 1` and a "last hit" time. The writes are **batched**: counted in memory and written in
//!   one transaction when 20 entries are waiting or a minute has passed since the last write (and
//!   when the store is dropped, and before the hygiene pass reads them), so a turn never costs a
//!   database write of its own. A crash loses at most a minute of hits.
//! - **Hygiene** ([`MemoryStore::hygiene_candidates`], [`MemoryStore::demote_core`]): the
//!   curator's deterministic pass proposes demoting a `core` entry nobody has touched for 60 days.
//!   The proposal is staged; this module only finds candidates and applies one when the user
//!   approves.
//!
//! `last_hit` lives in `meta` (`hit:<row id>`), not in a new column: the schema is unchanged, so an
//! older build still opens the file.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::Value;

use crate::db;
use crate::store::{MemoryStore, Operation, Target, TurnFailures};
use crate::tiers::{self, Extras};

/// A `core` entry untouched for this long is proposed for demotion.
pub const STALE_DAYS: i64 = 60;
/// A row proposed once is not proposed again for this long (the user may have said no).
const REPROPOSE_DAYS: i64 = 30;
/// Hits are written when this many distinct entries wait...
const FLUSH_ENTRIES: usize = 20;
/// ...or this long after the last write.
const FLUSH_EVERY: Duration = Duration::from_secs(60);
/// The tag that keeps a `core` entry out of the hygiene pass (the "pinned" entry).
pub const PINNED_TAG: &str = "pinned";

/// A live `project` or `archive` row as the index stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexRow {
    pub id: i64,
    /// `project` or `archive`.
    pub tier: String,
    /// `project:<key>` for a project entry (the same key as `hermes_tools::project_skills::
    /// memory_scope_key`), `""` for the archive.
    pub scope: String,
    /// `user` or `fact`.
    pub kind: String,
    pub text: String,
    pub tags: Vec<String>,
    pub links: Vec<String>,
}

/// What ranking needs to know about a row now.
#[derive(Clone, Debug, PartialEq)]
pub struct RowStats {
    pub id: i64,
    pub created: i64,
    pub last_confirmed: i64,
    pub hits: i64,
    /// Seconds since the epoch of the last hit, when there was one.
    pub last_hit: Option<i64>,
    pub links: Vec<String>,
}

/// An entry that was put in front of the model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HitRef {
    /// A `project` or `archive` row.
    Row(i64),
    /// A `core` entry, by its text (the index doesn't know core rows' ids).
    Core { target: Target, text: String },
}

/// A `core` entry as the Star map and the hygiene pass see it.
#[derive(Clone, Debug, PartialEq)]
pub struct CoreRow {
    pub id: i64,
    pub text: String,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub created: i64,
    pub last_confirmed: i64,
    pub hits: i64,
    pub last_hit: Option<i64>,
}

/// A `core` entry the hygiene pass proposes to demote.
#[derive(Clone, Debug, PartialEq)]
pub struct StaleEntry {
    pub id: i64,
    pub target: Target,
    pub text: String,
    /// Seconds since the epoch of the last time it was confirmed or hit.
    pub last_active: i64,
}

/// An archive entry as the Star map's additive `archive` field lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct ArchiveRow {
    pub id: i64,
    pub kind: String,
    pub text: String,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub created: i64,
    pub hits: i64,
}

/// Hits waiting to be written.
#[derive(Default)]
pub(crate) struct HitBuffer {
    pending: HashMap<HitRef, u32>,
    last_flush: Option<Instant>,
}

fn list(column: &str) -> Vec<String> {
    serde_json::from_str(column).unwrap_or_default()
}

fn hit_key(id: i64) -> String {
    format!("hit:{id}")
}

fn propose_key(id: i64) -> String {
    format!("hygiene:proposed:{id}")
}

impl MemoryStore {
    /// The generation of the non-`core` rows (see the module doc); `None` when `memory.db` can't
    /// be used right now.
    pub fn tier_generation(&self) -> Option<String> {
        let mut inner = self.lock_inner();
        let db = inner.db(self.db_path(), false)?;
        Some(db::get_meta(db.conn(), db::TIERS_GENERATION).ok()?.unwrap_or_else(|| "0".into()))
    }

    /// Every live `project` and `archive` row, in id order, with the generation they are
    /// from (read in one transaction). `None` when `memory.db` can't be used.
    pub fn tier_index_rows(&self) -> Option<(String, Vec<IndexRow>)> {
        let mut inner = self.lock_inner();
        let db = inner.db(self.db_path(), false)?;
        let conn = db.conn();
        let generation = db::get_meta(conn, db::TIERS_GENERATION).ok()?.unwrap_or_else(|| "0".into());
        let mut statement = conn
            .prepare_cached(
                "SELECT id, tier, scope, kind, text, tags, links FROM entry
                 WHERE tier IN ('project', 'archive') AND removed IS NULL AND superseded_by IS NULL
                 ORDER BY id",
            )
            .ok()?;
        let rows = statement
            .query_map([], |r| {
                Ok(IndexRow {
                    id: r.get(0)?,
                    tier: r.get(1)?,
                    scope: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    tags: list(&r.get::<_, String>(5)?),
                    links: list(&r.get::<_, String>(6)?),
                })
            })
            .ok()?;
        Some((generation, rows.filter_map(Result::ok).collect()))
    }

    /// The current numbers of the live rows among `ids` (rows removed since are left out).
    pub fn row_stats(&self, ids: &[i64]) -> Vec<RowStats> {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return Vec::new();
        };
        let conn = db.conn();
        let (Ok(mut statement), Ok(mut hit)) = (
            conn.prepare_cached(
                "SELECT created, last_confirmed, hits, links FROM entry
                 WHERE id = ?1 AND removed IS NULL AND superseded_by IS NULL",
            ),
            conn.prepare_cached("SELECT value FROM meta WHERE key = ?1"),
        ) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|&id| {
                let (created, last_confirmed, hits, links) = statement
                    .query_row([id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?))
                    })
                    .ok()?;
                let last_hit = hit
                    .query_row([hit_key(id)], |r| r.get::<_, String>(0))
                    .ok()
                    .and_then(|v| v.parse().ok());
                Some(RowStats {
                    id,
                    created,
                    last_confirmed,
                    hits,
                    last_hit,
                    links: list(&links),
                })
            })
            .collect()
    }

    /// The live `core` row of `target` with this id (the review's `supersedes`): its text.
    pub fn core_entry_by_id(&self, target: Target, id: i64) -> Option<String> {
        self.core_rows(target).into_iter().find(|r| r.id == id).map(|r| r.text)
    }

    /// The live `core` rows of `target` with their bookkeeping, in file order. Empty when
    /// `memory.db` can't be used. (The file stays the truth for what is in the prompt: call
    /// [`MemoryStore::entries`] first when the rows must match it.)
    pub fn core_rows(&self, target: Target) -> Vec<CoreRow> {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return Vec::new();
        };
        let conn = db.conn();
        let Ok(mut statement) = conn.prepare_cached(
            "SELECT id, text, tags, links, created, last_confirmed, hits FROM entry
             WHERE tier = 'core' AND scope = '' AND kind = ?1 AND removed IS NULL
             ORDER BY position, id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([target.kind()], |r| {
            Ok(CoreRow {
                id: r.get(0)?,
                text: r.get(1)?,
                tags: list(&r.get::<_, String>(2)?),
                links: list(&r.get::<_, String>(3)?),
                created: r.get(4)?,
                last_confirmed: r.get(5)?,
                hits: r.get(6)?,
                last_hit: None,
            })
        }) else {
            return Vec::new();
        };
        let mut rows: Vec<CoreRow> = rows.filter_map(Result::ok).collect();
        for row in &mut rows {
            row.last_hit = db::get_meta(conn, &hit_key(row.id))
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok());
        }
        rows
    }

    /// The newest `limit` live archive entries (the Star map's additive `archive` list).
    pub fn archive_rows(&self, limit: usize) -> Vec<ArchiveRow> {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return Vec::new();
        };
        let Ok(mut statement) = db.conn().prepare_cached(
            "SELECT id, kind, text, tags, links, created, hits FROM entry
             WHERE tier = 'archive' AND removed IS NULL AND superseded_by IS NULL
             ORDER BY created DESC, id DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([limit as i64], |r| {
            Ok(ArchiveRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                text: r.get(2)?,
                tags: list(&r.get::<_, String>(3)?),
                links: list(&r.get::<_, String>(4)?),
                created: r.get(5)?,
                hits: r.get(6)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    /// The row that replaced row `id` (a patch, a replace, a review's `supersedes`), if it was.
    pub fn successor_of(&self, id: i64) -> Option<i64> {
        let mut inner = self.lock_inner();
        let db = inner.db(self.db_path(), false)?;
        db.conn()
            .query_row("SELECT superseded_by FROM entry WHERE id = ?1", [id], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .ok()
            .flatten()
    }

    /// Whether a live `project` or `archive` row has this id (a `memory:<id>` link's end).
    pub fn live_tier_row(&self, id: i64) -> bool {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return false;
        };
        db.conn()
            .prepare_cached(
                "SELECT 1 FROM entry WHERE id = ?1 AND tier IN ('project', 'archive')
                 AND removed IS NULL AND superseded_by IS NULL",
            )
            .and_then(|mut s| s.exists([id]))
            .unwrap_or(false)
    }

    // ---- hits ------------------------------------------------------------------------------

    /// Counts entries that were put in front of the model. Cheap: the write is deferred (see the
    /// module doc); this may write the batch when it is due, so call it off the async workers.
    pub fn record_hits(&self, hits: impl IntoIterator<Item = HitRef>) {
        let due = {
            let mut buffer = self.hits.lock();
            for hit in hits {
                *buffer.pending.entry(hit).or_insert(0) += 1;
            }
            if buffer.pending.is_empty() {
                false
            } else {
                let waiting = buffer.pending.len();
                let first = *buffer.last_flush.get_or_insert_with(Instant::now);
                waiting >= FLUSH_ENTRIES || first.elapsed() >= FLUSH_EVERY
            }
        };
        if due {
            self.flush_hits();
        }
    }

    /// Hits counted and not yet written (tests, status).
    pub fn pending_hits(&self) -> usize {
        self.hits.lock().pending.len()
    }

    /// Writes the waiting hits in one transaction. A failure keeps them for the next try.
    pub fn flush_hits(&self) {
        let pending: HashMap<HitRef, u32> = {
            let mut buffer = self.hits.lock();
            buffer.last_flush = Some(Instant::now());
            std::mem::take(&mut buffer.pending)
        };
        if pending.is_empty() {
            return;
        }
        let written = (|| -> Option<()> {
            let mut inner = self.lock_inner();
            let db = inner.db(self.db_path(), false)?;
            let tx = db.begin().ok()?;
            let now = chrono::Utc::now().timestamp();
            for (hit, count) in &pending {
                let id: Option<i64> = match hit {
                    HitRef::Row(id) => Some(*id),
                    HitRef::Core { target, text } => tx
                        .prepare_cached(
                            "SELECT id FROM entry WHERE tier = 'core' AND scope = '' AND kind = ?1
                             AND text = ?2 AND removed IS NULL LIMIT 1",
                        )
                        .ok()?
                        .query_row(params![target.kind(), text], |r| r.get(0))
                        .ok(),
                };
                let Some(id) = id else { continue };
                tx.prepare_cached("UPDATE entry SET hits = hits + ?1 WHERE id = ?2")
                    .ok()?
                    .execute(params![count, id])
                    .ok()?;
                db::set_meta(&tx, &hit_key(id), &now.to_string()).ok()?;
            }
            tx.commit().ok()
        })();
        if written.is_none() {
            // Put them back (the next flush retries); a failing disk must not lose or grow them
            // without bound: the map is keyed by entry.
            let mut buffer = self.hits.lock();
            for (hit, count) in pending {
                *buffer.pending.entry(hit).or_insert(0) += count;
            }
        }
    }

    // ---- hygiene ---------------------------------------------------------------------------

    /// `core` entries not confirmed or hit for [`STALE_DAYS`] days, oldest first, at most
    /// `limit`: the demotion candidates. Left out: entries tagged `pinned`, entries too long for
    /// the archive, entries proposed in the last 30 days, entries the file no longer holds.
    /// Zero model requests: a read of `memory.db`.
    pub fn hygiene_candidates(&self, now: i64, limit: usize) -> Vec<StaleEntry> {
        self.flush_hits();
        let cutoff = now - STALE_DAYS * 86_400;
        let repropose = now - REPROPOSE_DAYS * 86_400;
        let mut out = Vec::new();
        for target in [Target::Memory, Target::User] {
            if !self.enabled(target) {
                continue;
            }
            // The file wins: only what it still holds is a candidate.
            let live: Vec<String> = self.entries(target);
            for row in self.core_rows(target) {
                if !live.contains(&row.text)
                    || row.tags.iter().any(|t| t.eq_ignore_ascii_case(PINNED_TAG))
                    || row.text.chars().count() > tiers::ARCHIVE_ENTRY_CHARS
                {
                    continue;
                }
                let active = row.last_confirmed.max(row.created).max(row.last_hit.unwrap_or(0));
                if active > cutoff {
                    continue;
                }
                if self.proposed_after(row.id, repropose) {
                    continue;
                }
                out.push(StaleEntry { id: row.id, target, text: row.text, last_active: active });
            }
        }
        out.sort_by_key(|e| (e.last_active, e.id));
        out.truncate(limit);
        out
    }

    fn proposed_after(&self, id: i64, since: i64) -> bool {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return false;
        };
        db::get_meta(db.conn(), &propose_key(id))
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|at| at > since)
    }

    /// Remembers that these rows were proposed now (so a rejected proposal is not made again
    /// at the next run).
    pub fn mark_proposed(&self, ids: &[i64], now: i64) {
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return;
        };
        let Ok(tx) = db.begin() else { return };
        for id in ids {
            let _ = db::set_meta(&tx, &propose_key(*id), &now.to_string());
        }
        let _ = tx.commit();
    }

    /// Applies a demotion the user approved: `entry` (the exact text of a live `core` entry of
    /// `target`) moves to the archive, keeping its tags and links. The archive copy is written
    /// first, so a failure on the way never loses the entry; then the core entry is removed
    /// (limits, threat scan and staleness are checked by the same code as any write: an entry
    /// changed since it was proposed is "stale"). `Err(message, stale)`.
    pub fn demote_core(&self, target: Target, entry: &str) -> Result<String, (String, bool)> {
        let entry = entry.trim();
        if !self.entries(target).iter().any(|e| e.trim() == entry) {
            return Err((
                "That entry is no longer in the core memory (it changed or was removed since it was proposed).".into(),
                true,
            ));
        }
        let row = self.core_rows(target).into_iter().find(|r| r.text.trim() == entry);
        let extras = Extras {
            tier: Some("archive".into()),
            tags: row.as_ref().map(|r| r.tags.clone()).filter(|t| !t.is_empty()),
            links: row
                .as_ref()
                .map(|r| r.links.clone())
                .filter(|l| !l.is_empty()),
            ..Extras::default()
        };
        let add = Operation {
            action: "add".into(),
            content: Some(entry.to_string()),
            ..Operation::default()
        };
        let added = self.apply_ext(target, &[add], &[extras], &mut TurnFailures::default());
        if added["success"] != Value::Bool(true) {
            return Err((
                format!(
                    "Could not copy the entry to the archive: {}",
                    added["error"].as_str().unwrap_or("the archive refused it")
                ),
                false,
            ));
        }
        let remove = Operation {
            action: "remove".into(),
            old_text: Some(entry.to_string()),
            matched_entry: Some(entry.to_string()),
            ..Operation::default()
        };
        let removed = self.apply(target, &[remove], &mut TurnFailures::default());
        if removed["success"] != Value::Bool(true) {
            return Err((
                format!(
                    "The entry is in the archive now, but it could not be removed from the core memory: {}",
                    removed["error"].as_str().unwrap_or("the write failed")
                ),
                removed["stale"] == Value::Bool(true),
            ));
        }
        Ok(format!("Moved to the archive: {}", entry.chars().take(80).collect::<String>()))
    }
}

impl Drop for MemoryStore {
    fn drop(&mut self) {
        self.flush_hits();
    }
}

#[cfg(test)]
#[path = "retrieval_tests.rs"]
mod tests;
