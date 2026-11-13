//! `memory.db`: the SQLite file behind the memory store (Backend v2, Phase 9). One table of
//! entries with a tier (`core`, `project`, `archive`), a scope, a kind (`user` = USER.md,
//! `fact` = MEMORY.md), tags, links and bookkeeping, an FTS5 index over the text, and a `meta`
//! table (what each file looked like when it was last synced). This module is the SQL only; how
//! the database and `MEMORY.md` / `USER.md` are kept in step is `sync.rs`.
//!
//! Two ways an entry leaves the `core` view ([`Removal`]):
//! - **marked** (`Mark`): the agent removed or rewrote it (the tool, a review, an approved
//!   staged write) or the database disagreed with our own last export (`repair`). The row stays
//!   with `removed` set (and `superseded_by` pointing at its replacement when one is known); an
//!   entry that comes back gets its old row (and id) back.
//! - **purged** (`Purge`): the *user* removed it: Settings ▸ Memory ▸ reset ([`purge_kind`]), a
//!   delete or rewrite on the Star map (`MemoryStore::edit_entry`), or an edit of the file by
//!   hand. The row, the rows it superseded (its older versions) and their text in the FTS index
//!   are deleted; `secure_delete` zeroes the freed pages and the caller checkpoints the WAL, so
//!   the text does not linger in `memory.db` (the pre-migration backup copies still hold what the
//!   files held then: PITFALLS.md).
//!
//! CP9.1 fills only `core` entries (the files' entries, in the files' order). The other columns
//! exist so the later checkpoints need no schema change.
//!
//! Not from Hermes (Hermes keeps memory in the two files only): see `PORTING.md`, `diverged`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};

/// Bumped when the tables change. A file with a higher version was written by a newer build and
/// is neither read nor written (never downgraded).
pub(crate) const SCHEMA_VERSION: i64 = 1;

/// The `synced:<kind>` value that says "the file did not exist when it was last synced".
pub(crate) const ABSENT: &str = "absent";

/// How long a statement waits for another process's write lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE entry(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tier TEXT NOT NULL CHECK (tier IN ('core', 'project', 'archive')),
    scope TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL CHECK (kind IN ('user', 'fact')),
    text TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '[]',
    links TEXT NOT NULL DEFAULT '[]',
    source_session TEXT,
    created INTEGER NOT NULL,
    last_confirmed INTEGER NOT NULL,
    hits INTEGER NOT NULL DEFAULT 0,
    superseded_by INTEGER,
    position INTEGER NOT NULL DEFAULT 0,
    removed INTEGER,
    removed_why TEXT
);
CREATE INDEX entry_view ON entry(tier, scope, kind, removed, position);
CREATE INDEX entry_text ON entry(kind, text);
CREATE VIRTUAL TABLE entry_fts USING fts5(
    text, tags, content='entry', content_rowid='id', tokenize='porter unicode61'
);
CREATE TRIGGER entry_ai AFTER INSERT ON entry BEGIN
    INSERT INTO entry_fts(rowid, text, tags) VALUES (new.id, new.text, new.tags);
END;
CREATE TRIGGER entry_ad AFTER DELETE ON entry BEGIN
    INSERT INTO entry_fts(entry_fts, rowid, text, tags)
        VALUES ('delete', old.id, old.text, old.tags);
END;
CREATE TRIGGER entry_au AFTER UPDATE OF text, tags ON entry BEGIN
    INSERT INTO entry_fts(entry_fts, rowid, text, tags)
        VALUES ('delete', old.id, old.text, old.tags);
    INSERT INTO entry_fts(rowid, text, tags) VALUES (new.id, new.text, new.tags);
END;
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// The rows a file is the view of: live global `core` entries of one kind, in file order.
const LIVE_CORE: &str = "tier = 'core' AND scope = '' AND kind = ?1 AND removed IS NULL";

/// Why [`MemoryDb::open`] gave up.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// Written by a newer build (its schema version): left exactly as it is.
    Newer(i64),
    /// Not a database, or a damaged one (SQLite says so, or `quick_check` fails).
    Corrupt(String),
    /// Anything else (no permission, a folder in the way, locked past the busy timeout, a
    /// simulated crash in tests).
    Other(String),
    /// Another process held a lock (creating the tables, switching the journal mode): retried
    /// inside [`MemoryDb::open`] until the busy timeout, then reported; the store tries again at
    /// its next use instead of switching the database off.
    Busy(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Newer(version) => write!(
                f,
                "it was written by a newer version of the app (schema {version}, this build knows {SCHEMA_VERSION})"
            ),
            OpenError::Corrupt(why) => write!(f, "it is damaged ({why})"),
            OpenError::Other(why) | OpenError::Busy(why) => f.write_str(why),
        }
    }
}

fn classify(error: rusqlite::Error) -> OpenError {
    match error.sqlite_error_code() {
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => {
            OpenError::Corrupt(error.to_string())
        }
        _ if is_busy(&error) => OpenError::Busy(error.to_string()),
        _ => OpenError::Other(error.to_string()),
    }
}

/// Another connection holds a lock SQLite would not wait for (or waited past the timeout for):
/// try again later; nothing is wrong with the file.
pub(crate) fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub(crate) struct MemoryDb {
    conn: Connection,
}

/// `fact` for MEMORY.md, `user` for USER.md.
pub(crate) fn meta_synced(kind: &str) -> String {
    format!("synced:{kind}")
}

impl MemoryDb {
    /// Opens the file, creating it (and the tables) when it is new. A newer schema, a damaged
    /// file or any other error leaves the file as it was. `crash_after_create` is a test
    /// failpoint: stop right after the tables were created, as a crash there would.
    pub(crate) fn open(path: &Path, crash_after_create: bool) -> Result<Self, OpenError> {
        // Two processes starting at once: one creates the tables or switches the journal mode
        // while the other looks, and some of those steps answer "busy" without waiting.
        let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
        loop {
            match Self::try_open(path, crash_after_create) {
                Err(OpenError::Busy(why)) if std::time::Instant::now() >= deadline => {
                    // The caller tries again at its next use (nothing is wrong with the file).
                    return Err(OpenError::Busy(format!("locked by another process: {why}")));
                }
                Err(OpenError::Busy(_)) => std::thread::sleep(Duration::from_millis(25)),
                other => return other,
            }
        }
    }

    fn try_open(path: &Path, crash_after_create: bool) -> Result<Self, OpenError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OpenError::Other(e.to_string()))?;
        }
        let mut conn = Connection::open(path).map_err(classify)?;
        // The desktop app, `hermes-acp.exe` and the GPUI app may have it open at once.
        conn.busy_timeout(BUSY_TIMEOUT).map_err(classify)?;
        // Deleted text is overwritten, not just unlinked (a purge must erase it).
        conn.pragma_update(None, "secure_delete", true)
            .map_err(classify)?;
        // The first read: a file that is not a database fails here.
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(classify)?;
        if version > SCHEMA_VERSION {
            return Err(OpenError::Newer(version));
        }
        // WAL (readers never wait for the writer) is a property of the file: set once.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(classify)?;
        if !mode.eq_ignore_ascii_case("wal") {
            let _: String = conn
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .map_err(classify)?;
        }
        if version == 0 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(classify)?;
            // Another process may have created it while we waited for the lock.
            let version: i64 = tx
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(classify)?;
            if version > SCHEMA_VERSION {
                return Err(OpenError::Newer(version));
            }
            if version == 0 {
                let tables: i64 = tx
                    .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
                    .map_err(classify)?;
                if tables > 0 {
                    return Err(OpenError::Other(
                        "it holds tables but no schema version: not a memory database".into(),
                    ));
                }
                tx.execute_batch(SCHEMA).map_err(classify)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(classify)?;
            }
            tx.commit().map_err(classify)?;
            if crash_after_create {
                return Err(OpenError::Other(
                    "simulated crash after creating the database".into(),
                ));
            }
        } else {
            let check: String = conn
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .map_err(classify)?;
            if check != "ok" {
                return Err(OpenError::Corrupt(check));
            }
        }
        Ok(Self { conn })
    }

    /// A write transaction: takes the database's write lock now (waiting up to the busy
    /// timeout), so a read-modify-write is serialized with other processes.
    pub(crate) fn begin(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// After a purge: copies the WAL into the database and empties it, so the deleted text is
    /// not left in old WAL frames. Best effort: waits at most a moment for other processes'
    /// readers (they keep it until their next checkpoint otherwise).
    pub(crate) fn checkpoint(&self) {
        let _ = self.conn.busy_timeout(Duration::from_millis(250));
        if let Err(error) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0))
        {
            tracing::warn!("memory.db: checkpoint after a purge failed ({error})");
        }
        let _ = self.conn.busy_timeout(BUSY_TIMEOUT);
    }
}

/// How an entry leaves the view (module doc).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Removal<'a> {
    /// Keep the row, `removed` set, with this reason.
    Mark(&'a str),
    /// Delete it and its older versions (the user removed it).
    Purge,
}

/// What [`set_core`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub changed: bool,
    /// Rows were deleted: the caller checkpoints after its commit.
    pub purged: bool,
}

/// Deletes the rows `ids`, every removed row they superseded (recursively: the older versions
/// of the text), and every removed row of `kind` with the same text, and merges the FTS index so
/// their tokens leave it too.
///
/// Returns the note files (`notes.rs` slugs) the deleted rows linked to: the caller deletes them
/// once the transaction has committed.
fn purge_rows(tx: &Connection, kind: &str, ids: &[i64]) -> rusqlite::Result<Vec<String>> {
    let mut doomed: HashSet<i64> = HashSet::new();
    let mut queue: Vec<i64> = ids.to_vec();
    while let Some(id) = queue.pop() {
        if !doomed.insert(id) {
            continue;
        }
        let text: Option<String> = tx
            .prepare_cached("SELECT text FROM entry WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        let mut older = tx.prepare_cached(
            "SELECT id FROM entry WHERE removed IS NOT NULL
             AND (superseded_by = ?1 OR (kind = ?2 AND text = ?3))",
        )?;
        let found = older.query_map(params![id, kind, text], |r| r.get::<_, i64>(0))?;
        for older in found {
            queue.push(older?);
        }
    }
    let mut slugs = Vec::new();
    let mut delete = tx.prepare_cached("DELETE FROM entry WHERE id = ?1")?;
    for id in &doomed {
        let links: Option<String> = tx
            .prepare_cached("SELECT links FROM entry WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        slugs.extend(note_slugs(links.as_deref().unwrap_or("[]")));
        delete.execute([id])?;
    }
    // The 'delete' rows the trigger wrote only mask the old tokens; merging the segments drops
    // them from the index's pages (which `secure_delete` then zeroes).
    tx.execute("INSERT INTO entry_fts(entry_fts) VALUES('optimize')", [])?;
    slugs.sort();
    slugs.dedup();
    unreferenced(tx, slugs)
}

/// The note files a `links` column (a JSON array of strings) points at.
pub(crate) fn note_slugs(links: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(links)
        .unwrap_or_default()
        .iter()
        .filter_map(|link| crate::notes::slug_of_link(link))
        .collect()
}

/// Of `slugs`, the notes no row still links to (live or removed): a patched entry's successor
/// carries its predecessor's note links, so a purge of one version must not delete a note a
/// surviving version still shows.
fn unreferenced(tx: &Connection, slugs: Vec<String>) -> rusqlite::Result<Vec<String>> {
    let mut used = tx.prepare_cached("SELECT 1 FROM entry WHERE instr(links, ?1) > 0 LIMIT 1")?;
    let mut out = Vec::new();
    for slug in slugs {
        // The link as it is stored inside the JSON array, quotes included (slugs are [a-z0-9-]).
        let needle = format!("\"{}\"", crate::notes::link_for(&slug));
        if !used.exists([needle])? {
            out.push(slug);
        }
    }
    Ok(out)
}

/// Settings ▸ Memory ▸ reset: deletes every `core` row of `kind` (live or removed) and its FTS
/// text: what the page's confirmation names (MEMORY.md / USER.md). The `project` and `archive`
/// tiers are not in those files and are not touched (PITFALLS.md). Whether anything was
/// deleted. (`core` rows never link notes.)
pub(crate) fn purge_kind(tx: &Connection, kind: &str) -> rusqlite::Result<bool> {
    let deleted = tx.execute("DELETE FROM entry WHERE tier = 'core' AND kind = ?1", [kind])?;
    if deleted > 0 {
        tx.execute("INSERT INTO entry_fts(entry_fts) VALUES('optimize')", [])?;
    }
    Ok(deleted > 0)
}

pub(crate) fn get_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

pub(crate) fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

/// `(id, text)` of the live `core` entries of `kind`, in file order.
pub(crate) fn live_rows(conn: &Connection, kind: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT id, text FROM entry WHERE {LIVE_CORE} ORDER BY position, id"
    ))?;
    let rows = statement.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The texts of the live `core` entries of `kind`, in file order.
pub(crate) fn live_texts(conn: &Connection, kind: &str) -> rusqlite::Result<Vec<String>> {
    Ok(live_rows(conn, kind)?.into_iter().map(|(_, t)| t).collect())
}

/// Whether any `core` row of `kind`, live or removed, holds exactly `text` (the file is the view
/// of `core` only: an entry of the same text in another tier doesn't make it known).
pub(crate) fn knows(conn: &Connection, kind: &str, text: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT 1 FROM entry WHERE tier = 'core' AND kind = ?1 AND text = ?2 LIMIT 1",
    )?
        .exists(params![kind, text])
}

/// Makes the live `core` entries of `kind` exactly `texts`, in that order (`texts` has no
/// duplicates: it comes from `parse_entries`). A text already live keeps its row; a text that
/// was removed earlier gets its row back; anything else is a new row. Live rows whose text is
/// gone leave as `removal` says: marked removed (`supersedes`, old text → new text from a patch,
/// records the replacement in `superseded_by`) or purged.
///
/// `crash_mid_import` (a test failpoint) fails right after the first new row, inside the
/// caller's transaction, as a crash there would.
pub(crate) fn set_core(
    tx: &Connection,
    kind: &str,
    texts: &[String],
    removal: Removal<'_>,
    supersedes: &[(String, String)],
    now: i64,
    crash_mid_import: bool,
) -> rusqlite::Result<Changes> {
    let live = live_rows(tx, kind)?;
    let mut by_text: HashMap<&str, Vec<i64>> = HashMap::new();
    for (id, text) in live.iter().rev() {
        by_text.entry(text.as_str()).or_default().push(*id);
    }
    let mut assigned: Vec<Option<i64>> = texts
        .iter()
        .map(|text| by_text.get_mut(text.as_str()).and_then(Vec::pop))
        .collect();
    let kept: HashSet<i64> = assigned.iter().flatten().copied().collect();
    let mut changed = false;
    for (index, text) in texts.iter().enumerate() {
        if assigned[index].is_some() {
            continue;
        }
        let revived: Option<i64> = tx
            .prepare_cached(
                "SELECT id FROM entry WHERE tier = 'core' AND scope = '' AND kind = ?1
                 AND text = ?2 AND removed IS NOT NULL ORDER BY removed DESC, id DESC LIMIT 1",
            )?
            .query_row(params![kind, text], |r| r.get(0))
            .optional()?;
        let id = match revived {
            Some(id) => {
                tx.prepare_cached(
                    "UPDATE entry SET removed = NULL, removed_why = NULL, superseded_by = NULL,
                     last_confirmed = ?1, position = ?2 WHERE id = ?3",
                )?
                .execute(params![now, index as i64, id])?;
                id
            }
            None => {
                tx.prepare_cached(
                    "INSERT INTO entry(tier, scope, kind, text, created, last_confirmed, position)
                     VALUES ('core', '', ?1, ?2, ?3, ?3, ?4)",
                )?
                .execute(params![kind, text, now, index as i64])?;
                tx.last_insert_rowid()
            }
        };
        assigned[index] = Some(id);
        changed = true;
        if crash_mid_import {
            // A simulated crash in the middle of an import.
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    let mut purge: Vec<i64> = Vec::new();
    for (id, text) in &live {
        if kept.contains(id) {
            continue;
        }
        changed = true;
        let why = match removal {
            Removal::Purge => {
                purge.push(*id);
                continue;
            }
            Removal::Mark(why) => why,
        };
        let successor = supersedes
            .iter()
            .find(|(old, _)| old == text)
            .and_then(|(_, new)| texts.iter().position(|t| t == new))
            .and_then(|index| assigned[index]);
        tx.prepare_cached(
            "UPDATE entry SET removed = ?1, removed_why = ?2, superseded_by = ?3 WHERE id = ?4",
        )?
        .execute(params![now, why, successor, id])?;
    }
    if !purge.is_empty() {
        // `core` entries have no note files.
        purge_rows(tx, kind, &purge)?;
    }
    for (index, id) in assigned.iter().enumerate() {
        let moved = tx
            .prepare_cached("UPDATE entry SET position = ?1 WHERE id = ?2 AND position <> ?1")?
            .execute(params![index as i64, id])?;
        changed |= moved > 0;
    }
    Ok(Changes {
        changed,
        purged: !purge.is_empty(),
    })
}

/// A live row of the `project` or `archive` tier.
#[derive(Clone, Debug)]
pub(crate) struct TierRow {
    pub id: i64,
    pub kind: String,
    pub text: String,
    /// JSON array of strings.
    pub tags: String,
    /// JSON array of strings.
    pub links: String,
    pub created: i64,
    pub hits: i64,
}

/// The live rows of `tier` in `scope` (`''` for `archive`), of one `kind` or of both (`None`:
/// `fact` entries first), in their order.
pub(crate) fn tier_rows(
    conn: &Connection,
    tier: &str,
    scope: &str,
    kind: Option<&str>,
) -> rusqlite::Result<Vec<TierRow>> {
    let mut statement = conn.prepare_cached(
        "SELECT id, kind, text, tags, links, created, hits FROM entry
         WHERE tier = ?1 AND scope = ?2 AND removed IS NULL AND (?3 IS NULL OR kind = ?3)
         ORDER BY kind, position, id",
    )?;
    let rows = statement.query_map(params![tier, scope, kind], |r| {
        Ok(TierRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            text: r.get(2)?,
            tags: r.get(3)?,
            links: r.get(4)?,
            created: r.get(5)?,
            hits: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// A new row of a non-`core` tier; its id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_tier_row(
    tx: &Connection,
    tier: &str,
    scope: &str,
    kind: &str,
    text: &str,
    tags: &str,
    links: &str,
    created: i64,
    hits: i64,
    now: i64,
    position: usize,
) -> rusqlite::Result<i64> {
    tx.prepare_cached(
        "INSERT INTO entry(tier, scope, kind, text, tags, links, created, last_confirmed, hits, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?
    .execute(params![
        tier,
        scope,
        kind,
        text,
        tags,
        links,
        created,
        now,
        hits,
        position as i64
    ])?;
    Ok(tx.last_insert_rowid())
}

pub(crate) fn set_position(tx: &Connection, id: i64, position: usize) -> rusqlite::Result<()> {
    tx.prepare_cached("UPDATE entry SET position = ?1 WHERE id = ?2 AND position <> ?1")?
        .execute(params![position as i64, id])?;
    Ok(())
}

pub(crate) fn set_row_meta(tx: &Connection, id: i64, tags: &str, links: &str) -> rusqlite::Result<()> {
    tx.prepare_cached("UPDATE entry SET tags = ?1, links = ?2 WHERE id = ?3")?
        .execute(params![tags, links, id])?;
    Ok(())
}

/// The agent removed (or rewrote) a row: it stays, marked, pointing at its replacement.
pub(crate) fn mark_removed(
    tx: &Connection,
    id: i64,
    now: i64,
    why: &str,
    successor: Option<i64>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE entry SET removed = ?1, removed_why = ?2, superseded_by = ?3 WHERE id = ?4",
    )?
    .execute(params![now, why, successor, id])?;
    Ok(())
}

/// Deletes one live row of a non-`core` tier and its older versions: the user removed it. The
/// note files they linked to are returned for the caller to delete after the commit.
pub(crate) fn purge_entry(tx: &Connection, kind: &str, id: i64) -> rusqlite::Result<Vec<String>> {
    purge_rows(tx, kind, &[id])
}

/// Sets the tags and links of the live `core` row holding `text` (what the agent's add or patch
/// said; `None` leaves a column as it is).
pub(crate) fn set_core_meta(
    tx: &Connection,
    kind: &str,
    text: &str,
    tags: Option<&str>,
    links: Option<&str>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE entry SET tags = COALESCE(?3, tags), links = COALESCE(?4, links)
         WHERE tier = 'core' AND scope = '' AND kind = ?1 AND text = ?2 AND removed IS NULL",
    )?
    .execute(params![kind, text, tags, links])?;
    Ok(())
}

/// After a patch: a live `core` row that has no tags or links of its own takes those of the row
/// it replaced.
pub(crate) fn inherit_core_meta(tx: &Connection, kind: &str) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE entry SET
           tags = (SELECT o.tags FROM entry o WHERE o.superseded_by = entry.id ORDER BY o.id DESC LIMIT 1),
           links = (SELECT o.links FROM entry o WHERE o.superseded_by = entry.id ORDER BY o.id DESC LIMIT 1)
         WHERE tier = 'core' AND scope = '' AND kind = ?1 AND removed IS NULL
           AND tags = '[]' AND links = '[]'
           AND EXISTS (SELECT 1 FROM entry o WHERE o.superseded_by = entry.id)",
    )?
    .execute([kind])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn open() -> (tempfile::TempDir, MemoryDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("memory.db"), false).unwrap();
        (dir, db)
    }

    fn set(db: &mut MemoryDb, kind: &str, items: &[&str], supersedes: &[(&str, &str)]) -> bool {
        let tx = db.begin().unwrap();
        let supersedes: Vec<(String, String)> = supersedes
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let changed = set_core(&tx, kind, &texts(items), Removal::Mark("test"), &supersedes, 1, false)
            .unwrap()
            .changed;
        tx.commit().unwrap();
        changed
    }

    fn purge(db: &mut MemoryDb, kind: &str, items: &[&str]) -> Changes {
        let tx = db.begin().unwrap();
        let changes = set_core(&tx, kind, &texts(items), Removal::Purge, &[], 2, false).unwrap();
        tx.commit().unwrap();
        db.checkpoint();
        changes
    }

    /// Whether `needle` occurs anywhere in the database's files (main file and WAL).
    fn on_disk(dir: &Path, needle: &str) -> bool {
        ["memory.db", "memory.db-wal"].iter().any(|name| {
            std::fs::read(dir.join(name)).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            })
        })
    }

    #[test]
    fn a_purge_erases_the_text_and_its_older_versions_from_disk() {
        let (dir, mut db) = open();
        set(&mut db, "user", &["password is hunter2-alpha", "Likes tea"], &[]);
        // The agent patched it: the old version is a removed row pointing at the new one.
        set(
            &mut db,
            "user",
            &["password is hunter2-bravo", "Likes tea"],
            &[("password is hunter2-alpha", "password is hunter2-bravo")],
        );
        db.checkpoint();
        assert!(on_disk(dir.path(), "hunter2-alpha"));
        // The user deletes it: both versions go, the other entry stays.
        let changes = purge(&mut db, "user", &["Likes tea"]);
        assert_eq!(
            changes,
            Changes {
                changed: true,
                purged: true
            }
        );
        assert!(!knows(db.conn(), "user", "password is hunter2-alpha").unwrap());
        assert!(!knows(db.conn(), "user", "password is hunter2-bravo").unwrap());
        assert_eq!(live_texts(db.conn(), "user").unwrap(), ["Likes tea"]);
        let fts: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM entry_fts WHERE entry_fts MATCH 'hunter2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0);
        drop(db);
        assert!(!on_disk(dir.path(), "hunter2"), "the text is still in the file");
    }

    #[test]
    fn purge_kind_deletes_one_kind_only() {
        let (dir, mut db) = open();
        set(&mut db, "user", &["secret-user-text"], &[]);
        set(&mut db, "user", &["other"], &[]);
        set(&mut db, "fact", &["kept-fact"], &[]);
        let tx = db.begin().unwrap();
        assert!(purge_kind(&tx, "user").unwrap());
        tx.commit().unwrap();
        db.checkpoint();
        assert!(!knows(db.conn(), "user", "secret-user-text").unwrap());
        assert!(!knows(db.conn(), "user", "other").unwrap());
        assert_eq!(live_texts(db.conn(), "fact").unwrap(), ["kept-fact"]);
        drop(db);
        assert!(!on_disk(dir.path(), "secret-user-text"));
    }

    #[test]
    fn the_schema_has_the_planned_columns_and_reopens() {
        let (dir, mut db) = open();
        let columns: Vec<String> = db
            .conn
            .prepare("SELECT name FROM pragma_table_info('entry')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            columns,
            [
                "id",
                "tier",
                "scope",
                "kind",
                "text",
                "tags",
                "links",
                "source_session",
                "created",
                "last_confirmed",
                "hits",
                "superseded_by",
                "position",
                "removed",
                "removed_why"
            ]
        );
        assert!(set(&mut db, "fact", &["one"], &[]));
        drop(db);
        let db = MemoryDb::open(&dir.path().join("memory.db"), false).unwrap();
        assert_eq!(live_texts(db.conn(), "fact").unwrap(), ["one"]);
        let version: i64 = db
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_schema_is_refused_and_left_alone() {
        let (dir, db) = open();
        db.conn.pragma_update(None, "user_version", 99).unwrap();
        drop(db);
        let before = std::fs::read(dir.path().join("memory.db")).unwrap();
        let error = MemoryDb::open(&dir.path().join("memory.db"), false)
            .err()
            .unwrap();
        assert!(matches!(error, OpenError::Newer(99)), "{error}");
        assert_eq!(std::fs::read(dir.path().join("memory.db")).unwrap(), before);
    }

    #[test]
    fn garbage_is_reported_as_corrupt_and_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let garbage = b"this is not a database, it is a note someone saved here".repeat(100);
        std::fs::write(&path, &garbage).unwrap();
        let error = MemoryDb::open(&path, false).err().unwrap();
        assert!(matches!(error, OpenError::Corrupt(_)), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), garbage);
    }

    #[test]
    fn a_foreign_database_is_not_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE theirs(x); INSERT INTO theirs VALUES (1);")
            .unwrap();
        let error = MemoryDb::open(&path, false).err().unwrap();
        assert!(matches!(error, OpenError::Other(_)), "{error}");
        let n: i64 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM theirs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn rows_keep_their_id_and_removed_rows_stay() {
        let (_dir, mut db) = open();
        set(&mut db, "fact", &["a", "b", "c"], &[]);
        let before = live_rows(db.conn(), "fact").unwrap();
        // Reordered, one dropped, one new: the survivors keep their ids.
        set(&mut db, "fact", &["c", "new", "a"], &[]);
        let after = live_rows(db.conn(), "fact").unwrap();
        assert_eq!(after[0], before[2]);
        assert_eq!(after[2], before[0]);
        assert!(before.iter().all(|(id, _)| *id != after[1].0));
        // "b" is not live, but its row and text are still there.
        assert!(knows(db.conn(), "fact", "b").unwrap());
        let (removed, why): (Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT removed, removed_why FROM entry WHERE id = ?1",
                [before[1].0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((removed, why.as_deref()), (Some(1), Some("test")));
        // It comes back with its old id.
        set(&mut db, "fact", &["c", "new", "a", "b"], &[]);
        assert_eq!(live_rows(db.conn(), "fact").unwrap()[3], before[1]);
        // No change, no write.
        assert!(!set(&mut db, "fact", &["c", "new", "a", "b"], &[]));
        // The other kind is its own list.
        assert!(live_texts(db.conn(), "user").unwrap().is_empty());
    }

    #[test]
    fn a_patch_records_its_replacement() {
        let (_dir, mut db) = open();
        set(&mut db, "user", &["Prefers Python"], &[]);
        let old = live_rows(db.conn(), "user").unwrap()[0].0;
        set(
            &mut db,
            "user",
            &["Prefers C++"],
            &[("Prefers Python", "Prefers C++")],
        );
        let new = live_rows(db.conn(), "user").unwrap()[0].0;
        let successor: Option<i64> = db
            .conn
            .query_row(
                "SELECT superseded_by FROM entry WHERE id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(successor, Some(new));
    }

    #[test]
    fn a_crash_mid_import_leaves_nothing_behind() {
        let (_dir, mut db) = open();
        set(&mut db, "fact", &["a"], &[]);
        {
            let tx = db.begin().unwrap();
            assert!(
                set_core(&tx, "fact", &texts(&["a", "b", "c"]), Removal::Mark("t"), &[], 2, true)
                    .is_err()
            );
            // Dropped without commit: rolled back, as after a crash.
        }
        assert_eq!(live_texts(db.conn(), "fact").unwrap(), ["a"]);
        assert!(!knows(db.conn(), "fact", "b").unwrap());
    }

    #[test]
    fn the_text_index_follows_inserts_and_edits() {
        let (_dir, mut db) = open();
        set(
            &mut db,
            "fact",
            &["The repo builds with cargo", "Deploys run on Fridays"],
            &[],
        );
        let search = |db: &MemoryDb, q: &str| -> Vec<i64> {
            let mut s = db
                .conn
                .prepare(
                    "SELECT e.id FROM entry_fts f JOIN entry e ON e.id = f.rowid
                     WHERE entry_fts MATCH ?1 AND e.removed IS NULL ORDER BY e.id",
                )
                .unwrap();
            s.query_map([q], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let rows = live_rows(db.conn(), "fact").unwrap();
        assert_eq!(search(&db, "cargo"), [rows[0].0]);
        // `porter`: "deploy" finds "Deploys".
        assert_eq!(search(&db, "deploy"), [rows[1].0]);
        set(&mut db, "fact", &["Deploys run on Fridays"], &[]);
        assert!(search(&db, "cargo").is_empty());
    }
}
