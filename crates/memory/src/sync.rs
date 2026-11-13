//! Keeps `memory.db` and the two files (`MEMORY.md`, `USER.md`) in step (Backend v2, Phase 9,
//! CP9.1). The database holds the entries (and every entry that ever left them, marked
//! removed); the files are an exported view of its global `core` entries, so the user's editor,
//! other tools and the frozen GPUI app keep working, and the system prompt is built from exactly
//! what it was built from before this phase.
//!
//! The rules, all chosen so that memory is never lost:
//! - **Every access compares the file with the database** (inside the database's write lock
//!   when anything has to change, so two processes never decide on different views). In step
//!   means: the file's hash is the one recorded at the last sync *and* its entries are the live
//!   `core` entries, in order. Otherwise the file is taken over as it is: it was edited by hand,
//!   written by an older build, or a crash came between our file write and our commit. Entries
//!   that are no longer in the file were removed by the user (or an older build), so they are
//!   purged from the database, as they were gone for good before this phase (`db::Removal`);
//!   when *every* entry left at once (the file deleted or emptied: likely an accident) they are
//!   first copied to `<file>.bak.dropped.<unix time>`, and without that copy nothing is taken
//!   over. Only `repair` (our own hash, other rows) keeps them as removed rows.
//! - **Settings ▸ Memory ▸ reset** goes through [`MemoryStore::reset`]: the file and every row
//!   of its kind, erased, no copy.
//! - **A read never writes the `.md` files.** The first migration and a re-import only read
//!   them. The files are written by a store write (tool, Star map, approval), as before.
//! - **Before the first migration of a file it is copied** to
//!   `<file>.bak.pre-memory-db.<unix time>` (never overwritten, read back to check it, never
//!   deleted by us; not repeated while an identical copy exists). No copy, no migration: the store
//!   then works from the file alone for that call, as it did before this phase.
//! - **A write is one step under the database's write lock:** take the file over if it changed,
//!   decide the new entries, record them and the new hash, write the file to a temporary name,
//!   re-read the real file (changed since we read it → roll back and start again; it holds an
//!   entry the database doesn't know → refuse), rename over it, commit. Stopped anywhere before
//!   the rename, nothing changed; stopped after it, the file holds the write and the next access
//!   takes it over.
//! - **Any database trouble falls back to the file alone** for that call (logged), with the same
//!   re-read-before-rename check. The file is then ahead and is taken over when the database
//!   works again. A newer schema, or a damaged file that could not be moved aside, switches the
//!   database off for the life of the process.
//!
//! Not from Hermes: see `db.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::db::{self, ABSENT, MemoryDb, OpenError};
use crate::store::{MemoryStore, Target, parse_entries};

/// How many times the final rename over a memory file is tried (20, 40, 80, 160 ms apart).
const RENAME_ATTEMPTS: u32 = 5;

/// Where a test stops the store, as a crash (or a power cut) at that point would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Failpoint {
    /// `memory.db` was just created (tables, no entries).
    AfterDbCreate,
    /// The pre-migration backup copy can't be made (a full disk, no permission).
    BackupFails,
    /// The pre-migration backup copy was written; nothing imported yet.
    AfterBackup,
    /// Inside the import transaction, after its first row.
    MidImport,
    /// The temporary export file is half written (a power cut).
    MidExportTmp,
    /// The temporary export file is complete; the real file not replaced yet.
    AfterExportTmp,
    /// The real file was replaced; the database transaction not committed yet.
    AfterExportRename,
}

/// What the store's lock guards: the database connection, opened at the first use.
#[derive(Default)]
pub(crate) struct Inner {
    db: DbState,
    /// Why the database is not used (for the log and [`MemoryStore::database_problem`]).
    problem: Option<String>,
}

#[derive(Default)]
enum DbState {
    #[default]
    Unopened,
    Open(MemoryDb),
    /// Not used until the app restarts: the files alone, as before this phase.
    Off,
}

/// One of the two files, as it is on disk right now.
pub(crate) enum FileState {
    Absent,
    /// It exists but can't be read as text (locked, no permission, not UTF-8).
    Unreadable,
    Text {
        /// The text without a Notepad BOM.
        raw: String,
        bytes: Vec<u8>,
        hash: String,
    },
}

impl FileState {
    /// The value recorded as `synced:<kind>` for this state (`None`: unreadable).
    pub(crate) fn hash(&self) -> Option<&str> {
        match self {
            FileState::Absent => Some(ABSENT),
            FileState::Unreadable => None,
            FileState::Text { hash, .. } => Some(hash),
        }
    }

    pub(crate) fn raw(&self) -> &str {
        match self {
            FileState::Text { raw, .. } => raw,
            _ => "",
        }
    }
}

/// A file's entries as the store sees them now.
pub(crate) struct View {
    pub entries: Vec<String>,
    /// Whether `entries` can be trusted: the database answered, or the file was read. False
    /// only when the file exists but can't be read and the database has nothing for it.
    pub trusted: bool,
}

/// Why one attempt at a database-backed operation stopped.
pub(crate) enum Abort {
    /// The file changed between our read and our rename (a writer that doesn't take the
    /// database's lock: an editor, an older build): start again.
    Conflict,
    /// Another process kept the database's write lock past the busy timeout: start again
    /// (a write does not fall back to the file alone for this, which would race that process).
    Busy(String),
    /// The database failed: fall back to the file alone.
    Db(String),
    /// The file can't be taken over safely right now (a copy that must come first could not be
    /// made): a read answers from the file alone, a write is refused.
    Refused(String),
    /// A test failpoint fired: stop, as a crash would.
    Crash,
}

impl From<rusqlite::Error> for Abort {
    fn from(error: rusqlite::Error) -> Self {
        if db::is_busy(&error) {
            Abort::Busy(error.to_string())
        } else {
            Abort::Db(error.to_string())
        }
    }
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn read_file(path: &Path) -> FileState {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FileState::Absent,
        Err(_) => return FileState::Unreadable,
    };
    match std::str::from_utf8(&bytes) {
        Ok(text) => FileState::Text {
            raw: text.trim_start_matches('\u{feff}').to_string(),
            hash: hash(&bytes),
            bytes,
        },
        Err(_) => FileState::Unreadable,
    }
}

/// Writes a new file and flushes it to disk; `Ok(false)` when the name is taken.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    };
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(true)
}

/// `memory.db` and its `-wal` / `-shm` companions.
fn db_files(path: &Path) -> [PathBuf; 3] {
    let with = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [with("-wal"), with("-shm"), path.to_path_buf()]
}

/// Moves a damaged `memory.db` (and its journal files) aside to `memory.db.corrupt-<time>`, so
/// a fresh one can be made without writing over it. The new name, or why it couldn't be moved
/// (then nothing was moved).
fn move_aside(path: &Path) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let mut base = path.as_os_str().to_owned();
    base.push(format!(".corrupt-{stamp}"));
    let base = PathBuf::from(base);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in db_files(path).into_iter().zip(db_files(&base)) {
        if !from.exists() {
            continue;
        }
        if to.exists() {
            return Err(format!("{} already exists", to.display()));
        }
        if let Err(error) = std::fs::rename(&from, &to) {
            // All or nothing: a fresh database next to a stale journal would be damaged too.
            for (back_to, back_from) in moved.iter().rev() {
                let _ = std::fs::rename(back_from, back_to);
            }
            return Err(format!("{} could not be moved: {error}", from.display()));
        }
        moved.push((from, to));
    }
    Ok(base)
}

impl Inner {
    /// The database, opened at the first call. `None`: the files alone.
    pub(crate) fn db(
        &mut self,
        path: Option<&Path>,
        crash_after_create: bool,
    ) -> Option<&mut MemoryDb> {
        if matches!(self.db, DbState::Unopened) {
            self.db = match path {
                None => DbState::Off,
                Some(path) => self.open(path, crash_after_create),
            };
        }
        match &mut self.db {
            DbState::Open(db) => Some(db),
            _ => None,
        }
    }

    fn open(&mut self, path: &Path, crash_after_create: bool) -> DbState {
        let mut problem = None;
        let mut off = |why: String| {
            tracing::error!("{why}; memory works from MEMORY.md / USER.md alone");
            problem = Some(why);
            DbState::Off
        };
        let state = match MemoryDb::open(path, crash_after_create) {
            Ok(db) => DbState::Open(db),
            Err(OpenError::Busy(why)) => {
                // Another process is creating or migrating it: the files alone for this call,
                // the next use tries again.
                tracing::warn!("{} is busy ({why}); trying again at the next use", path.display());
                return DbState::Unopened;
            }
            Err(OpenError::Corrupt(why)) => match move_aside(path) {
                Ok(aside) => {
                    tracing::error!(
                        "{} is damaged ({why}); it was moved aside to {} and a new one is made from MEMORY.md / USER.md",
                        path.display(),
                        aside.display()
                    );
                    match MemoryDb::open(path, crash_after_create) {
                        Ok(db) => {
                            self.problem = Some(format!(
                                "memory.db was damaged and was moved aside to {}",
                                aside.display()
                            ));
                            DbState::Open(db)
                        }
                        Err(error) => off(format!("{} can't be used: {error}", path.display())),
                    }
                }
                Err(move_error) => off(format!(
                    "{} is damaged ({why}) and was left in place ({move_error})",
                    path.display()
                )),
            },
            Err(error) => off(format!("{} is not used: {error}", path.display())),
        };
        if problem.is_some() {
            self.problem = problem;
        }
        state
    }

    pub(crate) fn problem(&self) -> Option<String> {
        self.problem.clone()
    }
}

/// What a sync found.
pub(crate) struct Synced {
    /// The live `core` entries after the sync; `None`: the file can't be read and was never
    /// migrated, so there is nothing to answer from.
    pub entries: Option<Vec<String>>,
    /// Whether the database changed (a migration or a re-import).
    pub changed: bool,
    /// Rows were deleted (a hand edit removed entries): checkpoint after the commit.
    pub purged: bool,
}

impl MemoryStore {
    /// Fires a failpoint (tests only; always false otherwise).
    pub(crate) fn crash_at(&self, at: Failpoint) -> bool {
        #[cfg(test)]
        {
            let mut armed = self.failpoint.lock();
            if *armed == Some(at) {
                *armed = None;
                return true;
            }
        }
        let _ = at;
        false
    }

    /// Brings the live `core` entries of `target` in line with `file` (read inside the
    /// transaction `tx`, so no other process of ours changes either in between).
    pub(crate) fn sync_in(
        &self,
        tx: &rusqlite::Connection,
        target: Target,
        file: &FileState,
    ) -> Result<Synced, Abort> {
        let kind = target.kind();
        let key = db::meta_synced(kind);
        let seen = db::get_meta(tx, &key)?;
        let live = db::live_texts(tx, kind)?;
        let (entries, file_hash) = match file {
            FileState::Unreadable => {
                return Ok(Synced {
                    entries: seen.is_some().then_some(live),
                    changed: false,
                    purged: false,
                });
            }
            FileState::Absent => (Vec::new(), ABSENT),
            FileState::Text { raw, hash, .. } => (parse_entries(raw), hash.as_str()),
        };
        if seen.as_deref() == Some(file_hash) && live == entries {
            return Ok(Synced {
                entries: Some(live),
                changed: false,
                purged: false,
            });
        }
        let why = match seen.as_deref() {
            None => "migration",
            Some(s) if s == file_hash => {
                // Our hash, other entries: the database is not what we last wrote. The file is
                // what the user sees and what the prompt was built from, so it wins.
                tracing::warn!(
                    "memory.db: {} matches the last export but the entries differ; taking the file over",
                    target.file_name()
                );
                "repair"
            }
            Some(_) if matches!(file, FileState::Absent) => "file-deleted",
            Some(_) => "manual-edit",
        };
        if seen.is_none() {
            if let FileState::Text { bytes, .. } = file {
                self.backup_before_migration(target, bytes)
                    .map_err(|e| Abort::Db(format!("no backup copy could be made: {e}")))?;
                if self.crash_at(Failpoint::AfterBackup) {
                    return Err(Abort::Crash);
                }
            }
        }
        // A change made outside the app (by hand, an older build) is the user's: what left the
        // file is purged, as it was gone for good before this phase. But a file with no entries
        // left (deleted, emptied) is more likely an accident: the entries are first copied next
        // to it, or nothing is taken over (and no write either, which would build on the empty
        // file and hide the loss).
        let removal = match why {
            "manual-edit" | "file-deleted" => {
                if entries.is_empty() && !live.is_empty() {
                    self.copy_dropped(target, why, &live).map_err(|e| {
                        Abort::Refused(format!(
                            "{} has no entries left and they could not be copied aside first ({e})",
                            target.file_name()
                        ))
                    })?;
                }
                db::Removal::Purge
            }
            why => db::Removal::Mark(why),
        };
        let now = chrono::Utc::now().timestamp();
        let crash = self.crash_at(Failpoint::MidImport);
        let changes = match db::set_core(tx, kind, &entries, removal, &[], now, crash) {
            Ok(changes) => changes,
            Err(_) if crash => return Err(Abort::Crash),
            Err(error) => return Err(error.into()),
        };
        db::set_meta(tx, &key, file_hash)?;
        if seen.is_none() {
            db::set_meta(tx, &format!("migrated:{kind}"), &now.to_string())?;
        }
        if why != "migration" {
            tracing::info!(
                "memory.db: {} was changed outside the store ({why}); its entries were taken over",
                target.file_name()
            );
        }
        Ok(Synced {
            entries: Some(entries),
            changed: changes.changed || seen.is_none(),
            purged: changes.purged,
        })
    }

    /// Before an outside change that drops every entry of `target` is taken over: the entries
    /// as the database last had them, in the file's format, to
    /// `<file>.bak.dropped.<unix time>` (create-new, read back; never deleted by us), so they can
    /// be restored by renaming it back.
    fn copy_dropped(&self, target: Target, why: &str, live: &[String]) -> std::io::Result<()> {
        let content = live.join(crate::ENTRY_DELIMITER);
        let path = self.write_copy(target, "dropped", content.as_bytes(), false)?;
        tracing::warn!(
            "memory: {} lost all its entries outside the app ({why}); they were copied to {}",
            target.file_name(),
            path.display()
        );
        Ok(())
    }

    /// The entries of `target` now, through the database when it can be used (migrating or
    /// re-importing the file first when they differ), else from the file alone. Whether the
    /// database changed. Never writes the `.md` file.
    pub(crate) fn view(&self, target: Target) -> (View, bool) {
        let path = self.path(target);
        let mut inner = self.lock_inner();
        let crash_after_create = self.crash_at(Failpoint::AfterDbCreate);
        if let Some(db) = inner.db(self.db_path(), crash_after_create) {
            match self.view_db(db, target, &path) {
                Ok(Some(view)) => return view,
                Ok(None) => {}
                Err(Abort::Db(error) | Abort::Busy(error) | Abort::Refused(error)) => tracing::warn!(
                    "memory.db: reading {} failed ({error}); using the file alone",
                    target.file_name()
                ),
                Err(Abort::Conflict | Abort::Crash) => {}
            }
        }
        drop(inner);
        let file = read_file(&path);
        let trusted = !matches!(file, FileState::Unreadable);
        (
            View {
                entries: parse_entries(file.raw()),
                trusted,
            },
            false,
        )
    }

    fn view_db(
        &self,
        db: &mut MemoryDb,
        target: Target,
        path: &Path,
    ) -> Result<Option<(View, bool)>, Abort> {
        // Fast path, no lock: already in step.
        let file = read_file(path);
        if let Some(file_hash) = file.hash() {
            let conn = db.conn();
            let kind = target.kind();
            if db::get_meta(conn, &db::meta_synced(kind))?.as_deref() == Some(file_hash) {
                let live = db::live_texts(conn, kind)?;
                if live == parse_entries(file.raw()) {
                    return Ok(Some((
                        View {
                            entries: live,
                            trusted: true,
                        },
                        false,
                    )));
                }
            }
        }
        // Something to do: under the write lock, from a fresh read of the file.
        let tx = db.begin()?;
        let file = read_file(path);
        let synced = self.sync_in(&tx, target, &file)?;
        tx.commit()?;
        if synced.purged {
            db.checkpoint();
        }
        Ok(synced.entries.map(|entries| {
            (
                View {
                    entries,
                    trusted: true,
                },
                synced.changed,
            )
        }))
    }

    /// The copy made before a file is first taken into the database:
    /// `<file>.bak.pre-memory-db.<unix time>` (`.<n>` added if that name is taken). Never
    /// overwritten; skipped when a copy with the same bytes is already there (a migration
    /// retried after a crash, or after `memory.db` was deleted).
    pub(crate) fn backup_before_migration(
        &self,
        target: Target,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let path = self.write_copy(target, "pre-memory-db", bytes, true)?;
        tracing::info!(
            "memory: {} copied to {} before its first import into memory.db",
            target.file_name(),
            path.display()
        );
        Ok(())
    }

    /// Writes `<file>.bak.<label>.<unix time>` (`.<n>` added if the name is taken): created new,
    /// flushed and read back, never overwritten. With `reuse`, an existing `<label>` copy with
    /// the same bytes is the answer instead of a new one.
    fn write_copy(
        &self,
        target: Target,
        label: &str,
        bytes: &[u8],
        reuse: bool,
    ) -> std::io::Result<PathBuf> {
        if self.crash_at(Failpoint::BackupFails) {
            return Err(std::io::Error::other(
                "simulated: the copy can't be written",
            ));
        }
        let dir = self.dir();
        let prefix = format!("{}.bak.{label}.", target.file_name());
        if reuse {
            if let Ok(listing) = std::fs::read_dir(dir) {
                for entry in listing.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(&prefix)
                        && std::fs::read(entry.path()).is_ok_and(|b| b == bytes)
                    {
                        return Ok(entry.path());
                    }
                }
            }
        }
        std::fs::create_dir_all(dir)?;
        let stamp = chrono::Utc::now().timestamp();
        for n in 0..1000 {
            let name = if n == 0 {
                format!("{prefix}{stamp}")
            } else {
                format!("{prefix}{stamp}.{n}")
            };
            let path = dir.join(name);
            if write_new(&path, bytes)? {
                // A copy that doesn't read back is not a backup.
                if std::fs::read(&path)? != bytes {
                    return Err(std::io::Error::other(format!(
                        "{} does not read back as written",
                        path.display()
                    )));
                }
                return Ok(path);
            }
        }
        Err(std::io::Error::other("no free backup name"))
    }

    /// Replaces the file of `target` with `content`, unless it changed since it was read as
    /// `seen` (its [`FileState::hash`]): then `Conflict`. With a database (`tx`), also refuses
    /// when the file on disk holds an entry that is neither in `texts` (what the write decided
    /// from and writes) nor known to the database (live or removed), so no write can make an
    /// entry vanish from both. A temporary file, flushed, then renamed over the real one: a
    /// crash leaves the old file or the new one, never a mix.
    pub(crate) fn replace_file(
        &self,
        tx: Option<&rusqlite::Connection>,
        target: Target,
        seen: &str,
        content: &str,
        texts: &[String],
    ) -> Result<Result<(), String>, Abort> {
        let path = self.path(target);
        if let Err(error) = std::fs::create_dir_all(self.dir()) {
            return Ok(Err(error.to_string()));
        }
        let tmp = self
            .dir()
            .join(format!(".mem_{}.tmp", uuid::Uuid::new_v4().simple()));
        if self.crash_at(Failpoint::MidExportTmp) {
            let half = &content.as_bytes()[..content.len() / 2];
            let _ = std::fs::write(&tmp, half);
            return Err(Abort::Crash);
        }
        match write_new(&tmp, content.as_bytes()) {
            Ok(true) => {}
            Ok(false) => return Ok(Err(format!("{} already exists", tmp.display()))),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(Err(error.to_string()));
            }
        }
        if self.crash_at(Failpoint::AfterExportTmp) {
            return Err(Abort::Crash);
        }
        let discard = |abort: Abort| {
            let _ = std::fs::remove_file(&tmp);
            Err(abort)
        };
        let now = read_file(&path);
        if now.hash() != Some(seen) {
            return discard(Abort::Conflict);
        }
        if let (Some(tx), FileState::Text { raw, .. }) = (tx, &now) {
            let texts: std::collections::HashSet<&String> = texts.iter().collect();
            for entry in parse_entries(raw) {
                let known = match db::knows(tx, target.kind(), &entry) {
                    Ok(known) => known,
                    Err(error) => return discard(error.into()),
                };
                if !texts.contains(&entry) && !known {
                    let _ = std::fs::remove_file(&tmp);
                    tracing::error!(
                        "memory: refusing to write {}: it holds an entry memory.db doesn't know",
                        target.file_name()
                    );
                    return Ok(Err(format!(
                        "{} holds an entry that is not saved in memory.db; writing would lose it",
                        target.file_name()
                    )));
                }
            }
        }
        // On Windows a virus scanner or an indexer opening the file for a moment makes the
        // replace fail (sharing violation): try a few times, briefly, before giving up.
        let mut attempt = 0;
        while let Err(error) = std::fs::rename(&tmp, &path) {
            attempt += 1;
            if attempt >= RENAME_ATTEMPTS {
                let _ = std::fs::remove_file(&tmp);
                return Ok(Err(error.to_string()));
            }
            std::thread::sleep(std::time::Duration::from_millis(20 << attempt));
        }
        if self.crash_at(Failpoint::AfterExportRename) {
            return Err(Abort::Crash);
        }
        Ok(Ok(()))
    }

    /// Settings ▸ Memory ▸ reset: erases `target`'s memory. Its file is deleted and, with a
    /// database, every `core` row of its kind (live or removed) and their FTS text are deleted
    /// with it (not the `project` / `archive` tiers: the page's confirmation names the file),
    /// in one step under the database's write lock (the file goes first, inside the transaction:
    /// stopped between the two, the next access finds the file gone and keeps a `.bak.dropped`
    /// copy rather than losing anything). The pre-migration backups are not touched (they are
    /// the user's; PITFALLS.md). Whether a file was deleted.
    pub fn reset(&self, target: Target) -> Result<bool, String> {
        let path = self.path(target);
        let remove = |path: &Path| match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("Could not delete {}: {e}", target.file_name())),
        };
        let mut inner = self.lock_inner();
        let deleted = match inner.db(self.db_path(), false) {
            Some(db) => {
                let failed = |e: rusqlite::Error| {
                    format!("Could not reset {}: memory.db: {e}. Nothing was changed — retry in a moment.", target.file_name())
                };
                let tx = db.begin().map_err(failed)?;
                // `core` only: the project and archive tiers are not in the file the page's
                // confirmation names, so a reset doesn't erase them (PITFALLS.md).
                let purged = db::purge_kind(&tx, target.kind()).map_err(failed)?;
                db::set_meta(&tx, &db::meta_synced(target.kind()), ABSENT).map_err(failed)?;
                let deleted = remove(&path)?;
                tx.commit().map_err(failed)?;
                if purged {
                    db.checkpoint();
                }
                deleted
            }
            None => remove(&path)?,
        };
        drop(inner);
        self.changed();
        Ok(deleted)
    }

    /// What an export of `target` would write now: the live `core` entries joined by the
    /// delimiter, straight from the database (no sync). `None` while the database can't be
    /// used.
    pub fn export_view(&self, target: Target) -> Option<String> {
        let mut inner = self.lock_inner();
        let db = inner.db(self.db_path(), false)?;
        db::live_texts(db.conn(), target.kind())
            .ok()
            .map(|entries| entries.join(crate::ENTRY_DELIMITER))
    }

    /// Why `memory.db` is not in use (or was replaced), if it isn't; `None` when all is well.
    pub fn database_problem(&self) -> Option<String> {
        let mut inner = self.lock_inner();
        let _ = inner.db(self.db_path(), false);
        inner.problem()
    }
}

#[cfg(test)]
impl MemoryStore {
    /// Arms a failpoint: the next time the store reaches it, it stops as a crash would.
    pub(crate) fn arm(&self, at: Failpoint) {
        *self.failpoint.lock() = Some(at);
    }
}
