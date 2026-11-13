# Phase 8 progress

## CP8.1 — skill outcomes in `.usage.json`, provenance in SKILL.md frontmatter
Worktree: C:\Users\alexandros\repos\hermes-rs-p8   Branch: backend/p8-skills   Model: Sonnet 5.5 (subagent)
Cut from `main` at 847a8c2 (Phases 0-7 merged). Uncommitted.
Done:
- **Outcomes.** New `crates/runtime/src/skill_outcomes.rs` (pure `plan(..)`, `apply(..)`,
  `apply_rewind(..)`, module doc holds the rule) and, in `crates/tools/src/curator.rs`,
  `SkillRecord.outcomes: Vec<UseEntry {session, turn, outcome, at}>` (newest `MAX_OUTCOMES` = 50),
  `clean_rate` (share of `clean` among the kept entries, 3 decimals), `Outcome` =
  `clean | corrected | undone | errored` (+ an `Other(String)` that keeps a value a newer version
  wrote), `SkillStore::record_skill_use(name, session, turn, outcome, counts_as_use)` and
  `worsen_outcomes(session, turns, to)`. `uses` and `last_used` already existed (a `skill_view`
  bumps them); a skill the local index *attached* now bumps them too (via `counts_as_use`),
  otherwise a skill attached every turn would look idle to the curator.
- **What is a use / how the outcome becomes known** (zero requests; all in `skill_outcomes.rs`):
  a use is a skill the index attached to the turn's message (`Block.skills`, captured in
  `begin_turn`) or a whole-skill `skill_view` call that went through (from `TurnState::calls`),
  once per skill and turn. When the turn ends the use is written `clean`, `errored` (the turn
  failed, or a `Workaround` signal in it) or `corrected` (a `Denial` signal in it). A
  `Correction` signal in turn `T` makes the uses of turn `T-1` (only) `corrected`; a rewind
  (`rewind_as`, the signal's `turn`/`turns`) makes the dropped turns' uses `undone`. Outcomes
  only get worse; a cancelled turn is not judged (no entry). The correction signal that
  `analyze` makes for a "remember this" the agent already saved is ignored. A `/retry` after an
  error replaces the failed attempt's `errored` entry; a rewound turn's number can be reused.
  The file work runs in `spawn_blocking` and is awaited before `TurnFinished` so a test (and the
  page) sees it at turn end.
- **`.usage.json` robustness.** `SkillRecord` has `#[serde(flatten)] extra` (unknown fields kept),
  entries that aren't records are kept as they are, an unparseable file is copied to
  `.usage.json.corrupt` once before the next save. Old files (no outcomes) load unchanged
  (`outcomes` skipped when empty, `clean_rate` None).
- **Provenance.** New `crates/tools/src/provenance.rs` (+ `provenance/tests.rs`):
  `SourceNote {session, date, why}`, `append_source(text, note)`, `carry_sources(before, new)`,
  `sources_of(text)`, `clean_why`. It edits the frontmatter **as text** (only the `sources:` block
  lines change), then parses the result and checks "other keys equal, sources = old + 1"; any
  failure (no frontmatter, `sources` as a flow list or scalar, YAML that doesn't parse) leaves the
  text as it was (the write goes through without the entry). Copies the existing items' indent
  and the file's line ending, keeps a BOM, caps at 20 entries (oldest items go), skips an entry
  with the same session and `why`. `why`: one line, 160 chars, control/invisible characters out,
  threat scan (a hit becomes a fixed phrase so it neither blocks the write nor rides along).
- **Wiring.** `SkillOp` got `source: Option<SourceNote>` and `own_hand: bool` (serialized for
  staged ops; `parse_call` clears both for a model's call; `user_write` sets `own_hand`). Every
  SKILL.md write path (`create`, `rewrite`/edit, patch of SKILL.md, `write_file` to SKILL.md, and
  the `Planner` that makes staged diffs) goes through `finish_skill_md`: unless `own_hand`, a
  rewrite that drops or changes the old `sources` gets the old block back (`carry_sources`); then
  the op's own source is appended. Sources: `review_once/apply.rs` (session + the review's `why`),
  `SessionTools::skill_source` for the agent's foreground `skill_manage` (also when staged,
  `stage_skill_call_from`) and for the agentic review (`ReviewTools`); the curator's writes get
  none. The foreground `why` is a fixed phrase ("the agent changed it during this session"): the
  tool has no reason field and I did not change the tool schema.
- **Not done (left for later):** the rate on the Skills screen (no plugin/patch: the screen has
  no spot for it without a new patch against Hermes's skills component and a webapp build; `GET
  /api/skills` is unchanged, so the page protocol is untouched). `/skill-name` invocations are
  not counted as uses yet.
Files: `crates/tools/src/{curator,provenance,skill_manage,staging,review,lib}.rs`,
`crates/tools/src/provenance/tests.rs`, `crates/runtime/src/{skill_outcomes,lib,signals}.rs`,
`crates/runtime/src/review_once/apply.rs`, `crates/runtime/tests/turns.rs`, `PORTING.md`,
`GAPS.md`, `docs/ARCHITECTURE.md`, `docs/PITFALLS.md`.
Tests: see the list below (all with `CARGO_TARGET_DIR=target/claude`).
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`:
  green (hermes_tools 206 incl. 13 provenance, 7 skill_manage, 7 curator new).
- Turns tests added: `an_attached_skill_is_clean_until_the_user_corrects_the_next_turn`,
  `a_loaded_skill_counts_once_and_an_undo_makes_it_undone`, `a_turn_without_a_skill_leaves_no_outcome`;
  `a_skill_the_session_loaded_is_patched_with_a_strict_diff` also asserts the review's `sources`
  entry and the `corrected` outcome. Unit tests of the rules in `skill_outcomes.rs`.
Request counts: unchanged (outcomes and provenance cost no request; the new turns tests assert
the main request counts).
Deviations from the plan: the Skills screen rate is not shown (optional in the checkpoint).
For the reviewer:
- **Provisional outcomes.** A use is written `clean` at turn end, so the rate dips when a
  correction arrives. Only turn `T-1` is blamed for a correction in `T`; a skill used earlier
  with an unrelated turn in between is not. Is that the rule you want? (Alternative: blame every
  skill loaded since the last user message that wasn't a correction.)
- **`Correction` covers rules too** ("always use X"). Those count as a correction of the
  previous turn. A "remember" the agent already saved is excluded; other weak signals are not.
- **Attached skills now bump `uses` / `last_used`**, so `learning.rs`'s "learned" filter
  (`uses > 0`) now shows bundled skills that were only ever *attached*, on the Star map. Check
  that this is acceptable.
- `load_file` treats any top-level entry that deserializes as a `SkillRecord` as one, so a
  non-record object entry (`_meta: {v: 9}`) becomes a record with defaults plus its fields in
  `extra` (kept, but gets the default fields added). A non-object entry is kept untouched.
- `record_skill_use` skips a skill `find_any` doesn't know (deleted or an external name not in
  the catalog). A skill with the same name in a different scope (CP8.2) shares one record.
- `carry_sources` runs for every non-`own_hand` rewrite, including a *patch* whose old/new strings
  touch the `sources` lines: the patch's change to the block is reverted silently (the rest of the
  patch applies). Staged ops and the `Planner` do the same, so the diff the user sees is what is
  written.
- `SkillOp.source` is serialized into the staged payloads: a staged op keeps the session and the
  date of when it was staged, not of when it is approved.
- `skill_view` reads in `GET /api/skills/*` return the whole SKILL.md including `sources`; the
  `why` text therefore re-enters the model's context when it loads a skill (cleaned, capped,
  threat-scanned).
- Not verified: a live check in the page (nothing the page calls changed); the desktop build
  and tests are listed in the final report of the session. The worktree has a junction
  `target/hermes-src` to the main checkout's export (gitignored by `/target`).

### Review (Opus 5.5)
Verdict: sound design (pure `plan`, zero requests, text-edit provenance with a parse check);
five problems fixed, the rest confirmed.
Fixed:
- **`uses` changed meaning** (the implementer's own doubt was right): attached skills bumped
  `uses`/`last_used`, so every bundled skill the index ever attached became "learned" on the Star
  map (`learning.rs`: `uses > 0`). Now counted apart in `attached` / `last_attached` (omitted from
  the file while 0/None); `last_activity` and the curator's "never used" check count them, so the
  curator still sees an attached-every-turn skill as active. `uses` is a whole-skill load again.
- **`.usage.json` data loss.** A file that exists but can't be read (locked, AV) loaded as empty
  and the next write replaced every record with one: now nothing is written. One field of the
  wrong type (`"uses": "3"`, `null` count, `"state": "Stale"`, `"pinned": "true"`, a `turn` as a
  string) made the whole record "foreign", and the next update for that skill overwrote it with
  defaults (pinned lost): fields are now coerced (as Hermes's `_int_or_zero`) or dropped one by
  one. A BOM made the whole file "corrupt" (replaced): stripped. An empty file no longer takes the
  single `.corrupt` slot. A non-record object (`_meta: {v: 9}`) is kept untouched (was re-saved
  with default record fields); a skill-named one gives its fields to the record. The `.corrupt`
  copy is one file, written once: no loop, can't fill the disk.
- **Provenance forgeable.** `carry_sources` only acted when the *old* file had a block-form list:
  a model could create a skill with forged `sources` (any form), or replace a list with a flow
  one, and it stuck. Now `keep_sources(before, new)` makes `sources` what the file had (none for a
  new skill) for every non-user write, in any YAML form (generic key extent + the same parse
  check); a write where that can't be verified is refused with a clear error, not let through.
- **`file_path: "./SKILL.md"` (or `/SKILL.md`) bypassed the guard** (patch checked
  `file_path.is_none()`, write_file `== "SKILL.md"`; the patch also skipped frontmatter
  validation). `parse_call` normalises these to `SKILL.md`; patch checks the label. Also closed a
  pre-existing escape: `C:/SKILL.md` / `/x/SKILL.md` passed `validate_file_path` and joined to an
  absolute path outside the skill folder; absolute paths and `:` are refused now.
- A skipped provenance entry (unparseable frontmatter, flow/scalar list) is logged, not silent.
Confirmed: the outcome rule (T-1 only; no double blame; worsen is idempotent; cancelled turns
unjudged; attached + viewed in one turn = one use; side turns, subagents, review and curator never
reach `start_turn`'s outcome code; compaction keeps user messages user-visible so turn numbers
hold); `clean_rate` is over the kept 50, `None` at 0 (fits CP8.3's ≥5 rule); `why` is one
JSON-quoted line (`: `, `- `, `---`, `#`, `&`, `!!`, U+2028/NEL tested); staged ops replan with
the staged source; system prompt and injected block format untouched; request counts unchanged.
Tests added: every historical `.usage.json` shape (M4's `{views, uses, last_used}`, BOM,
wrong types, null, non-records, empty, 2000 records), an unreadable file left alone, forged
`sources` in five forms, a user's flow list carried, byte-for-byte fixtures (mixed CRLF/LF, no
trailing newline, comments, anchors, tags, `---` in body, `sources_old`, 1 MB body, tabs),
hostile `why`s, the SKILL.md path variants and the absolute-path refusal.
Left: two processes (app + ACP) writing `.usage.json` can still lose an update (in-process lock
only; pre-existing, now hit per turn with a skill); a file lock would fix it. `/skill-name`
invocations still aren't uses.
Tests: hermes_tools 214 (+ memory/store/prompts/llm) green; hermes_runtime lib 137 + turns 170
green (default, balanced, requests, tokens); hermes-desktop and `cargo build --workspace` green.

## CP8.2 — project skills: `.agents/skills`, read-only fallbacks, index scope
Worktree: C:\Users\alexandros\repos\hermes-rs-p8   Branch: backend/p8-skills   Model: Sonnet 5.5 (subagent)
Cut on top of CP8.1 (e2e46e4). Uncommitted.
Done:
- **Discovery** (`crates/tools/src/project_skills.rs`, new; its module doc holds the rules). `find_root(cwd)`:
  canonical working folder, nearest ancestor with `.git` (a folder, or a worktree's file); a nested repo
  is its own project; no repo means no project skills; a repo at the home folder is not a project.
  `skill_dirs`: `.agents/skills` (writable), `.hermes/skills`, `.claude/skills` (read-only), each only if
  it is a plain folder all the way (a symlink or junction anywhere in the path = not there).
  `catalog(root)`: bounded walk (depth 4, 200 skills per folder), plain files only, 100k limit, threat
  scan (cached per file stamp), a name the skill rules refuse is skipped, one skill per name, precedence
  `.agents` > `.hermes` > `.claude`. Category shown as `project` / `project/<folder>`.
- **D5 name clashes, as implemented and tested**: the user's library wins over every project folder (D5
  only fixes the order of the project folders and calls the fallbacks "lower priority"; I added "library
  first" so a cloned repo can't replace a skill the user relies on: see the reviewer list); `.agents` >
  `.hermes` > `.claude`; a shadowed copy is invisible (not listed, not indexed, not attached); creating a
  project skill over a read-only copy of the same name is allowed (that customizes it); two repositories
  may use one name.
- **Scoped store** (`skills.rs`): `catalog_for / catalog_all_for / find_for / find_any_for / list_tool_for
  / view_tool_for / index_prompt_for` take the session's repository root; the old methods are the user's
  library (so the Skills screen, the curator, slash commands, bundles and the agentic review are
  unchanged and never see a project skill). `SkillEntry.project`. With no project skills the index prompt
  is byte-identical to before (tested for all three styles); with them: a `project` category and one note
  saying they are the repository's files, not instructions from the user. A project skill's view result
  says `"scope": "project"`, skips `.usage.json` (`bump`), re-scans the text and support files, reads
  support files only when they resolve inside the skill, and lists support files without following links.
- **Index scope** (`index.rs`, `SCHEMA_VERSION` 3, in `index.db`, not `sessions.db`): `skill_doc.scope` and
  `skill_files.scope`; a query names its repository (`IndexQuery::project_root`) and the SQL is
  `scope IN ('global', <that>)`, so another repository's rows are never read. A repository is indexed once
  a query names it (kept for the process, 32 at most) and its files' `stat` signature is checked at each
  such query, so edits made outside the app show up. Active-name logic per scope (library first; each
  repository has its own names). A project skill failing the scan or name rules is remembered as not
  indexed. `SkillHit.project`.
- **Session scope** (`tools/src/lib.rs`): `SessionTools::project_root()` is computed once in `new` from the
  session's folder. The shell's `cd` does not change it (decision: stable, and an agent that `cd`s into a
  cloned repo doesn't pick up its skills). Subagents `inherit_project` from the parent (`delegate.rs`,
  also when the parent's shell is elsewhere); the curator has none (`without_project`, its prompt too).
  The frozen system prompt is built for the repository of the session's folder when it is made and is
  never rewritten (a model switch or compaction rebuilds it for the repository then; a workspace move
  doesn't: documented in ARCHITECTURE).
- **Preinject / review** (`preinject.rs`, `review_once*`, `review_schedule.rs`): the query carries the
  root; hits from a repository are re-read through `project_skills::read_skill_md` (inside the repo, plain
  file, scan) and labelled `scope="project"` with one more sentence in the block's preamble; the review's
  loaded and related skills are read the same way (the packet heading says "project skill", and a
  `## Project` section tells the model it may use scope project). The reply's `scope` field (`global` |
  `project` | null) is in the schema, the parser (ignored on a patch, an unknown value refuses that op)
  and the prompt. null = `review_once::evidence_scope`: project only when the session's file tools and
  terminal `workdir`s touched files of that one repository and nothing outside it (scratch output
  ignored), else global. `scope: project` in a session with no repository is refused.
- **Writes** (`skill_manage.rs`): `SkillOp.project` (session root) and `SkillOp.scope`; both are cleared
  from a model's call (`parse_call`), so the model's own `skill_manage` can patch / write support files /
  delete `.agents` skills it sees but only create global ones (tool schema untouched). A project `create`
  goes to `<root>/.agents/skills/[category/]name` after `write_dir_usable` (a file, symlink or junction at
  `.agents` or `.agents/skills` refuses; the skill folder must not exist as a file or be non-empty; the
  path must resolve inside), with the same name / frontmatter / size / threat rules; the folders a refused
  create made are removed again. Every other action refuses a read-only project skill (the message says
  how to customize it) and re-checks `ensure_inside` against links; delete only from `.agents/skills`, only
  a plain folder. Nothing is committed (`commit` and `record_write` are skipped, only the index hook
  runs), nothing goes in `.usage.json`. Staging: `stage_skill_call_in`; staged ops and `FileBase` carry the
  root, the diff says "project skill, in .agents/skills"; a tainted session stages as before; applying
  re-validates the root (`is_root`).
- **Untrusted content** (`taint.rs`, `lib.rs`, `path_policy.rs`): a project skill put in front of the model
  (`skill_view` with `scope: project`, or the index attaching it in `begin_turn`) marks the session
  tainted ("a project skill (name)"): memory and skill writes stage, flagged commands always ask, Yolo
  only warns (Phase 6 rules, unchanged). `skills_list` does not taint (names and descriptions only). A
  command naming a file under `scripts/` of any of the three project skill folders is flagged
  (`DESC_PROJECT_SKILL_SCRIPT`) in every mode that flags (not Yolo); unlike global skills nothing is
  exempt. Skills never auto-execute anything.
- `scan_skill` no longer descends into links or past depth 12 (also for global writes).
Files: `crates/tools/src/{project_skills.rs,project_skills/tests.rs,skills,skill_manage,index,staging,taint,path_policy,lib}.rs`,
`crates/runtime/src/{lib,preinject,delegate,curator,compress,review_schedule,review_once}.rs`,
`crates/runtime/src/review_once/{apply,contract,packet,tests}.rs`, `crates/prompts/src/review.rs`,
`crates/desktop/src/transcript.rs` (a test literal), `crates/runtime/tests/turns.rs`, `PORTING.md`,
`GAPS.md`, `docs/ARCHITECTURE.md`, `docs/PITFALLS.md`.
Tests (all with `CARGO_TARGET_DIR=target/claude`):
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`: green
  (hermes_tools 238; new: 10 discovery/link/hostile tests and 10 scoped store / tools / index tests in
  `project_skills/tests.rs`, 4 `SessionTools` tests in `lib.rs`).
- `cargo test -p hermes_runtime`: lib 142 (new: contract scope, `evidence_scope`, project apply / stage /
  refuse, preinject label and read), `--test turns` 187 (7 ignored as before) including 10 new: attach only
  in the own repository with two repositories and a plain folder open at once, a repo without skills
  unchanged, `skill_view` / `skills_list` scoped and tainting, a hostile skill never loaded or attached
  (also when edited after indexing), subagent, review create with scope project lands in `.agents/skills`
  (provenance, packet `## Project`, schema enum), default scope by files, project scope without a repo
  refused, tainted: staged then applied into the repo, a review in one repo never reads another's skill.
  Also under `HERMES_TEST_PROFILE` balanced, requests, tokens: see the lines at the end of this section.
- `cargo test -p hermes-desktop`, `cargo build --workspace`: see the lines at the end of this section.
Request counts: unchanged for sessions without project skills (the old turns tests pass untouched, and
`a_repo_without_project_skills_changes_nothing` repeats S6 in a repo with no skills: 1 main request). A
session with a project skill: 1 main request per turn under `requests`, none extra (project skills cost no
request); `a_repositorys_skill_is_attached...` asserts 5 main requests for 5 turns over 3 sessions.
Deviations from the plan: none in scope. Additions the plan doesn't state: the library-first name rule,
taint on delivery, the script flag for project skill scripts, the session scope frozen at the start, and
the project hints in the review prompt and packet (the evidence prompt is about 150 tokens longer;
`reserved_tokens` measures it, so the packet budgets follow).
For the reviewer:
- **Taint on delivery** is my conservative reading of "decide conservatively". The cost: in a repo with
  project skills the session is tainted as soon as one is attached or viewed, so every memory / skill write
  (including the review's project creates) goes to Pending and flagged commands always ask. It matches how
  Phase 6 treats other third-party text, but it will be noisy for a user's *own* repo skills. An
  alternative that keeps the safety: trust a skill whose SKILL.md fingerprint is one *we* wrote in a clean
  session (a `project-skill-trust.json` next to `taint.json`). Not built; your call.
- **Library wins a clash** is an addition to D5 (D5 says only that the fallbacks rank below `.agents`). The
  opposite (project overrides library) would let a cloned repo replace e.g. a bundled skill the agent trusts.
- **The system prompt index** lists project skill names and descriptions (scanned, 60 characters) under
  `project`, built once when the session is made; the descriptions are not a taint source. Under
  `requests` it is a count only. After a workspace move the frozen prompt still names the old repository's
  skills (already in the context, so no new leak) while tools and index use the new repository.
- **Not in the model's tool schema**: the model can't pick `scope` in `skill_manage` (cleared in
  `parse_call`), so foreground creates are global; only the evidence review makes project skills. Enough for
  CP8.2, or should the schema grow `scope`? (A schema change makes every request bigger.)
- The agentic review (`nudges`, `agentic`, `/refine`) and the curator are library-only by construction; the
  agentic review's forked agent sees project skills only through the conversation it replays.
- Edges tested: `..`, nested repo, worktree (`.git` file), a junction into a repo and one out of it, no
  repo, a junction as `.agents`, as a skill folder, as `references` / `assets`, a link loop, a name like
  `../../etc/passwd`, injected text, a 120k file, a file where the skill folder goes, a non-empty non-skill
  folder, `.agents` as a file. Home as a repo is code only (needs a repo at the real home). The junction
  tests call `cmd /C mklink /J` (Windows only, like the rest of the suite).
- `ensure_inside` is check-then-write (a TOCTOU window if something swaps a folder for a junction between the
  check and the write); the attacker would need to run code in the repo already.
- A `skill_manage` batch mixing a library op and a project op: decided by name (`project_ops`); the history
  commits only the library ops. Not tested beyond single-op batches.
- `LocalIndex.projects` is in memory only: after a restart a repository is indexed again at its first query
  (rows of repositories nobody queried this run are dropped at the next refresh). `sync_project` stats up
  to ~600 files per query; fine for the sizes I tried, not measured.
- Not verified: a live check in the page (nothing the page calls changed: `/api/skills` is the library only,
  project skills have no UI and no `/name` command). No patch to Hermes's frontend.
- A project skill in a category folder shows as `project/<folder>` in `skills_list`; the page never sees it.
- Results: turns 180 passed / 7 ignored under the default profile and under HERMES_TEST_PROFILE balanced, requests and tokens; hermes_runtime lib 142; hermes-desktop 174 + 68 integration; hermes_tools 238; cargo build --workspace green.

### Review (Opus 5.5)
Verdict: sound. The scope is enforced where it must be (the index's SQL `scope IN ('global', ?)`
before `LIMIT`, so no post-filter starves results; every session-facing lookup goes through
`*_for(project_root)`; the page's routes, slash commands, the curator and `.usage.json` use the
library only). Four leaks/robustness problems fixed, the rest confirmed.
Fixed:
- **Review packet carried repository text into an un-held write.** The related-skills lookup can
  put a project skill in the packet of a session that never saw one (so wasn't tainted); the
  review's writes then applied directly. Now a packet with a project skill (loaded or related)
  holds every write like a tainted session ("the review read a project skill (name)"). The
  turns test `the_reviews_default_scope_follows_the_files_the_session_touched` now checks the
  staged item is a library create and applies it.
- **`skill_manage` patch failure leaked untainted text**: `file_preview` (500 chars of a project
  skill's SKILL.md or an unscanned support file) went to the model without taint. The result now
  carries `name` + `"scope": "project"`, which the tool layer already taints on.
- **Batch rollback could copy a link's target into the repository**: `copy_dir` (snapshot and
  restore) followed file symlinks, so `references/key -> ~/.ssh/...` came back as a real file after
  a failed batch (and a junction failed the snapshot). `copy_dir` now skips links (also the Hermes
  import: safer, a link there was copied as its target before).
- **Unbounded reads**: `skill_view` of a project skill's support file read any size (now ≤ the
  SKILL.md byte cap, `read_support`); the walks (`skill_files`, `linked_files`) had per-folder caps
  but no total: 1 000 folders per walk now (runs at every query that names the repository).
- `evidence_scope` (canonicalizes paths) moved into the review's `spawn_blocking`.
Confirmed: `find_root` (canonical cwd, `.git` dir or file, nested repo wins, home excluded,
filesystem/UNC root stops, `GIT_DIR`/bare repos ignored = no project); `skill_view`'s name check
refuses `a/../b`, and absolute `file_path`s fail `starts_with`; staged ops carry their root and
re-check `is_root` on apply (a moved/deleted repo makes the item stale, never redirected); `.agents`
writes refuse links, non-empty folders, files; index v2 → v3 drops and rebuilds; at most 32
repositories' rows (others dropped at the next refresh); evidence default = project only when every
touched path is inside the one repository (two repositories → global); balanced/requests/tokens
request counts unchanged.
**D5 name clashes: kept library-first.** D5 orders only the project folders; the plan's "project
specialises global" use case is still served by a different name, while the reverse rule would let a
cloned repo shadow `git` / `code-review`. Matches Claude Code (personal over project). Project names
are lowercase ASCII, so case/lookalike clashes can't arise; documented in the module doc.
Left: taint for the user's *own* repo skills is noisy (writes go to Pending); a trust list keyed by
fingerprints we wrote is the right fix later, not now. The system-prompt index shows 60 scanned
characters of a project skill's description without tainting (same as `skills_list`). `find_root`
and the prompt's catalog walk run where the session is made (bounded, as the existing prompt build).
Tests: hermes_tools 241 (+3: huge support file + preview scope, rollback with a junction, folder
cap), memory/store/prompts/llm green; hermes_runtime lib 142, turns 180 (7 ignored) under default,
balanced, requests, tokens; hermes-desktop and `cargo build --workspace` green.
