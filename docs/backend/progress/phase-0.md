# Phase 0 progress

## CP0.1 — Groundwork: rules and request-counting harness
Worktree: C:\Users\alexandros\repos\hermes-rs-p0   Branch: backend/p0-groundwork   Model: Sonnet 5.5
Done:
- AGENTS.md rule 1 split into *Protocol* (ported exactly) and *Backend* (ours, deviations
  documented and `diverged` in PORTING.md); link to this plan in the "Read next" table.
- docs/WORKFLOW.md intro states the two rules; PORTING.md Map section explains `diverged`.
- `turns.rs`: the mock `Script` counts requests per `x-hermes-rs-purpose` header (absent = `main`)
  under one lock; `Purpose` enum, `request_count`, `assert_requests(&h, Purpose, range)`.
- Baseline scenarios S1–S5 (`baseline_s*` tests); numbers recorded in PHASES.md *Baselines*:
  S1 5, S2 2, S3 3, S4 11, S5 4 (all `main` until Phase 2).
Files: AGENTS.md, PORTING.md, docs/WORKFLOW.md, docs/backend/PHASES.md,
  docs/backend/progress/phase-0.md, crates/runtime/tests/turns.rs
Tests: `cargo test -p hermes_runtime --test turns` (CARGO_TARGET_DIR=target/claude): 74 passed,
  7 ignored (live-model tests), 0 failed. `cargo build --workspace` not run: no shared crate
  changed (test file and docs only).
Request counts: first baselines, as above.
Deviations from the plan: the helper takes the harness (`assert_requests(&h, ...)`) rather than the
  mock, since the mock lives inside `Harness`. S4 waits on the mock's request counter, not the
  review bridge: the review only calls `review_finished` when it saved something.
For the reviewer: S4's 11 = 10 turns + 1 review request, counted as `main` today; after Phase 2
  this becomes 10 main + 1 review. The S4 wait has a 0.5 s grace sleep to catch a stray extra
  request (a timing-based negative check; make it stricter if it proves flaky). The second mock
  (`RateLimited`, daily-limit test) is not counted by purpose. PORTING.md has no `diverged` rows
  yet; the marker is only documented.
Review (Opus, 2026-11-12): accepted with fixes.
- S2 as written (empty first reply) never reached `EmptyResponseRecovery`: with no tool call
  before it, `agent/inference.rs` retries the empty reply itself. S2 is now tool round → empty →
  recovery nudge → answer (3, asserts the nudge was sent); the old case is kept as S2b (2).
- S4 now asserts the 11th request is one combined memory + skill review prompt.
- Counter: one lock per request, independent of the `requests` lock; fine under parallel children.
- Rule 1 wording keeps the protocol strict; numbers are today's behaviour, no runtime code changed.
Tests: `turns` 75 passed, 7 ignored, 0 failed.
