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
