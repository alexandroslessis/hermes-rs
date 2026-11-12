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
- **A patch in git's own format was silently skipped by the webapp build** (fixed 2026-11-12,
  Phase 6 review). `git apply` run in `target/hermes-src` finds the enclosing hermes-rs repo,
  reads a `diff --git` patch's paths as relative to *that* root and skips every file with exit
  code 0 ("Skipped patch" only with `-v`). Patches 0022-0026 are in that format, so until then
  no build had 0022 (budget profile) or 0023 (local aux models) in it, whatever their notes
  said. `build-hermes-webapp.ps1` now sets `GIT_CEILING_DIRECTORIES` to `target\` (the export
  is then not inside a repository, both formats apply relative to it) and stops on any
  "Skipped patch" in `git apply --verbose`'s output. Checking a patch by hand inside the repo
  has the same trap: set the same variable, and look for "Checking patch", not just exit 0.
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

- **Under `requests` a user message may carry a `<context>` block** (Phase 5 CP5.2): the top skills and
  memory entries the local index finds, in front of the user's text, stored with the message (the
  `preinject` note says what; the `references` note's `typed` has what the user wrote). Anything that
  shows or reuses the user's words must go through `hermes_runtime::preinject::user_text` / `strip`
  (the transcript, `/undo`, rewind, the `/review` briefing and the compaction summarizer's input do;
  session search's rows go through `hermes_store::without_attached_context`, the same cut);
  `as_concat_text()` on a user message has the block. Never move it to the system prompt
  and never rebuild it on resume: the stored bytes are what the provider's cache has seen. `/plan`,
  `/init`, `/learn`, a skill or bundle command, a scripted first step (`/review`) and `/retry` get no
  block (`@url:`/image messages do, in the same deferred step). "Already attached" counts only the
  messages the model still sees: a compaction hides the summarized ones but keeps them in the
  conversation. The index's floor for a skill to be attached is stricter than its own
  (`preinject::MIN_SKILL_SCORE` 5.0), and a message needs two search words of its own: generic
  messages score up to 4.4 on the bundled skills, and with Hermes's 230 skills "hello" reaches 5.2
  (`ascii-art`) and "explain this error" 5.2 (`anatomy`, still attached: a known miss).
- **The skills block of the system prompt has three shapes** (`skills::IndexStyle`): `Full`
  (`balanced`, Hermes's text), `Categories` (`requests`: it says the relevant skills ride on the user's
  message, so a session under `requests` that is then switched to `balanced` keeps a prompt that
  lists no skill names: the model finds them with `skills_list`), `Names` (`tokens`). Subagents always
  get `Full` instead of `Categories` (their task gets no block).
- **Turn tests under `requests`**: a message that matches a bundled skill well gets a block, so a test
  that compares the user message exactly names its profile or uses a text no skill matches.

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

- **Compression via an aux model that is down** (Phase 4 CP4.2): `summarizer_for` gives the aux
  `Summarizer` a `fallback` (the session's model) and at most one HTTP retry, so a stopped local
  server costs one refused connection, not seven seconds of backoff; the summary then costs one
  `compress` request on the main provider (as with no aux model). Only when that fails too is the
  compaction marked failed for the rest of the turn. A hung (not refused) aux server would hold the
  turn for the HTTP client's 600 s read timeout per attempt, so the aux try has its own deadline
  (`compress::AUX_SUMMARY_DEADLINE`, 300 s) before the fallback. The fallback is logged only (a
  `warn` line), not shown in the chat; the ledger shows it as a `compress` request on the main
  provider. Under `requests` with no aux model,
  compression still uses the main model: it cannot wait for idle, the turn needs the room.
- **`session.compress` answers `status: "noop"` for a real compaction**: it says "compressed" only
  when the visible message list got shorter, and a compaction hides messages and adds a notice
  and handoff instead. The token counts in the answer (`before_tokens`/`after_tokens`) are the
  truth; checks use those.
- **"Use a local model" adds providers**: when no configured provider points at the well-known
  Ollama / LM Studio port but a server answers there, `aux_models::use_local` adds it to
  `config.json` as `ollama` / `lm-studio` (via `endpoints::save`, not made the default). Servers
  are probed side by side, 3 s each at most; a configured local provider that is down is not
  offered even when it lists its models itself (`discover_models` off). A live check on a machine
  where Ollama or LM Studio runs will also see that server in `GET /api/model/auxiliary/local`.

## Taint (Phase 6 of `docs/backend/PHASES.md`)

- **What taints, and what does not.** `web_search`, `web_extract`, `browser_*` and `vision_analyze`
  on an `http(s)` image taint when the call succeeded; `@url:` references taint when a page was
  fetched (`runtime/src/lib.rs`, the deferred-message path); an MCP result taints unless the
  server's config has `"trusted": true` (not a Hermes key; errors count, the text is the server's);
  `read_file` and `search_files` taint when the path classifies `Outside` (the working folder, the
  scratch folder, the data folder and the skills/memory folders do not; persistence places such as
  `~/.ssh` are not `Outside` either); a subagent that read anything taints its parent
  (`delegate.rs`, "a subagent that read ..."). A terminal command that names a fetcher (`curl`,
  `wget`, `Invoke-WebRequest`/`iwr`, `Invoke-RestMethod`/`irm`, `Net.WebClient`...: `taint::
  command_source`, a name check) taints; other terminal output does not (build logs, `git`), nor
  does `python -c "urllib..."` or a `git clone` whose files are then read *inside* the working
  folder. `vision_analyze` on a local image outside the working folder taints like `read_file`.
  Skill and memory content (their *writes* are staged, see below), `session_search`, MCP *tool descriptions* (in every
  request from the start; tainting on them would make every session with a server careful) and
  the user's own attachments do not taint.
- **Parallel tool calls** in one assistant message run concurrently (`ToolOperation`,
  `join_all`), so a sibling call is checked before another sibling's result has tainted the
  session. That is not a hole: the model wrote every call of the batch *before* any of their
  results existed, so no call in the batch can have been steered by a sibling's result; the next
  batch, written after reading them, is checked tainted.
- **Branches.** `session.branch` (and `session.create` seeded with a parent's rows) copies the
  parent's taint to the new session (`Runtime::copy_taint`), even when the branch was cut before
  the tainting read. Rewind and `/undo` don't clear it either: conservative on purpose.
- **Two levels, one effect.** `Taint.turn` is set during the turn; at the next turn it becomes
  `Taint.session`. Both have the same effects (the plan said "a lower level" for the session one
  without naming a difference); only the wording of the question differs ("this turn read ..." /
  "earlier in this session the agent read ..."). Nothing clears the flag; a session that read
  untrusted content stays careful for good. The desktop does not configure MCP servers
  (Settings has no MCP screen yet), so `trusted` can only be set where `mcp_servers` is written
  (the frozen GPUI app's config).
- **Persistence.** The session level is `taint.json` in the data folder (`{session_id: {sources}}`),
  written when a source is read and removed when the session is deleted. Compression does not
  touch it (the tools outlive the conversation rewrite) and reopening a session loads it in
  `SessionTools::new`. A subagent's tools inherit the parent's taint, never save it, and hand what
  they read back to the parent when they finish. A damaged file reads as "clean" (it is written
  through a temp file and a rename, so only an outside hand damages it), and the desktop app and
  `hermes-acp.exe` are separate processes: two of them writing within the same moment can lose
  one entry (the in-memory taint of a running session is unaffected; only a reopen would miss it).
  Source names are one line, at most 160 characters (a path in one comes from the model).
- **What changes while tainted** (`SessionTools::check_command` / `check_edit`): a *flagged*
  command (dangerous pattern or protected path, not every command) asks every time in Manual,
  Accept edits and Auto, is never smart-approved and its answer is not remembered; the question
  carries the reason in its description ("recursive delete (this turn read a web page)"). The
  Phase 1 rule that an `Outside` write asks in Accept edits and Auto already existed, so the only
  addition there is the reason text. In Yolo nothing blocks: the action runs and
  `take_taint_warnings()` hands the line "ran after reading untrusted content: <sources>" to
  `GuardedTools`, which emits it after the call. A command that touches a persistence place asks in
  Yolo as before, with the reason added, and gives no warning.
- **The warning rides on `status.update` with `kind: "fallback"`.** The page only keeps one
  `status.update` kind in the conversation, the fallback-model switch (`status.ts`, a system line
  with the event's `text`); every other kind is ignored or drives the compaction indicator. There
  is no page-compatible "notice" event, so the warning borrows that kind rather than inventing a
  new event or a patch. The line lives in the page's session state only (it is not stored, so it
  is gone after a reload) and is also logged (`warn`). Zed (ACP) shows it as a thought. If the
  page ever treats `fallback` as more than a line, move this to a patch (`crates/desktop/patches/`).

## The staging area (Phase 6 CP6.2 of `docs/backend/PHASES.md`)

- **A tainted session does not write memory or skills.** `SessionTools::dispatch` stages every
  `memory` and `skill_manage` call while `taint().is_tainted()`, in every approval mode including
  Yolo (it asks nothing and costs no request), and the model's tool result starts "Saved for review
  by the user". A call that would have failed (a threat-scan hit, a patch whose text isn't there, a
  bad name, a full memory) is refused at staging with the tool's own message and is not kept. A
  background review of a tainted session stages its writes the same way (`ReviewTools`: it reads the
  whole conversation, untrusted text included); the curator has its own tools and a clean taint.
  Clean sessions are untouched: same code path, same request counts.
- **Where.** `staging/<id>.json` in the data folder: `{v, id, kind: memory|skill, op, diff, reason,
  session_id, sources, created, status: pending|stale, error}`. `v` is the format (1); a file from a
  newer format, a damaged file or one whose `id` isn't its name is skipped and *left on disk*. Ids are
  12 lowercase hex characters (`valid_id` accepts 8 to 32 hex, so no id can be a path or a Windows
  device name). Files are written whole to a temporary file and renamed; at most 200 files (damaged
  ones count), at most 50 from one session (so one session looping on hostile content can't crowd
  out the rest), and 1 MiB each (a larger file is not read). The very same change staged again is
  kept once (the model is told it already waits). The diff is **never cut** (approving a cut diff
  could approve a hidden tail): a change too big to hold with its whole diff is refused. What the
  user reviews (diff, summary, reason, sources, error) shows control, zero-width and bidi
  characters as `<U+XXXX>` (`staging::display_text`), and a memory diff marks every line of a
  multi-line entry, so a staged write can't make its diff read differently from what it writes.
  The diff is display only: Apply runs the payload, so a payload edited on disk (the terminal can
  reach the data folder; the file tools can't) is still validated, but its diff may not match.
  The terminal is the bigger gap anyway: a command naming the data, skills or memory folder is
  flagged (so it asks while tainted, Yolo only warns), but that is a name check a determined
  command gets past; Phase 15's sandbox is the boundary for a shell writing `MEMORY.md` directly.
- **`op` is the kind's payload**, so Phases 7 and 8 reuse the store: `memory` is `{target,
  operations}` (each patch/remove pinned to the entry it was reviewed against), `skill` is
  `{operations, origin, bases}`. A new kind needs a payload and an arm in `StagingStore::apply`.
- **Apply re-checks everything, and goes through the stores.** Memory: `MemoryStore::apply` (limits,
  threat scan, the pinned entry still there: otherwise the item is `stale`). Skills: a new
  `SkillStore::apply_staged` takes the skills write lock, compares each file the batch was reviewed
  against (`FileBase`: FNV-1a fingerprint of the file at staging, or "must not exist") and then runs
  the ordinary `apply_ops` (validators, threat scan of the whole skill, git commit, index hook). A
  changed file, a deleted or newly created skill make the item stale; any other failure leaves it
  pending; both keep the item with its `error` and write nothing (the batch is all-or-nothing). A
  patch is stale when *any* change hit its file, even one elsewhere in it: strict on purpose.
  Staging a skill batch simulates it first (`SkillStore::plan`: later ops see earlier ones), so most
  mistakes reach the model at once; Apply is still the real check.
- **The user's Apply is not tainted.** It calls the stores directly (`Runtime::apply_staged`), not a
  tool, so taint can't stage it again. Staged items outlive their session (deleting a session keeps
  its pending items) and a restart; `session_id` only says who asked.
- **One list in the page, two queues behind it (CP6.3).** The page shows `staging/` only (Settings ▸
  Memory and Capabilities ▸ Skills, patches 0024-0026; Hermes's page has no screen for its own
  `write_approval` queues). So the foreground `memory` / `skill_manage` calls of a session whose
  `memory.write_approval` (a Settings switch) / `skills.write_approval` is on now stage here too, with
  the reason "write approval is on" (`SessionTools::stages_memory` / `stages_skills`); otherwise that
  switch would fill a queue nobody can see from the page. Still on the old per-store queues
  (`pending/memory/`, `skills/.pending/`, listed by `/memory` and the GPUI app): items made before
  CP6.3 (not imported), the unattended review's memory proposals, and the review's skill writes
  while write approval is on; Phase 7 reworks the review onto `staging/`. Taint takes precedence
  for the reason shown (`this turn read ...`). The fold is in `SessionTools`, so it holds for every
  front end on the shared data folder: ACP (`hermes-acp.exe`, no review screen of its own) and the
  frozen GPUI app (whose Memory / Skills views list only the old queues) now stage too, and those
  items are reviewed in the desktop app's screens: nothing is lost, but a GPUI user won't see them
  there. `/memory` in the desktop chat counts them ("N memory change(s) wait for your review in
  Settings ▸ Memory & Context") beside the old queue.
- **The Pending section is attacker-influenced text.** The page renders diff, reason, sources and
  errors only as React text (never `dangerouslySetInnerHTML`), shows the `<U+XXXX>` forms as sent,
  truncates nothing (the diff box scrolls), shows a stale item's reason with Reject only and a failed
  Apply's message on the card. A new field added to the item must go through the same text path.
  The section renders nothing on an error from `GET /api/staging` (an older backend), never a
  guessed list.
- **`staging.*` is ours** (not Hermes's contract): `staging.list` -> `{items, count}`,
  `staging.apply {id}` -> `{id, kind, applied: true, message}` or an error saying why,
  `staging.reject {id}` -> `{id, kind, rejected: true}`; REST mirror `GET /api/staging`,
  `POST /api/staging/<id>/apply|reject` (404 unknown, 409 could not apply). The page ignores
  methods it doesn't know; the Memory and Skills screens call them since CP6.3.
- **Sessions built without a data folder can't stage** (`StagingStore::usable`): a tainted write is
  then refused, never written (fail closed). Only tests build such sessions.

## Learning signals (Phase 7 CP7.1 of `docs/backend/PHASES.md`)

- `signals/<id>.json` is written by the signal store's own thread, after the turn's events are
  sent. A test or check must read it through `Runtime::signals(id).await` (which waits for every
  change sent before it), not the file, or it races the writer.
- Only main turns (`begin_turn`) and rewinds add signals; the review, curator, subagents, `/bg`
  and `/btw` never do. Hermes's `/retry` after a provider error (`retry_turn`) is not a signal;
  the page's `/retry` (a `session.rewind` and resend) is.
- Phrases are matched in English only, on the user's own words (code, quotes, logs, pasted
  e-mails removed). A Greek or Spanish correction gives no textual signal; the non-textual ones
  (undo, denial, workaround, long turn) still count.
- Excerpts in `signals/<id>.json` are masked with `hermes_tools::redact` before they are kept; a
  test that expects a pasted key verbatim in the evidence is wrong. A terminal "workaround"
  needs the success to retry what failed (a shared word other than the program); probing
  commands (`rg`, `test -f`, `git status`…) never count, since they fail as an answer.

## The evidence review (Phase 7 CP7.2)

- Under `requests` (`review_mode = idle_once`) the scheduler (`review_schedule.rs`, CP7.3) runs
  it; `Runtime::review_once` runs it on demand and answers `Skipped("a review of this session is
  running")` while any review of the session (scheduled, agentic, `/refine`) holds it.
- Its provider never retries a 4xx (`with_transient_only`): a 400 to `response_format` is answered
  by the review itself (one text request), and a plain retry would spend the same refused
  request again. The "refused json_schema" memory is per process and keyed by base URL + model,
  and is kept only when the text request then goes through. Tests share it: a mock server can
  get a port a finished test freed, so a test that makes or checks that memory reviews under a
  model name of its own (`review_as` in `turns.rs`).
- A skill patch is checked against the fingerprint of SKILL.md **when the packet was built**: if
  the user (or the curator) edits the skill while the review request is out, the patch is refused
  ("changed after the review read it"), never merged. A diff must apply exactly: line numbers may
  be off (the exact lines are then looked up, and must occur once), the text may not.
- A skill whose shown text was cut (over 12k characters, or the room left), had a secret masked,
  or had a packet tag defused (`<skill`, `</excerpt`… become `&lt;…`) is not "shown whole": a
  full-body replacement of it is refused (it would drop the cut tail or write the mask), only a
  diff can change it, and a diff whose context includes the masked text won't apply.
- Memory operations are applied one at a time (each a one-item `MemoryStore::apply` batch), not
  as one batch: a refused one doesn't take the others down, but an add that only fits after a
  remove listed later in the reply is refused for the limit. A memory **replace or remove** is
  always staged, tainted or not (nobody watches a background review; the evidence quotes tool
  output an untainted session can still carry, e.g. a README in the project): a test expecting
  one applied directly is wrong. Only adds and skill writes of an untainted session apply.
- The packet's token count is pessimistic on purpose (`packet::estimate_tokens`: an ASCII
  character 1/3 token, other characters 1/2 token per UTF-8 byte), not `compress.rs`'s bytes / 4,
  and the budget also leaves the window room for the system prompt, the schema, the repair turn
  and a reply (`reserved_tokens`). A small window gives budget 0 and the review is `Failed`
  with no request; an unknown window keeps the 24k knob. No `max_tokens` is sent (a cap a model
  doesn't take is a 400, which would read as a json_schema refusal).
- A diff hunk that only adds lines (no context) is refused unless SKILL.md is empty; `---`/`+++`
  headers, when present, must name `SKILL.md` and never `/dev/null` as the new file.
- The packet is redacted and the prompt says the packet is evidence, not instructions; that is a
  mitigation, not the defence. The defence is that the reply can only name the contract's
  operations, every write meets the stores' threat scans and limits, and a tainted session's
  writes all go to `staging/`.

## When the review runs (Phase 7 CP7.3, `review_schedule.rs`)

- `budget.review_mode` decides: `idle_once` (the `requests` profile) = the evidence review once a
  session has been quiet `review_idle_minutes` (10) or was switched away from / closed, only if its
  signals reached `review_threshold` (3.0); `nudges` (`balanced`, `tokens`) = Hermes's counters
  **plus** the score, both starting the agentic review; `agentic` = Hermes's counters only, exactly
  as before Phase 7 (for comparison; delete it a few weeks after Phase 7). Settings ▸ Model ▸ Main
  shows it as a select only with patch 0027 in the webapp build (the backend schema alone isn't
  enough: Hermes's Settings lists only the keys its `SECTIONS` names); "" = the profile's.
- A test that counts requests around corrections must pick its mode: under `balanced` (the suite's
  default) two corrections now start an agentic review at the turn's end. `quiet_budget()` in
  `turns.rs` (`agentic`) is the "nothing on its own" budget the CP7.1/7.2 tests use.
- Under `idle_once` a quiet session's timer fires once; nothing re-fires until there is activity
  again (a turn, a rewind, the page resuming it). A smooth session costs a snapshot read, never a
  request. "Switched away" is the gateway seeing a `session.resume` (not `session.history`) or
  `session.create` of another id **on the same socket**; the left session's review waits
  `LEFT_DELAY` (3 s) so flicking back and forth starts nothing. The page's `session.close` RPC is
  only sent on delete or a drift abort and is not hooked; ACP `session/close` (`Runtime::close`) is.
- One review per session at a time, of any kind: a trigger during one is dropped (not queued); the
  signals stay unreviewed for the next idle. `/refine` is refused (an error to the user) while an
  evidence review runs, rather than silently skipped.
- The per-process back-off and the hourly cap live in memory. What survives a restart is
  `review_tries` / `review_started_ms` in `signals/<id>.json`: written just before the review's
  request, cleared when a reply is read. The first cut-off review (the app closed mid-review) is
  redone at the next idle; from the second unfinished one on the session waits 1 h, doubling to
  24 h, so a crash loop can't send a review per start. A review the governor held back (quota under
  20%) is not a failure and is tried at the next idle. The hourly cap counts starts per process: a
  restart opens a new window, but only sessions the page opens again are armed after one.
- Timers use tokio's monotonic clock: after a sleep/resume, timers that came due fire together;
  the hourly cap (6) bounds how many reviews that starts. Two windows on one session: switching
  one of them away doesn't count as leaving while the other still shows it.
- After a restart nothing sweeps old sessions: a session with unreviewed signals is reviewed only
  when it next goes idle in this process (the page opening it counts). Deliberate: no startup burst.
- `nudges`: a finished agentic review of memory **and** skills marks the signals it saw (read
  before its conversation snapshot) reviewed, so the score doesn't restart it after every turn; a
  memory-only counter review marks nothing. The agent loop's `ExitOnError` ends a failed review
  *normally* (the error is the last message), so `review.rs` checks that message: a review that
  ended on an error covers nothing. A score-started review that didn't cover the signals holds the
  score trigger back for an hour (`Scheduler::nudge_due`), else a failing review would be started
  again after every turn.
- A "remember this" the agent already saved with its `memory` tool (add/patch, not refused) in the
  same turn is recorded as a weak signal (1.0), not an explicit one, so it doesn't force a review
  that would only find the entry there.
