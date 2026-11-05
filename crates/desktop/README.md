# hermes-desktop (prototype)

Hermes's own desktop frontend in a Tauri window, running on the hermes-rs backend instead of
Hermes's Python one. The frontend is Hermes's unmodified webapp bundle (the desktop app built for
the browser); this crate serves it and speaks the protocol it expects:

- `src/server.rs`: a local HTTP server for the bundle, with the globals Hermes's own server injects
  (`hermes_cli/web_server_dashboard.py`), the session token, and plugin defaults.
- `src/gateway.rs`: Hermes's `tui_gateway` JSON-RPC over `/api/ws` (`tui_gateway/ws.py`), mapped
  onto `hermes_runtime`. Shapes follow `apps/shared/src/gateway-contract.openrpc.json`.
- `src/rest.rs`: the REST routes under `/api` the frontend needs at startup.
- `src/transcript.rs`: our messages as Hermes's transcript and stream events.
- `plugins/hermes-rs/plugin.js`: a desktop plugin (Hermes's plugin API) that hides features this
  backend doesn't implement.

Anything not implemented answers Hermes's own "not here" signals (JSON-RPC -32601, REST 404 "No
such API endpoint"), which the frontend treats as an older backend without that feature. Every such
call is logged under the `hermes_desktop::missing` target, which is the to-do list.

## Running

1. Build Hermes's webapp once (needs Node 26+ in `tools/node`):
   `powershell -File scripts/build-hermes-webapp.ps1`
2. `cargo run -p hermes-desktop`

It uses the same data folder and settings as the GPUI app (`HERMES_RS_HOME` to point elsewhere).
`HERMES_DESKTOP_PORT` fixes the local server's port, so a browser can open the same page while
debugging.

## Installer

After step 1 above, from `crates/desktop` (Node in `tools/node` on the `PATH`):

    npx -y @tauri-apps/cli@2 build

It builds the release app and an NSIS installer in `<target>/release/bundle/nsis/`
(`Hermes_<version>_x64-setup.exe`): a per-user install (no admin prompt) that carries the webapp
next to the exe (`webapp\`) and fetches WebView2 if the computer lacks it. Data stays in the same
data folder as a developer's run, so an installed and a built copy share settings and sessions.

## Sessions from the original Hermes

The Import screen (command palette, Ctrl+K ▸ "Import session") lists the sessions of the original
Hermes on this computer, from every profile (`HERMES_HOME`, else `%LOCALAPPDATA%\hermes`, else
`~\.hermes`), with a preview and one-click import; `/import-hermes` imports all of them. Hermes's
database is only read. An imported session keeps its title, folder, times and conversation (text,
tool calls and results; reasoning isn't carried over) and talks to the current default endpoint
and model. A session Hermes had compressed keeps that compression: its whole history is shown,
and the model gets Hermes's summary and what followed, as it did in Hermes. Each is imported once (`hermes-imports.json` in the data folder remembers them).
`src/hermes_import.rs`; `scripts/perf/import-check.mjs` checks it against a home built by
Hermes's own code (`scripts/perf/make-hermes-db.py`).

## Status

Works: startup, the session list and search, transcripts, prompts with streamed reasoning, text
and tool calls, stop, queue and steer; approval cards and clarify questions; titles; skills and
bundles in the `/` menu; rename, delete, pin, branch; model and effort per session;
restore-and-rerun, `/undo`, `/retry`, checkpoints and `/rollback`; built-in commands (`/compress`,
`/status`, `/diff`, `/usage`, `/insights`, `/context`, `/tools`, `/save`), coding commands
(`/plan`, `/init`, `/review`, `/learn`), `/btw`, `/bg`; custom endpoints and keys in Settings;
attachments (images, files, paste, drop); the side panel's file preview/edit, git review and
terminal; live subagents; Capabilities ▸ Skills and Tools; the Star map and Settings ▸ Memory;
Command Center (maintenance, curator, logs, insights); importing Hermes sessions; the installer.

Not here (hidden or answering "not here"): MCP connectors, goals and loops, archiving, auxiliary
model settings, fallbacks, artifacts, scheduled jobs, messaging, bots, kanban, pets, voice. The
list with evidence is [GAPS.md](../../GAPS.md); how to build each is
[docs/GAPS-PLAYBOOK.md](../../docs/GAPS-PLAYBOOK.md).
