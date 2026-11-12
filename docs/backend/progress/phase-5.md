# Phase 5 progress

## CP5.1 — the local index: `index.db`, incremental rebuild, query, retrieval goldens
Worktree: C:\Users\alexandros\repos\hermes-rs-p5   Branch: backend/p5-index   Model: Sonnet 5.5 (subagent)
Cut from `main` at 8c1f5cc (Phases 0-4 merged). Uncommitted.
Done:
- **New `crates/tools/src/index.rs`** (in `hermes_tools`, not a new crate: it needs `SkillStore` and its frontmatter parsing,
  and `hermes_tools` already depends on `hermes_memory`). `LocalIndex` over a separate `data_dir/index.db` (rusqlite
  "bundled" already in `Cargo.lock`, FTS5 is compiled in; one new dependency line in `crates/tools/Cargo.toml`, no new crate).
  - Tables: FTS5 `skill_doc(name, category, description, body, path, mtime, hash)` and `memory_doc(id, tier, scope, text,
    tags)`, `tokenize = 'porter unicode61'`; plus plain `skill_files(path PK, doc rowid, mtime, size, hash, name, active)`
    (FTS5 can't look a row up by path) and `meta`. `PRAGMA user_version` = 1: another version, or a file SQLite can't read, is
    deleted and rebuilt (it is only a cache).
  - **Incremental `refresh()`** (blocking): walks the skill folders (own + `external_dirs`, catalog order) with `stat` only;
    re-reads a file when mtime or size changed, or when its mtime is < 2 s old (racy: a second same-size write could keep the
    stat); re-indexes only when the hash changed too; drops files that went; the first copy of a name in catalog order is the
    `active` one (a shadowed copy is indexed but never returned; removing the first lets the second take over without a
    re-read). Skills not for Windows are remembered (`doc` NULL) so they aren't re-read. Memory: re-indexed when the hash of
    MEMORY.md / USER.md entries changes (ids `memory:<hash8>` / `user:<hash8>`, tier = target, scope `global`, until Phase 9).
    Returns a `RefreshReport {files_seen, files_read, added, updated, removed, memory_rebuilt}` (the tests assert on it).
  - **Freshness.** `SkillStore::set_on_change` / `MemoryStore::set_on_change` (new; a `Fn() + Send + Sync` hook). The skills hook
    is in `commit()` and `commit_outside_changes()` (every write path ends in one: `skill_manage`, the Skills screen's
    `user_write`, the curator, restore, import, seeding); the memory hook is in `mutate`'s successful write (`memory` tool, the
    Star map's `edit_entry`). `LocalIndex::attach()` registers a hook that calls `notify_changed()`: sets `dirty` and starts (or
    keeps) one background `hermes-index` thread that refreshes until clean, so a write never waits. `query()` refreshes first
    when dirty (serialized by the connection mutex), so it never answers from before a write. The hooks hold a `Weak`.
  - **`Runtime::new`** opens the index (`index.db` in the data dir), attaches it and calls `notify_changed()` (first refresh in the
    background, after `seed_bundled_in_background`); `Runtime::index() -> Option<Arc<LocalIndex>>` (None only if even a fresh
    file can't be opened; logged). Nothing in the prompts or turns uses it yet.
  - **Query**: `query(&IndexQuery{message, last_assistant, project}, &Limits{skills, memory, min_skill_score,
    min_memory_score}) -> Retrieval{skills: Vec<SkillHit>, memory: Vec<MemoryHit>}`, plus `query_async` / `refresh_async`
    (`spawn_blocking`) and `project_name(cwd)` (folder of the nearest `.git`, else the cwd's name). Score = `-bm25`, weights name
    10 / category 2 / description 6 / body 0.3 (memory: text 1 / tags 2). Thresholds: `DEFAULT_MIN_SKILL_SCORE` 2.5,
    `DEFAULT_MIN_MEMORY_SCORE` 1.0, plus a hit must reach 30% of the best hit's score; defaults k = 3 skills, 5 memories
    (`Limits::default()`; CP5.2 takes k from `skills.preload`). Disabled skills and disabled memory targets are filtered at query
    time (they are indexed).
  - **Escaping.** User text never reaches `MATCH` as written: cut on non-alphanumerics, lowercased, stopwords and 1-letter ASCII
    words dropped, each token capped at 32 chars, at most 32 message + 12 assistant (first 500 chars) + 4 project tokens, message
    read only up to 3000 chars; every token goes in as `"token"` joined with ` OR `. `NEAR`, `AND`, `*`, `:`, `-`, `(`, quotes
    are separators or plain words. No tokens -> empty result (no query run).
- Refactor in `skills.rs`: `catalog_all`'s per-file parsing moved to `skill_entry(root, external, path, text)` (same code, shared with
  the index; the catalog's result is unchanged: the whole `hermes_tools` suite passes), `catalog_roots()`, and `skill_files`, `hash`,
  `seed_files` made `pub(crate)`.
Files: crates/tools/{Cargo.toml,src/index.rs (new),src/lib.rs,src/skills.rs}, crates/memory/src/store.rs,
  crates/runtime/{src/lib.rs,tests/turns.rs}, Cargo.lock, PORTING.md (row, diverged), GAPS.md, docs/ARCHITECTURE.md,
  docs/backend/progress/phase-5.md
Tests (CARGO_TARGET_DIR=target/claude):
  - `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts`: tools lib 140 passed / 1 ignored (24 new in
    `index::tests`), memory 22, store 15, prompts 11 (all ok).
  - `index::tests`: `goldens` (17 queries on the bundled skills: 15 top-1 expectations and 2 "no skill passes the threshold":
    "write a git commit" -> requesting-code-review, "open a pull request on github" -> github, debugging -> systematic-debugging,
    TDD, arxiv, pdf, Word/PowerPoint -> microsoft-office, architecture diagram, meeting notes, inbox triage, youtube, maps,
    blocked page -> blocked-page-recovery, weather -> nothing, empty -> nothing), `memory_goldens_with_the_default_threshold`,
    project name / last-assistant steering, disabled skills/memory, incremental (second refresh reads 0 files; change / add /
    delete each read only what changed; touched-identical file read but not re-indexed; racy same-size rewrite seen; other-platform
    skill not re-read; shadowed names), write hooks (`skill_manage` create/delete and `memory` add/remove seen by the next query
    with no manual refresh; background `notify_changed`), corrupt file / stale schema rebuilt, escaping (31 hostile strings as the
    message and as assistant/project text, 1 MB inputs, 100k-repeat inputs; expression shape asserted), token budgets, unicode,
    async wrappers, cost on 400 generated skills.
  - Cost sanity (debug build, this machine, 400 generated skills of ~3 KB): first refresh ~450 ms, idle refresh ~260 ms (of which
    the directory walk `skill_files` alone is ~210 ms: Windows `read_dir` + `is_file` per directory, the same walk `catalog_all`
    does), one changed file -> 1 read, query ~0.4 ms. The 37 bundled skills: a few ms.
  - `cargo test -p hermes_runtime`: lib 64, `turns` 105 passed / 7 ignored (1 new: `the_local_index_follows_skill_and_memory_writes`:
    startup index of the bundled skills, a `skill_manage` create and a memory add are each found by the next query, 0 model requests).
  - `cargo test -p hermes-desktop`: lib 169, `acp` 68, ok. `cargo build --workspace`: ok, no warnings.
    (`target/hermes-src` junction made, gitignored.)
Request counts: none changed (no model request is involved; S1-S5 untouched).
Deviations from the plan:
- `crates/tools/src/index.rs` rather than a new crate (the plan allowed either).
- Project skills (Phase 8) and `memory.db` (Phase 9) are not indexed; memory ids are a hash of the entry's text until Phase 9.
- The relative score cut (30% of the best hit) and the body weight 0.3 are additions to the plan's "score threshold"; reason: with
  only 37 documents, absolute BM25 scores are small and a long query adds up body-only matches (first try: weather query scored 3.5,
  "write a git commit" 3.9, no separable threshold; with body weight 0.3 the worst golden positive is 3.1 and the worst
  negative 1.6).
- The index is refreshed in the background after writes, not synchronously inside the write: a write returns before the index
  is current; `query()` closes the gap by refreshing when dirty.
- Edits made with the file tools / by hand (no `commit`) are not hooked directly: they are picked up at the next write, at
  startup, or when something calls `refresh()` (the curator's `commit_outside_changes` does). CP5.2 may want a cheap `refresh()`
  per turn (it is stat-only when nothing changed).
For the reviewer:
- **Threshold tuning is on the 37 bundled skills only.** The goldens pin it, but a user with 300 skills has a different IDF
  distribution (more distinct names -> higher scores for rare words, more noise for common ones). The defaults are in
  `DEFAULT_MIN_SKILL_SCORE` / `Limits`; CP5.2 should expose them via the profile if live use shows trouble.
- Idle refresh cost is dominated by `skill_files` (the directory walk), not by SQLite or reads. If CP5.2 refreshes per turn on a
  big skills folder, consider a cheaper change check (e.g. the folder's own mtime is not reliable on Windows for nested edits).
- `notify_changed` loop: `dirty` is cleared inside `refresh()` after the connection lock is held; the worker's exit re-checks
  `dirty` after clearing `worker` (the classic lost-wakeup window). Please re-read that loop.
- `commit()` now calls the hook before `ensure_repo()`/git, while `write_lock` is usually held by the caller; the hook only sets
  an atomic and spawns a thread, but a refresh reads files while a writer may still be mid-batch. A half-written batch is
  re-indexed by the hook the batch's *own* later write triggers (each batch ends in `commit()` after its files are on disk).
- `catalog_roots` and the index treat `external_dirs` like the catalog; a changed `external_dirs` setting only takes effect at the
  next refresh (nothing marks the index dirty on a config change).
- The memory hook fires inside `mutate` while `MemoryStore.lock` is held; `LocalIndex::refresh` calls `MemoryStore::entries`,
  which does not take that lock (reads the file), so no deadlock; a refresh may read a file between a write's rename and the
  hook, which is fine (atomic rename).
- `SkillStore`/`MemoryStore::set_on_change` hold one hook each; a second `attach` replaces the first.
- The racy window re-reads every file written in the last 2 s: right after the bundled seed on a first run, the first refresh reads
  all of them twice (once per refresh, the second finds the hash equal and only updates the stat). Harmless.
- Not done on purpose (CP5.2): injection, `index_prompt` variants, profile wiring, `preinject-check.mjs`. No live check was added:
  the page calls nothing new.

### Review (Opus 5.5)
Main had not moved (branch still at 8c1f5cc): no merge. `/code-review` not run (subagent); manual pass over the diff and
`index.rs`.
Review focus:
1. **Blocking work.** Every SQLite call is behind `LocalIndex::{refresh,query}`, which are documented blocking and only reached
   through `spawn_blocking` (`*_async`), the `hermes-index` std thread, or tests. The write hooks only set an atomic and maybe
   spawn that thread: they never touch SQLite or the files, so no write path waits. `notify_changed` re-read: no lost wakeup.
   The writer does `dirty=1; swap(worker)`, the worker `worker=0; load(dirty); swap(worker)` (all SeqCst): either the writer's
   swap sees 0 and spawns, or the worker's load sees the write's `dirty` and continues; when both happen the worker's swap sees
   the new thread and exits. Lock order is conn -> store config only (hooks never take conn): no deadlock. **Found:** a failed
   refresh cleared `dirty` for good (stale until the next write) and `query()` returned the error. Fixed: `refresh` sets `dirty`
   again on failure, the worker stops instead of spinning on a lasting error, `query` logs and answers from the index as it is.
2. **Escaping.** Tokens are alphanumerics only, quoted, `OR`-joined; the existing 31 hostile strings plus 8 I added (spacing
   marks, superscripts, roman numerals, Arabic digits, titlecase, ZWJ/BOM/soft hyphen, math alphanumerics, `İ ß ﬁ`, U+FFFD):
   characters Rust calls alphanumeric but `unicode61` doesn't give an empty phrase, which FTS5 accepts. No gap found.
3. **Cost.** Idle refresh on 400 skills ~260 ms (debug), almost all the directory walk; a `query()` right after a write pays
   one refresh (it would otherwise wait for the background one on the connection lock). Acceptable for CP5.2 from
   `spawn_blocking`; a per-turn `refresh()` on a big folder costs the walk each turn, worth a note for CP5.2.
Retrieval design: probed the goldens with Hermes's whole skill set (`../hermes-agent/skills` + `optional-skills` as external
folders: 232 files, bundled ones shadowed). With the 40 000-char body, 2 of 17 failed: "open a pull request on github" put
`github` 7th (2.2), and the weather query got `ascii-art` (5.0). Cause: FTS5's BM25 normalizes by the *whole row's* length,
so a long body drowns a name/description match whatever the column weights. **Fixed:** index the first 2 000 characters of the
body (`MAX_BODY_CHARS`, `SCHEMA_VERSION` 2 so an old file is rebuilt): all 17 hold on both sets, top-1 margins roughly
double (e.g. `pdf` 19.5 vs 9.0), the weather query passes nothing at 232 too. New test `goldens_hold_on_hermess_full_skill_set`
(skips when `../hermes-agent` is absent). The goldens are not overfit in the sense that worried the implementer: thresholds
unchanged, and they hold at 6x the documents. "write a git commit" -> `requesting-code-review` ("Pre-commit review") is
acceptable: no git skill is bundled (the plan's golden assumed one); commented in the golden. Short generic messages ("hello",
"explain this error") can still pass 2.5 on a 230-skill set (`ascii-art`, `anatomy`); CP5.2 should keep k small and may want
the threshold in the profile.
Other findings, fixed:
- Racy-mtime rule only worked if the next refresh came within 2 s; a same-size, same-mtime rewrite after a racy read, picked up
  by a later refresh, was missed. Now a racy read stores a smudged mtime (-1), as git does, so the next refresh always re-reads
  it. Test `a_racy_read_is_re_read_even_when_the_next_refresh_comes_late` (sleeps 2.2 s).
- A memory file that exists but can't be read (locked) was indexed as empty, dropping every entry until the next write. Now
  skipped (new additive `MemoryStore::readable_entries`). Test.
- A SKILL.md that can't be read right now was deleted from the index; now its old row is kept. A non-UTF-8 SKILL.md was
  indexed via `from_utf8_lossy` although the catalog skips it; now left out like the catalog. Test.
- The category was stored with `/` turned into spaces and turned back on the way out (a folder with a space came back split);
  stored as is now (`/` is a tokenizer separator anyway).
- Changing `external_dirs` didn't mark the index dirty; `SkillStore::set_config` now calls the hook when the folders change.
  Test.
Checked, no change: the store schema is untouched (`index.db` is separate, user_version 2); module doc and PORTING row say
diverged; nothing per-turn reaches prompts or turns (only `Runtime::index()` is new); `skill_entry()` is the old loop body
verbatim (the `seen` check moved after parsing, same result), catalog tests pass; the hooks fire after files are on disk.
Left: the plan's "project skills" (Phase 8) and `memory.db` (Phase 9) as noted; hand edits outside `commit()` still wait for the
next write/startup/curator pass (CP5.2: a cheap `refresh()` per turn, minding the walk cost above).
Tests (CARGO_TARGET_DIR=target/claude): tools lib 145 passed / 1 ignored (29 in `index::tests`), memory 22, store 15, prompts 11;
runtime lib 64, `turns` 105 passed / 7 ignored; desktop lib 169, `acp` 68; `cargo build --workspace` ok, no warnings; clippy
clean on the changed crates.
