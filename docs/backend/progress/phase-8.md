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

## CP8.3 — curator v2: clusters, low performers, one request per job, proposals in staging
Worktree: C:\Users\alexandros\repos\hermes-rs-p8   Branch: backend/p8-skills   Model: Sonnet 5.5 (subagent)
Cut on top of CP8.2 (66080d0). Uncommitted. The phase's last checkpoint.
Done:
- **Two curators behind `Runtime::run_curator`.** The deterministic pass (stale / archive by age) is
  unchanged. Then `budget.curator_mode` decides: `Loop` (`balanced`, `tokens`: today's agent loop, the
  code and its summary untouched) or the new `PerCluster` (`requests`; any profile can opt in with
  `budget.curator_mode: per_cluster`, or out with `loop`). New knob `curator_max_requests` (3; override
  range 1-20). Module `crates/runtime/src/curator/` (`cluster.rs`, `contract.rs`, `propose.rs`,
  `per_cluster.rs`, `gate.rs`, `seen.rs`); prompts in `crates/prompts/src/curator.rs`. Every module's
  doc says what it does and why.
- **Candidates**: the user's own library skills only (`propose::library`): not a repository's, an
  external folder's, a bundled one (the seeding manifest lists its files; a copy the user made is
  theirs), a pinned one, an archived or switched-off one. Not limited to agent-created skills (the
  loop's rule): a hand-written skill may be merged, with the user's approval.
- **Duplicate clusters** (`cluster.rs`): Jaccard of 5-word shingles of the body (front matter left out),
  exact, deterministic, single linkage, threshold **0.5**, at most 4 skills a cluster, skills under 30
  shingles never compared, at most 400 skills a run, pairs whose sizes can't reach the threshold skipped.
  Measured: the 39 bundled skills (the plan's 37 plus two nested) top out at **0.128**
  (`microsoft-office` / `pdf`), nothing clusters, and the test pins a 2x margin; Hermes's 193 skills
  (`skills` + `optional-skills`, informational test) have exactly one pair over 0.5 (two vendored
  library manuals, `pytorch-fsdp` / `unsloth`, 0.56). Why not BM25 from the index: it needs the index
  and measures topic, not copied text.
- **Low performers** (`propose::is_low_performer`): "uses" = the entries of `outcomes` this build
  understands (CP8.1: last 50, a verdict each; not the `uses` counter, not `attached`); at least 5 of
  them and fewer than half `clean` (recomputed, not the stored `clean_rate`). A repair also needs
  **evidence**: excerpts of the signals (`signals/<session>.json`) of the turns that went wrong (turn
  `T` and `T+1`), up to 6 uses; without any, the job is left out and no request is spent. A cluster's
  skills are not also repaired in the same run.
- **One request per job** (`propose.rs`, `per_cluster.rs`): the job's skills in full (secrets: a skill
  whose text masking would change is not sent at all; a job over the model's window, `JOB_TOKENS` 16k or
  half of what the window leaves, is not sent), no tools, text JSON contract (reply read as in
  `review_once`: fence or sentence tolerated, anything else malformed, **exactly one repair request**,
  then given up). At most `curator_max_requests` jobs a run; clusters first (best score), then repairs
  (worst rate). The governor is asked before the run and before every job (`Purpose::Curator`: nothing
  below 20% left, nor at the daily limit); a failed request ends the run and a daily-limit 429 teaches
  the ledger; `gate.rs` has an hourly cap (2 runs that send) and a back-off after a failed run (1 h,
  doubling to 24 h), in memory like the review scheduler's.
- **The answer** (`contract.rs`): `{action: merge|repair|none, keep, content, archive[], why}`, each part
  judged alone against the job: `keep` must be a listed skill and `content` a SKILL.md whose front
  matter says `name: <keep>`, else the whole answer is refused; an `archive` entry outside the job, the
  kept skill, a repeat, or any entry of a repair is dropped with its reason while the rest stands; a
  merge that archives nothing is refused. Result: `edit` of the kept skill + `archive` of each absorbed
  one. Merged `sources` = the union of the cluster's (the model's own `sources` is stripped) + this run's
  entry (`curator`, date, why).
- **Staging, not writing** (`propose::deliver`): `skills.plan` (validation, threat scan, size, description
  budget, a pin, staleness bases) then one staged item (`session_id` `curator`, reason, full diff:
  new text, and the absorbed skills' removal with "moves to the archive (restorable)"). Approving it goes
  through `apply_staged` (re-checks, base fingerprints: a skill edited since is stale and nothing is
  written). `curator.auto_apply` (config, default false; `Runtime::set_curator_auto_apply`, set from
  `apply_settings`) applies through the same `plan` + `apply_staged` unless `skills.write_approval` is
  on. Jobs judged (a proposal, "none", a refusal, an unreadable reply) are remembered 30 days in
  `curator-seen.json` unless the skills change; a job a waiting proposal touches is left out.
- **New skill-store op `archive`** (`tools/src/skill_manage.rs`): moves a library skill to `.archive/`
  after its text was merged into `absorbed_into`. Needed because `delete` must be the sole op of a call
  and `archive_for_curator` only takes curator-managed skills. Must come after every other op of a
  batch; every archive is checked (not pinned, not bundled / external / project, the absorber exists)
  before anything changes; a model's `skill_manage` call can't use it (`parse_call` refuses it).
- **Desktop** (`maintenance.rs`): same trigger (weekly after 2 idle hours, or Run now). When the knob is
  `per_cluster` it passes `CuratorModel` from `aux_models.curator`, else the review model, else the main
  model (window from the provider's `context_length` setting, else unknown); other profiles pass `None`
  as before (archiving only). `curator.auto_apply` is in the config defaults (Settings shows a switch).
  `providers::AuxModels` reads `curator`.
Files: `crates/runtime/src/{curator.rs,budget.rs,lib.rs,review_once.rs}`,
`crates/runtime/src/curator/{cluster,contract,gate,per_cluster,propose,seen,tests}.rs`,
`crates/runtime/src/review_once/contract.rs` (`extract` crate-visible), `crates/runtime/tests/turns.rs`,
`crates/prompts/src/{curator.rs,lib.rs}`, `crates/tools/src/{skill_manage.rs,curator.rs}`,
`crates/desktop/src/{maintenance.rs,gateway.rs,providers.rs,rest.rs}`, `PORTING.md`, `GAPS.md`,
`docs/{ARCHITECTURE,PITFALLS,TESTING}.md`.
Tests (all with `CARGO_TARGET_DIR=target/claude`): 
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`: green
  (hermes_tools 242, +1: the `archive` op).
- `cargo test -p hermes_runtime --lib`: 166 green (new: 23 in `curator/tests.rs` (clusters, bundled and
  Hermes sets, low-performer rule, contract, staging, auto-apply, refusals, selection, message), 2 in
  `gate.rs`, 1 budget knobs test).
- `cargo test -p hermes_runtime --test turns`: 194 passed / 7 ignored under the default profile and under
  `HERMES_TEST_PROFILE` balanced, requests and tokens (new: 14 curator tests after the `Phase 8, CP8.3`
  banner; `the_curator_archives_an_absorbed_skill_and_reports` now pins `Balanced`).
- `cargo test -p hermes-desktop`: 174 + 68 integration green. `cargo build --workspace`: green.
  Bundled-set measurement: `cargo test -p hermes_runtime --lib curator::tests::bundled -- --nocapture`.
Request counts: unchanged for every existing test. New (`requests`, `per_cluster_budget`): two
clusters -> 2 `curator` requests (cap 3), cap 1 -> 1, no clusters or low performers -> 0 (and no model
needed), unreadable reply -> 2 (one repair) then nothing on the next run, governor below 20% left -> 0
(81 and 100 of 100; 79 of 100 -> 2), failed request -> 1 and the next run 0 (back-off), `balanced` with
duplicate skills -> 0 (summary `skipped (no candidates)`, byte for byte), `balanced` opted in -> 1.
Deviations from the plan: none in what it asks. Choices the plan leaves open: Jaccard of shingles
(not BM25); a new `archive` skill op; text JSON contract only (no `response_format: json_schema`, so no
schema-refusal fallback: at most 2 requests a job); the merged skill replaces an existing member (no new
umbrella name); repairs need evidence; jobs judged are remembered 30 days.
For the reviewer:
- **`archive` is a new write path in `skill_manage.rs`.** Check `validate_ops` / `apply_ops_locked` /
  `Planner`: archives last, all pre-checked before the first write, refused in `parse_call`. A failure
  of an archive after another archive succeeded (an I/O error: the pre-check passed) leaves the first
  archived and the batch's snapshot restore recreating the second's folder next to its `.archive` copy;
  rare, restorable, not handled.
- **`own_hand` on a merge op.** The merged text's `sources` is built by `contract::merged_sources` and
  the op is `own_hand: true` so `keep_sources` doesn't revert it to the kept skill's old list. That is a
  second use of a flag documented as "the user's editor". The text has been through `keep_sources(None,
  ..)` (model-written `sources` stripped) first; staged ops keep the flag. Is that acceptable, or should
  the union be a separate field?
- **Who the curator may merge.** All the user's library skills, not only agent-created ones, because
  the plan says "the user's library skills". With the approval gate that seemed right; with
  `curator.auto_apply` a hand-written skill can be archived (restorable from `.archive`, and in the
  skills git history) without a click. The default is off.
- **The model's choice of `keep`** is any member (so a merge can keep the less-used name). The prompt
  asks for the one a search would look for. No new umbrella names (they'd need clash and category
  handling).
- **Repairs need evidence from the signals files.** A session whose signals file is gone (deleted
  session) or whose signals were all dropped gives none; the plan said "where available". I chose not
  to spend a request on a repair with nothing to repair from. Also the evidence maps a use in turn `T`
  to signals of turns `T` and `T+1`, but signal turns are renumbered by rewinds (informational per
  the signals module), so evidence can occasionally belong to another turn.
- **Desktop model fallback.** With no `aux_models.curator` and no review model, the curator uses the
  main model on the main provider, counted as `curator` in the ledger and held back below 20% (the
  governor's table is the only protection; Phase 4's "no side job on the main quota" applies to the
  approval judge only). The window is the provider setting's `context_length`, else unknown (the 16k
  job cap applies; a smaller real window would make the request fail and back the curator off).
  The aux `curator` row has no Settings entry (hidden slot).
- **The page**: `curator.auto_apply` shows as a switch under a new "curator" category from the config
  schema; I did not open the page to see where it lands. Staged curator items show in Pending like the
  others (kind `skill`, summary `skill: edit x, archive y`); not checked in the page (no live check:
  nothing the page calls is new).
- **Hermes's set**: one pair at 0.56 clusters over there (informational). If a threshold of 0.6 is
  preferred, that costs a hand-copied-and-edited pair around 0.5-0.6.
- **Not done**: the per-skill clean rate and the curator proposals have no UI beyond Pending; the
  "Run now" action's summary lines are the places the numbers show (`/api/actions/curator-run`, and
  `logs/curator/<run>/REPORT.md`, whose "Consolidation pass" section lists every job and refusal).
- The ledger counts requests a moment late, so "stops before the next job once under 20%" is asserted
  as a range (1 or 2).

### Review (Opus 5.5)
Main had not moved (nothing to merge). Read the whole diff and the new `curator/` module; no
`/code-review` in a subagent, so a manual pass. Verdict: sound design (local selection, zero
requests with nothing to do, every answer validated against its job, staging by default, the store's
own checks on plan and on apply); fixed the data-loss paths below.
Findings and fixes:
- **Clobbering an edit made while the request ran** (the real data-loss path). The staged item's
  bases were taken at *delivery*, after a request of up to minutes, so a user edit of the kept skill
  in between was overwritten by the model's merge of the old text (silently under `auto_apply`; under
  staging the diff showed it reverted). `propose::deliver` now takes the job and refuses a proposal
  when a touched skill's SKILL.md fingerprint differs from the one the model read (test
  `a_skill_edited_while_the_model_was_reading_it_refuses_the_proposal`, both modes). After staging,
  the existing base check makes the item stale (confirmed by `an_approval_finds_a_skill_edited_...`).
- **Archiving files the model never saw.** The model sees SKILL.md only, but `archive` moves the whole
  folder, so a merge could archive `references/` / `scripts/` the merged text needs. Now a skill with
  anything besides SKILL.md may be kept but never archived (`contract::judge` drops it; the message
  tells the model which one to keep), and a cluster with two such skills is left to the user without
  a request (`a_skill_with_its_own_files_may_be_kept_but_never_archived`). Hermes's one clustering
  pair (pytorch-fsdp / unsloth, both with `references/`) is exactly this case.
- **The implementer's doubt, batch rollback after an archive**: confirmed real (on Windows a rename
  fails whenever a file in the folder is open) and worse than stated: the record stayed `archived`
  and `archive_skill` had committed "user: archive x" mid-batch. The `archive` op now uses
  `move_to_archive` (no commit); `apply_ops_locked` remembers each archive's destination and record
  and, on a later failure, deletes the moved copy after the snapshot restores the folder and puts the
  record back. The batch commits once (`skill_manage: edit a, archive b`). Test
  `an_archive_that_fails_after_another_went_through_leaves_no_duplicate`; the archived copy is
  checked byte for byte.
- **`auto_apply` on hand-written skills**: now applies directly only when every touched skill is
  `created_by: agent` (`curator_managed`, the loop curator's own rule); the user's and chats' skills
  are staged even with it on. Reason: undoing an applied merge means moving the archived folder back
  by hand (this app has no restore route; `restore_archived` exists in `hermes_tools` only) and
  taking the kept skill's old text from the skills history.
- **Threshold 0.5 -> 0.35.** Jaccard of shingle sets is about `s / (2 - s)` for a shared fraction `s`,
  so the realistic duplicate (new title, same core steps, a few of its own each: 60% shared) scored
  0.43 and did not cluster (new test `two_skills_sharing_about_sixty_percent_of_their_steps_cluster`).
  Measured: the bundled 39 top out at 0.128 (the 2x margin test holds at 0.175); Hermes's 193 give
  the same single pair at every threshold from 0.3 to 0.55 (at 0.2 a third, axolotl, joins).
  Normalisation confirmed: front matter out, case folded, markdown/punctuation are separators, code
  counts as words; deterministic (sorted, union-find, ties by name); 400 skills a run, 200 KB a file.
- Docs: gate.rs said an unreadable reply backs off (it doesn't: it is remembered in `seen.rs`); it now
  says why a crash loop can't burst (`last_run_at` saved before the first request; judged jobs on
  disk). The checklist claimed a Settings switch for `curator.auto_apply`: the page's Settings only
  shows sections it knows, so there is none; the config description and the checklist say so.

Confirmed without change: zero requests when nothing to do (returns before the governor, no model
needed); requests = jobs (+1 repair each), cap 1-20 with 0 refused; governor before the run and before
every job, `Purpose::Curator`, a daily-limit 429 taught to the ledger; `seen.rs` corrupt file = empty
(re-asks once, bounded by the caps, then rewritten atomically); `archive` unreachable from a model:
`parse_call` refuses it (both call shapes), the review's `SkillAction` is create/patch only, staged
payloads are our own serialized ops; pinned / bundled / external / project skills never selected and
re-checked by `check_archive` at plan and apply; secrets: a skill whose masking would change is never
sent, so the model can't echo one back; the threat scan runs at plan and at apply; `own_hand` only
skips `keep_sources` (no taint or staging bypass), and the union is built from the files' own
`sources` after stripping the model's, so it can't be forged; the curator has no session, so no taint
applies: staging by default is the gate. `balanced` / `tokens`: the old loop test is pinned to
`Balanced` and still runs; `balanced` with duplicates sends 0. Ground rules: no store schema change
(side file `curator-seen.json`), file work in `spawn_blocking`, no prompt touched mid-session.

**Decision for the user: model fallback.** With no `aux_models.curator` and no review model, the
curator uses the main model on the main provider. Kept: it is bounded (the governor's 20% floor, 3
jobs = at most 6 requests a run, 2 runs an hour, weekly by schedule, judged jobs not re-asked for 30
days), and requiring an aux model would make the `requests` default do nothing for most users.
Documented in PITFALLS; set a background review model to keep it off the main one.

Left: no restore button for archived skills in this app (pre-existing; a folder move by hand); no UI
switch for `curator.auto_apply` (config file); evidence turn mapping across rewinds (informational).
Tests (`CARGO_TARGET_DIR=target/claude`): hermes_tools 243 (+1) and memory/store/prompts/llm green;
hermes_runtime lib 169 (+3); turns 194 passed / 7 ignored under default, balanced, requests and
tokens; hermes-desktop green; `cargo build --workspace` green.

### User checklist (Phase 8, all three checkpoints)
Build the webapp once if you haven't (`scripts/build-hermes-webapp.ps1`), start the app, and keep the
budget profile at **Requests** (the default; Settings > Model > Main > Budget profile).
1. **Duplicate skills -> one proposal.** In Capabilities > Skills create a skill with a dozen lines
   of steps, then a second one: copy the text, change the `name`, the title and a step or two (at
   least half of the text the same). Have a model set in Settings > Model > Main (optionally a
   "Background review" model in Settings > Model > Auxiliary: the curator then uses that one
   instead). Open Command Center > Maintenance and press **Run now** next to *Skill curator*. Within
   seconds the output box there says "per-cluster: 1 job(s) asked (1 request(s)): 1 proposal(s)
   staged". Open Capabilities > Skills: at the top, "Waiting for your review" shows one item "skill:
   edit A, archive B" with a diff (the merged text, and B moving to the archive).
2. **Apply it.** Press Apply: B disappears from the list, A holds the merged text, and A's front
   matter (open it in the skill editor) ends its `sources:` list with an entry from `curator`.
   B's folder is now in `%APPDATA%\hermes-rs\skills\.archive\B`, unchanged; to undo, move that folder
   back into `%APPDATA%\hermes-rs\skills\` (there is no restore button yet).
3. **Reject instead.** Repeat 1 with two new duplicates and press Reject: nothing changes, and Run
   now again lists that pair as "asked about in the last 30 days", with no request.
4. **Nothing to do, no request.** Run now on a library without duplicates: "nothing to merge or
   repair; no request"; the status bar's request count doesn't move.
5. **Pinned skills are left alone.** Pin one of two duplicates and Run now: no proposal.
6. **Auto-apply leaves your own skills alone.** Close the app, add `"curator": {"auto_apply": true}`
   to `%APPDATA%\hermes-rs\desktop-config.json` (there is no switch in Settings), start it, make a
   new duplicate pair by hand and Run now: the proposal still waits in "Waiting for your review"
   (only skills the background review created are merged without asking). Set it back to `false`.
7. **A skill that goes wrong gets repaired.** Use one of your own skills in chats and correct the
   agent about it ("no, ...") on 5 or more uses. Run now: a "repair" job asks once, and the proposal
   shows the corrected text. (Takes real use; the automated tests cover it.)
8. **Project skills stay out.** In a git repository put a copy of one of your skills in
   `.agents/skills/<name>/SKILL.md` and Run now: the repository's copy is never proposed or archived.
9. **Provenance and outcomes.** A merged or review-written skill's `sources:` list says which session
   or curator run each change came from; outcomes are in `%APPDATA%\hermes-rs\skills\.usage.json`
   (`outcomes`, `clean_rate`); the Skills screen does not show the rate yet.
10. **Other profiles unchanged.** Switch the budget profile to Balanced and Run now: idle skills are
    archived as before, and no request is sent.
