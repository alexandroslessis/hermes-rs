# Phase 4 progress

## CP4.1 — heuristic titles, no main-model approval judge, review not cancelled by a new turn
Worktree: C:\Users\alexandros\repos\hermes-rs-p4   Branch: backend/p4-sidejobs   Model: Sonnet 5.5 (subagent)
Cut from `main` at 4a268c4 (Phases 0-3 merged). Uncommitted.
Done:
- **Step 2, heuristic titles.** New `crates/runtime/src/titles.rs`: `heuristic_title(opening)`: drops fenced and
  inline code and `@refs`, takes the first sentence (first non-empty line, up to `.`/`!`/`?`), drops leading filler
  ("can you please ...") and articles, caps at 6 words, title case (identifiers, file names, acronyms and numbers
  keep their spelling). Empty when nothing is left (the gateway then keeps the old "first line, 60 chars" title).
  `budget.rs`: `requests` now has `titles: TitleMode::Heuristic` (first consumer of that knob).
  `gateway.rs` (`submit`): when `titles == Heuristic` and no `auxiliary.title_generation` model is set, the title is
  `heuristic_title(shown)` at turn start and **no** model title is requested afterwards (`titling` is `None`). With an
  aux title model, or `balanced`/`tokens`, nothing changes (first line at once, model title after the turn).
- **Step 3, smart approval.** `begin_turn`: with no aux approval model, the judge falls back to the main model only if
  `budget.side_jobs_may_use_main_model()` (new; false for the `requests` profile). Under `requests` the guard is `None`,
  so a flagged command goes straight to the user. An aux approval model still judges under every profile.
- **Step 4, review no longer cancelled.** `OpenSession.review` holds a `ReviewHandle {cancel, started}` instead of a bare
  token. `begin_turn` no longer cancels it. The review task (`review::review`) now waits, polling every 100 ms (cancel-aware),
  until `session.running` is `None`, and takes its conversation snapshot *under the `running` lock* (which `begin_turn`
  holds while it claims the session, before it touches the conversation), then sets `started`. So: a turn that starts before
  the snapshot pauses the review (it then reviews the conversation including that turn); once it has the snapshot it
  finishes on its own copy while the turn runs. `ReviewSetup::start`: a newly due review is skipped while one that has
  started is running; a paused one is replaced (cancelled, since the new one sees the same conversation); `/refine`
  (`requested`) always replaces.
Files: crates/runtime/src/{titles.rs (new),budget.rs,lib.rs,review.rs,session.rs}, crates/runtime/tests/turns.rs,
  crates/desktop/src/gateway.rs, crates/desktop/tests/acp.rs, PORTING.md (row, diverged), GAPS.md, docs/PITFALLS.md
Tests (CARGO_TARGET_DIR=target/claude):
  - `cargo test -p hermes_runtime`: lib 62 (6 new: `heuristic_title`; budget test updated), `turns` 99 passed / 7 ignored
    (was 94): the same under `HERMES_TEST_PROFILE=balanced`, `=requests` and `=tokens`: 99 / 7 ignored each.
  - New `turns` tests: `s4_makes_no_title_and_no_approval_requests` (all profiles: 0 `title`, 0 `approval`; main 10 /
    review 1 stay asserted by the baseline S4),
    `requests_profile_smart_approval_without_an_aux_model_asks_the_user` (0 approval requests, the bridge is asked),
    `an_aux_approval_model_still_judges_and_balanced_still_falls_back` (1 approval request each),
    `a_turn_starting_mid_review_does_not_cancel_it` (slow mock: review request held 1.5 s; second turn completes; the review
    still makes its 2nd request, saves its skill and reports; its copy lacks the 2nd turn's text; session has no review
    messages), `a_turn_starting_before_the_reviews_first_request_pauses_it` (`refine` then `start_turn` with no yield: the
    review request comes after the turn's and contains it). Harness: `harness_scripted` (replies and delays per purpose).
  - `smart_approvals_let_the_reviewer_decide_...` now pins `balanced` (it tests the main-model judge).
  - `cargo test -p hermes-desktop`: lib 165, `acp` 68 (1 new: under `requests` the thread gets the heuristic title
    "Introduce Yourself" and the mock sees no title request). The ACP test agent now writes `desktop-config.json` with
    `budget.profile = balanced` by default (else `a_thread_gets_a_title_update` would see the new default).
  - `cargo build --workspace`: OK, no warnings. (`target/hermes-src` junction made, gitignored.)
Request counts: S1-S5 unchanged on the main provider. Title requests under `requests` without an aux title model:
  1 per new session -> 0 (desktop/ACP path; the runtime's own turns never made any). Approval-judge requests under
  `requests` without aux approval: 1 per flagged command with smart approvals on -> 0. A review started by turn N
  and running when turn N+1 starts: was cancelled (its requests wasted) -> finishes.
Deviations from the plan:
- Step 4 "pauses it": implemented as the review waiting for `running` to clear before its snapshot rather than an
  explicit pause call from `begin_turn`; same observable behaviour, no race window between "turn starts" and "review checks".
- A newly due review is skipped (not queued) while one that already started is running; the nudge counters were already
  reset, so that review is lost, not delayed.
- No live check (`scripts/perf/*-check.mjs`) added: the page calls nothing new. Not run against `perf.ps1` either.
- `side_jobs_may_use_main_model()` derives from the profile name (not a table knob); overrides keep the profile.
For the reviewer:
- **Race review of a review concurrent with a turn.** Review: private `OpenSession` fork (own conversation, `context`),
  `persist: false` (no message writes), `record_usage: true` (one `usage` insert per request into `sessions.db`, a
  mutex-guarded SQLite connection). Shared with the turn: `session.tools` (memory_failures counter, read-state, memory and skill
  stores). Memory store writes take its own mutex; skills' `manage_with` should be checked the same way (I did not read it
  to the end). `ReviewTools` dispatch of `read_file`/`skill_view` marks files read in the shared tool state: a concurrent review
  could satisfy the turn's read-before-edit guard (this already happened after turns; concurrency makes it likelier).
- `usage_totals` takes the *last* row's `input_tokens` as the context gauge: a review's row can land after the next turn's
  first request and show the review's size until that turn's next request. Not fixed (store schema/semantics).
- `RunGuard`/`running` lock order: review takes `running` then `conversation`; no code path takes them in the other order
  that I found (`begin_turn` releases `running` before touching the conversation) - please re-check.
- The snapshot-then-`started` happens under the lock, but `ReviewSetup::start` replaces a not-started handle without
  waiting for its task to exit; the replaced task sees its token cancelled at its next 100 ms poll and returns `Ok(vec![])`.
  If it had just passed the `running` check it would set `started` on its own (detached) handle: harmless (the slot holds the
  new handle), but it would then run to completion against a cancelled token (the state machine stops at cancel).
- Pause test relies on the single-threaded test runtime (spawn, then start_turn with no await); documented in the test.
- Heuristic quality: small ("Why Does Hang" for ``Why does `cargo build` hang``: inline code is dropped as the plan says).
  Check the filler list (`i`, `we`, `to`, `go`...) is not too aggressive for non-English openings; non-English text passes
  through title-cased by `char` uppercase rules.
- Live perf checks that assert a model-written title (`aux-models-check.mjs` etc.) were not run; under the new default
  profile they may need `budget.profile=balanced` or an aux title model.

### Review (Opus 5.5)
Main had not moved (branch at 4a268c4); no merge. `/code-review` not run (manual pass instead).
Verdict: sound; the concurrency design holds. Findings, point by point:
- **(d) locks/races.** Order is `running` -> `conversation` in the review; no path takes `conversation` then
  `running` (checked every `running.lock()` site in `lib.rs`); `curator.rs:61` takes `running` then `review`. Both
  are `parking_lot` guards never held over an `.await`. A turn can't start between the check and the snapshot
  (both under `running`). No starvation beyond "waits while turns run back to back"; no deadlock. **Fixed** a gap:
  `ReviewSetup::start` checked `has_started()` under the `review` lock only, so a paused review on another worker
  could take its snapshot between the check and its cancellation (one wasted request). `start` now holds
  `running` (then `review`) across check-and-replace.
- **(f) signals.** **Fixed** two losses when a paused review is replaced: a newly due skills-only review dropped the
  paused one's `memory` flag (now OR-ed in: `ReviewHandle` carries `memory`/`skills`/`requested`), and a due review
  replaced a paused `/refine`, so the user's report never came (a paused `/refine` now stays). New test
  `a_paused_refine_is_not_replaced_by_a_review_that_falls_due` (fails without the fix). The skip of a due review
  while a started one runs is left: not lost, only delayed, since every review reads the whole conversation; it
  is lost only if the session never reaches the next nudge. Re-arming the nudge would cost requests.
- **(b) read state.** Real, and made likelier by concurrency: the review's `read_file` stamped the session's
  `FileState`, so the turn could `write_file` a file its model never read. **Fixed**: `files::SIDE_READ`
  (task-local) set by `ReviewTools` around its `read_file`; `read_file` skips the session stamp under it (the
  review's own `read` set still records it). Test `a_review_read_does_not_count_as_the_sessions` (hermes_tools).
- **(a) shared tools.** Skill writes serialise on `SkillStore::write_lock` (`apply_ops`), memory on its store
  mutex, taken in the same order (`memory_failures` then store) by turn and review. Left: `begin_turn` resets the
  shared `memory_failures` counter, so a turn and a concurrent review share one per-turn failure budget (benign;
  noted in PITFALLS). `apply_ops` runs file IO synchronously in an async task (pre-existing, not this checkpoint).
- **(c) gauge.** Left: the review's usage rows already landed after the turn before this change, and its input
  is about the conversation's size, so the gauge drift is small and brief; fixing it means a store change.
- **(e) pause.** Accepted: the plan's observable behaviour ("pauses if no request yet") holds; `started` is set
  at the snapshot, just before the first request.
- **(g) titles.** Filler is stripped only at the start, so non-English text mostly passes through (a German
  "Will"/"So" opener is lost: acceptable). **Fixed**: numbered/bulleted/heading lines gave titles like "1"; URLs
  made titles unbounded (now dropped, and titles capped at 60 chars like the first-line fallback); CJK full stops
  now end a sentence. No panics found (slices are on char boundaries; tests for empty, whitespace, unclosed code,
  Cyrillic, Japanese, 10k chars). **Fixed** a false doc claim: a heuristic title is *not* upgraded later when an
  aux title model appears (only new sessions get model titles; re-titling needs a marker the frozen schema lacks).
- Ground rules: no protocol shape changes, no schema change, no new blocking work, prompts untouched, PORTING row
  `diverged` (extended for the fixes), module docs state the deviation. Request counts in `turns.rs` asserted.
- Left: no live check (the page calls nothing new); `aux-models-check.mjs`-style perf checks expecting a model
  title were not run and may need `budget.profile=balanced` under the new default.
Tests after fixes (CARGO_TARGET_DIR=target/claude): `hermes_runtime` lib 63; `turns` 100 passed / 7 ignored under default, `balanced`, `requests` and `tokens`; `hermes_tools` 117 (+1); `hermes-desktop` lib 165, `acp` 68; `cargo build --workspace` OK.

## CP4.2 — one-click local aux models, compression via aux with a clean fallback
Worktree: C:\Users\alexandros\repos\hermes-rs-p4   Branch: backend/p4-sidejobs   Model: Sonnet 5.5 (subagent)
On top of CP4.1 (d302a5c). Uncommitted.
Done:
- **Step 1, one click.** `aux_models.rs`: `local_servers` (local providers from Settings that answer `/models`, then
  Ollama `:11434/v1` and LM Studio `:1234/v1` when not already configured; 3 s probe each; embedding models left out),
  `local_list` (`GET /api/model/auxiliary/local`) and `use_local` (`POST /api/model/auxiliary/local`, body optional
  `{provider, model}`): picks the server (first that answers) and its model (the endpoint's `default_model` if served,
  else the first listed chat model, sorted), adds a well-known server that isn't a provider yet via `endpoints::save`
  (not made the default), then reuses `assign` with `task: ""` so all four slots (compression, approval, title_generation,
  background_review) take it. 400 with a plain `detail` and nothing changed when no server answers or the model/provider
  is unknown. Routes in `gateway.rs` (clears the context-length cache). Page: **patch 0023** (button "Use a local model
  for background jobs" beside "Reset all to main", calls `assignLocalAuxiliaryModels`); checked with `git apply --check`
  against the pinned source, NOT built into a webapp or seen in the page (same status as 0021/0022). Protocol shapes Hermes
  expects are untouched (`/api/model/auxiliary`, `/api/model/set`); the new route is ours and absent on other backends.
- The `requests`-profile half of step 1 (no aux model: titles use the heuristic, approval asks the user, review waits for
  idle and goes through the governor, curator through the governor) was CP4.1 plus Phase 2; nothing new needed. The governor
  already checks the review's *aux* provider id (`review_provider_id`), so a local aux review isn't held back by the main quota.
- **Step 5, compression via aux.** The aux compression model was already used (Phase "2.9"); what was missing is the failure
  path. `compress::Summarizer` gets `fallback: Option<Box<Summarizer>>`; `summarizer_for` builds the aux summarizer with the
  session's model as fallback, and gives the aux provider at most one HTTP retry (`http_retries.min(1)`): a refused local
  connection otherwise backs off 1+2+4 s under `requests`. `summarize` tries the aux, on any error (down, HTTP error, empty
  reply) logs a warning and asks the session's model, i.e. exactly what happens with no aux model (one `compress` request
  on the main provider, only in that failure case). If that fails too, unchanged: `compression_failed` for the turn /
  `Failed` for `/compress`. Chosen over a deterministic fallback: no new summary quality to invent, "as today".
Files: crates/desktop/src/{aux_models.rs,gateway.rs}, crates/desktop/patches/{0023-local-aux-models-button.patch,README.md},
  crates/runtime/src/{compress.rs,lib.rs}, crates/runtime/tests/turns.rs, scripts/perf/{local-aux-check.mjs (new),README.md},
  GAPS.md, PORTING.md (2 rows, diverged), docs/{ARCHITECTURE.md,PITFALLS.md}
Tests (CARGO_TARGET_DIR=target/claude):
  - New `turns` tests: `compression_via_the_aux_model_makes_no_summary_request_on_the_main_provider` (0 `compress` and 4
    `main` on the main mock, 1 request on the aux mock, handoff has the local summary),
    `compression_falls_back_to_the_main_model_when_the_aux_server_is_down` (closed port: completes in < 10 s, 1 main
    `compress` request, handoff has the main summary), `compression_falls_back_when_the_aux_server_errors` (aux answers 500),
    `auto_compression_uses_the_aux_model_and_falls_back_when_it_is_down` (in-turn `AutoCompress`, alive and dead).
  - `cargo test -p hermes_runtime --test turns`: 104 passed / 7 ignored (was 100) under default, `balanced`, `requests`, `tokens`.
  - `cargo test -p hermes-desktop`: lib 168 (3 new), `acp` 68; `hermes_runtime` lib 63; `cargo build --workspace` OK.
  - New desktop unit tests (`aux_models.rs`, wiremock servers): configured local server (embedding model dropped, all four
    slots set, main model and the rest of `config.json` untouched, wrong model refused), well-known server added as a provider
    then not duplicated, no server up = 400 and nothing changed.
  - Live: `scripts/perf/local-aux-check.mjs 47810` against my own instance (debug build, `target/perf`, stopped afterwards):
    all 19 checks pass (server found, refusals, one click, main model untouched, `/compress` on the local server with 0 `compress`
    requests on the main provider, local server stopped -> main model writes the summary, reset, endpoint removed).
Request counts: baselines S1-S5 unchanged. Changed: with an aux compression model, summary requests on the main provider
  1 -> 0 per compaction (already so before; now tested); aux down: 1 main `compress` request instead of a failed
  compaction (and no 7 s retry stall).
Deviations from the plan:
- Fallback is the session's model, not a deterministic summary (the plan allowed either).
- "Wait for idle" for review/curator under `requests` without an aux model is not new code (CP4.1 + governor).
- The page button is a patch (0023), because Hermes's UI has per-job rows and "Reset all to main" but no apply-to-all; the
  one-click route is ours rather than a sentinel in `/api/model/set` (the probe is async; `assign` is sync).
For the reviewer:
- Fallback fires on *any* aux error, including a 4xx for a wrong model name: the user's aux model is then silently replaced by
  the main model for that compaction (a warning in the log only; `AgentEvent::CompactionFailed` is not emitted). Is a quiet
  fallback right, or should the chat say so?
- A hung (accepting but not answering) local server is bounded only by the HTTP client's timeouts before the fallback runs.
- `use_local` default model = first listed model alphabetically (embedding models filtered by the substring `embed`): not
  size-aware. Ollama's native API could rank by size.
- `use_local` and the apply-to-all overwrite a user's per-job choices without asking (the button's name says so).
- `endpoints::save` adds the provider with `discover_models: true` and no context length; the aux context window is then learned
  by the gateway's existing lookup.
- Patch 0023 untested in a real webapp build (needs npm). `window.hermesDesktop.api` + `capabilityScoped` follow `setModelAssignment`.
- Pre-existing, left: `session.compress` answers `status: "noop"` (and the page "Nothing to compress yet.") for a real
  compaction, because it counts visible messages and compaction hides rather than removes them (see PITFALLS). The live check
  asserts on `before_tokens`/`after_tokens`. Fixing means comparing agent-visible tokens in `gateway.rs`; not CP4.2's scope.
- Style: the files I touched read CRLF in the working copy and I wrote LF back through a script; `git diff` shows only real
  changes, but check `git diff --stat` for line-ending noise before committing.

### Review (Opus 5.5)
Main had not moved (branch at d302a5c); no merge. `/code-review` not run (manual pass instead).
Verdict: sound; the fallback holds for a refused, erroring and empty-reply server. Findings:
- **Hung aux server: not bounded in practice.** The summary request streams, so only the client's 600 s read
  timeout applied, per attempt (with the 1 retry: ~20 min of a stalled turn before the fallback). **Fixed**:
  `Summarizer.deadline` (`compress::AUX_SUMMARY_DEADLINE` = 300 s, set only on the aux summarizer; Hermes bounds a
  summary at 120 s idle / 600 s total) wraps the aux try; past it the session's model writes the summary. Unit test
  `a_hung_aux_server_falls_back_at_its_deadline` (a listener that never answers, 1 s deadline, fallback answers).
- **4xx for a wrong model name**: goes through the same `Err` arm as a 500 (covered by
  `compression_falls_back_when_the_aux_server_errors`); empty reply is an `ensure!` error, same arm. No stall: 1 retry.
  No double billing of the main provider: the aux attempt is on the aux provider's ledger row; the main gets one
  `compress` request only when falling back. Ground rule 8: only compaction rewrites messages.
- **Visible notice for the fallback: left as a log `warn`.** The compaction succeeds and costs what it did before
  Phase 4; showing it would need a new runtime event and a page kind. Documented in PITFALLS and PHASES.
- **Probes.** **Fixed**: a configured local provider with `discover_models` off was offered without being probed
  (a down server could be chosen); now every candidate must answer `/models`. Probes ran one after another (each hung
  server cost 3 s); now side by side (`join_all`). Test `hung_servers_are_bounded_by_the_probe_timeout`, and the
  down-server test now includes a fixed-list provider.
- **Ground rule 7.** **Fixed**: `local_servers` read `config.json` and the key store (`provider.runtime()` -> keyring)
  on the async worker, and `use_local` wrote `config.json` (`endpoints::save`, `assign`) there; both now in
  `spawn_blocking`. (The neighbouring `/api/model/set` arm still calls `assign` inline: pre-existing, left.)
- **Rule 1/5.** Hermes's routes keep their shapes; the new `GET/POST /api/model/auxiliary/local` is ours, now said in
  the module doc and PORTING (`diverged`); an older backend answers 404 and the page shows its error. OK.
- **Line endings.** `.gitattributes` has `* text=auto eol=lf`: the index is LF, the CRLF warnings are only about
  the working copy; `git diff --stat` shows real changes only. Nothing to restore.
- **Patch 0023.** 0001..0023 apply cleanly in order to `git archive c419cf7 apps/desktop locales`; every
  identifier it uses exists there (`capabilityScoped`, `ModelAssignmentResponse`, `setApplying`, `setSwitchStaleAux`,
  `setCaughtError` -> `readableError`, which shows the 400's `detail`). Not built (no npm packages).
- Note: the main provider, when it is itself on localhost, is listed as a local server too (the live check's mock
  is): harmless, the one click then picks it only if it is first and nothing else was asked.
- Perf checks and titles: only `aux-models-check.mjs` asserts a model-written title, and it sets an aux title model,
  so it passes under `requests`; no check needed `budget.profile=balanced`.
Tests after fixes (CARGO_TARGET_DIR=target/claude): `hermes_runtime` lib 64 (+1); `turns` 104 passed / 7 ignored under
default, `balanced`, `requests`, `tokens`; `hermes-desktop` lib 169, `acp` 68; `hermes_tools` 117, `hermes_llm` 468,
the others green; `cargo build --workspace` OK, no warnings. Live: `local-aux-check.mjs 47810` against my own instance
(debug build, `target/perf`): 18/18 ok; instance stopped.
