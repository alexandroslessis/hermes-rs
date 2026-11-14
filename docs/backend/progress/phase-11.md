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

## CP11.2 — Deferred tool schemas (`tool_search`) and `skill_manage` for the review only, `tokens` profile
Worktree: C:\Users\alexandros\repos\hermes-rs-p11   Branch: backend/p11-deferred   Model: Sonnet 5.5 (subagent)
On top of CP11.1 (reviewed and committed, e8fc41d). Uncommitted.
Done:
- **Knobs** (`budget.rs`): `tool_schemas` (`all` | `deferred`; the variant already existed, `tokens` now has `Deferred`) and the new
  `deferred_mcp_keep` (default 3). Both are override keys with validation (`budget.tool_schemas` must be all or deferred, "" = the profile's own;
  `deferred_mcp_keep` 0..=50). `balanced` and `requests` are unchanged (`balanced_is_todays_constants` pins `All`).
- **Which sessions: the frozen prompt decides.** A session is deferred exactly when its stored system prompt holds the `# Deferred tools`
  block (`hermes_prompts::deferred::is_deferred_prompt`). The profile decides when a prompt is built (new session, model switch, compaction),
  so the existing rule "what changes the prompt or the tool list applies from the next session" holds with no per-session flag or side file, and a
  resumed session knows. Both directions are tested (`a_session_keeps_its_tool_list_when_the_profile_changes`).
- **The wrapper** (`runtime/src/deferred.rs`): `GuardedTools { DeferredTools { DelegatingTools { SessionTools } } }` on the main agent's chain only. In a deferred session
  `tools()` = the core tools (schema order, MCP tools up to `deferred_mcp_keep`), `tool_search`, then the loaded tools in load order (deterministic: the cache prefix
  breaks only at a load). Anywhere else it returns the inner list untouched, so `requests` / `balanced` lists are byte-identical (a test compares every
  request of two turns and each description with the schema files, and pins the size at 27,675 bytes).
- **Deferred set (by measured size and rarity)**, serialised bytes as sent (one `{"type":"function",...}` entry):

  | tool | bytes | why |
  |---|---|---|
  | `session_search` | 2,284 | rare in a coding session (recall of old sessions) |
  | `clarify` | 1,739 | the model can ask in plain text |
  | `delegate_task` | 1,680 | rare, needs a plan |
  | `process_manage` | 1,168 | only after a background `terminal` command |
  | `web_extract` | 1,126 | web only |
  | `vision_analyze` | 810 | only with an image, and a vision model sees attached images natively |
  | `web_search` | 753 | web only |
  | *absent for the main agent:* `skill_manage` | 3,154 | the review writes skills (step 4) |
  | **removed from every request** | 13,005 with separators | |
  | added: `tool_search` | 735 | |

  Core, kept: `terminal` 1.8 KB, `run_script` 1.7 KB (the batching guidance names it), `read_file`, `write_file`, `patch`, `search_files` (2.4 KB, the largest),
  `todo_list`, `memory` (2.6 KB; the memory guidance in the prompt names it and the user's "remember this" is immediate), `skills_list`, `skill_view`. MCP tools:
  the first N = 3 (server-name order, then the server's own) stay core, the rest are deferred. The 3 is a judgement: a small single-purpose server stays whole (no round trip for its
  everyday tool) while a large one (10-30 tools at 0.3-1 KB each: the part of the plan's "~8k" that is real) is deferred. Tunable per user (`budget.deferred_mcp_keep`).
- **Measured** (`turns.rs`, a one-line "hello" turn): `tools` array 27,675 -> 15,332 bytes (12,343 saved, about 3,090 tokens, on every request); whole request body
  35,991 -> 24,648 bytes (~2,800 tokens), including the new frozen block (1,120 bytes). **This is not the plan's ~8k**: all 18 tools were 6.9k tokens after
  CP11.1; the only way to 8k is MCP tools (each deferred one saves its own schema and costs one line in the block). One use of a deferred tool costs one more request
  (3 main requests against 2) and a cache miss from the `tools` array on at the load.
- **`tool_search({names, query})`** (`prompts/tools/tool_search.json`, 735 bytes, not in `tool_names()`): `names` exact, at most 8 (more: refused, nothing loaded), a single string is
  accepted; the result is JSON with `loaded` + `schemas` (the full name/description/parameters), `already_loaded`, `already_in_your_tool_list`, `turned_off_by_the_user`,
  `not_available_to_you` (`skill_manage`), `unknown` + `available_deferred`, and `matches` for `query` (all words must occur in name or description, at most 8, loads nothing);
  neither argument is an error that lists the deferred names. It is an error result only when nothing helped.
- **(a) A deferred tool called before it was loaded: not run.** `UnhandledToolCalls` (the tool operation only runs listed tools) answers it with an error that carries the schema ("deferred
  tool ... not run ... in your tool list from your next step"). Reason: arguments formed without the schema are wrong often enough on a small local model that running them risks a bad `patch` or
  `delegate_task`; the retry costs the same one round trip `tool_search` costs. `ValidateToolCalls` keeps deferred names valid (no invalid-name strike).
- **(b) The loaded set is a function of the conversation** (`loaded_in`): answered `tool_search` calls and answered direct calls to deferred tools, in order. Nothing is stored:
  a restart, a branch and a rewind (drops the later loads) recompute it, and a compaction keeps loads because hidden messages stay in the conversation (the test asserts the
  `tool_search` call was hidden by the compaction and the tool is still listed; the rebuilt prompt is still deferred).
- **(d) Gating:** the wrapper is inside `GuardedTools`, so switched-off tools are never listed or run (`tool_search` says "turned off"); MCP tools of a disconnected server are not in the
  inner list ("unknown"); the review, curator and subagents never see the wrapper. The page shows `tool_search` as a generic tool call (MCP tools already have arbitrary names); ACP has a title
  "Load tools: a, b" (kind `other`).
- **(f) Prompt** (`# Deferred tools` block, 1,120 bytes, at the end of the stable tier after the budget wording): the deferred names with one line each (MCP tools: first sentence, 70 characters; at most 40
  MCP lines, then "... and N more"), the how-to, the "a deferred tool called early is not run" rule, and "you have no skill_manage tool: a background review saves skills; say plainly what the
  task taught you". Frozen with the session; a compaction rebuilds it under the then-current profile.
- **(h) Subagents** of a deferred session get the whole small set (everything but `delegate_task`, `clarify`, `memory` and `skill_manage`), no `tool_search`, and a plain prompt: they make few
  requests and often need web or session search at once. **The review** keeps the whole `SessionTools` list (it never goes through the wrapper), so it has `skill_manage` as before, and its fork of the
  prompt has the deferred block stripped (its instructions say the opposite).
- **Step 4 (`skill_manage` only in the review):** the main agent's prompt is built without the tool: `tool_names` drops it (so `SKILLS_GUIDANCE` and the memory "(skill_manage)" routing sentence take Hermes's no-tool
  branch), the skills block's "fix it with skill_manage(action='patch')" / "offer to save as a skill" lines are rewritten to "say so in your answer: the background review ..." (`hermes_prompts::deferred::without_skill_manage`),
  and `memory`'s "belongs in that task's skill via skill_manage" is rewritten in the schema of a deferred session. A test greps the prompt and every core description for the tool's name in a calling position.
- **Tests changed:** `s5_delegate_two_children` for `Tokens` loads `delegate_task` first (5 requests, 3 main); `the_long_tool_schemas_are_short_in_every_profile` finds only `memory` of the four in `Tokens`; `test_budget`
  keeps the full tool list for the suite-wide `HERMES_TEST_PROFILE=tokens` run (every other test calls tools directly) while tests that name `Tokens` get the deferred list.
Files: `prompts/tools/tool_search.json`, `crates/prompts/src/{deferred.rs,lib.rs}`, `crates/runtime/src/{deferred.rs,lib.rs,budget.rs,ops.rs,delegate.rs,review.rs,curator.rs}`,
`crates/runtime/tests/turns.rs`, `crates/desktop/src/acp/tools.rs`, `scripts/perf/deferred-tools-check.mjs`, docs (`PORTING.md`, `GAPS.md`, `docs/{ARCHITECTURE,PITFALLS,TESTING}.md`,
`prompts/tools/README.md`, `scripts/perf/README.md`, `docs/backend/PHASES.md`).
Tests (all with `CARGO_TARGET_DIR=target/claude`, 2026-11-14): see the end of this section.
Request counts: `requests` / `balanced`: every scenario unchanged. `tokens`: 1 more request each time a deferred tool is first used (`tool_search` + the call = 3 main requests for a one-tool task, 2 without
deferral; a direct call to a deferred tool = 3 as well); S5 under `tokens` = 5 (3 main + 2 delegate).
Deviations from the plan: the "new knob e.g. `deferred_tools`" is the existing `tool_schemas` knob (plus `deferred_mcp_keep`); `clarify` and `process_manage` are deferred too (the plan's list did not name them) and `memory` stays core;
the plan's "~8k" is not reachable without MCP tools (above). Subagents get the full set, not their own deferral.
Model: Sonnet 5.5 (subagent)

For the reviewer:
1. **The suite-wide `HERMES_TEST_PROFILE=tokens` run no longer exercises deferral.** `test_budget(None)` forces `tool_schemas: All` because ~16 existing tests call `delegate_task`, `session_search`, `vision_analyze`,
   `skill_manage`... directly (without this they fail by design: the call is answered with the schema). The deferral is covered by the 12 new tests, which name `Tokens`. Say if you want the env run to be deferred and those
   tests to load first.
2. **Deferral is decided by the stored prompt, so a compaction or `set_session_route` under another profile flips a session's mode** (documented in PITFALLS). I judged it acceptable (both rewrite the prompt anyway, and the list follows
   the prompt every request, so nothing is inconsistent). The alternative, a per-session record, needs a side file for something the prompt already says.
3. **`loaded_in` and `tool_search` must stay the same function** (`requested_names` + the 8-name rule): changing one silently changes an old session's list on resume. Unit tests pin the cases; nothing pins it across versions.
4. **The deferred set is a judgement.** I kept `memory` (2.6 KB) and `run_script` (1.7 KB) core: the first because the guidance and the user's "remember" are immediate, the second because the frozen batching guidance names it.
   Deferring both would save ~1,100 tokens more per request for one more round trip on each first use. `todo_list` (1.3 KB) and `read_file` / `write_file` / `patch` / `search_files` are core for the same reason.
5. **A direct call to a deferred tool is not run** (reason above). If a real small model keeps doing that instead of using `tool_search` it costs the same as the loader; a live check against a local model was not done.
6. **Estimates still use all 18 tools**: `compress::tool_definition_tokens()` (the fixed part before the first usage report) and the context breakdown (`Runtime::tool_definitions`) overstate a deferred session by ~3k tokens until the provider reports usage. Not fixed (the gauge uses the provider's real usage); say if the breakdown should follow the session.
7. **The MCP block lists the tools connected when the session was created.** A server that connects later has its tools past N reachable only by `tool_search` `query` or by exact name. Not tested with a late connection.
8. **The page's rendering of `tool_search` was not looked at in the real page** (no built frontend in this worktree; `target/hermes-src` is a placeholder): the live check asserts the `tool.start` event only. MCP tools already carry arbitrary names, so a generic card is the existing path.
9. `cargo fmt` was not run (as in CP11.1); test code is formatted by hand and some lines are over 100 columns.
10. Open to push-back: the `requested_names` leniency (a single string, quotes and backticks trimmed) and `query` matching (all words, case-insensitive substring); both exist to make small models' calls land.

User checklist (Phase 11, for the user to try):
1. Open Settings, find **Budget profile**, pick **tokens**, and start a **new** chat (a chat that already exists keeps the tool list it started with).
2. Send "hello". In the context gauge / usage line, the first request should be about 3,000 tokens smaller than the same message in a new chat under **requests** or **balanced** (the difference is the tool schemas). Nothing looks different in the chat itself.
3. Ask something that needs an old conversation, e.g. "what did we work on earlier about <a topic from another chat>?". You should see one extra tool card named **tool search** (it loads `session_search`), then the `session_search` card, then the answer.
   That one extra step is the price of the smaller requests. From then on in that chat `session_search` is available without searching again (reopen the chat later: it still is).
4. Ask for a web search ("search the web for ...") and for a delegation ("use a subagent to count the .rs files"): each shows a **tool search** card first (once per tool). If the model calls the tool without loading it first, the card shows the tool's schema as the answer and the model retries: that is expected, not an error.
5. Ask the agent to "save what you learned as a skill": in this profile it has no skill tool; it should say that the background review saves skills. In **requests** / **balanced** it calls `skill_manage` as before.
6. Switch the profile back to **requests** (Settings > Budget profile): chats started under `tokens` keep their short tool list (a chat does not change mid-way); start a new chat to get the full list again.
7. Capabilities > Tools: switch a toolset off (for example Web); in a `tokens` chat ask for a web search: the agent should say the tool is turned off and not load it.

CP11.2 test results (2026-11-14, `CARGO_TARGET_DIR=target/claude`):
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`: all ok (hermes_prompts 35, 6 new in `deferred.rs`).
- `cargo test -p hermes_runtime --lib`: 206 ok (6 new, `deferred.rs`). `--test turns`: 234 ok (7 ignored; 12 new tests, `s5` and `the_long_tool_schemas...` adapted) under the default,
  `HERMES_TEST_PROFILE=balanced`, `requests` and `tokens` profiles (the last keeps the full tool list, see "For the reviewer" 1).
- `cargo test -p hermes-desktop`: 176 + 68 ok (an empty `target/hermes-src/apps/desktop/dist-webapp` placeholder for the build script, removed afterwards). `cargo build --workspace`: ok.
- Live: `scripts/perf/deferred-tools-check.mjs` against my own instance (port 47810, data in `target/perf`, the worktree's `target/claude/debug/hermes-desktop.exe`): all checks passed;
  the instance was stopped by its path afterwards.

### Review (Opus 5.5)
Reviewer: Opus 5.5 (subagent), 2026-11-14, in the worktree. `main` had not moved (nothing to merge). `/code-review` was not run (the manual pass of
the brief instead): read every changed file, then tried the risky paths with new `turns.rs` tests and a request-by-request comparison against HEAD.

**Byte-identity of the other profiles (the review focus): holds.** A temporary `Drop` on the test harness dumped every request of every
`turns.rs` test (main agent, subagents, review, curator, resume after a restart, compaction, rewind, model switch, profile switch) at HEAD
(e8fc41d, the CP11.1 commit) and with this checkpoint, under `HERMES_TEST_PROFILE=balanced` and `requests`. Of the 273 test harnesses both
versions have, the 14 that build a `tokens` session by name differ as intended; in the other 259, **768 (balanced) / 767 (requests) requests have
byte-identical `tools` arrays and byte-identical system prompts** (temp-folder names normalised). The dumper was not committed. ACP sessions go
through the same `start_turn`; the only ACP change is the `tool_search` title.

Findings and fixes:
1. **Mode flip at a compaction or a model switch (fixed).** The rebuilt prompt took the current profile, so a `tokens` session compacted under
   `balanced` lost the block and its list grew by ~11 KB (and the other way round: a `balanced` session dropped seven tools mid-session). Now
   `PromptTools::of_session` keeps the session's kind in `prompt_rebuild` and `set_session_route`; the budget wording still follows the profile, as
   before. Test `a_compaction_or_a_model_switch_keeps_the_kind_of_tool_list` (tokens->balanced, balanced->tokens, requests->tokens; it fails with
   the fix reverted).
2. **A copy of the block anywhere in the prompt made a session deferred (fixed).** `is_deferred_prompt` was `contains(MARKER) && contains(END)`, and
   the context tier carries the repository's AGENTS.md / CLAUDE.md, the volatile tier skills and memory: a repository whose docs quote the block (this
   one's could) turned a `balanced` session deferred, and the review's `strip_block` would have cut a span out of a plain prompt. The block's last
   line is now sealed with an FNV-1a 64 of the prompt text before it (`seal`, `find_block`); the block ends the stable tier, which is only identity and
   fixed guidance, so a copy elsewhere fails the check. Tests: `a_copied_block_does_not_count` (unit), `a_copied_deferred_block_in_a_context_file_changes_nothing`
   (a real session's block written into AGENTS.md of a `balanced` / `requests` session: full 18-tool list). Cost: 17 bytes per deferred prompt.
3. **MCP split recomputed every request (fixed).** "The first `deferred_mcp_keep` MCP tools" was counted anew per request, so a server that connected
   later (they connect in the background at start-up) with a name sorting first, a reconnect, or a changed knob pushed a core MCP tool out of a running
   session's list, unnamed in its block. The block now records the core MCP names (`<!-- core MCP tools: ... -->`); every other MCP tool is deferred,
   including a late server's (found by `query`, loaded by name). Test `the_core_mcp_tools_are_the_ones_the_session_started_with`.
4. **Context gauge (fixed, small).** The page's breakdown counted all 18 tools; `Runtime::session_tool_definitions` gives what a deferred session sends
   (core + `tool_search` + loaded), used by `session.context_breakdown` and `Runtime::context_breakdown`. `compress::tool_definition_tokens()` (the
   compression trigger's estimate before the first usage report) still counts all 18: conservative, left. Test
   `the_context_breakdown_counts_the_tools_a_deferred_session_sends`; live check line added (4,311 vs 6,712 tokens).
5. **Override knobs had no test (added)** `the_deferred_tool_knobs_are_validated`. `budget.tool_schemas` was not an override key before CP11.2 (the
   `Deferred` variant existed and nothing consumed it), so no existing config changes behaviour; documented in `budget.rs`. It has no Settings control.

Checked, no change needed: loads come only from real assistant tool calls with an answer (`MessageContent::ToolRequest` only exists in assistant
messages; text in a user message, a web page or a file result cannot load anything; a withdrawn call loads nothing); the tool operation runs only listed
tools, so a loaded tool called in the same response as its `tool_search` is answered with its schema (not run), and so is `skill_manage` (never run
for the main agent); a direct call to a deferred tool counts as a load, so the retry is listed (no loop; bounded by `max_iterations` anyway);
loaded tools go through the same `GuardedTools` approval / taint / result-size path; disabled toolsets and disconnected servers are never listed;
subagents of a deferred session have no `skill_manage` (list and prompt); review and curator keep the whole list; `tool_search` with 9+ names,
duplicates, non-strings, empty or neither argument is handled; rewind/branch/resume recompute the set; a compaction keeps loads because hidden
messages stay in the conversation, and repeated compactions only hide more (the test asserts the call was hidden). The page renders an unknown tool
name with the generic title (`toolMeta` -> `titleForTool`, read in Hermes's `apps/desktop/src/components/assistant-ui/tool/fallback-model/index.ts`
at `HERMES_COMMIT`; not seen in a built page: no webapp in this worktree).

Left, with reasons: `test_budget(None)` keeps the full list for the suite-wide `HERMES_TEST_PROFILE=tokens` run: acceptable, the dump comparison and
14 `Tokens`-named tests cover the deferred path, and making every other test load first would double the suite's scripts (TESTING.md now says so
plainly). A cheaper design for one deferred use (run the same response's later calls after `tool_search`) would save the extra request but run
calls formed without the schema: noted, not redesigned. The review's forked prompt still carries the "the background review fixes skills" wording of
the skills block (harmless: its own instructions come in its message). The gateway's breakdown opens the session synchronously in an async handler,
as the `messages` call next to it already did. `cargo fmt` was run on the two new `deferred.rs` files only.

Measured (this tree, `turns.rs`, a one-line "hello"): `tools` 27,675 -> 15,332 bytes in `tokens` (requests / balanced 27,675, unchanged); whole
request body 35,991 -> 24,665 bytes (~2,830 tokens; the block 1,120 bytes + its 17-byte seal). The plan's ~8k is out of reach without MCP tools
(PITFALLS / PHASES say so). Request counts: other profiles unchanged in every scenario; S5 under `tokens` = 5 (3 main + 2 delegate).

Tests (2026-11-14, `CARGO_TARGET_DIR=target/claude`): `hermes_tools`, `hermes_memory`, `hermes_store`, `hermes_prompts` (37), `hermes_llm`: ok.
`hermes_runtime --lib` 207 ok; `--test turns` 238 ok (7 ignored) under the default, `balanced`, `requests` and `tokens`. `hermes-desktop` 176 + 68 ok
(empty `target/hermes-src` placeholder, removed). `cargo build --workspace` ok. Live: `deferred-tools-check.mjs` against my own instance (the worktree's
debug build, port 47810, data in `target/perf`): all 23 checks passed; stopped by its exe path.

User checklist (Phase 11, corrected):
1. Settings > Model > Main > **Budget profile**: pick **tokens**, then start a **new** chat (an existing chat keeps the tool list it started with).
2. Send "hello", open the context gauge: *Tool definitions* is roughly 2,500-3,000 tokens smaller than in a new chat under **requests**.
3. Ask "what did we work on in my other chats about <a topic>?": you see a **Tool search** card, then **Session search**, then the answer. Ask again
   later in that chat, or after closing and reopening the app: no second Tool search.
4. Ask "search the web for <something>": one **Tool search** card first, then the web search. If a card shows a schema and "not run", the model
   called the tool too early; it retries by itself: expected.
5. Ask "save what you learned as a skill": it answers that the background review saves skills (no skill tool card).
6. Switch back to **requests**: the tokens chat keeps its short list (also after "compress"); a new chat has every tool and no Tool search card.
7. Capabilities > Tools: switch **Web** off; in a new tokens chat ask for a web search: it says the tool is turned off.
