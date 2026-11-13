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
| `crates/runtime` | `hermes_runtime` | Sessions and turns over the agent: `Runtime` | `lib.rs` (API), `session.rs`, `ops.rs` (per-turn tool guards), `delegate.rs` + `subagents.rs`, `compress.rs`, `review.rs` (background review), `signals.rs` (learning signals: what each turn says is worth learning, scored locally into `signals/<id>.json`), `skill_outcomes.rs` (how the turns a skill was used in went, into `.usage.json`), `review_once.rs` (the evidence review: one request with a packet of signals, skills and memory; a JSON reply validated and applied or staged), `review_schedule.rs` (when it runs: per-session idle timers, switched-away/closed, one review at a time, back-off, hourly cap), `curator.rs` + `curator/` (the curator: the agent loop of `balanced`, and per-cluster proposals under `requests`), `mcp.rs`, `commands.rs` (`/diff`, `/usage`…), `guards.rs`, `changes.rs` |
| `crates/agent` | `hermes_agent` | The agent state machine (from Goose): stream a reply, run tool calls, repeat | `events.rs` has `AgentEvent` |
| `crates/llm` | `hermes_llm` | Messages, the OpenAI-compatible provider, retries (from Goose) | `conversation::message::{Message, MessageContent}`; `observer.rs` is where every model POST is reported (purpose, session, provider) to the request ledger |
| `crates/tools` | `hermes_tools` | Every agent tool + `SessionTools` (the per-session tool set) | One module per tool family; `paths.rs` resolves paths; `path_policy.rs` classifies where file edits and terminal commands may go (workspace, app data, skills/memory, persistence places, outside); `approval.rs` the dangerous-command check; `taint.rs` what the session has read that the user didn't write (see Taint in PITFALLS.md); `staging.rs` the writes a tainted session's `memory` / `skill_manage` calls leave for the user (`staging/<id>.json`, applied through the stores' own write paths); `index.rs` the local FTS5 index (`index.db`) over skills and memory, kept fresh by write hooks on `SkillStore` / `MemoryStore` |
| `crates/memory` | `hermes_memory` | The memory store and tool (`§`-separated entries, char limits, drift guard) | `store.rs` (the API every caller uses), `tool.rs`, `threats.rs`; since Phase 9: `db.rs` (`memory.db`: entries with tier/scope/kind/links; the agent's removals kept as marked rows, the user's erased) and `sync.rs` (the files as the exported view of the database: migration with a backup copy, re-import of manual edits, the write step under the database's lock, fallback to the files alone) |
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

Where to look first for each thing the user sees. (`gw` = an arm in `gateway.rs`.)

| Screen / command | Backend | Calls | Live check |
|---|---|---|---|
| Chat: send, stream, stop, queue, steer | gw `submit`, `transcript.rs`, runtime `start_turn` | `prompt.submit`, `session.interrupt`, `session.steer`, `prompt.*_queued` | `smoke.mjs`, `stop-check.mjs`, `provider-error-check.mjs` |
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
| Settings ▸ Model ▸ Auxiliary | `aux_models.rs`, gw `turn_options` | `/api/model/auxiliary`, `/api/model/set {scope: auxiliary}`, `/api/model/auxiliary/local` (ours: "Use a local model for background jobs", patch 0023) | `aux-models-check.mjs`, `local-aux-check.mjs` |
| Settings ▸ Providers ▸ Custom endpoints | `endpoints.rs`, `providers.rs` | `/api/providers/custom-endpoints*` | `endpoints-check.mjs` |
| Settings (all other pages) | `rest.rs` (`config_defaults`, `config_schema`), gw `apply_settings` | `/api/config*` | — |
| Settings ▸ Memory, the Star map | `learning.rs`, `hermes_memory` | `/api/learning/*`, `/api/memory*` | `memory-check.mjs` |
| Capabilities ▸ Skills | `skills.rs` | `/api/skills*` | — |
| Writes waiting for review (tainted sessions, or write approval on): "Waiting for your review" on Settings ▸ Memory and Capabilities ▸ Skills (patches 0024-0026) | `staging.rs`, `hermes_tools::staging` | `staging.list/apply/reject` (ours), `/api/staging*` | — |
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
   (`title_session`, aux "titles" model), and the background review may run. Just before
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
6. Approvals and clarify questions: the runtime calls its `UserBridge` (`hub::DesktopBridge`),
   which emits `approval.request`/`clarify.request` and waits for the page's
   `approval.respond`/`clarify.respond`.

Delegation: the `delegate_task` tool goes to `Delegator` (`runtime/src/delegate.rs`), which runs
child agents (at most the budget profile's `delegate_max_children`), registers them in `Subagents` (`subagents.rs`) for
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
checkpoints, memory limits, disabled toolsets, budget knobs). Call it after any settings write that affects the
runtime; it runs at startup and after `PUT /api/config`. Defaults and descriptions for keys
Hermes's page shows live in `rest.rs`.

**Request accounting** (Phase 2 of `docs/backend/PHASES.md`): every model request is announced by
`hermes_llm::observer` to the `Ledger` (`runtime/src/ledger.rs`, one thread owning `requests.json`),
labelled with a purpose (`main`, `review`, `curator`, `title`, `approval`, `compress`, `delegate`,
`btw`) taken from a task-local context: set around each turn and background job, re-labelled
for steps inside a turn. `governor.rs` turns today's count and the provider's daily limit into
allow / warn / deny per purpose. The limit comes from Settings ▸ Providers ▸ Custom endpoints
(`daily_request_limit`, patch 0021) or the `requests.limits.<provider id>` config key, else it is
learned from a provider's "N of N" 429. The page sees it as `requests.status` and as the status
bar's allowance gauge (`plan_gauge` in `session.usage`).

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

Data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`):

| Path | Owner |
|---|---|
| `sessions.db` | `hermes_store` (frozen schema) |
| `config.json`, `desktop-config.json`, `ui-preferences.json` | above |
| `generation-stats.json` | gateway (tokens/s per session) |
| `branches.json` | `branch.rs` (child → parent session) |
| `archived-sessions.json` | `archive.rs` (archived sessions, last auto-archive sweep) |
| `hermes-imports.json` | `hermes_import.rs` (imported Hermes session ids) |
| `requests.json` | `hermes_runtime::ledger` (model requests per provider, day and purpose; the provider's learned limit) |
| `memory.db` (+ `-wal`, `-shm`) | `hermes_memory` (`db.rs`, `sync.rs`): the memory entries since Phase 9 (CP9.1), own schema version; a damaged one is moved aside to `memory.db.corrupt-<time>` |
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
  blocking calls in `tokio::task::spawn_blocking(move || …).await`.
- Shared state in `Gateway` is `std::sync::Mutex` / `parking_lot::Mutex` held briefly, never across
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
