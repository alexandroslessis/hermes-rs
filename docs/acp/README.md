# ACP: using hermes-rs from Zed (and later t3code)

Status: **planned** (2026-11-10). Nothing here is implemented yet. This folder is the plan and the
reference for implementing it; read it whole before writing code, then follow
[PHASES.md](PHASES.md) step by step.

| File | What it is |
|---|---|
| [README.md](README.md) | This file: what we're building, the decisions, the architecture, the file layout |
| [PROTOCOL.md](PROTOCOL.md) | The parts of ACP v1 we speak, with exact JSON, and every mapping table (gateway event → ACP update, tool → kind, approval → permission) |
| [PHASES.md](PHASES.md) | The implementation, phase by phase: tasks, files, functions, tests, done criteria |
| [TESTING.md](TESTING.md) | How ACP is tested: unit tests, the black-box `tests/acp.rs` harness, the probe script, the Zed checklist |
| [ZED.md](ZED.md) | Setting Zed up to run hermes-rs, and debugging it there (for the user and for you) |

Read [AGENTS.md](../../AGENTS.md) first if you haven't: every ground rule there (port Hermes,
cite the source, copy never depend, never block async workers, frozen store schema, leave the docs
true) applies here too.

## What ACP is, in one paragraph

The **Agent Client Protocol** (agentclientprotocol.com) lets an editor ("client": Zed, t3code,
JetBrains, Neovim plugins…) drive a coding agent ("agent": us). The editor starts the agent as a
child process and they talk **JSON-RPC 2.0 over the child's stdin/stdout, one JSON object per
line**. The editor sends `initialize`, `session/new`, `session/prompt`, `session/cancel`; the agent
streams progress back as `session/update` notifications (text chunks, thoughts, tool calls, plans)
and asks the editor things with requests of its own (`session/request_permission`). Python
analogy: it is LSP for agents. C++ analogy: two processes exchanging a tagged-union message type
over a pipe.

## What the user gets

- In Zed's Agent panel: "New Hermes (rs) Thread", chat with the hermes-rs agent (same model,
  tools, memory, skills, prompts as the desktop app), see its tool calls with file diffs, approve
  dangerous commands, answer its clarify questions, see its todo list as Zed's plan, stop a turn,
  reopen past threads, pick the model.
- The sessions are the desktop app's sessions (same `sessions.db`): a thread started in Zed shows
  in the desktop app's sidebar and vice versa.
- Later: the same agent in t3code (Phase 6), which speaks the same protocol.

## Decisions (made while planning; the user may override any of them)

| # | Decision | Why | Rejected alternative |
|---|---|---|---|
| D1 | **Same exe, new mode:** `hermes-desktop.exe --acp` runs the ACP server on stdio and never opens a window. | Reuses every module of `crates/desktop` (providers, settings, gateway) without splitting the crate; the installer already ships this exe. | A new `hermes-acp.exe`: needs `crates/desktop` split into lib + bins first (a big mechanical refactor, risky for no user benefit). Possible later if the GUI-subsystem release build gives trouble (see D1 check in Phase 0). |
| D2 | **ACP is a client of our own Gateway, in-process.** The ACP layer calls `Gateway::dispatch("session.create" / "prompt.submit" / "approval.respond" …)` with its own `Outbox` channel and translates the gateway's page events (`message.delta`, `tool.start`, `approval.request` …) into ACP updates. | This is exactly how Hermes's own ACP adapter works (`acp_adapter/gateway_server.py`: ACP is a viewer of the tui_gateway). Everything the page gets for free (queueing, titles, model routing, effort, aux models, slash commands, approvals, compression notices) the editor gets too, with no runtime changes and no duplicated turn logic. Port map stays 1:1 with Hermes. | Driving `hermes_runtime::Runtime` directly (Hermes's older `acp_adapter/server.py` path): duplicates `Gateway::submit`, `turn_options`, `provider_for`, titling, queueing (~600 lines) and drifts. |
| D3 | **No ACP SDK crate.** Hand-written newline-delimited JSON-RPC over stdio with `serde_json::Value` and `json!`, like `gateway.rs`. Exact shapes are written down in [PROTOCOL.md](PROTOCOL.md). | The Rust SDK (`agent-client-protocol`) changes its API every major version (Goose pins 2.2, Zed 3.6, both with different builder APIs); its schema is small; plain JSON is easy to read, test and debug, and adds no dependency. | `agent-client-protocol` / `agent-client-protocol-schema` crates. If the user prefers typed structs later, the schema crate (types only) can replace the `json!` builders module by module. |
| D4 | **Tools run in our process, on the real filesystem**, as in the desktop app. We do not use the editor's `fs/*` or `terminal/*` client methods. | Hermes's ACP adapter does the same; our tools have their own guards (`paths.rs`, approvals, checkpoints). | Routing reads/writes through Zed (sees unsaved buffers, but every tool would need a second code path). Possible later for `read_file` only. |
| D5 | **Separate process, shared data folder.** `--acp` uses the same `%APPDATA%\hermes-rs` (or `HERMES_RS_HOME`) as the GUI app, which may be running at the same time. | Sessions, memory, skills, settings and keys are shared, which is what the user wants. SQLite is in WAL mode with a 5 s busy timeout (`crates/store/src/lib.rs`), so two processes are fine. | A private ACP data folder: the user would have two disconnected agents. |
| D6 | **stdout is the protocol.** In `--acp` mode logging goes to `logs/acp.log` and stderr, never stdout; nothing else may print to stdout. | One stray line on stdout breaks the editor's parser. Today `init_logging` writes to stdout **and** the file (`main.rs`). | — |
| D7 | **No auth methods.** `authMethods: []`. With no provider configured, `session/new` fails with a readable message telling the user to set one up in the desktop app. | Providers and keys are set up in the desktop app (Settings ▸ Providers). | Hermes's terminal-auth setup flow: no CLI here. |
| D8 | **Background jobs stay in the GUI app.** In `--acp` mode the gateway doesn't schedule the curator or the auto-archive sweep. | Two processes must not run the curator on the same skills folder at once. | — |

Things deliberately **left out** (write them in the module docs when you get there): the
editor's per-session MCP servers (`mcpServers` in `session/new` is logged and ignored: the
runtime's MCP manager is global and configured in the desktop app; see Phase 5), audio prompts,
`session/fork`, the unstable ACP v2 protocol, the ACP registry listing, and Hermes's edit-approval
modes (hermes-rs has no "ask before edits"; our modes are `default` / `yolo`, Phase 4).

## Architecture

```
Zed (client)                         hermes-desktop.exe --acp  (one process, no window)
────────────                         ───────────────────────────────────────────────────────────
                stdin  (lines)       acp::rpc  reader task ──► parse ──► request?  ──► spawn handler
  JSON-RPC  ───────────────────────►                                   response? ──► pending client
                                                                                      request (oneshot)
                stdout (lines)       acp::rpc  writer task ◄── mpsc<String> ◄── every outgoing message
  JSON-RPC  ◄───────────────────────
                                     acp::server::AcpServer
                                       ├─ initialize / session/new / load / list / prompt / cancel …
                                       ├─ sessions: HashMap<SessionId, AcpSession>   (turn state)
                                       │
                                       │  Gateway::dispatch(method, params, &outbox)   (D2)
                                       ▼
                                     gateway::Gateway  (the same object the page talks to)
                                       │   events → Outbox (mpsc<String>, frames {"method":"event"…})
                                       ▼
                                     acp::server event pump: frame → acp::translate → session/update
                                       │                     approval.request → session/request_permission
                                       ▼
                                     hermes_runtime::Runtime → agent loop → tools → provider
```

- One reader task, one writer task, one **event pump** task (reads the gateway Outbox), and one
  spawned task per incoming request (so a long `session/prompt` never blocks `session/cancel`).
- Incoming requests are answered when their handler finishes. `session/prompt` finishes when the
  gateway emits `message.complete` for that session.
- Outgoing requests (`session/request_permission`, `elicitation/create`) get ids from a counter;
  their responses come back on stdin and are routed to a `oneshot` by id.

## Where the code goes

```
crates/desktop/src/
  main.rs            + `--acp` check at the very top of main(): acp::run() and return
  acp/
    mod.rs           run(): logging (D6), tokio runtime, Runtime + Hub + Gateway (D8), serve stdio
    rpc.rs           framing: line reader/writer, JSON-RPC message kinds, ids, pending requests
    server.rs        AcpServer: the method match, per-session state, the event pump
    translate.rs     gateway event → ACP session/update(s)   (pure, unit-tested)
    tools.rs         tool name → kind/title/locations/content, diffs   (pure)
    prompt.rs        ACP content blocks → our prompt text + images     (pure)
    history.rs       stored transcript rows → replay updates for session/load   (pure)
    permissions.rs   approval/clarify request → ACP options; outcome → gateway answer   (pure)
  gateway.rs         small visibility changes only (Phase 0/1): `pub(crate) async fn dispatch`,
                     a way to build it without background jobs (D8)
crates/desktop/tests/
  acp.rs             black-box tests: spawn the exe with --acp against a wiremock model
scripts/acp/
  acp-probe.mjs      send a scripted conversation to the exe and print every line (debugging)
```

Every module starts with a header citing its source (`//! Hermes: \`acp_adapter/…\``), and gets a
row in `PORTING.md` (ground rule 2). The Hermes files to port from, at `HERMES_COMMIT`:

| Our module | Hermes source (`../hermes-agent/acp_adapter/`) |
|---|---|
| `acp/mod.rs` | `entry.py` (stdio only, logging to stderr) |
| `acp/server.rs` | `gateway_server.py` (`GatewayACPAgent`: initialize, new/load/resume/list, prompt, cancel, `_events`, `_project`), `server.py` (`initialize`, `list_sessions` paging, `_session_response_fields`) |
| `acp/translate.rs` | `gateway_server.py` `_project`, `gateway_reply.py` (`_project_tool`, `_finish_reply`, `_chunk`, `_project_side_update`), `events.py` (`_build_plan_update_from_todo_result`) |
| `acp/tools.rs` | `tools.py` (`TOOL_KIND_MAP`, `build_tool_title`, `build_tool_start`, `build_tool_complete`, `extract_locations`, `_parse_unified_diff_content`) |
| `acp/prompt.rs` | `content.py` |
| `acp/history.rs` | `server.py` `_history_replay_updates` |
| `acp/permissions.rs` | `permissions.py`, `elicitation.py`, `gateway_server.py` `_answer_permission` / `_answer_clarify` |
| slash commands | `commands.py`, `gateway_commands.py` |
| config options / modes | `model_catalog.py`, `modes.py`, `gateway_server.py` `set_config_option` |

Hermes's tool names differ a little from ours (`todo` vs our `todo_list`, `process` vs
`process_manage`); the tables in [PROTOCOL.md](PROTOCOL.md) use **our** names.
