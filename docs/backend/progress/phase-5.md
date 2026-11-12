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

## CP5.2 — per-turn pre-injection: the `<context>` block, hidden from the page, and the skills block variants
Worktree: C:\Users\alexandros\repos\hermes-rs-p5   Branch: backend/p5-index   Model: Sonnet 5.5 (subagent)
On top of CP5.1 (c58479d). Uncommitted.
Done:
- **New `crates/runtime/src/preinject.rs`.** `retrieve()` (async: `spawn_blocking` + a 3 s timeout; an error, panic or timeout is logged
  and means no block) asks `LocalIndex::query` and `render()` builds the block (pure, unit-tested):
  ```
  <context>
  <preamble: attached by the system, background not user text, follow like a loaded skill>
  <relevant-skills> <skill name=".." category="..">full SKILL.md body (frontmatter cut)</skill> ... </relevant-skills>
  <relevant-memory> <entry id="memory:ab12cd34" tier="memory">text</entry> ... </relevant-memory>
  </context>

  <what the user wrote, with any @-reference expansion>
  ```
  Bounds: 12 000 chars per skill, 24 000 for all skills of one block (a cut skill says `skill_view` has the rest), 800 per memory
  entry; a body containing `</context>` is escaped so the block can't close early.
- **Wiring in `Runtime::begin_turn`.** `inject` = eligible text (`preinject::eligible`: not a skill / bundle invocation, `/plan`, `/init`,
  `/learn`), no scripted prelude (`/review`), a knob above 0, an index. The user message is built exactly as before (so a refused `@file`
  still fails `start_turn` synchronously: a test caught that) but *held*, and added in the turn's task after the query
  (`Block::attach` prepends the text and writes the notes), like the existing `@url`/image path. Query = typed text + last assistant
  message + `project_name(cwd)`. `balanced` / `tokens`, and any message that gets no block, take the old synchronous path unchanged.
- **Stored with the message, hidden from the page.** The block is part of the message's text (resume / rewind / branch / replay send the
  same bytes). Two notes: `preinject` (`skills`: names newly attached, `memory`: ids) and, if absent, `references`/`typed` = what the
  user wrote. `preinject::strip` / `user_text` cut the block; used in `transcript::messages` (what the page's `session.resume` shows),
  `transcript::typed_prompts` (ACP `/undo`, `/retry`), the rewind box (`session.rewind`'s `target_message.content`) and `/undo`.
  The page cuts `@file` expansions itself (its marker "--- Attached Context ---"); it knows nothing of `<context>`, hence the server-side cut.
- **Attached once per conversation.** `already_attached(messages)` reads the `preinject` notes off the live conversation; a skill already
  there is listed as `already provided earlier in this conversation` (no body); a memory id already there is skipped. A pointer-only block
  is still sent (about 30 tokens).
- **Knobs** (`budget.rs`): new `memory_preload`; `requests`: `skills_preload` 3, `memory_preload` 3, `skills_index` `CategoriesAndRetrieval`;
  `balanced` 0 / 0 / `Full`; `tokens` 0 / 0 / `NamesOnly`. Overrides `budget.skills_preload` (0-10) and `budget.memory_preload` (0-20)
  (`OVERRIDE_KEYS` 14 -> 16; 0 is valid: injection off).
- **Threshold.** The index's own floor (2.5) lets generic messages through ("refactor this function" -> `test-driven-development` 4.4,
  "explain this error" -> `dogfood` 2.9 on the 37 bundled skills; the first test run attached a 12 KB QA skill to "explain this error").
  Injection therefore uses `MIN_SKILL_SCORE` 5.0 and a skill after the first must reach 50% of the best one (the index keeps 2.5 / 30%, its
  goldens pin them). Weakest right answers on the bundled set: 5.2 (`github`, "open a pull request..."), 5.7 ("write a git commit").
- **`IndexStyle` in `skills.rs`** (`Full` = Hermes's text byte for byte, `Categories`, `Names`; `index_prompt_with(style)` replaces the bool).
  `Categories`: `## Skills` + "grouped by category (the number is how many skills a category holds); the skills that look relevant to the
  user's message are attached to it in a <context> block: follow them; for anything else, list a category with skills_list(category) and load a
  skill with skill_view(name)" + `<skill_categories>` lines `  - name (n)`. `Categories` and `Names` carry no "Err on the side of loading".
  Chosen at session start from `skills_index` and frozen with the prompt (CP3.2's mechanism). Subagents' base prompt maps `Categories` back to
  `Full` (their task gets no block).
- Display/doc: `GAPS.md`, `PORTING.md` (new `preinject.rs` row, diverged; `budget.rs` row), `docs/ARCHITECTURE.md`, `docs/PITFALLS.md`,
  `docs/TESTING.md`, `scripts/perf/README.md`, `docs/backend/PHASES.md` (knob table, baselines S6).
Files: crates/runtime/{src/preinject.rs (new), src/lib.rs, src/budget.rs, tests/turns.rs}, crates/tools/src/{skills.rs,lib.rs},
  crates/desktop/src/{transcript.rs,gateway.rs}, scripts/perf/preinject-check.mjs (new), scripts/perf/README.md, GAPS.md, PORTING.md,
  docs/{ARCHITECTURE,PITFALLS,TESTING}.md, docs/backend/PHASES.md, docs/backend/progress/phase-5.md
Tests (CARGO_TARGET_DIR=target/claude):
  - `cargo test -p hermes_runtime --test turns`: 117 passed / 7 ignored under each of `HERMES_TEST_PROFILE=balanced`, `requests`, `tokens`
    (105 + the 12 new). New: `s6_balanced_loads_the_skill_with_a_request_and_nothing_is_attached` (2 requests, user message byte for byte
    as typed, full list), `s6_requests_attaches_the_skill_and_needs_no_skill_view` (1 request, body in the first request's user message, no
    `skill_view`, categories prompt without the body), `s6_tokens_attaches_nothing_and_lists_names`, `the_page_sees_what_the_user_typed_not_the_block`,
    `a_skill_is_attached_once_and_the_system_prompt_never_changes` (second message says "already provided earlier", body once in the request,
    first message replayed identically, system prompt byte-identical), `resuming_and_rewinding_reproduce_the_stored_block` (reopened runtime
    replays the same bytes; after a rewind the same text gives the same block again), `short_generic_messages_attach_nothing` (9 messages incl.
    "hello", "explain this error", "refactor this function"), `a_relevant_memory_entry_is_attached_with_its_id` (frozen prompt unchanged),
    `the_knobs_can_switch_the_attachment_off_or_widen_it`, `a_broken_index_never_breaks_a_turn` (`index.db` is a folder: no index, the turn
    runs), `references_and_the_block_live_in_one_message`, `the_prompts_a_slash_command_writes_get_no_block`.
  - `hermes_runtime` lib 74 (new `preinject` tests: render, once-only, bounds, closing-tag escape, strip, share filter; `budget` preload
    knobs); `hermes_tools` lib 145 / 1 ignored (the `skills` test now checks the three `IndexStyle`s), memory 22, store 15, prompts 11;
    `hermes-desktop` lib 170 (new `the_block_the_index_attached_never_reaches_the_page`), `acp` 68; `cargo build --workspace` ok.
  - Live: `scripts/perf/preinject-check.mjs` against my own instance (debug build, port 47810, `target/perf`; stopped after): 14 checks ok
    (1 main request, block in front of the message, no `skill_view`, categories prompt, `session.resume` row = typed text and no row has
    the block, byte-identical replay, pointer on the second message, same system prompt, "hello" untouched, rewind box = typed text,
    re-attached after the rewind, `balanced` as typed with the full list). `perf.ps1 -Do start` printed node-not-found noise (the worktree
    has no `tools/node`; the main checkout's was used for the check); the instance itself started fine.
Request counts: new scenario S6 "a task a bundled skill covers": `balanced` 2 main requests (`skill_view` round trip) -> `requests` 1.
  S1-S5 unchanged (baseline table in PHASES.md has the S6 row).
Deviations from the plan:
- **"Already injected" is read off the conversation, not a JSON side file.** The `preinject` note on each stored user message lists the skill
  names / memory ids; `already_attached(messages)` scans the live conversation. Same effect, and rewinds, branches and compactions that drop the
  message also free the skill again (a side file would claim the model still has a skill it no longer sees). No file to keep in step.
- Injection uses a stricter skill threshold than the index's (5.0 and 50% of the best, `preinject::MIN_SKILL_SCORE`) because 2.5 attached a 12 KB
  QA skill to "explain this error" in the first test run. The index's own thresholds and goldens are untouched.
- A message that gets a block is added in the turn's task (like `@url`/image messages), not synchronously, because the query is blocking SQLite
  (ground rule 7); it is *built* synchronously so a refused `@file` still fails `start_turn`.
- A pointer-only block ("already provided earlier") is still sent when every relevant skill was attached before.
- New knob `memory_preload` (plan table lists only `skills.preload`); `requests` 3, others 0.
- Subagents keep the `Full` skills list under `requests` (they get no block).
For the reviewer:
- **Caching.** Check `Block::attach` / `user_message` / the held-message path in `begin_turn`: nothing goes to the system prompt; the block is
  stored in the message text; resume/rewind tests assert byte equality. Please re-read that a *failed* turn (`TurnFinished(Failed)` before the
  message is added) can't leave a half state: the held message is simply dropped, `running` cleared.
- **Display.** The page does not know `<context>`; every place that shows a user message's text must use `preinject::user_text`/`strip`. I
  covered `transcript::messages`, `typed_prompts`, `/undo` and `session.rewind`. Not covered (acceptable?): session search over `sessions.db`
  and the GPUI app (it prefers the `typed` note, so it is fine) and anything that exports the raw messages; compaction summaries and
  `session_search` see the block text.
- **Message text now varies per turn for plain messages**: a plain message that gets a block also gets a `references`/`typed` note (so `/undo`
  and the GPUI app have the typed text). Nothing else reads that note.
- **Threshold on a big skills set.** 5.0 / 50% was tuned on the 37 bundled skills (weakest right answer 5.2, strongest wrong 4.4); the CP5.1 review
  ran the goldens on Hermes's 232 skills but the *preload* floor was not rerun there. If live use shows misses, `preinject::MIN_SKILL_SCORE` is the
  one place (not yet a knob).
- **Memory duplicates the frozen prompt.** The system prompt still carries the whole MEMORY.md/USER.md snapshot, so an attached entry that
  existed at session start is a repeat (up to 3 per message, once per conversation); entries written during the session are new to the model.
  Phase 9 (memory tiers) is where this goes away.
- **Memory scoring with very few entries**: a lone entry scores ~0 in BM25 (no rarer word), so with 1-2 memories nothing is attached; the test
  seeds 6. Looks acceptable (small memories are in the prompt anyway).
- `index.query` refreshes first when dirty (up to hundreds of ms on a big skills folder) inside the 3 s limit; hand edits outside `commit()` are
  still only picked up at the next write/startup (CP5.1 note): a per-turn `refresh()` was not added (stat walk cost ~210 ms for 400 skills).
- A session started under `requests` whose profile is later switched to `balanced` keeps the categories prompt (no skill names) but gets no
  block: the model must `skills_list` / `skill_view` (the prompt says so). Same frozen-prompt trade-off as the budget wording.
- Not done: ACP session option for the profile (as before), no UI change (no patch needed: the block is cut server-side).

### Review (Opus 5.5)
Main had not moved (branch at c58479d on top of 8c1f5cc): no merge. `/code-review` not run (subagent); manual pass over the diff,
`preinject.rs`, `begin_turn`, compaction, session search and every place that shows a user message's text.
Review focus, with evidence:
1. **Caching holds.** Nothing per-turn reaches the system prompt: the block is prepended to the user message in the turn's task
   (`Block::attach`) and appended once; the `IndexStyle` is part of the prompt built at session creation and stored in the session
   record (`record.system_prompt`), so resume, a profile switch and a model switch (rebuilds from the then-current profile, as since
   CP3.2) behave like the budget wording. `a_skill_is_attached_once_and_the_system_prompt_never_changes` and the live check assert the
   system prompt is byte-identical across requests. Retry/continue (`text = None`) never inject; a turn injects at most once.
2. **Replay is byte for byte.** The block is stored in the message text; nothing recomputes it (resume / rewind / branch copy messages
   verbatim, the provider request is built from stored messages): `resuming_and_rewinding_reproduce_the_stored_block`, live check.
3. **The page shows the typed text**: transcript (`session.resume`, ACP history and export read `transcript::messages`), typed prompts
   (ACP `/undo`, `/retry`), rewind box, `/undo`, titles (heuristic and model both take the submitted `text`, never the stored message),
   session list (shows the title). **Found and fixed:** session search (`search_rows`, used by the sidebar's `/api/sessions/search`
   and the agent's `session_search`) indexed the block: every message with a skill attached matched that skill's words and snippets
   showed skill text. The cut now lives in `hermes_store::without_attached_context` (used by `row_for` and by `preinject::strip`, one
   implementation). Test `the_attached_context_block_is_not_searchable`; live check now asserts a body word ("curl") doesn't find the
   session and a typed word does. Also fixed: the `/review` briefing (`review_task`) quoted the block as the user's words; it now
   strips it and counts attached skills as loaded (they never go through `skill_view`).
Other findings, fixed:
- **"Already attached" after a compaction was wrong.** A compaction doesn't drop the summarized messages, it hides them from the model
  (`agent_visible = false`, kept for the page). `already_attached` scanned them, so a skill whose only copy had been summarized away
  was answered with "already provided earlier" and never re-attached. Now only agent-visible messages count (unit test).
- **The compaction summarizer read the block as the user's request**: `serialize` bounds each record (head + tail), so a 24k-char block
  pushed the actual request out of the summarizer's input. `serialize` now strips the block (test
  `the_summarizer_reads_the_request_not_the_attached_skills`). This answers the implementer's doubt about compaction summaries.
- **Threshold on Hermes's full skill set** (probe on `../hermes-agent` skills + optional-skills, ~230): "hello" scored 5.2 (`ascii-art`)
  and would have been attached; "explain this error" 5.2 (`anatomy`); "refactor this function" 5.2 (TDD). One search word says nothing
  about a task, so a message now needs 2 search words of its own before a skill is attached (`MIN_MESSAGE_WORDS`, via the new
  `index::word_count`; memory unaffected): "hello", "thanks", "continue", "ok, go on", "yes please" get no skill on any set. Real
  tasks on the full set: arxiv 14.7, architecture-diagram 11.9, youtube 11.5, systematic-debugging 8.3 ("fix the bug in main.rs"),
  requesting-code-review 8.4, pdf 7.1, microsoft-office 7.5 all pass. **Left:** "explain this error" -> `anatomy` (5.2) and "refactor
  this function" -> TDD (5.2) still attach on the 230-skill set (not on the bundled 37); "open a pull request on github" -> `github`
  scores 4.0 / 3.1 and is not attached (the model loads it). `MIN_SKILL_SCORE` should become a profile knob if live use shows misses.
- `strip` trimmed the typed text's own leading whitespace; now it removes exactly the "\n\n" joint. Skill names/categories in the
  block's attributes are escaped like bodies (a frontmatter `name` could close the block). The query read `record` inside the
  `conversation` lock; now one lock at a time. `/context` counted 0 skills tokens under `requests` (it looked for
  `<available_skills>` only); it now counts `<skill_categories>`.
- PITFALLS said `@url:`/image messages get no block: they do (the deferred path queries too); corrected.
Judged, no change:
- "Already injected" read off the conversation's notes instead of the plan's side file: better (rewind/branch/compaction stay
  consistent without a file to keep in step; the store schema is untouched; notes persist in the message JSON already used for
  `references`). PORTING row and module doc say diverged.
- A session started under `requests` then switched to `balanced`: categories prompt, no block; the prompt tells the model to use
  `skills_list` / `skill_view`, the same frozen-prompt trade-off CP3.2 accepted. Acceptable. Subagents keep `Full`: right, a
  delegated task gets no block.
- Turn failures: a held message is dropped with `running` cleared before append; a cancel during the query appends the message
  without a block and ends Cancelled (same as the `@url` path). The query is `spawn_blocking` under a 3 s timeout and also races
  the turn's cancel, so a locked/slow index can't stall a turn; `a_broken_index_never_breaks_a_turn` covers a missing index.
  UTF-8: `clip` counts chars, no byte slicing.
- `balanced` is byte for byte today's: S6 balanced asserts the user message is exactly the typed text and the full list; S1-S5
  hold under all three profiles. S6 under `requests`: 2 -> 1 main request, asserted with `assert_requests(.., 1..=1)` and no
  `skill_view` in any request.
Tests (CARGO_TARGET_DIR=target/claude): tools lib 145 / 1 ignored, memory 22, store 12 (1 new), prompts ok; runtime lib 76
(2 new), `turns` 117 passed / 7 ignored under `HERMES_TEST_PROFILE` unset, `balanced`, `requests`, `tokens`; desktop lib 170,
`acp` 68; `cargo build --workspace` ok, no warnings. Live: `preinject-check.mjs` against my own instance (debug, 47810,
`target/perf`, stopped after): 16/16 ok.
Docs: GAPS (Phase 5 done), PORTING (preinject row: search and summarizer cut), ARCHITECTURE, PITFALLS, scripts/perf/README,
PHASES (phase map ✅ 2026-11-12, *After 5* column: S1-S5 unchanged, S6 2 -> 1).
