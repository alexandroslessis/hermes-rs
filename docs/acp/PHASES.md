# ACP implementation, phase by phase

Each phase ends in something the user can try in Zed, with its tests green and its docs true.
Do them in order; don't start a phase before the previous one is committed. Within a phase, do the
steps in order and run the phase's tests before moving on.

Before Phase 0, read: [README.md](README.md) (decisions, architecture), [PROTOCOL.md](PROTOCOL.md)
(the shapes), [TESTING.md](TESTING.md) (the harness), and from the repo: `AGENTS.md`,
`docs/ARCHITECTURE.md` (the gateway section), `crates/desktop/src/hub.rs`,
`crates/desktop/src/transcript.rs`, and in `gateway.rs` the functions `handle`, `dispatch`,
`create_session`, `resume_session`, `submit`.

Rules that matter most here:
- **Nothing on stdout but protocol lines.** No `println!`, no `dbg!`, and logging goes to file +
  stderr (Phase 0 step 2). A test checks it (TESTING.md T-stdout).
- **Never block the async workers** (ground rule 7): store reads, file reads for diffs, the
  session list: `tokio::task::spawn_blocking`.
- **Gateway changes stay small.** The ACP layer is a *client* of the gateway (README D2). If you
  find yourself copying logic out of `gateway.rs`, stop: make the existing function
  `pub(crate)` or add a gateway method the page could use too, and say why in the commit.
- **Don't touch the user's data.** Every test sets `HERMES_RS_HOME` to a temp folder. Build
  into `CARGO_TARGET_DIR=target/claude`.
- When ACP behaviour isn't settled by [PROTOCOL.md](PROTOCOL.md), look at Hermes
  (`../hermes-agent/acp_adapter/`), then Zed's client (`../zed/crates/agent_servers/src/acp/`,
  `../zed/crates/acp_thread/`), then ask the user.

---

## Phase 0 — The `--acp` mode answers `initialize`

✅ **Done 2026-11-10.**

**Goal:** `hermes-desktop.exe --acp` starts without a window, reads JSON-RPC lines from stdin,
answers `initialize`, answers -32601 to everything else, and exits when stdin closes. Zed lists the
agent and connects to it (creating a thread fails with "method not found", which is expected).

1. **Entry point** (`crates/desktop/src/main.rs`). First lines of `main()`, before
   `init_logging` and before anything Tauri:
   ```rust
   if std::env::args().skip(1).any(|a| a == "--acp") {
       return acp::run();
   }
   ```
   Add `mod acp;` with the other modules. `acp::run() -> anyhow::Result<()>` lives in
   `src/acp/mod.rs`.

2. **Logging** (`acp/mod.rs`, `fn init_logging`). Copy `main.rs`'s `init_logging` but write to
   `logs/acp.log` (same 5 MB rotation) and `std::io::stderr`, **not stdout**. Default filter
   `info`, `RUST_LOG` overrides. Also log `clientInfo` on `initialize`.

3. **Startup** (`acp::run`): `RuntimeConfig::for_current_user()` (same shell choice as `main.rs`:
   move `wants_git_bash` to `pub(crate)` and reuse it), build a multi-thread tokio runtime (2–4
   workers), then inside it:
   - `Runtime::new(config, bridge)` in `spawn_blocking`, with the same `DesktopBridge` over a new
     `Hub` that `main.rs` uses (look at how `main.rs` builds them and do the same).
   - `Gateway` built **without background jobs** (README D8): add a constructor
     `Gateway::new_for_acp(runtime, hub, data_dir)` (or a `background_jobs: bool` parameter) that
     skips `maintenance::schedule_curator`; don't call `Gateway::schedule` (auto-archive).
   - `AcpServer::new(gateway).serve(tokio::io::stdin(), tokio::io::stdout()).await`.
   Startup work must not delay reading stdin for long: Zed times out an `initialize` that takes
   too long. If `Runtime::new` is slow (skills seeding), start it in the background and make
   `initialize` not depend on it (only `session/*` wait for it, via a `tokio::sync::OnceCell`).

4. **Framing** (`acp/rpc.rs`). Generic over `AsyncRead`/`AsyncWrite` so tests can use
   `tokio::io::duplex`:
   ```rust
   pub enum Incoming {
       Request { id: Value, method: String, params: Value },
       Notification { method: String, params: Value },
       Response { id: Value, result: Result<Value, RpcError> },
   }
   pub struct RpcError { pub code: i64, pub message: String, pub data: Option<Value> }
   pub fn parse_line(line: &str) -> Result<Incoming, RpcError>;   // pure, unit-tested

   /// The one writer: everything outgoing goes through this channel, one line each.
   #[derive(Clone)]
   pub struct Sender { tx: mpsc::UnboundedSender<String>, next_id: Arc<AtomicU64>,
                       pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>> }
   impl Sender {
       pub fn respond(&self, id: Value, result: Result<Value, RpcError>);
       pub fn notify(&self, method: &str, params: Value);
       pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError>; // waits for the client's response
       pub fn resolve(&self, id: &Value, result: Result<Value, RpcError>);                 // called by the reader
   }
   ```
   - Reader loop: `BufReader::lines()`; skip empty lines; `parse_line`; a parse error →
     `respond(Value::Null, Err(-32700))`; `Request` → `tokio::spawn(server.handle_request(...))`;
     `Notification` → handled inline (cheap) or spawned; `Response` → `sender.resolve`.
   - Writer task: receives `String`s, writes each + `"\n"`, **flushes after every line**.
   - EOF on stdin: the client is gone. Interrupt every running turn (gateway `session.interrupt`
     for each session in our map), fail all pending outgoing requests, give tasks ~2 s, then
     return from `run` (the process exits). Check that terminal processes the agent started are
     stopped (`Runtime::stop_processes`).
   - Analogy: this is a tiny version of what `gateway.rs` `connection`/`handle` do for the
     WebSocket, with stdin/stdout instead.

5. **Server skeleton** (`acp/server.rs`): `AcpServer { gateway: Arc<Gateway>, sender: Sender,
   client: Mutex<ClientInfo> }` with `async fn handle_request(&self, method, params) -> Result<Value, RpcError>`
   as a `match method { "initialize" => …, _ => Err(method_not_found) }` — the same shape as
   `Gateway::dispatch`. `initialize` answers exactly PROTOCOL.md §2 with the Phase-1 capability
   values (everything off). Unknown methods log under target `hermes_desktop::acp::missing`.

6. **Probe script** (`scripts/acp/acp-probe.mjs`, TESTING.md §3): spawns the exe with `--acp` and a
   given `HERMES_RS_HOME`, sends lines from a JSON file, prints every line both ways with a
   timestamp, answers permission requests from the script. You'll use it in every phase.

7. **D1 check (once):** the release build is a GUI-subsystem exe (`windows_subsystem = "windows"`).
   Verify that stdio pipes still work: build release into `target/claude`
   (`cargo build -p hermes-desktop --release`, it is slow) and run the probe against it. If stdout
   is lost, fix it in `acp::run` (Win32 `GetStdHandle` should still return the inherited pipe; if
   not, report to the user before trying `AttachConsole` tricks) and write the finding in
   `docs/PITFALLS.md`.

**Tests:** `rpc.rs` unit tests (parse every message kind, bad JSON, `\r\n`, ids as numbers and
strings; `Sender::request` resolves on the matching response only); `tests/acp.rs`
`initialize_answers_version_1_and_capabilities`, `unknown_method_is_method_not_found`,
`stdout_carries_only_json_lines` (T-stdout), `closing_stdin_exits`.

**Docs:** `PORTING.md` rows for `acp/mod.rs`, `acp/rpc.rs`, `acp/server.rs`; `docs/ARCHITECTURE.md`
startup section: one paragraph on `--acp`; `docs/PITFALLS.md`: "stdout is the protocol in --acp".

**User checklist:** ZED.md "Phase 0" (agent appears, connects; new thread shows an error).

---

## Phase 1 — Chat: new session, prompt, streaming, cancel

✅ **Done 2026-11-10.**

**Goal:** in Zed, a new Hermes thread, type a message, the reply streams in (with thinking shown as
thoughts), Stop works, errors show as errors.

1. **Gateway access.** Make `Gateway::dispatch` `pub(crate)`. `AcpServer` owns one gateway
   `Outbox` (`mpsc::unbounded_channel::<String>()`) and passes `&outbox` to every `dispatch` call.
   Every session we create, load or prompt gets subscribed to it by the gateway itself
   (`create_session`/`resume_session`/`submit` call `subscribe`). Note why it matters: an event
   for a session nobody follows is dropped, and an approval nobody can see is **denied**
   (`hub.rs` `ask` returns `None`).

2. **Per-session state** (`server.rs`):
   ```rust
   struct AcpSession {
       cwd: String,
       turn: Option<TurnState>,          // Some while a prompt is running
   }
   struct TurnState {
       done: oneshot::Sender<Result<Value, RpcError>>,   // answers session/prompt
       translator: translate::Turn,                      // message ids, open tool calls, diffs
   }
   sessions: Mutex<HashMap<String, AcpSession>>
   ```
   (`std::sync::Mutex`, never held across `.await` — copy what you need out, then await.)

3. **`session/new`** (PROTOCOL §2): wait for the runtime (Phase 0 step 3), `dispatch("session.create", {"cwd": cwd})`,
   map an error to -32603 with the gateway's message (the "no model set up" text when that's the
   cause: read what `create_session` returns for it), insert `AcpSession`, answer `{sessionId}`.

4. **The event pump** (`server.rs`, one task started in `serve`): reads the outbox receiver,
   parses each frame, keeps only `method == "event"`, finds the session; if it has no running turn,
   ignore the event (log at debug); else feed it to `translate::Turn::event(...)`, which returns
   `Vec<Action>`:
   ```rust
   pub enum Action {
       Update(Value),                     // send as session/update for this session
       AskPermission(Value),              // Phase 2: spawn the approval/clarify task
       Finish(Result<Value, RpcError>),   // answer the prompt (stopReason or error)
   }
   ```
   Keep `translate.rs` **pure**: no IO, no gateway, no channels. The pump does the IO. That's what
   makes it unit-testable with recorded event frames.

5. **`translate::Turn`** for Phase 1: `message.start` → new `messageId`; `message.delta` →
   `agent_message_chunk`; `reasoning.delta` → `agent_thought_chunk`; `status.update` → thought;
   `message.complete` → `Finish` per PROTOCOL §2's table. Everything else → nothing (yet).
   Port the "think tags" and empty-text rules from `transcript.rs` (it already strips `<think>`).

6. **`session/prompt`**: text only in Phase 1 (`prompt.rs`: join `text` blocks; any other block
   type → -32602 "This agent doesn't accept <type> yet"). If the session has a running turn, wait
   for it to finish first. Create the `oneshot`, store `TurnState`, then
   `dispatch("prompt.submit", {"session_id", "text", "submission_id": uuid})`. If dispatch fails,
   remove the turn and answer the error. Otherwise await the oneshot and return its result.
   Read `submit` first: check whether a `gated` / admission flag changes when events start, and
   whether a queued prompt changes `message.complete` ordering.

7. **`session/cancel`**: `dispatch("session.interrupt", {"session_id"})`; nothing else. The turn
   ends through `message.complete` with `interrupted`.

8. **A turn that never ends.** If the gateway dies mid-turn (it shouldn't), the prompt would hang.
   Guard: when the outbox receiver closes, `Finish` every running turn with -32603.

**Tests (TESTING.md):** `translate.rs` unit tests over recorded frames (deltas in order, one
messageId per reply, empty deltas skipped, complete/interrupted/error → the three answers);
`tests/acp.rs`: `a_prompt_streams_the_reply_and_ends_the_turn`,
`thinking_streams_as_thought_chunks`, `cancel_ends_the_turn_with_cancelled`,
`a_provider_error_answers_an_error`, `no_provider_gives_a_readable_error`,
`two_sessions_stream_independently`.

**Docs:** PORTING rows (`translate.rs`, `prompt.rs`); GAPS row updated with what works.

**User checklist:** ZED.md "Phase 1".

---

## Phase 2 — Tools, diffs, approvals, clarify, plan

✅ **Done 2026-11-10.** As built: a `patch` card's diff comes from the tool's own result (one `diff`
item per hunk, like Hermes); a `write_file` card's "before" is read by the server when the call
starts (`Action::Snapshot`), which can lose a race with a very fast write, in which case the card
shows the file as new. `Gateway::call` (not `dispatch` directly) is the way in: it releases a
started turn's events, which otherwise wait for the page's receipt.

**Goal:** tool calls show as Zed tool cards with the right icon (kind), title and file location;
file edits show as diffs; dangerous commands ask in Zed; clarify questions can be answered;
the todo list shows as Zed's plan.

1. **`tools.rs`** (pure): `kind(name) -> &'static str`, `title(name, &args) -> String`,
   `locations(&args, cwd) -> Vec<Value>`, `failed(&result_text) -> bool`,
   `complete_content(name, &result_text) -> Vec<Value>`, `plan_from_todo(&result_text) -> Option<Value>`.
   Tables in PROTOCOL §3.1/3.3. **First** list our real tool names
   (`ls prompts/tools/`, `grep -rn "fn name" crates/tools/src`) and the JSON our `todo_list`,
   `write_file`, `patch` and `terminal` return (run them in a unit test or read their code), and
   write the tables from that, not from Hermes's names.

2. **translate**: `tool.start` → close the current `messageId`, `tool_call` (status
   `in_progress`); `tool.complete` → `tool_call_update` + `plan` for `todo_list`; at
   `message.complete`, every tool call still open → `tool_call_update {status: "failed"}` before
   `Finish` (Hermes `flush_open_tool_calls`).

3. **Diffs** (PROTOCOL §3.2): `translate` can't read files (it's pure), so it returns
   `Action::Snapshot { tool_id, path }` on `tool.start` of an edit tool and, on its
   `tool.complete`, `Action::Diff { tool_id, path, old }`; the pump reads the file
   (`spawn_blocking`) and sends the update. Keep the 1 MB / binary limits. Handle a `patch` that
   names several files (check its schema in `prompts/tools/`).

4. **Approvals** (`permissions.rs`, pure helpers + one async fn in `server.rs`): on
   `approval.request`, the pump spawns a task: build the request (PROTOCOL §4.1), `sender.request(
   "session/request_permission", …)`, map the outcome, `dispatch("approval.respond", {"prompt_id", "choice"})`,
   send the closing `tool_call_update`. **Spawned**, never awaited in the pump: the pump must keep
   reading while the user decides. Keep the task's `JoinHandle` by `prompt_id` so a `*.settled`
   event (or the turn ending) can abort it.

5. **Clarify** (PROTOCOL §4.2): same pattern. Remember `elicitation.form` from `initialize`. If
   `elicitation/create` errors with -32601 once, remember it and use the permission card from then
   on.

6. **Cancel while asking:** when the user presses Stop in Zed while a permission card is open,
   Zed answers it `cancelled` → we deny → the turn continues to its end and `session.interrupt`
   stops it. Check this in a test; it's the path most likely to hang.

**Tests:** `tools.rs` table tests (every kind, every title format, Windows locations,
failed detection); translate tests with tool frames (start → update ordering, open calls flushed,
plan entries); `tests/acp.rs`: `a_tool_call_shows_start_and_completion`,
`write_file_sends_a_diff`, `a_flagged_command_asks_permission_and_runs_when_allowed`,
`a_denied_command_does_not_run`, `a_cancelled_permission_denies`,
`clarify_with_form_elicitation`, `clarify_without_elicitation_uses_a_card`,
`todo_list_sends_the_plan`.

**User checklist:** ZED.md "Phase 2".

---

## Phase 3 — Sessions: history, list, titles, usage, commands, context

✅ **Done 2026-11-10.** As built: every command in `commands.catalog` is offered with an input hint; a command the gateway answers with output is shown as one message, a `send`/`skill` one is submitted as the turn's text, an unknown one goes to the model as typed. A thread nobody wrote in isn't listed.

**Goal:** past threads reopen with their history; Zed's thread history lists hermes sessions for
this folder; titles appear; the context-usage meter fills; `/` commands are offered and work;
`@`-mentioned files and pasted images reach the model.

1. **`session/load`** + `history.rs` (PROTOCOL §2 and "History replay"). Then advertise
   `loadSession: true`.
2. **`session/list`** (PROTOCOL §2), store access in `spawn_blocking`. Advertise
   `sessionCapabilities.list: {}`.
3. **Titles:** on `session.info` events, compare the title with the last one sent for the session;
   send `session_info_update {title, updatedAt}` when it changed. The gateway titles a new
   session after its first complete turn (`title_session`), so the event arrives *after*
   `message.complete`: the pump must keep routing `session.info` for sessions without a running
   turn (an exception to Phase 1 step 4).
4. **Usage:** after each turn, `usage_update {used, size}`: `used` from
   `dispatch("session.context_breakdown", …)` total (or `message.complete`'s `usage` if it has the
   context size — check `usage_snapshot`), `size` = the model's window (the same number `/status`
   shows). Before answering the prompt.
5. **Slash commands** (PROTOCOL §6).
6. **Context blocks** (`prompt.rs`, PROTOCOL "Prompt blocks"): `resource_link`, `resource`;
   advertise `embeddedContext: true`. Images: `image` blocks → `image.attach_bytes` (read its arm
   and `attachments.rs` for params; it may want base64 or bytes and a file name) before
   `prompt.submit`; advertise `image: true`. If the session's model can't take images the gateway
   already falls back to naming the file for `vision_analyze` (GAPS 1.2), nothing to do here.

**Tests:** `history.rs` and `prompt.rs` unit tests (Windows `file://` URIs with spaces and `%20`,
drive letters, a resource without a name); `tests/acp.rs`: `load_replays_history_before_answering`,
`load_of_unknown_session_is_not_found`, `list_filters_by_cwd_and_pages`,
`a_new_session_gets_a_title_update`, `usage_update_after_a_turn`,
`available_commands_after_new_session`, `a_slash_command_answers_without_the_model`,
`a_file_link_reaches_the_model_as_a_file_reference`, `an_image_is_sent_to_the_model`.

**User checklist:** ZED.md "Phase 3".

---

## Phase 4 — Model picker, effort, modes

✅ **Done 2026-11-10.** As built: the model list comes from `model.options` (bounded to 4 s, so an unreachable provider doesn't hold up opening a thread); the mode is the runtime's per-session YOLO flag, which lasts as long as the process (a reloaded thread starts in Default).

**Goal:** Zed's model picker lists the desktop's models and switches the thread's model; the
effort selector works; modes (Auto, Manual, Accept edits, Plan, YOLO: the session's `ApprovalMode` in `hermes_tools`) change what asks the user.

1. `configOptions` in `session/new`/`session/load` answers (PROTOCOL §5), from `model.options`.
2. `session/set_config_option` for `model` and `effort`, reusing the page's calls; answer the full
   list; `config_option_update` if the gateway changes them itself (e.g. a pending route applied).
3. `modes` + `session/set_mode` (PROTOCOL §5) over the runtime's per-session yolo.
4. Zed's settings `default_mode` / `default_config_options` (ZED.md) then work without more code:
   check them.

**Tests:** `tests/acp.rs`: `config_options_list_the_models`, `switching_model_routes_the_next_turn`
(the mock provider sees the new model name in the request body),
`switching_model_while_running_is_refused`, `yolo_mode_runs_flagged_commands_without_asking`.

**User checklist:** ZED.md "Phase 4".

---

## Phase 5 — Hardening and living alongside the desktop app

✅ **Done 2026-11-11** (the editor's MCP servers were dropped by the user: `mcpServers` in
`session/new` stays ignored). As built:

1. **Both apps at once.** Two processes (the desktop app, `hermes-acp.exe`) share the data folder
   (`sessions.db`, memories, skills, `config.json`). A thread made in one is in the other's list
   (`two_agent_processes_share_threads_and_continue_each_others`). What is and isn't shared is in
   PITFALLS ("Two windows on one data folder").
2. **The same thread in both.** `hermes_runtime::turnlock`: while a turn (or `/compress`) runs, its
   process holds `<data>/running/<session id>.lock` open with no sharing; a second process's
   `start_turn` fails with "This thread is running a turn in another Hermes window…", in the
   editor as the prompt's error, in the page as an error card. The OS drops the lock if the
   process dies. And the stale-cache bug under it: rows are keyed by position, so a window whose
   cached conversation was behind would overwrite the other's messages; `Runtime::reload_if_stale`
   (count + last message id against the store) reloads at turn start, `/compress`, `messages()`
   and `rewind()`.
3. **Subagents.** `subagent.start|tool|complete` page events become lines on the open
   `delegate_task` card (the whole log each time: an editor replaces a card's content), kept above
   the result when the call ends (`translate.rs` `subagent`).
4. ~~Editor MCP servers~~ dropped.
5. **Crash safety.** A request handler runs in its own task: a panic answers -32603 ("Hermes hit
   an internal error…") and the agent and the other threads carry on (`_test/panic` in debug
   builds proves it); panics are logged to `acp.log`. Child processes die with the agent: every
   shell's job object has KILL_ON_JOB_CLOSE (`crates/tools/src/job.rs`), even on a hard kill.
6. **Installer.** `hermes-acp.exe` ships next to the app (done with the console-exe fix).

---

## After the review (2026-11-11)

✅ Fixed: rewinds take the thread's lock; subagents inherit the mode; the Plan exception compares
resolved paths; Manual/Plan cards are once-or-deny; the effort shown is the effort sent; store
reads in the ACP server go through `spawn_blocking`; the subagent log keeps its last 100 lines; the
model list is refetched on every open; the flaky budget test (a background review's request) is
fixed. Added: `session/close`, `session/resume`, live terminal output into the terminal card.

---

## Phase 6 — t3code

✅ **Built 2026-11-11** (the user's checklist in [T3CODE.md](T3CODE.md) is open). t3code adds a
local ACP agent as an "ACP Registry" provider with source Local and a command path. Read from its
bundled server (`resources/server.asar`, `runtimeLayer-*.mjs`): it sends `protocolVersion: 2` and
speaks v1 to v1 agents; it reads models only from the `session/new` answer's `configOptions`; it
starts a fresh agent per thread and probes the agent with a test session every ~15 min. As built:
the model list persists in `<data>/acp-model-options.json` (loaded at start, rewritten when the
background refresh finds it changed); threads this connection made are deleted when closed or when
the editor leaves while still empty (`forget_if_empty`). Tests:
`a_fresh_agent_answers_with_the_model_list_an_earlier_one_fetched`,
`an_empty_thread_this_agent_made_is_deleted_when_closed`.

---

## Finishing each phase

- `cargo test -p hermes-desktop` (unit + `tests/acp.rs`), `cargo build --workspace`.
- `GAPS.md` row (section 5, "ACP") updated with ✅ and the date for what's done.
- `PORTING.md` rows for new modules, citing the Hermes files.
- This file: tick the phase (`✅ Done YYYY-MM-DD`) under its heading, plus anything you learned
  that changes later phases.
- Commit (CONVENTIONS.md format), then give the user the ZED.md checklist for the phase.
