//! The local index: a SQLite FTS5 file (`data_dir/index.db`) over the skills and memory entries,
//! so the agent can be handed the skills and memories that matter for a message without asking
//! for them (Backend v2, Phase 5; the per-turn injection itself is CP5.2).
//!
//! **Ours, not ported from Hermes.** Hermes has no local index: it lists every skill in the system
//! prompt and the model loads them with `skill_view`. This replaces that with retrieval.
//!
//! - **Separate file.** `index.db`, never `sessions.db` (its schema is frozen). The index is only a
//!   cache: it can be deleted at any time and is rebuilt from the SKILL.md files and MEMORY.md /
//!   USER.md (a wrong schema version or a corrupt file is dropped and rebuilt the same way).
//! - **Tables.** `skill_doc(name, category, description, body, path, mtime, hash)` and
//!   `memory_doc(id, tier, scope, text, tags)`, both FTS5 with the `porter unicode61` tokenizer,
//!   ranked by BM25 (name and description weigh more than the body). `skill_files` is a plain side
//!   table (path, mtime, size, hash, FTS row, whether the copy is the one the catalog serves):
//!   FTS5 can't look a row up by path.
//! - **Incremental.** A refresh walks the skill folders with `stat` only; a file is re-read when its
//!   mtime or size changed (or its mtime is within the last 2 s, the "racy" window where a second
//!   write could keep the same mtime: such a read is stored with a "smudged" mtime so the next
//!   refresh reads it once more), and re-indexed only when its hash changed too. Files that went
//!   are dropped. Memory is a few KB: re-indexed whenever the file hash changes (a memory file that
//!   exists but can't be read right now is left as indexed).
//! - **Scoring.** Only the first 2 000 characters of a body are indexed (see `MAX_BODY_CHARS`), and
//!   a hit must pass an absolute score and 30% of the best hit's score. Tuned on the 37 bundled
//!   skills and checked on Hermes's ~230 (`goldens_hold_on_hermess_full_skill_set`).
//! - **Fresh.** [`LocalIndex::attach`] hooks the skill and memory stores: every write (`skill_manage`,
//!   the Skills screen, the curator, restore, import, `memory`) marks the index dirty and a
//!   background thread refreshes it (writes never wait). [`LocalIndex::query`] refreshes first when
//!   it is dirty, so it never answers from before a write. The runtime also marks it dirty at startup.
//! - **Threads.** Everything here is blocking SQLite: call it from `spawn_blocking` (the `*_async`
//!   wrappers do) or a plain thread, never from an async worker (ground rule 7).
//! - **Escaping.** User text never reaches `MATCH` as written: it is cut into lowercase
//!   alphanumeric tokens, stopwords dropped, and each token is sent as a quoted phrase, joined with
//!   `OR`. A quoted token can't be an operator (`NEAR`, `AND`, `*`, `:`, `-`, `(`).
//!
//! Disabled skills are indexed but left out of the results; disabled memory targets likewise.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use hermes_memory::{MemoryStore, Target};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::skills::{self, SkillStore};

/// Bump to throw the old file away (the schema or the way text is indexed changed).
const SCHEMA_VERSION: i64 = 2;
/// A file modified this recently is re-read even when its stat is unchanged (see the module doc).
const RACY_NANOS: i64 = 2_000_000_000;
/// The stored mtime of a file read inside the racy window: never equal to a real one, so the
/// next refresh reads it again (git's "smudged" index entry) however late that refresh comes.
const SMUDGED_MTIME: i64 = -1;
/// The most of a skill's body that is indexed. FTS5's BM25 normalizes by the length of the whole
/// row, so a long body would drown a match in the name or description (with 40 000 characters,
/// `github` fell out of the top 3 for "open a pull request on github" among Hermes's 230 skills);
/// the opening of a SKILL.md (overview, when to use) carries its topic.
const MAX_BODY_CHARS: usize = 2_000;
/// Query tokens taken from the user's message, the last assistant message and the project name.
const MESSAGE_TOKENS: usize = 32;
const ASSISTANT_TOKENS: usize = 12;
const PROJECT_TOKENS: usize = 4;
/// Text read from the user's message (a pasted log shouldn't cost anything).
const MESSAGE_CHARS: usize = 3_000;
/// The assistant's text used (the plan: its first 500 characters).
const ASSISTANT_CHARS: usize = 500;
const MAX_TOKEN_CHARS: usize = 32;

/// What a [`LocalIndex::refresh`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefreshReport {
    /// SKILL.md files found.
    pub files_seen: usize,
    /// Files whose content was read (stat changed or racy).
    pub files_read: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// MEMORY.md / USER.md were re-indexed.
    pub memory_rebuilt: bool,
}

/// What to look up: the user's message, the previous assistant message (its first 500 characters
/// are used) and the project name (see [`project_name`]).
#[derive(Debug, Default, Clone)]
pub struct IndexQuery {
    pub message: String,
    pub last_assistant: Option<String>,
    pub project: Option<String>,
}

/// How many hits, and how good they must be (a score is `-bm25`: higher is better).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Top-k skills (the `skills.preload` budget knob in CP5.2).
    pub skills: usize,
    pub memory: usize,
    pub min_skill_score: f64,
    pub min_memory_score: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            skills: 3,
            memory: 5,
            min_skill_score: DEFAULT_MIN_SKILL_SCORE,
            min_memory_score: DEFAULT_MIN_MEMORY_SCORE,
        }
    }
}

/// Tuned on the retrieval goldens (`tests::goldens`): a single weak word shouldn't pull a skill in.
pub const DEFAULT_MIN_SKILL_SCORE: f64 = 2.5;
pub const DEFAULT_MIN_MEMORY_SCORE: f64 = 1.0;
/// A hit must also reach this share of the best hit's score (the tail of an `OR` query is noise).
const MIN_RELATIVE_SCORE: f64 = 0.3;

#[derive(Debug, Clone, PartialEq)]
pub struct SkillHit {
    pub name: String,
    pub category: Option<String>,
    pub description: String,
    /// The skill's `SKILL.md`.
    pub path: PathBuf,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryHit {
    /// Stable for an entry's text: `<tier>:<hash>` (Phase 9 replaces it with real ids).
    pub id: String,
    /// `memory` or `user`.
    pub tier: String,
    pub text: String,
    pub score: f64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Retrieval {
    pub skills: Vec<SkillHit>,
    pub memory: Vec<MemoryHit>,
}

pub struct LocalIndex {
    conn: Mutex<Connection>,
    skills: Arc<SkillStore>,
    memory: Arc<MemoryStore>,
    /// Something may have changed since the last refresh started.
    dirty: AtomicBool,
    /// The background refresher thread is alive.
    worker: AtomicBool,
}

impl LocalIndex {
    /// Opens (creating it when needed) the index file. A file with another schema version, or
    /// one SQLite can't read, is deleted and started over: it's only a cache. Nothing is indexed
    /// yet: call [`Self::refresh`] or [`Self::notify_changed`].
    pub fn open(
        path: &Path,
        skills: Arc<SkillStore>,
        memory: Arc<MemoryStore>,
    ) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = match Self::open_conn(path) {
            Ok(conn) => conn,
            Err(first) => {
                tracing::warn!("index.db unusable ({first:#}); rebuilding it");
                for suffix in ["", "-wal", "-shm", "-journal"] {
                    let mut p = path.as_os_str().to_owned();
                    p.push(suffix);
                    let _ = std::fs::remove_file(PathBuf::from(p));
                }
                Self::open_conn(path).context("opening a fresh index.db")?
            }
        };
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            skills,
            memory,
            dirty: AtomicBool::new(true),
            worker: AtomicBool::new(false),
        }))
    }

    fn open_conn(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // A rebuild is one transaction; WAL + NORMAL keeps its cost to a single sync.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            conn.execute_batch(
                "DROP TABLE IF EXISTS skill_doc;
                 DROP TABLE IF EXISTS memory_doc;
                 DROP TABLE IF EXISTS skill_files;
                 DROP TABLE IF EXISTS meta;",
            )?;
        }
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS skill_doc USING fts5(
                 name, category, description, body,
                 path UNINDEXED, mtime UNINDEXED, hash UNINDEXED,
                 tokenize = 'porter unicode61');
             CREATE VIRTUAL TABLE IF NOT EXISTS memory_doc USING fts5(
                 id UNINDEXED, tier UNINDEXED, scope UNINDEXED, text, tags,
                 tokenize = 'porter unicode61');
             CREATE TABLE IF NOT EXISTS skill_files(
                 path TEXT PRIMARY KEY, doc INTEGER, mtime INTEGER NOT NULL, size INTEGER NOT NULL,
                 hash TEXT NOT NULL, name TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        // Prove the file reads back (a corrupt one fails here rather than mid-query).
        conn.query_row("SELECT count(*) FROM skill_files", [], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(conn)
    }

    /// Hooks the skill and memory stores so every write marks the index dirty (see the module doc).
    /// The hooks hold a weak reference: dropping the index is fine.
    pub fn attach(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(index) = weak.upgrade() {
                index.notify_changed();
            }
        });
        self.skills.set_on_change(hook.clone());
        self.memory.set_on_change(hook);
    }

    /// Marks the index dirty and refreshes it on a background thread (coalescing: a burst of
    /// writes is one or two refreshes). Never blocks the caller.
    pub fn notify_changed(self: &Arc<Self>) {
        self.dirty.store(true, Ordering::SeqCst);
        if self.worker.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        let spawned = std::thread::Builder::new()
            .name("hermes-index".into())
            .spawn(move || {
                loop {
                    let mut failed = false;
                    while this.dirty.load(Ordering::SeqCst) {
                        if let Err(e) = this.refresh() {
                            // `refresh` left `dirty` set: the next query or write retries.
                            // Looping here would spin on a lasting error (a full disk).
                            tracing::warn!("index refresh failed: {e:#}");
                            failed = true;
                            break;
                        }
                    }
                    this.worker.store(false, Ordering::SeqCst);
                    // A write may have landed between the last check and the store above.
                    if failed
                        || !this.dirty.load(Ordering::SeqCst)
                        || this.worker.swap(true, Ordering::SeqCst)
                    {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            self.worker.store(false, Ordering::SeqCst);
        }
    }

    /// Brings the index up to date with the skill folders and the memory files. Blocking.
    pub fn refresh(&self) -> Result<RefreshReport> {
        let mut conn = self.conn.lock();
        // Cleared once we hold the connection: a write after this point sets it again.
        self.dirty.store(false, Ordering::SeqCst);
        let result = (|| {
            let tx = conn.transaction()?;
            let mut report = self.refresh_skills(&tx)?;
            report.memory_rebuilt = self.refresh_memory(&tx)?;
            tx.commit()?;
            Ok(report)
        })();
        if result.is_err() {
            // Nothing was committed: still behind the files.
            self.dirty.store(true, Ordering::SeqCst);
        }
        result
    }

    pub async fn refresh_async(self: &Arc<Self>) -> Result<RefreshReport> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.refresh()).await?
    }

    fn refresh_skills(&self, tx: &rusqlite::Transaction<'_>) -> Result<RefreshReport> {
        struct Row {
            doc: Option<i64>,
            mtime: i64,
            size: i64,
            hash: String,
            name: String,
            active: bool,
        }
        let mut existing: HashMap<String, Row> = HashMap::new();
        {
            let mut stmt =
                tx.prepare("SELECT path, doc, mtime, size, hash, name, active FROM skill_files")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Row {
                        doc: r.get(1)?,
                        mtime: r.get(2)?,
                        size: r.get(3)?,
                        hash: r.get(4)?,
                        name: r.get(5)?,
                        active: r.get::<_, i64>(6)? != 0,
                    },
                ))
            })?;
            for row in rows {
                let (path, row) = row?;
                existing.insert(path, row);
            }
        }
        let mut report = RefreshReport::default();
        let now = nanos(SystemTime::now());
        // (path, name) of every indexed file in catalog priority order, and the current `active`.
        let mut order: Vec<(String, String)> = Vec::new();
        let mut active_now: HashMap<String, bool> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (root, external) in self.skills.catalog_roots() {
            for skill_md in skills::skill_files(&root) {
                let key = skill_md.to_string_lossy().to_string();
                if !seen.insert(key.clone()) {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(&skill_md) else {
                    seen.remove(&key);
                    continue;
                };
                report.files_seen += 1;
                let mtime = meta.modified().map_or(0, nanos);
                let size = meta.len() as i64;
                let row = existing.get(&key);
                let racy = now - mtime < RACY_NANOS;
                let stored_mtime = if racy { SMUDGED_MTIME } else { mtime };
                if let Some(r) = row.filter(|r| r.mtime == mtime && r.size == size && !racy) {
                    if r.doc.is_some() {
                        order.push((key.clone(), r.name.clone()));
                    }
                    active_now.insert(key, r.active);
                    continue;
                }
                let Ok(bytes) = std::fs::read(&skill_md) else {
                    // Locked or unreadable right now: keep what is indexed (re-read next time,
                    // since the stored stat no longer matches), or skip a file never indexed.
                    match row {
                        Some(r) => {
                            if r.doc.is_some() {
                                order.push((key.clone(), r.name.clone()));
                            }
                            active_now.insert(key, r.active);
                        }
                        None => {
                            seen.remove(&key);
                        }
                    }
                    continue;
                };
                report.files_read += 1;
                let digest = skills::hash(&bytes);
                if let Some(r) = row.filter(|r| r.hash == digest) {
                    tx.execute(
                        "UPDATE skill_files SET mtime = ?1, size = ?2 WHERE path = ?3",
                        params![stored_mtime, size, key],
                    )?;
                    if r.doc.is_some() {
                        order.push((key.clone(), r.name.clone()));
                    }
                    active_now.insert(key, r.active);
                    continue;
                }
                // New or changed content.
                if let Some(doc) = row.and_then(|r| r.doc) {
                    tx.execute("DELETE FROM skill_doc WHERE rowid = ?1", [doc])?;
                }
                if row.is_some() {
                    report.updated += 1;
                } else {
                    report.added += 1;
                }
                // The catalog skips a SKILL.md that isn't UTF-8 (`read_to_string`): so does the index.
                let entry = std::str::from_utf8(&bytes).ok().and_then(|text| {
                    Some((text, skills::skill_entry(&root, external, &skill_md, text)?))
                });
                let (doc, name) = match entry {
                    Some((text, entry)) => {
                        let body: String = skills::parse_frontmatter(text)
                            .1
                            .chars()
                            .take(MAX_BODY_CHARS)
                            .collect();
                        tx.execute(
                            "INSERT INTO skill_doc(name, category, description, body, path, mtime, hash)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                entry.name,
                                // As is: `/` is a separator to the tokenizer, so each folder is a word.
                                entry.category.clone().unwrap_or_default(),
                                entry.description,
                                body,
                                key,
                                mtime,
                                digest
                            ],
                        )?;
                        order.push((key.clone(), entry.name.clone()));
                        (Some(tx.last_insert_rowid()), entry.name)
                    }
                    // Not for Windows: remembered so it isn't re-read on every refresh.
                    None => (None, String::new()),
                };
                tx.execute(
                    "INSERT INTO skill_files(path, doc, mtime, size, hash, name, active)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
                     ON CONFLICT(path) DO UPDATE SET doc = ?2, mtime = ?3, size = ?4, hash = ?5,
                                                     name = ?6, active = 0",
                    params![key, doc, stored_mtime, size, digest, name],
                )?;
                active_now.insert(key, false);
            }
        }
        // Files that went away.
        for (path, row) in &existing {
            if !seen.contains(path) {
                if let Some(doc) = row.doc {
                    tx.execute("DELETE FROM skill_doc WHERE rowid = ?1", [doc])?;
                }
                tx.execute("DELETE FROM skill_files WHERE path = ?1", [path])?;
                report.removed += 1;
            }
        }
        // The first copy of a name (catalog order) is the one served.
        let mut names = HashSet::new();
        for (path, name) in &order {
            let want = names.insert(name.clone());
            if active_now.get(path).copied() != Some(want) {
                tx.execute(
                    "UPDATE skill_files SET active = ?1 WHERE path = ?2",
                    params![want as i64, path],
                )?;
            }
        }
        Ok(report)
    }

    /// Re-indexes MEMORY.md / USER.md when their content changed; whether anything was.
    fn refresh_memory(&self, tx: &rusqlite::Transaction<'_>) -> Result<bool> {
        let mut rebuilt = false;
        for target in [Target::Memory, Target::User] {
            // A file that exists but can't be read now (locked) keeps its indexed entries: an
            // empty view would drop them until the next memory write.
            let Some(entries) = self.memory.readable_entries(target) else {
                continue;
            };
            let digest = skills::hash(entries.join("\u{0}").as_bytes());
            let key = format!("memory:{}", target.name());
            let old: Option<String> = tx
                .query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| {
                    r.get(0)
                })
                .optional()?;
            if old.as_deref() == Some(digest.as_str()) {
                continue;
            }
            tx.execute("DELETE FROM memory_doc WHERE tier = ?1", [target.name()])?;
            let mut ids = HashSet::new();
            for entry in &entries {
                let id = format!("{}:{}", target.name(), &skills::hash(entry.as_bytes())[..8]);
                if !ids.insert(id.clone()) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO memory_doc(id, tier, scope, text, tags) VALUES (?1, ?2, 'global', ?3, '')",
                    params![id, target.name(), entry],
                )?;
            }
            tx.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, digest],
            )?;
            rebuilt = true;
        }
        Ok(rebuilt)
    }

    /// The best skills and memory entries for `query`. Blocking; refreshes first when dirty.
    /// Empty when the query has no usable words. A refresh that fails is logged and the query
    /// answers from the index as it is (better a stale hit than no context at all).
    pub fn query(&self, query: &IndexQuery, limits: &Limits) -> Result<Retrieval> {
        if self.dirty.load(Ordering::SeqCst)
            && let Err(e) = self.refresh()
        {
            tracing::warn!("index refresh before a query failed: {e:#}");
        }
        let tokens = query_tokens(query);
        let Some(expr) = match_expr(&tokens) else {
            return Ok(Retrieval::default());
        };
        let disabled = self.skills.config().disabled;
        let conn = self.conn.lock();
        let mut out = Retrieval::default();
        if limits.skills > 0 {
            // Weights: name, category, description, body (then the unindexed columns). The body
            // weighs little: a word that only occurs in a long text is weak evidence.
            let mut stmt = conn.prepare_cached(
                "SELECT name, category, description, path,
                        -bm25(skill_doc, 10.0, 2.0, 6.0, 0.3, 0.0, 0.0, 0.0)
                 FROM skill_doc WHERE skill_doc MATCH ?1
                 ORDER BY bm25(skill_doc, 10.0, 2.0, 6.0, 0.3, 0.0, 0.0, 0.0) LIMIT ?2",
            )?;
            let mut active =
                conn.prepare_cached("SELECT active FROM skill_files WHERE path = ?1")?;
            let rows = stmt.query_map(params![expr, (limits.skills * 4 + 8) as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })?;
            for row in rows {
                let (name, category, description, path, score) = row?;
                if score < limits.min_skill_score {
                    break;
                }
                if disabled.contains(&name) {
                    continue;
                }
                if !active
                    .query_row([&path], |r| r.get::<_, i64>(0))
                    .optional()?
                    .is_some_and(|a| a != 0)
                {
                    continue;
                }
                if out
                    .skills
                    .first()
                    .is_some_and(|best| score < best.score * MIN_RELATIVE_SCORE)
                {
                    break;
                }
                let category = (!category.is_empty()).then_some(category);
                out.skills.push(SkillHit {
                    name,
                    category,
                    description,
                    path: PathBuf::from(path),
                    score,
                });
                if out.skills.len() >= limits.skills {
                    break;
                }
            }
        }
        if limits.memory > 0 {
            let mut stmt = conn.prepare_cached(
                "SELECT id, tier, text, -bm25(memory_doc, 0.0, 0.0, 0.0, 1.0, 2.0)
                 FROM memory_doc WHERE memory_doc MATCH ?1
                 ORDER BY bm25(memory_doc, 0.0, 0.0, 0.0, 1.0, 2.0) LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![expr, (limits.memory * 3 + 6) as i64], |r| {
                Ok(MemoryHit {
                    id: r.get(0)?,
                    tier: r.get(1)?,
                    text: r.get(2)?,
                    score: r.get(3)?,
                })
            })?;
            for hit in rows {
                let hit = hit?;
                if hit.score < limits.min_memory_score {
                    break;
                }
                if out
                    .memory
                    .first()
                    .is_some_and(|best| hit.score < best.score * MIN_RELATIVE_SCORE)
                {
                    break;
                }
                if Target::parse(&hit.tier).is_some_and(|t| !self.memory.enabled(t)) {
                    continue;
                }
                out.memory.push(hit);
                if out.memory.len() >= limits.memory {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub async fn query_async(
        self: &Arc<Self>,
        query: IndexQuery,
        limits: Limits,
    ) -> Result<Retrieval> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.query(&query, &limits)).await?
    }

    /// Indexed skills (one per file; a name shadowed by an earlier copy is counted too).
    pub fn skill_count(&self) -> usize {
        self.count("skill_doc")
    }

    pub fn memory_count(&self) -> usize {
        self.count("memory_doc")
    }

    fn count(&self, table: &str) -> usize {
        self.conn
            .lock()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }
}

fn nanos(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

/// The project a cwd belongs to: the folder holding the nearest `.git` above it, else the cwd's
/// own folder name. Used as a query word (a skill or memory that names the project ranks up).
pub fn project_name(cwd: &Path) -> Option<String> {
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd);
    root.file_name().map(|n| n.to_string_lossy().to_string())
}

/// Words that carry no topic; dropping them keeps an `OR` query from matching everything.
const STOPWORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as",
    "at", "be", "because", "been", "before", "being", "below", "between", "both", "but", "by",
    "can", "could", "did", "do", "does", "doing", "done", "down", "during", "each", "few", "for",
    "from", "further", "get", "go", "had", "has", "have", "having", "he", "her", "here", "hers",
    "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "let", "like", "me",
    "more", "most", "my", "need", "no", "nor", "not", "now", "of", "off", "on", "once", "only",
    "or", "other", "our", "out", "over", "own", "please", "same", "she", "should", "so", "some",
    "such", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "those", "through", "to", "too", "under", "until", "up", "us", "use", "using", "very", "want",
    "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom", "why", "will",
    "with", "would", "you", "your",
];

/// Lowercase alphanumeric words of `text` (at most `max_tokens`, none already in `out`), read
/// from its first `max_chars` characters. Everything else (quotes, `*`, `:`, `-`, `(`, control
/// characters) is a separator, so nothing FTS5 treats as syntax survives.
fn push_tokens(text: &str, max_chars: usize, max_tokens: usize, out: &mut Vec<String>) {
    let start = out.len();
    let clipped: String = text.chars().take(max_chars).collect();
    for word in clipped.split(|c: char| !c.is_alphanumeric()) {
        if out.len() - start >= max_tokens {
            break;
        }
        let word: String = word
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(MAX_TOKEN_CHARS)
            .collect();
        // One-letter words are noise, except a lone non-ASCII character (CJK words are short).
        let keep = match word.chars().count() {
            0 => false,
            1 => !word.is_ascii(),
            _ => true,
        };
        if keep && !STOPWORDS.contains(&word.as_str()) && !out.contains(&word) {
            out.push(word);
        }
    }
}

fn query_tokens(query: &IndexQuery) -> Vec<String> {
    let mut tokens = Vec::new();
    push_tokens(&query.message, MESSAGE_CHARS, MESSAGE_TOKENS, &mut tokens);
    if let Some(text) = &query.last_assistant {
        push_tokens(text, ASSISTANT_CHARS, ASSISTANT_TOKENS, &mut tokens);
    }
    if let Some(project) = &query.project {
        push_tokens(project, 200, PROJECT_TOKENS, &mut tokens);
    }
    tokens
}

/// `"a" OR "b" OR ...`, or `None` when there is nothing to search for. Each token is a quoted
/// FTS5 string made of letters and digits only (see [`push_tokens`]), so it can't be syntax.
fn match_expr(tokens: &[String]) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    struct Fixture {
        _dir: tempfile::TempDir,
        skills: Arc<SkillStore>,
        memory: Arc<MemoryStore>,
        index: Arc<LocalIndex>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let skills = Arc::new(SkillStore::new(
            dir.path().join("skills"),
            Default::default(),
        ));
        let memory = Arc::new(MemoryStore::new(
            dir.path().join("memories"),
            Default::default(),
        ));
        let index =
            LocalIndex::open(&dir.path().join("index.db"), skills.clone(), memory.clone()).unwrap();
        Fixture {
            _dir: dir,
            skills,
            memory,
            index,
        }
    }

    /// A fixture holding the bundled skills (the files only: no git history).
    fn bundled() -> Fixture {
        let f = fixture();
        f.skills.seed_files();
        age_all(f.skills.root());
        f
    }

    fn write_skill(f: &Fixture, rel: &str, name: &str, description: &str, body: &str) -> PathBuf {
        let dir = f.skills.root().join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
        )
        .unwrap();
        path
    }

    /// Makes a file look old, so the racy-mtime rule doesn't re-read it.
    fn age(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    fn age_all(root: &Path) {
        for path in skills::skill_files(root) {
            age(&path);
        }
    }

    fn q(message: &str) -> IndexQuery {
        IndexQuery {
            message: message.into(),
            ..Default::default()
        }
    }

    // --- retrieval goldens on the bundled skills ---

    /// (message, a skill that must be first, or `None` for "no skill passes the threshold").
    pub(super) const GOLDENS: &[(&str, Option<&str>)] = &[
        // No git skill is bundled (the plan assumed one); "Pre-commit review" is the closest.
        ("write a git commit", Some("requesting-code-review")),
        (
            "open a pull request on github for this branch",
            Some("github"),
        ),
        ("create a GitHub issue and review the PR", Some("github")),
        (
            "my test is failing and I don't know why, find the root cause",
            Some("systematic-debugging"),
        ),
        (
            "this bug keeps coming back, help me debug it",
            Some("systematic-debugging"),
        ),
        (
            "write the tests first, then the code (TDD)",
            Some("test-driven-development"),
        ),
        ("search arxiv for papers on diffusion models", Some("arxiv")),
        ("merge these two PDF files and fill the form", Some("pdf")),
        (
            "edit the Word document and the PowerPoint deck",
            Some("microsoft-office"),
        ),
        (
            "draw an architecture diagram of our cloud infrastructure",
            Some("architecture-diagram"),
        ),
        (
            "turn the meeting notes into action items with owners",
            Some("meeting-action-items"),
        ),
        (
            "triage my inbox and draft replies to the emails",
            Some("email-inbox-triage"),
        ),
        (
            "get the transcript of this youtube video and summarize it",
            Some("youtube-content"),
        ),
        (
            "find the coordinates and a route between two addresses",
            Some("maps"),
        ),
        (
            "the page returned 403, I'm blocked by a paywall",
            Some("blocked-page-recovery"),
        ),
        ("what is the weather like tomorrow in the mountains", None),
        ("", None),
    ];

    #[test]
    fn goldens() {
        let f = bundled();
        let report = f.index.refresh().unwrap();
        assert!(report.added >= 37, "{report:?}");
        let mut failures = Vec::new();
        for (message, expected) in GOLDENS {
            let hits = f
                .index
                .query(&q(message), &Limits::default())
                .unwrap()
                .skills;
            let got = hits.first().map(|h| h.name.as_str());
            if got != *expected {
                failures.push(format!(
                    "{message:?}: expected {expected:?}, got {:?}",
                    hits.iter().map(|h| (&h.name, h.score)).collect::<Vec<_>>()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The thresholds were tuned on 37 skills; the same goldens must hold with Hermes's whole
    /// skill set (~190 more, its `skills/` and `optional-skills/` as external folders, so each
    /// bundled skill is also there as a shadowed copy). Skipped when `../hermes-agent` is absent.
    #[test]
    fn goldens_hold_on_hermess_full_skill_set() {
        let hermes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../hermes-agent");
        if !hermes.join("optional-skills").is_dir() {
            eprintln!("skipped: no {}", hermes.display());
            return;
        }
        let f = bundled();
        let mut config = f.skills.config();
        config.external_dirs.push(hermes.join("skills"));
        config.external_dirs.push(hermes.join("optional-skills"));
        f.skills.set_config(config);
        let report = f.index.refresh().unwrap();
        assert!(report.added >= 150, "{report:?}");
        let mut failures = Vec::new();
        for (message, expected) in GOLDENS {
            let hits = f
                .index
                .query(&q(message), &Limits::default())
                .unwrap()
                .skills;
            if hits.first().map(|h| h.name.as_str()) != *expected {
                failures.push(format!(
                    "{message:?}: expected {expected:?}, got {:?}",
                    hits.iter().map(|h| (&h.name, h.score)).collect::<Vec<_>>()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn hits_carry_category_path_and_a_score_in_order() {
        let f = bundled();
        f.index.refresh().unwrap();
        let hits = f
            .index
            .query(
                &q("create a GitHub issue and review the PR"),
                &Limits::default(),
            )
            .unwrap()
            .skills;
        let first = &hits[0];
        assert_eq!(first.name, "github");
        assert_eq!(first.category.as_deref(), Some("software-development"));
        assert!(first.path.ends_with("SKILL.md") && first.path.is_file());
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
        assert!(hits.len() <= 3);
    }

    #[test]
    fn the_project_name_and_last_assistant_message_steer_the_query() {
        let f = fixture();
        write_skill(
            &f,
            "ops/zephyr-deploy",
            "zephyr-deploy",
            "Ship the zephyr service.",
            "Run the pipeline.",
        );
        write_skill(
            &f,
            "ops/other",
            "other",
            "Something else entirely.",
            "Nothing.",
        );
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        let by_project = f
            .index
            .query(
                &IndexQuery {
                    message: "ship it".into(),
                    project: Some("zephyr".into()),
                    ..Default::default()
                },
                &limits,
            )
            .unwrap();
        assert_eq!(by_project.skills[0].name, "zephyr-deploy");
        let by_assistant = f
            .index
            .query(
                &IndexQuery {
                    message: "yes do that".into(),
                    last_assistant: Some("Shall I deploy the zephyr service now?".into()),
                    ..Default::default()
                },
                &limits,
            )
            .unwrap();
        assert_eq!(by_assistant.skills[0].name, "zephyr-deploy");
    }

    #[test]
    fn project_name_is_the_git_root_folder_or_the_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("my-repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        assert_eq!(
            project_name(&repo.join("src/deep")).as_deref(),
            Some("my-repo")
        );
        let plain = dir.path().join("scratch");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(project_name(&plain).as_deref(), Some("scratch"));
    }

    #[test]
    fn disabled_skills_and_memory_targets_are_left_out() {
        let f = fixture();
        write_skill(
            &f,
            "dev/gitbook",
            "gitbook",
            "Everything about gitbook.",
            "gitbook gitbook",
        );
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        assert_eq!(
            f.index.query(&q("gitbook"), &limits).unwrap().skills.len(),
            1
        );
        let mut config = f.skills.config();
        config.disabled.insert("gitbook".into());
        f.skills.set_config(config);
        assert!(
            f.index
                .query(&q("gitbook"), &limits)
                .unwrap()
                .skills
                .is_empty()
        );

        // A fact in USER.md, with the user profile switched off: indexed, not returned.
        add_memory(&f, Target::User, "Prefers tabs over spaces.");
        f.index.refresh().unwrap();
        let ask = q("tabs spaces");
        let on = Limits {
            min_memory_score: 0.0,
            ..limits
        };
        assert_eq!(f.index.query(&ask, &on).unwrap().memory.len(), 1);
        let mut cfg = f.memory.config();
        cfg.user_profile_enabled = false;
        f.memory.set_config(cfg);
        assert!(f.index.query(&ask, &on).unwrap().memory.is_empty());
    }

    // --- memory ---

    fn add_memory(f: &Fixture, target: Target, content: &str) {
        let op = hermes_memory::Operation {
            action: "add".into(),
            content: Some(content.into()),
            ..Default::default()
        };
        let reply = f
            .memory
            .apply(target, &[op], &mut hermes_memory::TurnFailures::default());
        assert_eq!(reply["success"], true, "{reply}");
    }

    #[test]
    fn memory_entries_are_indexed_and_found() {
        let f = fixture();
        add_memory(
            &f,
            Target::Memory,
            "The deploy pipeline for the billing service lives in infra/billing.",
        );
        add_memory(&f, Target::Memory, "Run cargo clippy before every commit.");
        add_memory(
            &f,
            Target::User,
            "Alexandros prefers concise answers and metric units.",
        );
        let report = f.index.refresh().unwrap();
        assert!(report.memory_rebuilt);
        assert_eq!(f.index.memory_count(), 3);
        let limits = Limits {
            min_memory_score: 0.0,
            ..Limits::default()
        };
        let hits = f
            .index
            .query(&q("where is the billing deploy pipeline"), &limits)
            .unwrap()
            .memory;
        assert_eq!(hits[0].tier, "memory");
        assert!(hits[0].text.contains("billing"));
        assert!(hits[0].id.starts_with("memory:"));
        let hits = f
            .index
            .query(&q("which units does the user like"), &limits)
            .unwrap()
            .memory;
        assert_eq!(hits[0].tier, "user");
        // An unchanged memory is not re-indexed.
        assert!(!f.index.refresh().unwrap().memory_rebuilt);
    }

    #[test]
    fn memory_goldens_with_the_default_threshold() {
        let f = fixture();
        for (target, text) in [
            (
                Target::Memory,
                "The billing service deploys from infra/billing with the release pipeline.",
            ),
            (
                Target::Memory,
                "Run cargo clippy and cargo fmt before every commit in this repo.",
            ),
            (
                Target::Memory,
                "The staging database is reset every Friday night.",
            ),
            (
                Target::Memory,
                "Windows paths in the shell need quoting because of spaces.",
            ),
            (
                Target::User,
                "Alexandros is strong in Python, new to Rust, and likes short answers.",
            ),
            (Target::User, "Prefers metric units and a dark theme."),
        ] {
            add_memory(&f, target, text);
        }
        f.index.refresh().unwrap();
        let top = |message: &str| {
            f.index
                .query(&q(message), &Limits::default())
                .unwrap()
                .memory
        };
        assert!(
            top("how do I deploy the billing service")[0]
                .text
                .contains("billing")
        );
        assert!(top("run clippy before I commit")[0].text.contains("clippy"));
        assert!(
            top("explain this Rust borrow error to me")[0]
                .text
                .contains("Rust")
        );
        assert!(
            top("when is the staging database reset")[0]
                .text
                .contains("staging")
        );
        assert!(top("what is the capital of france").is_empty());
        assert!(top("write a haiku").is_empty());
    }

    #[test]
    fn a_memory_write_marks_the_index_dirty_and_the_next_query_sees_it() {
        let f = fixture();
        f.index.attach();
        f.index.refresh().unwrap();
        add_memory(
            &f,
            Target::Memory,
            "The staging database password rotates on Fridays.",
        );
        let limits = Limits {
            min_memory_score: 0.0,
            ..Limits::default()
        };
        let hits = f
            .index
            .query(
                &q("when does the staging database password rotate"),
                &limits,
            )
            .unwrap();
        assert_eq!(hits.memory.len(), 1);
        // A replace and a remove are seen too.
        let op = hermes_memory::Operation {
            action: "remove".into(),
            old_text: Some("staging database".into()),
            ..Default::default()
        };
        let reply = f.memory.apply(
            Target::Memory,
            &[op],
            &mut hermes_memory::TurnFailures::default(),
        );
        assert_eq!(reply["success"], true, "{reply}");
        let hits = f
            .index
            .query(&q("staging database password"), &limits)
            .unwrap();
        assert!(hits.memory.is_empty());
    }

    // --- skills: incremental rebuild ---

    #[test]
    fn a_second_refresh_reads_nothing_and_changes_nothing() {
        let f = bundled();
        let first = f.index.refresh().unwrap();
        assert_eq!(first.files_read, first.files_seen);
        let second = f.index.refresh().unwrap();
        assert_eq!(second.files_read, 0, "{second:?}");
        assert_eq!((second.added, second.updated, second.removed), (0, 0, 0));
        assert_eq!(second.files_seen, first.files_seen);
        assert_eq!(f.index.skill_count(), first.added);
    }

    #[test]
    fn a_changed_added_and_deleted_skill_are_picked_up_alone() {
        let f = fixture();
        let alpha = write_skill(
            &f,
            "x/alpha",
            "alpha",
            "Handles quasar telemetry.",
            "quasar",
        );
        let beta = write_skill(&f, "x/beta", "beta", "Handles pulsar logging.", "pulsar");
        age_all(f.skills.root());
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        assert_eq!(
            f.index.query(&q("quasar"), &limits).unwrap().skills[0].name,
            "alpha"
        );

        // Change one file (its mtime moves forward).
        std::fs::write(
            &alpha,
            "---\nname: alpha\ndescription: Handles nebula telemetry.\n---\nnebula\n",
        )
        .unwrap();
        let report = f.index.refresh().unwrap();
        assert_eq!(
            (
                report.files_read,
                report.updated,
                report.added,
                report.removed
            ),
            (1, 1, 0, 0),
            "{report:?}"
        );
        assert!(
            f.index
                .query(&q("quasar"), &limits)
                .unwrap()
                .skills
                .is_empty()
        );
        assert_eq!(
            f.index.query(&q("nebula"), &limits).unwrap().skills[0].name,
            "alpha"
        );

        // Add one.
        age_all(f.skills.root());
        f.index.refresh().unwrap(); // settles the files written just now
        write_skill(&f, "y/gamma", "gamma", "Handles comet tracking.", "comet");
        let report = f.index.refresh().unwrap();
        assert_eq!((report.files_read, report.added), (1, 1), "{report:?}");
        assert_eq!(
            f.index.query(&q("comet"), &limits).unwrap().skills[0]
                .category
                .as_deref(),
            Some("y")
        );

        // Delete one.
        age_all(f.skills.root());
        f.index.refresh().unwrap();
        std::fs::remove_dir_all(beta.parent().unwrap()).unwrap();
        let report = f.index.refresh().unwrap();
        assert_eq!((report.files_read, report.removed), (0, 1), "{report:?}");
        assert!(
            f.index
                .query(&q("pulsar"), &limits)
                .unwrap()
                .skills
                .is_empty()
        );
        assert_eq!(f.index.skill_count(), 2);
    }

    #[test]
    fn a_touched_but_identical_file_is_read_but_not_reindexed() {
        let f = fixture();
        let path = write_skill(
            &f,
            "x/alpha",
            "alpha",
            "Handles quasar telemetry.",
            "quasar",
        );
        age(&path);
        f.index.refresh().unwrap();
        // Same bytes, new mtime.
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let report = f.index.refresh().unwrap();
        assert_eq!(
            (report.files_read, report.updated, report.added),
            (1, 0, 0),
            "{report:?}"
        );
        // ... and now it's settled.
        assert_eq!(f.index.refresh().unwrap().files_read, 0);
    }

    #[test]
    fn a_just_written_file_is_re_read_even_if_its_stat_matches() {
        // Two writes of the same size in the racy window: the second must still be seen.
        let f = fixture();
        let path = write_skill(&f, "x/alpha", "alpha", "Handles quasar telemetry.", "aaaa");
        f.index.refresh().unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(
            &path,
            "---\nname: alpha\ndescription: Handles quasar telemetry.\n---\nbbbb\n",
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        assert_eq!(f.index.refresh().unwrap().updated, 1);
        assert_eq!(f.index.query(&q("bbbb"), &limits).unwrap().skills.len(), 1);
    }

    #[test]
    fn a_racy_read_is_re_read_even_when_the_next_refresh_comes_late() {
        // Read inside the racy window, then rewritten (same size, same mtime) after the read; the
        // next refresh comes once the window is over: the stored stat must not hide the change.
        let f = fixture();
        let path = write_skill(&f, "x/alpha", "alpha", "Handles quasar telemetry.", "aaaa");
        f.index.refresh().unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(
            &path,
            "---\nname: alpha\ndescription: Handles quasar telemetry.\n---\nbbbb\n",
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        std::thread::sleep(Duration::from_nanos(RACY_NANOS as u64) + Duration::from_millis(200));
        assert_eq!(f.index.refresh().unwrap().updated, 1);
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        assert_eq!(f.index.query(&q("bbbb"), &limits).unwrap().skills.len(), 1);
        // Now settled with its real mtime.
        assert_eq!(f.index.refresh().unwrap().files_read, 0);
    }

    #[test]
    fn a_skill_md_that_is_not_utf8_is_left_out_like_the_catalog_does() {
        let f = fixture();
        let dir = f.skills.root().join("x/broken");
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = b"---\nname: broken\ndescription: quasar\n---\nquasar ".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x80]);
        std::fs::write(dir.join("SKILL.md"), bytes).unwrap();
        age(&dir.join("SKILL.md"));
        assert!(f.skills.catalog_all().is_empty());
        f.index.refresh().unwrap();
        assert_eq!(f.index.skill_count(), 0);
    }

    #[test]
    fn an_unreadable_memory_file_keeps_its_indexed_entries() {
        let f = fixture();
        add_memory(
            &f,
            Target::Memory,
            "The staging database is reset on Fridays.",
        );
        f.index.refresh().unwrap();
        // Not UTF-8: the store refuses to read it (as it would a locked file).
        std::fs::write(f.memory.path(Target::Memory), [0xff, 0xfe, 0x80]).unwrap();
        f.index.refresh().unwrap();
        let limits = Limits {
            min_memory_score: 0.0,
            ..Limits::default()
        };
        let hits = f.index.query(&q("staging database"), &limits).unwrap();
        assert_eq!(hits.memory.len(), 1);
    }

    #[test]
    fn a_new_external_folder_is_indexed_without_a_manual_refresh() {
        let f = fixture();
        f.index.attach();
        f.index.refresh().unwrap();
        let external = tempfile::tempdir().unwrap();
        let dir = external.path().join("zebra-care");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: zebra-care\ndescription: Look after zebras.\n---\nzebra\n",
        )
        .unwrap();
        let mut config = f.skills.config();
        config.external_dirs.push(external.path().to_path_buf());
        f.skills.set_config(config);
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        let hits = f.index.query(&q("zebra"), &limits).unwrap().skills;
        assert_eq!(hits[0].name, "zebra-care");
    }

    #[test]
    fn a_skill_for_another_platform_is_not_indexed_and_not_re_read() {
        let f = fixture();
        let dir = f.skills.root().join("mac/notes");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(
            &path,
            "---\nname: apple-notes\ndescription: Mac only quasar.\nplatforms: [macos]\n---\n",
        )
        .unwrap();
        age(&path);
        f.index.refresh().unwrap();
        assert_eq!(f.index.skill_count(), 0);
        assert_eq!(f.index.refresh().unwrap().files_read, 0);
    }

    #[test]
    fn the_first_copy_of_a_name_is_the_one_served() {
        let f = fixture();
        let external = tempfile::tempdir().unwrap();
        let mut config = f.skills.config();
        config.external_dirs.push(external.path().to_path_buf());
        f.skills.set_config(config);
        let own = write_skill(&f, "a/dup", "dup", "Own copy about zebra.", "zebra");
        let ext = external.path().join("dup");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("SKILL.md"),
            "---\nname: dup\ndescription: External copy about zebra.\n---\nzebra\n",
        )
        .unwrap();
        age(&own);
        age(&ext.join("SKILL.md"));
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        let hits = f.index.query(&q("zebra"), &limits).unwrap().skills;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].description.starts_with("Own"));
        // Remove the first: the external copy takes over without any re-read of itself.
        std::fs::remove_dir_all(own.parent().unwrap()).unwrap();
        let report = f.index.refresh().unwrap();
        assert_eq!((report.removed, report.files_read), (1, 0), "{report:?}");
        let hits = f.index.query(&q("zebra"), &limits).unwrap().skills;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].description.starts_with("External"));
    }

    #[test]
    fn skill_manage_writes_keep_the_index_fresh_without_a_manual_refresh() {
        let f = fixture();
        f.index.attach();
        f.index.refresh().unwrap();
        let reply = f.skills.manage_tool(&serde_json::json!({
            "operations": [{
                "action": "create", "name": "kubernetes-rollout", "category": "devops",
                "content": "---\nname: kubernetes-rollout\ndescription: Roll out a kubernetes deployment safely.\n---\n# Rollout\nUse kubectl rollout status.\n"
            }]
        }));
        assert_eq!(reply["success"], true, "{reply}");
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        let hits = f
            .index
            .query(&q("roll out a kubernetes deployment"), &limits)
            .unwrap()
            .skills;
        assert_eq!(hits[0].name, "kubernetes-rollout");
        // And an edit, then a delete.
        let reply = f.skills.manage_tool(&serde_json::json!({
            "operations": [{"action": "delete", "name": "kubernetes-rollout"}]
        }));
        assert_eq!(reply["success"], true, "{reply}");
        assert!(
            f.index
                .query(&q("kubernetes"), &limits)
                .unwrap()
                .skills
                .is_empty()
        );
    }

    #[test]
    fn notify_changed_refreshes_in_the_background() {
        let f = fixture();
        write_skill(
            &f,
            "x/alpha",
            "alpha",
            "Handles quasar telemetry.",
            "quasar",
        );
        f.index.notify_changed();
        let start = std::time::Instant::now();
        while f.index.skill_count() == 0 && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(f.index.skill_count(), 1);
    }

    #[test]
    fn a_stale_schema_or_a_corrupt_file_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let skills = Arc::new(SkillStore::new(
            dir.path().join("skills"),
            Default::default(),
        ));
        let memory = Arc::new(MemoryStore::new(
            dir.path().join("memories"),
            Default::default(),
        ));
        let path = dir.path().join("index.db");
        std::fs::write(&path, b"this is not a sqlite file, not even close, really").unwrap();
        let index = LocalIndex::open(&path, skills.clone(), memory.clone()).unwrap();
        let dir_skill = skills.root().join("x/alpha");
        std::fs::create_dir_all(&dir_skill).unwrap();
        std::fs::write(
            dir_skill.join("SKILL.md"),
            "---\nname: alpha\ndescription: quasar.\n---\n",
        )
        .unwrap();
        assert_eq!(index.refresh().unwrap().added, 1);
        drop(index);
        // An older schema version: dropped, then rebuilt from the files.
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 0)
            .unwrap();
        let index = LocalIndex::open(&path, skills, memory).unwrap();
        assert_eq!(index.skill_count(), 0);
        assert_eq!(index.refresh().unwrap().added, 1);
    }

    // --- escaping ---

    const NASTY: &[&str] = &[
        "",
        "   ",
        "\"",
        "\"\"\"unbalanced",
        "NEAR(a b, 3)",
        "a NEAR b",
        "foo AND bar OR baz NOT qux",
        "*",
        "pref*",
        "col:value name:git",
        "-git",
        "git -commit",
        "(git",
        "git)",
        "((()))",
        "^start",
        "{name description}: git",
        "a'b\"c",
        "git; DROP TABLE skill_doc; --",
        "path\\with\\backslashes C:\\Users\\x",
        "résumé naïve café Ünïcödé",
        "日本語のテキスト と 中文",
        "emoji \u{1F600}\u{1F680} test",
        "\u{0}nul\u{0}byte",
        "\n\r\t tabs and\nnewlines",
        "'; --",
        "+ - * / ( ) [ ] { } < > = ! ? @ # $ % & | ~ ` , . ;",
        "rank bm25 MATCH skill_doc",
        "NOT",
        "OR",
        "AND",
        "NEAR",
        // Characters Rust calls alphanumeric that the `unicode61` tokenizer may not (spacing
        // marks, superscripts, roman numerals, other scripts' digits), alone and mixed: a quoted
        // token that tokenizes to nothing must not be an error.
        "\u{093E}",
        "\u{093E} \u{0BCD}\u{0BBE}",
        "² ³ Ⅻ ٣ ǅ",
        "x\u{0301}\u{0301}\u{0301}",
        "\u{200D}\u{FEFF}\u{00AD}",
        "\u{1D400}\u{1D401}",
        "İstanbul ß ẞ ﬁ",
        "\u{FFFD}\u{FFFD}",
    ];

    #[test]
    fn hostile_text_never_breaks_the_match_expression() {
        let f = bundled();
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            min_memory_score: 0.0,
            ..Limits::default()
        };
        for text in NASTY {
            f.index
                .query(&q(text), &limits)
                .unwrap_or_else(|e| panic!("{text:?}: {e:#}"));
            f.index
                .query(
                    &IndexQuery {
                        message: "git".into(),
                        last_assistant: Some((*text).into()),
                        project: Some((*text).into()),
                    },
                    &limits,
                )
                .unwrap_or_else(|e| panic!("as context {text:?}: {e:#}"));
        }
        // Very long input, and a very long single word.
        f.index
            .query(&q(&"git commit ".repeat(100_000)), &limits)
            .unwrap();
        f.index.query(&q(&"x".repeat(1_000_000)), &limits).unwrap();
        f.index.query(&q(&"\"".repeat(50_000)), &limits).unwrap();
    }

    #[test]
    fn operators_in_user_text_are_plain_words() {
        // `NEAR`, `AND`, `OR`, `NOT` and the prefix star must not act as syntax.
        assert_eq!(
            match_expr(&query_tokens(&q("NEAR(git commit, 2)"))).as_deref(),
            Some("\"near\" OR \"git\" OR \"commit\"")
        );
        assert_eq!(
            match_expr(&query_tokens(&q("git*"))).as_deref(),
            Some("\"git\"")
        );
        assert_eq!(
            match_expr(&query_tokens(&q("col:name -git \"x\""))).as_deref(),
            Some("\"col\" OR \"name\" OR \"git\"")
        );
        assert_eq!(match_expr(&query_tokens(&q("\" * ( ) :"))), None);
        assert_eq!(match_expr(&query_tokens(&q(""))), None);
        // Every expression is only quoted alphanumerics joined by OR.
        for text in NASTY {
            if let Some(expr) = match_expr(&query_tokens(&q(text))) {
                for part in expr.split(" OR ") {
                    let inner = part
                        .strip_prefix('"')
                        .and_then(|p| p.strip_suffix('"'))
                        .unwrap_or_else(|| panic!("{text:?} -> {expr}"));
                    assert!(
                        !inner.is_empty() && inner.chars().all(char::is_alphanumeric),
                        "{text:?} -> {expr}"
                    );
                }
            }
        }
    }

    #[test]
    fn token_budget_and_sources_are_bounded() {
        let long = (0..500)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let tokens = query_tokens(&IndexQuery {
            message: long.clone(),
            last_assistant: Some(long.clone()),
            project: Some(long),
        });
        assert!(tokens.len() <= MESSAGE_TOKENS + ASSISTANT_TOKENS + PROJECT_TOKENS);
        assert!(tokens.iter().all(|t| t.chars().count() <= MAX_TOKEN_CHARS));
        // The assistant text counts only through its first 500 characters.
        let tokens = query_tokens(&IndexQuery {
            message: "hello".into(),
            last_assistant: Some(format!("{} tailword", "x ".repeat(300))),
            project: None,
        });
        assert!(!tokens.contains(&"tailword".to_string()));
    }

    #[test]
    fn unicode_text_is_found_through_the_tokenizer() {
        let f = fixture();
        write_skill(
            &f,
            "x/cafe",
            "cafe-notes",
            "Notes about the café menu.",
            "résumé du café",
        );
        f.index.refresh().unwrap();
        let limits = Limits {
            min_skill_score: 0.0,
            ..Limits::default()
        };
        // Diacritics fold; porter stems "menus" to "menu".
        assert_eq!(
            f.index.query(&q("cafe menus"), &limits).unwrap().skills[0].name,
            "cafe-notes"
        );
    }

    // --- cost ---

    fn generate(f: &Fixture, n: usize) {
        for i in 0..n {
            write_skill(
                f,
                &format!("gen{}/skill-{i}", i % 12),
                &format!("skill-{i}"),
                &format!(
                    "Generated skill number {i} about topic{} and widget{}.",
                    i % 40,
                    i % 17
                ),
                &format!(
                    "# Skill {i}\n{}\n",
                    "Some filler text about nothing in particular. ".repeat(60)
                ),
            );
        }
    }

    #[test]
    fn rebuild_cost_on_a_few_hundred_skills() {
        let f = fixture();
        generate(&f, 400);
        age_all(f.skills.root());
        let t = std::time::Instant::now();
        let first = f.index.refresh().unwrap();
        let first_time = t.elapsed();
        assert_eq!(first.added, 400);
        // Nothing changed: stat only, nothing read.
        let t = std::time::Instant::now();
        let walk = skills::skill_files(f.skills.root()).len();
        eprintln!("walk of {walk} files alone: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let second = f.index.refresh().unwrap();
        let second_time = t.elapsed();
        assert_eq!(second.files_read, 0);
        // One changed file among 400: one read.
        let path = f.skills.root().join("gen3/skill-3/SKILL.md");
        std::fs::write(
            &path,
            "---\nname: skill-3\ndescription: changed.\n---\nchanged\n",
        )
        .unwrap();
        let third = f.index.refresh().unwrap();
        assert_eq!((third.files_read, third.updated), (1, 1));
        // Queries stay fast.
        let t = std::time::Instant::now();
        for i in 0..50 {
            f.index
                .query(
                    &q(&format!("topic{} widget{} please", i % 40, i % 17)),
                    &Limits::default(),
                )
                .unwrap();
        }
        let query_time = t.elapsed() / 50;
        eprintln!(
            "400 skills: first refresh {first_time:?}, idle refresh {second_time:?}, query {query_time:?}"
        );
        // Generous bounds (debug build, shared CI disk): they catch an accidental O(n) re-read.
        assert!(second_time < Duration::from_secs(2), "{second_time:?}");
        assert!(query_time < Duration::from_millis(100), "{query_time:?}");
    }

    #[tokio::test]
    async fn the_async_wrappers_run_off_the_worker() {
        let f = bundled();
        let index = f.index.clone();
        let report = index.refresh_async().await.unwrap();
        assert!(report.added >= 37);
        let hits = index
            .query_async(q("search arxiv papers"), Limits::default())
            .await
            .unwrap();
        assert_eq!(hits.skills[0].name, "arxiv");
    }
}
