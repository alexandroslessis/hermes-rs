# Phase 1 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-n1   Branch: native/p1-core

## CP1.1 — Core, sessions, turns, events, questions
Worktree: C:\Users\alexandros\repos\hermes-rs-n1   Branch: native/p1-core   Model: Opus 5.5 (subagent)
Cut from `main` at 83e7e87.

### Verdict

`crates/core` (`hermes_core`) exists and the Tauri gateway and the ACP agent run on it: every
front end builds a `Core` (`Core::new(config, handle)`), the gateway is `Gateway::new(core)`. The
questions (the runtime's `UserBridge`), the event stream, the side files and the session logic
(list, create, rename, delete, archive and the idle sweep, branch, move to a folder, stop, rewind,
projects) are in core, typed; `crates/desktop` keeps the page's JSON. A pure refactor for the page
and ACP: the RPC table fixture (every method's answer) is unchanged, the ACP suite and the live
checks pass. **Not moved in this checkpoint** (recorded below, "What stayed and why"): the turn's
event loop with the queue behind a running turn (`gateway/turn.rs` `submit`), and `/` command
dispatch (with the command catalog, CP1.2).

### Done

1. **`Core`** (`crates/core/src/lib.rs`): `{ runtime, handle, data_dir, events, questions,
   branches, workspaces, archive, branch_keys }`, built from `RuntimeConfig` + a tokio `Handle`.
   `Core::new` builds the runtime with core's own bridge, so nothing outside core implements
   `UserBridge` any more. `spawn` for a front end on another executor (GPUI). What `Gateway` owned
   that isn't protocol moved: the hub's pending questions, `branches.json`,
   `session-workspaces.json`, `archived-sessions.json`, the branch idempotency keys. Still in
   `Gateway` (page or later-checkpoint state): `starts` (admission receipts), `context_lengths`,
   `generation` (usage, CP1.3), `file_tickets` (CP1.2), `viewing` (keyed by the page's socket),
   `actions` (curator, CP1.3), `rest` (settings, CP1.3).
2. **Events** (`events.rs`): `CoreEvent` — `TextDelta`, `ReasoningDelta`, `ToolStarted`,
   `ToolProgress`, `ToolCompleted`, `Status` (compression, fallback, quota, taint lines), `Usage`,
   `TurnEnded`, `TitleChanged`, `ApprovalRequested`, `ClarifyRequested`, `Settled`,
   `ProcessFinished`, `ReviewFinished`. `Core::subscribe()` gives an `UnboundedReceiver<CoreEvent>`
   (the native UI's way). A second way, `Core::listen(Arc<dyn Listener>)`, calls a listener in the
   publisher's thread and in order; `publish` returns whether a listener *showed* the event. The
   gateway's `Hub` is a listener: it turns each event into Hermes's frame on the socket that
   follows the session, synchronously, so the core's frames keep their order with the ones the
   gateway still sends itself (see "For the reviewer" 1). `stream.rs` (`TurnStream`) builds the
   stream events from `AgentEvent`s (it was `transcript.rs` `TurnStream` + the gateway's status
   arms + the compaction texts).
   Curator results: no `CoreEvent` yet — the curator runs from `maintenance.rs` (CP1.3) and
   reports through `Actions`, not the bridge; it gets an event when it moves.
3. **Questions** (`questions.rs`): `Questions` (pending by prompt id) and `CoreBridge` (the
   `UserBridge`: clarify one question at a time, approvals with the four choices or once/deny,
   process and review notices published as events). Answers: `Core::answer_approval(prompt_id,
   ApprovalChoice)`, `Core::answer_clarify(prompt_id, text)`, and `Core::answer(prompt_id, text)`
   which the gateway uses (the page's `approval.respond` / `clarify.respond` answer any open prompt
   by id with the text, as before). `Core::open_questions`, `is_waiting`, `stop` (cancel + drop the
   session's questions). The hub builds `approval.request` / `clarify.request` / `*.settled` and
   `session.resume`'s `prompts` from them (`hub::question_payload`, same fields, same order).
4. **Sessions** (`sessions.rs`, `projects.rs`, `transcript.rs`):
   - list: `Core::sessions(archive::Filter) -> Vec<SessionSummary>` (record, message count,
     running, live, waiting, archived, git identity, parent); `live_sessions`; `summary`. The
     gateway's `session_row` / `session_rows` / `session_list` / `session.active_list` read them.
   - projects: `hermes_core::projects::tree` is the grouping, typed and generic over a `TreeRow`
     (the gateway feeds it its JSON rows and renders `projects.tree`); `Core::projects`,
     `create_project`, `update_project` (`ProjectEdit`), `delete_project`.
   - create (`NewSession`: provider, model, cwd, title, seeded messages, parent), rename (publishes
     `TitleChanged`), delete (`delete_session`, `forget_archived`), archive (`set_archived`,
     `auto_archive(days)`, `reopened`), branch (`BranchRequest` + `BranchPoint::{Whole,
     ThroughRow, ThroughEntry}`, `BranchError<E>`; the provider is a closure so its error stays the
     gateway's and keeps its place in the order of checks), move to a folder (`move_session`:
     busy check, store + git identity on a blocking thread, evict), `rewind_to_row` (`session.rewind`).
   - transcript: `entries` (typed `Entry::{Tool, Message}` with row ids and the failed-turn
     boundary), `message_error`, `error_surface` (typed `ErrorSurface`), `turn_error_text`,
     `persisted_turn` (typed), `typed_prompts`, `user_turn_index`, `row_id`, `kept_before_row`.
     `crates/desktop/src/transcript.rs` is now only the `TranscriptMessage` / `error_surface` /
     `persisted_turn` JSON.
   - `branch.rs`, `archive.rs`, `workspace.rs` moved whole (`git mv`); `workspace::move_session`
     now takes a resolved `&Path` (resolving the page's `~` / `file:` spellings stays in the
     gateway, `files::resolve`). `git.rs` in core holds `git` / `git_line` / `CREATE_NO_WINDOW`
     (desktop's `git.rs` re-exports them). `commands.rs` in core holds `typed_command` and
     `review_instructions` (the transcript and submit need them; desktop's `commands.rs` re-exports).
   - Desktop keeps `pub use hermes_core::{archive, branch, workspace}` in `lib.rs` so the
     modules that name `crate::archive::...` compile unchanged.
5. **Cross-app lock** (`lock.rs`): `AppLock::acquire(data_dir)` holds `<data>/app.lock` open with
   no sharing (the `turnlock` pattern: the OS closes it when the process dies, so a crash never
   locks the folder). `hermes-desktop` takes it in `main` before the window; a second desktop on
   the same data folder shows an info dialog "Hermes is already open. Switch to its window, or
   close it before starting Hermes again." and exits (code 1). The ACP agent does not take it (it
   runs beside the desktop app today, sharing through SQLite WAL and the per-thread `TurnLock`,
   unchanged). The native app does not take it yet (see "For the reviewer" 4).

Docs: `PORTING.md` (an *Application layer* block with every core module; the desktop rows for
`hub.rs`, `transcript.rs`, `projects.rs` say what shape they keep; `archive.rs`, `branch.rs`,
`workspace.rs` rows moved under core), `AGENTS.md` (`crates/core/` row, `hermes_core` in the
package names), `docs/ARCHITECTURE.md` (crate row, startup step 2 and 4, events, approvals row,
feature map rows, data folder rows incl. `app.lock`), `docs/PITFALLS.md` (a `hermes_core` section;
the "second instance" line).

### Files

New: `crates/core/{Cargo.toml, src/lib.rs, src/events.rs, src/questions.rs, src/sessions.rs,
src/sessions/tests.rs, src/stream.rs, src/transcript.rs, src/projects.rs, src/commands.rs,
src/lock.rs, src/git.rs}`. Moved: `crates/desktop/src/{branch,archive,workspace}.rs` →
`crates/core/src/`. Changed: `Cargo.toml` (workspace dep), `Cargo.lock`, `crates/desktop/Cargo.toml`,
`crates/desktop/src/{lib.rs, main.rs, hub.rs, transcript.rs, projects.rs, git.rs, commands.rs,
acp/mod.rs, gateway/{mod.rs, sessions.rs, turn.rs, projects.rs, rest.rs, tests.rs,
rpc_table_tests.rs}}`, the docs above, this note.

`crates/desktop/src` line counts (whole tree 23,237 → 21,613): `branch.rs` 134 → 0, `archive.rs`
280 → 0, `workspace.rs` 322 → 0, `transcript.rs` 651 → 241, `projects.rs` 358 → 172, `hub.rs` 339
→ 325 (the bridge and questions left; the event-to-frame translation and its tests came in),
`git.rs` 256 → 226, `commands.rs` 314 → 298, `gateway/sessions.rs` 1,127 → 1,019, `gateway/turn.rs`
929 → 874, `gateway/projects.rs` 139 → 109, `gateway/mod.rs` 473 → 462, `gateway/rest.rs` 807 →
795. `crates/core/src`: 3,827 lines (about 1,300 of them tests).

### Tests

All with `CARGO_TARGET_DIR=target/claude` (the worktree's).

- `cargo test -p hermes_core`: 45 passed. New tests: `events` (order, shown-means-shown, a
  dropped subscriber leaves), `questions` (an approval asked and answered, Manual/Plan's once/deny
  and a stopped turn denying, a multi-select answer split, nobody to ask denies and skips, choice
  round trip; these replace `hub.rs`'s two bridge tests), `sessions/tests.rs` over a real runtime
  on a temp folder (create + list with parent, rename trimmed and announced, archive unpins and
  delete forgets, branch through a row / an entry / retried by key / empty / unknown / provider
  error, rewind to a row, move to a missing and a real folder, the idle sweep once a day and
  `reopened`, project create/update/delete), `lock` (second app refused until the first exits,
  another folder not held back, a crash's leftover file doesn't hold), `stream` (text, think tags,
  tool start/complete names, compression lines), `transcript` (the moved reading tests, typed),
  `projects` (the two moved grouping tests, typed, plus previews and last activity), `commands`,
  and the moved `branch`, `archive`, `workspace` tests.
- `cargo test -p hermes-desktop`: lib 176 passed (incl. `gateway::rpc_table_tests`
  `every_method_answers_as_the_table_says` against the **unchanged** `tests/fixtures/rpc_table.txt`,
  and the new `hub` tests: the canonical approval card's fields in order on the following socket,
  not shown without a follower, a JSON tool result read as its value; `transcript` and
  `projects` keep shape tests), `tests/acp.rs` 68 passed.
- `cargo test -p hermes_runtime --test turns`: 258 passed, 7 ignored (unchanged).
- `cargo build --workspace`: passes (the frozen GPUI app included).
- `cargo clippy -p hermes_core --all-targets --no-deps`: clean. `hermes-desktop` has only warnings
  that were there before (none on lines this checkpoint wrote); `hermes_tools`' known
  redundant-clone errors stop a clippy run with deps, hence `--no-deps`.
- `cargo fmt -p hermes_core -- --check`: clean. `hermes-desktop`: the files this checkpoint wrote
  or changed are clean; `cargo fmt -p hermes-desktop -- --check` still lists files untouched here
  (`acp/tools.rs`, `aux_models.rs`, `endpoints.rs`, `learning.rs`, `maintenance.rs`, `rest.rs`,
  `gateway/slash.rs`, `tests/acp.rs`): not reformatted (a formatting-only change is its own commit).
- Live checks against my own instance (`perf.ps1`, port 47810, data `target/perf/home`, the debug
  exe from this worktree), each its own provider: `queue-check` (queue, steer, redirect, the
  approval and clarify cards answered) all passed; `stop-check` ok; `archive-check`,
  `provider-error-check`, `rpc-dispatch-check` (all 80 methods over the socket), `workspace-check`,
  `coding-commands-check` all passed; `tool-stream-check` ok; `side-and-branch-check` all passed
  on a fresh instance (a second run in the same instance fails 4 checks because the script reuses
  idempotency key `k1`, which the process remembers: the first run's branch comes back; the same
  before CP1.1, where the keys were the gateway's). `rollback-check` did not run (it needs an
  argument this run didn't give: `TypeError ... path ... undefined`); not touched by CP1.1
  (`rollback` stayed in the gateway). `artifacts-check`: 1 failure, "opening the file asks the
  backend": the page made two `/api/hermes-rs/open-file` calls, both 400 from
  `open_file::run` (opening the `.md` with its default app failed on this machine); `open_file.rs`
  and that route are untouched, the listing checks (over `stored_messages`, i.e. the new
  transcript shape) passed. Not counted as a regression; the reviewer may want it re-run on
  `main` to confirm.
- The lock's dialog was not exercised live (it would put a modal on the user's screen); unit
  tests cover the lock, the dialog is the user's checklist item.
- `scripts/clean-test-temp.ps1`: dry run 117 folders / 52.5 MB (the `turns` suite's known
  leftovers), then `-Delete` removed 126 (0 left); no `hermes_testdir: could not remove` line in
  the test output.

### User's checklist (desktop app built from this branch)

1. Start Hermes, open a chat, send a message: the reply streams, a tool call shows its card and
   result, the title appears.
2. Ask for something that needs approval (e.g. "delete the build folder"): the approval card
   shows; "Allow once" runs it. Stop a turn while a card is open: the card closes.
3. Sidebar: rename, archive (it leaves the list; Settings ▸ Sessions lists it), restore, delete.
4. Branch from a message (`/branch`): the branch is listed under its parent with "(branch)".
5. Projects view: sessions under their project / repository / Home as before; drag a session
   onto a project (move): it moves.
6. With Hermes open, start a second `hermes-desktop.exe` (same build): a dialog says "Hermes is
   already open…" and no second window opens. Zed's `hermes-acp.exe` still works beside it.

### What stayed in `crates/desktop`, and why

- **The turn's event loop and the queue** (`gateway/turn.rs` `submit`, `run_queued`, `enqueue*`,
  `dequeue`, `set_live`/`end_live`, `title_session`, the heartbeat). Too entangled to split cleanly
  in this checkpoint without a behaviour risk: the loop interleaves page-only frames built from
  gateway state (`message.start` with the admission id, the admission gate on the `prompt.submit`
  receipt, `session.info` with the queue panel's rows, `session.usage` snapshots and generation
  speed (CP1.3's `usage.rs`), `message.complete` with `persisted_turn` and the error card), and
  the queue's items are the page's raw `prompt.submit` params, which the runtime's session actor
  stores as `serde_json::Value` (`PageState::queue`). What moved from it: the stream translation
  (`stream.rs`, published as `CoreEvent`s the hub sends), `TurnEnded` and `Usage` events, titles
  (`announce_title`), stop (`Core::stop`), `reopened`, and the pieces of submit that are plain
  logic (`typed_command`, `review_instructions`, `kept_before_row`, `user_turn_index`,
  `message_error`, `turn_error_text`). The proposed next step: `Core::submit(SubmitTurn { text,
  images, truncate, ... }) -> Started | Queued` with a typed queue item (needs a typed
  `PageState::queue` in the runtime, or a wrapper that keeps the params beside the typed fields for
  the page), and the loop in core publishing `TurnStarted` / `TurnEnded` with the persisted rows;
  the gateway would keep only the admission gate and its `session.info` frames.
- **`/` command dispatch** (`gateway/slash.rs`, `commands.rs`): planned with the command catalog in
  CP1.2 (`commands.rs`, `gateway/slash.rs` minus the shapes). CP1.1's list names "`/` command
  dispatch"; the two plans overlap, and splitting `slash.rs` twice would churn it. Only the two
  pure helpers the transcript needs moved now.
- **Per-session page state reads** (`picked_effort`, `revision`/`bump_revision`, `pending_route`,
  `pick_effort`, `pending_rows`): one-line reads of the runtime's session actor (`PageState`),
  already typed there; moving them adds a pass-through. They go with the submit/queue move.
- **`viewing`** (the review's "switched away" signal): keyed by the page's socket (`Outbox`). The
  runtime calls it needs (`note_session_left` / `note_session_viewed`) are already typed.
- **`session_info`, `context_breakdown`, `status_text`, `compress_session`, `export_session`,
  `rollback`**: page shapes over runtime calls, and/or CP1.2/CP1.3 modules (export, usage,
  settings: `turn_options`, `provider_for`).

### Deviations from the plan

- Turn submission and the queue did not move (above). The plan's step 4 lists them.
- `/` command dispatch did not move (above; CP1.2).
- The native app does not take the lock yet: `crates/native` is still the Phase 0 spike on fake
  data and does not depend on `hermes_core` (adding it would link the whole runtime into the spike
  for a lock). Desktop takes it; native takes it when it first builds a `Core` (Phase 3).
- `hub.rs` stayed in desktop as the routing table plus the event-to-frame translation; the plan
  says "built from `RuntimeEvent` and `hub.rs`": the *questions* half of `hub.rs` is what moved.
- `Gateway::new` / `new_for_acp` take an `Arc<Core>` instead of `(runtime, hub, data_dir)`.

### For the reviewer

1. **Event order.** The page's frames now come from two places: the gateway's `emit` and the
   core's bus through the hub listener. Both run synchronously in the caller's thread, so the
   order within a session is the order of the calls, as before. A `subscribe()` channel would
   *not* keep that order relative to direct frames; that is why the hub is a `Listener`. Worth a
   look: `turn.rs`'s loop publishes `TurnEnded` just before `message.complete` (the hub ignores
   it), and `Usage` after `session.usage`.
2. **"Shown" means a listener showed it.** A question with no follower is answered "no answer" (an
   approval denied, a clarify `Undelivered`) only when *no* listener shows it. With the native app
   later subscribing through a channel, every question counts as shown while its receiver lives.
   Fine for one app per data folder (the lock), but a front end that subscribes must also answer
   or stop the turn.
3. **`review_finished` / `process_finished` now also publish events** (`ReviewFinished`,
   `ProcessFinished`); the hub ignores them, so the page sees nothing new. They were log lines only.
4. **The lock and the user's habits.** A second `hermes-desktop.exe` on the same data folder used
   to open on a random port with empty local settings; now it shows the dialog and exits (the plan
   asks for it). The perf instance (`HERMES_RS_HOME=target/perf/home`) and tests use other folders.
   The user's own installed app (an older build) does not take the lock, so the guard only works
   between two new builds. The message is plain text in an info dialog; Hermes has no wording for
   it to copy.
5. **`gateway/tests.rs` and `rpc_table_tests.rs`** now build a `Core` (`Core::new(config,
   Handle::current())`); the RPC table fixture was not re-blessed (it passes unchanged).
6. **`delete_session` errors:** `session.delete`'s archive-forget step now reports a stopped
   blocking thread as "The request stopped: the archive's thread stopped" (before: "The request
   stopped: <join error>"). Only reachable on a panic.
7. `Core::sessions` computes `live` and `waiting` per row (an actor status read and a scan of the
   open questions) that the page's rows don't use; cheap, but it is per row of up to 1,000.
8. Formatting: `cargo fmt -p hermes_core -p hermes-desktop -- --check` result is under Tests.

### Review (Opus 5.5)

Main had not moved since the branch was cut (83e7e87): no merge needed.

**Verdict: accepted, with the turn/queue/`/` dispatch move carried into CP1.2 as a named task.**
What is here is a faithful move: the bridge, the stream translation, the transcript reading, the
branch/rewind/archive/move logic and the projects grouping were compared with the code at 83e7e87
line by line (same checks, same order, same error texts except point 6 above); the hub listener
produces the same frames in the same order (the RPC-table fixture passes **unchanged**, not
re-blessed). Locks: the bus copies its listener list and delivers outside the lock; `Questions`
releases its map before publishing; the hub's `emit` holds its map only to clone an outbox; so a
listener that calls back into `Core` cannot deadlock. A dropped subscriber is pruned on the next
publish; subscribers are unbounded (no lag, but a front end that stops draining grows memory: the
native UI must drain). Lock: an exclusive non-inheritable handle, so a crash (or children the app
spawned) never holds it; a hung process still does, which is correct (it is still running). ACP
does not take it (`--acp` returns before the lock). Store schema untouched; no new blocking on
async workers (the moved calls run where they ran before; `move_session` and `forget_archived`
use `spawn_blocking`); prompt caching untouched; Hermes citations moved with the logic; PORTING rows
point at `crates/core`; tests use `hermes_testdir`, `TestDir` last in the sessions fixture.

**Found and fixed**
- *A question whose waiting future was dropped stayed open forever.* A turn cancelled without
  `Core::stop` (ACP's `session/close` and editor shutdown call `runtime.cancel` only; the agent
  loop's cancel `select!` drops the tool call) left the entry in the pending map: `is_waiting`
  stayed true (the native sidebar's "waiting" status) and `session.resume` re-showed a dead card.
  Pre-existing in `hub.rs`, but `SessionSummary.waiting` now exposes it. `Questions::ask` holds a
  guard that removes the entry and publishes `Settled` on drop (a no-op after an answer or a stop).
  Test: `questions::a_question_its_turn_stopped_waiting_for_closes`.
- Docs: PITFALLS (`hermes_core` section: no stream without the gateway's loop until CP1.2; the
  question guard), GAPS-PLAYBOOK's archive path, PHASES.md CP1.2 (below).

**Deviations.** Accepted as recorded, except that they are not a CP1.3-or-later detail: the
native UI's Phases 4–5 need `Core::submit` with the queue, cancel (present: `Core::stop`) and `/`
dispatch from Rust, and today the stream `CoreEvent`s are published **only** by
`gateway/turn.rs`'s loop, so a `Core` without a `Gateway` sees no turn at all. Moving the loop is
not a contained change (874 lines interleaving page frames, admission and the page's raw queue
params), so it is recorded as the first task of CP1.2 in PHASES.md ("Carried over from CP1.1",
with done criteria, implemented by Opus). Curator results (no event yet) stay with CP1.3, where
the curator moves. Native app taking the lock: Phase 3, as noted.

**Live-check oddities, resolved.** Built `main` (83e7e87) in a separate worktree and target dir
under `target\claude` (removed afterwards) and ran `artifacts-check` on fresh instances of both:
both pass on a fresh instance, and **both** fail the same check on a second run in the same
instance (the script wraps `window.fetch` again on a page it does not reload, so one click logs
two calls). Not this change. `side-and-branch-check` passes on a fresh instance with its default
temp folder; in a folder inside a git repository it fails "the side question saw the conversation"
(the repo context is attached to the user message), which is the script's assumption, not a
regression. `rollback-check` needs `<port> <empty folder>` and mock_llm.py: run that way, the
checkpoint lists, the diff shows the file, restore removes it.

**Re-run by the reviewer** (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes_core` 46
passed; `-p hermes-desktop` lib 176 and `tests/acp.rs` 68 passed; `cargo test -p hermes_runtime
--test turns` 258 passed, 7 ignored; `cargo build --workspace` passes.
Live, fresh instance (debug exe of this branch, port 47810, `target/perf`): `queue-check`
(queue, steer, redirect, approval and clarify cards), `stop-check`, `rollback-check`,
`archive-check`, `rpc-dispatch-check`, `workspace-check`, `provider-error-check`,
`tool-stream-check`, `coding-commands-check`, `side-and-branch-check`, `artifacts-check`: all
passed. Line counts in the note are right within two lines (`gateway/projects.rs` 107,
`gateway/rest.rs` 797).

**Left:** the lock's dialog is not exercised live (it would show a modal on the user's screen);
it is the user's checklist item 6.

## CP1.2 — Workspace features
Worktree: C:\Users\alexandros\repos\hermes-rs-n1   Branch: native/p1-core   Model: Opus 5.5 (subagent)
On top of CP1.1 (2a99eb9). Not committed.

### Verdict

Part A (carried over from CP1.1) is done: the turn runs in the core. `Core::submit` starts a
prompt or queues it behind the running turn; the turn's loop (stream, heartbeat, usage, end,
title, the next queued prompt) is `hermes_core::turns` and publishes the stream itself, so a
front end with a `Core` and no `Gateway` sees the whole turn (`turns/tests.rs`). `/` commands run
from Rust (`Core::run_command`, `dispatch_command`) with the catalog in `hermes_core::commands`.
Part B moved the logic of every listed module (git reads and review, files, `open_file`, the
terminal's shells, attachments, `@` paths, export, processes and subagents); `crates/desktop`
keeps the routes, the socket protocol and the JSON. A pure refactor for the page and ACP: the
RPC-table fixture passes **unchanged** (not re-blessed), the ACP suite and the live checks pass
(below).

### Done

**Part A — turns** (`crates/core/src/turns.rs`, was `gateway/turn.rs` `submit` / `run_queued` /
`enqueue*` / `dequeue` / `set_live` / `end_live` / `title_session`):
- `Core::submit(session, SubmitTurn, Arc<dyn TurnHooks>) -> Result<Submitted, SubmitError>`
  (blocking): `Submitted::{Started, Queued}` with the admission id; `SubmitError::{Busy, Invalid,
  Failed}` (the gateway maps them to `session busy` / -32602 / -32000 with the same texts).
  `SubmitTurn { admission_id, text, images, truncate: Option<Truncate>, shown, gated }`.
- The queue, typed: `queue`, `has_queued`, `enqueue`, `remove_queued`, `steer_queued`
  (`SteerQueued::{Idle, Started}`), `replace_queued`, `redirect`, `steer`. Storage stays the
  session actor's `PageState::queue` (JSON values): each item is a `SubmitTurn` serialized under
  the page's names (`submission_id`, `text`, `shown`), which `dequeue` looks up by. No runtime
  change.
- The loop (`TurnRun`): gate, `TurnStarted`, `hooks.started`, then the events (stream through
  `TurnStream`, `Usage` after `hooks.generated`, `Subagent`, notices, a missed steer queued at the
  front), then `TurnEnded { status, end: Box<TurnEnd> }` (outcome, error text, the provider error
  and which provider/model, the text, `partial`, the persisted rows), `hooks.ended`, `Idle` +
  `hooks.idle` unless a prompt is queued, the title (governor, ledger, `announce_title`,
  `hooks.changed`), and last `hooks.between_turns` (the model picked mid-turn) and the next
  queued prompt, on a blocking thread, with the same hooks.
- `TurnHooks` (the front end's side, every method called on a blocking thread, in order with the
  events): `settings(record)` (provider, title model: Settings stay the gateway's until CP1.3),
  `options(session)`, `follow`, `gate`, `started`, `changed`, `heartbeat`, `generated`, `ended`,
  `queued_failed`, `idle`, `between_turns`. `NoHooks` for a front end with a fixed provider.
- New `CoreEvent`s: `TurnStarted { admission_id }`, `TurnEnded { end }`, `QueueChanged`, `Idle`,
  `Subagent { kind, payload }`. The hub sends `TurnStarted` as `message.start` and `Subagent` as
  its `subagent.*` frame; it ignores the other three.
- Gateway (`gateway/turn.rs`, 874 → 548 lines): params to `SubmitTurn`, the receipt, the error
  mapping, `PageTurn` (the hooks: `session.info`, `session.usage`, `message.complete`,
  `note_generation`, the admission gate in `starts`, `apply_pending_route`), and the queue
  handlers over the core. `pending_rows` reads `Core::queue`.

**Part A — `/` commands** (`crates/core/src/commands.rs`, was desktop `commands.rs` +
`gateway/slash.rs`): `entries` / `complete` (typed `CommandEntry { name, description, origin }`),
`coding_command` (typed `CommandOutcome::{Output, Send, Skill}`), `Core::run_command(session,
name, args, &dyn CommandEnv)` (coding command, then report, then skill, else `NotACommand`),
`dispatch_command` (`command.dispatch`: coding, then skill), `status_text`, `context_text`, the
reports (`/usage`, `/insights`, `/diff`, `/tools`, `/memory`, `/sandbox`, `/refine`),
`review_task`, `sandbox_text`, `split_command`. `CommandEnv` (the gateway's `PageCommands`):
`context_length`, `route` (provider + options), `import_hermes`. Desktop `commands.rs` is the JSON
(`catalog`, `completions`, `outcome`); `slash.rs` maps `CommandError` to the same RPC errors.

**Part B** (logic to `crates/core`, JSON in `crates/desktop`):

| core module | typed API | stays in desktop |
|---|---|---|
| `git.rs` (+ the reads) | `repo_status -> RepoStatus { files: Vec<FileStatus>, .. }`, `rev_parse`, `entries`, `renamed_to`, `untracked_lines`, `default_branch` | `git.rs`: `HermesRepoStatus` JSON |
| `git_review.rs` (`git mv`) | `ReviewList`, `ReviewFile`, `CommitContext`, `GhAuth`, `ShipInfo`, `PullRequests`, `Worktree`, `WorktreeRequest`, `AddedWorktree`, `BranchInfo`, `BaseBranch`; changes answer `Result<_, String>` | `git_review.rs`: `route()` and every route's JSON |
| `files.rs` (`git mv`) | `resolve`, `is_sensitive`, `mime`, `readable`, `read_text -> TextPreview`, `write_text`, `mkdir`, `git_root`, `FileError`, `percent_decode` (from `rest.rs`, re-exported there) | `files.rs`: statuses and bodies |
| `open_file.rs` (`git mv`, whole) | already typed (`Open`, `plan`, `reveal_plan`, `run`) | `lib.rs` re-exports it |
| `terminal.rs` (`git mv`) | `Terminals::new(version)`, `spawn -> Arc<Shell>` / `SpawnError`, `get`, `remove`, `close_all`; `Shell::{attach -> Attachment { viewer, snapshot, truncated, output }, is_current, detach, write, resize, has_exited, exited}` | `terminal.rs`: the socket (`serve`, `pump`, meta frame, resize frames, close codes) |
| `attachments.rs` | `image_from_data_url`, `image_from_base64 -> SavedImage`, `store_upload -> StagedUpload`, `read_data_url`, `image_bytes`, `attach_file(cwd, name, AttachSource) -> Attached`, `AttachError`; `Core::attach_file`, `hold_image`, `detach_image` | `attachments.rs`: route statuses and JSON, `image_paths`, `attach_source` |
| `paths.rs` (`git mv`) | `complete -> Vec<PathItem>`, `list_dir -> DirListing` | `paths.rs`: the items' and `/api/fs/list` JSON |
| `export.rs` (`git mv`) | `Format`, `document` (Markdown, HTML over `transcript::entries`), `file_name`, `clean` | `export.rs`: the JSON snapshot (Hermes's `TranscriptMessage` rows) |
| `delegation.rs` | `Core::processes`, `kill_process -> KillOutcome`, `stop_processes`, `subagents`, `subagent_tail`, `interrupt_subagent`, `steer_subagent`, `delegation -> Delegation`, `pause_delegation` | `gateway/delegation.rs`: JSON |

Docs: `PORTING.md` (core rows for every moved module, desktop rows say which shape they keep),
`docs/ARCHITECTURE.md` (crate row, events paragraph, "A turn, end to end" steps 2 and 5, the
feature map rows), `docs/PITFALLS.md` (`hermes_core` section: the turn in the core, hooks vs
listener, the queue's JSON names, terminal output), `PHASES.md` (the carried-over block's status).

### Files

New in core: `src/turns.rs`, `src/turns/tests.rs`, `src/attachments.rs`, `src/delegation.rs`.
Moved with `git mv`, then split: `crates/desktop/src/{git_review,files,open_file,paths,terminal,export}.rs`
to `crates/core/src/`; desktop got new shape files of the same names (except `open_file.rs`).
Changed: core `Cargo.toml` (`base64`, `dirs`, `portable-pty`; dev `wiremock`), `lib.rs`,
`events.rs`, `commands.rs`, `git.rs`; desktop `attachments.rs`, `commands.rs`, `git.rs`,
`hub.rs`, `lib.rs`, `main.rs`, `rest.rs`, `server.rs`, `transcript.rs`,
`gateway/{mod,turn,slash,sessions,attachments,delegation}.rs`; `Cargo.lock`; the docs above;
this note.

`crates/desktop/src` line counts (whole tree 21,613 → 18,101): `gateway/turn.rs` 874 → 548,
`gateway/slash.rs` 483 → 182, `gateway/sessions.rs` 1,019 → 932, `gateway/attachments.rs` 94 →
82, `gateway/delegation.rs` 115 → 99, `git_review.rs` 1,167 → 267, `git.rs` 226 → 68, `files.rs`
370 → 121, `open_file.rs` 204 → 0, `terminal.rs` 546 → 208, `attachments.rs` 427 → 176,
`paths.rs` 394 → 52, `export.rs` 264 → 103, `commands.rs` 298 → 126, `rest.rs` 727 → 708,
`hub.rs` 325 → 339, `transcript.rs` 241 → 251. `crates/core/src`: 3,827 → 9,853 lines.

### What stayed in `crates/desktop`, and why

- **The routes, the socket protocol and Hermes's JSON** (by design): `git_review::route`, the
  terminal's socket, `/api/fs/*` statuses, `TranscriptMessage` rows, the export's JSON snapshot
  (Hermes's own file format, built from the page's transcript rows).
- **Settings-dependent pieces of a turn and of the commands**: the provider and title model
  (`PageTurn::settings`), `turn_options`, context windows and model facts, `provider_for`,
  `import_route` and the importer: CP1.3 (providers, aux models, fallbacks, settings, import).
  They reach the core through `TurnHooks` / `CommandEnv`.
- **`session.info`, `session.usage`, `note_generation`, `usage_snapshot`**: page frames over
  `usage.rs` (CP1.3); sent from the hooks.
- **`/btw` and `/bg`** (`prompt_btw`, `prompt_background`): not turns of the session, and their
  frames (`btw.complete`, `background.complete`) are page-only; the runtime calls they wrap are
  already typed. Small; can move with CP1.3 if the native UI wants them.
- **`file_tickets`** (download links, `/api/files/ticket`): a URL the page fetches; nothing the
  native UI needs.
- **`Terminals` is not held by `Core`**: the desktop's server state makes it before the core
  exists (the window and the server start first); the native app makes its own
  `Terminals::new(version)`.

### Deviations from the plan

- The plan says the gateway keeps `message.start`, `session.info` and `message.complete` "as
  frames built from those events". `message.start` is (hub, from `TurnStarted`); `session.info`,
  `session.usage` and `message.complete` are built in `TurnHooks` called by the core's loop with
  the same typed data (`TurnEnd`), because they read the store and a listener runs on the turn's
  task (it must not block). The frames keep the order the gateway's loop gave them.
- `CoreEvent` gained `QueueChanged`, `Idle` and `Subagent` besides the planned `TurnStarted` /
  `TurnEnded` (the native UI needs to know when the queue changes and when the session settles).
- Terminal output is a per-shell broadcast stream, not `CoreEvent`s (high volume, one viewer).

### Tests

All with `CARGO_TARGET_DIR=target/claude` (the worktree's).

- `cargo test -p hermes_core`: 80 passed. New: `turns/tests.rs` over a real runtime and a
  wiremock provider, **no gateway**: `a_prompt_sent_while_a_turn_runs_queues_and_runs_next_with_no_gateway`
  (first turn delayed; the second queues with its admission; a cut while running is `Busy`; both
  turns stream `TextDelta`s through the core alone, `TurnStarted` names both admissions,
  `TurnEnded` carries the text and the persisted rows, `QueueChanged`, then `Idle`) and
  `a_queued_prompt_is_removed_and_a_stopped_turn_ends_interrupted` (remove, remove again, steer
  a removed admission gives `Started`, `Core::stop` gives `TurnEnded` interrupted with no text and
  `Idle`; then steer gives `Idle` and redirect `false`). Moved and typed: `commands` (coding
  commands, completions, typed commands), `attachments`, `files`, `git` (+ a new untracked-lines
  test), `git_review`, `paths`, `export`, `terminal` (+ attach, supersede and resize in the shell
  test), `open_file`; new `delegation` (unknown process and subagent, pause).
- `cargo test -p hermes-desktop`: lib 155 passed (incl. `rpc_table_tests` against the
  **unchanged** `tests/fixtures/rpc_table.txt`); shape tests for every split module (`commands`,
  `attachments`, `files`, `git`, `git_review` routes over a temp repository, `paths`, `export`,
  `terminal`); `tests/acp.rs` 68 passed.
- `cargo test -p hermes_runtime --test turns`: 258 passed, 7 ignored.
- `cargo build --workspace`: passes.
- `cargo clippy -p hermes_core --all-targets --no-deps`: clean (one moved `sort_by` fixed);
  desktop: no warning on lines this checkpoint wrote.
- `cargo fmt -p hermes_core`: done; desktop files written here formatted with `rustfmt` one by
  one (the untouched not-clean files left alone, as in CP1.1).
- Live checks, each on a **fresh instance** (debug exe of this worktree, port 47810, data
  `target/perf/home` deleted before each start, `mock_llm.py` on :47900 for the checks with no
  provider of their own; temp folders outside any git repository): `queue`, `stop`,
  `coding-commands`, `rpc-dispatch` (all 80 methods), `workspace`, `tool-stream`,
  `side-and-branch`, `git` (review pane), `files`, `terminal`, `attach`, `subagents`,
  `artifacts` (open-file), `file-tree-open`, `archive`, `provider-error`, `rollback`: all passed
  (exit 0, every line `ok`). Only processes under `target\claude` and the mock this run started
  were stopped; none left running.
- `scripts/clean-test-temp.ps1`: dry run 431 folders / 172 MB (mostly the `turns` suite's known
  leftovers), `-Delete` removed 431, could not remove 0; a second dry run finds 0. No
  `hermes_testdir: could not remove` line in the test output.

### User's checklist (desktop app built from this branch)

1. Send a message: the reply streams, a tool card shows, the title appears, the status bar's
   tokens/s and context update.
2. While a reply runs, send two more: they show as queue cards; steer one into the turn, edit the
   other, delete it; Stop, then type and use "redirect": the redirected text runs first.
3. `/` menu: `/status`, `/context`, `/tools`, `/plan something` (sent as a prompt), a skill.
4. `@` in the composer: `@file:` lists files; `@name` finds a file deep in the project.
5. Attach an image and a text file; send: the model gets both.
6. Side panel: open a file, edit and save; Review tab: diff, stage, unstage, commit; worktree and
   branch pickers list; Terminal: run `echo hi`, reload the page, the output is replayed.
7. `/save` as Markdown, HTML and JSON (with "redact").

### For the reviewer

1. **Hooks, not events, for the store-reading frames.** `session.info` / `session.usage` /
   `message.complete` come from `PageTurn` hooks the core's loop awaits on a blocking thread
   (a listener must not block). Check the order against the old loop: `message.start` (hub, sync
   on the turn task) → `started` hook (`session.info` + context-length spawn) → stream; `Usage`
   event after the `generated` hook; `TurnEnded` then `ended` (`message.complete`); the usage
   snapshot for `message.complete` is now read after `TurnEnded` is published (no frame between).
2. **Queue storage**: items are `SubmitTurn` JSON with the page's names in `PageState::queue`.
   `prompt.replace_queued`'s `text` (a raw `Value` in `QueueRef`) is now stringified like
   `prompt.submit`'s (a missing text becomes `"null"`; before, the card showed the raw value).
   Only malformed params reach this. `shown` is still read from `prompt.submit` params, as the raw
   params used to carry it.
3. **Hooks follow the queue**: a queued prompt starts with the hooks of the turn that ran before
   it (same socket as before), and its Settings are reloaded then (as `run_queued` did); the first
   turn uses the Settings `dispatch` loaded (`PageTurn` takes them once).
4. `session.status` now loads Settings for `PageCommands` (one extra file read; unused there).
5. `Core::submit` and the queue methods are **blocking** (store reads, actor asks): the native UI
   must call them from `spawn_blocking`. Queue handlers on the gateway run where they ran before
   (some on async workers, as before CP1.2).
6. `Terminals` is a standalone type, not held by `Core` (see "What stayed"); output is a per-shell
   broadcast stream. `TERM_PROGRAM_VERSION` is passed in (`Terminals::new`) so it stays the
   desktop's 0.4.0, not the core crate's 0.1.0.
7. `kill_process` types `command` / `exit_code` (`Option<String>` / `Option<i64>`): a non-integer
   exit code (never produced) would now read `null`. Process and subagent rows stay the runtime's
   own JSON, passed through.
8. `/btw`, `/bg`, `file_tickets`, `turn_options`, model facts and the importer stay in the
   gateway (CP1.3 or page-only).
9. `percent_decode` moved to `hermes_core::files` (`rest.rs` re-exports); `open_file.rs` moved
   whole and is re-exported from desktop's `lib.rs`.
10. `gh` paths of `git_review` (ship info with a PR, pr-list, create-pr) are typed but, as before,
    not covered by tests (no GitHub here); `git-check` exercised `ship-info` without a remote.

### Review (Opus 5.5)

Main had not moved since 2a99eb9: no merge. `/code-review` was not run (no Skill pass in this
subagent); the pass below is by hand.

**Verdict: accepted.** Part A was compared with `gateway/turn.rs` / `gateway/slash.rs` at 2a99eb9
line by line: the same checks in the same order (archive reopen, busy on a cut, enqueue only while
running, the cut, the provisional title before the provider check, `first_row`, options, composer
then held images, `/review`), the same error texts, the same frame order (`message.start` from the
hub on the turn task, then the `started` hook's `session.info`; `generated` before `Usage`;
`TurnEnded` then `message.complete`; `Idle`/`running: false` only with nothing queued; the title
after; `apply_pending_route` then the next queued prompt on one blocking thread). Every hook is
awaited before the loop reads the next event, so hook frames and hub frames keep their order. No
core locks are held across a hook or a publish. `TurnEnded` is published once per
`TurnFinished`, and the runtime sends `TurnFinished` also when its turn task panics (lib.rs, the
crash guard) and for a provider error (a `Completed` turn whose last message is the error: the
core turns it into `Outcome::Error` with the error text, as before). The heartbeat is the loop's
own interval and ends with it; the title runs only after a complete turn. `Core::submit`'s callers
are `prompt_submit` (through `Gateway::blocking`), `run_queued` (inside the loop's
`spawn_blocking`) and the tests (`spawn_blocking`): none on an async worker. The queue handlers
(`prompt.cancel`/`steer_queued`/`replace_queued`, `session.redirect`) run where they ran before.
The `/` dispatch keeps the old order (coding command, report, skill; `command.dispatch`: coding,
then skill) and argument trimming. Part B: `files` (statuses map 1:1 to the old codes, the
sensitive-path and traversal checks untouched), `terminal` (same attach/detach/supersede/exit
logic and lock order; a lagging viewer skips, a closed feed ends the pump, `remove`/`close_all`
kill the child; the reader thread holds the shell, so the pseudo-console's handles live until its
pipe ends, exactly as before the move), `attachments`, `paths`,
`export` (same output over `transcript::entries`), `delegation`, `git`/`git_review` (string
literals and git arguments compared old vs. new: none lost). RPC-table fixture: unchanged since
ceddf45 and the test passes. Store schema untouched; prompt caching untouched; Hermes citations
moved with the logic; PORTING rows right; new tests use `hermes_testdir`, `TestDir` last.

**Found and fixed**
- *The core test synchronised on a sleep.* `a_prompt_sent_while_a_turn_runs…` held the first
  reply 1.5 s and assumed the second prompt queues within that. The mock now holds the first reply
  until the test releases it (a channel; dropped with the fixture, before the server), so the
  first turn runs exactly as long as the test needs. The stop test holds it until the fixture
  drops instead of 20 s. Runs in ~1.4 s; 5 consecutive runs green.
- *A queued prompt that could not start was invisible without the page's hooks.* `run_queued`
  told only `TurnHooks::queued_failed` (the page's `message.complete` error); a front end with
  `NoHooks` saw the prompt leave the queue and `Idle`, never why. New `CoreEvent::QueuedFailed {
  admission_id, error }` (the hub ignores it: no page change), and `QueueChanged` when a queued
  item can't be read. Test: `a_queued_prompt_that_cannot_start_says_why_and_the_session_settles`
  (an attached image gone by the time the prompt runs).
- Docs: PITFALLS (`QueuedFailed`; the queue races below).

**Checked and left**
- `prompt.replace_queued` with a non-string `text`: the old code stored the raw value (the card
  showed `null`) and, when it ran, sent `"null"` to the model (`submit` stringified it). Now the
  text is stringified when queued, so the card shows `"null"` too. The old code did not reject it,
  and Hermes's page always sends a string: left as is.
- Queue races the old gateway had (PITFALLS): a steer, an edit or a redirect landing in the one
  actor round trip after the turn ended re-queues the prompt with no loop to run it. Same as
  before; a fix needs an "enqueue only while running, else submit" step: follow-up, not CP1.2.
- A panic inside the core's own loop task (not the runtime's) leaves the session live with no
  `TurnEnded`; the old gateway loop had the same gap. Follow-up if the native UI wants a guard.
- Line counts: the note's `crates/core/src` "before" is 3,892 lines at 2a99eb9 (it says 3,827);
  after this review 9,919. The desktop numbers are right (18,102 now).

**Deviations: accepted.** Hooks instead of events for `session.info` / `session.usage` /
`message.complete`: right, those read the store and a listener must not block; the typed data
(`TurnEnd`, `Usage`, `QueueChanged`, `Idle`) is in the events, so the native UI loses nothing. The
queue as `SubmitTurn` JSON in the actor: typed at the API (`Core::queue`), and PITFALLS warns
about renaming fields. `/btw`, `/bg`, `file_tickets` in desktop: page-only frames over typed
runtime calls; move with CP1.3 if the native composer wants `/btw`. `Terminals` outside `Core`:
fine (one per app; the native app builds its own). With a `Core` and no `Gateway` the native UI
(Phases 4–6) has: `Core::submit` with `NoHooks` or its own hooks (provider and options from its
side until CP1.3 moves Settings), `queue` / `remove_queued` / `steer_queued` / `replace_queued` /
`redirect` / `steer`, `Core::stop`, the whole stream plus `TurnStarted` / `TurnEnded` /
`QueueChanged` / `QueuedFailed` / `Idle`, `run_command` with a `CommandEnv` it implements
(context window, route, import), and `Terminals`. Nothing material is missing for CP1.3.

**Re-run by the reviewer** (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes_core` 81
passed (the three `turns` tests 5 times in a row, ~1.4 s each); `-p hermes-desktop` lib 155
(RPC table unchanged) and `tests/acp.rs` 68 passed; `cargo test -p hermes_runtime --test turns`
258 passed, 7 ignored; `cargo build --workspace` passes; clippy on `hermes_core` clean. Live,
each on a fresh instance (debug exe of this branch, port 47810, `target/perf` home deleted before
each, `mock_llm.py` where a check has no provider; temp folders in `%TEMP%`): `queue`, `stop`,
`coding-commands`, `rpc-dispatch`, `tool-stream`, `git`, `files`, `terminal`, `attach`,
`subagents`: all exit 0. Only this build's processes and the mock were stopped; none left.
`clean-test-temp.ps1`: 216 folders / 88.7 MB, all removed, a second dry run finds 0; the checks'
own temp folders removed too.

## CP1.3 — Settings and Hermes features
Worktree: C:\Users\alexandros\repos\hermes-rs-n1   Branch: native/p1-core   Model: Sonnet 5.5 (subagent)
On top of CP1.2 (cc0666d). Not committed. This is Phase 1's last checkpoint.

### Verdict

The logic of every module on the list is in `hermes_core`, typed, with Rust errors; `crates/desktop`
keeps Hermes's JSON, the `{detail}` / RPC error shapes, the route tables and the page-only frames.
A pure refactor for the page and ACP: the RPC-table fixture passes **unchanged** (not re-blessed),
the ACP suite and the live checks pass (below). Nothing on the list was left behind: the modules
that were "mostly typed already" (`providers.rs`, `fallback.rs`, `hermes_import.rs`) moved whole
with `git mv`; the others were split.

### Done

| core module | typed API | stays in desktop |
|---|---|---|
| `providers.rs` (`git mv`, whole) | `Settings`, `ProviderSettings`, `ModelChoice`, `AuxModels`, `save_default`, keys, `model_ids` | re-export (`lib.rs`) |
| `config.rs` (new, from `rest.rs`) | `ConfigRecord` (`desktop-config.json`: `merged`, `save`, `set_value`, `value`), `defaults()`, `deep_merge`, `default_model` | `rest.rs`: the schema, `/api/config*` routes, UI preferences, plugins; `Rest` holds the core's `Arc<ConfigRecord>` |
| `endpoints.rs` | `list -> EndpointList`, `save(EndpointInput) -> id`, `delete`, `activate`, `validate -> Validation`, `EndpointError::{Invalid, NotFound, Failed}` | JSON, 400 / 404 / 500 `{detail}` |
| `aux_models.rs` | `list -> AuxList`, `assign(Assignment { task, provider, model, EffortChange }) -> Assigned`, `local_servers`, `use_local -> UsedLocal`, `runtime_model(s)`, `AuxError` | JSON, `effort_of` (the page's null / absent / string) |
| `fallback.rs` (`git mv`, whole) | `validate`, `resolve` over the config record's list (a file format, not a page shape) | re-export |
| `profiles.rs` | `soul -> Soul`, `save_soul`, `ProfileError::{NoSuchProfile, Io}` | JSON, 404 `Profile '<name>' does not exist.` / 500 |
| `toolsets.rs` | `TOOLSETS`, `list -> Vec<Toolset>`, `switch`, `disabled_tools` | JSON, the config and terminal-backend stubs |
| `skills.rs` | `list -> Vec<SkillInfo>`, `toggle`, `content`, `create`, `edit`, `apply_config`, `SkillError` | routes, JSON, the skill hub's "nothing here" answers |
| `maintenance.rs` | `Actions` (typed `ActionStatus`), `curator_status`, `set_curator_paused`, `curator_model`, `run_curator` / `schedule_curator` (publish `CuratorStarted` / `CuratorFinished`), `memory_status`, `reset_memory(ResetTarget)` | JSON, `/api/actions/*` status shape |
| `staging.rs` | `list -> Vec<Item>`, `apply`, `reject -> Done`, `Failure` | `staging.*` and REST JSON, 404 / 409 |
| `learning.rs` | `graph -> Graph` (typed nodes, edges, cards, links, stats), `detail`, `delete`, `edit`, `LearningError` | `StarmapGraph` JSON (field order kept), `{ok, message, detail}` answers |
| `usage.rs` (from `gateway/usage.rs`) | `Core::usage_snapshot -> UsageSnapshot`, `note_generation`, `requests_status`, `insights`; `generation-stats.json` | JSON (`session.usage`, `requests.status`, `insights.get`) |
| `hermes_import.rs` (`git mv`, then typed) | `list -> ForeignPage`, `preview -> Preview`, `import -> Imported`, `import_all`, `ImportError` | `session.foreign.*` JSON |
| `settings.rs` (from `gateway/settings.rs`) | `Core::apply_settings`, `sweep_archive`, `turn_options`, `picked_effort` / `pick_effort` / `effort`, `pending_route` / `apply_pending_route`, `model_options -> ModelOptions`, `switch_model(ModelSwitch, ..., changed) -> ModelSwitched`, `route_session`, `known_context` / `context_length` / `model_facts` / `sends_images` / `forget_model_facts`, `provider_for` / `session_provider` / `import_route` (`RouteError`, `ImportRouteError`), `setup_status`, `provider_limits`, `sandbox_grants` | `config.get` / `config.set` shapes, `setup.*` and `profiles.list` JSON, the picker's JSON, the `session.info` frames |
| `commands.rs` (CP1.2's hook) | `CoreCommands` (`CommandEnv` over the core's own Settings: window, route, `/import-hermes`) | `Core::run_command(.., &CoreCommands::new(&core))` needs no gateway; the gateway passes the request's Settings (`with_settings`) |
| `events.rs` | `CoreEvent::CuratorStarted`, `CuratorFinished { ok, lines }` (no session: `session_id()` is empty) | the hub ignores them |

`Core` gained `config()`, `actions()`, `run_curator()`, `schedule_curator()`; it owns the models
cache (`provider\nmodel` → window / vision) and the generation stats the gateway held. The gateway
lost `actions`, `context_lengths`, `generation`, `GENERATION_FILE`, `ModelFacts`, `turn_options`,
`pending_route`, `pick_effort`, `picked_effort`, `effort`, `auto_archive`, `PageCommands` and the
settings logic; what it keeps in `gateway/settings.rs` and `usage.rs` is JSON and error mapping.
Docs: `PORTING.md` (core rows for the moved modules with their Hermes citations and the diverged
notes, desktop rows say what shape they keep), `docs/ARCHITECTURE.md` (the picture now shows
`hermes_core::Core` between the gateway and the runtime; crate row; the feature map; data folder
rows), `docs/PITFALLS.md` (CP1.3 bullets), `core/src/lib.rs` module table, PHASES.md (CP1.3 status),
and small path fixes in GAPS.md, GAPS-PLAYBOOK.md, TESTING.md, `docs/acp/PROTOCOL.md`.
No new patch and no new perf check (a pure refactor): `scripts/perf/README.md` and
`crates/desktop/patches/README.md` unchanged.

### Files

New in core: `src/{config,endpoints,aux_models,profiles,toolsets,skills,maintenance,staging,
learning,settings,usage}.rs`, `src/settings_tests.rs`. Moved with `git mv`: `crates/desktop/src/
{providers,fallback,hermes_import}.rs` to `crates/core/src/`. Changed in core: `Cargo.toml`
(`hermes_memory`, `futures`, `keyring`, `rusqlite`, `sha2`: all already in `Cargo.lock`),
`lib.rs`, `commands.rs`, `events.rs`. Changed in desktop: `src/{endpoints,aux_models,profiles,
toolsets,skills,maintenance,staging,learning,hermes_import,rest,hub,lib}.rs`, `src/acp/server.rs`,
`src/gateway/{mod,settings,usage,sessions,turn,slash,rest,tests}.rs`. `Cargo.lock`. The docs above
and this note.

`crates/desktop/src` line counts (whole tree 18,101 → 14,586): `providers.rs` 218 → 0, `fallback.rs`
288 → 0, `hermes_import.rs` 774 → 77, `endpoints.rs` 484 → 248, `aux_models.rs` 757 → 281, `profiles.rs`
157 → 65, `toolsets.rs` 183 → 62, `skills.rs` 330 → 251, `maintenance.rs` 289 → 129, `staging.rs`
125 → 79, `learning.rs` 769 → 453 (its JSON tests stayed), `rest.rs` 708 → 601, `gateway/settings.rs`
643 → 249, `gateway/usage.rs` 156 → 97, `gateway/slash.rs` 182 → 105, `gateway/sessions.rs` 932 → 885,
`gateway/turn.rs` 548 → 505, `gateway/mod.rs` 461 → 420. `crates/core/src`: 9,919 → 16,254 lines.

### Tests

All with `CARGO_TARGET_DIR=target/claude` (the worktree's).

- `cargo test -p hermes_core`: 137 passed (was 81). New or moved typed tests per module (endpoints
  incl. the daily limit, aux models incl. the local-server probes and the hung-server timeout,
  profiles, toolsets, skills, maintenance `Actions` / memory reset / curator pause, learning,
  settings `provider_limits` / `sandbox_grants` / `setup_status`, config record, usage) and
  `settings_tests.rs` over a real runtime with **no gateway**: the config record reaches the
  runtime (`apply_settings`), a model pick moves the session and the default (the `changed`
  callback, an unknown provider refused), a session's provider and a turn's options (window,
  "ultra" effort sent as "max"), usage / generation speed / quota gauge, a curator run reporting
  `CuratorStarted` then `CuratorFinished` and one run at a time, staging with nothing waiting.
- `cargo test -p hermes-desktop`: lib 130 passed (was 155: typed tests moved to core), including
  `gateway::rpc_table_tests::every_method_answers_as_the_table_says` against the **unchanged**
  `tests/fixtures/rpc_table.txt`; shape tests for every split module (the routes' JSON and
  statuses); `tests/acp.rs` 68 passed.
- `cargo test -p hermes_runtime --test turns`: 258 passed, 7 ignored.
- `cargo build --workspace`: passes.
- `cargo clippy -p hermes_core --all-targets --no-deps`: clean (two moved `sort_by` and one
  redundant clone in a moved test fixed). `cargo fmt -p hermes_core`: done; desktop files written
  here formatted with `rustfmt` one by one (`rest.rs`, `gateway/{mod,sessions}.rs` and `lib.rs`
  were not clean before and are left alone: `rustfmt` on a `mod.rs` would reformat their whole
  tree).
- Live checks, each on a **fresh instance** (debug exe of this worktree, port 47810, `target/perf/home`
  deleted before each start, started with perf.ps1's environment; temp folders in `%TEMP%`,
  outside any git repository; `mock_llm.py` only for `endpoints` and `stop`, every other check is
  its own provider): `budget`, `requests`, `endpoints`, `aux-models`, `local-aux`, `fallback`,
  `profiles`, `toolsets`, `staging`, `memory` (the Star map, node edits, the curator's pass,
  memory files), `import` (a home built by `make-hermes-db.py`), `rpc-dispatch` (all methods),
  `queue`, `stop`, `coding-commands`, `archive`, `provider-error`, `side-and-branch`,
  `subagents`, `review`, `workspace`: **all exit 0**, every line `ok`. (`import` first failed two
  lines because my runner named the project folder `import`, not `import-folder`, which the
  check greps for; rerun with the right folder: all ok.) The instance and the mock were stopped
  after each check (only processes under `target\claude` and the mock this run started); none
  left running.
- `scripts/clean-test-temp.ps1`: dry run 235 folders / 93 MB (the `turns` suite's known leftovers
  and the ACP tests'), `-Delete` removed 235, could not remove 0; a second dry run finds 0. The
  checks' own temp folders were removed too.

### User's checklist for Phase 1 (desktop app built from this branch)

A short pass over the main screens; everything should look and behave as before. The first two
are the lock and the CP1.2 items, then CP1.3's.

1. **"Hermes is already open".** Start `hermes-desktop.exe`, then start the same exe a second
   time (same data folder, i.e. without `HERMES_RS_HOME`): a dialog says "Hermes is already
   open. Switch to its window, or close it before starting Hermes again." and no second window
   opens. Close the first app and start it again: it opens (a crash never leaves the folder
   locked). Zed's `hermes-acp.exe` still works beside the running app.
2. **Chat** (CP1.1 / CP1.2): send a message: the reply streams, a tool call shows its card and
   result, the title appears, the status bar's tokens/s and context percentage update.
3. **Queue and stop** (CP1.2): while a reply runs, send two more: they show as queue cards; steer
   one into the turn, edit the other, delete it; Stop, then type and use "redirect".
4. **Approvals**: ask for something that needs approval ("delete the build folder"): the card
   shows; "Allow once" runs it. Stop a turn while a card is open: the card closes.
5. **`/` menu and `@`** (CP1.2): `/status`, `/context`, `/tools`, `/plan something`, a skill;
   `@file:` lists files. `/import-hermes` (CP1.3, needs a Hermes install) imports your Hermes
   sessions or says none were found.
6. **Sidebar and Projects**: rename, archive (it leaves the list; Settings ▸ Sessions lists it),
   restore, delete; `/branch` from a message; drag a session onto a project.
7. **Side panel** (CP1.2): open a file, edit and save; Review tab: diff, stage, commit; Terminal:
   `echo hi`, reload the page, the output is replayed; attach an image; `/save` as Markdown.
8. **Model picker** (CP1.3): open the picker in the composer: your providers and their models
   are listed; pick another model for this chat (session only); pick one as the default: a new
   chat uses it. Change the effort (the brain icon): it shows in the status bar.
9. **Settings ▸ Providers ▸ Custom endpoints** (CP1.3): add an endpoint (Test connection lists
   its models), make it the default, edit it (name, daily request limit), delete it: the default
   moves to another endpoint.
10. **Settings ▸ Model** (CP1.3): Auxiliary: pick a model and an effort for "Title generation",
    reset all; "Use a local model for background jobs" if Ollama / LM Studio runs; Fallbacks:
    add and remove an entry. Settings ▸ Safety: switch the approval mode and Checkpoints; the
    status bar's mode follows at once. Settings ▸ Memory: change a size; Settings ▸ Sessions:
    auto-archive on.
11. **Capabilities** (CP1.3): Tools: switch Terminal off, send a message that needs it: the agent
    has no terminal; switch it on. Skills: toggle a skill, create one, edit it; "Waiting for your
    review" shows items when a tainted session left writes (apply / reject).
12. **Memory and Star map** (CP1.3): the map shows learned skills and memories; open a memory
    node, edit it, delete it. **Command Center ▸ Maintenance**: "Run curator now" finishes and
    shows its lines; pause / resume; memory file sizes; **profile rail ▸ Edit SOUL.md**: save,
    reopen. **Usage**: the status bar's allowance gauge shows when the endpoint has a daily limit.

### What stayed in `crates/desktop`, and why

- **Hermes's JSON, statuses and RPC errors** (by design): every `list` / `save` shape, the
  `{detail}` errors, `StarmapGraph`, the toolsets' and skills' fields, the skill hub's empty answers,
  `config.get` / `config.set`'s params and answers, `setup.*`, `profiles.list`, the picker's JSON.
- **`rest.rs`**: the config schema (`config_schema`, the descriptions), the REST routes over the
  record, UI preferences, desktop plugins. The record itself (defaults, merge, save) moved.
- **`POST /api/model/set`** (`gateway/rest.rs`): a three-line check (known provider) and
  `save_default`; page-only validation, left.
- **`/btw`, `/bg`, `file_tickets`** (from CP1.2's list): page frames over typed runtime calls; they
  can move when the native composer wants them (Phase 5).
- **`session_info`, `status_text`'s page shape, `context_breakdown`**: page shapes over the
  runtime and the typed `usage` / `settings` calls.
- **`viewing`** and the review route (`install_review_route`): keyed by the page's socket.

### Deviations from the plan

- None in scope. `CommandEnv` stays a trait (a front end may still supply its own); `CoreCommands`
  is the default one, so "Core::run_command no longer needs the gateway" holds for every part
  (window, route, import).
- `Core::new` does not call `apply_settings` (as before, the gateway does at start); a front end
  with no gateway must call `core.apply_settings()` and `hermes_core::skills::apply_config(
  &runtime.skills(), core.config())` once at start (see "For the reviewer" 2).

### For the reviewer

1. **`apply_settings` order.** The gateway's wrapper now runs `Core::apply_settings` (everything
   on the runtime), then sends `session.info` to the followed sessions, then `sweep_archive`.
   Before, the `session.info` frames were sent right after the approval mode was set and before
   the environment, sandbox, ledger, budget, curator, checkpoints, memory and tool settings. The
   frame reads the record, so it can only be more current now; there was no frame between.
2. **Two start-up calls for a front end with no gateway**: `Core::apply_settings()` and
   `skills::apply_config(...)`. Folding the second into the first would be harmless (it is
   idempotent) but would change what a settings change touches, so it is not done; the native UI
   (Phase 3) must call both.
3. **One record, two owners' worth of code.** `Rest::new(data_dir)` still builds a record of its
   own (the `rest.rs` and `skills.rs` tests use it); the gateway uses `Rest::with_config(.., core
   .config().clone())`. Two records over one file would overwrite each other: PITFALLS says so.
4. **`switch_model`'s `changed` callback** is called at the same points the gateway emitted
   `session.info` (a deferred pick, a routed session), before `save_default`; `apply_pending_route`
   now returns whether it moved the session and the gateway emits. The error texts are the
   same strings (`RpcError::server` of the same text; the busy text still maps to `Busy`).
5. **`provider_for` / `session_provider`**: kept as two core functions because their errors differ
   (a store error vs "no session", "Session x not found" vs "No session x."); the gateway maps
   `RouteError::NoSession` for the second to `RpcError::invalid` as before.
6. **`Graph.archive` is `hermes_memory::ArchiveRow`** (not a copy), so the Star map's typed graph
   depends on that type; the JSON is built from it unchanged.
7. **Curator events carry no session**: `CoreEvent::session_id()` returns "" for them. Fine for the
   hub (it ignores them); a subscriber that indexes by session id must skip them.
8. **`schedule_curator` spawns on the core's handle** (it used `Handle::try_current()` and did
   nothing when there was no runtime); `Gateway::new` is always built inside the tokio runtime
   in the app and the tests, so nothing changes there.
9. **Duplicated tests are intended**: the learning tests that read JSON stayed in desktop and
   the typed versions are in core (the two files each test their own layer). Desktop's lib count
   went 155 → 130 because the typed halves moved; core 81 → 137.
10. **`staging.rs` has no unit tests of its own** beyond the core test (nothing staged, unknown
    id): staging an item needs a tainted session; the gateway's staging tests (`gateway/tests.rs`)
    and `staging-check.mjs` cover apply / reject / shapes. Adding a core test with a seeded
    `staging/` folder is easy if wanted.
11. **Formatting**: `cargo fmt -p hermes-desktop -- --check` still lists files untouched here and
    `rest.rs`, `gateway/mod.rs`, `gateway/sessions.rs`, `lib.rs` (not clean before).

### Review (Opus 5.5)

Main had not moved since 83e7e87: no merge. `/code-review` was not run (no Skill pass in this
subagent); the pass below is by hand.

**Verdict: accepted; Phase 1 done.** Compared against HEAD (cc0666d) function by function:
`apply_settings` (same keys, same order, same defaults and bounds), `sweep_archive` (= the old
`auto_archive`), `switch_model` / `route_session` / `apply_pending_route` (same checks, same error
texts, `session.info` at the same points through the callback or the return value),
`known_context` / `context_length` / `model_facts` / `sends_images` (one cache, same key, same
"asked again only when the provider did not answer"), `provider_for` / `session_provider` /
`import_route` (same texts; `NoSession` still `-32602` for the second), `usage_snapshot` (same
arithmetic: the cap of `used` at the window, the `used > 0` condition, `used_bp` saturating and
capped at 10,000, `resets_at * 1000`; the JSON built from it has the same keys), `CoreCommands`
(= the old `PageCommands`, the import text verbatim), the config record (`defaults`, `deep_merge`,
the `model` / `budget` filtering, `set_value`'s fold and `value`'s dotted read; the old
`{"config": ...}` wrapping round trip is equivalent), endpoints (same validation order and texts,
`daily_request_limit` absent vs null kept apart, keys only to the key store, previews masked as
before), `staging`, `maintenance`, `hermes_import` (no new `unwrap`/indexing on user data; the
two indexings outside tests are guarded as before), `profiles` (only "default", atomic write
kept). Secrets: no key is logged or returned anywhere it was not before. RPC-table fixture:
`git diff` shows `crates/desktop/tests/` untouched since 2a99eb9, and the test passes. Store
schema untouched; prompt caching untouched (Settings still apply on the next turn / session);
blocking calls are reached through `blocking` / `spawn_blocking` as before; Hermes citations
moved with the logic; PORTING rows point at `crates/core/src`; tests use `hermes_testdir` with the
`TestDir` last.

**The `session.info` order in `Gateway::apply_settings`** (implementer's point 1): not visible.
The frames are built and sent inside the same synchronous call either way, after the approval
mode is set; the only difference is that the frame now also sees the later settings, and no page
frame can come between. Left as is.

**Found and fixed**
- *Start-up was two calls a front end could forget.* New `Core::start(background)`: Settings on
  the runtime, the auto-archive sweep, the scheduled curator when `background`, the switched-off
  skills: exactly what `Gateway::build` did, in its order. The gateway now calls it (the
  `session.info` loop it skips has no followed session at build time), so the native app (Phase 3)
  starts the same way. Test: `start_puts_the_stored_settings_on_the_runtime` (`Core::new` applies
  nothing, `start` applies a stored record).
- *A curator run that panicked never finished.* `CuratorStarted` would have no `CuratorFinished`
  and the `curator-run` action stayed "running", refusing every later run until a restart (the old
  code had the same gap). The run is now its own task; a panic ends the action as failed and
  publishes `CuratorFinished { ok: false }`. A runtime shut down mid-run still ends without one
  (the app is closing).
- *Staging had no apply / reject test in core.* `a_staged_write_is_applied_or_rejected` stages two
  memory writes, applies one (it reaches MEMORY.md), rejects the other, and checks neither id can
  be decided again.
- *`rest.rs`'s three tests leaked their folders* (`%TEMP%\hermes-desktop-rest-*` / `-prefs-*`,
  made with `std::env::temp_dir()`, which `clean-test-temp.ps1` does not sweep; there before this
  checkpoint): now `hermes_testdir::tempdir()` (ground rule 9).
- Docs: `ARCHITECTURE.md` (`desktop-config.json`'s owner, `Core::apply_settings` / `Core::start`),
  `PITFALLS.md` (`Core::start`; curator events always paired),
  `CONVENTIONS.md` and `GAPS-PLAYBOOK.md` (where a Settings key's default and its runtime push go
  now), `GAPS.md` 3.8's path to `fallback.rs`; PHASES.md (Phase 1 ✅, the review line).

**Checked and left**
- `desktop-config.json` and `generation-stats.json` are still written with `std::fs::write` (not
  temp + rename): as before the move; a crash mid-write loses the record. Follow-up, not CP1.3.
- `curator_model` reads `config.json` on an async worker (as before).
- The implementer's open points 3–11: agreed (two `ConfigRecord`s over one file only in tests;
  curator events with an empty session id; `schedule_curator` on the core's handle).
- The 12-step checklist above was spot-checked against the code (the lock's dialog text, the
  local-models button, the curator / SOUL.md routes, the allowance gauge): accurate.

**Re-run by the reviewer** (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes_core` 139
passed (137 + the two new tests); `-p hermes-desktop` lib 130 (RPC table unchanged) and
`tests/acp.rs` 68; `cargo test -p hermes_runtime --test turns` 258 passed, 7 ignored; `cargo build
--workspace` passes; clippy on `hermes_core` clean, `cargo fmt -p hermes_core` clean. Live, each on
a fresh instance (debug exe of this branch, port 47810, `target/perf/home` deleted before each,
`mock_llm.py` for `rpc-dispatch`, `endpoints`, `stop`; temp folders in `%TEMP%`, outside any git
repository; `import` against a home from `make-hermes-db.py` with the project in `import-folder`):
`rpc-dispatch` (87 ok), `budget`, `endpoints`, `aux-models`, `fallback`, `profiles`, `toolsets`,
`staging`, `memory`, `import`, `review`, `queue`, `stop`: all exit 0, no `FAIL` line. Only this
build's processes and the mocks it started were stopped; none left. `clean-test-temp.ps1`: 220
folders / 80.8 MB removed, a second dry run finds 0; the runs' own folders removed.

