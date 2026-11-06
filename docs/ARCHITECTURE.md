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
│   gateway.rs  Gateway { runtime, hub, rest (settings), side stores … }            │
│     │  per-feature modules: endpoints, attachments, files, git_review,           │
│     │  terminal, branch, toolsets, learning, hermes_import, export, skills …     │
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
handler holds (behind an `Arc`, i.e. a refcounted shared pointer), `Runtime` is the agent library
it calls into. Everything runs in one process.

## Crates

| Crate | Package | Role | Notes |
|---|---|---|---|
| `crates/desktop` | `hermes-desktop` | The app: Tauri window, axum server, the protocol layer | The main app. Most feature work lands here. |
| `crates/runtime` | `hermes_runtime` | Sessions and turns over the agent: `Runtime` | `lib.rs` (API), `session.rs`, `ops.rs` (per-turn tool guards), `delegate.rs` + `subagents.rs`, `compress.rs`, `review.rs` (background review), `curator.rs`, `mcp.rs`, `commands.rs` (`/diff`, `/usage`…), `guards.rs`, `changes.rs` |
| `crates/agent` | `hermes_agent` | The agent state machine (from Goose): stream a reply, run tool calls, repeat | `events.rs` has `AgentEvent` |
| `crates/llm` | `hermes_llm` | Messages, the OpenAI-compatible provider, retries (from Goose) | `conversation::message::{Message, MessageContent}` |
| `crates/tools` | `hermes_tools` | Every agent tool + `SessionTools` (the per-session tool set) | One module per tool family; `paths.rs` guards file access; `approval.rs` the dangerous-command check |
| `crates/memory` | `hermes_memory` | The memory store and tool (`§`-separated entries, char limits, drift guard) | |
| `crates/store` | `hermes_store` | SQLite: sessions, messages, usage, projects | **Schema frozen** (shared with GPUI) |
| `crates/prompts` | `hermes_prompts` | Builds the system prompt; tool schemas; coding-command and side-question prompts | Text comes from `prompts/` |
| `crates/app` | — | The frozen GPUI app | Keep it compiling |
| `crates/markdown`, `highlight`, `themes` | — | Used by the GPUI app only | |

## Startup (`crates/desktop/src/main.rs`)

1. `RuntimeConfig::for_current_user()` picks the data folder (`HERMES_RS_HOME`, else
   `%APPDATA%\hermes-rs`) and starts file logging (`logs/desktop.log`).
2. A tokio runtime with 4 workers is built; Tauri keeps the main thread.
3. The port: `HERMES_DESKTOP_PORT`, else a fixed default (the page's localStorage is per origin,
   so the port must stay the same between runs), else any free port.
4. In parallel with the window: `Runtime::new` (store, skills seeding, tools) inside
   `spawn_blocking`, then `Gateway::new`, then `server::serve`. The page's first requests wait in
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

### JSON-RPC over `/api/ws` (`gateway.rs`)

- `connection` reads frames; `handle` spawns each request; `dispatch(method, params, outbox)` is a
  big `match method { … }` returning `RpcResult = Result<Value, (i64, String)>`.
- Error codes: `METHOD_NOT_FOUND` -32601 (we don't serve it: page hides the feature),
  `INVALID_PARAMS` -32602 (bad/missing params: `session_id(params)?` gives you this), `SERVER_ERROR`
  -32000 (it failed: message shown to the user, so write it for a human).
- Unknown methods fall to the `_` arm, which logs under target `hermes_desktop::missing` and
  answers -32601. That log is the live to-do list.
- **Events** go out as `{"method":"event","params":{"type":…,"session_id":…,"payload":…}}` via
  `self.emit(session_id, event, payload)`. Delivery goes through `Hub` (`hub.rs`): each socket
  subscribes to the sessions it opened (`subscribe`), so a session's events reach whichever
  window shows it. Turn events are translated from `RuntimeEvent`/`AgentEvent` in `submit` and
  `transcript.rs` (`message.start`, `message.delta`, `reasoning.delta`, `tool.start`,
  `tool.complete`, `message.complete`, `approval.request`, `clarify.request`, `subagent.*`,
  `btw.complete`, `background.complete`, `session.info`, …).
- Bigger features live in their own module and `dispatch` has a one-line arm calling it
  (e.g. `"session.branch" => self.branch(...)`, `crate::git_review::route`).

### REST under `/api` (`Gateway::rest`)

- Signature: `rest(method, path, query, body) -> Option<(u16, Value)>`. It is a chain of
  `if path == … / strip_prefix(…)` blocks, each returning `Some((status, json))`; falling through
  ends in `self.rest.handle(...)` (`rest.rs`: config, ui-preferences, startup routes) and then
  `None` → 404 (`server.rs` `api` logs it under `hermes_desktop::missing`).
- Error bodies are `{"detail": "…"}` (FastAPI's shape; the page shows `detail`).
- A prefix block delegates to its module: `/api/providers/custom-endpoints*` → `endpoints.rs`,
  `/api/fs/*` and `/api/files/*` → `files.rs`, `/api/git/*` → `git_review::route`,
  `/api/tools/*` → `toolsets.rs`, `/api/learning/*` → `learning.rs`, `/api/skills*` → `skills.rs`,
  `/api/sessions/{id}` → `session_route`, `/api/sessions/{id}/messages` → `stored_messages`.

## Feature map: screen → code → check

Where to look first for each thing the user sees. (`gw` = an arm in `gateway.rs`.)

| Screen / command | Backend | Calls | Live check |
|---|---|---|---|
| Chat: send, stream, stop, queue, steer | gw `submit`, `transcript.rs`, runtime `start_turn` | `prompt.submit`, `session.interrupt`, `session.steer`, `prompt.*_queued` | `smoke.mjs`, `stop-check.mjs` |
| Approvals, clarify | `hub.rs` (`DesktopBridge`), `hermes_tools::approval` | `approval.respond`, `clarify.respond` | `smoke.mjs` |
| Sidebar: sessions, search, pin, rename, delete | gw `session_list`, `session_route` | `/api/profiles/sessions`, `/api/sessions/*` | — |
| Artifacts (sidebar), `file:` links | page over `stored_messages`; `open_file.rs` (patch 0013) | `/api/sessions/{id}/messages`, `/api/hermes-rs/open-file` | `artifacts-check.mjs` |
| Archive, Settings ▸ Sessions | `archive.rs`, gw `set_archived`, `auto_archive` | PATCH `archived`, `session.mutate archive`, `?archived=` | `archive-check.mjs` |
| Projects view | `projects.rs` | `projects.tree`, `projects.*` | — |
| `/` menu, skills commands | `commands.rs` | `commands.catalog`, `complete.slash`, `slash.exec`, `command.dispatch` | — |
| Built-in commands (`/status`, `/compress`, `/diff`…) | gw `builtin_command`, runtime `commands.rs` | `slash.exec` | — |
| `/plan`, `/init`, `/review`, `/learn`, `/import-hermes` | `commands.rs` `coding_command`, gw `review_task` | `slash.exec` | `coding-commands-check.mjs` |
| `/branch`, `/btw`, `/bg` | `branch.rs`, runtime `side_question`/`background` | `session.branch*`, `prompt.btw`, `prompt.background` | `side-and-branch-check.mjs` |
| `/undo`, `/retry`, restore, `/rollback` | gw `rewind_turn`, `rollback`, `hermes_tools::checkpoints` | `session.rewind`, `session.mutate`, `rollback.*` | `rollback-check.mjs` |
| `/save` | `export.rs` | `session.export` | — |
| `@` completions | `paths.rs` | `complete.path` | — |
| Attachments | `attachments.rs`, `server.rs` upload | `/api/chat/image-upload`, `/api/chat/file-upload`, `image.attach`, `file.attach` | `attach-check.mjs` |
| Side panel: file preview/edit | `files.rs`, `server.rs` `file_bytes` | `/api/fs/*`, `/api/files/*` | `files-check.mjs` |
| Side panel: git review, coding rail | `git_review.rs`, `git.rs` | `/api/git/*` | `git-check.mjs` |
| Side panel: terminal | `terminal.rs`, `server.rs` | `/api/host-terminal` (PTY socket) | `terminal-check.mjs` |
| Subagents | runtime `delegate.rs`, `subagents.rs`; gw `subagent.*` | `subagent.*`, `delegation.*` | `subagents-check.mjs` |
| Model picker, effort | gw `model_options`, `switch_model`, `providers.rs` | `model.options`, `config.set {key: "model"}`, `session.mutate` | `switch.mjs` |
| Settings ▸ Model ▸ Auxiliary | `aux_models.rs`, gw `turn_options` | `/api/model/auxiliary`, `/api/model/set {scope: auxiliary}` | `aux-models-check.mjs` |
| Settings ▸ Providers ▸ Custom endpoints | `endpoints.rs`, `providers.rs` | `/api/providers/custom-endpoints*` | `endpoints-check.mjs` |
| Settings (all other pages) | `rest.rs` (`config_defaults`, `config_schema`), gw `apply_settings` | `/api/config*` | — |
| Settings ▸ Memory, the Star map | `learning.rs`, `hermes_memory` | `/api/learning/*`, `/api/memory*` | `memory-check.mjs` |
| Capabilities ▸ Skills | `skills.rs` | `/api/skills*` | — |
| Capabilities ▸ Tools | `toolsets.rs`, runtime `set_disabled_tools` | `/api/tools/*` | `toolsets-check.mjs` |
| Command Center ▸ Maintenance, logs, insights | `maintenance.rs`, gw | `/api/curator*`, `/api/logs`, `insights.get` | — |
| Import Hermes sessions | `hermes_import.rs` | `session.foreign.*`, `/import-hermes` | `import-check.mjs` |
| Profiles window, SOUL.md editor | `profiles.rs` | `/api/profiles/{name}/soul`, `/api/profiles` | `profiles-check.mjs` |
| Window caption buttons, zoom, dialogs | `server.rs` `window_control`, `main.rs` | `/api/hermes-rs/window` | — |

## A turn, end to end

1. Page: `prompt.submit {session_id, text, …}` (or `session.create` first).
2. `Gateway::submit`: resolves the session's provider/model (`provider_for`, `route_session`),
   attachments (`pending_images`, `attachments.rs`), builds the `TurnOptions` with
   `Gateway::turn_options` (the model's known window, the session's effort, the side jobs'
   models — use it for every runtime call that takes options), queues it if a turn is running
   (`enqueue`/`run_queued`), then `runtime.start_turn(...)` (or `start_turn_with_images`,
   `start_turn_with_call` for a scripted first tool call).
3. `Runtime::start_turn` (runtime `lib.rs`): loads the session from the store, builds the system
   prompt once per session (`hermes_prompts`, kept for caching), builds `SessionTools` minus
   disabled toolsets, wraps them in the per-turn ops (`ops.rs`: `GuardedTools` checks paths,
   approvals and disabled tools; `UnhandledToolCalls` answers unknown/disabled tool names;
   `DeliverSteer` injects `/steer` text), and runs the Goose agent loop on a task. Returns a
   `Turn` with an `events` channel and a cancel handle.
4. The agent streams from `hermes_llm`'s OpenAI-compatible provider, runs tool calls
   (`hermes_tools`), persists each message to the store, and emits `AgentEvent`s.
5. Gateway forwards those as page events, records generation stats (`note_generation`), and on
   `TurnFinished` sends `message.complete`, runs the next queued prompt, titles a new session
   (`title_session`, aux "titles" model), and the background review may run.
6. Approvals and clarify questions: the runtime calls its `UserBridge` (`hub::DesktopBridge`),
   which emits `approval.request`/`clarify.request` and waits for the page's
   `approval.respond`/`clarify.respond`.

Delegation: the `delegate_task` tool goes to `Delegator` (`runtime/src/delegate.rs`), which runs
child agents (max `MAX_CONCURRENT_CHILDREN`), registers them in `Subagents` (`subagents.rs`) for
`subagent.list/tail/steer/interrupt`, and relays their progress as `AgentEvent::Subagent{kind,
payload}` → `subagent.*` events. `/bg` (`Runtime::background`) is a detached child; `/btw`
(`Runtime::side_question`) is one tool-less request over a snapshot.

## Settings and data

Two settings files, with different owners — don't mix them up:

| File | Read/written by | Holds |
|---|---|---|
| `config.json` | `providers::Settings::load/save`, `aux_models.rs` | providers (custom endpoints), the default model, the side jobs' models (`aux_models`), shared with GPUI |
| `desktop-config.json` | `rest.rs` (`config_value("a.b")`, `set_config_value`) | everything on Hermes's Settings page (Hermes's `config.yaml` keys, dotted) |
| `ui-preferences.json` | `rest.rs` | the page's interface preferences |

`Gateway::apply_settings()` pushes `desktop-config.json` values into the runtime (approvals mode,
checkpoints, memory limits, disabled toolsets). Call it after any settings write that affects the
runtime; it runs at startup and after `PUT /api/config`. Defaults and descriptions for keys
Hermes's page shows live in `rest.rs`.

Data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`):

| Path | Owner |
|---|---|
| `sessions.db` | `hermes_store` (frozen schema) |
| `config.json`, `desktop-config.json`, `ui-preferences.json` | above |
| `generation-stats.json` | gateway (tokens/s per session) |
| `branches.json` | `branch.rs` (child → parent session) |
| `archived-sessions.json` | `archive.rs` (archived sessions, last auto-archive sweep) |
| `hermes-imports.json` | `hermes_import.rs` (imported Hermes session ids) |
| `memories/MEMORY.md`, `memories/USER.md` | `hermes_memory` |
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
  blocking calls in `tokio::task::spawn_blocking(move || …).await`.
- Shared state in `Gateway` is `std::sync::Mutex` / `parking_lot::Mutex` held briefly, never across
  an `.await`. Poisoned locks: `lock().unwrap_or_else(|e| e.into_inner())`.
- Long work started from an RPC (a turn, a background job) is `tokio::spawn`ed; the RPC answers at
  once and progress arrives as events.
