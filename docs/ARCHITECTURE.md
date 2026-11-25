# Architecture

How a click in the window becomes a model request and a tool call, and where each piece lives.
Read [AGENTS.md](../AGENTS.md) first.

## The picture

```
┌─────────────── hermes-desktop.exe ────────────────────────────────────────────────┐
│                                                                                   │
│  main thread: Tauri ── WebView2 window ── loads http://127.0.0.1:PORT/            │
│                                   │  (Hermes's React app, patched + plugin)       │
│                                   │                                               │
│  tokio runtime (4 workers, "hermes-runtime")                                       │
│   server.rs (axum) ───────────────┼────────────────────────────────────────┐      │
│     GET /…            static files from the webapp dist (+ injected globals)│      │
│     /api/ws           WebSocket → Gateway::connection → handle → dispatch   │      │
│     /api/{*rest}      → Gateway::rest  (None → 404 "No such API endpoint")  │      │
│     /api/host-terminal, /api/fs/download, /api/files/*, file-upload, window │      │
│                                                                            ▼      │
│   gateway/    Gateway { core, hub, rest (the page's own state) }: Hermes's        │
│     │  JSON-RPC and REST shapes over typed calls; hub = CoreEvent → frames        │
│     ▼                                                                             │
│   hermes_core::Core  ── the application layer, no JSON, no Hermes field names:    │
│     │  sessions, turns and their queue, CoreEvents, questions, Settings,          │
│     │  providers, skills, staging, usage, git, files, terminal, import …          │
│     │  (the ACP agent goes through the gateway; the native UI will call it)       │
│     ▼                                                                             │
│   hermes_runtime::Runtime  ── sessions, turns, delegation, compression, MCP       │
│     ├─ hermes_agent (Goose's agent loop)  ── hermes_llm (OpenAI-compatible HTTP) ─┼──► model endpoint
│     ├─ hermes_tools (terminal, files, search, skills, web, vision, checkpoints…) │
│     ├─ hermes_memory (MEMORY.md / USER.md)                                        │
│     ├─ hermes_store (SQLite sessions.db)                                          │
│     └─ hermes_prompts (system prompt, tool schemas from prompts/)                 │
└───────────────────────────────────────────────────────────────────────────────────┘
```

Python analogy: `server.rs` is a FastAPI app, `Gateway` is the one big service object every route
handler holds (behind an `Arc`, i.e. a refcounted shared pointer) that turns Hermes's JSON into calls on
`hermes_core::Core` (the service layer: typed functions and events, no JSON) which drives `Runtime`, the
agent library. Everything runs in one process. The native UI (`crates/native`) will call `Core` the way
the gateway does, with no page and no JSON in between.

## Crates

| Crate | Package | Role | Notes |
|---|---|---|---|
| `crates/desktop` | `hermes-desktop` | The app: Tauri window, axum server, the protocol layer | The main app. Most feature work lands here. |
| `crates/core` | `hermes_core` | The application layer (native UI Phase 1): `Core` over the runtime, typed `CoreEvent`s, the questions (`CoreBridge`, the runtime's `UserBridge`), sessions, the side files, the app lock | CP1.1 moved sessions, questions, events, the transcript reading, projects, branch/archive/workspace here; CP1.2 the turn (`turns.rs`: submit, the queue, the turn's loop, titles), the `/` menu (`commands.rs`), attachments, `@` paths, files, `open_file.rs`, git reads and the review pane (`git.rs`, `git_review.rs`), the terminal's shells (`terminal.rs`), export, processes and subagents (`delegation.rs`); `crates/desktop` keeps the protocol's shapes. CP1.3 the Hermes features and Settings: the config record (`config.rs`), providers and custom endpoints (`providers.rs`, `endpoints.rs`), auxiliary models and the fallback chain (`aux_models.rs`, `fallback.rs`), Settings put on the runtime, the model picker, a turn's options, a model's window (`settings.rs`), usage and the request ledger (`usage.rs`), toolsets, skills, staging, the Star map, maintenance and the curator (events `CuratorStarted` / `CuratorFinished`), the Hermes importer, profiles. `crates/desktop` keeps only Hermes's JSON, statuses and RPC errors for them |
| `crates/runtime` | `hermes_runtime` | Sessions and turns over the agent: `Runtime` | `lib.rs` (API), `session.rs`, `session_actor.rs` (each session's actor: run slot, review slot, the page's state; Phase 13d), `agent_loop.rs` (the agent loop: passes over the checks, Stop, `Next`; Phase 13e), `ops.rs` (the loop's checks: retry and continuation rules, per-turn tool guards), `delegate.rs` + `subagents.rs`, `compress.rs`, `review.rs` (background review), `signals.rs` (learning signals: what each turn says is worth learning, scored locally into `signals/<id>.json`), `skill_outcomes.rs` (how the turns a skill was used in went, into `.usage.json`), `review_once.rs` (the evidence review: one request with a packet of signals, skills and memory; a JSON reply validated and applied or staged), `review_schedule.rs` (when it runs: per-session idle timers, switched-away/closed, one review at a time, back-off, hourly cap), `curator.rs` + `curator/` (the curator: the agent loop of `balanced`, and per-cluster proposals under `requests`), `mcp.rs`, `commands.rs` (`/diff`, `/usage`…), `guards.rs`, `changes.rs` |
| `crates/agent` | `hermes_agent` | The agent's steps (from Goose): stream a reply (`inference.rs`, with the fallback chain), run tool calls (`tool.rs`); the loop that drives them is the runtime's `agent_loop.rs` since Phase 13e | `events.rs` has `AgentEvent` |
| `crates/llm` | `hermes_llm` | Messages, the OpenAI-compatible provider, retries (from Goose) | `conversation::message::{Message, MessageContent}`; `observer.rs` is where every model POST is reported (purpose, session, provider) to the request ledger; `discovery.rs` lists an endpoint's `/models` and fills gaps from `canonical/` (a trimmed, compiled-in model catalog since Phase 14: see PITFALLS.md "The model catalog") |
| `crates/tools` | `hermes_tools` | Every agent tool + `SessionTools` (the per-session tool set) | One module per tool family; `paths.rs` resolves paths; `path_policy.rs` classifies where file edits and terminal commands may go (workspace, app data, skills/memory, persistence places, outside); `approval.rs` the dangerous-command check; `script.rs` + `script_guard.rs` the `run_script` tool (a script in PowerShell, Python through uv, or bash, run from a per-run scratch folder; `script_guard` reads the whole script for the approval, see PITFALLS.md "run_script"); `taint.rs` what the session has read that the user didn't write (see Taint in PITFALLS.md); `staging.rs` the writes a tainted session's `memory` / `skill_manage` calls leave for the user (`staging/<id>.json`, applied through the stores' own write paths); `index.rs` the local FTS5 index (`index.db`) over skills and memory, kept fresh by write hooks on `SkillStore` / `MemoryStore`; `sandbox.rs` the OS sandbox's spawn (`terminal.sandbox = low`: a low-integrity token, every agent command in PowerShell) and `sandbox_labels.rs` the folders it may write to (workspace, scratch, `sandbox-cache`, grants: labelled Low, recorded in `sandbox-labels.json`, undone by `/sandbox remove-labels`; see PITFALLS.md "The shell sandbox") |
| `crates/memory` | `hermes_memory` | The memory store and tool (`§`-separated entries, char limits, drift guard) | `store.rs` (the API every caller uses), `tool.rs`, `threats.rs`; since Phase 9: `db.rs` (`memory.db`: entries with tier/scope/kind/links; the agent's removals kept as marked rows, the user's erased), `tiers.rs` (CP9.2: the `project` tier per git root and the `archive` tier, tags/links/notes validation, the project block of the system prompt, `mutate_tier`), `notes.rs` (long-form notes in `memories/notes/`), `similar.rs` (dedupe on add), `retrieval.rs` (CP9.3: what the index, the map and the curator read: indexable rows, the generation, batched hits, hygiene candidates, `demote_core`) and `sync.rs` (the files as the exported view of the database: migration with a backup copy, re-import of manual edits, the write step under the database's lock, fallback to the files alone) |
| `crates/store` | `hermes_store` | SQLite: sessions, messages, usage, projects | **Schema frozen** (shared with GPUI). `writer.rs`: one writer thread owns the write connection and commits in batches; reads use their own connection (see "The store's writer" below) |
| `crates/prompts` | `hermes_prompts` | Builds the system prompt; tool schemas; coding-command and side-question prompts | Text comes from `prompts/` |
| `crates/native` | `hermes-native` | The native UI on GPUI Kit ([docs/native-ui/](native-ui/README.md)), being built (v0.5) | CP5.3 adds the approval and question panel (`thread/composer/ask.rs`, `panel.rs`; `Backend::answer`; `AppModel` keeps every thread's requests, the sidebar's Approval / Input marks follow them) and `notify.rs` (taskbar flash + Windows toast of a waiting thread, `notifications` in `native-ui.json`); CP5.2 adds mentions, commands and attachments to the composer (`mention.rs`, `suggest.rs`, `attach.rs`, `strip.rs`); CP5.1: the composer (`thread/composer/`: `ComposerModel` over `Backend::controls` / `send` / `stop` / `queue_op` / `pick` / `model_choices` / `create_thread`, `NativeHooks` applies a model picked mid-turn between turns; `ComposerView` the card, queue strip and menus; drafts per thread in `native-ui.json`); CP4.3: the thread header in the title bar (`thread/header.rs` view over `ThreadModel` + `AppModel`: project, editable title, git chip, model chip, the actions menu; `header_data.rs` the pure parts; `Backend::header_info` / `export`; `dev_send.rs`: `--dev --dev-send`, the app drives one turn for the live check `scripts/perf/native-thread-check.ps1`); CP4.2: the work log (`thread/tools.rs` tool cards from the call's args and result, `ansi.rs`, `diff.rs`, `work.rs` groups of calls and the turn footer, `cards.rs` their rows; Retry and Branch through `Backend::retry` / `branch`); CP4.1: the thread view (`thread/`: `model.rs` `ThreadModel` = the open thread's rows, history pages and streaming turn; `view.rs` the virtual list; `markdown.rs` GPUI Kit's markdown with our code header, alerts, sideways tables and file links; `data.rs`; `recording.rs` + `fixtures/thread-session.json`, a turn recorded through the real core), history read by `Backend::history` from the store, the core's events forwarded by `AppModel` to the open thread, `--dev` sends a prompt (`Backend::submit` = `Core::submit`); Phase 3: the sidebar (`sidebar/`: `data.rs` the list logic, `view.rs`/`render.rs`, `empty.rs`) over the app model (`model/`: `AppModel` + the `Backend` trait with `CoreBackend` over `hermes_core::Core` and `FixtureBackend`; `app.rs` takes `<data>/app.lock`, starts tokio and `Core::start(true)`); Phase 2: the app frame (`app.rs` starts it; `workspace.rs` the regions, `titlebar.rs`, `palette.rs`, `keymap.rs`, `ui_state.rs` = `native-ui.json`), the design layer (`design/`: tokens and components) and themes (`theme/`); Phase 0: `spike.rs` (the CP0.1 window, `hermes-native --spike`), `gallery/` (stories, binary `hermes-native-gallery`, snapshot tests under `--features snapshots`), `dev/` (`.probe()`, the element dump), `testing.rs` (layout checks); checked by `scripts/native/check.ps1` (TESTING.md §6). No protocol: it will call `hermes_core` in-process |
| `crates/app` | — | The frozen GPUI app | Keep it compiling |
| `crates/markdown`, `highlight`, `themes` | — | Used by the GPUI app only | |

## Startup (`crates/desktop/src/main.rs`)

1. `RuntimeConfig::for_current_user()` picks the data folder (`HERMES_RS_HOME`, else
   `%APPDATA%\hermes-rs`) and starts file logging (`logs/desktop.log`).
2. `hermes_core::AppLock` takes `<data>/app.lock`; when another app holds it, a dialog says
   "Hermes is already open" and the process exits. A tokio runtime with 4 workers is built; Tauri
   keeps the main thread.
3. The port: `HERMES_DESKTOP_PORT`, else a fixed default (the page's localStorage is per origin,
   so the port must stay the same between runs), else any free port.
4. In parallel with the window: `hermes_core::Core::new` (`Runtime::new`: store, skills seeding,
   tools) inside `spawn_blocking`, then `Gateway::new`, then `server::serve`. The page's first requests wait in
   the listener's queue until it serves.
5. Tauri makes the window (no decorations; Hermes draws caption buttons, which call
   `/api/hermes-rs/window`), sets its icons per DPI (`set_window_icons`), disables Tauri's own
   drag-drop handler (so HTML5 drops reach the page), and loads the URL.
6. `webapp_dist()`: `HERMES_UI_DIST`, else `<exe dir>/webapp/` (installed), else
   `target/hermes-src/apps/desktop/dist-webapp` (developer build).

## Serving the page (`server.rs`)

- `static_file` serves the dist and injects into `index.html` the globals Hermes's own Python
  server injects (`window.__HERMES_…__`, the session token), plus `hide.css`. Keep that list in
  sync with `hermes_cli/web_server_dashboard.py` when Hermes changes.
- The built-in plugin `plugins/hermes-rs/plugin.js` is written to
  `<data>/desktop-plugins/hermes-rs/plugin.js` on start; Hermes's plugin loader picks it up.
- A middleware (`only_this_app`) refuses any request whose `Origin`/`Host` isn't this app's own
  loopback origin: a website open in the user's browser must not reach the gateway (it could run
  commands) and DNS rebinding is refused. Keep it in front of every route you add.
- Dedicated axum routes exist only where JSON-in/JSON-out doesn't fit: the WebSocket, the PTY
  socket (`/api/host-terminal`), file bytes with ranges (`tower_http::ServeFile`), multipart upload,
  the window bridge. Everything else goes to `Gateway::rest`.

## The protocol

Two channels, both shaped by Hermes; `target/hermes-src/apps/shared/src/gateway-contract.openrpc.json`
is the contract (257 methods), `apps/desktop/src/api/` is how the page calls them.

### JSON-RPC over `/api/ws` (`gateway/`)

`gateway/` is a directory module (Phase 13 CP13f): `mod.rs` has `Gateway`, the socket and
`dispatch`; `rpc.rs` has the typed requests and errors; one file per area has the handlers
(`sessions.rs`, `turn.rs`, `slash.rs`, `settings.rs`, `projects.rs`, `attachments.rs`,
`delegation.rs`, `usage.rs`, `rest.rs`: the table is at the top of `mod.rs`). All handlers are
`impl Gateway` blocks (Python: methods of one class spread over several files).

- `connection` reads frames; `handle` spawns each request; `dispatch(method, params, outbox)` parses
  the request with `Request::parse` and `match`es the typed `Request` (one variant per served
  method, `#[serde(tag = "method", content = "params")]`), each arm one call to a handler returning
  `Reply = Result<Value, RpcError>`. `Gateway::call` (the ACP server's way in) answers the same
  but with `(code, message)` as the error.
- **Params are lenient on purpose.** The page sends loose JSON and every handler always read it as
  `params["f"].as_str()...`: a missing field, `null` params or a field of the wrong type was never an
  error by itself. `rpc.rs` keeps that: each method's params type is built by `FromParams` (the same
  reads, in one place), never by serde's struct deserializer (it rejects `null` and reads an array
  positionally). What a method requires it checks itself (`SessionRef::id()` is "session_id is
  required"). A handler that passes the params on to a long function reads `Raw`, the value as sent.
- Errors are `RpcError`: `MethodNotFound(name)` -32601 (we don't serve it: page hides the feature),
  `InvalidParams(text)` -32602 (bad/missing params: `p.id()?` / `RpcError::invalid(..)`),
  `Server(text)` -32000 (it failed: message shown to the user, so write it for a human;
  `RpcError::server(..)`, `RpcError::failed(e)` for an `anyhow` chain), `Busy` (-32000 `session
  busy` with `data.reason = "session_busy"`). `to_json()` is the `error` object the page gets.
- Unknown methods: `Request::parse` fails for any name that is not exactly a variant (other case,
  spaces, empty included), `handle` logs it under target `hermes_desktop::missing` and answers -32601
  `Method not found: <name>`. That log is the live to-do list.
- **Adding a method:** a variant in `rpc::Request` (`#[serde(rename = "a.b")]`, its params type
  next to the others, with the old-style lenient reads) and its name in `rpc::SERVED` (the tests
  check the two lists agree), a handler in the area's file (`pub(super)`), one arm in `dispatch`,
  then `HERMES_RPC_TABLE_BLESS=1 cargo test -p hermes-desktop --lib rpc_table` and read the diff of
  `crates/desktop/tests/fixtures/rpc_table.txt` (the table of what every method answers).
- **Events** go out as `{"method":"event","params":{"type":…,"session_id":…,"payload":…}}` via
  `self.emit(session_id, event, payload)`. Delivery goes through `Hub` (`hub.rs`): each socket
  subscribes to the sessions it opened (`subscribe`), so a session's events reach whichever
  window shows it. Since native UI CP1.2 the turn runs in `hermes_core::turns` (`Core::submit`):
  its stream (`stream.rs`: text, reasoning, tool calls, status lines), `TurnStarted`, subagent
  rows, titles and the questions are `CoreEvent`s on the core's bus, and the `Hub` is a listener
  on it that sends each as the page's frame (`message.start`, `message.delta`, `reasoning.delta`,
  `tool.start`, `tool.progress`, `tool.complete`, `status.update`, `subagent.*`,
  `session.title`, `approval.request`, `clarify.request`, `*.settled`) in the publisher's thread,
  so they keep their order with the gateway's own. The frames that need the store
  (`session.info` with the queue panel, `session.usage`, `message.complete`) come from the
  gateway's `TurnHooks` (`gateway/turn.rs` `PageTurn`), which the core's loop calls on a
  blocking thread at those points. `btw.complete` and `background.complete` are the gateway's.
- Bigger features live in their own module and `dispatch` has a one-line arm calling it
  (e.g. `Request::SessionBranch(..) => self.session_branch(...)`, `crate::git_review::route`).

### REST under `/api` (`Gateway::rest`)

- Signature: `rest(method, path, query, body) -> Option<(u16, Value)>`. It is a chain of
  `if path == … / strip_prefix(…)` blocks, each returning `Some((status, json))`; falling through
  ends in `self.rest.handle(...)` (`rest.rs`: config, ui-preferences, startup routes) and then
  `None` → 404 (`server.rs` `api` logs it under `hermes_desktop::missing`).
- Error bodies are `{"detail": "…"}` (FastAPI's shape; the page shows `detail`).
- A prefix block delegates to its module: `/api/providers/custom-endpoints*` → `endpoints.rs`,
  `/api/fs/*` and `/api/files/*` → `files.rs`, `/api/git/*` → `git_review::route`,
  `/api/tools/*` → `toolsets.rs`, `/api/learning/*` → `learning.rs`, `/api/skills*` → `skills.rs`,
  `/api/staging*` → `staging.rs` (ours), `/api/sessions/{id}` → `session_route`, `/api/sessions/{id}/messages` → `stored_messages`.

## ACP mode (`--acp`, `crates/desktop/src/acp/`)

`hermes-acp.exe` (a console program in the installer; `hermes-desktop.exe --acp` is the same, for
debug builds) is the agent for an editor (Zed): no window, no web server, JSON-RPC
lines on stdin/stdout (Agent Client Protocol). It builds the same `Runtime` and `Gateway` as the
app (`Gateway::new_for_acp`: no curator, no auto-archive) and is a *client of that gateway* in its
own process: `Gateway::call(method, params, outbox)` is the page's socket without the socket, and
the page events that arrive on the outbox (`message.delta`, `tool.start`, `approval.request`, …)
become `session/update`s. Plan and reference: [acp/README.md](acp/README.md),
[acp/PROTOCOL.md](acp/PROTOCOL.md).

| Module | Role |
|---|---|
| `mod.rs` | `run()`: logging to `logs\acp.log` + stderr (**stdout is the protocol**), runtime, gateway, serve stdio |
| `rpc.rs` | Line framing, requests both ways (`Sender::request` waits for the editor's answer), one writer |
| `server.rs` | `AcpServer`: the methods, per-session state, the event pump, approval/clarify tasks |
| `translate.rs` | One turn's events → updates (pure) |
| `tools.rs`, `history.rs`, `prompt.rs`, `permissions.rs`, `controls.rs` | Pure mappings: tool cards and diffs; stored rows → replay; ACP content blocks → text and images; approvals and questions; model/effort/mode options |

It shares `%APPDATA%\hermes-rs` with the app, which may run at the same time (SQLite is in WAL
mode): a thread started in Zed is in the app's sidebar. Approval modes (`hermes_tools::ApprovalMode`, per session) decide what asks the user; the runtime's
`turnlock` and `reload_if_stale` keep two windows on one data folder from running or overwriting
one thread (PITFALLS: "Two windows"). Tests: `crates/desktop/tests/acp.rs`
(the real exe on pipes against a scripted model), probe: `scripts/acp/acp-probe.mjs`.

## Feature map: screen → code → check

Where to look first for each thing the user sees. (`gw` = a handler in `gateway/`.)

| Screen / command | Backend | Calls | Live check |
|---|---|---|---|
| Chat: send, stream, stop, queue, steer | gw `submit`, `transcript.rs`, runtime `start_turn` | `prompt.submit`, `session.interrupt`, `session.steer`, `prompt.*_queued` | `smoke.mjs`, `stop-check.mjs`, `provider-error-check.mjs` |
| Approvals, clarify | `hermes_core` `questions.rs` (`CoreBridge`), `hub.rs` (the cards' JSON), `hermes_tools::approval` | `approval.respond`, `clarify.respond` | `smoke.mjs` |
| Sidebar: sessions, search, pin, rename, delete | gw `session_list`, `session_route` | `/api/profiles/sessions`, `/api/sessions/*` | — |
| Artifacts (sidebar), `file:` links | page over `stored_messages`; `hermes_core` `open_file.rs` (patch 0013) | `/api/sessions/{id}/messages`, `/api/hermes-rs/open-file` | `artifacts-check.mjs` |
| Archive, Settings ▸ Sessions | `hermes_core` `archive.rs`, `sessions.rs`, `settings.rs` (`sweep_archive`), gw `set_archived` | PATCH `archived`, `session.mutate archive`, `?archived=` | `archive-check.mjs` |
| Projects view | `hermes_core` `projects.rs` (grouping), `projects.rs` (JSON) | `projects.tree`, `projects.*` | — |
| `/` menu, skills commands | `hermes_core` `commands.rs`, `commands.rs` (JSON), gw `slash.rs` | `commands.catalog`, `complete.slash`, `slash.exec`, `command.dispatch` | — |
| Built-in commands (`/status`, `/compress`, `/diff`…) | `hermes_core` `commands.rs` (`Core::run_command`), runtime `commands.rs` | `slash.exec` | — |
| `/plan`, `/init`, `/review`, `/learn`, `/import-hermes` | `hermes_core` `commands.rs` (`coding_command`, `review_task`, `CoreCommands`), `hermes_import.rs` | `slash.exec` | `coding-commands-check.mjs` |
| `/branch`, `/btw`, `/bg` | `hermes_core` `branch.rs`, `sessions.rs`, runtime `side_question`/`background` | `session.branch*`, `prompt.btw`, `prompt.background` | `side-and-branch-check.mjs` |
| `/undo`, `/retry`, restore, `/rollback` | gw `rewind_turn`, `rollback`, `hermes_tools::checkpoints` | `session.rewind`, `session.mutate`, `rollback.*` | `rollback-check.mjs` |
| `/save` | `hermes_core` `export.rs`, `export.rs` (JSON) | `session.export` | — |
| `@` completions | `hermes_core` `paths.rs`, `paths.rs` (JSON) | `complete.path` | — |
| Attachments | `hermes_core` `attachments.rs`, `attachments.rs` (JSON), `server.rs` upload | `/api/chat/image-upload`, `/api/chat/file-upload`, `image.attach`, `file.attach` | `attach-check.mjs` |
| Side panel: file preview/edit | `hermes_core` `files.rs`, `files.rs` (JSON), `server.rs` `file_bytes` | `/api/fs/*`, `/api/files/*` | `files-check.mjs` |
| Side panel: git review, coding rail | `hermes_core` `git_review.rs`, `git.rs`; `git_review.rs`, `git.rs` (routes, JSON) | `/api/git/*` | `git-check.mjs` |
| Side panel: terminal | `hermes_core` `terminal.rs` (shells), `terminal.rs` (socket), `server.rs` | `/api/host-terminal` (PTY socket) | `terminal-check.mjs` |
| Subagents | runtime `delegate.rs`, `subagents.rs`; gw `subagent.*` | `subagent.*`, `delegation.*` | `subagents-check.mjs` |
| Model picker, effort | `hermes_core` `settings.rs` (`model_options`, `switch_model`, `pick_effort`), `providers.rs`; gw `settings.rs` (JSON) | `model.options`, `config.set {key: "model"}`, `session.mutate` | `switch.mjs` |
| Settings ▸ Model ▸ Auxiliary | `hermes_core` `aux_models.rs`, `settings.rs` (`turn_options`); `aux_models.rs` (JSON) | `/api/model/auxiliary`, `/api/model/set {scope: auxiliary}`, `/api/model/auxiliary/local` (ours: "Use a local model for background jobs", patch 0023) | `aux-models-check.mjs`, `local-aux-check.mjs` |
| Settings ▸ Providers ▸ Custom endpoints | `hermes_core` `endpoints.rs`, `providers.rs`; `endpoints.rs` (JSON) | `/api/providers/custom-endpoints*` | `endpoints-check.mjs` |
| Settings (all other pages) | `hermes_core` `config.rs` (the record, its defaults), `settings.rs` (`apply_settings`); `rest.rs` (`config_schema`, the routes) | `/api/config*` | — |
| Settings ▸ Memory, the Star map | `hermes_core` `learning.rs`; `learning.rs` (JSON); `hermes_memory` | `/api/learning/*`, `/api/memory*` | `memory-check.mjs` |
| Capabilities ▸ Skills | `hermes_core` `skills.rs`; `skills.rs` (routes, JSON) | `/api/skills*` | — |
| Writes waiting for review (tainted sessions, or write approval on): "Waiting for your review" on Settings ▸ Memory and Capabilities ▸ Skills (patches 0024-0026) | `hermes_core` `staging.rs`, `staging.rs` (JSON), `hermes_tools::staging` | `staging.list/apply/reject` (ours), `/api/staging*` | — |
| Capabilities ▸ Tools | `hermes_core` `toolsets.rs`, runtime `set_disabled_tools`; `toolsets.rs` (JSON) | `/api/tools/*` | `toolsets-check.mjs` |
| Command Center ▸ Maintenance, logs, insights | `hermes_core` `maintenance.rs` (curator, memory files), `usage.rs` (`insights`); `maintenance.rs` (JSON), gw | `/api/curator*`, `/api/logs`, `insights.get` | — |
| Import Hermes sessions | `hermes_core` `hermes_import.rs`; `hermes_import.rs` (JSON) | `session.foreign.*`, `/import-hermes` | `import-check.mjs` |
| Shell sandbox (`terminal.sandbox`, config only) | tools `sandbox.rs`, `sandbox_labels.rs`; `hermes_core` `commands.rs` `sandbox_text` | `/sandbox`, `/sandbox remove-labels` (`slash.exec`) | — (unit tests; the `slash.exec` answer's shape is `/import-hermes`'s) |
| Profiles window, SOUL.md editor | `hermes_core` `profiles.rs`; `profiles.rs` (JSON) | `/api/profiles/{name}/soul`, `/api/profiles` | `profiles-check.mjs` |
| Window caption buttons, zoom, dialogs | `server.rs` `window_control`, `main.rs` | `/api/hermes-rs/window` | — |

## A turn, end to end

1. Page: `prompt.submit {session_id, text, …}` (or `session.create` first).
2. `Gateway::prompt_submit` turns the params into a `SubmitTurn` and calls `Core::submit`
   (`hermes_core::turns`) with its `PageTurn` hooks: the core queues it if a turn is running, else
   cuts the conversation for an edit, sets a provisional title, resolves the provider (the hooks'
   `settings`, from Settings), attachments (`hermes_core::attachments`), the `TurnOptions` (the
   hooks' `options` = `Core::turn_options`: the model's known window, the session's effort, the
   side jobs' models — use it for every runtime call that takes options), then
   `runtime.start_turn(...)` (or `start_turn_with_images`, `start_turn_with_call` for `/review`'s
   scripted first tool call).
3. `Runtime::start_turn` (runtime `lib.rs`): loads the session from the store, builds the system
   prompt once per session (`hermes_prompts`, kept for caching), builds `SessionTools` minus
   disabled toolsets, wraps them in the per-turn ops (`ops.rs`: `GuardedTools` checks paths,
   approvals and disabled tools; `UnhandledToolCalls` answers unknown/disabled tool names;
   `DeliverSteer` injects `/steer` text), and runs the agent loop (`agent_loop.rs`, below) on a
   task. Returns a `Turn` with an `events` channel and a cancel handle.
4. The agent streams from `hermes_llm`'s OpenAI-compatible provider, runs tool calls
   (`hermes_tools`), queues each message for the store's writer (committed in ~5 ms batches, and
   flushed before `TurnFinished`), and emits `AgentEvent`s.
5. The core's loop (`turns.rs` `TurnRun`) publishes those as `CoreEvent`s (the hub sends the
   page's frames), calls the hooks for generation stats (`note_generation`) and `session.usage`,
   and on `TurnFinished` publishes `TurnEnded` (the hooks send `message.complete`), runs the next
   queued prompt, titles a new session (the aux "titles" model), and the background review may run. Just before
   `TurnFinished` the runtime scores the turn's learning signals (`signals.rs`: corrections,
   denials, workarounds, long turns, explicit requests; no request) and hands them to the signal
   store's writer thread; a rewind or `/undo` adds one too. The same step writes the outcome of
   every skill the turn used (attached by the index, or loaded with `skill_view`) into the skill's
   record (`skill_outcomes.rs`, on a blocking thread), and a correction in the message makes the
   previous turn's uses `corrected`.
   Under `budget.review_mode = idle_once` (the `requests` profile) the turn's end arms the
   session's idle timer (`review_schedule.rs`); a quiet session with signals at the threshold, or
   one the page switched away from, gets one evidence review. Under `nudges` the counters or the
   score start the agentic review (`review.rs`) right after the turn.
6. Approvals and clarify questions: the runtime calls its `UserBridge` (`hermes_core::questions::CoreBridge`,
   which publishes them as `CoreEvent`s the gateway's `Hub` sends as the page's cards),
   which emits `approval.request`/`clarify.request` and waits for the page's
   `approval.respond`/`clarify.respond`.

Delegation: the `delegate_task` tool goes to `Delegator` (`runtime/src/delegate.rs`), which runs
child agents (at most the budget profile's `delegate_max_children`), registers them in `Subagents` (`subagents.rs`) for
`subagent.list/tail/steer/interrupt`, and relays their progress as `AgentEvent::Subagent{kind,
payload}` → `subagent.*` events. `/bg` (`Runtime::background`) is a detached child; `/btw`
(`Runtime::side_question`) is one tool-less request over a snapshot.

### The conversation is a list of shared messages (Phase 13a)

`hermes_llm::conversation::Conversation` is `Vec<Arc<Message>>` (Python: a list of references, so a
copy of the list is a shallow copy; C++: a `vector<shared_ptr<const Message>>`). The live
conversation of an `OpenSession` (`conversation: Mutex<Conversation>`) and everything derived from it
share the same messages:

- `OpenSession::snapshot()` (what each pass of the agent loop sees), a review's or the compressor's view,
  `HistoryReplaced` events and `Conversation::clone()` copy pointers only.
- Changing a message goes through `Arc::make_mut`: copy-on-write. `Conversation::push` (a streamed
  chunk merging into the last message), `TurnRuntime::update_message`, `ConversationEffect::
  ensure_message_ids`, `compress::undo` and the repair passes of `fix_conversation` copy a message only
  if a snapshot or projection still holds it; a message nobody else holds is changed in place. A
  snapshot never sees a later change.
- The request is built as `Vec<Arc<Message>>` (`inference.rs`: `request_base` -> `messages_for_provider`
  -> `finalize_request`; `applies` builds the same pointer list once more to decide whether the model is
  next, without copying a message); a message with nothing to hide, repair or merge is the
  conversation's own `Arc` all the way to `Provider::stream(.., &[Arc<Message>])`, where the OpenAI
  formatter turns it into JSON (the one copy the wire needs). `Message::has_content_hidden_from_agent`
  decides whether `agent_visible_content` would change anything.
- Plain `Vec<Message>` is still what the store loads, what the page's transcript functions take
  (`Runtime::messages`, `rewind`, `/diff`... return `Conversation::to_vec()`, a deep copy: these run
  on a user's click, not per request) and what a one-message request is built from.
- `messages()` is `&[Arc<Message>]` and `messages_mut()` is `&mut Vec<Arc<Message>>`: never assign through
  an element (`make_mut` it), or a snapshot would see the change. Helpers over a conversation's list take
  `&[Arc<Message>]` (`hermes_llm::conversation::arcs(vec)` wraps plain messages, mostly for tests);
  `Store::replace_messages` and the OpenAI formatter are generic over `Borrow<Message>`.

### The store's writer (Phase 13b)

`hermes_store::Store` has two connections to `sessions.db`. The **write** connection belongs to
one thread (`crates/store/src/writer.rs`, "hermes-store-writer"; C++: a worker thread with a
message queue owning the connection); every write is a job on its channel, run in the order sent.
The **read** connection is separate (`query_only`; WAL lets it read while the writer writes). An
in-memory store (tests) has one connection that both use.

- **Queued writes** (`Store::queue(StoreOp)`): the turn path. `TurnRuntime::append`,
  `update_message`, `drop_trailing_errors`, a replaced history, compaction and token usage queue
  their write and go on (`session.rs`). `Err` from `queue` is an *earlier* queued write of that
  session that failed: the turn stops there, as when writes were synchronous.
- **Waiting writes**: every other `Store` write method (`create_session`, `set_title`,
  `replace_messages`, `delete_session`, projects...) is a job too, and returns once committed with
  its own result. So it never overtakes a queued write, and the synchronous API the gateway and
  the GPUI app use is unchanged.
- **Batches**: from the first job, the writer keeps collecting for `BATCH_WINDOW` (5 ms), up to
  `MAX_BATCH` (256), and stops early at a waiting write or a flush. One `BEGIN IMMEDIATE`; each
  job in a savepoint (a failing job is rolled back alone and reported, the rest commit); a failed
  `COMMIT` is retried job by job. A queued upsert of a message that a later job of the batch
  upserts again (a streamed reply) is skipped.
- **Reads see every write sent before them**: each read method calls `Writer::settle` first,
  which asks the writer to commit now and waits (no wait when nothing is pending). No caller
  needs to remember a flush for read-after-write in this process.
- **Flush and errors**: `flush()` / `flush_session(id)` commit and return the queued failures
  (each reported once). `Runtime` calls `flush_turn` before every `TurnFinished` (a turn, the
  early-out path, `/compress`, a panicked turn) and `after_save` turns a failure into
  `TurnStatus::Failed("... not fully saved ...")`. Failures are also logged at `error`.
- **Exit**: `Store::shutdown()` (and `Drop`) drains the queue and joins the thread; the desktop app
  calls it on `RunEvent::Exit`, ACP when stdin closes. A killed process loses at most the batch
  not yet committed (~5 ms); never a finished turn. After shutdown, writes run inline.
- Other processes (the GPUI app, a second `Store` on the file) see committed rows only: a flush
  (or a turn's end) is what makes queued writes visible to them.

### The session actor (Phase 13d)

Each session's control state belongs to one thread, its actor (`crates/runtime/src/session_actor.rs`; C++: a
worker thread with a message queue owning its data; Python: a thread reading a `queue.Queue`). Everything else holds
a `SessionHandle` (from `Runtime`'s `SessionRegistry`, the one map by session id) and sends it `Command`s, handled
one at a time in the order sent:

- **What it owns**: which `OpenSession` is loaded (`Attach` / `Detach`), the **run slot** (`ClaimRun` /
  `Release`, with the turn's cancel token and steer box), the agentic review's slot (`ClaimReview`,
  `ReleaseReview`, the paused review's `IdleEpoch` / `ConfirmIdle`), the review counters, and the page's state
  for the gateway (`PageState`: queued prompts, live, the model picked mid-turn, the effort pick, the revision,
  images held for the next prompt). The data a turn works on (`record`, `conversation`, `context`) stays on
  `OpenSession` under its mutexes (the turn is its only writer while it holds the run slot).
- **Reads** go to the published `SessionStatus` (swapped whole after each command, before its answer): no round
  trip. **Writes and check-then-act** are commands, always waited for (`ask` from sync code, `ask_async` from
  async code).
- **`RunGuard`** (RAII) is the turn's claim: dropping it frees the slot and the cross-process `TurnLock` on every
  path. **Stop** cancels the published token directly, never behind the queue.
- **Never blocks**: handlers do in-memory work only (no IO, no awaits, no call to a handle), so a synchronous wait
  is bounded like a mutex; that is also why the actor is a thread and not a task (synchronous `start_turn` from a
  one-thread runtime). A stopped actor or a panicking handler answers `Gone`, never hangs.
- Lifecycle: started on first use (no IO), kept across a close or a workspace move, removed on delete; it ends when
  its last handle (a turn or a review still ending) lets go. Rules for new code: PITFALLS "The session actor".

### The agent loop (Phase 13e)

`crates/runtime/src/agent_loop.rs` runs a turn, a `delegate_task` child, the agentic review and the curator.
It replaced Goose's `StateMachine` (a `Vec` of `Step` trait objects) with the same rules written out; Python:
a `while True:` over a fixed list of `if`s.

- **Passes.** Each pass takes a snapshot (`TurnRuntime::load`, which also takes a one-shot output cap a retry
  set), then asks the checks in a fixed order; the first that acts ends the pass, its effects are applied
  (`TurnRuntime::apply_effects`: memory, the store's queue, events), and its `Next` decides:
  `Continue` / `Retry(reason)` start another pass, `Done` ends the run; a pass where nothing acts ends it too.
  Order: iteration budget, validate tool calls, run tools, unanswered calls, `/steer`, length continuation,
  empty response, provider error, compression, inference (`AgentLoop::check`). One pass is one action, and
  every pass starts again at the top (after tools ran the budget is asked before the next request).
- **Checks** are plain methods (`ops.rs`, `AutoCompress::check` in `compress.rs`) returning
  `Checked = Result<Option<Outcome>>`: `None` = nothing to do here; else effects plus `Next`. `Reason` names
  every retry rule (`TruncatedToolCall`, `InvalidToolName`, `InvalidJson`, `LengthContinuation`,
  `EmptyAfterTools`, `ThinkingOnly`, `IterationLimit`, `EmptySummary`...); counters live in `TurnState`.
  The agent crate's `ToolOperation` and `InferenceRunner` keep their Goose `Operation` / `Inference` traits; the
  loop maps their result (`Applied` + `Continuing` -> `Continue`). The inference step's own empty-reply retries
  and the fallback chain stay inside `InferenceRunner` (one model call's business).
- **Stop.** The token is checked before each check, and each check is raced against Stop with `biased`
  priority for the check: one that finishes in the poll that sees Stop keeps its result (tools rely on it), one
  still waiting is dropped. `finalize` then saves the interrupted step's leftovers (the inference step's
  streamed text and usage), answers the turn's unanswered tool calls ("Tool call was interrupted before
  completing"), then the other steps' leftovers. An error from a check ends the run without that, unless Stop
  came first.
- A review and the curator have no `/steer` and no compression (`steer: None`, `compress: None`), as before.

## Settings and data

Two settings files, with different owners — don't mix them up:

| File | Read/written by | Holds |
|---|---|---|
| `config.json` | `providers::Settings::load/save`, `aux_models.rs` | providers (custom endpoints), the default model, the side jobs' models (`aux_models`), shared with GPUI |
| `desktop-config.json` | `hermes_core` `config.rs` (`ConfigRecord::value("a.b")`, `set_value`; `rest.rs` and the gateway reach it as `config_value` / `set_config_value`) | everything on Hermes's Settings page (Hermes's `config.yaml` keys, dotted); `fallback_providers` is the fallback chain (read per turn by `fallback.rs`) |
| `ui-preferences.json` | `rest.rs` | the page's interface preferences |

`Core::apply_settings()` (`hermes_core` `settings.rs`) pushes `desktop-config.json` values into the
runtime (approvals mode, checkpoints, memory limits, disabled toolsets, budget knobs). Call it (from the
gateway: `Gateway::apply_settings()`, which also refreshes the followed sessions' `session.info`) after any
settings write that affects the runtime; it runs after `PUT /api/config`, and at startup inside
`Core::start`, the one call every front end makes after `Core::new`. Defaults live in `hermes_core`
`config.rs` (`defaults()`); descriptions for keys Hermes's page shows in `rest.rs` (`config_schema`).

**Request accounting** (Phase 2 of `docs/backend/PHASES.md`): every model request is announced by
`hermes_llm::observer` to the `Ledger` (`runtime/src/ledger.rs`, one thread owning `requests.json`),
labelled with a purpose (`main`, `review`, `curator`, `title`, `approval`, `compress`, `delegate`,
`btw`) taken from a task-local context: set around each turn and background job, re-labelled
for steps inside a turn. `governor.rs` turns today's count and the provider's daily limit into
allow / warn / deny per purpose. The limit comes from Settings ▸ Providers ▸ Custom endpoints
(`daily_request_limit`, patch 0021) or the `requests.limits.<provider id>` config key, else it is
learned from a provider's "N of N" 429. The page sees it as `requests.status` and as the status
bar's allowance gauge (`plan_gauge` in `session.usage`).

**Provider fallback** (Phase 12): see "The provider fallback chain" at the end of this file.

**Budget profiles** (Phase 3): `runtime/src/budget.rs` holds every limit the agent loop spends
requests or tokens on (iteration budget, retry caps, compression thresholds, result/read/search
sizes, delegation width) as `BudgetKnobs`, bundled by `budget.profile` = `requests` (default, also
for configs without the key) | `balanced` (the old constants) | `tokens`, with per-knob overrides
`budget.<knob>`. `apply_settings` hands them to `Runtime::set_budget`; each turn copies
`Runtime::budget()` once. The profile's wording in the system prompt (and the shape of its skills
block: full, categories under `requests`, names under `tokens`) is chosen when a session is created
and stored with it. Settings ▸ Model ▸ Main shows the select via patch 0022.

**Per-turn pre-injection** (Phase 5): with `skills_preload` / `memory_preload` above 0 (`requests`:
3 / 3), `Runtime::begin_turn` asks the local index (`Runtime::index()`, `tools/src/index.rs`) with the
user's message, the last assistant message and the project name, off the async workers
(`preinject::retrieve`, `spawn_blocking`, 3 s limit, any failure means no block), and the message
is added in the turn's task with a `<context>` block in front of its text (`runtime/src/preinject.rs`).
The block is part of the stored message, so resume, rewind and branches replay it byte for byte and the
prompt cache holds; nothing per-turn reaches the system prompt. The message's `preinject` note lists
what was attached (skill names, memory ids): that is how a skill is attached once per conversation
(read off the messages the model still sees, so a rewind that drops the message, or a compaction
that hides it from the model, frees it again), and how `preinject::strip` / `user_text` give the
page, `/undo`, rewind, the `/review` briefing and the compaction summarizer only what the user
typed (session search's rows: `hermes_store::without_attached_context`, the same cut).

**Short tool schemas** (Phase 11 CP11.1): `prompts/tools/{skill_manage,session_search,memory,delegate_task}.json`
carry only what a call needs plus `Details: skill_view hermes-tools-guide`; the rest (every parameter,
examples, gotchas, the old text) is the bundled skill `assets/skills/autonomous-ai-agents/hermes-tools-guide`,
loaded on demand with `skill_view` (the local index never retrieves it, so it is never pre-injected). `crates/prompts/src/tools.rs` holds byte budgets (`SHORTENED`) and
the snapshot test; `prompts/tools/README.md` has the rules.

**Deferred tools** (Phase 11 CP11.2, `runtime/src/deferred.rs`, `prompts/src/deferred.rs`): in a `tokens` session
(`budget.tool_schemas: deferred`) the main agent's tool chain is `GuardedTools { DeferredTools { DelegatingTools { SessionTools } } }`.
`DeferredTools::tools` lists the core tools, `tool_search`, then the tools the conversation has loaded (`loaded_in`: a function of the
answered `tool_search` / direct deferred calls, so resume, rewind, branch and compaction need no stored state); `call` answers
`tool_search` with the schemas. A session is deferred exactly when its frozen prompt holds the *sealed* `# Deferred tools` block (built by
`PromptSource` for `PromptTools::Deferred`, which also drops `skill_manage` and the guidance that names it; the seal is a checksum of the
prompt before the block, so a copy in a context file does not count). A compaction or a model switch keeps the session's kind
(`PromptTools::of_session`). The block also records which MCP tools stay core; later ones are deferred. A call to a deferred tool that is
not loaded is answered by `UnhandledToolCalls` with the schema. In every other profile (and any session whose prompt lacks the block) the
wrapper is transparent. Review, curator and subagents do not go through it. The decisions and the cache cost are in PITFALLS "Deferred tool
schemas".

**First-turn context pack** (Phase 10 CP10.2, `runtime/src/context_pack.rs`): under `requests`
(`budget.context_pack`; `balanced` and `tokens` off) the first user message of a session whose folder is
inside a git repository (`project_skills::find_root`, a worktree counts) gets a `<repo-context>` element
at the front of its `<context>` block (`Block::with_pack`, or a block of its own): branch, the last 5
commit subjects, `git status --short`, the project file names, the README head and a depth-3
`.gitignore`-aware tree, at most 14,000 characters. Collected in the turn's task (`spawn_blocking`, 12 s
overall, every `git` call 4 s with `core.fsmonitor` and `.gitattributes` filters off and no inherited `GIT_*`
variables, output capped; a failure leaves a part out), cached
per repository + HEAD + a fingerprint of the status text in `<data>/cache/context-pack/` (32 files at
most), stored with the message (resume and rewind replay it; the page, `/undo` and session search see only
what was typed, through the `preinject` note), never in the system prompt, once per session (a session that
already has a user message gets none). Repository text is untrusted: tags escaped, a README past the
context-file threat scan; it does not taint (PITFALLS "The first-turn context pack").
`read_file` also takes `paths` (up to 20 files, one entry each) and `search_files` `group_by_file`.

Data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`):

| Path | Owner |
|---|---|
| `sessions.db` | `hermes_store` (frozen schema) |
| `config.json` | `hermes_core` `providers.rs`, `endpoints.rs`, `aux_models.rs` (providers, default model, side-job models; shared with the GPUI app) |
| `desktop-config.json` | `hermes_core` `config.rs` (the config record Settings writes) |
| `ui-preferences.json` | `rest.rs` (the page's own preferences) |
| `generation-stats.json` | `hermes_core` `usage.rs` (tokens/s per session) |
| `branches.json` | `hermes_core` `branch.rs` (child → parent session) |
| `archived-sessions.json` | `hermes_core` `archive.rs` (archived sessions, last auto-archive sweep) |
| `session-workspaces.json` | `hermes_core` `workspace.rs` (a moved session's git identity) |
| `app.lock` | `hermes_core` `lock.rs`: held open by the running app (desktop or native, not ACP); a second app says "Hermes is already open" and exits |
| `hermes-imports.json` | `hermes_core` `hermes_import.rs` (imported Hermes session ids) |
| `requests.json` | `hermes_runtime::ledger` (model requests per provider, day and purpose; the provider's learned limit) |
| `memory.db` (+ `-wal`, `-shm`) | `hermes_memory` (`db.rs`, `sync.rs`): the memory entries since Phase 9 (CP9.1), own schema version; a damaged one is moved aside to `memory.db.corrupt-<time>` |
| `index.db` rows for memory (CP9.3) | `hermes_tools` (`index.rs`): `memory_doc` = the user's own entries (`memory` / `user`, scope `global`) plus the live `project` / `archive` rows of `memory.db` (`project:<n>` / `archive:<n>`, scope `project:<key>` or `global`); a cache, rebuilt from the files and `memory.db`; retrieved per turn under `requests` into the user message's `<context>` block (`preinject.rs`), never the system prompt |
| `memories/notes/<slug>.md` | `hermes_memory` (`notes.rs`): the long-form bodies of project and archive entries (CP9.2), linked from the entry as `file:notes/<slug>.md`; deleted when the user removes the entry or resets memory |
| `memories/MEMORY.md`, `memories/USER.md` | `hermes_memory`: the exported view of `memory.db`'s `core` entries, re-imported when edited by hand; `*.bak.pre-memory-db.<time>` are the copies made before the first migration (never deleted by us) |
| `skills/` (+ `.usage.json`, `.archive/`) | `hermes_tools::skills`, curator |
| `checkpoints/` | `hermes_tools::checkpoints` |
| `images/`, `uploads/` | attachments |
| `desktop-plugins/hermes-rs/` | written on start |
| `logs/desktop.log` | tracing |

API keys are in Windows Credential Manager (`keyring`), never in a file.

## Frontend integration

- `scripts/build-hermes-webapp.ps1`: `git archive` of `../hermes-agent` at `HERMES_COMMIT` into
  `target/hermes-src`, applies `crates/desktop/patches/*.patch` in order, `npm ci`, builds the
  webapp (`apps/desktop/dist-webapp`). Our own instance and the installer both serve that dist.
- Patches: only where the page cannot work otherwise (native dialogs, window controls, zoom,
  paste). The README in that folder documents each one and why.
- Hiding: `plugin.js` (Hermes's plugin API: sidebar rows) and `hide.css` (everything else, keyed on
  stable `data-tour` handles and icon classes). Unhiding a feature = deleting its selector.
- The page routes by hash (`#/settings`, `#/starmap`); Settings sub-pages are
  `[data-tour='nav-config:<section>:<page>']`.

## Threading rules

- Async handlers must not block: wrap filesystem, git (`std::process::Command`), SQLite and other
  blocking calls in `tokio::task::spawn_blocking(move || …).await`. `Store::flush*` and
  `shutdown` wait for the writer thread: from async code always through `spawn_blocking` (as
  `flush_turn` does). `Store::queue` never blocks and may be called anywhere, even under a lock.
  A `Store` read waits for the writer's pending commit, so it counts as blocking too. In the
  gateway use `Gateway::blocking(|g| ...)`; in the tools, the skill/memory/`session_search`
  dispatch, the checkpoint before a write, the post-write lint and the hint files all run on the
  blocking pool (Phase 13c; the audit is in `docs/backend/progress/phase-13.md`, the test in
  `crates/tools/src/blocking_tests.rs`).
- Per-session state is the session actor's (above), not a lock or a map in `Gateway` / `OpenSession`.
  Other shared state in `Gateway` is `std::sync::Mutex` / `parking_lot::Mutex` held briefly, never across
  an `.await`. Poisoned locks: `lock().unwrap_or_else(|e| e.into_inner())`.
- Long work started from an RPC (a turn, a background job) is `tokio::spawn`ed; the RPC answers at
  once and progress arrives as events.

## Project skills (Phase 8 CP8.2)

`tools/src/project_skills.rs` has the rules (read its module doc); the wiring is:

- **Which repository** (`find_root`): the real working folder, then the nearest folder above it with
  `.git`; no repository (or the home folder) means no project skills. `SessionTools::new` takes it
  from the session's folder once (`SessionTools::project_root`); the shell's `cd` doesn't change it,
  a subagent copies its parent's (`inherit_project`, in `delegate.rs`), the curator has none
  (`without_project`). The system prompt (`PromptSource::build`) is built for the folder the session
  has then and is frozen as usual: a model switch or a compaction rebuilds it for the repository
  the session is in then, a workspace move doesn't, and the tools and the index always follow the
  session's *current* repository.
- **What reads it, and only for its own repository**: `SkillStore::*_for(project, ..)` (catalog,
  `find`, `find_any`, `skills_list`, `skill_view`, the index prompt) take the root; the plain
  methods (the Skills screen, the curator, slash commands, bundles, the agentic review's tools) are
  the user's library only. The local index tags rows with a scope and a query names its repository
  (`IndexQuery::project_root`; preinject and the review's related-skills lookup both pass the
  session's). Preinject re-reads a project skill's file through `project_skills::read_skill_md` and
  labels it `scope="project"`.
- **What writes it**: `SkillOp.project` (the session's root, set by `SessionTools` and the review
  from the session, cleared from a model's call) says where names are looked up; `SkillOp.scope` is
  `project` only from the evidence review (`review_once/apply.rs`), never from a model's
  `skill_manage` call. `create` with scope project lands in `<root>/.agents/skills/[category/]name`
  (`write_dir_usable` refuses a non-folder or a link in the path; a refused create removes the
  folders it made); every other action on a project skill works on `.agents/skills` only and
  re-checks that the target resolves inside the skill's folder. Not committed to any git history, not
  in `.usage.json`. A tainted session's project writes stage like any other (the staged op carries
  the root; `FileBase.project` too).
- **Taint**: a project skill's text reaching the model (`skill_view`, `skills_list` does not count;
  the index attaching it) marks the session tainted (`taint::project_skill_source`).
- **The review's scope**: the reply's `scope` (`global` | `project` | null) is validated in
  `contract.rs`; null means `review_once::evidence_scope` (project when the session's file tools
  and terminal `workdir`s only touched that repository, global otherwise, scratch output ignored).

## Skill outcomes and provenance (Phase 8 CP8.1)

- **Outcomes** (`runtime/src/skill_outcomes.rs`, `tools/src/curator.rs`). A skill's record in
  `skills/.usage.json` has `outcomes` (newest 50: `{session, turn, outcome, at}`) and `clean_rate`
  (the share of them that are `clean`). `uses` / `last_used` stay what they were (a whole-skill
  `skill_view` load); a skill the index attached is counted apart, in `attached` / `last_attached`,
  which the curator counts as activity (so a skill attached every turn is not idle) and the Star
  map doesn't (its "learned" filter is `uses > 0`). The rules, in the module doc: a use is
  written `clean` when its turn ends (or `errored` / `corrected` by what the turn shows); a
  `Correction` signal in turn `T` makes turn `T-1`'s uses `corrected`; a rewind of turns
  `T..T+n` makes theirs `undone`; an outcome only gets worse; a cancelled turn is not judged.
  Entries are keyed by (session, turn): the turn numbers are the signals' (renumbered by rewinds,
  so a rewound turn's entries become `undone` before the number is reused).
- **`.usage.json` round trip.** A record keeps fields this version doesn't know (`extra`,
  flattened), an unknown outcome string stays as it is, entries that aren't records are kept, a field
  of the wrong type (`"uses": "3"`, `null`, a BOM) is coerced or dropped alone, never the record;
  an unparseable file is copied once to `.usage.json.corrupt` before the next save replaces it; a
  file that exists but can't be read (locked) is not written at all. Writes go
  through `usage_lock` and a temp file + rename; the runtime does them in `spawn_blocking`.
- **Provenance** (`tools/src/provenance.rs`). `sources:` in the SKILL.md frontmatter, appended as
  a text edit of that block only. `SkillOp.source` is set by our code (review apply, the session's
  `skill_manage` via `SessionTools::skill_source`, staged ops keep it); the model's call can't set
  it (`parse_call` clears it, as `own_hand`). Any write but the user's keeps `sources` as the file had it
  (`keep_sources`: none for a new skill; whatever the writer put there in any form is replaced, and a
  write where that can't be done safely is refused); the editor in the app (`user_write`) may change it.
  A skill the module can't edit safely (no frontmatter, `sources` as a flow list or scalar, YAML
  that doesn't parse) is written without the entry.

## Curator v2 (Phase 8 CP8.3)

`runtime/src/curator.rs` is two curators behind one `Runtime::run_curator(Option<CuratorModel>)`.
Every run first does the deterministic pass (idle curator-managed skills go stale, then to the
archive), then, with a model, one of:

- **`budget.curator_mode = loop`** (`balanced`, `tokens`): today's agent loop over the agent-created
  skills with the three skill tools (`consolidate`), unchanged.
- **`per_cluster`** (`requests`; any profile can opt in with `budget.curator_mode`): `curator/`.
  `propose.rs::library` is the user's own skills (not a repository's, an external folder's, a
  bundled one, a pinned one); `select` finds the jobs (`cluster.rs`: Jaccard of 5-word shingles,
  threshold 0.35, single linkage, at most 4 skills a cluster; `is_low_performer`: at least 5 judged
  uses, under half clean, with evidence from `signals/<id>.json`); `per_cluster.rs` runs at most
  `curator_max_requests` of them, one request each (text contract, `contract.rs`, one repair), with
  the governor asked before every job and `gate.rs`'s hourly cap and back-off in front; `contract::judge`
  turns a reply into `edit` + `archive` ops; `propose::deliver` stages them (one item, with a diff;
  `session_id` `curator`) or, with `curator.auto_apply` and only skills the review created
  (`created_by: agent`), applies them through `plan` + `apply_staged`; a skill whose SKILL.md changed
  since the model read it refuses the proposal, and a skill with files besides SKILL.md is never
  archived by a merge.
  `seen.rs` (`curator-seen.json` in the data folder) remembers jobs already judged for 30 days, unless
  their skills changed. Zero jobs means zero requests and no model needed.
- **The desktop** passes the model (`maintenance::curator_model`: `aux_models.curator`, else the
  review model, else the main one) only when the knob says `per_cluster`; otherwise `None` as before
  (archiving only). The trigger is unchanged: the weekly schedule after two idle hours, or Run now.
- **`SkillOp` `archive`** (tools): moves a library skill to `.archive/` after its text was merged into
  `absorbed_into`; only valid after every other op of a batch, checked (not pinned, not bundled /
  external / project, the absorber exists) for all archives before the first change; a failure after
  an archive went through takes it back out of `.archive` (no duplicate). A model's
  `skill_manage` call can't use it (`parse_call` refuses the action).

## The provider fallback chain (Phase 12 CP12.1)

When the main provider cannot serve a request, the turn goes on with the next provider the user
listed (GAPS 3.8). Four small modules, one per layer; the switch itself is in one place,
`InferenceRunner::open_stream` (`crates/agent/src/inference.rs`).

| Layer | File | What it holds |
|---|---|---|
| `hermes_llm` | `fallback.rs` | `classify(&ProviderError) -> Option<Trigger>`: the only place that decides which failures switch. Also `daily_limit_in` (the "N of N" of a quota 429) |
| `hermes_agent` | `fallback.rs`, `inference.rs` | `FallbackChain` (targets, `FallbackGate`, `FallbackShared`), the walk in `open_stream`, the notice |
| `hermes_runtime` | `fallback.rs` | `FallbackModel` (a configured provider + model), the gate (governor, ledger, token estimate), `TurnOptions.fallback` |
| `hermes-desktop` | `fallback.rs` | `fallback_providers` (config record) -> validated `FallbackModel`s |

**Config.** `fallback_providers` in `desktop-config.json`: `[{provider, model}, ...]`, in the order tried,
at most 3 (`MAX_FALLBACKS`). It is the key Hermes's own Settings ▸ Model ▸ Fallbacks sub-page edits (unhidden in
`hide.css`; the schema entry is in `rest.rs`). It is global, not per profile or per session: this backend has
no profile layering and a session stores only its own provider and model. Read at every turn
(`Core::turn_options`), so a change applies from the next message. An entry names a provider that is
configured under Settings ▸ Providers; its endpoint and key are that provider's, and nothing else in the entry
is read (the page may write `base_url`, `api_key`... beside it; they are ignored, a key never travels in the
chain). Unusable entries are dropped with a logged warning, never a failed turn: unknown or key-less provider,
a model a fixed model list does not contain, the main model itself, a repeat, the fourth and later. A model that
a discovering provider does not have is not checked (it would cost a network call per turn): the provider
answers "model not found", which moves on.

**What switches** (`classify`; the table is in the module doc): a daily-quota 429 (the "N of N" body, "usage
limit reached", "free-models-per-day", "per day", a Retry-After of an hour or more), our own governor at 0 for
the provider (no request is sent), 5xx or no answer after the provider layer's own retries, and an unknown
model (404, or 400 saying "not a valid model" / "does not exist"). **Never** 401 / 403 (a wrong key is the
user's to fix, and the conversation would leave for a provider the user did not choose for that failure), 400,
422, 413 / context too long (compression handles it), 402, a per-minute 429 (the existing back-off), a stream
that stopped decoding. `should_retry` no longer retries a daily-quota 429 or a missing model (they would cost
`http_retries` extra requests of the scarce quota before the switch).

**One request, not the session.** The runner is built per turn and keeps the chain position in a `ChainRun`:
after a switch the rest of the TURN stays on the fallback (no flapping between providers mid-turn), and the
next turn starts at the primary again. The primary is skipped without a request only when the governor knows it
is out of requests until its reset: its configured daily limit is used up, or it answered a daily-quota 429
(`Ledger::mark_exhausted`, in memory, until the provider's next reset, honoured by `Governor::check`, so by
`begin_turn` too). An outage or an unknown model is not remembered: the next turn asks the primary once, at the
cost of one failed request (plus its retries) per turn while it is down. The same request is re-issued: same
messages and tools, signed reasoning of another provider dropped as for any provider change
(`finalize_request`), the fallback's model and (known) window, no reasoning-effort params (they are
model-specific). Tool results already executed are not re-run: only the model call moves.

**A response is never delivered twice.** A request moves only before anything of its response reached the
page: an HTTP error before the stream, or an error as the stream's first item (the runner peeks it). Once a
chunk was delivered, a failing stream ends the turn as it always did (what arrived stays, the error follows);
it is never re-asked on another provider (that would answer twice).

**Every provider keeps its own limits.** The gate asks `Governor::check(provider, Main)` for each position
before a request goes there, so a switch cannot spend a quota the user capped; a refused fallback is skipped and
the walk continues. The ledger counts a request on the provider that served it (every client is labelled with its
provider id), so a turn that moved shows one request on each. `begin_turn` refuses a turn at 0 only when no
provider of the chain may take it.

**Context length is checked before switching.** Per target, `fit_limit` is the compression threshold of its
window (`Budget::with_compress_at`); the runner estimates the request (`FallbackGate::estimate`, chars / 4).
If it does not fit, the runner asks for compression once per target and turn: it records a `Pending` and returns
an empty `applied` (`Next::Continue`), the agent loop starts another pass, and `AutoCompress` (which reads `FallbackShared`) compresses for that
window, writing the summary on the target's provider instead of the session's (an auxiliary compression model
keeps its own provider, with the target behind it in place of the session model); then the runner re-checks. Still too big: the target is skipped, never sent an over-limit request.
Compression is the one allowed exception to prompt caching. A target with an unknown window is not checked.

**The notice.** `AgentEvent::Status { kind: "fallback", text }` -> `status.update`, the one kind the page keeps as
a line in the conversation (the same one the taint warnings use). Text: which provider could not serve and why,
`switched to B (model) for the rest of this turn`, that the conversation is now sent to B, that its prompt cache
starts empty (the next request reads everything again), and that the next message tries A again. If the whole
chain fails, the error shown is the first provider's own, followed by `Fallback chain: a: ...; b: ...`.

**Who uses the chain.** Only the main turn. The review, curator, titles, compression summaries (except for the
fallback's window, above), the approval judge and `delegate_task` children keep their own providers: the
governor already defers background jobs when quota is low, and a chain for them would send work the user did
not see to another provider. ACP (`hermes-acp`) drives the same gateway, so it gets the chain too (the notice
reaches Zed as a thought line, like the compression status). Subagents of a turn run on the primary.
