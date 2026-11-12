# Pitfalls

Things that cost hours once. Read this before debugging; check here first when something is odd.

Two windows on one data folder (the desktop app and an editor's `hermes-acp.exe` are separate
processes):
- **Shared, live:** `sessions.db` (sessions, messages), memories, skills, `config.json` (read when
  used). A thread made in one is in the other's list.
- **One process each:** a session's cached conversation (reloaded from the store when stale, see
  below), its effort pick, its mode (`ApprovalMode`), its todo list, the approvals' "always
  allowed" list (`command_allowlist.json` is read at start), running processes.
- **Last writer wins** (small JSON side files, rewritten whole): `archived-sessions.json`,
  `branches.json`, `generation-stats.json`, `session-workspaces.json`, `ui-preferences.json`,
  `hermes-imports.json`.
- **One turn per thread across windows:** `hermes_runtime::turnlock` holds
  `<data>/running/<session id>.lock` open without sharing while a turn runs; the other window's
  turn is refused with a readable error. Don't "fix" it with a pid file: the OS closing the handle
  when a process dies is what keeps a crash from leaving the thread locked.
- **Rewinds take the thread's lock too** (`Runtime::rewind`): `/undo`, `/retry`, the page's edit
  and resend all rewrite rows, so they are refused while another window runs a turn on the thread.
- **Message rows are keyed by position** (`upsert_message(session, seq, …)`): a process writing
  from a conversation that is behind the store overwrites the other's rows. `reload_if_stale`
  compares count and last message id at turn start, `/compress`, `messages()` and `rewind()`;
  any new write path on a cached conversation must do the same.

ACP mode (`--acp`, [docs/acp/](acp/README.md)):
- **The effort picker shows what is sent** (`settle_effort`): a thread with nothing picked gets
  Settings' effort, else medium, picked for it when it opens. (The page still sends nothing for an
  unpicked effort.) A model that rejects `reasoning_effort` needs a provider that drops it.
- **Live terminal output** (`translate.rs` `tool_progress`): `tool.progress` carries the output's
  *tail* (a sliding window), Zed's `terminal_output` *appends*, so only the new part is sent
  (`new_part` lines the old tail up in the new one; overlapping matches count, the last one wins),
  and the end sends what the last progress hadn't. Line ends go out as `\r\n` for the terminal.
- **`session/close`** stops the thread's turn, shell and processes (`Runtime::close`); the thread
  stays loadable. **`session/resume`** is `session/load` without the replay (Zed still shows it).
- **Editors that start a fresh agent per thread** (t3code) see the model picker only in the
  `session/new` answer: the list is cached on disk (`acp-model-options.json`), not just in memory.
  They also probe with test sessions: threads a connection made are deleted when closed or left
  empty (`forget_if_empty`), or they pile up in the desktop app.
- **`session/new` and `session/load` must not wait on the network.** The editor blocks on their
  answer, and `model.options` asks the provider for its model list (seconds: it made a thread take
  ~3 s to open in Zed). `AcpServer::controls` is instant (cached list, else the thread's own model);
  `refresh_model_options` fetches in the background on every open and sends a
  `config_option_update` when the list changed (so a provider added in Settings appears). Time it with
  `scripts/acp/acp-probe.mjs` (the ms column) on a copy of the real `config.json`.
- **Zed shows only what the agent lists.** Modes must also be a config option (`category: "mode"`,
  `controls::mode_option`), or Zed shows no mode selector; a slash command must be in
  `available_commands_update` (`controls::GATEWAY_COMMANDS` for the ones `slash.exec` runs that the
  catalog doesn't list), or Zed refuses it before it reaches us.
- **PowerShell tables were padded to 4096 columns** (`Out-String -Width 4096`, there so long lines
  aren't cut): each line is `TrimEnd`ed in `run_powershell`. A command that doesn't parse (`&&` in
  Windows PowerShell 5.1) prints nothing on stdout; its CLIXML stderr is turned into the result.
- **A subagent inherits its thread's mode** (`delegate.rs` `run_child` → `set_mode`); it can't
  ask (`ChildBridge` denies), so in Manual or Plan what would ask is refused. Without this, Plan
  mode was escaped by delegating.
- **Plan mode's `.hermes/plans/` exception compares resolved, `..`-free paths** (`lexical`,
  `is_within` in `tools/src/lib.rs`): a substring check let `.hermes/plans/../../src/x` through.
- **Cards in Manual / Plan offer "Allow once" and "Deny" only** (`CommandApproval::once_only`,
  the hub's `choices`): those modes ask every time, so "for session" / "always" meant nothing.
- **Modes are `hermes_tools::ApprovalMode`, per session** (`SessionTools::mode`; none picked →
  follows Settings' `approvals.mode`). `check_command` asks for every command in Manual/Plan;
  `check_edit` (write_file/patch) asks in Manual and refuses in Plan.
- **Terminal output needs Zed's terminal card.** When the client announces
  `clientCapabilities._meta.terminal_output`, a `terminal` call is a terminal card
  (`_meta.terminal_info` on the call, `terminal_output` + `terminal_exit` on the result), else a
  text card. Unverified in Zed 1.26 until the user checks (checklist Phase 2, item 1).
- **"Editing previous messages is not available"** (Zed, on a hermes thread): checked in Zed 1.27
  (2026-11-11): its `AgentConnection::truncate` defaults to "not supported" and neither the v1
  nor the beta v2 ACP connection implements it; ACP v1 has no rewind request (the `session/rewind`
  proposal was closed, superseded by the v2 prompt lifecycle). So the edit button can't work for
  us yet. `/undo [N]` and `/retry` (`AcpServer::rewind_command`, over `session.rewind`) are the
  stand-in; ACP can't delete messages from the editor's view, so after `/undo` Zed still shows
  them until the thread is reopened. If Zed gains truncate for external agents: map its message
  ids to our rows (side file), call `session.rewind` on truncate, then the edited prompt is a
  normal `session/prompt`.
- **Editors need `hermes-acp.exe`, not the release `hermes-desktop.exe`.** The release app is a
  Windows GUI-subsystem program; launched by Zed with pipes it exits (code 0) right after
  `initialize`, while the debug build (a console program) works. `hermes-acp.exe`
  (`src/bin/hermes-acp.rs`, a console program over the `hermes_desktop` library) is built and
  shipped by the installer (Tauri bundles every bin of the package). `tests/acp.rs` runs it.
- **stdout is the protocol.** `main.rs`'s `init_logging` writes to stdout, so `--acp` returns
  before it and logs through `acp::init_logging` (stderr + `logs\acp.log`). One stray `println!`
  breaks the editor; `tests/acp.rs` `the_stdout_rule_holds_through_a_whole_tool_turn` guards it.
- **Call the gateway through `Gateway::call`**, not `dispatch`: a started turn's events wait for the
  page's admission receipt, which only `handle` (the socket path) and `call` release.
- An event for a session the gateway's `Hub` doesn't route to us is dropped, and an approval
  nobody sees is denied. `session.create` / `session.resume` / `prompt.submit` subscribe our outbox.
- **Tests that need a flagged command** use PowerShell syntax (`Remove-Item -Recurse -Force`): the
  terminal tool runs PowerShell unless `desktop-config.json` says bash, and `rm -rf` is not valid there.
- **The test model must tell a title request from a turn** (a turn offers tools, a title doesn't);
  both are chat completions, and a title request that eats a scripted reply fails the next turn.

## Shell and tooling (Windows)

- **Two shells.** The Bash tool is Git Bash; PowerShell is separate. Git Bash's `tar` can't handle
  `C:\` paths, so `scripts/build-hermes-webapp.ps1` must run in PowerShell. From Git Bash, run it
  as a child process and send output to a file, because npm writes warnings to stderr and that
  fails PowerShell's `&` operator:
  `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-hermes-webapp.ps1 *> target/webapp-build.log`
- **`python` is the Microsoft Store stub.** Use `~/.local/bin/uv.exe run --no-project python -I …`
  (add `--with <pkg>` when needed).
- **Node isn't on PATH.** `tools/node/node.exe`. The installer build needs it on PATH:
  `$env:PATH = "$PWD\tools\node;$env:PATH"`.
- **No `gh`.** GitHub work goes through plain `git` (see RELEASING.md for pushing a branch made
  with plumbing).
- **sed / heredocs corrupt Rust and regexes.** Escapes in char literals (`'\\'`), `\r\n`, `$`
  and backticks get mangled. Use the Edit tool for source changes. A `node -e` replace script
  that throws writes nothing — check its output.
- **Python edit scripts write CRLF on Windows** (`Path.write_text` translates `\n`). Git
  normalizes to LF on commit, so the commit is clean, but the working copy changes; pass
  `newline="\n"` (`open(..., "w", newline="\n")`) to avoid it. Check `git diff --cached --stat`
  before committing: a whole-file change means line endings.
- **Relative `CARGO_TARGET_DIR`** is resolved against the cwd: running cargo from `crates/x/src`
  with `target/claude` created a 3.4 GB `crates/desktop/src/target`. Use an absolute path or run
  from the repo root.
- **A rebuilt frontend isn't served until `cargo build` runs again.** Tauri's build step copies
  `dist-webapp` (the bundle's `resources`) into `<target>/<profile>/webapp/`, and the exe
  prefers that copy (`main.rs` `webapp_dist`). After `build-hermes-webapp.ps1`, run
  `cargo build -p hermes-desktop` before restarting the instance, or it serves the previous
  page. Check with `cdp.mjs "performance.getEntriesByType('resource').map(e => e.name)"`
  against the hashed file names in `dist-webapp/assets`.
- **A patch in git's own format is silently skipped by the webapp build.** `git diff
  --no-index` emits `diff --git` and `index` lines; the build's `git apply` (run inside the
  hermes-rs repo, on the non-repo `target/hermes-src`) then skips every hunk and exits 0 —
  the build prints "Applied" but the page still serves the old code, and `--check` says
  "Skipped patch" instead of "Checking patch". Write patches in the traditional `diff -u`
  format (`--- a/<path>` / `+++ b/<path>`, no `diff --git`/`index` lines — strip them with a
  byte-safe `node -e` filter when the diff came from git), and confirm a fresh `--check`
  prints "Checking patch".
- **`cargo test` doesn't rebuild the app binary.** Run `cargo build -p hermes-desktop` before a
  live check, or you're testing the old exe.
- **The user's build locks `target/debug`.** Always `target/claude`.
- **Stopping processes:** only ones whose path is under `target\claude` (`perf.ps1 -Do stop`).
  `taskkill /im hermes-desktop.exe` would kill the user's app.
- **The sandbox's `%APPDATA%\hermes-rs` may be a stale view.** Don't diagnose the user's data
  through it; ask the user, or reproduce on your own instance.

## Rust

- **E0618 "expected function"**: a local variable shadows a function of the same name
  (`let detail = …; detail(…)`, also hit with `admission_id`, `tree`). Rename the variable.
- **Borrowed temporaries in spawned futures**: build owned values (e.g. `ChildTask`) *inside* the
  `async move` block, or clone before it; `tokio::spawn` needs `'static`.
- `get_tool_response_ids` returns a `HashSet`, not a `Vec`.
- rmcp's content type is `ContentBlock`, not `Content` (older docs and Goose code say `Content`).
- `Role` comes from `rmcp::model::Role` in message code.
- Holding a `std::sync::Mutex` guard across `.await` makes the future `!Send` (compile error in
  `tokio::spawn`) or deadlocks; scope the guard in a block.

## Tauri and WebView2

- **Window icon:** tauri-codegen 2.6 uses the `.ico`'s *first* entry as the window icon, so the
  taskbar was blurry. `main.rs` `set_window_icons` sets `WM_SETICON` big/small with
  `LoadIconWithScaleDown` at the window's DPI. The exe icon comes from `icons/icon.ico` via the
  build script; rebuild it with `scripts/make-app-icon.py` from Hermes's `icon-dark.png`.
- **Drag and drop:** Tauri's own drop handler swallows HTML5 drops; it's disabled in `main.rs`
  (`disable_drag_drop_handler`) so files reach the page.
- **Zoom:** CSS zoom misplaces Radix popovers (they open at the wrong place, clipped). Zoom is
  WebView2's native zoom through the window bridge (patch 0010).
- **localStorage is per origin**: the port must be stable (`DEFAULT_PORT` 47731) or the page loses
  its local settings. A second instance gets a random port and empty local settings, by design.
- **The page uses a HashRouter.** Navigate with `location.hash = '#/settings'`; `pushState` does
  nothing.
- **The page sees this backend as a remote gateway** (`isRemoteGateway()` is true in the
  browser build): actions on files take Hermes's remote paths (download a copy over
  `/api/fs/download`, ignore `file:` links). Where opening on this machine is right, a patch
  asks our backend first (0013, `open_file.rs`).
- **Native dialogs** (folder pick, save as) come through patches 0002/0008 and the window bridge;
  the browser's own pickers don't give paths.

## Protocol and page behaviour

- **-32601 / 404 hide features.** If you answer a method with something half-right, the page shows
  a broken feature instead of hiding it. Serve it completely or not at all.
- **Error bodies**: REST errors must be `{"detail": "..."}`; anything else shows as "[object
  Object]" or a generic error.
- **Settings sub-pages** are hidden by `data-tour` handle in `hide.css`; unhide only the ones you
  serve. `nav-config:<section>:<page>` for sub-pages, `nav-config:<section>` for sections.
- **The page reads some settings from `/api/config` once** at startup; after changing defaults in
  `rest.rs`, restart your instance.
- **Hermes's frontend checks capabilities** through `client.capabilities`, `setup.status` and
  feature probes (a 404 on a probe route). When a screen stays empty, look for the probe in the
  wire log.

## Runtime and agent behaviour

- **ConPTY asks for the cursor position** (`ESC[6n`) when a shell starts and waits for the
  answer. Tests that drive the terminal must reply `ESC[1;1R`, and must *not* reply again during a
  replay of the buffer (it would type into the shell).
- **git's default branch:** the machine's global `init.defaultBranch=master` made `main` invisible
  to a check; the configured default now counts only if that branch exists. Tests set the branch
  name explicitly.
- **git prints "nothing to commit" on stdout** with exit code 1; don't treat stdout as success.
- **Memory drift guard:** after the user lowers the memory size limit, writes are refused while any
  existing entry exceeds the new limit (by design, Hermes's behaviour). Tests that lower limits
  must account for it.
- **Disabled tools:** a call to a switched-off tool must say it was turned off by the user, not
  that the tool doesn't exist (the model would otherwise invent workarounds or retry).
- **Prompt caching:** changing the system prompt, tools or earlier messages mid-session breaks the
  provider's cache and costs the user money/time. Apply such settings from the next turn/session.
- **Timing-flaky test:** `an_exhausted_budget_ends_with_a_toolless_summary` fails under heavy
  machine load; re-run alone.

## Live checks

- **The perf instance keeps its data** (`target/perf/home`) between runs: use unique names and
  find your own session by id. Old sessions pile up; that's fine.
- **Port 47900** is shared by `mock_llm.py` and every check's embedded provider; only one at a
  time.
- **The importer needs `HERMES_HOME`** set before the instance starts, pointing at a home made by
  `make-hermes-db.py` (built with Hermes's own `SessionDB`, so the schema is real).

## Data and Hermes sources

- **`prompts/hermes/SOURCE.md` line 1** records the commit of the tree the extractor ran on; run
  against `target/hermes-src` it writes *this* repo's commit. Set it to `HERMES_COMMIT`.
- **`target/hermes-src` has our patches applied.** To see Hermes's original file, read
  `../hermes-agent` at `HERMES_COMMIT` (`git -C ../hermes-agent show <commit>:<path>`).
- **No Hermes install on this machine**; import tests use generated homes.
- **Hermes keeps two views of a session.** Rows its compression folded away stay as
  `active = 0, compacted = 1` (shown, not sent), its summary is a row with
  `_compressed_summary = 1` (sent, not shown), and only `active = 1` rows go to the model.
  Importing the display view as the model's context made a long Hermes session's first turn here
  re-summarize everything with no visible progress: "loads forever" (fixed 2026-11-05 in
  `hermes_import.rs`, imported as user-only / agent-only messages). Any other reader of Hermes's
  database must make the same split.
- **Pass the model's window.** Until 2026-11-05 no desktop turn set `TurnOptions.context_length`,
  so compression assumed the runtime's 128k default (threshold 96k) whatever the model held: a
  32k local model overflowed first. Build options with `Gateway::turn_options`, never
  `TurnOptions::default()`.
- **Compression must be visible.** A turn that compresses first emits `status.update`
  `compacting` / `compacted` (the page's "Summarizing thread" indicator); without it a long
  compression on a local model looks like a hung turn (Hermes's own comment in
  `tui_gateway/server.py` `_status_update`).
- **The store is shared with the GPUI app**: a schema change breaks the user's other app. Side
  files only.

## Path policy and the shell environment (Phase 1 of `docs/backend/PHASES.md`)

- **File tools refuse some folders in every mode.** `write_file` / `patch` into the app's data
  folder (`HERMES_RS_HOME`: settings, `sessions.db`, approvals) or its `skills` / `memories`
  folders fail with a message naming the right tool (`skill_manage`, `memory`). Yolo doesn't lift
  it. A test that writes there with a file tool must go through the real tool instead.
- **Persistence places always ask**, in every mode including Yolo, and "always allow" is not
  remembered: startup folders, `$PROFILE` scripts, `~/.ssh`, `~/.gitconfig`, `~/.bashrc`-style
  rc files, `.git/hooks`, `.git/config`, scheduled-task folders. Editing a repo's `.git/config`
  from the agent is a normal-looking request that now asks.
- **Auto (smart approvals) now asks for edits outside the workspace.** It used to let every file
  edit through; AcceptEdits and Auto behave the same here, Yolo allows them.
- **The terminal check is token matching, not a sandbox.** It looks at quoted or bare words in the
  command (`~`, `$HOME`, `$env:APPDATA`, `%USERPROFILE%` expanded), so an unquoted path with spaces,
  a path built in a variable or a script file's contents get past it. Reads are flagged as well as
  writes (`cat ~/.ssh/config` asks). The real boundary is the OS sandbox of Phase 15.
- **The agent's shell has no secrets in its environment.** Variables whose name contains `KEY`,
  `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or `AUTH` are removed before `bash` /
  PowerShell starts, so a tool that needs one (`gh`, `cargo publish`, `ssh-agent`:
  `SSH_AUTH_SOCK`) fails with its own "not logged in" error. Keep a variable by listing its name
  in `terminal.env_passthrough` (Settings config record, case-insensitive). `GIT_AUTHOR_*` is
  kept (`AUTHOR` is not `AUTH`). MCP stdio servers are not scrubbed at all: they inherit the
  app's whole environment plus their configured env (many read their key from an inherited
  variable; scrubbing them would break them silently).
- **Scripts in skills are flagged.** A command that runs a file under
  `skills/*/scripts/` asks ("runs a script the agent wrote into a skill") unless the file is
  byte-for-byte the copy bundled with the app (compared with the embedded one, so editing the
  extracted copy or the manifest doesn't pass).
- **`SessionToolsConfig` has a `data_dir`**; a session built without it (empty path) gets no
  AppData / skills / memories protection.

## Request ledger and governor (Phase 2 of `docs/backend/PHASES.md`)

- **A model request made outside a request context is not counted.** The context is a tokio
  task-local: `tokio::spawn` does *not* carry it (like a thread not inheriting a Python
  `contextvar`). Start a task that calls the model with `observer::spawn_with_purpose` /
  `spawn_in_context`, or wrap its future in `Ledger::scoped(purpose, session, fut)`. A POST from a
  labelled client with no context logs one `warn`; a client without a provider id (Ollama's
  `api/show` during discovery) is never counted.
- **Counts are per HTTP attempt**, so a retried request counts once per try and a connection
  failure counts too. Only POSTs count; model-list GETs don't.
- **`requests.json` is written at most every 2 s and when the app exits.** Tauri's `run` never
  returns, so the flush is on `RunEvent::Exit` (`main.rs`), and `hermes-acp.exe` flushes when the
  editor closes stdin; a hard kill loses up to 2 s of counts.
- **The governor reads a snapshot that may lag the last few requests**, so at the boundary one
  extra request can slip through; the provider's own 429 is the hard stop. The main turn is
  refused only when the limit is used up; review, curator and titles wait below 20% left, and
  delegation and the smart-approval judge stop below 5%. `/review` starts with a scripted
  `delegate_task`, so below 5% it runs without the child.
- **A daily limit has three sources**: the endpoint's own field (Settings ▸ Providers; needs patch
  0021 and a webapp rebuild), `requests.limits.<provider id>.daily_limit` in the config record,
  and the "N of N" a daily-limit 429 states (learned, used only when none is set; the latest wins).
  When both are set, the config record's limit wins over the endpoint's (`gateway::provider_limits`);
  a null or 0 there leaves the endpoint's in force. The config record **merges** what is set into what is there, so a limit is removed by setting it
  to `null` (`config.set` key `requests.limits.<id>`, value `{"daily_limit": null}`), not by
  sending `{}`.
- **A day starts at the provider's midnight as a fixed UTC offset** (`reset_offset_minutes`, default
  UTC), not a named time zone: a provider that follows daylight saving needs the offset changed
  twice a year.
- **The `requests.status` / allowance gauge are ours**: the page's gauge is Hermes's "plan usage"
  item (`plan_gauge` in `session.usage`), shown as "Requests 312/800 39%" with the reset time in
  its tooltip; patch 0021 points its click at Settings ▸ Providers instead of the (hidden) Billing
  tab. The gauge is sent only with `session.usage` / `message.complete`, so a changed limit shows
  from the next turn. The warning near zero goes out as
  `status.update` `{kind: "quota"}`, a kind the page ignores.
- **ACP shows no quota**: `usage_update` carries tokens (`used`, `size`) only.

## Budget profiles (Phase 3 of `docs/backend/PHASES.md`)

- **The default is `requests`, not today's behaviour.** A config without `budget.profile` (every
  existing install) runs `requests` caps: 150 iterations, 2 strikes for invented or cut-off tool
  calls, compression at 90%/85%, 3 subagents at once. `balanced` is the old constants. The `turns`
  harness pins `balanced` unless `HERMES_TEST_PROFILE` says otherwise, so a test passing there
  says nothing about the user's default: name the profile with `harness_profile` when it matters.
- **Knobs apply from the next turn; the wording from the next session.** The budget paragraph in
  the system prompt (and `tokens`' names-only skills block) is chosen at `create_session` and
  stored with the session, so switching the profile doesn't change a running or resumed session's
  prompt. Sessions created before Phase 3 have no budget paragraph at all. A compaction or a model
  switch rebuilds the prompt anyway (cache miss either way) and then takes the profile in force.
- **Tool schemas state no profile-dependent number** (`read_file`'s `limit`, `delegate_task`'s
  width): a schema rides in every request of every session while the profile can change. Don't add
  "default: N" back; the tool result and the delegate refusal state the real limit.
- **Bad overrides**: `config.set budget.<knob>` refuses them (`-32602`), `PUT /api/config` drops
  them silently, a hand-edited `desktop-config.json` value out of range is ignored (the profile's
  value stays). `ACP` reads the profile once at start: a switch in the desktop reaches a running
  `hermes-acp.exe` only after the editor restarts it.

- **A background review outlives the turn that follows it** (Phase 4). It works on a private copy
  of the conversation (`persist: false`) and may only call memory/skill tools, which serialise on
  their own mutexes; it takes that copy under the session's `running` lock, so it never copies a
  half-written turn, and a turn starting first makes it wait. Its `read_file` calls run under
  `files::SIDE_READ`, so they don't satisfy the turn's read-before-write guard. It shares
  `SessionTools` with the turn otherwise: the per-turn memory failure counter is reset by the
  turn's `begin_turn` and counts the review's failures too. Its usage rows go to the same
  session in `sessions.db`, so the context gauge (last row's input tokens) can briefly show the
  review's. Tests that start a turn right after one finishes must not assume the review has
  already taken its snapshot.
- **Under `requests` nothing side-job-shaped reaches the main model unasked**: no title request
  (heuristic title) and no approval judge without an aux approval model. A test that expects a
  model title or a judge request pins `balanced` (the ACP tests write
  `desktop-config.json` with `budget.profile`).
