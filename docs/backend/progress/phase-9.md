# Phase 9 progress

## CP9.1 — `memory.db`, migration from MEMORY.md / USER.md, export view, re-import of manual edits
Worktree: C:\Users\alexandros\repos\hermes-rs-p9   Branch: backend/p9-memory   Model: Opus 5.5 (subagent)
Cut from `main` at 3c182fa (Phases 0-8 merged). Uncommitted.
Done:
- **Inherited draft.** The worktree had two untracked, unreferenced files from an abandoned
  session (`crates/memory/src/db.rs`, `sync.rs`). Read and judged: the overall idea was sound
  (reads never write the files, a backup before the first migration, transaction + temp file +
  rename + commit) and parts were kept (schema shape, FTS5 triggers, the backup-file writer,
  several test scenarios), but both files were **rewritten**. Data-safety problems in the draft:
  a write loaded the entries in one transaction and saved them in another, so a second process
  writing in between was overwritten (lost entry); re-imports `DELETE`d rows (a file copy was
  kept, but rotated: only the newest 5 kept, i.e. our code deleted backups); a database whose
  hash matched but whose rows were empty or wrong was believed, and the next write exported that
  over the file (shrinking it); `db_path` was the memory folder's parent, which for tests on a
  temp root is the shared `%TEMP%`; it referenced store functions that did not exist.
- **`db.rs`** (`memory.db`, schema `user_version` 1): `entry(id, tier, scope, kind, text, tags,
  links, source_session, created, last_confirmed, hits, superseded_by` + `position, removed,
  removed_why)`, CHECKs on tier (`core|project|archive`) and kind (`user|fact`), FTS5 `entry_fts`
  (`porter unicode61`, external content, triggers), `meta(key, value)`. Opening: WAL (set once),
  busy timeout 5 s, a newer `user_version` → refused, never written (`OpenError::Newer`); not a
  database / damaged / `quick_check` not ok → `Corrupt`; a database with foreign tables and no
  version → refused; "busy" while another process creates it → retried up to 5 s. `set_core`
  makes the live `core` rows of a kind exactly a list: kept rows keep their id; a text that comes
  back revives its old row; rows leaving the view get `removed` + `removed_why` (+
  `superseded_by` for a patch). **No `DELETE` anywhere.**
- **`sync.rs`**: the files as the exported view.
  - Every access compares the file with the database: in step = the hash recorded at our last
    export (`meta synced:<kind>`, SHA-256 of the bytes, or `absent`) **and** the parsed entries
    equal the live rows. Otherwise the file is taken over (`migration`, `manual-edit`,
    `file-deleted`, or `repair` when our hash matches but the rows don't). So the prompt is always
    built from exactly what the file parses to (byte-for-byte unchanged), and an emptied or
    damaged database can never be exported over the file.
  - Anything that changes the database runs under its write lock (`BEGIN IMMEDIATE`) and reads
    the file again inside it; the fast path (in step) takes no lock.
  - First migration of a file: `<file>.bak.pre-memory-db.<unix time>` (`create_new`, flushed, read
    back and compared), skipped while an identical copy exists, never overwritten or deleted; no
    copy → no import (the file alone answers). A missing file migrates as `absent`, no copy.
  - A read never writes a `.md` file.
  - A write (`store.rs` `mutate`): lock, re-read and sync, decide (the unchanged closures, now
    `FnMut`), record the new rows and hash, write a temp file (flushed), **re-read the real file**
    (changed since read → roll back, start again, 3 attempts; holds an entry the database
    doesn't know, live or removed → refuse), rename, commit. A crash before the rename changes
    nothing; after it, the file holds the write and the next access takes it over.
  - Database trouble → the files alone for that call (the old code path, now with the same
    re-read-before-rename check); "busy" past the timeout → the write is retried, not done
    without the lock. A damaged `memory.db` is moved aside with its `-wal`/`-shm` (all or none)
    to `memory.db.corrupt-<time>` and rebuilt from the files; if it can't be moved, or the schema
    is newer, the database is off until restart. `MemoryStore::database_problem()` says why.
- **`store.rs`**: public API unchanged (`entries`, `readable_entries`, `snapshot`, `usage`,
  `apply`, `resolve`, `edit_entry`, `validate_single`, `set_on_change`, `tool`…). New
  constructors: `open(dir, db_path, config)` (the runtime: `<data>/memories` +
  `<data>/memory.db`), `file_only(dir, config)` (old behaviour, the tests' reference); `new(dir)`
  now puts the database in `dir`. `readable_entries` / `entries` answer from the database when
  the file is unreadable after a migration (before: "unreadable"/empty); writes are still refused
  then. The `on_change` hook fires after every write as before, and also after a re-import of a
  hand edit (outside the store's lock now).
- **Off the async workers:** the `memory` tool (foreground and the agentic review's unattended
  one) now runs in `spawn_blocking` (`SessionTools::run_memory_tool`); the turn's failure
  counter is carried as a copy and merged back.
- `Runtime::new` uses `MemoryStore::open(.., data_dir/memory.db, ..)`.
Files: `crates/memory/{Cargo.toml, src/lib.rs, src/store.rs, src/db.rs, src/sync.rs,
src/sync_tests.rs}`, `crates/runtime/src/lib.rs`, `crates/tools/src/{lib.rs, review.rs}`,
`PORTING.md`, `GAPS.md`, `docs/ARCHITECTURE.md`, `docs/PITFALLS.md`, this note.
Tests: (all `CARGO_TARGET_DIR=target/claude`, temp folders only)
- `cargo build --workspace`: ok, no warnings.
- `cargo test -p hermes_memory -p hermes_tools -p hermes_store -p hermes_prompts -p hermes_llm`:
  green (hermes_memory 42 + 1 ignored helper; hermes_tools 243; store 12; prompts 23; llm 468+5).
  The 23 pre-existing `hermes_memory` tests pass **unmodified** (they now run against
  `<dir>/memory.db`); no other crate's test was changed.
- New memory tests (`db.rs` 8, `sync_tests.rs` 19): round trip on 16 file shapes (typical,
  one entry, trailing newline or not, CRLF lines, CRLF inside an entry, empty, whitespace only,
  empty entries, duplicates, padded entries, delimiter-like text, a lone delimiter, BOM, unicode,
  10k entries): same entries and **byte-identical snapshot** vs `file_only`, file untouched,
  backup = file, export = the old store's serialisation (= the file for tidy files), restart
  stable, a write + undo gives the old store's bytes; missing file; manual edit (move, remove,
  change, add, put back → old id; write after edit; restart); file deleted; removals and
  `superseded_by` from writes; change hook; crashes at `AfterDbCreate`, `AfterBackup`,
  `MidImport`, `MidExportTmp` (half-written temp), `AfterExportTmp`, `AfterExportRename`: files
  intact (or holding the complete write), next start converges and the write can be redone;
  backup made first, not repeated, never overwritten (new numbered copy after `memory.db` is
  deleted and the file changed); no backup → no migration; emptied database never shrinks the
  file; the export guard refuses dropping an unknown entry; a file changed under a write is not
  overwritten and the retry keeps both; unreadable file; newer schema left byte-identical;
  damaged database moved aside (bytes kept) and rebuilt; a folder in the way → files alone;
  two stores × 25 writes + a reader in threads; **two child processes + the parent** × 15 writes
  each (the test binary re-run as a child): nothing lost, file = database.
- `cargo test -p hermes_runtime`: lib 169, `--test turns` 194 (7 ignored) green; and
  `HERMES_TEST_PROFILE=balanced|requests|tokens cargo test -p hermes_runtime --test turns`: 194
  green each.
- `cargo test -p hermes-desktop`: lib 174, `tests/acp.rs` 68 green.
Request counts: unchanged (no request is made or removed; `turns` baselines pass unchanged).
Deviations from the plan:
- Extra columns `position` (file order), `removed`, `removed_why` (nothing is deleted).
- "The DB is the source of truth" holds for the data (the database is a superset: every entry
  ever seen, with ids and metadata); for the **content of `core`** in CP9.1 the file wins any
  disagreement, because the file is what the user edits and what the prompt was built from.
  CP9.2 (tiers that exist only in the database) builds on this; an export of non-file data is not
  needed until then.
- `MemoryStore::new(dir)` keeps `memory.db` in `dir` (not the parent) so tests on a temp root
  never share `%TEMP%/memory.db`; only the runtime uses the plan's `<data>/memory.db`.
For the reviewer:
- **Merge rule for entries removed from the file by hand** (decided, please confirm): marked
  `removed` (`manual-edit`, or `file-deleted` when the file is gone, which includes Settings ▸
  Memory ▸ reset), never deleted; a changed entry = old row removed + new row (no fuzzy pairing:
  a wrong pairing would have overwritten the old text). Consequence: a "reset" no longer erases
  the text from disk; it stays in `memory.db` as removed rows. If the user expects reset to erase,
  `reset_memory` should also purge the rows (not done; it would be the only hard delete).
- **File wins on disagreement** (see deviations): a non-atomic writer that truncates the file (an
  editor crashing mid-save) makes entries "removed" in the view, as before, but they stay in the
  database (recoverable, not yet surfaced anywhere).
- Writers that don't take the database's lock (an editor, an old GPUI build, a store whose
  database is off) are caught by the re-read just before the rename, not prevented: a write
  landing in the microseconds between that read and the rename is overwritten (the same window
  as before Phase 9, which had no cross-process lock at all).
- `snapshot()` / `create_session` still run synchronously where they did (on an async worker in
  the gateway's `session.create`), now including a SQLite read (and, after a hand edit, a short
  write transaction that may wait up to 5 s for another process). Pre-existing pattern (the same
  call already does `sessions.db` I/O); left to Phase 13c. The memory tool itself is fixed.
- The unreadable-file case now answers from the database (snapshot, Star map, index) instead of
  empty; writes are still refused. Intentional, but a behaviour change.
- Stale `.mem_*.tmp` files left by a crash mid-export are not cleaned up (as before).
- The "corrupt" path renames a damaged `memory.db` even if another process still has it open;
  on Windows the rename then fails and that process goes file-only (tested only single-process).
- Failpoints are `#[cfg(test)]` (`MemoryStore::arm`): a "crash" returns early without cleanup and
  the test drops the store. A real kill was simulated only via a child process in the
  concurrency test, not mid-write.
- Not verified live: no page call changed (no live check written); the user's real data was
  never touched.

### Review (Opus 5.5)
`main` had not moved (cut at 3c182fa); no merge. `/code-review` not run as a skill (subagent);
manual pass over db.rs, sync.rs, store.rs, the tools/runtime/desktop callers.

**Shrink audit (every path that writes MEMORY.md / USER.md):** only `mutate_once` → `replace_file`
(tool `apply`/`tool`/`unattended_tool`, `edit_entry`, staging apply, review apply, GPUI screen) and
now `MemoryStore::reset` (delete). Reads, migration, re-import and `export_view` never write a
`.md`. A write decides from `parse_entries(file)` exactly as before Phase 9 (the file wins; the
database can't feed an export: the "emptied database" test), writes under `BEGIN IMMEDIATE`, and
renames only if the file's hash is still the one it decided from. So a file can only lose what the
operation removes, as before. Unreadable / invalid UTF-8 → write refused, never lossy-converted.
Fallback (no usable database) = the old code path plus the hash re-check. All processes (desktop,
ACP, GPUI) build `Runtime::new` → one `<data>/memory.db`; `MemoryStore::new(dir)` is test-only.
Backup before the first migration: create-new, flushed, read back, deduplicated, never deleted;
backup failure → no import, file alone (test `no_backup_no_migration`). Crash failpoints checked:
each converges (tests).

**Findings and fixes:**
1. *Privacy (decision 1).* Soft-removal kept the text the user removed (reset, Star map delete,
   hand edit) in `memory.db` + FTS with no way to see it. Decided: **the user's removals erase**
   — `db::Removal::Purge` deletes the row, its older versions (`superseded_by` chain, removed rows
   with the same text) and merges the FTS index (`optimize`); `secure_delete = ON` for every
   connection; WAL `checkpoint(TRUNCATE)` after a purge (250 ms busy budget). Applies to
   `edit_entry` (Star map delete/rewrite) and hand edits of the file. **The agent's removals stay
   soft** (`store`, `superseded_by`: history for CP9.3), as does `repair`; "ask the agent to forget"
   is therefore kept until a reset (documented; GPUI's frozen Memory screen goes through `apply`).
2. *Reset.* Settings ▸ Memory ▸ reset deleted the files behind the store's back, leaving every
   entry as `file-deleted` rows. New `MemoryStore::reset(target)`: under the write lock, deletes
   every row of the kind, records `absent`, deletes the file, commits, checkpoints; file-only mode
   = the old delete. `maintenance::reset_memory` takes the store and the gateway runs it in
   `spawn_blocking`. Pre-migration backups are not touched; PITFALLS says they (and
   `.bak.dropped` copies, old drift snapshots) hold old text and are never deleted by us.
3. *A deleted / emptied file silently emptied core.* Now a take-over that leaves **no** entries
   while the database had some first writes `<file>.bak.dropped.<time>` (create-new, read back;
   rename back to restore). If that copy can't be made: nothing is taken over, reads answer from
   the file alone, and **writes are refused** (`Abort::Refused`) — otherwise a write on the empty
   file would hide the loss and the next take-over would purge without a copy. A hand edit that
   keeps ≥1 entry makes no copy (it may be someone taking a secret out).
4. *Busy at first open switched the database off for the process* (`Other` after 5 s). Now stays
   `Unopened`: that call uses the file alone, the next use retries.
5. *Windows rename* over a file a scanner holds failed at once: retried 5× (20–320 ms backoff)
   under the lock, temp removed on final failure.
6. The export guard now allows what the write decided from (`before`) as well as what it writes,
   so a purge can't trip it (it is belt-and-braces behind the hash check).

**Decision 2 (file wins):** sound for CP9.1 — core is exactly the file, prompt byte-identical
(round-trip test vs `file_only` on 17 shapes incl. BOM, CRLF, delimiters, 10k entries, and now NUL
bytes). Exports happen only on writes and write the old store's serialisation, so no churn without
an entry change (as before Phase 9, a write normalises a hand-formatted file). **Decision 3:** the
unlocked-writer window (re-read → rename) is the same as before Phase 9 and acceptable.
**Decision 4:** `create_session`'s synchronous `snapshot()` is pre-existing (Phase 13c); the fast
path takes no lock; worst case a hand edit triggers one short transaction (≤5 s wait, plus ≤250 ms
checkpoint after a purge). The memory tool's `spawn_blocking`: no deadlock — the change hook runs
after the store's lock is dropped in every path (incl. `reset`).

Left: agent removals soft (by design, documented); stale `.mem_*.tmp` after a crash mid-export not
cleaned (as before); move-aside of a damaged DB held open by another process fails → that process
goes file-only (safe). No live check (no page shape changed; reset's answer is unchanged).
New tests: `db.rs` purge erases the text and its older versions from disk (file bytes scanned),
`purge_kind`; `sync_tests` reset, Star-map delete erasing the patch chain, hand removal erased
(no copy), deleted/emptied file → one `.bak.dropped` copy, no copy → no take-over and writes
refused, NUL bytes in the round trip; desktop `memory_status_and_reset` goes through the store.
Tests (`CARGO_TARGET_DIR=target/claude`): hermes_memory 48 + 1 ignored helper, hermes_tools 243
(`process::tests::stdin_submit_and_kill` failed once under load in the full run, passed alone and
in a re-run; unrelated to memory), hermes_store 12, hermes_prompts 23, hermes_llm 468+5; runtime
lib 169; `--test turns` 194 (7 ignored) under default, balanced, requests, tokens; hermes-desktop
lib 174 + `tests/acp.rs` 68; `cargo build --workspace` ok, no warnings. The worktree's
`target/hermes-src` was a junction to the main checkout's (removed by its `cargo clean`); the
webapp was rebuilt into the worktree itself (`tools/node` junction to the main checkout's Node).
Request counts unchanged.
