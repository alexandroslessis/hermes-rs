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

## Status

Works: startup, the session list, opening a session's transcript, sending a prompt with streamed
reasoning, text and tool calls, stopping a turn; approval cards for flagged commands and clarify
questions; titles from the title model; skills and bundles in the `/` menu; rename, delete and pin;
switching a session's model (within its provider) and reasoning effort; restore-and-rerun; Settings
(`desktop-config.json`) and interface preferences (`ui-preferences.json`) in the data folder.

Not here (hidden or answering "off"): messaging, artifacts, scheduled jobs, bots, kanban, radio,
pets, voice, goals and loops, archiving, Hermes's built-in backend slash commands.
