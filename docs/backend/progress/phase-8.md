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
