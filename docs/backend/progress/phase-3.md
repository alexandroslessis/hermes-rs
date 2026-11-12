# Phase 3 progress

## CP3.1 — `budget.rs`, constants replaced by knobs (`balanced` still effective)
Worktree: C:\Users\alexandros\repos\hermes-rs-p3   Branch: backend/p3-budget   Model: Sonnet 5.5 (subagent)
Cut from `main` at f9def42 (Phases 0, 1 and 2 merged).
Done:
- `crates/runtime/src/budget.rs`: `BudgetProfile` (`Requests` is `Default`; `parse`/`as_str`/`ALL`),
  `BudgetKnobs` (every row of the plan's table, `Copy`), `BudgetKnobs::BALANCED` (const) and
  `for_profile`. Unit tests: `balanced` equals today's constants written out as literals, `requests`
  and `tokens` follow the table, the default is `requests`, names round-trip.
- `RuntimeConfig.max_iterations` is replaced by `RuntimeConfig.budget: BudgetKnobs`; `for_current_user`
  sets `BALANCED`, so `balanced` is the effective profile. `begin_turn` copies it once
  (`let budget = self.config.budget;`) and threads `&budget` to everything the turn builds.
- Constants replaced at their use sites (all four loops that build steps: main turn, delegate
  children, review, curator):
  - `max_iterations` (`IterationBudget`), `curator_max_iterations`, `delegate_child_iterations`
  - `invalid_tool_retries`, `truncated_tool_retries` (`ValidateToolCalls::new(state, names, &budget)`)
  - `length_continuations` (`LengthContinuation::new(state, &budget)`)
  - `empty_response_retries`: `hermes_agent::InferenceRunner::with_empty_response_retries`
    (default `DEFAULT_EMPTY_RESPONSE_RETRIES = 3`, so other users of the agent crate are unchanged)
  - `http_retries`: `OpenAiCompatibleProvider::with_max_retries` (overrides `Provider::retry_config`);
    `ProviderConfig::provider_with(n)` used by the turn, its aux providers, /refine, /compress, /bg
    and the curator; `provider()` (side jobs: titles, approval judge) keeps the default 3
  - `compress_at`: `Summarizer.compress_at` + `Budget::with_compress_at`; `Budget::new` stays the
    balanced one (the GPUI app calls it)
  - `result_size_pct`: `ops::result_size_limit(context_length, &budget)` (percent of today's limit)
  - `read_file_max_lines`, `search_context_factor`: `hermes_tools::ToolLimits` (default = today's),
    `SessionTools::set_limits` called once per turn in `begin_turn` and for each delegate child
  - `delegate_max_children`: `Delegator.budget`; `MAX_CONCURRENT_CHILDREN` stays as the balanced
    value re-exported for the page's `delegation.status` (see "For the reviewer")
- Knobs that wait for a later phase (`review_mode`, `curator_mode`, `skills_index`, `skills_preload`,
  `titles`, `tool_schemas`) are in the struct with their future variants; `for_profile` gives them
  the `balanced` value for every profile, with a comment naming the phase. Nothing consumes them yet.
Files: crates/runtime/src/{budget.rs (new),lib.rs,ops.rs,compress.rs,delegate.rs,review.rs,curator.rs},
  crates/runtime/tests/turns.rs, crates/agent/src/inference.rs, crates/llm/src/openai_compatible.rs,
  crates/tools/src/{lib.rs,files.rs,search.rs}, PORTING.md (budget.rs row, diverged)
Tests (CARGO_TARGET_DIR=target/claude):
  - `cargo test -p hermes_runtime`: lib 53 (new: 4 budget, `result_limit_follows_the_profile`,
    `compression_thresholds_follow_the_profile`), `turns` 89 passed / 7 ignored (was 85).
  - Parameterised `turns`: the six baseline scenarios (S1, S2, S2b, S3, S4, S5) now run under all
    three profiles (`baseline_s*` loop over `BudgetProfile::ALL`, via `harness_profile`), and four
    new tests show the caps that differ: `empty_reply_retries_follow_the_profile`,
    `length_continuations_follow_the_profile`, `truncated_tool_call_retries_follow_the_profile`,
    `delegation_width_follows_the_profile`.
  - `HERMES_TEST_PROFILE=requests|tokens` also runs the *whole* `turns` suite under that profile
    (default `balanced`; `max_iterations` stays 20 in tests). Under `requests` 2 tests fail, as
    expected (see below); `tokens`: all 89 pass.
  - `cargo test -p hermes_tools -p hermes_agent -p hermes_llm`: green (new: read_file default lines and
    search context follow `ToolLimits`; `with_max_retries`). `cargo test -p hermes-desktop`: 162 + 67.
  - `cargo build --workspace`: OK, no warnings (this worktree has a `target/hermes-src` junction to the
    main checkout's webapp, gitignored, as in Phase 2).
Request counts: unchanged. Under every profile S1–S5 make the same requests as the baselines: at this
  checkpoint the profiles only move caps, and the request-lowering knobs (`idle_once`, heuristic titles,
  skills preload) wait for their phases. The plan's "`requests` assertions lower than baseline for S2,
  S3, S5" cannot hold yet; the baselines table is not touched.
Deviations from the plan:
- Per-knob overrides from config ("merged with per-knob overrides from config") are CP3.2 with the config
  key; `RuntimeConfig.budget` is the merge point.
- Knobs not in the plan's table stay constants: `MAX_INVALID_JSON_RETRIES` (3), `MAX_THINKING_PREFILLS`
  (2) in `ops.rs` and `REVIEW_MAX_ITERATIONS` (16) in `review.rs`. They sit beside table knobs, but the
  plan gives them no per-profile value; say so if they should become knobs.
- `read_file default lines`: implemented as one knob, `read_file_max_lines`, used both as the default and
  the cap (balanced 2000 = the old `MAX_LINES`, requests 4000). `READ_CHAR_BUDGET` (100k chars) still caps a
  read, so a very wide file stops earlier. The tool schema text in `prompts/` still says what it said.
- `search context lines ×2`: the model's requested `context` is multiplied by the factor (cap 10 × factor);
  the default of 0 stays 0, so "×2" only bites when the model asks for context.
- `result_size_limit` ×4 / ×0.5 scales the final limit after the 15%-of-window clamp (so requests can
  exceed 100k chars, 400k at most).
- `http_retries`: requests = 3 as in the table. "429-daily: 0" needs no knob: `should_retry` already
  refuses usage-limit 429s for every profile.
- `compress_at` is on `Summarizer` (which `compact`, `AutoCompress` and delegate children share), so manual
  `/compress` and auto-compression agree on the threshold-derived tail size.
- `skills_index` (tokens: names only) has a value but no consumer: the index is part of the system prompt,
  which CP3.2 decides at session start ("profile-dependent prompt text").
For the reviewer:
- **`balanced` byte-for-byte**: check `BALANCED` against the old constants (git diff shows each removed
  `const`); `result_size_limit` at `pct = 100` is `base * 100 / 100` (exact for usize); the compression
  fractions `0.75` / `0.50` are the same f64 literals; `read_file` at 2000 and `search` ×1 are the old
  values. The whole `turns` suite passes under the default.
- **Under `requests` two existing tests fail on purpose** when the suite runs with
  `HERMES_TEST_PROFILE=requests`: `three_batches_of_invented_tools_end_the_turn` (the strike limit is 2)
  and `a_request_past_the_threshold_is_compressed_before_the_next_call` (90% instead of 75%). They are
  written for `balanced`; I did not duplicate them per profile.
- **No constant left behind**: grep shows no `MAX_INVALID_TOOL_RETRIES`, `MAX_TRUNCATED_TOOL_RETRIES`,
  `MAX_LENGTH_CONTINUATIONS`, `MAX_EMPTY_RESPONSE_RETRIES`, `CHILD_MAX_ITERATIONS`, `CURATOR_MAX_ITERATIONS`,
  `MAX_LINES` or `RuntimeConfig::max_iterations` left. Remaining literals by design: `DEFAULT_MAX_RETRIES`
  (the llm crate's own default, also what `provider()` uses), `DEFAULT_EMPTY_RESPONSE_RETRIES` (agent crate
  default), `MAX_CONCURRENT_CHILDREN` (= `BALANCED.delegate_max_children`).
- **Knobs read once per turn**: `begin_turn` copies the config once; the delegator, review setup,
  summarizer, tools and every step get that copy. Read separately (once each, at their own start):
  `/bg`, manual `/compress`, `/refine` and the curator pass read `self.config.budget` themselves. Config
  is immutable today, so there is nothing to disagree; CP3.2 makes it settable, which is when "per turn"
  starts to matter.
- **`MAX_CONCURRENT_CHILDREN` in the gateway**: `gateway.rs` (~L1046) still reports the balanced 10 as
  `delegation.max_concurrent_children`. The turn enforces its own profile's value. CP3.2 should report the
  effective one when the profile becomes settable (not changed here: desktop untouched).
- **Review/curator use the starting turn's knobs**: `ReviewJob.budget` is the turn's copy; the curator uses
  `self.config.budget`. Review's iteration cap (16) and approval-judge/title/`/btw` requests are not
  profile-dependent (side jobs, Phase 4).
- `Provider::retry_config` override: `OpenAiCompatibleProvider` now builds `RetryConfig::default()` with
  `max_retries` replaced only when set, so every other field (backoff, transient_only) is unchanged.
- Style: `cargo fmt --check` already reports diffs in files from earlier phases (ledger.rs, observer.rs,
  ...); I formatted only the new `budget.rs`, and left the others to avoid churn in the diff.

### Review (Opus 5.5)
`main` had not moved since the branch was cut (no merge). `/code-review` not run (not available in
this subagent); manual pass over every changed function.

Review focus, point by point:
- **`balanced` byte-for-byte**: confirmed. Every replaced constant equals its `BALANCED` field
  (pinned by `balanced_is_todays_constants` with literals); comparisons are unchanged
  (`<` / `>=` / `<` as before); `result_size_limit` at 100% is `base * 100 / 100` (exact);
  `Budget::new` delegates to `with_compress_at(BALANCED)` with the same f64 literals; the
  `OpenAiCompatibleProvider::retry_config` override starts from `RetryConfig::default()` (the
  trait default it replaces) and only sets `max_retries`, 3 = `DEFAULT_MAX_RETRIES`;
  `read_file` 2000 / search ×1 are the old values. Whole `turns` suite green under the default.
- **No constant left behind**: confirmed for every row of the plan's table (grep for the removed
  names is empty; `max_iterations` only in `ops::IterationBudget` and the knobs). Side jobs
  (titles, `/btw`, approval judge, `smart_review`) keep `provider()`, i.e. 3 retries, which equals
  every profile's `http_retries`.
- **Read once per turn**: `begin_turn` and the curator took one snapshot; `/refine`, `/compress`
  and `/bg` read `self.config.budget` two or three times each. Harmless while the config is
  immutable, but CP3.2 makes it settable, so **fixed**: each now copies it once into a local.

Implementer's open questions:
- `MAX_INVALID_JSON_RETRIES`, `MAX_THINKING_PREFILLS`, `REVIEW_MAX_ITERATIONS` stay constants:
  agreed. The plan gives them no per-profile value, and the review loop is rewritten in Phase 7.
- `skills_index` with no consumer: correct for CP3.1; the skills block is system-prompt text,
  chosen at session start in CP3.2 (step 4).
- `gateway.rs` reporting the balanced 10: correct while `balanced` is the only effective profile.
  **For CP3.2**: report the effective `delegate_max_children`.
- Two `turns` tests failing under `HERMES_TEST_PROFILE=requests`: a suite that fails on purpose
  under a documented switch is a trap. **Fixed**: both now pin `BudgetProfile::Balanced` (they test
  balanced's three strikes and 75% threshold); `HERMES_TEST_PROFILE=requests` and `=tokens` now run
  the whole suite green.

Left for CP3.2 (profile-dependent text, chosen at session start): the `read_file` schema still says
"default: 2000, max: 2000" and `delegate_task`'s says "up to 10 in parallel"; under `requests` they
understate/overstate the real caps. Also: config overrides must not allow 0 for
`result_size_pct` / `length_continuations` / `invalid_tool_retries` (0 would persist every result
or end a turn on the first strike); `read_file` and `search` already clamp to ≥1. `cargo fmt`
drift is pre-existing across Phase 2 files; not reformatted here.

Tests (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_runtime` lib 53, `turns` 89 passed /
7 ignored; the same `turns` under `HERMES_TEST_PROFILE=requests` and `=tokens`: 89 / 7 ignored;
`cargo test -p hermes_tools -p hermes_agent -p hermes_llm`: green; `cargo build --workspace`: OK.

## CP3.2 — `budget.profile`: config, Settings, default `requests`, prompt wording at session start
Worktree: C:\Users\alexandros\repos\hermes-rs-p3   Branch: backend/p3-budget   Model: Sonnet 5.5 (subagent)
On top of CP3.1 (reviewed; uncommitted when this was written).
Done:
- **Config key.** `budget.profile` (`requests | balanced | tokens`) plus flat per-knob overrides beside it
  (`budget.max_iterations`, `budget.http_retries`, `budget.compress_at_small_window`, ...: `OVERRIDE_KEYS`,
  14 numbers). `budget.rs`: `from_config(&Value) -> BudgetKnobs` (profile, then each valid override; a
  missing, unknown or non-string profile is `requests`, so configs without the key get it),
  `check_config_value(key, value)` and `apply_override` (one table of ranges). `BudgetKnobs` has a new
  `profile` field (which profile the knobs started from; overrides keep it), so the prompt wording follows
  the profile, not a knob value.
- **Overrides refuse bad values** (reviewer's CP3.1 item): 0 is refused for `max_iterations`,
  `length_continuations`, `invalid_tool_retries`, `truncated_tool_retries`, `result_size_pct`,
  `read_file_max_lines`, `search_context_factor`, `delegate_max_children`, `delegate_child_iterations`,
  `curator_max_iterations`; allowed for `empty_response_retries` and `http_retries` (0 = don't retry).
  Fractions must be 0.2..0.95; every count has a ceiling. Refused through `config.set` (INVALID_PARAMS with
  the reason) and dropped by `Rest::save_config` (the page's `PUT /api/config`); a hand-edited
  `desktop-config.json` with a bad value is ignored by `from_config` (the profile's value stays).
- **Runtime.** `Runtime.budget: Mutex<BudgetKnobs>` (`budget()` / `set_budget()`); the six places that read
  `config.budget` now call `self.budget()`, still once per turn (turn, `/refine`, `/compress`, `/bg`,
  curator). `RuntimeConfig.budget` is the *initial* value; `for_current_user` now uses
  `BudgetKnobs::default()` = `requests` (was `BALANCED`), so a runtime nobody configures (the GPUI app, the
  example) is `requests` too.
- **Gateway.** `apply_settings` (run at start, after `config.set`, after `PUT /api/config`, and by the ACP
  gateway at start) sets the knobs from `config_value("budget")`. `config.set budget.*` validates, saves to
  the config record and applies. `delegation.status` reports the effective `delegate_max_children`;
  `MAX_CONCURRENT_CHILDREN` is deleted.
- **Settings UI.** `rest.rs`: default `budget: {profile: "requests"}` (so `config.get` and `/api/config`
  answer `requests` without a stored key) and a select schema entry with the three options and a
  description. Hermes's Settings only shows keys in its `SECTIONS` list, so there is no field to reuse:
  **patch 0022** (two lines in `constants.ts`, one in `config-subpages.ts`) puts `budget.profile` under
  Settings ▸ Model ▸ Main as "Budget profile". `git apply --check` passes against the pinned source;
  **not built into a webapp or tried in the page** (needs npm; same state as patch 0021).
- **Prompt text, chosen at session start (step 4).** `PromptSource` carries a snapshot of the knobs taken
  when the source is made (`prompt_source()`); a new session's prompt uses them and is stored, so it is
  frozen with the session. Two profile-dependent pieces:
  - A budget paragraph in the *stable* tier (`hermes_prompts::BUDGET_GUIDANCE_REQUESTS` / `_TOKENS`,
    `SystemPromptInput.budget_guidance`): requests = batch independent calls, read whole files, load
    skills and memory in the first step, don't stop to confirm what you can decide; tokens = keep the
    context small. No numbers in it (the knobs can change mid-session, the prompt cannot). `balanced` adds
    nothing, so its prompt is byte-identical to before.
  - `tokens`: `SkillStore::index_prompt_with(names_only)`: names only, and without the "err on the side of
    loading" push (the `skills_index: NamesOnly` knob from CP3.1 now has its consumer).
- **Tool schemas that must not lie** (reviewer's item): the schema text is the same for every session of
  every profile, so `read_file`'s `limit` lost "default: 2000, max: 2000" (and `default`/`maximum`) and now
  says it follows the budget profile and that `next_offset` continues a longer file; `delegate_task`'s
  `tasks` lost "up to 10" (the refusal already states the real limit). `prompts/tools/README.md` updated.
  Consequence: existing sessions' schema text changes once on upgrade (one prompt-cache miss), and
  `balanced` no longer tells the model the number 10. `prompts/hermes/...` (extracted Hermes text) untouched.
- Not done: **ACP session config option** (not cheap: the ACP server has no plumbing for a config option
  that isn't a model or mode). ACP reads the profile from the config at start.
Files: crates/runtime/src/{budget.rs,lib.rs,delegate.rs}, crates/runtime/tests/turns.rs,
  crates/prompts/src/{system.rs,lib.rs}, crates/tools/src/skills.rs, crates/desktop/src/{gateway.rs,rest.rs},
  crates/desktop/patches/0022-budget-profile-setting.patch (+README), prompts/tools/{read_file,delegate_task}.json
  (+README), scripts/perf/budget-check.mjs (+README), GAPS.md, PORTING.md, docs/{ARCHITECTURE,TESTING}.md
Tests (CARGO_TARGET_DIR=target/claude):
  - `cargo test -p hermes_runtime`: lib 56, `turns` 93 passed / 7 ignored (4 new: wording per profile at
    session start; a running session keeps its prompt across a switch while a new session gets the new
    wording; `tokens` lists skill names only; tool schemas state no profile-dependent number). The same
    `turns` under `HERMES_TEST_PROFILE=requests` and `=tokens`: 93 / 7 ignored.
  - `cargo test -p hermes-desktop`: lib 165 (new: `a_data_folder_without_the_key_runs_requests`,
    `config_set_switches_the_profile_and_refuses_bad_values`, the REST default/select/filter test), `acp` 67.
  - `cargo test -p hermes_tools -p hermes_prompts -p hermes_agent -p hermes_llm -p hermes_memory -p
    hermes_store`: green (new: names-only skills block; budget guidance in the stable tier only).
    `budget.rs`: 3 new unit tests (no key = `requests`, overrides on top, 0 refused).
    `cargo build --workspace`: OK, no warnings.
  - Live: `scripts/perf/budget-check.mjs` against my own instance (port 47810, debug build): 17 checks, all ok.
Request counts: unchanged per profile (CP3.1's table). What changes is the default: a real user's turns now
  run `requests` caps (150 iterations, 2 strikes, 90% compression, 3 children). The `turns` baselines are
  unchanged: its harness pins `balanced` unless told otherwise.
Deviations from the plan:
- `for_current_user` is `requests` (the plan: default for everyone); the test harness pins `balanced` by name.
- Overrides are flat `budget.<knob>` keys, not a nested `overrides` object (`deep_merge` and the dotted
  `config.set` handle that for free). They are reachable through `config.set` and the config file, not the
  Settings page (one field only; more fields would mean more patches).
- `skills_index: NamesOnly` (tokens) is implemented now although the plan's table lists Phase 5 for the
  index: step 4 ("prompt text that depends on the profile") covers it and the knob already existed.
- `a_skill_written_in_one_session_is_indexed_and_invocable_in_the_next` asserts full skill descriptions, so
  it pins `balanced`; under `tokens` the index has no descriptions by design.
For the reviewer:
- **Prompt text chosen at session start only**: `PromptSource.budget` is a copy taken in `prompt_source()`;
  `create_session` builds the prompt once and stores it. Two paths rebuild a *running* session's prompt:
  `prompt_rebuild` after a compaction and `set_session_route` (model switch). Both already rewrite it on
  purpose (compression is the documented exception; a model switch already misses the cache), but they now
  take the *current* profile's wording, so a session created under `requests` and compacted after a switch
  to `tokens` gets the tokens wording. Left as is: storing the profile per session needs a side file. Say
  if you want that.
- **Children's prompts** (`child_base_prompt`) use the profile as it is at delegation time; they have their
  own conversation, so there is no cache to protect.
- **Configs without the key**: three places make it `requests`: `config_defaults()` (the page and
  `config.get` see it), `from_config` (missing/unknown/non-string), `for_current_user`. Test:
  `a_data_folder_without_the_key_runs_requests` starts a gateway on a `desktop-config.json` without it.
- **Validation split**: `config.set` rejects (error to the caller), `PUT /api/config` silently drops invalid
  `budget.*` entries (the page can't show a rejection for one key of a bulk save), a hand-edited file is
  tolerated. The ranges live in one table (`apply_override`); check the ceilings (`read_file_max_lines`
  100000, `max_iterations` 10000, ...) are acceptable.
- **Schema text**: `read_file` no longer states a limit and `balanced` no longer states 10 children; check
  you are fine with the model not knowing the numbers (tool results and the delegate refusal state them).
- **ACP**: no config option; ACP is a separate process that reads the config once at start, so a profile
  switched in the desktop applies to an already-running ACP agent only after its restart.
- **Patch 0022 is untested in the page** (no webapp build here). Two small hunks; the schema side is checked
  live (`budget-check.mjs`, first check).
- `cargo fmt`: `budget.rs` formatted; the other touched files keep the drift from earlier phases.

### Review (Opus 5.5)
`main` had not moved since the branch was cut (no merge). `/code-review` not run (not available in
this subagent); manual pass over every changed function and the code around it.

Review focus, with evidence:
- **Prompt text chosen at session start only.** `create_session` builds the prompt once from
  `prompt_source(self.budget())` and stores it in `sessions.db`; every request sends
  `session.system_prompt` (`ops.rs`), and `Runtime::open` loads it from the store, never rebuilds it.
  So a running session keeps it across a switch (`switching_the_profile_leaves_a_running_sessions_prompt_alone`)
  and a resumed one after a restart under another profile keeps it too (**new test**
  `a_resumed_session_keeps_the_wording_it_started_with`: second runtime on the same data folder under
  `tokens`, same system prompt byte for byte). The wording sits in the stable tier and states no number.
  Tool schemas lost their profile-dependent numbers, so they are the same for every session: one
  cache miss on upgrade, then stable.
- **Configs without the key get `requests`**: `config_defaults()` (page and `config.get`),
  `from_config` (missing / unknown / non-string), `for_current_user`. Tested in `budget.rs`,
  `rest.rs` and `a_data_folder_without_the_key_runs_requests`; live check's second line.
- Ground rules: `config.get/set` and `PUT /api/config` keep their shapes (`{key, value, ok}`, `-32602`
  for a refusal, as other arms); `delegation.status` keeps its shape, only the number is now the
  effective one. No schema change (profile lives in `desktop-config.json`). No new blocking work in
  async handlers (the config write in `config.set` is the existing path). PORTING row is `diverged`.
- Patch 0022: applied 0001–0022 in order to a `git archive` of the pinned commit in a scratch folder:
  all apply. Read against the consumer: `sectionFieldEntries` (helpers.ts) takes `SECTIONS` keys and
  `schema[k]`; `config-field.tsx` renders `type: "select"` from `schema.options` and takes the
  description from the schema; without the `budget.` prefix on the Model ▸ Main subpage the field
  would belong to no subpage (and Hermes's own `subpages.test.ts` would fail). Not built (no npm
  packages here), so not driven in the page.

Implementer's doubts:
- **Rebuild after compaction / model switch takes the current profile's wording**: left, by design.
  Both already rewrite the prompt (fresh memory snapshot and skills index too), so the cache is missed
  anyway, and the wording then matches the knobs the session runs under. Storing the profile per
  session (a side file) would buy nothing for caching. Documented in `budget.rs` and PITFALLS.
- One-time schema-text cache miss on upgrade: acceptable (the same happens on any Hermes update of a
  schema), and better than a schema lying under `requests`.
- Override ceilings: **fixed** `result_size_pct` (was ≤1000; ×10 of 15% of the window lets one tool
  result exceed the window): now ≤400, `requests`' own value. Others are fine (`read_file` is also
  capped by its 100k-char budget; iteration counts only bound a runaway).

Other findings, fixed:
- **A turn could mix two profiles**: `begin_turn` copied the knobs once, but `prompt_rebuild` and
  `child_base_prompt` called `prompt_source()`, which read `Runtime::budget()` again, so a
  `config.set` landing mid-setup gave the compaction rebuild / children a different profile's wording
  than the turn's knobs. `prompt_source` now takes the knobs; the turn and `/compress` pass their copy.
- `PUT /api/config` with a non-object `budget` (`"budget": "tokens"`) replaced the stored object whole
  (`deep_merge`); now dropped like other invalid budget values (test extended).

Left: ACP session config option (not cheap, as the note says; ACP reads the config at start);
`cargo fmt` drift from earlier phases.

Tests (CARGO_TARGET_DIR=target/claude): `hermes_runtime` lib 56, `turns` 94 passed / 7 ignored, the
same under `HERMES_TEST_PROFILE=requests` and `=tokens`; `hermes-desktop` 165 + `acp` 67;
`hermes_tools`/`prompts`/`agent`/`llm`/`memory`/`store` green; `cargo build --workspace` OK.
Live: `budget-check.mjs` on my own instance (47810, worktree debug build): 18/18 ok; instance stopped.
