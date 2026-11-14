# Phase 11 progress

## CP11.1 — Short tool schemas for every profile, and the `hermes-tools-guide` skill
Worktree: C:\Users\alexandros\repos\hermes-rs-p11   Branch: backend/p11-deferred   Model: Sonnet 5.5 (subagent)
Cut from `main` at e2011ad (Phases 0-10 merged). Uncommitted.
Done:
- **Measured** (`prompts/tools/*.json`, bytes as serialised into the request: one `{"type":"function",
  "function":{name, description, parameters}}` entry per tool, compact JSON; tokens = bytes / 4, the project's
  estimator (`ContextBreakdown`, "characters / 4")). **The plan's sizes were the pretty-printed files** (6.4 /
  3.9 / 2.8 / 2.7 KB on disk); on the wire the four were 3.5 / 3.4 / 3.7 / 2.5 KB, and `skill_manage`'s is mostly
  its six-way `anyOf` structure, which has to stay. "Descriptions" = every `description` string in the schema
  (top level and parameters), in bytes.

  | tool | descriptions before | descriptions after | serialised before | serialised after | ~tokens before | ~tokens after |
  |---|---|---|---|---|---|---|
  | `skill_manage` | 1,686 | 1,362 | 3,478 | 3,154 | 869 | 788 |
  | `session_search` | 2,648 | 1,510 | 3,424 | 2,284 | 856 | 571 |
  | `memory` | 2,873 | 1,642 | 3,656 | 2,413 | 914 | 603 |
  | `delegate_task` | 2,170 | 1,338 | 2,534 | 1,680 | 633 | 420 |
  | **the four** | 9,377 | 5,852 | 13,092 | 9,531 | 3,273 | 2,382 |
  | the other 14 | 12,219 | 12,219 | 17,409 | 17,409 | 4,352 | 4,352 |
  | **all 18 tools** | 21,596 | 18,071 | 30,501 | 26,940 | 7,625 | 6,735 |

  Saved: 3,561 bytes, about 890 tokens, per request, in every profile (the tool list is the same in all
  three). In an actual request (`turns.rs`, array brackets and commas included): the four 9,591 bytes, the
  whole `tools` array 27,229. The rest of the other 14 tools, largest first: `search_files` 2,388, `terminal`
  1,819, `clarify` 1,739, `run_script` 1,734, `read_file` 1,528, `todo_list` 1,292, `process_manage` 1,168,
  `web_extract` 1,126, `write_file` 1,085, `patch` 1,040, `vision_analyze` 810, `web_search` 753,
  `skill_view` 623, `skills_list` 304 (unchanged).
  **Consequence for Phase 11:** the plan's "~8k tokens per request" cannot come from shorter text (the whole
  list is 6.7k tokens now); only CP11.2 (not sending the rarely used tools at all) can reach it.
- **Four schemas shortened, only `description` strings changed** (`prompts/tools/{skill_manage,
  session_search,memory,delegate_task}.json`; the files were re-serialised with `indent=2`, so the diff of
  `memory.json` and `session_search.json` also shows inline arrays unfolded). Each top-level description ends
  with ` Details: skill_view hermes-tools-guide` (39 bytes). What stayed, per tool:
  - `skill_manage`: operations array, atomic rollback, the five actions and what each is for, `create` before
    that skill's other ops, `patch` preferred and `content` alone REPLACES the file (skill_view first),
    `delete` sole op, existing skills edited wherever they live, the 60-char description rule and its shape,
    never the category name, "lessons not logs"; parameters: name rule (lowercase, hyphens/underscores, max
    64), the one-shape-per-action rule, `write_file` path rule (relative, first segment), `absorbed_into`
    (must exist), `replace_all`, `new_string` empty deletes.
  - `session_search`: the four shapes picked by args (discovery / scroll / read / browse), history-only
    warning with the "inspect the direct source first, never conclude not found" rule; every parameter with
    its limits (`limit` max 10, default 3; `window` clamp [1, 20], default 5; `exclude_session_ids` cap 20;
    `after` / `before` formats and "only if the user names a time frame"; `sort`, `detail`, `role_filter`).
  - `memory`: one call per target through `operations`, atomic, limit checked on the FINAL result (remove/patch
    + add together), reissue ONE batch when full, what belongs (every session) and what does not (task
    learnings go to a skill), one call one tier, `note_read` a call by itself; parameters: targets, which
    fields each action needs (`content`, `old_text`), `tier` incl. "applies to every action", `tags` / `links`
    at most 8 with the link kinds, `note` (project/archive, read with `note_read`).
  - `delegate_task`: everything goes in `tasks`, one entry one subagent, parallel, joined results, when not to
    use (mechanical, single call, needs the user, durable work), `context` is the only channel (also language
    and tone), summaries are SELF-REPORTS to verify, children cannot call `delegate_task` / `clarify` /
    `memory`, the limit follows the budget profile and a longer list is refused with the limit.
  - **Moved to the skill only**: rationale, examples, "one rule per lesson", "extend a references/ file before
    adding", "children cannot close tracked work", "stop halts running subagents (interrupted)", `role_filter`'s
    "'tool' alone" form, the scroll-forward / scroll-backward hint (the result carries it), the FTS5 OR / NOT /
    prefix recovery (the empty-result message carries it), the 2,200 / 1,375 / 1,500 character limits (the
    full-store result states them), the 20-operation batch cap (error message).
  - One correction rather than a shortening: Hermes's `session_search` `session_id` text said "Must be paired
    with around_message_id", which `recall.rs` does not enforce (`session_id` alone reads the whole session,
    as the top-level description already said). The short text says what the code does.
- **`hermes-tools-guide`** (`assets/skills/autonomous-ai-agents/hermes-tools-guide/SKILL.md`, 23 KB): frontmatter
  as the Hermes skills (name, 59-character "Use when ..." description, version, author, license,
  `metadata.hermes.tags` / `category`), then per tool: when to use, parameters (generated from the old schema's
  own parameter text, so every one is named and keeps its wording), examples, gotchas, and the **old long
  description word for word** with the Hermes source file named in the intro. It is in `assets/skills`
  (embedded by `RustEmbed`, seeded into the skills folder like the others; `skills/` mirrors Hermes and is
  not touched). Same loading path as every bundled skill: in `skills_list`, `skill_view`, the index (Full:
  a line; Names: the name; Categories: one more in `autonomous-ai-agents`'s count), never injected into the
  prompt. In the seeding manifest, so `SkillStore::is_bundled` is true and the curator's library leaves it
  out (`propose.rs`). Category `autonomous-ai-agents` (existing), so no new category line in the `requests`
  prompt.
- Tests: `hermes_prompts` `tools.rs` (budgets per schema and for the block, pointer sentence on exactly the
  four, shape snapshot `crates/prompts/tests/fixtures/tool_shapes.json` = the old schemas without descriptions,
  must-say phrases, parameter descriptions all still present); `hermes_tools` `skills.rs` (guide bundled,
  `is_bundled`, indexed in Full and Names, `skill_view` returns it, a section per tool, **every property name
  of the four schemas appears in the skill**, original text kept); `turns.rs`
  `the_long_tool_schemas_are_short_in_every_profile` (1 request, the pointer, four <= 9,800 bytes, block <=
  27,500, in each profile).
- Docs: `PORTING.md` (row, diverged), `prompts/tools/README.md`, `GAPS.md`, `docs/ARCHITECTURE.md`,
  `docs/PITFALLS.md`, `docs/TESTING.md`, `docs/backend/PHASES.md` (status line), `crates/prompts/src/tools.rs`
  header, the curator test's bundled-skill count comment (40).
Files: `prompts/tools/{skill_manage,session_search,memory,delegate_task}.json`, `assets/skills/autonomous-ai-agents/
hermes-tools-guide/SKILL.md`, `crates/prompts/src/tools.rs`, `crates/prompts/tests/fixtures/tool_shapes.json`,
`crates/tools/src/skills.rs` (test), `crates/runtime/tests/turns.rs`, `crates/runtime/src/curator/tests.rs` (comment).
Tests (all with `CARGO_TARGET_DIR=target/claude`, 2026-11-14): see the end of this section.
Request counts: unchanged for every scenario (no logic changed; the full `turns` suite passes under the default,
`balanced`, `requests` and `tokens` profiles). Request body: `tools` 30.5 KB -> 26.9 KB in every profile.
Model: Sonnet 5.5 (subagent)

For the reviewer:
1. **Is anything a model needs to form a correct call gone?** Diff `prompts/tools/*.json` against `HEAD` for the
   four, with the list under "What stayed" in hand. The shape is pinned by a test, so the question is only
   about `description` text. Candidates I judged droppable: `skill_manage`'s "one rule per lesson, references/
   named by topic (extend before adding)" and "skill_view() shows format conventions" (the first moved to the
   skill; the second is dropped from the schema: the model learns to `skill_view` a skill from the skills block
   anyway); `memory`'s "the response reports current/limit chars and confirms completion" (so the model might
   repeat a finished call: the result says so; guide keeps it); `delegate_task`'s "children cannot close
   tracked work".
2. **Budgets are above the brief's "~1.3 KB each"** for `memory` (1,642) and `session_search` (1,510); the
   test caps are 1,700 and 1,550 (`skill_manage` 1,362 and `delegate_task` 1,338 are under 1,400). Those two
   carry 8 and 11 parameter descriptions, and CP9.2's `tier` / `tags` / `links` / `note` text is call-forming;
   cutting more meant dropping limits. Say if you want them tighter and which facts to give up.
3. **Honest measurement:** the plan's per-schema sizes were file sizes (see the table); the saving is ~890 tokens,
   not thousands. Nothing in this checkpoint can change that; CP11.2 has to do the work. PHASES.md has a status
   line saying so; consider adjusting the Phase 11 goal text.
4. **Bundled is not read-only for the agent.** The curator never touches it (verified: `is_bundled`), and a
   user's edit survives upgrades, but `skill_manage` (the agent, or the review) can `patch` it like any
   bundled skill (existing rule; `skill_manage.rs` only refuses project/external folders). If the guide is
   patched, the "Original description" text can drift, and an edited guide never refreshes on upgrade. I did not
   add a new protection (outside the CP's scope); say if you want `skill_manage` to refuse bundled skills named
   `hermes-tools-guide`.
5. **Pre-injection under `requests`:** the guide is a normal indexed skill, 23 KB; a user message that scores
   >= 5.0 against it would get it attached, clipped to 12,000 characters (3k tokens). The 59-character
   description and name are the only high-weight text; I did not test a wide set of messages. Worth a look if a
   message like "how does session search work" attaching it is undesirable (it would actually be right).
6. The guide's gotchas that go beyond the old text were checked against the code (20-operation cap, 2,200 /
   1,375 / 1,500 limits, ambiguous `old_text`, near-duplicate answer, staged-write message, scroll hints), but
   they are prose: nothing tests them.
7. `cargo fmt` was NOT applied to the repository (it reformats ~20 untouched files; I reverted that). My test
   code is formatted by hand and has a few lines over 100 columns in `skills.rs`.
8. The tool-list change is one cache miss for every existing session on upgrade (PITFALLS).

CP11.1 test results (2026-11-14, `CARGO_TARGET_DIR=target/claude`):
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`: all ok (hermes_tools 328 lib tests,
  hermes_prompts 29, incl. 5 new schema tests; 1 new `skills.rs` test).
- `cargo test -p hermes_runtime --lib`: 200 ok. `--test turns`: 222 ok (7 ignored) under the default profile and under
  `HERMES_TEST_PROFILE=balanced`, `requests` and `tokens` (1 new test).
- `cargo test -p hermes-desktop`: 176 + 68 ok (an empty `target/hermes-src/apps/desktop/dist-webapp` placeholder for the build
  script, removed afterwards). `cargo build --workspace`: ok.
- No live check against the perf instance: the page calls nothing new.

### Review (Opus 5.5)
Reviewed 2026-11-14 on `backend/p11-deferred` (main had not moved: no merge). Verdict: **sound, with fixes**. The shapes
are untouched (recomputed from `git show HEAD:prompts/tools/*.json` with every description removed: the fixture equals
HEAD's shapes for the four; the other 14 files are byte-identical), the sizes in the table reproduce exactly (30,501 ->
26,940 with the request's serialisation), the `session_id` correction matches `recall.rs` (a `session_id` without
`around_message_id` reads the session; `query` is ignored whenever `session_id` is set). Two real problems:

1. **The guide would have been pre-injected on ordinary coding messages.** Probed against the bundled index (scores are
   `-bm25`, attach threshold 5.0): "fix the memory leak in the session manager" 9.6, "refactor the memory store and
   session search tools" 9.0, "write a tool to manage memory sessions" 10.5, "delegate the tests to subagents in
   parallel" 6.2, "the memory tool failed with an error" 8.6, all with the guide first. Under `requests` that is
   12,000 characters (~3k tokens) added to the message, and the review's related-skills packet (same query) would
   have carried it too. Fixed: `skills::TOOLS_GUIDE`, skipped in `LocalIndex::query` (global scope), so it is never
   retrieved and the other skills rank as if it were absent; `skill_view` / `skills_list` / the index line unchanged.
   Test `index::tests::the_tools_guide_is_never_retrieved` (eight messages, threshold 0).
2. **Behaviour rules dropped from the short text** (the table below). Restored the ones that keep junk out of memory
   and skills or stop a repeated call; the cost is 446 bytes. Sizes now: descriptions `skill_manage` 1,451,
   `session_search` 1,704, `memory` 1,805, `delegate_task` 1,338; the four 9,977 bytes serialised, all 18 tools
   27,386 (from 30,501: **3,115 bytes, ~780 tokens saved per request**, not 890). Caps raised: `SHORTENED` 1,500 /
   1,750 / 1,850 / 1,400, block 27,700 (`tools.rs` and `turns.rs`), four <= 10,100 in `turns.rs`. The ~1.3 KB target
   is missed on purpose: correctness first.

Audit (old rule -> new schema: **kept**, **restored** by the review, **guide** = moved to the guide only, judged safe):

| tool | old rule | status |
|---|---|---|
| skill_manage | ops array, atomic rollback, create first, patch preferred, `content` alone replaces, delete sole op, edited where they live | kept |
| skill_manage | description <= 60 chars, shape, never the category name; name rule <= 64; per-action shapes; `absorbed_into` must exist; `replace_all`; empty `new_string` deletes | kept |
| skill_manage | "imperative rule + why, no PR numbers/dates/incident narration, one rule per lesson" | **restored** (was cut to "rule + why") |
| skill_manage | `file_path` "no leading slash" | **restored** (code refuses `has_root()`) |
| skill_manage | "lands in the skills directory", "references/ by topic, extend before adding", "skill_view() shows conventions", 20-op cap | guide (error text / skill_view cover them) |
| session_search | four shapes, history-only + never conclude 'not found', every limit (limit 3/10, window 5/[1,20], cap 20), after/before formats and "only if named" | kept |
| session_search | "Use for 'what did we do about X', 'where did we leave Y'" (when to call) | **restored** |
| session_search | `query` ignored in scroll shape | **restored**, corrected to the code: ignored whenever `session_id` is set |
| session_search | `sort` newest = where we left X / oldest = how X started; "a bias, not a bound" | **restored** (compact) |
| session_search | `role_filter` 'tool' alone = tool output only | **restored** |
| session_search | `limit` "bump to 5-10", "results are DB messages", detail's bookends wording | guide |
| memory | one call per target, atomic, FINAL-result limit, IF FULL reissue one batch, targets, what belongs, task learnings -> skill, one tier per call, note_read alone, old_text/content per action and once-only span, tier applies to every action, tags/links <= 8 + kinds, note project/archive + note_read | kept |
| memory | "confirms completion; one batch call finishes the update, so don't repeat it" | **restored** |
| memory | SKIP: trivial/obvious, easily re-discovered, task progress, completed-work logs, TODO state | **restored** (was cut to "progress logs or raw dumps") |
| memory | archive "entries stay one-line summaries" | **restored** ("one-line entries, long text in `note`") |
| memory | "Omit for 'add'" (old_text), char-limit numbers | guide (result states them) |
| delegate_task | tasks array, one entry = one subagent, parallel + joined, use / don't-use list, context-isolation ("know nothing of this conversation", language/tone, repeat per task), SELF-REPORTS + verify, blocked tools, inherit model, limit by profile + refused with limit | kept |
| delegate_task | "Stop or closing the session halts subagents (interrupted)" | guide ("durable work -> terminal background" kept) |
| delegate_task | "children cannot close tracked work" | dropped from guide gotchas too: no tracked-work concept here (kept only in the quoted Hermes original) |

Other findings:
- **Bundled read-only:** not an existing rule. `skill_manage` refuses only read-only project folders, and `archive`
  refuses bundled skills; other bundled skills are patchable by the agent and an edited one is not refreshed on upgrade
  (manifest hash). The guide follows the same rule; no new protection added (scope). The curator leaves it alone
  (`is_bundled`).
- Guide claims checked against the code (13): limit 3/10 and window 5/[1,20] (`recall.rs` `clamp`), exclude cap 20,
  the current session never returned, scroll-forward/backward hint text, words ANDed (FTS join), `skill_manage` batch
  cap 20 (`MAX_BATCH_OPS`), leading slash / absolute refused, memory limits 2,200 / 1,375 / 1,500, patch span must occur
  once, several different entries matching refused (`find_unique_match`), `BLOCKED_TOOLS` = delegate_task/clarify/memory,
  child uses the parent's model. Fixed in the guide: `session_id`'s parameter line still said "must be paired"
  (now says what the code does), `query` "ignored" condition, the "tracked work" gotcha removed, "candidates listed"
  for an ambiguous `old_text` (not verified) replaced by what the code does.
- The guide test checked parameter names anywhere in the 23 KB text (`name`, `content` would always pass); it now
  checks each tool's own section, before its quoted original.
- `must_say` extended with the restored rules. No other consumer asserts these descriptions (ACP lists the same
  schemas; the review and curator use their own contracts); `prompts/hermes/` untouched.
- The plan's "~8k tokens per request" is not reachable by shortening: the whole tool list is ~6.8k tokens now.
  CP11.2 must supply the rest by not sending tools (it can only reach ~8k counting MCP tools).
- Docs updated: PITFALLS (sizes, never pre-injected), ARCHITECTURE, GAPS, PHASES status, `prompts/tools/README.md`.

Review test results (2026-11-14, `CARGO_TARGET_DIR=target/claude`): `hermes_tools` 329 + `hermes_memory` / `hermes_store` /
`hermes_prompts` (29) / `hermes_llm` all ok (incl. the goldens on Hermes's full skill set); `hermes_runtime --lib` 200 ok;
`--test turns` 222 ok (7 ignored) under the default, `balanced`, `requests` and `tokens` profiles (request counts
unchanged); `hermes-desktop` 176 + 68 ok and `cargo build --workspace` ok (an empty `target/hermes-src/.../dist-webapp`
placeholder, removed afterwards).
