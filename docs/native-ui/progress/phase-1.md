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
