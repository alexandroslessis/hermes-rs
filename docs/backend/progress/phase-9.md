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

## CP9.2 — tiers and scopes, limits, notes, the memory tool's new fields
Worktree: C:\Users\alexandros\repos\hermes-rs-p9   Branch: backend/p9-memory   Model: Sonnet 5.5 (subagent)
Cut on top of CP9.1 (+ its review). Uncommitted.
Done:
- **Tiers.** `core` is untouched (the files, file wins, same limits and errors). `project` and
  `archive` live in `memory.db` only (`tiers.rs`; no schema change: the columns existed). The agent's
  batches in them go through the same planner as `core` (`store.rs` `batch`: matching, errors,
  all-or-nothing, threat scan) and are recorded by `MemoryStore::mutate_tier` (one transaction under
  the write lock; removed/rewritten rows are marked, never deleted; a patched row's successor takes
  its tags, links, created, hits). One call writes one tier; `note_read` stands alone.
- **Project scope** = `hermes_tools::project_skills::scope_key(find_root(cwd))` (Phase 8's root;
  worktree = own project, nested repo = own, subfolder = parent's root, no repo = no project tier,
  the call is refused, nothing falls back). The memory crate knows no paths: the tools crate passes
  the key (`SessionTools::memory_scope`, `tool_in`, `unattended_tool_in`, `stage_memory_call_in`);
  it is never read from model arguments and each staged operation carries it.
- **Prompt.** `snapshot_for(scope)` adds a project block (`PROJECT MEMORY (notes for this
  repository only) [..]`) read only for that scope; `PromptSource::build` appends it after the core
  blocks when the session starts (frozen with the prompt). Without project entries the prompt is
  byte-identical (turns test compares prompts before/after archive and other-repo writes).
  `context_breakdown` counts it as memory.
- **Limits.** `MemoryConfig::project_char_limit` (default 1500; `memory.project_char_limit` in
  config, validated to 200-20000 else default, shown in Settings), per project, both kinds together.
  Archive: no char limit; 1000 chars an entry, 5000 entries a target. Same error style (current
  entries, how much to prune).
- **Notes** (`notes.rs`): chosen design = `add`/`patch` take `note` (the body) and the new action
  `note_read` (old_text + tier) reads it; no `note_write`. File `memories/notes/<slug>.md`, link
  `file:notes/<slug>.md`; slug sanitising (`[a-z0-9-]`, 48 chars, device names, no empty), 20000-char
  cap, atomic temp+rename, never overwritten, threat-scanned, reads only via valid slug + regular
  file. Core has no notes. Model-supplied links into `notes/` are refused.
- **Deletion.** Agent removal keeps row and note (history). User removal (`delete_tier_entry`, and
  `reset`, which now erases every tier of its kind and their notes) purges rows, older versions,
  FTS and the note files.
- **Tool schema** (`prompts/tools/memory.json`): `tier`, `tags`, `links`, `note`, `note_read`, a
  TIERS paragraph. Old-shape calls: identical results/errors/files (test compares against
  `file_only` over 19 calls incl. errors). Tags (<=8, 32 chars) and links (<=8, kinds `skill:` `file:`
  `session:` `memory:`, 300 chars) validated; `file:` links stay strings. Core entries may carry
  tags/links (database only; a patch carries them over). Schema change = one cache miss (PITFALLS).
- **Staging/review.** `PendingWrite.extras` and the taint staging payload carry tier/scope/tags/
  links/note (absent for old shape, so old records serialize as before); the dry run applies limits
  and scan; the diff shows tier, tags, links, note. Phase 7 review ops are unchanged (core).
- `Operation` is unchanged (CP9.1 tests build it literally); extras are a parallel `Extras` list
  (`apply_ext`, `resolve_ext`, `parse_call_in`; `parse_call` removed, its one caller updated).
Files: `crates/memory/src/{tiers,tiers_tests,notes,db,store,tool,sync,lib}.rs`,
`crates/tools/src/{lib,staging,project_skills/tests}.rs`, `crates/runtime/src/lib.rs`,
`crates/runtime/tests/turns.rs`, `crates/desktop/src/{gateway,rest}.rs`, `prompts/tools/memory.json`,
`PORTING.md`, `GAPS.md`, `docs/{ARCHITECTURE,PITFALLS}.md`, this note.
Tests (`CARGO_TARGET_DIR=target/claude`): hermes_memory 78 (+1 ignored; CP9.1's 53 unmodified),
hermes_tools 245, store 12, prompts 23, llm 468+5, runtime lib 169, `--test turns` 196 (7 ignored)
under default/balanced/requests/tokens, hermes-desktop and `cargo build --workspace` (see final
report). New: 27 memory tests (scope isolation, limits, bounds, notes slugs/escapes/limits/
history, link validation, hard delete incl. bytes scan, reset, staging, unattended, old-shape
identity), 1 tools test (worktree/subfolder/nested/no-repo roots), 1 staging test, 2 turns tests
(core-only prompt byte-identical, project block only in its repo and frozen mid-session, 8 requests
for 4 turns).
Request counts: unchanged.
Deviations: `Operation` not extended (parallel `Extras`); `reset` erases all tiers of the kind;
`snapshot()` keeps its signature (new `snapshot_for`).
For the reviewer:
- A worktree is its own project (Phase 8's root): its memory is not shared with the main checkout.
  Confirm, or map worktrees to the common git dir.
- `reset` erasing project/archive entries and notes of that target: confirm it is what the user
  expects from "reset memory".
- Notes only for project/archive (core entries are in every prompt; a core note's link would be
  invisible). Confirm.
- Archive/project rows are not in `index.db` yet (global indexing would leak across repos); CP9.3
  must index with scope.
- `source_session` is not filled (the store doesn't know the session); CP9.3 if wanted.
- An agent-removed project entry keeps its text (and note) on disk until the user removes it
  through a path that purges; there is no page for non-core tiers yet (CP9.3 map).
- A crash between writing a note and the commit leaves an unlinked note file.
- Schema text grew ~700 chars: one cache miss on upgrade (PITFALLS).
- Not verified live: no page call changed; no `*-check.mjs` written.

### Review (Opus 5.5)
`main` had not moved (cut at 3c182fa + CP9.1 6ef3bda); no merge. `/code-review` not run as a skill
(subagent); manual pass over tiers.rs, notes.rs, tool.rs, store.rs, db.rs, sync.rs, the tools,
runtime and desktop callers.

**Old-shape calls (evidence).** A scratch workspace (outside the repo, same `target/claude`) built
the CP9.1 crate (`git archive 6ef3bda crates/memory`, renamed) next to this one and ran 55 calls
in sequence on both, in three modes (direct, `write_approval`, unattended review): add/patch/
remove/replace (batch and legacy single-op, `new_text`), both targets, not found, ambiguous,
missing/invalid/numeric target, `operations` as string/empty/number item/no action, unknown and
mis-cased actions, null action, numeric content/old_text, empty and blank content, a 100k-char
entry, unknown top-level and per-op fields, null/string/array args, duplicate add, threat text,
unicode, `§`, newlines, a mixed batch, overflow to the consolidation-failure cap, removing the
last entry. Compared: result JSON and error texts, MEMORY.md / USER.md bytes, snapshot blocks,
staged records. **Identical**, except (a) `note_read` (the new action; it is a read, so it no
longer counts as a failure) and (b) staged records of calls that use the new fields gain an
`extras` field. One regression found and fixed: `"tags": "misc"` (a string; the old code ignored
the field and wrote) was refused; `tags`/`links` given as a string are now read as a
comma-separated list, an empty string as none. Schema: valid JSON Schema, `strict: false` (our
provider), `additionalProperties: false` per item, enums only on strings; fine for llama.cpp
grammars and OpenAI-compatible servers. Core-only prompt byte-identical (turns test).

**Findings and fixes:**
1. *Worktree scope (decision).* A linked worktree was its own project: memory written there was
   invisible from the main checkout and orphaned when the worktree is removed (this project
   works in worktrees). New `project_skills::main_checkout` / `memory_scope_key`: follows the
   `.git` file's `gitdir:`, requires the pointer back (`<gitdir>/gitdir` = this `.git`, which a
   foreign folder can't forge since it lives in the other repo's `.git`) and a `commondir` naming
   a `.git` folder; absolute and relative pointers. Submodules (`.git/modules`, no `commondir`),
   forged pointers, bare-repo worktrees and nested repos stay their own project. Project *skills*
   keep `scope_key` per checkout. Used by `memory_scope`, the tool and staging paths (now computed
   inside `spawn_blocking`) and `PromptSource::build`. Case/slash/`..`/junction/8.3/`\?\`
   spellings already collapse via `canonical` + lowercase key. The scope comes only from the
   session's project root (`take_extras` never reads a `scope` field; items reject extra fields).
2. *Reset destroyed data the dialog doesn't name.* The page asks "Delete Agent memory
   (MEMORY.md)? This cannot be undone."; CP9.2 made it also erase every repository's project and
   archive entries of that kind and their notes. Now `reset` purges `core` rows only (live and
   removed, FTS, checkpoint as in CP9.1); other tiers stay. PITFALLS, GAPS, PORTING updated.
3. *A purge could delete a note a live entry still shows.* A patched entry's successor keeps its
   predecessor's note link, and `purge_rows` also takes removed rows of the same text from any
   tier; erasing an unrelated entry with the old text deleted the live successor's note file.
   `db::unreferenced` keeps any note a remaining row links.
4. *Notes folder as a junction/symlink* was followed by write/read/delete. Now refused (test
   plants a real junction with `mklink /J`).

Checked, no change: leaks — project rows only via `tier_rows(scope)`; errors list entries of the
session's scope only (archive is global by design); no REST/Star map/index (`memory_doc`) path
reads non-core tiers; children inherit the parent's root, the curator has none; Phase 7
`review_once` ops are core. Every `Operation` consumer that stages or applies (`approve`, staging
`apply_memory`, unattended, `write_approval`) passes `extras`; GPUI uses `approve`. Limits and
boundaries tested (1000/1001 chars, 5000 rows, project 1500, 200-20000 clamp, chars not bytes);
one tier per call, one `BEGIN IMMEDIATE` transaction, notes removed on a failed commit.
Left: orphan note after a crash between note write and commit (not swept); no page to erase
project/archive entries until CP9.3 (only `delete_tier_entry`, or a soft agent removal); an old
build approving a staged non-core write would apply it to core (PITFALLS); `source_session`
unfilled; no live check (no page shape changed).
Tests (`CARGO_TARGET_DIR=target/claude`): hermes_memory 81 + 1 ignored (new: reset core-only,
note kept for a surviving row, string tags/links, junction), hermes_tools 246 + 1 ignored (new:
linked worktree), store 12, prompts 23, llm 468+5; runtime lib 169; `--test turns` 196 (7
ignored) under default, balanced, requests, tokens; hermes-desktop 174 + acp 68;
`cargo build --workspace` ok, no warnings. Request counts unchanged.

## CP9.3 — retrieval into the context block, dedupe on add, hygiene, supersedes, links for the map
Worktree: C:\Users\alexandros\repos\hermes-rs-p9   Branch: backend/p9-memory   Model: Sonnet 5.5 (subagent)
Cut on top of CP9.1 + CP9.2 (+ their reviews, b9bc0c0). Uncommitted. Phase 9's last checkpoint.
Done:
- **Retrieval.** Under `requests` (`budget.memory_preload` = 3; `balanced`/`tokens` have 0 and are
  byte for byte unchanged) `Runtime::begin_turn`'s existing Phase 5 step now also finds the `project`
  entries of the session's repository and the global `archive` entries, and puts them in the
  `<context>` block of the **user message** as `<entry id="archive:<n>" tier="archive">` (or
  `project:<n>`; `<n>` = the row id in `memory.db`, the number a `memory:<n>` link names), with one
  extra sentence about the memory tool. Same machinery as skills: stored with the message (resume,
  rewind and branches replay the bytes; the `preinject` note lists the ids; the page shows the typed
  text); "already attached" is read off the conversation. Nothing reaches the system prompt.
- **`index.db` schema 4** (`index.rs`): `memory_doc` gets a `kind` column and also holds the live
  `project` / `archive` rows: `scope` = `global` (the user's own entries and the archive) or
  `project:<key>` (`memory_scope_key`, worktrees folded in). The query selects
  `scope IN ('global', <own>)` **in SQL before the LIMIT** (a test fills the candidate list with another
  repository's 40 better matches: the session's own entry still comes back). Rows are re-indexed when
  `memory.db`'s **generation** (`meta.tiers_gen`, bumped in the transaction of every non-core write and
  delete) changed: this process's writes mark the index dirty (the existing hook) and a query refreshes
  first, another process's are seen by comparing the generation at each query. Soft-removed and
  superseded rows are never indexed; a row the user deleted is deleted from the index; a retrieved row
  is re-checked against `memory.db` (`row_stats`).
- **Ranking and gates** (tier rows only; core hits as in Phase 5): BM25 (text 1.0, tags 2.0) plus up to
  0.5 recency (half-life 90 days from the last confirmation / hit) plus up to 1.0 hits (0.3 * ln(1+hits)).
  Relevance gates, because BM25 over a handful of rows is near zero (SQLite floors the idf): the message
  needs two own words (`word_count`, Phase 5's rule) and the row must share two stemmed words with it, or
  one with a score >= 1.0; a match through the assistant's last text or the project name alone is not
  enough; a hit under 30% of the best one's score is dropped. Disabled kinds (Settings) are skipped.
- **Mid-session visibility**: tested end to end (`turns`): turn 1's `memory` call writes an archive entry,
  turn 2's first request carries it in the user message, not in the system prompt (byte-identical across
  all three requests).
- **Hits** (`retrieval.rs`): the entries attached to a message get `hits + 1` through
  `MemoryStore::record_hits`: counted in memory, written in one transaction at 20 waiting entries, a
  minute after the first, at drop and before the hygiene pass (so a turn costs no database write of its
  own; a crash loses up to a minute). Called from the same `spawn_blocking` as the query. The last hit
  time is `meta` key `hit:<id>` (no schema change, an older build still opens the file). A core entry's
  hit is found by its text.
- **Skill boost**: `skill:<name>` links of the retrieved tier entries raise that skill's score by 1.0 each,
  at most 2.0, before the thresholds; only a skill that already matches the message (a linked skill that
  doesn't is never brought in); the skills part of `query` was reordered (memory first) and collects
  before cutting, identical results without a boost (the Phase 5 goldens pass unchanged).
- **Dedupe on add** (`similar.rs`, any tier, in the same batch planner): the new text's stemmed word
  set is compared with the live entries of the same tier, scope and kind (Jaccard >= 0.6, or one set 90%
  inside the other when it has >= 4 words). Decision: a **byte-exact repeat is a no-op that counts as
  a confirmation** (`last_confirmed` = now, tags still applied) with a sentence in `message` and a
  `duplicates` field; a **near-duplicate is added and flagged** in the result of the same call
  (`similar_entries` with the old entry's row id and text, and a sentence naming `#id`, the entry and
  "update the existing one (action 'patch') or remove one") — no extra request, never a silently
  dropped fact; a batch that removes/patches the similar entry first is not flagged. Non-duplicate
  results are byte-identical (CP9.2's old-shape comparison test passes unmodified).
- **Hygiene** (`memory_hygiene.rs`, in `curate()` for every profile, zero requests): `core` entries not
  confirmed or hit for 60 days, not tagged `pinned`, <= 1000 characters (the archive's cap), not proposed
  in the last 30 days, still in the file, are staged oldest first, at most 5 per run, as a `memory` item
  with one `demote` operation (summary "demote memory", diff "- entry / + [moved to the archive] entry",
  reason "memory hygiene: not used or confirmed for 60 days", session `curator`); the staging area's own
  rule keeps one item per identical proposal. Never applied directly. Apply = `MemoryStore::demote_core`:
  the entry is re-looked up (changed since = stale, nothing copied), copied to the archive with its tags
  and links through `apply_ext` (limits, threat scan), then removed from core. The tool refuses `demote`.
  `last_confirmed` moves on a repeat `add`, a patch or a replace (new row); entries made by the migration
  count from migration day, so nothing is proposed for 60 days after the upgrade.
- **Supersedes** (`review_once`): a memory `add` may carry `supersedes: <id>` (optional, null/absent =
  today's contract; the schema lists it, strict mode requires it nullable; only valid on `add`; numbers or
  "#12"). The packet shows `<entry id="12">` for entries the database knows (unchanged text otherwise,
  golden intact) and the prompt explains the field. Applying: the id must name a live `core` entry of the
  same file (an archive row, the other file, a removed row: rejected, nothing staged); the op then becomes a
  replace of that entry (the old row is marked `superseded_by` the new one, so it leaves the prompt and the
  index) and is **held for the user** like any review replace (tainted: with the taint reason).
- **Map** (`learning.rs`): the page already draws `{source,target}` edges between nodes, so a core card's
  `skill:<name>` link to a learned skill and `memory:<id>` link to another card are real edges (no pair
  repeated; edges only join existing nodes); additive fields the page ignores: `archive` (newest 100 archive
  entries) and `memory_links` (every link of a core card or archive entry with `resolved` true / false /
  null) and two `stats`. Dangling links (skill deleted, memory removed) are listed unresolved and drawn
  nowhere. **Project memory is not in the map** (it has no working folder; a repository's notes must not
  show in another's view). No patch (the page's shapes are unchanged).
Files: `crates/memory/src/{similar,retrieval,retrieval_tests,store,tiers,db,lib}.rs`,
`crates/tools/src/{index,index_memory_tests,staging}.rs`,
`crates/runtime/src/{memory_hygiene,preinject,curator,lib}.rs`, `crates/runtime/src/review_once.rs`,
`crates/runtime/src/review_once/{contract,packet,apply,tests}.rs`, `crates/runtime/{Cargo.toml,tests/turns.rs}`,
`crates/prompts/src/review.rs`, `crates/desktop/src/learning.rs`, `scripts/perf/{memory-check.mjs,README.md}`,
`PORTING.md`, `GAPS.md`, `docs/{ARCHITECTURE,PITFALLS,TESTING}.md`, this note.
Tests: (all `CARGO_TARGET_DIR=target/claude`, temp folders only)
- `cargo build --workspace`: ok, no warnings.
- `cargo test -p hermes_memory -p hermes_tools -p hermes_store -p hermes_prompts -p hermes_llm`: green (hermes_memory 101 + 1 ignored (CP9.1/9.2's 81 unmodified; new: 4 `similar`, 16 `retrieval_tests`), hermes_tools 264 + 1 ignored (new: 15 `index_memory_tests`, 3 staging demotion), store 12, prompts 23, llm 468+5).
- `cargo test -p hermes_runtime`: lib 178 (new: 4 `memory_hygiene`, 5 supersedes/contract/packet in `review_once`); `--test turns` 204 (7 ignored; 8 new) green under default and under `HERMES_TEST_PROFILE=balanced|requests|tokens`.
- `cargo test -p hermes-desktop`: lib 176 (2 new in `learning.rs`) green.
- Live: `scripts/perf/memory-check.mjs` against my own instance (port 47810, data in target/perf): all checks passed (22, 10 of them new); instance stopped.
Request counts: unchanged baselines. New, exact: archive scenario = 2 requests (the memory call, the
answer) + 1 (turn 2, entry attached or not) = 3 under `requests`, `balanced` and `tokens`; four turns in
two repositories = 4; three turns with a near-duplicate and a distinct add = 6 (the dedupe note adds none);
the curator's hygiene pass = 0 (`total_requests == 0`).
Deviations from the plan:
- Dedupe by stemmed word sets (Jaccard / containment), not "BM25 top-1": it also covers `core` (planner sees
  plain texts, the database may be off) and doesn't depend on how many other entries exist.
- A `supersedes` is applied as a replace and therefore always held for the user (the plan says "staged when
  tainted as today"); see the first item below.
- `last_hit` lives in `meta`, not a column (no schema change). "Pinned" = the tag `pinned`.
- Tier rows must share two own words with the message (or one with a good score) on top of BM25 (small
  corpora score ~0 in BM25); the plan didn't say.
For the reviewer:
- **Supersedes is held in every session** (a background review never changes an entry the user has
  unattended: Hermes's rule, CP7.2). Confirm, or allow a direct apply for an entry the agent itself wrote.
- **Relevance gate for tier rows** (two shared stemmed words, or one with BM25 >= 1.0; recency/hits only
  reorder) is tuned on small fixtures (a 13-query golden); a real archive of thousands of entries is
  untested. The thresholds are constants in `index.rs` / `similar.rs`.
- **Core entries are still retrieved too** (Phase 5 behaviour, `memory:<hash>` ids) although they are in
  the system prompt; I left it (changing it would change `requests` behaviour outside this checkpoint).
- **Hits for core** are found by text and also feed the hygiene pass (a core entry the model sees retrieved
  stays out of the pass); an entry only ever in the prompt, never retrieved, goes stale after 60 days even
  if the model uses it every turn. That is the plan's "hit/confirmed" in the only form we can observe.
- **Demotion of USER.md entries** is proposed like MEMORY.md ones (user facts go stale less than tool
  facts; staged, so the user decides). Say if `user` should be exempt.
- **Pinning has no UI** (add the same text again with the tag `pinned`); no page lists, pins or erases
  project / archive entries (the Star map only lists archive entries additively, the page ignores them).
  `delete_tier_entry` still has no caller.
- Hit counting writes the batch from the turn's blocking task when due (20 entries / a minute): a slow
  disk could add its latency to that one turn; bounded by the 3 s query timeout only for the query part.
- `memory_hygiene` and demotion also run when the user clicks "run curator" (any profile).
- The review prompt grew (one sentence + `supersedes` in the text-contract shape): a one-time cache miss
  for review requests. The model might copy the example value; an unreadable id is rejected alone.
- `rusqlite` is a dev-dependency of `hermes_runtime` (one turns test backdates rows).
- Not verified: a real model's use of `supersedes` / the dedupe note; the webapp (no patch was needed, none applied); disk-full behaviour of hit batches.

### Review (Opus 5.5)
`main` had not moved (b9bc0c0 + CP9.3 uncommitted); no merge. `/code-review` not run as a skill
(subagent); manual pass over the whole diff, the page's Star map source and a live run.

**Caching (evidence).** Retrieval only ever writes the user message's `<context>` block, through
Phase 5's `Block` path (stored with the message, `preinject` note, `typed_text`); the turns tests
compare the system prompt byte for byte over all three requests of the archive scenario, replay the
stored block byte for byte after a resume from disk and after a rewind, and see turn 1's archive
write in turn 2's first request (`tiers_gen` + the dirty hook; another `MemoryStore` on the same
folder is seen by the generation, `another_stores_write_is_seen_without_a_hook`). `balanced` /
`tokens`: no block, no note, same three requests. **Map shape:** the page's `StarmapGraph`
(`types/hermes.ts`) reads `nodes`, `edges`, `clusters`, `memory`, `stats` only; edges are drawn
generically (`simulation.ts` keeps an edge only when both ends are nodes; `star-map.tsx`
adjacency), so card→skill and card→card edges are drawn and `archive` / `memory_links` are ignored
(TypeScript types are compile-time only; no runtime schema check). Driven in my own instance over
DevTools (`#/starmap` with a card linking a skill, another card, a dangling memory id and a missing
skill, plus an archive row holding `</entry>`): the canvas rendered, no page error, the archive text
not shown. No cwd: project memory is never read by `graph`.

**Findings and fixes:**
1. *Noise on a real-sized archive.* The gate ("two of the message's words, or one with BM25 >= 1.0")
   counted stopwords (the memory crate's short list: "how", "do", "what", "you" counted) and, on a
   240-entry archive, "looks good, thanks" pulled "... thanks the reviewers in the changelog", "fix
   the bug" and "explain this error" pulled four entries each (any rare word scores > 1.0 once there
   are hundreds of rows; on a handful every score is ~0, which is why the floor never bit in the
   goldens). Now: the message's words are the index's own search words (its stopword list), a tier
   row needs a message of **>= 3** of them and **>= 2 shared** (stemmed); the one-word path is gone.
   New test `a_large_varied_archive_injects_nothing_for_small_talk_or_vague_requests` (16 small-talk /
   vague messages: nothing; two specific questions: their entry first). The 13 goldens pass unchanged.
2. *Block structure could be forged.* `sanitize` only rewrote `</context>`: an archive entry (written
   by the agent, maybe from tainted evidence and approved) holding `</entry><entry id="memory:x"
   tier="core">` or `</relevant-memory><relevant-skills><skill ...>` broke out of its element. New
   `sanitize_body` writes the `<` of the block's own tags (any case, opening or closing) as `&lt;`, for
   entry and skill bodies; `Vec<T>`, `<b>`, `<entrypoint>` untouched. Test
   `an_entry_cannot_break_out_of_its_element_or_forge_another`. (Only new messages change; stored
   blocks replay as stored.)
3. *Hygiene under balanced/tokens would make the agent forget.* There is no archive search tool, and
   only `requests` retrieves the archive or records hits; under the other profiles every core entry
   would be "unused" after 60 days and a demotion would take it out of the model's sight for good. The
   pass now runs only when `budget.memory_preload` > 0. Test
   `the_hygiene_pass_proposes_nothing_under_a_profile_that_never_retrieves_the_archive`.
4. *A hit flush could cost the turn its block.* `record_hits` ran inside the query's 3 s time limit; a
   due batch waiting on `memory.db` (busy timeout 5 s, another process writing) would time the
   retrieval out. Hits are now recorded in a task of their own after the block is returned, and only
   for a block that is used.
5. *Demotion vs a hand edit* (no change needed, test added): `a_hand_edit_of_memory_md_between_proposal_and_apply_is_kept_never_clobbered`
   — editing another entry by hand keeps the edit and still demotes; editing the proposed entry by
   hand makes Apply stale, the file byte-identical, nothing copied.

Checked, no change: scope filter in SQL before `LIMIT` (`scope IN ('global', ?3)`, test with 40
better foreign rows); no cwd = `''` matches no project row; worktree key = main checkout's; schema-4
bump drops and rebuilds `index.db` (a cache); soft-removed/superseded rows never indexed, a retrieved
row re-checked by `row_stats`, a user delete bumps the generation and leaves the index at the next
query; dedupe: only byte-exact repeats are no-ops, "port 8080" vs "port 8081" is flagged similar but
both stored, "prefers tabs" vs "prefers spaces" not similar; hits/recency bounded (+1.0 / +0.5, and
only among rows that pass the gate, so the hits feedback loop can reorder but never inject);
`supersedes`: validated against live core rows of the same file, hallucinated ids rejected alone,
always held (agree: conservative, matches CP7.2), cannot remove core entries unapproved; hygiene
zero requests, staged only, 5 per run, 30-day re-proposal in `meta` (survives restarts), pinned
exempt; `sessions.db` untouched; blocking work in `spawn_blocking`; `rusqlite` dev-dependency fine.
Live check: 21 checks, not 22 as the note says (all pass).
Left: an archive entry is not threat-scanned again when injected (scanned at write); the map's
`archive` list includes `user` rows when USER.md is disabled (ignored by the page); core entries still
retrieved although in the prompt (Phase 5, unchanged); USER.md entries are demotable (staged, the
user decides); no UI to pin or to list/erase project and archive entries.
Tests (`CARGO_TARGET_DIR=target/claude`): hermes_memory 101 + 1 ignored, hermes_tools 265 + 1 ignored,
store 12, prompts 23, llm 468 + 7 ignored (+5 integration); runtime lib 180; `--test turns` 205 (7 ignored; 1 new) under default, balanced, requests, tokens;
hermes-desktop 176 + acp 68; `cargo build --workspace` ok, no warnings. Live: `memory-check.mjs` all
passed against my instance (stopped after). Request counts unchanged; Phase 9 baselines note added.

### User checklist (Phase 9)
1. Open Settings > Memory (or the Star map): your existing MEMORY.md / USER.md entries are all still
   there, unchanged. Your files next to the app data still hold them
   (`%APPDATA%\hermes-rs\memories\MEMORY.md`, `USER.md`).
2. In a session inside repository A, ask the agent to "remember in this project's memory that release builds
   need the lto profile" (it uses tier `project`). Start a new session in A and ask "how do release builds work here":
   the agent already knows (a new session in A has the project notes in its instructions; under the default
   `requests` profile a note written *during* a session also rides on your next message there; the
   transcript shows only what you typed).
   Start a session in repository B (or a folder outside a repository) and ask the same: it does not know.
3. In any session, tell the agent a fact for the archive ("keep a note in the archive that the deploy password
   rotates on the first Monday"). A few messages later (same or another session), ask "when does the deploy
   password rotation happen": the answer comes without a memory tool call. (This needs the `requests`
   budget profile, the default: Settings > Model > Budget profile. A short or vague message, such as
   "thanks" or "fix the bug", never pulls archive notes in: ask with at least three real words.)
4. Ask the agent to remember something it already has, reworded: its tool result says "similar to an existing
   entry #N ..." and it may update that entry; asking the exact same thing again says "already stored".
5. Under the `requests` profile only (the default; nothing is proposed under balanced/tokens, which never bring
   archive notes back), after a core entry has gone 60 days unconfirmed (nothing is proposed in the first 60
   days after the upgrade; the curator runs on its schedule or when you start it): Settings > Memory > "Waiting for
   your review" lists "demote memory" items. Apply moves the entry to the archive (it leaves MEMORY.md and the prompt, and is
   still found when a message is about it); Reject keeps it (it is not proposed again for 30 days). To keep an
   entry out of this for good, ask the agent to add the same entry again with the tag `pinned` (there is no
   pin button: pinning cannot be done from the UI).
6. Settings > Memory > reset deletes MEMORY.md / USER.md contents only (what its dialog names): project and
   archive notes stay. **No page lists or erases project / archive entries** (cannot be done from the UI):
   ask the agent to remove one (it is then hidden from the agent but kept in `memory.db` as a removed row),
   or erase by hand in `memory.db` with the app closed.
7. Backups: before the first migration the app copied your files next to them as
   `MEMORY.md.bak.pre-memory-db.<time>` / `USER.md.bak.pre-memory-db.<time>` (never deleted by the app; they
   hold the old text); a file you emptied or deleted by hand is copied to `<file>.bak.dropped.<time>` first;
   a damaged `memory.db` is moved aside to `memory.db.corrupt-<time>`. All in `%APPDATA%\hermes-rs\`
   (`memories\` for the files, the folder itself for `memory.db`).
8. The Star map: a memory card whose entry links a skill (`skill:name`) is joined to that skill by a line.
   Project memory is never shown there.
