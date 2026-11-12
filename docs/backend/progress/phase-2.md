# Phase 2 progress

## CP2.1 — Observer, purpose context, ledger actor, side file
Worktree: C:\Users\alexandros\repos\hermes-rs-p2   Branch: backend/p2-ledger   Model: Sonnet 5.5
Cut from `main` at 5a9d388 (Phases 0 and 1 merged).
Done:
- `crates/llm/src/observer.rs`: `Purpose` (the 8 purposes, `as_str`/`parse`), `RequestObserver`
  (`on_request`, `on_response` with `Outcome::{Status, TransportError}`), `RequestMeta`,
  `RequestContext {purpose, session_id, observer}` as a tokio task-local with `scope`, `current`
  and `spawn_in_context` (a plain `tokio::spawn` loses the context; documented in the module).
- `ApiClient` (`api_client.rs`): `with_provider_id`; `response_post` and `multipart_post` announce
  each HTTP attempt to the current context's observer and report the status (429s included).
  Retries call `response_post` again, so each try is one observation; streaming requests are
  observed too. A POST outside any context isn't observed and logs one `warn`.
- `crates/runtime/src/ledger.rs`: `Ledger` = handle to one OS thread owning the state and
  `data_dir/requests.json` (version 1: provider -> reset_offset_minutes + days -> by_purpose +
  rate_limited). Hot path is a channel send. Debounced write every 2 s when dirty, write-then-
  rename, final write when the last handle drops, unreadable file set aside as `.bad`, 31 days kept,
  day = calendar date at the provider's fixed UTC offset. `snapshot()` (async, oneshot),
  `flush_blocking()`, `observer()`, `context(purpose, session)`.
- Runtime: `Runtime::ledger()`, `ProviderConfig::client()` labels the client with the provider id,
  and the main turn's task runs inside `observer::scope(Purpose::Main)`.
Files: crates/llm/{Cargo.toml,src/lib.rs,src/api_client.rs,src/observer.rs,tests/request_observer.rs},
  crates/runtime/{src/lib.rs,src/ledger.rs,tests/turns.rs}, Cargo.lock (llm dev-dep parking_lot)
Tests (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_llm -p hermes_runtime`: llm lib 465 +
  stream 1 + observer 3, runtime lib 34 (8 ledger), turns 79 passed / 7 ignored. New: observer unit
  tests, `request_observer.rs` (streaming once, every retry attempt, no context / plain spawn not
  counted), ledger unit tests (rollover across offsets, 31-day pruning, debounce + drop + reopen,
  8-thread burst of 2000 loses none, corrupt file, observer + 429), and a `turns` scenario (the
  ledger's main count equals what the mock served and `requests.json` is written).
  `cargo build --workspace --exclude hermes-desktop` OK. `hermes-desktop` not built here (this
  worktree has no `target/hermes-src`); it doesn't construct anything this checkpoint changed.
Request counts: unchanged (the baselines in PHASES.md are mock-side counts).
Deviations from the plan:
- `RequestMeta` has no attempt number: each HTTP attempt is its own observation, which is what the
  ledger needs; adding the number would mean setting another task-local in both retry loops.
- Counted: POSTs only (chat/completions, multipart); GETs (model lists) aren't requests against
  the daily quota in the providers we know. Easy to widen in `ApiClient` if a provider counts them.
- Day boundary is a fixed UTC offset in minutes (`Ledger::set_reset_offset`), not a named time
  zone: the provider settings with `reset_tz` and `daily_request_limit` are CP2.2.
- The observer travels in the task-local context (not on `ApiClient`): `ProviderConfig` is built
  by struct literal in ~17 places and `provider()` is called from many free functions, so a field
  there would have touched them all. Consequence below.
- The `x-hermes-rs-purpose` header (plan: loopback hosts only) is NOT sent yet; the mock-side
  per-purpose counters in `turns.rs` still see everything as `main`. CP2.2.
For the reviewer:
- **Undercounting until CP2.2**: only the main turn runs inside a context. Review, curator,
  titles, manual compression, `/bg` and `/btw` are spawned or called outside it, so they are not
  counted yet (one `warn` is logged the first time). The approval judge, auto-compression and
  delegate children run *inside* the turn task and are counted, but as `main` (corrected by the
  reviewer; see the call-site list in the review below). CP2.2 labels every caller; the review
  focus there is "no caller left unlabelled".
- Task-locals: `tokio::spawn` inside a scoped task loses the context. The main turn spawns the
  review (`review_setup.after_turn`), delegate children, forwarders... none use
  `spawn_in_context` yet.
- Ledger thread lifetime: it ends when the last `Ledger` clone drops (so with the `Runtime`). The
  observer inside a `RequestContext` holds a clone, so a long-lived context keeps it alive.
- `std::sync::mpsc` unbounded channel: a stalled disk can't grow it much (the thread only
  applies counts; I/O is at most every 2 s), but there is no cap.
- The test `changes_are_written_after_the_debounce...` waits up to 5 s for the file (poll), no
  fixed sleep.

### Review (Opus 5.5)
`main` had not moved (still 5a9d388), so nothing was merged. Ran `/code-review high` and checked the review focus by reading the code.

**Review focus**
- *Once per HTTP attempt*: in production, every model POST goes through
  `OpenAiCompatibleProvider::stream_payload` → `with_retry(|| … response_post)`, which builds a
  new request on each try, so each attempt is observed once. A streaming request is counted when
  its headers arrive (`send_bounded`); a later body failure doesn't count it again, and a turn-level
  retry is a new `response_post`. Nothing else sends model requests: `multipart_post` has no callers.
  The other `reqwest` users are `web.rs` (search/scrape APIs), `vision.rs` (an image GET),
  `canonical/registry.rs` (a catalog GET) and the GET-only discovery calls. The only non-model
  POST is Ollama's `api/show` during discovery (see fix 1).
- *Hot path*: `begin` → `current()` (task-local clone) → `LedgerObserver::record` →
  `std::sync::mpsc::Sender::send` (lock-free list channel). It takes no lock and does no I/O; it
  makes a few small `String` allocations, which I left alone.
- *Task-locals across spawns*: model-making call sites and their context, for CP2.2:
  | Call site | Context now | CP2.2 |
  |---|---|---|
  | main turn, `runtime/src/lib.rs` `tokio::spawn(scope(Main, …))` | Main | done |
  | approval judge (`lib.rs` `assess`, ~L146), runs in the turn | inherits **Main** | re-scope as `Approval` |
  | auto-compression inside the turn (`compress.rs` ~L948 via the agent) | inherits **Main** | re-scope as `Compress` |
  | delegate children (`delegate.rs`, run inline in the tool call) | inherits **Main** | re-scope as `Delegate` |
  | review, `lib.rs` ~L2194 `tokio::spawn(review::run(…))` (from `after_turn`, inside the turn task) | **lost** | `tokio::spawn(scope(ledger.context(Review, …), …))`, not `spawn_in_context` (that would label it Main) |
  | manual `/compress`, `lib.rs` ~L1316 `tokio::spawn` (from the gateway) | none | scope `Compress` |
  | `/bg`, `lib.rs` ~L1747 `tokio::spawn(delegator.background…)` | none | scope `Delegate` |
  | `/btw` side question, `lib.rs` ~L1832 `.complete` | none | scope `Btw` |
  | titles, `desktop/src/gateway.rs` ~L3074 `generate_title_with_effort` | none | scope `Title` |
  | curator, `desktop/src/maintenance.rs` `run_curator` / `schedule_curator` | none | scope `Curator` |
  The relay/forward/drain spawns (`lib.rs` ~L1126, `delegate.rs` ~L542, `review.rs` ~L182,
  `curator.rs` ~L188) and MCP connects make no model requests and need no context.
  So `spawn_in_context` is the right tool only where the child should keep the parent's purpose;
  every spawn listed above wants a *new* purpose, so `scope(ledger.context(P, session), fut)`.

**Fixed**
1. `observer::begin` observed POSTs from clients without a provider id (Ollama `api/show` in model
   discovery). Inside a context they were counted under provider `""`; outside one they used up the
   single warn, which would hide the real unlabelled callers. Now only labelled clients are
   observed, and each uncounted labelled request also logs at `debug`. New unit test.
2. Ledger: a failed `save` cleared `dirty`, so the counts waited for the next request to be
   written (and were lost at shutdown). `save` now returns `bool`, and a failed write stays dirty
   so the next interval tries again. New test (a folder blocks the temp file, then is removed).
3. Ledger: `load` treated *any* read error as "no file", so a locked or unreadable
   `requests.json` was overwritten with fresh counts 2 s later. Now only `NotFound` starts empty;
   other errors set the file aside as `.bad`, like a parse error. New test.
4. `Inner::drop` doesn't join when it runs on the ledger thread itself. Today the thread holds no
   `Ledger`, so this can't happen; the guard stops a future change from turning it into a self-join
   deadlock. Documented the join (it blocks the dropping thread for at most one write, once, at
   exit) and that a panicked thread's counts are simply lost.
5. The note's "For the reviewer" said the approval judge, compression and delegate children were
   uncounted. In fact the in-turn ones are counted as `main` (corrected above).
6. `PORTING.md`: added the `ledger.rs` (+ `observer.rs`) row, marked **diverged**. The module doc
   already said the row existed. (Rule 2 asks for a row for every new module.)

**Checked, fine**: the ledger thread owns no `Ledger` clone, so the observer's clone inside a
`RequestContext` only delays the shutdown write until the last context drops. `Shutdown` is the
last message queued, so pending counts are applied before the final write. On Windows,
`std::fs::rename` replaces an existing file (`MoveFileExW` with `REPLACE_EXISTING`). Day keys are
`YYYY-MM-DD`, so comparing them as strings orders them correctly; pruning keeps 31 days including
today, relative to each record's own time. The `sessions.db` schema is not touched (side file only).

**Left, and why**
- **Data loss on exit (CP2.3, hermes-desktop)**: tauri 2.11's `Builder::run` never returns
  (`std::process::exit`), so the `Runtime` and its `Ledger` are never dropped and the "final write
  on drop" never happens. Counts from the last ≤2 s before the window closes are lost. The fix is
  in `crates/desktop/src/main.rs`: `.build(ctx)?.run(|_, e| if let RunEvent::Exit = e {
  runtime.ledger().flush_blocking() })` (it needs the runtime handle there). I didn't do it here
  because hermes-desktop can't build in this worktree. The same bug means `terminals.close_all()`
  and `drop(tokio)` after `.run()` never run; that predates this branch. Check `hermes-acp.exe`'s
  exit path at the same time.
- Connection failures (refused, DNS) count as requests even though they never reached the
  provider, and the retry layer multiplies them. This is conservative, and a cancelled in-flight
  request must still count. If the governor needs precision, CP2.2 can discount
  `Outcome::TransportError`.
- The `turns` ledger test can fail if a run crosses 00:00 UTC (it reads `today(now)` after the
  turn). That is rare, so I left it.
- `RequestMeta` has no attempt number (implementer's deviation): accepted, since each attempt is
  its own observation.

**Tests** (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_llm -p hermes_runtime`: llm lib
466 passed (7 ignored), stream 1, request_observer 3; runtime lib 36 (10 ledger); turns 79 passed,
7 ignored. `cargo build --workspace --exclude hermes-desktop` OK.
