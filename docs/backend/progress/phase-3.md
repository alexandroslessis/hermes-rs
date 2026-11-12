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
