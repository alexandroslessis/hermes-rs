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

## CP2.2 — Every caller's purpose, governor, `requests.status`
Worktree: C:\Users\alexandros\repos\hermes-rs-p2   Branch: backend/p2-ledger   Model: Sonnet 5.5
Built on CP2.1 as reviewed and committed (97de1fb).
Done:
- **Every caller labelled** (the reviewer's table): inside a turn the context is *re-purposed*
  (`observer::with_purpose`: approval judge -> `approval`, compression summaries -> `compress`,
  delegate children -> `delegate`); work started outside a turn gets its own context from the
  ledger (`Ledger::context` / `Ledger::scoped`: review, manual `/compress`, `/bg`, `/btw`, titles,
  curator). New in `hermes_llm::observer`: `with_purpose`, `spawn_with_purpose`, `current_purpose`.
- **Purpose header**: `x-hermes-rs-purpose: <purpose>` on POSTs to loopback hosts only
  (`127.0.0.1`, `localhost`, `[::1]`), set inside a context; never sent to a real provider. The
  `turns.rs` mock already counted by it (Phase 0), so S4 is now 10 `main` + 1 `review` and S5 is
  2 `main` + 2 `delegate`.
- **Quota state in the ledger**: `ProviderDays.daily_limit` (from Settings) and `learned_limit`
  (from a 429); `Ledger::set_limit`, `learn_limit`, `usage()` (sync, read from a shared
  `RwLock<Snapshot>` that only the ledger thread writes; the request path still takes no lock),
  `Usage {used, limit, resets_at, by_purpose, rate_limited}`. `ledger::daily_limit_in` reads
  "N of N" from a 429 message; a daily-limit 429 that ends a main turn teaches the ledger.
- **`crates/runtime/src/governor.rs`**: `decide(usage, purpose, provider) -> Allow|Warn|Deny`, table
  in the module doc (under 20% left: review/curator/title wait; under 5%: also delegate and the
  approval judge, main is warned; nothing left: everything refused with the reset time).
  `Runtime::governor()`.
- **Where it applies**: `begin_turn` (refused only at 0; a `RuntimeEvent::Notice` when under 5%),
  `background` (/bg: only at 0), review start (deferred when low; `/refine` counts as the user's own
  work), the curator (skipped with a summary line), titles (desktop), approval judge (escalates to
  the user), `delegate_task` (error telling the model to do the work itself).
- **Desktop**: `requests.status` gateway method (`{provider, used, limit, resets_at, by_purpose,
  rate_limited}`, active provider by default); settings `requests.limits.<provider id>`:
  `daily_limit`, `reset_offset_minutes` read in `apply_settings`; `RuntimeEvent::Notice` becomes a
  `status.update` `{kind: "quota", text}`; the GPUI app ignores it.
Files: crates/llm/src/{observer,api_client}.rs, crates/llm/tests/request_observer.rs,
  crates/runtime/src/{ledger,governor,lib,compress,curator,delegate}.rs, crates/runtime/tests/turns.rs,
  crates/desktop/src/gateway.rs, crates/app/src/chat.rs
Tests (CARGO_TARGET_DIR=target/claude): `hermes_llm` 467 + 4 observer; `hermes_runtime` lib (14
  ledger, 7 governor ...), `turns` 85 passed / 7 ignored, `hermes-desktop` 158 + acp 67; `cargo build
  --workspace` OK (this worktree has a `target/hermes-src` junction to the main checkout's webapp,
  gitignored, so the desktop crate builds). New `turns` scenarios: used-up quota refuses the turn
  before any request (and `/bg`); nearly used up warns but runs; review waits at 15% left (main 10,
  review 0); delegation refused at 4% (main 2, delegate 0); `/btw` and `/compress` labelled and the
  ledger agrees with the mock; a daily-limit 429 teaches the limit. Governor table tests incl. the
  exact 20% / 5% edges and "main is only refused at zero" over every count 0..800.
Request counts: baselines now split by purpose (S4: main 10 + review 1; S5: main 2 + delegate 2).
Deviations from the plan:
- Day boundary stays a fixed UTC offset (`reset_offset_minutes`), not a named `reset_tz`: no tz
  database dependency; UTC and fixed-offset providers behave exactly as planned.
- Quota config is a config-record key (`requests.limits.<id>`), not a field of the provider
  settings struct (which the page's Providers screen owns); the Settings field the user checklist
  mentions needs a page patch and comes with CP2.3's display.
- Clearing a limit means setting `daily_limit` to null/0; deleting the whole `requests.limits`
  entry doesn't clear it until restart (the ledger keeps what it was given).
- The 429 limit is learned only from a *main* turn's final error message (the one place the body
  text reaches us); a 429 in a review or a title doesn't teach it.
- Not done from the plan list: the per-turn request count in the transcript metadata and the ACP
  usage update ("if the shape allows") are left for CP2.3 with the display; `x-hermes-rs-purpose`
  and the quota are in place for them.
For the reviewer:
- **No caller left unlabelled** is the focus: I re-grepped every `.complete(`, `response_post` and
  `tokio::spawn` that makes model requests (list in the CP2.1 review table). Please re-check
  `Runtime::refine` (it reuses `ReviewSetup::start`), the curator's own `SessionTools` agent loop
  (runs inside `ledger.scoped(Curator)`) and `delegate.rs`'s children (inside `with_purpose`).
  Anything missing shows up as an uncounted request and one `warn`.
- **The governor can't block the main turn except at 0**: `decide` is pure and tested over every
  count; `begin_turn` is the only place that refuses (before any state is taken: no lock, no
  message). `retry_turn` goes through it too.
- A review/curator/title denied by the governor is skipped, not queued: it is simply asked again the
  next time its trigger fires.
- `Ledger::usage()` takes a read lock on the shared snapshot while the thread takes the write lock
  per message and holds a read lock during a file write; contention is one writer thread, no
  deadlock path (it never waits for a reader).
- The `status.update` kind `quota` is not one the page knows; I did not check how the page treats an
  unknown kind (the Hermes frontend ignores unknown ones as far as I saw). Needs a look in CP2.3.

### Review (Opus 5.5)
`main` had not moved (still 5a9d388 under 97de1fb), so nothing was merged. Ran `/code-review high`
and checked the review focus by reading the code.

**Review focus**
- *No caller left unlabelled*: grepped `.complete(`, `.stream(`, `response_post`, `api_post`,
  `tokio::spawn`, `generate_title` in runtime, desktop, agent, llm and app. Every model request
  runs in a context with the right purpose: main turn (`Main`), approval judge and auto-compression
  inside the turn (`with_purpose` → `Approval` / `Compress`), delegate children (run inline in the
  tool call under `with_purpose(Delegate)`; `delegate.rs`'s relay spawn makes no request), review
  and `/refine` (`ReviewSetup::start` → `scope(Review)`), manual `/compress`, `/bg` (`Delegate`),
  `/btw`, desktop titles, the curator (its agent loop runs inline in `ledger.scoped(Curator)`; the
  drain spawn makes no request). ACP goes through the `Gateway`, so it inherits all of these.
  `smart_review` has no callers (its guard has `governor: None`; a future caller must scope it).
  `hermes_agent::CompactingProvider` / `compaction::summarize` are not used outside their tests.
  Discovery's `api/show` POST is unlabelled by design (CP2.1 fix 1). **One miss, fixed**: the GPUI
  app's titles (`crates/app/src/workspace.rs`).
- *Governor blocks the main turn only at 0*: `decide` returns `Deny` for `Main` only when
  `left == 0` (tested over 0..800). `begin_turn` (used by `start_turn`, `start_turn_with_call`,
  `start_turn_with_images`, `retry_turn`) checks after `open()` and before the running guard, the
  turn lock, `turn_state`, or any message, so a refusal leaves nothing behind. `/bg` checks before
  `open()`. In the gateway a refusal happens after the existing pre-start steps (first-line title,
  `pending_images` taken, rewind on edit), the same as for any other start error such as "already
  running"; a refused queued prompt surfaces as `message.complete` with the error, as before.
- *Ledger lock*: only the ledger thread writes the `RwLock<Snapshot>`, per message and briefly;
  it holds a read lock during the file write. Readers (`usage()`: governor, `requests.status`) never
  hold it across anything else, and nothing calls `usage()` on the ledger thread. The request path
  (`observer` → channel send) takes no lock. No deadlock path.
- *`status.update` kind `quota`*: the page's handler (`gateway-event/status.ts`) acts on
  `compacting`/`compressing`/`compacted`/`ready`/`process`/`goal` and fallback text (a regex on
  "model fallback|provider fallback|switch(ed|ing) to fallback", which our text can't match), and
  returns `true` for everything else: an unknown kind is silently ignored. Harmless; CP2.3 shows it.
- `sessions.db` schema: untouched (all new state is in `requests.json`).

**Fixed**
1. A daily limit from Settings was written to `requests.json`, and `apply_settings` only touched
   providers still listed, so removing a `requests.limits` entry (or the whole key) left the old
   limit in force for good, across restarts. Now `daily_limit` is `#[serde(skip)]` (Settings is
   its only source) and `apply_settings` calls the new `Ledger::configure(Vec<ProviderLimit>)`,
   which replaces the configured limits as a whole: unlisted providers go back to no limit and a
   UTC day. New unit test. (The note's deviation "doesn't clear it until restart" is now void.)
2. `learn_limit` kept the first learned value for ever and learned nothing while a Settings limit
   was set, so a provider that raised its quota stayed governed by the old number. Now the latest
   "N of N" wins (a configured limit still takes precedence while set). Test updated.
3. `daily_limit_in` split numbers at commas: "1,000 of 1,000" was not recognised. Thousands
   separators are dropped first. Test case added.
4. `apply_settings` cast `daily_limit` with `as u32` (wraps); now `u32::try_from`, and an offset
   beyond ±24 h is treated as absent.
5. GPUI app titles were sent outside any context (uncounted, and the one `warn` spent on them).
   Now `ledger.scoped(Title, …)` and skipped when the governor denies a title, like the desktop.
6. `PORTING.md`: added the `governor.rs` row (**diverged**), per rule 2.

**Left, and why**
- `/review` starts its turn with a scripted `delegate_task` call, which the governor refuses below
  5% left like any delegation; the model then reviews on its own. The user asked for it, so one
  could argue it should count as `Main` (as `/refine` does), but the tool can't tell a scripted
  prelude from the model's own call without new plumbing, and below 5% saving requests is the
  point. Revisit in Phase 3 (budget profiles) if it bites.
- A limit learned from a 429 is learned only from a *main* turn's final error (implementer's
  deviation): accepted.
- The governor reads the ledger's shared snapshot, which may lag the last few requests by a
  moment, so at the boundary one extra request can slip through. Accepted (the provider's own 429
  is the hard stop).
- The gateway's refused-start path drops `pending_images` and may already have set the first-line
  title; that predates this checkpoint and applies to every start error.

**Tests** (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_llm -p hermes_runtime -p
hermes-desktop`: llm lib 467 (7 ignored), stream 1, request_observer 4; runtime lib 47; turns 85
passed, 7 ignored; desktop 158 + acp 67. `cargo build --workspace` OK, no warnings.

## CP2.3 — Page display, exit flush, settings field, live check, docs
Worktree: C:\Users\alexandros\repos\hermes-rs-p2   Branch: backend/p2-ledger   Model: Sonnet 5.5
Built on CP2.2 as reviewed and committed (b23d48a). This is the **last checkpoint of Phase 2**.
Done:
- **Page display, no patch needed**: the page's status bar already has an allowance gauge
  ("plan usage", fed by `plan_gauge: {id, label, used_bp, resets_at_ms}` in the `session.usage`
  payload; the contract has it). `usage_snapshot` now adds it when the session's provider has a
  limit: `{id: "requests", label: "Requests 312/800", used_bp, resets_at_ms}`, which the page draws
  as "Requests 312/800 39%" (red from 80%, amber from 50%) with "used, resets in Xh" in the
  tooltip. An unknown gauge id shows our label as is. (There is no field for a bare "312/800", this
  is the existing one that comes closest.)
- **Exit flush**: `main.rs` builds the Tauri app and runs it with a callback that calls
  `Ledger::flush_blocking` on `RunEvent::Exit` (the reviewer's CP2.1 finding: `Builder::run` never
  returns); `hermes-acp.exe` flushes when stdin closes (`acp/mod.rs`).
- **Daily limit in Settings**: `ProviderSettings.daily_request_limit` (`config.json`), served and
  saved by the custom-endpoint routes (`endpoints.rs`; only when the page sent the key, so an older
  page can't wipe it; null/0/nonsense clears), merged with `requests.limits.*` in
  `Gateway::apply_settings`, which now also runs after an endpoint is saved or deleted. The page
  field is **patch 0021** (`custom-endpoints-settings.tsx`, `types/hermes.ts`).
- **Live check** `scripts/perf/requests-check.mjs` (all ok, twice in a row, on a perf instance built
  from this worktree; stopped afterwards): `requests.status` matches the mock's own counts in total
  and per purpose (2 main + 1 title for two turns, nothing unlabelled); a limit set through
  `config.set` shows in the status and as the gauge in `session.usage`; used up, the turn is refused
  ("used up (5 of 5). It resets at ...") with zero requests reaching the mock; cleared, turns run.
- Docs: ARCHITECTURE (llm observer, `requests.json`, request accounting paragraph), PITFALLS (a
  Phase 2 section), perf README, patches README (0021), GAPS (Phase 2 in the backend v2 row),
  PHASES Baselines (split by purpose).
Files: crates/desktop/src/{gateway,endpoints,providers,main,acp/mod}.rs,
  crates/desktop/patches/0021-daily-request-limit.patch, scripts/perf/requests-check.mjs, docs
Tests (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_llm -p hermes_runtime -p
  hermes-desktop` all green (desktop lib 159 with the new endpoint test, acp 67; runtime turns 85);
  `cargo build --workspace` OK; live check all ok.
Request counts: unchanged.
Deviations from the plan:
- **No per-turn request count in the transcript metadata**: the page's `session.usage.calls` already
  counts model calls per session from the store, and the ledger has the per-purpose day counts;
  adding a side file per session would duplicate them.
- **ACP shows no quota**: `usage_update` is `{used, size}` in tokens (the shape doesn't allow it).
- **Patch 0021 is not built or tried in the page**: I validated it with `git apply --check` against
  the pinned source only. A webapp build needs npm packages (not present beside the shared
  `target/hermes-src`, which is a junction to the main checkout's build, so I didn't rebuild it).
  Until it is built, the limit is set through the config record (`requests.limits.<id>`) or learned
  from a 429. The user checklist's "Settings field" needs that rebuild.
- Config-record limits can only be cleared by setting them to `null` (the record merges).
For the reviewer:
- Patch 0021 is the thing to look at: minimal (two files, ~20 lines), English literals instead of
  i18n keys (adding keys means editing four locale files), and unverified in the page. Decide
  whether to keep it, or to rebuild the webapp in your own session and look at the form.
- The gauge's `id: "requests"` is not one of the page's known ids (day/week/month/top_tier), so
  the page shows our label verbatim; its click target is `?tab=billing`, which we hide.
- `apply_settings` runs after every endpoint change now; check it is cheap (reads `config.json`).
- Phase 2 is complete with this checkpoint: docs per rule 10 are written, but the phase-map
  tick (and the merge into main, which needs the user's go-ahead) is the reviewer's step.

### Review (Opus 5.5)
`main` had not moved (still 5a9d388, an ancestor of this branch), so nothing was merged. Ran
`/code-review high` and checked the review focus by reading the code against the pinned page.

**Review focus**
- *Patch 0021*: correct and close to minimal. Exported Hermes at `HERMES_COMMIT` into a scratch
  folder and applied 0001–0021 in name order with `git apply` (as `build-hermes-webapp.ps1` does):
  all apply; only 0012 also touches `custom-endpoints-settings.tsx`, in other hunks. The field's
  value is typed by `CustomEndpointUpdate` (the patch extends it), `toPayload` always sends the key
  (null when empty, so clearing works), and `validateCustomEndpoint` gets it too (ignored by
  `validate`). English literals instead of i18n keys: accepted (four locale files otherwise).
  **Not built and not seen in the page**: safe to ship unbuilt, because the backend works without
  it (an unpatched page never sends the key, which `save` leaves alone) and the build script fails
  loudly if a patch doesn't apply; the risk left is a syntax, type or layout slip in ~20 lines
  read here against the pinned source, which the user's next `build-hermes-webapp.ps1` (a syntax
  error stops it; a pure type error may not, if the build doesn't type-check) and the checklist
  below catch.
- *`plan_gauge`*: matches `PlanGauge {id, label, used_bp, resets_at_ms?}`. `gaugeTitle` shows the
  server's `label` for an id it doesn't know ("requests"), the status bar adds the percent;
  `used_bp` is capped at 10 000 (and the page clamps too); `resets_at_ms` is `resets_at * 1000`
  (seconds → ms). `usage_snapshot` always sends `calls`, so `mergeUsage` deletes a stale gauge once
  a provider has no limit. Live: `{"id":"requests","label":"Requests 25/124","used_bp":2016,
  "resets_at_ms":1794528000000}`.
- *Exit flush*: `Builder::run(ctx)` in Tauri 2 is `self.build(ctx)?.run(|_, _| {})`, so
  `.build(ctx)?.run(callback)` keeps everything (plugins, `setup`, the window) and only adds the
  callback. Tested: set a limit on my instance and closed its window 0 s later (the ledger writes
  2 s after a change): `requests.json` was written 36 ms after the close, with the new offset. ACP:
  `flush_blocking` runs after `serve` returns (stdin closed), before the process exits; a blocking
  wait at the end of `block_on`'s own future can't stall other work. A hard kill still loses up
  to 2 s (PITFALLS says so).
- *`apply_settings`*: one more read of `config.json`, the same as the other config reads it already
  makes (a few small files, synchronous like the endpoint routes around it); runs on settings
  changes only, not per turn. No `config.json` → no endpoint limits, `requests.limits` alone.

**Fixed**
1. The limit field parsed "1,000" as 1 (`parseInt` stops at the comma): one request a day, then
   every turn refused. The patch now drops spaces, commas, dots, apostrophes and underscores first.
2. The gauge's click went to Settings ▸ Billing, which `hide.css` hides (an empty page). Patch 0021
   now sends our gauge (`id: "requests"`) to Settings ▸ Providers, where the limit is set (one line
   in `use-statusbar-items.tsx`; the patch is regenerated and re-checked against 0001–0020).
3. A hand-edited `daily_request_limit` that isn't a positive u32 ("800", -1, 5e9) failed the whole
   `config.json` parse, and `Settings::load` then fell back to *no providers at all*. It is now read
   leniently (anything else = unset). Test.
4. The merge of endpoint limits and `requests.limits` is now `gateway::provider_limits`, a pure
   function with tests (precedence, offsets, null in the record, no providers); the comment that
   said 0 in the record means "no limit" was wrong after the merge (it means "the record sets
   none") and is corrected; PITFALLS states the precedence.
5. The live check now also saves a limit through the custom-endpoint route (what the Settings
   field does) and checks the ledger takes it at once and that saving it empty clears it.
6. Docs: patches README (third file, separators, the click target), PITFALLS (precedence, click
   target, gauge refreshes on the next turn), perf README, phase map ✅.

**Left, and why**
- The gauge is sent with `session.usage` / `message.complete` only, so after a limit change it
  updates on the next turn. Pushing it to every open session on a settings change needs a list of
  open sessions per window; not worth it for a number that changes per turn anyway (documented).
- `requests.limits` wins over the endpoint's field when both are set: the record is the
  power-user path and the live check relies on it. Documented in PITFALLS.
- `perf.ps1 -Do start` piped or redirected (`| Select-Object`, `*> file`) waits until the app
  exits (the app inherits the pipe); run it bare. And from a worktree without `tools/node`, its
  page-timing step errors harmlessly (the instance still starts).

**Tests** (CARGO_TARGET_DIR=target/claude): `cargo test -p hermes_llm -p hermes_runtime -p
hermes-desktop`: llm 467 (7 ignored) + 1 + 4; runtime lib 47, turns 85 (7 ignored); desktop 162
(3 new) + acp 67. `cargo build --workspace` OK, no warnings. `requests-check.mjs`: all ok twice
in a row on my perf instance (built from this worktree), which was then stopped with its limit
cleared.

**For the user, after the next `build-hermes-webapp.ps1` and a build**
1. Settings ▸ Providers ▸ edit your endpoint: a "Daily request limit" field sits under the model
   and context window. Type `800`, save; reopen: it shows 800.
2. Send a message: the status bar shows "Requests N/800 x%"; hover it for "resets in …".
3. Click it: Settings ▸ Providers opens (not an empty page).
4. Empty the field, save, send a message: the gauge is gone.
