# hermes-rs in Zed

Zed runs ACP agents ("external agents") from its settings. Checked against Zed 1.26.2 on this
machine (`%LOCALAPPDATA%\Programs\Zed`) and Zed's source (`../zed`,
`crates/settings_content/src/agent.rs` `CustomAgentServerSettings`).

## Setup

1. Build the exe (or use the installed one):
   ```powershell
   $env:CARGO_TARGET_DIR = "target/claude"
   cargo build -p hermes-desktop
   ```
2. In Zed: command palette → `zed: open settings file` (`%APPDATA%\Zed\settings.json`), add:
   ```jsonc
   "agent_servers": {
     "Hermes (rs)": {
       "type": "custom",
       "command": "C:\\Users\\alexandros\\repos\\hermes-rs\\target\\claude\\debug\\hermes-desktop.exe",
       "args": [],
       "env": {
         // Optional. Without it the agent uses the desktop app's data (%APPDATA%\hermes-rs):
         // the same sessions, memory, providers. Point it elsewhere to experiment safely.
         // "HERMES_RS_HOME": "C:\\Users\\alexandros\\repos\\hermes-rs\\target\\acp-home",
         "RUST_LOG": "info"
       }
       // Phase 4: "default_mode": "default",
       //          "default_config_options": { "effort": "medium" }
     }
   }
   ```
   For the installed app, `command` is `hermes-acp.exe` in the install folder, next to the app
   (`C:/Users/<you>/AppData/Local/Hermes/hermes-acp.exe`), with no args. **Not**
   `hermes-desktop.exe --acp`: the release `hermes-desktop.exe` is a GUI program without a console,
   and Zed on Windows cannot drive it (it exits right after `initialize`, "Server exited with
   status exit code: 0"). `hermes-acp.exe` is the same agent as a console program (found 2026-11-10).
3. Agent panel (right dock) → `+` / new-thread menu → **New Hermes (rs) Thread**.

The data-folder choice matters: with the default (the desktop app's folder), a thread made in Zed
appears in the desktop app's sidebar and vice versa. Don't send prompts to the **same** thread
from both apps at once (README decision D5; Phase 5 may add a guard).

## Debugging

- **Zed's view of the traffic:** command palette → `dev: open acp logs`. Every JSON-RPC message
  in both directions. This is the first thing to look at when Zed shows an error.
- **Our log:** `<data folder>\logs\acp.log` (and stderr, which Zed keeps in its own log:
  command palette → `zed: open log`).
- **Zed caches the agent process** per window: after rebuilding the exe, close the threads and
  restart Zed (or the agent fails to start because the exe is locked by the old process: the
  build reports "Access is denied" — close Zed's Hermes threads first).
- More detail on our side: `"RUST_LOG": "info,hermes_desktop::acp=debug"` in the `env` above.

## Checklists for the user (one per phase)

Each item: what to do → what should happen. Report back the item number and, on a failure, the
relevant part of `dev: open acp logs`.

### Phase 0
1. Add the settings above, reopen the Agent panel's new-thread menu → "New Hermes (rs) Thread" is listed.
2. Start one → Zed shows an error that the agent doesn't support creating sessions (expected
   until Phase 1), not "failed to start" or a timeout.
3. `dev: open acp logs` → an `initialize` request and our answer with `"name":"hermes-rs"`.

### Phase 1
1. New Hermes (rs) thread, type `Say hello in five words.` → the reply streams in word by word.
2. With a reasoning model: the thinking shows as a collapsible "Thinking" block above the reply.
3. Ask for something long (`Write a 500-word story.`), press Stop mid-way → it stops within a
   second or two; the thread is usable again.
4. Open the desktop app → the thread is in its sidebar with the same messages.
5. Set `HERMES_RS_HOME` to an empty folder, restart Zed, start a thread → a readable error
   telling you to set up a provider in the desktop app.

### Phase 2
1. `List the files in this folder.` → a tool card titled with the command, terminal icon, and the
   output inside it when expanded.
2. `Create hello.txt containing "hi".` → an edit card with a diff (all lines added) and the file's
   path, clickable to open it.
3. `Change "hi" to "hello" in hello.txt.` → a diff with the line changed.
4. `Delete the folder C:\temp\acp-test recursively.` (create it first) → Zed asks for permission
   with Allow once / Allow for session / Allow always / Deny. Deny → the folder is still there and
   the agent says it was denied. Ask again, Allow once → it's deleted.
5. `Make a todo list of 3 steps for adding a README, then do them.` → Zed's plan panel shows the 3
   steps and ticks them off.
6. `Ask me which language I prefer (Rust or Python) before answering.` → Zed shows the question
   with the choices; the agent continues with your choice.

### Phase 3
1. Restart Zed, open the thread history (Agent panel) → your Hermes threads for this project are
   listed with titles.
2. Open an old one → its messages and tool calls are shown; continue it → the agent remembers.
3. Type `/` → hermes commands are offered (`/compress`, `/usage`, …); `/usage` answers without a
   model call.
4. `@`-mention a file and ask about it → the agent answers about that file's content.
5. Paste a screenshot and ask what's in it → described (with a vision model) or read through
   `vision_analyze`.
6. The context-usage indicator shows a value after each turn.

### Phase 4
1. The model picker in the thread lists the models the desktop app offers; pick another → the
   next reply comes from it (ask "which model are you?" if the model knows).
2. The effort selector changes the reasoning effort (visible with a reasoning model).
3. Mode menu → YOLO → a flagged command runs without asking; back to Default → it asks again.
