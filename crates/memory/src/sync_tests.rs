//! Tests of `memory.db` against the two files (`sync.rs`): migration round trips, re-import of
//! manual edits, a simulated crash at every step, the export guard, schema and corruption
//! handling, and concurrent writers (threads and processes). Everything runs in temp folders.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use crate::store::{MemoryConfig, MemoryStore, Operation, Target, TurnFailures, parse_entries};
use crate::sync::Failpoint;
use crate::{ENTRY_DELIMITER, MemorySnapshot};

const MEMORY: &str =
    "Repo builds with cargo\n§\nTwo lines:\n- one\n- two\n§\nΕλληνικά, 日本語, naïve — “quotes” 🦀";
const USER: &str = "Name is Alex\n§\nPrefers Python analogies";

/// A data folder: `memories/` with the two files, `memory.db` beside it (the app's layout).
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("memories")).unwrap();
        Self { dir }
    }

    fn with_files() -> Self {
        let world = Self::new();
        world.write(Target::Memory, MEMORY.as_bytes());
        world.write(Target::User, USER.as_bytes());
        world
    }

    fn folder(&self) -> PathBuf {
        self.dir.path().join("memories")
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("memory.db")
    }

    fn config() -> MemoryConfig {
        MemoryConfig {
            memory_char_limit: 1_000_000,
            user_char_limit: 1_000_000,
            ..Default::default()
        }
    }

    /// A store on this data folder: each call is "the app started again".
    fn store(&self) -> MemoryStore {
        MemoryStore::open(self.folder(), self.db(), Self::config())
    }

    /// How the store worked before Phase 9 (the reference for "unchanged").
    fn legacy(&self) -> MemoryStore {
        MemoryStore::file_only(self.folder(), Self::config())
    }

    fn file(&self, target: Target) -> PathBuf {
        self.folder().join(target.file_name())
    }

    fn write(&self, target: Target, bytes: &[u8]) {
        std::fs::write(self.file(target), bytes).unwrap();
    }

    fn bytes(&self, target: Target) -> Option<Vec<u8>> {
        std::fs::read(self.file(target)).ok()
    }

    fn text(&self, target: Target) -> String {
        String::from_utf8(self.bytes(target).unwrap()).unwrap()
    }

    /// The names in the memory folder containing `part`, sorted.
    fn named(&self, part: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.folder())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(part))
            .collect();
        names.sort();
        names
    }

    fn backups(&self, target: Target) -> Vec<Vec<u8>> {
        self.named(&format!("{}.bak.pre-memory-db.", target.file_name()))
            .iter()
            .map(|name| std::fs::read(self.folder().join(name)).unwrap())
            .collect()
    }

    /// Another connection to `memory.db`, as another process would have.
    fn sql(&self) -> Connection {
        let conn = Connection::open(self.db()).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        conn
    }

    /// `(text, removed_why)` of every row of `kind`, by id.
    fn rows(&self, kind: &str) -> Vec<(String, Option<String>)> {
        let conn = self.sql();
        let mut statement = conn
            .prepare("SELECT text, removed_why FROM entry WHERE kind = ?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn live_ids(&self, kind: &str) -> Vec<(i64, String)> {
        let conn = self.sql();
        let mut statement = conn
            .prepare(
                "SELECT id, text FROM entry WHERE kind = ?1 AND removed IS NULL ORDER BY position, id",
            )
            .unwrap();
        statement
            .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

fn op(action: &str, content: Option<&str>, old_text: Option<&str>) -> Operation {
    Operation {
        action: action.into(),
        content: content.map(Into::into),
        old_text: old_text.map(Into::into),
        matched_entry: None,
    }
}

fn apply(store: &MemoryStore, target: Target, ops: &[Operation]) -> Value {
    let result = store.apply(target, ops, &mut TurnFailures::default());
    assert_eq!(result["success"], true, "{result}");
    result
}

fn snapshot(store: &MemoryStore) -> MemorySnapshot {
    store.snapshot()
}

/// The file contents the round trip is checked on: typical and odd.
fn samples() -> Vec<(&'static str, String)> {
    let many: Vec<String> = (0..10_000)
        .map(|i| {
            format!(
                "Entry {i}: the build of crate {} uses feature {}",
                i % 97,
                i % 13
            )
        })
        .collect();
    vec![
        ("typical", MEMORY.to_string()),
        ("one entry", "Just one".to_string()),
        ("trailing newline", format!("{USER}\n")),
        ("no trailing newline", USER.to_string()),
        (
            "crlf lines",
            "First\r\nstill first\r\n§\r\nsecond\r\n".to_string(),
        ),
        ("crlf inside an entry", "a\r\nb\n§\nc".to_string()),
        ("empty file", String::new()),
        ("only whitespace", "  \n\n \t\n".to_string()),
        ("empty entries", "one\n§\n\n§\n\n§\ntwo".to_string()),
        (
            "duplicates",
            "same\n§\nother\n§\nsame\n§\nother".to_string(),
        ),
        ("padded entries", "  padded  \n§\n\ttabbed\t".to_string()),
        (
            "delimiter-like text",
            "a § b\n§\n§ at the start\n§\nends with §\n§\nx\n§ y\n§\n§§".to_string(),
        ),
        ("only a delimiter", "\n§\n".to_string()),
        ("bom", format!("\u{feff}{USER}")),
        (
            "unicode",
            "Ελληνικά\n§\n日本語のメモ\n§\nemoji 🦀🔥\n§\nzero\u{200b}width".to_string(),
        ),
        ("10k entries", many.join(ENTRY_DELIMITER)),
        ("nul bytes", "before\0after\n§\n\0\n§\nplain".to_string()),
    ]
}

#[test]
fn migration_round_trips_typical_and_odd_files() {
    for (name, content) in samples() {
        let world = World::new();
        world.write(Target::Memory, content.as_bytes());
        world.write(Target::User, USER.as_bytes());
        let legacy = world.legacy();
        let (before_entries, before_snapshot) = (legacy.entries(Target::Memory), snapshot(&legacy));
        let store = world.store();
        // md → db: the same entries, the same prompt blocks, byte for byte.
        assert_eq!(store.entries(Target::Memory), before_entries, "{name}");
        assert_eq!(snapshot(&store), before_snapshot, "{name}");
        assert_eq!(
            world
                .live_ids("fact")
                .into_iter()
                .map(|(_, t)| t)
                .collect::<Vec<_>>(),
            before_entries,
            "{name}"
        );
        // Migrating wrote no file, and the backup is the file.
        assert_eq!(
            world.bytes(Target::Memory).unwrap(),
            content.as_bytes(),
            "{name}"
        );
        assert_eq!(
            world.backups(Target::Memory),
            [content.as_bytes()],
            "{name}"
        );
        // db → md: what an export writes is what the store wrote before Phase 9 (the parsed
        // entries joined), which for a tidy file is the file itself.
        let export = store.export_view(Target::Memory).unwrap();
        assert_eq!(export, before_entries.join(ENTRY_DELIMITER), "{name}");
        // (A Notepad BOM is not text: the old store dropped it on its first write too.)
        let tidy = parse_entries(&content).join(ENTRY_DELIMITER) == content;
        if tidy && !content.starts_with('\u{feff}') {
            assert_eq!(export.as_bytes(), content.as_bytes(), "{name}");
        }
        // A restart changes nothing and makes no second backup.
        drop(store);
        let store = world.store();
        assert_eq!(snapshot(&store), before_snapshot, "{name}");
        assert_eq!(
            world.bytes(Target::Memory).unwrap(),
            content.as_bytes(),
            "{name}"
        );
        assert_eq!(world.backups(Target::Memory).len(), 1, "{name}");
        // A real export (a write, then its undo) gives exactly what the old store would have.
        apply(
            &store,
            Target::Memory,
            &[op("add", Some("temporary"), None)],
        );
        legacy_apply_matches(&world, name);
        apply(
            &store,
            Target::Memory,
            &[op("remove", None, Some("temporary"))],
        );
        let expected = if before_entries.is_empty() {
            // Removing the only entry leaves an empty file, as before.
            String::new()
        } else {
            before_entries.join(ENTRY_DELIMITER)
        };
        assert_eq!(world.text(Target::Memory), expected, "{name}");
        assert_eq!(
            store.export_view(Target::Memory).unwrap(),
            expected,
            "{name}"
        );
        // USER.md was never written.
        assert_eq!(
            world.bytes(Target::User).unwrap(),
            USER.as_bytes(),
            "{name}"
        );
    }
}

/// The file after a store write is what the store before Phase 9 would have written.
fn legacy_apply_matches(world: &World, name: &str) {
    let now = world.text(Target::Memory);
    let entries = parse_entries(&now);
    assert_eq!(
        entries.last().map(String::as_str),
        Some("temporary"),
        "{name}"
    );
    assert_eq!(now, entries.join(ENTRY_DELIMITER), "{name}");
}

#[test]
fn a_missing_file_is_migrated_without_a_backup_and_not_created() {
    let world = World::new();
    let store = world.store();
    assert!(store.entries(Target::User).is_empty());
    assert_eq!(snapshot(&store), MemorySnapshot::default());
    assert_eq!(world.bytes(Target::User), None);
    assert!(world.named(".bak.").is_empty());
    assert_eq!(store.export_view(Target::User).unwrap(), "");
    apply(&store, Target::User, &[op("add", Some("First"), None)]);
    assert_eq!(world.text(Target::User), "First");
}

#[test]
fn a_manual_edit_is_taken_over_entry_by_entry() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::Memory);
    let before = world.live_ids("fact");
    // By hand: the third moved first, the second removed, the first changed, one added.
    let edited = "Ελληνικά, 日本語, naïve — “quotes” 🦀\n§\nRepo builds with cargo and make\n§\nWritten in Notepad\n";
    world.write(Target::Memory, edited.as_bytes());
    let expected = [
        "Ελληνικά, 日本語, naïve — “quotes” 🦀",
        "Repo builds with cargo and make",
        "Written in Notepad",
    ];
    assert_eq!(store.entries(Target::Memory), expected);
    // Reading never writes the file.
    assert_eq!(world.bytes(Target::Memory).unwrap(), edited.as_bytes());
    let after = world.live_ids("fact");
    // Moved: the same row.
    assert_eq!(after[0], before[2]);
    // Changed and added: new rows.
    assert!(
        before
            .iter()
            .all(|(id, _)| *id != after[1].0 && *id != after[2].0)
    );
    // Removed and the old text of the changed one: the user took them out, so they are erased
    // from the database too (as they were gone for good before Phase 9), not kept.
    let rows = world.rows("fact");
    for gone in ["Repo builds with cargo", "Two lines:\n- one\n- two"] {
        assert!(!rows.iter().any(|(text, _)| text == gone), "{gone}: {rows:?}");
    }
    // Not every entry left, so no `.bak.dropped` copy.
    assert!(world.named(".bak.dropped.").is_empty());
    // The prompt is what the file says.
    assert_eq!(snapshot(&store), snapshot(&world.legacy()));
    // A removed entry put back is a new row.
    world.write(
        Target::Memory,
        format!("{edited}\n§\nTwo lines:\n- one\n- two").as_bytes(),
    );
    assert_eq!(store.entries(Target::Memory).len(), 4);
    assert!(before.iter().all(|(id, _)| *id != world.live_ids("fact")[3].0));
    // The next write builds on the edit, and a restart sees the same.
    apply(&store, Target::Memory, &[op("add", Some("After"), None)]);
    let file = world.text(Target::Memory);
    assert_eq!(parse_entries(&file).len(), 5);
    assert!(file.ends_with("\n§\nAfter"));
    assert_eq!(world.store().export_view(Target::Memory).unwrap(), file);
    // The other file was not touched.
    assert_eq!(world.bytes(Target::User).unwrap(), USER.as_bytes());
}

#[test]
fn a_deleted_file_empties_the_view_and_leaves_a_copy_of_its_entries() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    apply(&store, Target::User, &[op("add", Some("Added later"), None)]);
    let last = world.text(Target::User);
    // Deleted outside the app (by hand, an older build's reset).
    std::fs::remove_file(world.file(Target::User)).unwrap();
    assert!(store.entries(Target::User).is_empty());
    assert_eq!(snapshot(&store).user, None);
    assert_eq!(world.bytes(Target::User), None, "a read never recreates it");
    // Every entry left at once: more likely an accident, so they are copied next to the file
    // (restorable by renaming the copy back), then erased from the database.
    let copies = world.named("USER.md.bak.dropped.");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(world.folder().join(&copies[0])).unwrap(),
        last
    );
    assert!(world.rows("user").is_empty(), "{:?}", world.rows("user"));
    // Memory starts again from nothing; no second copy.
    apply(&store, Target::User, &[op("add", Some("New start"), None)]);
    assert_eq!(world.text(Target::User), "New start");
    store.entries(Target::User);
    assert_eq!(world.named("USER.md.bak.dropped.").len(), 1);
    // An emptied file (an editor that crashed mid-save) is the same case.
    world.write(Target::User, b"");
    assert!(store.entries(Target::User).is_empty());
    assert_eq!(world.named("USER.md.bak.dropped.").len(), 2);
}

#[test]
fn without_a_copy_a_file_that_lost_everything_is_not_taken_over() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    std::fs::remove_file(world.file(Target::User)).unwrap();
    store.arm(Failpoint::BackupFails);
    // The copy can't be made: the file alone answers, the database keeps the entries.
    assert!(store.entries(Target::User).is_empty());
    assert_eq!(world.live_ids("user").len(), 2);
    // A write would build on the empty file and hide the loss: refused while that holds.
    store.arm(Failpoint::BackupFails);
    let refused = store.apply(
        Target::User,
        &[op("add", Some("x"), None)],
        &mut TurnFailures::default(),
    );
    assert_eq!(refused["success"], false, "{refused}");
    assert_eq!(world.bytes(Target::User), None);
    assert_eq!(world.live_ids("user").len(), 2);
    // Once it can, the copy is made and the deletion is taken over.
    assert!(store.entries(Target::User).is_empty());
    assert_eq!(world.named("USER.md.bak.dropped.").len(), 1);
    assert!(world.rows("user").is_empty());
}

#[test]
fn reset_erases_the_file_and_every_row_of_its_kind() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    apply(
        &store,
        Target::User,
        &[op("patch", Some("token sk-secret-bravo"), Some("Prefers Python analogies"))],
    );
    apply(
        &store,
        Target::User,
        &[op("patch", Some("token sk-secret-charlie"), Some("token sk-secret-bravo"))],
    );
    // The agent's rewrites are kept as removed rows until the user erases them.
    assert!(world.rows("user").iter().any(|(t, _)| t.contains("sk-secret-bravo")));
    store.entries(Target::Memory);
    assert_eq!(store.reset(Target::User), Ok(true));
    assert_eq!(world.bytes(Target::User), None);
    assert!(world.rows("user").is_empty());
    assert!(!world.rows("fact").is_empty(), "the other file is not touched");
    assert_eq!(world.bytes(Target::Memory).unwrap(), MEMORY.as_bytes());
    // Not in the database's files either (secure_delete, FTS merged, WAL checkpointed).
    drop(store);
    for name in ["memory.db", "memory.db-wal"] {
        let bytes = std::fs::read(world.dir.path().join(name)).unwrap_or_default();
        assert!(
            !bytes.windows(9).any(|w| w == b"sk-secret"),
            "{name} still holds the text"
        );
    }
    // No `.bak.dropped` copy: the reset was the user's, through the app.
    let store = world.store();
    assert!(store.entries(Target::User).is_empty());
    assert!(world.named(".bak.dropped.").is_empty());
    // The pre-migration backup is the user's and stays (PITFALLS.md).
    assert_eq!(world.backups(Target::User), [USER.as_bytes()]);
    // A second reset has nothing to delete; memory starts again.
    assert_eq!(store.reset(Target::User), Ok(false));
    apply(&store, Target::User, &[op("add", Some("New start"), None)]);
    assert_eq!(world.text(Target::User), "New start");
}

#[test]
fn the_users_own_delete_erases_the_text_and_its_older_versions() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    apply(
        &store,
        Target::User,
        &[op("patch", Some("password zq-alpha-one"), Some("Prefers Python analogies"))],
    );
    apply(
        &store,
        Target::User,
        &[op("patch", Some("zq-alpha-two"), Some("zq-alpha-one"))],
    );
    let deleted = store.edit_entry(Target::User, "password zq-alpha-two", None);
    assert_eq!(deleted["success"], true, "{deleted}");
    assert_eq!(world.text(Target::User), "Name is Alex");
    let rows = world.rows("user");
    assert!(
        !rows.iter().any(|(t, _)| t.contains("zq-alpha") || t.contains("Python")),
        "{rows:?}"
    );
    drop(store);
    for name in ["memory.db", "memory.db-wal"] {
        let bytes = std::fs::read(world.dir.path().join(name)).unwrap_or_default();
        assert!(!bytes.windows(8).any(|w| w == b"zq-alpha"), "{name}");
    }
}

#[test]
fn a_hand_removed_entry_is_erased_from_the_database() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    world.write(Target::User, b"Name is Alex");
    assert_eq!(store.entries(Target::User), ["Name is Alex"]);
    assert_eq!(
        world.rows("user"),
        [("Name is Alex".to_string(), None)],
        "the hand-removed entry is not kept"
    );
    assert!(world.named(".bak.dropped.").is_empty());
    // Rewriting the only entry by hand (say, to take a password out) leaves no copy either.
    world.write(Target::User, b"Name is Alexandros");
    assert_eq!(store.entries(Target::User), ["Name is Alexandros"]);
    assert_eq!(world.rows("user"), [("Name is Alexandros".to_string(), None)]);
    assert!(world.named(".bak.dropped.").is_empty());
}

#[test]
fn writes_record_removals_and_replacements() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    let old_id = world.live_ids("user")[1].0;
    apply(
        &store,
        Target::User,
        &[op("patch", Some("C++"), Some("Python"))],
    );
    let new_id = world.live_ids("user")[1].0;
    let successor: Option<i64> = world
        .sql()
        .query_row(
            "SELECT superseded_by FROM entry WHERE id = ?1",
            [old_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(successor, Some(new_id));
    apply(&store, Target::User, &[op("remove", None, Some("Name is"))]);
    assert_eq!(world.text(Target::User), "Prefers C++ analogies");
    assert!(
        world
            .rows("user")
            .iter()
            .any(|(text, why)| text == "Name is Alex" && why.as_deref() == Some("store"))
    );
    // The Star map's edit goes the same way.
    let edited = store.edit_entry(Target::User, "Prefers C++ analogies", Some("Prefers Rust"));
    assert_eq!(edited["success"], true, "{edited}");
    assert_eq!(world.text(Target::User), "Prefers Rust");
    // Writes through the store are not manual edits.
    assert!(
        !world
            .rows("user")
            .iter()
            .any(|(_, why)| why.as_deref() == Some("manual-edit"))
    );
}

#[test]
fn the_change_hook_fires_on_writes_and_on_re_imports() {
    let world = World::with_files();
    let store = world.store();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    store.set_on_change(std::sync::Arc::new(move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let fired = || count.load(std::sync::atomic::Ordering::SeqCst);
    store.entries(Target::Memory);
    let after_migration = fired();
    store.entries(Target::Memory);
    assert_eq!(fired(), after_migration, "a read in step fires nothing");
    apply(&store, Target::Memory, &[op("add", Some("x"), None)]);
    assert_eq!(fired(), after_migration + 1);
    world.write(Target::Memory, b"edited by hand");
    store.entries(Target::Memory);
    assert_eq!(fired(), after_migration + 2);
}

// ---------------------------------------------------------------------------------------------
// Crashes

/// The two files are as they were, and the next start answers from them.
fn assert_intact_and_converges(world: &World) {
    assert_eq!(world.bytes(Target::Memory).unwrap(), MEMORY.as_bytes());
    assert_eq!(world.bytes(Target::User).unwrap(), USER.as_bytes());
    let store = world.store();
    assert_eq!(snapshot(&store), snapshot(&world.legacy()));
    assert_eq!(store.export_view(Target::Memory).unwrap(), MEMORY);
    assert_eq!(store.export_view(Target::User).unwrap(), USER);
    assert_eq!(world.backups(Target::Memory), [MEMORY.as_bytes()]);
    assert_eq!(world.backups(Target::User), [USER.as_bytes()]);
    // And it keeps working.
    apply(&store, Target::User, &[op("add", Some("Third"), None)]);
    assert_eq!(world.text(Target::User), format!("{USER}\n§\nThird"));
}

#[test]
fn a_crash_at_any_step_of_the_migration_leaves_the_files_intact() {
    for at in [
        Failpoint::AfterDbCreate,
        Failpoint::AfterBackup,
        Failpoint::MidImport,
    ] {
        let world = World::with_files();
        let store = world.store();
        store.arm(at);
        // The first access hits the failpoint: the process "dies" here.
        let _ = store.entries(Target::Memory);
        drop(store);
        if at != Failpoint::AfterDbCreate {
            // Nothing half-imported stayed behind.
            assert!(world.rows("fact").is_empty(), "{at:?}");
        }
        assert_intact_and_converges(&world);
    }
}

#[test]
fn a_crash_at_any_step_of_a_write_loses_nothing() {
    for at in [
        Failpoint::MidExportTmp,
        Failpoint::AfterExportTmp,
        Failpoint::AfterExportRename,
    ] {
        let world = World::with_files();
        let store = world.store();
        store.entries(Target::Memory);
        store.arm(at);
        let crashed = store.apply(
            Target::Memory,
            &[op("add", Some("Fourth"), None)],
            &mut TurnFailures::default(),
        );
        assert_eq!(crashed["success"], false, "{at:?}");
        drop(store);
        let with_fourth = format!("{MEMORY}\n§\nFourth");
        let expected = if at == Failpoint::AfterExportRename {
            // The file holds the write; the database never committed it.
            with_fourth.clone()
        } else {
            // The real file was never touched (a half-written temporary file may be left).
            MEMORY.to_string()
        };
        assert_eq!(world.text(Target::Memory), expected, "{at:?}");
        // The next start takes the file as it is, and the two agree again.
        let store = world.store();
        assert_eq!(
            store.entries(Target::Memory),
            parse_entries(&expected),
            "{at:?}"
        );
        assert_eq!(
            store.export_view(Target::Memory).unwrap(),
            expected,
            "{at:?}"
        );
        assert_eq!(world.text(Target::Memory), expected, "{at:?}");
        assert!(
            world.rows("fact").iter().all(|(_, why)| why.is_none()),
            "nothing was lost on the way: {:?}",
            world.rows("fact")
        );
        // And the write can be made again.
        apply(&store, Target::Memory, &[op("add", Some("Fourth"), None)]);
        assert_eq!(world.text(Target::Memory), with_fourth, "{at:?}");
        assert_eq!(world.bytes(Target::User).unwrap(), USER.as_bytes());
    }
}

#[test]
fn the_pre_migration_backup_is_made_first_and_never_overwritten() {
    let world = World::with_files();
    let store = world.store();
    store.arm(Failpoint::AfterBackup);
    let _ = store.entries(Target::Memory);
    drop(store);
    // The copy exists before anything was imported.
    assert_eq!(world.backups(Target::Memory), [MEMORY.as_bytes()]);
    assert!(world.rows("fact").is_empty());
    // Retried: no second copy of the same bytes.
    world.store().entries(Target::Memory);
    assert_eq!(world.backups(Target::Memory), [MEMORY.as_bytes()]);
    // memory.db deleted and the file edited: a new first migration, a new copy; the old one
    // stays as it was.
    std::fs::remove_file(world.db()).unwrap();
    let _ = std::fs::remove_file(world.dir.path().join("memory.db-wal"));
    let _ = std::fs::remove_file(world.dir.path().join("memory.db-shm"));
    world.write(Target::Memory, b"changed since");
    assert_eq!(world.store().entries(Target::Memory), ["changed since"]);
    let mut backups = world.backups(Target::Memory);
    backups.sort();
    let mut expected = vec![MEMORY.as_bytes().to_vec(), b"changed since".to_vec()];
    expected.sort();
    assert_eq!(backups, expected);
}

#[test]
fn no_backup_no_migration() {
    let world = World::with_files();
    let store = world.store();
    store.arm(Failpoint::BackupFails);
    // The copy can't be made: nothing is imported, the file alone answers.
    assert_eq!(store.entries(Target::Memory), parse_entries(MEMORY));
    assert!(world.rows("fact").is_empty());
    assert!(world.backups(Target::Memory).is_empty());
    assert_eq!(world.bytes(Target::Memory).unwrap(), MEMORY.as_bytes());
    // The next access makes the copy, then migrates.
    assert_eq!(store.entries(Target::Memory), parse_entries(MEMORY));
    assert_eq!(world.backups(Target::Memory), [MEMORY.as_bytes()]);
    assert_eq!(world.live_ids("fact").len(), 3);
}

// ---------------------------------------------------------------------------------------------
// The export guard

#[test]
fn an_emptied_database_never_shrinks_the_file() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::Memory);
    // The rows vanish behind the store's back (a bug, a tool, a bad restore); the hash still
    // says "in step".
    world.sql().execute("DELETE FROM entry", []).unwrap();
    // A read answers from the file, and repairs the database.
    assert_eq!(store.entries(Target::Memory), parse_entries(MEMORY));
    world.sql().execute("DELETE FROM entry", []).unwrap();
    // A write builds on the file, not on the empty database.
    apply(&store, Target::Memory, &[op("add", Some("Fourth"), None)]);
    assert_eq!(world.text(Target::Memory), format!("{MEMORY}\n§\nFourth"));
}

#[test]
fn an_export_that_would_drop_an_unknown_entry_is_refused() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::Memory);
    let seen = crate::sync::hash(MEMORY.as_bytes());
    // The database lost one entry outright, and an export without it is attempted.
    world
        .sql()
        .execute(
            "DELETE FROM entry WHERE text = 'Repo builds with cargo'",
            [],
        )
        .unwrap();
    let rest: Vec<String> = parse_entries(MEMORY)[1..].to_vec();
    let content = rest.join(ENTRY_DELIMITER);
    let mut inner = store.lock_inner();
    let db = inner.db(store.db_path(), false).unwrap();
    let tx = db.begin().unwrap();
    let outcome = store
        .replace_file(Some(&tx), Target::Memory, &seen, &content, &rest)
        .ok()
        .unwrap();
    assert!(outcome.unwrap_err().contains("not saved in memory.db"));
    drop(tx);
    drop(inner);
    assert_eq!(world.text(Target::Memory), MEMORY);
    assert!(world.named(".tmp").is_empty());
}

#[test]
fn a_file_changed_during_a_write_is_not_overwritten() {
    let world = World::with_files();
    let store = world.store();
    store.entries(Target::User);
    let seen = crate::sync::hash(USER.as_bytes());
    // Another program writes the file between the store's read and its rename.
    world.write(
        Target::User,
        format!("{USER}\n§\nFrom elsewhere").as_bytes(),
    );
    let texts = vec!["only".to_string()];
    let outcome = store.replace_file(None, Target::User, &seen, "only", &texts);
    assert!(matches!(outcome, Err(crate::sync::Abort::Conflict)));
    assert_eq!(
        world.text(Target::User),
        format!("{USER}\n§\nFrom elsewhere")
    );
    // Through the store the write starts again from the new file and keeps both.
    apply(&store, Target::User, &[op("add", Some("Mine"), None)]);
    assert_eq!(
        world.text(Target::User),
        format!("{USER}\n§\nFrom elsewhere\n§\nMine")
    );
}

// ---------------------------------------------------------------------------------------------
// Unreadable files, newer and damaged databases

#[test]
fn an_unreadable_file_is_neither_migrated_nor_written() {
    let world = World::new();
    let garbage = [0xff, 0xfe, 0x00, 0x41];
    world.write(Target::Memory, &garbage);
    let store = world.store();
    assert!(store.entries(Target::Memory).is_empty());
    assert_eq!(store.readable_entries(Target::Memory), None);
    let refused = store.apply(
        Target::Memory,
        &[op("add", Some("x"), None)],
        &mut TurnFailures::default(),
    );
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .starts_with("Refusing to write MEMORY.md")
    );
    assert_eq!(world.bytes(Target::Memory).unwrap(), garbage);
    assert!(world.backups(Target::Memory).is_empty());
    // Once it reads it is migrated like any file.
    world.write(Target::Memory, b"readable now");
    assert_eq!(store.entries(Target::Memory), ["readable now"]);
    // Unreadable after the migration: the database answers; writes still wait.
    world.write(Target::Memory, &garbage);
    assert_eq!(store.entries(Target::Memory), ["readable now"]);
    assert_eq!(
        store.readable_entries(Target::Memory),
        Some(vec!["readable now".to_string()])
    );
    let refused = store.apply(
        Target::Memory,
        &[op("add", Some("x"), None)],
        &mut TurnFailures::default(),
    );
    assert_eq!(refused["success"], false);
    assert_eq!(world.bytes(Target::Memory).unwrap(), garbage);
}

#[test]
fn a_newer_database_is_left_alone_and_the_files_work_as_before() {
    let world = World::with_files();
    world.store().entries(Target::User);
    world.sql().pragma_update(None, "user_version", 7).unwrap();
    let rows_before = world.rows("user");
    let store = world.store();
    assert_eq!(store.entries(Target::User), parse_entries(USER));
    apply(&store, Target::User, &[op("add", Some("Third"), None)]);
    assert_eq!(world.text(Target::User), format!("{USER}\n§\nThird"));
    assert!(store.database_problem().unwrap().contains("newer"));
    assert_eq!(store.export_view(Target::User), None);
    // Not written, not downgraded.
    let version: i64 = world
        .sql()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 7);
    assert_eq!(world.rows("user"), rows_before);
}

#[test]
fn a_damaged_database_is_moved_aside_and_rebuilt_from_the_files() {
    let world = World::with_files();
    let garbage = b"definitely not SQLite ".repeat(200);
    std::fs::write(world.db(), &garbage).unwrap();
    let store = world.store();
    assert_eq!(store.entries(Target::Memory), parse_entries(MEMORY));
    assert!(store.database_problem().unwrap().contains("moved aside"));
    let aside: Vec<PathBuf> = std::fs::read_dir(world.dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains("memory.db.corrupt-"))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read(&aside[0]).unwrap(), garbage);
    assert_eq!(store.export_view(Target::Memory).unwrap(), MEMORY);
    assert_eq!(world.bytes(Target::Memory).unwrap(), MEMORY.as_bytes());
}

#[test]
fn without_a_usable_database_the_files_work_as_before() {
    let world = World::with_files();
    // A folder where memory.db should be: it can't be opened or moved.
    std::fs::create_dir(world.db()).unwrap();
    let store = world.store();
    assert_eq!(store.entries(Target::User), parse_entries(USER));
    assert_eq!(store.export_view(Target::User), None);
    apply(&store, Target::User, &[op("add", Some("Third"), None)]);
    assert_eq!(world.text(Target::User), format!("{USER}\n§\nThird"));
    assert_eq!(world.bytes(Target::Memory).unwrap(), MEMORY.as_bytes());
    assert!(world.named(".bak.").is_empty(), "no migration, no backup");
}

// ---------------------------------------------------------------------------------------------
// Concurrency

#[test]
fn two_stores_writing_at_once_lose_nothing() {
    let world = World::with_files();
    let per_writer = 25;
    std::thread::scope(|scope| {
        for writer in ["a", "b"] {
            let store = world.store();
            scope.spawn(move || {
                for i in 0..per_writer {
                    apply(
                        &store,
                        Target::Memory,
                        &[op("add", Some(&format!("from {writer} #{i}")), None)],
                    );
                }
            });
        }
        let reader = world.store();
        scope.spawn(move || {
            for _ in 0..50 {
                let entries = reader.entries(Target::Memory);
                assert!(entries.len() >= 3, "a reader never sees memory shrink");
            }
        });
    });
    let entries = world.store().entries(Target::Memory);
    assert_eq!(entries.len(), 3 + 2 * per_writer);
    for writer in ["a", "b"] {
        for i in 0..per_writer {
            assert!(entries.contains(&format!("from {writer} #{i}")));
        }
    }
    assert_eq!(world.text(Target::Memory), entries.join(ENTRY_DELIMITER));
    assert!(world.rows("fact").iter().all(|(_, why)| why.is_none()));
}

/// The child half of [`two_processes_writing_at_once_lose_nothing`]: does nothing unless the
/// parent started this test binary with `HERMES_MEMORY_TEST_CHILD`.
#[test]
#[ignore = "run by two_processes_writing_at_once_lose_nothing"]
fn child_process_writer() {
    let Ok(spec) = std::env::var("HERMES_MEMORY_TEST_CHILD") else {
        return;
    };
    let (data, name) = spec.split_once('|').unwrap();
    let data = Path::new(data);
    let store = MemoryStore::open(
        data.join("memories"),
        data.join("memory.db"),
        World::config(),
    );
    for i in 0..15 {
        apply(
            &store,
            Target::Memory,
            &[op("add", Some(&format!("from {name} #{i}")), None)],
        );
    }
}

#[test]
fn two_processes_writing_at_once_lose_nothing() {
    let world = World::with_files();
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = ["p1", "p2"]
        .iter()
        .map(|name| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "sync_tests::child_process_writer",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env(
                    "HERMES_MEMORY_TEST_CHILD",
                    format!("{}|{name}", world.dir.path().display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let store = world.store();
    for i in 0..15 {
        apply(
            &store,
            Target::Memory,
            &[op("add", Some(&format!("from parent #{i}")), None)],
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let entries = store.entries(Target::Memory);
    for name in ["parent", "p1", "p2"] {
        for i in 0..15 {
            assert!(
                entries.contains(&format!("from {name} #{i}")),
                "from {name} #{i} missing: {entries:?}"
            );
        }
    }
    assert_eq!(entries.len(), 3 + 45);
    assert_eq!(world.text(Target::Memory), entries.join(ENTRY_DELIMITER));
}
