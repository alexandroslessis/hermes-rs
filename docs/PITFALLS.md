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
  its local settings. A second instance on another data folder (`HERMES_RS_HOME`, the perf
  instance) gets a random port and empty local settings, by design; a second one on the same data
  folder says "Hermes is already open" and exits (`hermes_core::lock`, native UI CP1.1).
- **The page uses a HashRouter.** Navigate with `location.hash = '#/settings'`; `pushState` does
  nothing.
- **The page sees this backend as a remote gateway** (`isRemoteGateway()` is true in the
  browser build): actions on files take Hermes's remote paths (download a copy over
  `/api/fs/download`, ignore `file:` links). Where opening on this machine is right, a patch
  asks our backend first (0013, `open_file.rs`).
- **Native dialogs** (folder pick, save as) come through patches 0002/0008 and the window bridge;
  the browser's own pickers don't give paths.

## Native UI (`crates/native`, GPUI Kit; [docs/native-ui/](native-ui/README.md))

- **Debug builds overflowed the main thread's 1 MB stack** before the first frame ("thread 'main'
  has overflowed its stack"). `crates/native/build.rs` links the binaries with an 8 MB stack, as
  Zed does. Tests were unaffected (test threads have their own stacks).
- **`#[gpui_kit::test]` and a glob import:** `use gpui_kit::*` (directly or through `use
  super::*`) brings GPUI's `test` attribute, which shadows the built-in `#[test]` the macro expands
  to ("recursion limit reached while expanding `#[test]`"). Test modules import
  `core::prelude::v1::test` explicitly, or write `#[core::prelude::v1::test]`.
- **Headless means fake text on Windows.** `gpui_kit::platform::current_platform(true)` is
  `WindowsPlatform::new(headless)`, whose text system is GPUI's `NoopTextSystem` (fixed advances,
  no glyphs), and `TestAppContext` (`#[gpui_kit::test]`) uses the same fake. Layout of text is
  then wrong and nothing is drawn. For real DirectWrite, take the text system of
  `current_platform(false)` (`testing::text_system()`; no window is opened) and build a
  `HeadlessAppContext` with it (`testing::headless_app()`).
- **Off-screen rendering on Windows** goes through `gpui-pre-wgpu`'s `WgpuHeadlessRenderer`
  (Vulkan or DirectX 12; it picked the NVIDIA card over Vulkan here). `gpui-pre-platform`'s
  `current_headless_renderer()` returns `None` on Windows and GPUI Kit's own `rendering` tests
  skip Windows, but the wgpu renderer builds and renders there; `crates/native` depends on it
  directly under the `snapshots` feature. GPUI's test windows have a scale factor of 2 and
  `render_to_image` sizes the image by the platform window's factor, so don't change only
  `Window::set_scale_factor` (the scene and the image would disagree).
- **`gpu-allocator` and `windows` in `Cargo.lock`.** `wgpu-hal` 30 (DX12 backend) needs
  `gpu-allocator` built against `windows` 0.62, but the lock had resolved `gpu-allocator 0.28`
  (which accepts 0.53–0.62) to `windows 0.57`, and `wgpu-hal` failed to compile with mismatched
  `ID3D12Heap` types. `cargo update -p gpu-allocator` picks 0.58 and touches unrelated packages;
  the lock's `gpu-allocator` entry was edited to `windows 0.62.2` by hand (CP0.2). Keep it there
  on a lock refresh.
- **GPUI Kit's resizable panels shift space onto the far pane.** In `h_resizable`, dragging a
  border into a pane past that pane's minimum takes the rest from the pane beyond it (dragging the
  right border left shrank the sidebar), the shrinking branch never clamps the growing pane to its
  maximum, and every panel has `flex-grow: 1`, so the excess a capped pane can't take went to the
  other growing panes: the left pane grew when the right border was dragged too far (the user's
  CP0.1 report). `resizable_panel().fixed()` on the side panes (they then neither grow nor give
  space to another border's drag) plus a `size_range` minimum on the middle pane fixes it
  (`spike.rs`, test `dragging_a_border_past_its_limit_leaves_the_other_side_pane_alone`). Phase 2's
  frame does the same, or uses `resize_handle` on its own sidebar.
- **GPUI Kit's theme deserializer is silent and strict in different places.** An unknown colour
  key is dropped without a word (`theme::kit` tests that every key we write comes back); a
  `highlight` section without `syntax` makes the whole config fail ("missing field `syntax`"),
  so `kit::config` adds an empty one; and a theme without a `highlight` keeps the *previous*
  theme's code colours unless `theme::apply` copies Kit's default for the mode in.
- **Fonts: name only installed families.** `Fonts::install` adds the bundled Inter and picks the
  first installed family of each stack (`theme/fonts.rs`); Windows draws weight 500 of a family
  that has no Medium as the next heavier face (Segoe: SemiBold), Inter's static files draw 500
  as Regular and 600 as Bold. Headless test apps (`#[gpui_kit::test]`) have no fonts, so
  `resolve` falls back to `.SystemUIFont`.
- **Kit's `ElementSnapshot::focused()` panics** when an element can take focus but no focus
  binding was observed (its way of flagging `.test_support()` placed after `.track_focus()`). The
  dump reads focus without panicking; a disabled Kit `Button` tracks no focus at all (`None`).
- **A Kit button's background is several quads.** Its border is painted as further quads of the
  same bounds with a transparent fill, on top of the background quad; the dump takes the topmost
  quad of the element's bounds *with a visible fill*. Kit's hover and active fills are
  semi-transparent (`#e5e5e580` in the light theme): compare colours as painted, not as opaque.

- **A drag zone swallows the buttons inside it.** `window_control_area(Drag)` makes Windows answer
  `HTCAPTION` over the whole element, and GPUI's hit test returns the first control area under
  the pointer whatever is drawn on top, so a button *inside* the zone never gets its click. The
  title bar (`titlebar.rs`) keeps buttons as siblings of the drag zone. The caption buttons are
  `window_control_area(Min/Max/Close)` with no click handler: Windows does the click (and shows
  the Windows 11 snap flyout over `HTMAXBUTTON`).
  The same goes for anything drawn *over* a drag zone (a resize handle, an overlay): a control area
  under it still wins, because elements without `.occlude()` do not hide it from the hit test. That
  is why the sidebar's resize handle starts below the 40 px top row (the sidebar header is a drag
  zone), and why a click on the palette's backdrop over the title bar moves the window.
- **Focus rings are `focus-visible`.** A clicked button takes the focus too; drawing the ring on
  `is_focused` alone leaves a blue ring on every title-bar toggle after a click. `design::Button`
  draws it only when `window.last_input_was_keyboard()`. A test or story that wants the ring must
  send a key first (the gallery's `Interaction::Focus` sends a bare `shift`), `window.focus_next`
  alone does not count as keyboard input.
- **Kit's tooltips and dialogs need `Root`.** `TooltipOverlay` is per window "via `Root`", so the
  app window's root view is `Root::new(workspace, ..)`; our design `Tooltip` uses GPUI's own
  `.tooltip(..)` and does not need it, but Kit's `Button::tooltip` would.
- **Resize handles of our own.** The frame does not use Kit's `h_resizable`: a handle is an
  absolutely positioned 6 px `div` with `on_drag` and one `on_drag_move` on the root that sets
  *one* size (`Workspace::drag_border`), so no other pane can move. Sizes are decided by the pure
  `frame_layout` (the right panel hides when the window is too narrow, the sidebar gives way first).
- **`.probe()` before you assert.** A plain `div().id("x")` is not in the layout dump; only probed
  elements and Kit controls are. A "no element named" failure usually means a missing `.probe()`
  (or an indexed id: `div().id(("toast", 3usize))` is named `toast:3`).
- **Extra Lucide icons** are compiled in by name (`design/icons.rs`, `icon_assets!`); Kit's default
  bundle has 101. Every window, the gallery and the test apps register `AppAssets`, not Kit's
  `Assets`, or an icon outside the default set fails to load. Kit's `Icon::with_size` needs
  `component::Sizable` in scope.
- **Text needs a line height.** GPUI's default line height is about 1.6x the font size, so a 12 px
  label in a 16 px row overflows it by 3 px in the dump: set `.line_height(text::XS.line_height)`
  together with `.text_size(..)`.
- **Hover holds toasts.** While the pointer is over the toast stack none expires (t3code's
  behaviour); a test that ticks the clock must move the pointer away first. Dismissing the last
  toast under the pointer removes the stack, so no "pointer left" arrives: `Toasts::dismiss` ends
  the hold itself when the stack empties.
- **`HeadlessAppContext` has a fake clock.** Timers (the 500 ms `native-ui.json` debounce, toast
  expiry) fire only after `cx.advance_clock(..)` and `cx.run_until_parked()`; sleeping the thread
  does nothing.

- **A Kit context menu wraps the element** (`ContextMenuExt::context_menu` returns a `ContextMenu<E>`,
  not an element you can `.probe()`), and its id lands in every descendant's dump path
  (`context-menu-Name("row")/row/title`). The sidebar probes an outer element and gives each part an
  id with the thread's or project's id in it.
- **Keys bound in a parent context eat typing.** A `space` binding in the `Sidebar` context would
  swallow the space typed into the filter box; only keys the text fields do not need are bound there
  (`sidebar/keys.rs`), and each handler returns early while a field has the keyboard.
- **A re-read must not undo a running turn's marks.** The core's `live` flag can lag the
  `TurnStarted` event; `AppModel` keeps the working and waiting sets from the events and ORs them into
  every snapshot until the end event.
- **`canonicalize` adds a verbatim prefix (`\\?\`).** A project's folder and a moved thread's
  folder must share a spelling (`project_for_path` compares text); the folder picker's paths are
  plain, so a test that uses a temp folder must strip the prefix.
- **Dropping a tokio runtime waits for its blocking tasks without end.** `hermes-native` owns the
  core's runtime; after the event loop returns it calls `shutdown_timeout(2 s)` so a curator or
  review call in flight cannot leave a windowless process holding `app.lock`.
- **`Core::sessions` reads an unreadable store as "no sessions"** (`unwrap_or_default`). A front end
  that shows a first-run screen for an empty list must ask the store once itself (`CoreBackend`
  does) or it greets a damaged database with "What should we work on?".
- **Optimistic edits must reach the backend in order.** Two quick edits of one thread spawned
  side by side land in either order; `AppModel` queues them and runs one at a time.
- **GPUI's text ellipsis in a flex row truncates early and leaves a gap** (`MiddleTruncate`'s
  head is cut even when the whole string would fit). Thread titles are end-truncated in one text
  box, as t3code's are; `MiddleTruncate` stays for paths and branches.
- **Mouse selection does not work under `headless_app()`.** With the real DirectWrite text system in a
  `HeadlessAppContext`, a pointer drag over a Kit `TextView` selects nothing (focus moves, `Copy` finds
  no text); the same drag under `#[gpui_kit::test]` (`TestAppContext`, fake text) selects. Selection
  tests use the latter (`thread/tests.rs`).
- **GPUI's `list` caches the height of rows it is not drawing.** Visible rows are laid out every frame,
  rows above and below keep their last measured height: a row whose content changes (a streamed reply)
  needs `remeasure_items(ix..ix + 1)` or the scroll anchor drifts. `ThreadModel` does it on every delta.
- **`Runtime::messages` opens the session.** It starts the session's actor, and an open session counts
  as live: the sidebar would show every thread looked at as working. The thread view reads history from
  the store (`store().load_messages`).
- **Kit's text view keeps its own copy of the source.** A streamed row holds only its `TextViewState`;
  "copy message" reads the source back through Kit's doc-hidden `base::text::Text::get_text`.
- **A Kit code block's actions sit in its top-right corner** (`code_block_actions`, absolutely placed):
  the code is pushed down by the `code_block` style's top padding to make a header row; a language label
  at the left would need a custom block renderer (losing Kit's selection in code).
- **A link in a message is the model's text.** `cx.open_url` hands the URL to the Windows shell, which
  runs whatever a scheme (`ms-msdt:`, `search-ms:`, `file:`) or a path names. `thread::markdown::link_action`
  opens only `http(s):`/`mailto:` (and `www.`) in the browser, sends file paths to the right panel and
  ignores the rest. Phase 6's file view must *show* a linked file, never run it.
- **Win32 messages did not drive a GPUI window.** `SendMessage`/`PostMessage` of `WM_LBUTTONDOWN` or
  `WM_KEYDOWN` (F12) to our own instance's `Zed::Window` showed no effect (CP4.1 review: no change in a
  `PrintWindow` capture, no F12 dump written; why was not found). Real input (`SendInput`) moves the
  user's own pointer and keyboard focus, so mouse checks (selection by drag) stay the user's.
- **`Handle::block_on` for core calls** (`delete_session`, `move_session`) is fine from GPUI's
  background threads and panics inside a tokio worker; `CoreBackend` is only called from the former.

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
  machine load; re-run alone. An idle-review test's idle time has to outlast the gap between one
  turn's end (which arms the timer) and the next turn's start on a loaded machine running the whole
  suite: `s4_requests_two_corrections_make_exactly_one_review_at_idle` failed once under heavy
  disk I/O at 1.5 s (the review firing between two of its ten turns) and now uses 4 s.

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
  32k local model overflowed first. Build options with `Core::turn_options`, never
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

## Project skills (Phase 8 CP8.2)

- **A repository's skills must never reach another repository's session.** Anything new that lists
  or loads skills for a session goes through `SkillStore::*_for(session.project_root(), ..)` (or the
  index with `IndexQuery::project_root`), never the plain `catalog()` / `find()` with a guess. The
  plain methods are the user's library only, and that is right for the Skills screen, the curator
  and slash commands; don't "fix" them to see projects.
- **The scope is the session's, fixed at the start.** Don't derive a project from the shell's live
  `cwd()` (the agent can `cd` into any repository, a cloned one included): use
  `SessionTools::project_root()`. A child built from `Runtime::child_tools` must `inherit_project`.
  The frozen system prompt keeps listing the project it was built for after a workspace move.
- **Never follow a link inside a repository.** A project folder, skill folder or file that is a
  symlink or junction is skipped (`is_reparse`, which also catches junctions, that `is_symlink`
  misses on older Rust); reads and writes check the resolved path is inside the skill
  (`ensure_inside`). Windows junctions need no privilege, so a cloned repository can carry one.
- **A project skill is not trusted.** It passes the threat scan every time it is read (cached per
  file stamp), and names that the skill rules refuse are skipped: a name is used as a path and in
  XML attributes. Reading one into the model's context taints the session; the cost is that memory
  and skill writes of such a session wait in Pending. To make project skills cheaper that way, don't
  weaken the taint: trust has to come from something the repository's author can't forge.
- **The library wins a name clash, then `.agents`, `.hermes`, `.claude`** (D5 fixes the order of the
  project folders; the library first is ours, so a cloned repository can't replace a skill the user
  relies on). A shadowed copy is invisible (not listed, not indexed). Creating a project skill over a
  read-only copy of the same name is allowed on purpose.
- **Project skills are not in `.usage.json` and not in the skills git history.** `bump`,
  `record_write` and `commit` are skipped for them (a project skill's name in the user's file would
  leak it to every session through the curator, the Star map and the Skills screen).
- **Every path that puts a project skill's text in front of a model taints or holds.** `skill_view`
  and a failed `skill_manage` patch's `file_preview` answer `"scope": "project"` (the tool layer
  taints on that); pre-injection taints from `Block.project_skills`; the evidence review holds its
  writes when its packet carries a project skill (loaded or related). A new delivery path needs
  the same, or the repository's text rides into a clean session.
- **`copy_dir` skips links** (the batch snapshot / rollback and the Hermes import): following a
  file symlink would bring a copy of its target back into the repository on a rollback.
- **`index.db` is version 3.** An older file is dropped and rebuilt like any schema change. A query
  that names a repository checks its skill files' `stat` signature first, so an edit made outside
  the app is seen; a repository nobody queries is not indexed.

## Skill outcomes and provenance (Phase 8 CP8.1)

- **An outcome is provisional.** A `clean` use may turn `corrected` when the next message
  arrives, or `undone` after a rewind; `clean_rate` moves then. Only the turn right before a
  correction is blamed: a skill used two turns earlier is not. Don't read the rate of a skill with
  fewer than a handful of entries.
- **`uses` means a whole-skill load, nothing else.** A `skill_view` bumps `uses` itself;
  `record_skill_use(.., attached)` counts the skills the local index attached in `attached`, not
  `uses` (the Star map shows any skill with `uses > 0` as learned: bundled skills the index
  attaches would all appear). A new way to load a skill (the `/skill-name` invocation is one that
  isn't counted yet) must pick one counter, not both.
- **Never edit SKILL.md frontmatter through a YAML round trip.** `serde_yaml` drops comments and
  reorders nothing it understands but rewrites quoting; user-written skills must stay byte for
  byte. `provenance::append_source` edits lines and then checks the parsed result equals the
  original minus the new entry; if you change it, keep that check.
- **The model can't pick a provenance.** `SkillOp.source` and `own_hand` deserialize (staged ops
  need them) but `SkillStore::parse_call` clears both for a model's call. A new entry point that
  builds ops from model JSON must clear them too, or the model can forge `sources` and skip the
  rewrite guard.
- **A flow-style `sources: [..]` (or a scalar) is skipped, not fixed.** The write goes through
  without the entry; that is deliberate. But a non-user write never decides `sources` itself:
  whatever the model put there (any form, also on `create`) is replaced by the file's previous
  list, and a write where that can't be done safely is refused.
- **`file_path: "SKILL.md"` is the skill's SKILL.md.** `parse_call` normalises `./SKILL.md` and
  friends to `SKILL.md`, and every SKILL.md path check compares with that; a check on
  `file_path.is_none()` alone lets a patch through the provenance guard.

## Curator v2 (Phase 8 CP8.3)

- **`requests` changed what the curator does.** Under `curator_mode = per_cluster` the agent loop
  (`consolidate`, `curator_max_iterations`) never runs; tests of the loop pin `Balanced`. The loop's
  candidates are agent-created skills only; the per-cluster curator looks at *all* of the user's own
  library skills (a hand-written skill is a candidate for a merge, with the user's approval).
  `curator.auto_apply` writes without a click only when every skill the proposal touches is
  `created_by: agent` (`curator_managed`); anything else is staged even then. It has no Settings
  switch (the page's Settings only shows the sections it knows): `desktop-config.json`, then restart.
- **The model read the skills minutes before the answer is delivered.** `propose::deliver` compares
  the plan's base fingerprint of every skill the ops touch with the one the job was read with; a
  difference refuses the proposal (never "rebase" the model's text onto the user's edit). After
  staging, the item's own bases make it stale instead.
- **A skill with files besides SKILL.md is never archived by a merge**: the model saw only SKILL.md.
  It may be the kept one (the message says so); two such skills in a cluster = no job.
- **The threshold is 0.35, not "about half"**: Jaccard of shingle sets is `s / (2 - s)` for a shared
  fraction `s`, so 60% shared text scores 0.43. Distinct skills (bundled, Hermes's 193) sit under 0.2.
- **No model of its own means the main one.** `maintenance::curator_model` falls back to the review
  model, then the main model on the main provider (counted as `curator`, held back by the governor
  under 20%, at most 3 jobs a run, 2 runs an hour, weekly by schedule). Set `aux_models.curator` (no
  Settings row) or a review model to keep it off the main model.
- **Undo of an applied merge**: the absorbed skill is in `skills/.archive/<name>` byte for byte (no
  restore button in this app; move the folder back), the kept skill's old text is in the skills
  history. A hand-restored skill keeps its `archived` record, so the curator leaves it alone.
- **"Uses" for the low-performer rule are `outcomes` entries, not the `uses` counter.** `uses`
  counts whole-skill loads since the file began; `attached` counts index attachments; neither has a
  verdict. `propose::judged_uses` counts the outcome entries this build understands (`Other(..)` is
  not one). Don't switch the rule to `uses` or `clean_rate` alone (`clean_rate` is `None` at zero).
- **A merge's `sources` is ours, so its op is `own_hand`.** `contract::merged_sources` strips whatever
  `sources` the model wrote and appends the union of the cluster's; the op carries `own_hand` so the
  store doesn't put the kept skill's old list back (`keep_sources`). Repairs are not `own_hand`.
- **The `archive` op is not a model action.** `parse_call` refuses it and `validate_ops` requires it
  after every other op; code that builds ops from model JSON must not pass an `archive` through.
  The whole batch is staged as one item, so approving archives without the merge is not possible.
- **A skill with a secret-like value is never sent**: masking would change its text, and the merged
  text would then overwrite the real one. The cluster is skipped (`propose::skill_block`); same for a
  skill that contains `</skill` and for a job over the model's window.
- **Judged jobs are remembered for 30 days** (`curator-seen.json`), keyed by names and file hashes:
  a rejected proposal isn't re-requested weekly, an edited skill is. Delete the file to ask again.
- **The curator's hourly cap and back-off are per process** (`gate.rs`), like the review scheduler's.
  A failed request ends the run and the next run waits an hour (doubling). A crash loop can't burst:
  the schedule's `last_run_at` is saved before the first request.
- **The ledger counts a moment late.** A run that crosses 20% left stops before the *next* job only
  once the ledger thread has applied the previous request; tests assert a range for that case.

## Memory in `memory.db` (Phase 9 CP9.1, `crates/memory/src/{db,sync}.rs`)

- **The files are a view, but the file wins a disagreement.** Every access compares MEMORY.md /
  USER.md with `memory.db` (the hash recorded at our last export *and* the parsed entries); when
  they differ the file is taken over. That is what makes a hand edit, an older build's write and a
  crash between our rename and our commit all come out right, and it keeps the system prompt
  byte for byte what the file says. Don't "fix" a mismatch by exporting the database over the file.
- **A read never writes a `.md` file.** Migration and re-import only read them. Tests that write a
  file and then read through the store see the new content (it is re-imported), as before.
- **What is kept and what is erased.** The *agent's* removals and rewrites (the tool, a review, an
  approved staged write) keep their row (`removed`, `removed_why = store`; a patched entry's old row
  points at the new one, `superseded_by`; a text that comes back gets its old row and id), as does
  `repair`. Later queries must filter `removed IS NULL`. The *user's* removals erase the text from
  `memory.db` (rows, older versions, FTS index; `secure_delete` on, WAL checkpointed): Settings ▸
  Memory ▸ reset (`MemoryStore::reset`; never delete the files directly any more, the next access
  would treat that as an accident), a delete or rewrite on the Star map (`edit_entry`), and an
  entry removed from the file by hand. So "ask the agent to forget X" keeps X as a removed row
  until a reset; the GPUI app's Memory screen goes through `apply` too (frozen, not changed).
- **A file left with no entries outside the app** (deleted, emptied) is taken over only after its
  entries were copied to `<file>.bak.dropped.<time>` (rename it back to restore); no copy, no
  take-over: reads answer from the file alone and writes are refused until the copy can be made.
  A hand edit that keeps at least one entry makes no copy (it may be the user removing a secret).
- **Backups hold old text.** `<file>.bak.pre-memory-db.<time>` (made before the first migration)
  and `<file>.bak.dropped.<time>` are plain copies in `memories/`; we never delete them, and reset
  does not either. Someone who wants a secret gone everywhere deletes those copies by hand (and the
  `*.md.bak.<time>` drift snapshots, which predate Phase 9).
- **The cross-process lock is the database's write lock** (`BEGIN IMMEDIATE`, busy timeout 5 s;
  a write tries 3 times, so a memory call waits 15 s at most, then refuses). A writer that doesn't
  take it (an editor, a store whose database is off) is caught by the re-read just before the
  rename (a conflict, the write starts again, 3 tries), not prevented; the window is the time
  between that read and the rename. The rename itself is retried for ~300 ms (a scanner holding
  the file). A database still busy 5 s into its first open is tried again at the next use.
- **Database trouble = the files alone**, as before Phase 9, logged (`MemoryStore::database_problem`).
  A newer schema version or a damaged file that can't be moved aside switches it off for the life of
  the process; the next start tries again.
- **`MemoryStore::new(dir)` puts `memory.db` in `dir`** (tests, tools); the app uses
  `MemoryStore::open(<data>/memories, <data>/memory.db)`. `file_only` is the old behaviour (the
  reference in the round-trip tests).
- **Failpoints are `#[cfg(test)]`** (`sync::Failpoint`, `MemoryStore::arm`): a "crash" returns
  early without cleanup and the test drops the store, as a dead process would.

## Memory tiers, scopes and notes (Phase 9 CP9.2, `crates/memory/src/{tiers,notes}.rs`)

- **The `memory` tool's schema grew** (`tier`, `tags`, `links`, `note`, the action `note_read`, a
  TIERS paragraph in the description). Tool definitions are part of the cached prefix, so every
  session's cache misses **once** after this upgrade (as at Phase 3); a session that already
  started keeps working (the runtime reads old and new shapes). Keep further additions small: the
  schema rides in every request.
- **`core` is the files; the other tiers are the database only.** `project` and `archive` rows
  never reach MEMORY.md / USER.md and are not synced from them. A query for the core view must
  keep `tier = 'core' AND scope = ''` (`db::LIVE_CORE`, `db::knows`); one for another tier must
  filter `removed IS NULL`. A hand edit of the file, the Star map and the index (`memory_doc`) see
  `core` only until CP9.3, which indexes `archive` and `project` with a scope (see the CP9.3
  section below; indexing project rows globally would leak one repository's memory into another's
  context).
- **Project scope = Phase 8's git root, worktrees folded in.** `memory_scope()` of the session's
  tools (`project_skills::memory_scope_key(find_root(cwd))`) is the only source of the scope; it
  is never read from the model's arguments (`take_extras` sets it, a staged operation carries it,
  so approving later writes the same repository). The path is canonical (`..`, junctions, 8.3
  names, `\\?\` resolved) and compared lowercase, so spellings of one folder are one key. A
  **linked worktree** (`git worktree add`) shares its main checkout's memory — `main_checkout`
  follows the `.git` file's `gitdir:` and requires the pointer back (`<gitdir>/gitdir`) and a
  `commondir` naming a `.git` folder; a forged `.git` file, a submodule (`.git/modules/...`, no
  `commondir`) and a bare repository's worktree stay projects of their own. Project *skills* stay
  per checkout (`scope_key`): they are files in it. Removing a worktree therefore loses no
  memory. A session outside a repository has no project tier (the call is refused, nothing falls
  back to global). Curator sessions have no repository. Computing the key reads two small files:
  do it in `spawn_blocking`.
- **The project block is built when the session starts and frozen with the prompt**
  (`PromptSource::build`, `snapshot_for`). A project write this session reaches the next
  session's prompt, not this one's (retrieval into a running session is CP9.3). `snapshot()`
  (no repository) never reads the project tier, so a core-only user's prompt is unchanged byte for
  byte; the `turns` test compares it.
- **One call writes one tier**, and `note_read` stands alone. Mixed batches would make "all or
  nothing" span a file and a database, so they are refused up front.
- **Limits.** `core`: today's char limits, same errors. `project`: `memory.project_char_limit`
  (default 1500, 200-20000) per project, both kinds together (the other kind's characters are
  reserved when a target is checked). `archive`: none, but 1000 characters an entry and 5000 entries
  a target; long text goes in a note (20000 characters, threat-scanned). Core has no notes.
- **Notes.** The slug is built only by `notes::slug_for` and re-checked (`valid_slug`) wherever a
  link is turned back into a path, so `..`, drives, separators and Windows device names can't come
  out of a link. The model's own `file:notes/...` links are refused (the store's link is what
  `note_read` trusts), and `file:` links are otherwise only strings, never opened. A note is never
  overwritten: a rewrite is a new file, the old one stays linked to the replaced row (the agent's
  history) until the user removes the entry, which deletes every file the row and its older
  versions linked to (`delete_tier_entry`) — except a file a surviving row still links (a patch's
  successor carries its predecessor's note links, and a purge also takes removed rows of the same
  text from any tier: `db::unreferenced`). A crash between writing a note and committing leaves an
  unlinked file (not cleaned up); a failed commit removes the files it wrote. A notes folder that
  is a junction or symlink is never read, written or deleted through.
- **Reset erases `core` only.** Settings ▸ Memory ▸ reset asks "Delete Agent memory (MEMORY.md)?
  This cannot be undone." (page text, not ours), so `MemoryStore::reset(target)` purges that
  kind's `core` rows and deletes the file, and leaves the `project` and `archive` tiers of every
  repository and their notes alone: erasing data the confirmation doesn't name would be a
  surprise that can't be undone. Until CP9.3's map shows those tiers, the user removes them one
  by one only through `delete_tier_entry` (no page yet) or by asking the agent (a soft removal:
  the row stays, marked). `edit_entry` (Star map) is core only until CP9.3.
- **A staged write carries `extras`** (tier, scope, tags, links, note). A build older than CP9.2
  (an old GPUI binary sharing the data folder) would read such a record without them and approve
  it into `core`; approve staged writes with a current build.
- **Tags and links of core entries live in the database only** (the file can't hold them); a patch
  carries them over, a hand edit of the file does not (the changed text is a new row).

## Memory retrieval, dedupe, hygiene and links (Phase 9 CP9.3)

- **Retrieval reaches the user message, never the system prompt.** Under `requests`
  (`budget.memory_preload` = 3) the `project` entries of the session's repository and the global
  `archive` are searched per turn and ride in the `<context>` block of the user message
  (`<entry id="archive:<n>" tier="archive">`), stored with it (resume and rewind replay the same
  bytes; the page shows the typed text). `balanced` and `tokens` have `memory_preload` 0: nothing
  is searched, nothing is attached, byte for byte as before. An entry written this session is
  retrievable by the **next** turn (the write marks the index dirty and a query refreshes first);
  the frozen system prompt never changes.
- **The scope filter is in SQL, before the limit.** `memory_doc.scope` is `global` for the user's
  own entries and the archive and `project:<key>` for a repository's rows
  (`memory_scope_key`, worktrees folded into their checkout); the query says `scope IN ('global',
  <own>)`, so another repository's rows are never read, let alone ranked. Keep it that way if you
  add a column or a ranking step: a filter after `LIMIT` lets a big repository crowd a small one
  out, and a filter in Rust has already read the rows.
- **A project entry is the repository's own words about itself, and the user's.** They are not
  tainted text (the agent or the user wrote them), but a repository's skills are (Phase 8); the
  index never mixes them up (`scope` columns of two tables, two schemas). Project entries are
  also in the system prompt of a session that **starts** in the repository (CP9.2's snapshot);
  retrieval adds the ones written since, and may repeat one the prompt already has (as for core
  entries since Phase 5).
- **The index is a cache of `memory.db`'s generation.** `meta.tiers_gen` (in `memory.db`) is bumped
  in the transaction of every non-core write and delete, by any process; the index compares it
  at each query, so the ACP binary's write is seen by the desktop app. A new write path for the
  `project` / `archive` tiers must call `db::bump_generation` or the index will not notice.
  A retrieved row is re-checked against `memory.db` (`row_stats`): a row removed since the index
  was refreshed is dropped.
- **Hits are batched and approximate.** `record_hits` counts in memory and writes one transaction
  at 20 waiting entries, after a minute, at drop and before the hygiene pass; a crash loses up to
  a minute of hits. `hit:<row id>` in `meta` is the last hit time (no schema change; the key stays
  after a purge, it holds no text). A `core` entry's hit is found by its text.
- **Dedupe is a word-set comparison, not BM25** (`similar.rs`; the plan said BM25). Byte-exact
  repeats are no-ops that confirm the entry; anything similar is **added and flagged** in the same
  result (`similar_entries`, `message`), never dropped. The ids in the note are `memory.db` row
  ids (core entries have one too); the model still edits an entry by `old_text`. Thresholds are
  `similar::{JACCARD_MIN, CONTAINMENT_MIN}`; a very short entry (< 3 words) is never "similar".
- **Tier retrieval needs a specific message.** A `project` / `archive` row is attached only when
  the message has at least 3 search words (stopwords out) and the row shares at least 2 of them
  (stemmed); BM25 decides the order, not the gate (over a few rows SQLite's idf makes every score
  ~0; over hundreds any rare word scores high, so "looks good, thanks" pulled in an entry about
  thanking reviewers). Constants `MIN_TIER_MESSAGE_WORDS` / `MIN_TIER_SHARED_WORDS` in `index.rs`;
  `a_large_varied_archive_injects_nothing_for_small_talk_or_vague_requests` is the 240-entry check.
  Hits and recency only reorder matches (at most +1.5 over BM25), so a popular entry can't win a
  message it doesn't match.
- **Block text is data.** Entry and skill bodies go through `preinject::sanitize_body`: besides
  `</context>`, the `<` of the block's own tags (`entry`, `relevant-memory`, `relevant-skills`,
  `skill`, `context`, opening or closing, any case) is written `&lt;`, so an archive entry can't
  close its element and forge a `tier="core"` one. Other angle brackets (`Vec<T>`) are kept.
- **Hits are counted outside the query's time limit** (`preinject::retrieve` spawns the
  `record_hits` call): a due batch waiting up to 5 s on a `memory.db` another process is writing
  must not cost the turn its block.
- **Hygiene proposes, the user decides.** The curator's memory pass (`memory_hygiene.rs`, zero
  requests, **only when `budget.memory_preload` > 0**, i.e. `requests`: under `balanced` / `tokens`
  nothing retrieves the archive, so a demotion would make the agent forget, and nothing records
  the hits that mark an entry as used) stages `demote` items for `core` entries not confirmed or hit for 60
  days, at most 5 a run, oldest first. Pin an entry by adding the same text again with the tag
  `pinned` (a patch needs a change). A row proposed is not proposed again for 30 days, so a
  rejection sticks. Entries over 1000 characters (the archive's cap) are never proposed. A new
  entry counts from the day it was made, so nothing is proposed in the first 60 days of a store.
  Applying (`demote_core`) copies to the archive first and then removes from core: a failure
  between the two leaves a copy in both, never a loss.
- **`supersedes` is a replace.** A review `add` with `supersedes: id` names a live `core` entry of
  the same file (an archive row, the other file's entry or a removed one is refused); it is held
  for the user like any replace (a background review never changes the user's entries
  unattended). The ids come from the packet (`<entry id="...">`).
- **The Star map**: only `core` cards are nodes (the page can only edit those); their `skill:` /
  `memory:` links are edges. The archive is an additive list the page ignores; **project memory is
  never in the map** (no working folder to scope it by). A link whose end is gone is listed with
  `resolved: false` and drawn nowhere.

## `run_script` (Phase 10 CP10.1, `crates/tools/src/{script,script_guard}.rs`)

- **The approval is an aid, not a sandbox.** A script is many commands in a language that can build
  more of them at run time, so no list of spellings proves one safe. `script_guard.rs` is
  deliberately generous (comments and string literals are scanned like code; a pattern it cannot
  evaluate counts as a match) and the boundary is the OS sandbox of Phase 15. What it does:
  - **hardline** on any line (`rm -rf /`, `Stop-Computer`, `shutdown`...) blocks the script in
    every mode, Yolo included, without asking;
  - the terminal's **dangerous** patterns run over the whole text, every physical line and the text
    with continuations undone (backtick-newline and every backtick in PowerShell, caret-newline,
    backslash-newline), after NFKC folding, so `I`nvoke-Expression`, `r\` + newline + `m -rf` and
    full-width look-alikes read as what they run, and an unbalanced quote on one line cannot blank
    the next lines for the guard's search-command masking;
  - **dynamic** constructs (a script that runs text it builds, fetches or decodes) are listed per
    language (PowerShell: `Invoke-Expression`/`iex`, `& $x` / `& (...)` / dot-sourcing, `Start-Process`
    and friends, `-EncodedCommand`, `FromBase64String`, `[scriptblock]`, `Add-Type`, reflection,
    `.Invoke(`, COM/`Process.Start`, aliases, another shell launched, a script or executable file
    written; Python: `eval`/`exec`/`compile`, `__import__`/`importlib`, `subprocess`/`os.system`/`popen`/
    `exec*`, `ctypes`/win32/`winreg`, `getattr`/`globals`/`__builtins__`/dunder escapes, `pickle`/
    `marshal`, `timeit`/`cProfile`/`pip`, base64/hex decoding, a script or executable file written,
    `chmod`; bash: `eval`/`source`/`exec`/`xargs`/`trap`, `$(...)`/backticks/`${!x}`/`$'..'`/`<(...)`, a
    variable as the command word, another shell started or piped into, `find -exec`, `awk system`,
    `perl -e`, base64 decoding, `chmod`, an executable script written). A script with one **always
    asks** (every mode but Yolo), is **never smart-approved** and **never remembered**: the card
    offers only once/deny and the answers "for this session" / "always" are treated as "once";
    remembered terminal approvals (session or allowlist) are not consulted for scripts at all.
  - The approval card shows the **whole script** (`run_script (<language>)` + the code). A script
    that needs a decision and is longer than 16,000 characters is refused ("split it"), so the user
    never approves text they cannot see. A script with no flagged line and no decision to make
    can be up to 64 KB and just runs.
  - Path literals: persistence places, the app's data folder and the skill folders are found by
    the terminal's `command_flag` over the whole text; an *obvious* path literal (drive path, UNC,
    `~`, `/c/..`, `/etc`.., `..\`) that resolves outside the working folder flags the script (reads
    count like writes). A URL, a regular expression or a bare relative name is not a path here.
  - Tainted sessions: a flagged script always asks (never smart-approved), Yolo runs it and the
    transcript gets the usual "ran after reading untrusted content" line. A script that fetches
    from the network (`curl`, `Invoke-WebRequest`, Python `requests`/`urllib`...) or names an outside
    path taints the session afterwards (`script::fetch_source`).
- **Added by the CP10.1 review** (each with a test in `script_guard_corpus.rs` / `script.rs`):
  - The terminal's command-position patterns also run on the commands *inside blocks*: each line is
    split, quote-aware, at `{ } ( ) ; | &` and after `then`/`do`/`else`, so `if ($x) { shutdown /s }`,
    `then rm -rf /` and `f() { rm -rf /; }` are hardline, and `if (1) { python -c ... }` is flagged
    (before, a terminal-flagged command wrapped in a block was not: `run_script` was a way around the
    terminal's approval).
  - The scan folds en/em dashes to `-` and curly quotes to quotes (PowerShell honours them:
    `Remove-Item x –Recurse –Force` deletes recursively) and treats `\v`, `\f`, NEL, U+2028/2029 as
    line breaks; a raw control character (ESC, backspace...) flags the script, and the **approval card
    shows every control, bidirectional and invisible character as `\u{XXXX}`**, so the script can't
    reorder or overwrite the card that shows it.
  - New rules: the `function:` drive (`Set-Item function:f -Value $text` runs text), `.Invoke*(`,
    `[wmiclass]`/`Win32_Process`, `Import-Module`/`using module`, `ii`, Windows helper binaries
    (`explorer`, `msbuild`, `installutil`, `regasm`, `cmstp`, `runas`...) and shells by any path or after
    `env`/`command`/`nohup`; `Remove-Item` aliases with abbreviated `-r`/`-fo`, `| Remove-Item`,
    `[IO.Directory]::Delete`; Python `rmtree`, bare `exec`/`eval` (`f = exec`), `o.execv(` on an alias,
    `from os import *`, `nt`/`posix`, `_ctypes`; bash `alias`, `awk ... | "sh"`, `sed .../e`; registry
    writes; `git -c alias.`/`core.pager`, `git config alias.`, `tar --to-command`, `rsync -e`,
    `ProxyCommand`; `.env`. **Persistence and credentials named anywhere** (`$PROFILE`, `.bashrc`, the
    startup folder, `.git/hooks`, `core.hooksPath`, `.ssh`, `authorized_keys`, `.aws`, `.npmrc`...) ask
    **even in Yolo**, like the terminal's `Persistence` paths (`Analysis::persistence`).
  - *Write, then run*: a PowerShell or bash script that writes a file (redirect, `Set-Content`, `cp`, a
    download...) **and** runs a program by path (`./x`, `.\a.exe`) or an interpreter on a file
    (`python x.dat`, `node x.mjs`) is dynamic. Split across two calls it still passes (residual 1).
  - Patterns run on the linear-time `regex` engine wherever they have no look-around: on the
    backtracking engine a 64 KB script hit the backtracking limit, which counts as a match, so every
    big script asked. A padded line (60 KB of spaces, words or quotes around `rm -rf build`) is caught.
  - The script file is written, then **held open read-only, sharing only reads**, until the run ends;
    what is read back through that handle must equal what was checked, and a run folder or `scripts`
    folder that is a junction is refused. A parallel script or a background command can't swap the
    file between check and run, or append to it while bash reads it. Run folders older than an hour
    (a crash's leftovers) are swept when the next script runs. All of it on `spawn_blocking`.
  - PowerShell is started by its full path under `%SystemRoot%`; `uv.exe` is looked up only in absolute
    PATH entries; bash runs without `BASH_ENV`, `ENV` and exported `BASH_FUNC_*` functions.
  - `timeout` accepts an integer, a float or a numeric string; zero, negative or junk is the default.
- **Residual risks** (what the guard does not and cannot see; each is why Phase 15 exists):
  1. *Write, then run, across calls.* One script (or `write_file`) writes a file and a later script
     runs it with `python x.dat` or `./x`: each call alone is unflagged. The terminal has the same
     hole (`write_file` then `terminal`). Within one script the write-then-run rule asks.
  2. *Programs that run programs.* Anything that runs what a file or argument says: `npm run`,
     `cargo build` (a `build.rs`), `make`, `pytest` (`conftest.py`), a hook already in the
     repository... The common option spellings (`git -c alias.`, `tar --to-command`, `rsync -e`,
     `ProxyCommand`, `zip -TT`) ask; build tools run what the repository says, as in the terminal.
  3. *Python is too dynamic.* The list is names, not semantics: a name not listed (a stdlib module
     that evaluates strings, a third-party package that is installed) gets through, and string tricks
     only matter once they reach one of the listed names.
  4. *Exfiltration over the network is not an approval trigger* (a POST of project files with
     `Invoke-RestMethod` or `requests.post`; credential files and `.env` named in the script do ask).
     The session is tainted afterwards.
  5. *Data in, code out.* A reviewer model in Auto mode sees the script text; comments after `#` are
     stripped, but PowerShell block comments and Python docstrings are not, so an injected
     "APPROVE" can reach it. It only judges scripts that are flagged for a dangerous pattern and
     nothing else (never dynamic, tainted or persistence ones), the same exposure as a flagged
     command.
  6. *The scan is of text.* A Python script that imports a module from the working folder runs that
     module's code (the model could have written it earlier); modules loaded by name from the
     user's own module paths likewise.
  7. *Scratch is not a sandbox.* The script runs with the user's rights, in the working folder; the
     per-run scratch folder only holds the script file.
  8. *Escaping the job object.* The process tree dies with the job, but a process started *through a
     service* (WMI `Win32_Process.Create`, the Task Scheduler, a COM server such as
     `Shell.Application`) is not in the job and outlives the script; those constructs are flagged.
  9. *A false positive costs a click:* a script that only prints `rm -rf build` asks.
- **Python goes through `uv`, never the `python` on PATH, and never a repository's venv.** The
  interpreter is `uv python find --system --no-project --no-config --no-python-downloads` (run from
  the scratch folder), then `<python> -X utf8 <file>` runs directly: `uv run` would have used a
  `.venv\Scripts\python.exe` in the working folder (the repository's executable and its `.pth`
  files), nothing is downloaded, and PEP 723 inline metadata (`# /// script`) is never read, so
  nothing is installed. Python adds the script's own (scratch) folder to `sys.path`, not the working
  folder. Only what is installed for that interpreter is importable (not the project's venv). No
  `uv` (looked on PATH, `~/.local/bin`, `$CARGO_HOME/bin`) or no Python is a plain error; so is no Git
  Bash for `language=bash`.
- **PowerShell is wrapped, the script is not.** The script is a file with a UTF-8 byte-order mark
  (Windows PowerShell reads a file without one as the ANSI code page); the command line is our own
  driver (`-EncodedCommand` of a few lines holding only the random path) that sets UTF-8 output,
  silences progress records (they come out on stderr as CLIXML) and returns the script's exit code
  (`exit n`, else `$LASTEXITCODE`, else `$?`). A terminating error ends the whole run with exit
  code 1. Output is CRLF-normalised; stderr comes back separately as `stderr`.
- **Per-run scratch folder.** `<scratch>/scripts/<random>/script.<ext>`, removed after the run (with
  retries while Windows still holds a killed process's handle), also when the turn is stopped or the
  call future is dropped (a guard's `Drop` retries on a thread). A crash of the whole app can leave
  one behind under `<data>/scratch/<session>/scripts/`; nothing sweeps it yet.
- **One call, one process tree.** Timeout default 60 s, at most 600 s; the tree is killed on timeout,
  on the turn being stopped, when the call future is dropped and when the script ends (a script
  that starts a server loses it). There is no background mode and no live output relay (the
  terminal's `tool.progress` is not ported).
- **Checking cost.** The analysis (hundreds of regexes) runs on a blocking thread: about a second per
  10 KB of script plus a few milliseconds per distinct line in a debug build, several times less in
  release. A script at the 64 KB limit costs a few seconds in debug; it never blocks a worker.
- **The tool list is part of the frozen prompt.** Adding `run_script` changes the cached prefix once
  for existing sessions on upgrade (the first request after it is a cache miss; the system prompt's text
  is unchanged, it only names the tools it needs). It is
  offered where `terminal` is: it belongs to the `terminal` toolset, so switching that toolset off
  removes `run_script` too; subagents get it like `terminal`; the background review and the curator
  are whitelisted and never see it. The batching guidance that names it is in the next section (CP10.2).
- **Failure bookkeeping treats it like the terminal** (`guards.rs`: a nonzero `exit_code` is a failure,
  a success resets the no-progress counter; `compress.rs` summarises a pruned result as
  `[run_script] ran a <language> script -> exit N, M lines output`).

## The first-turn context pack, batching guidance, multi-path reads (Phase 10 CP10.2)

- **Frozen prompt changes once.** Every new session's system prompt gains one sentence
  (`BATCHING_GUIDANCE`, after Hermes's parallel-calls text, only when `run_script` is offered), and
  `requests` sessions gain the todo sentence inside `BUDGET_GUIDANCE_REQUESTS`; the `read_file` and
  `search_files` schemas change (a `paths` / `group_by_file` property, `path` no longer `required`). A
  session that already exists keeps its stored prompt, so only the tool schemas (sent every request)
  change its cached prefix: the first request after the upgrade is a cache miss, as at Phase 3 and
  CP10.1. Nothing in the new text depends on a number that can change mid-session.
- **The pack rides on the first user message, nowhere else.** `begin_turn` adds it when the conversation
  has no user message yet, the budget knob `context_pack` is on (`requests`; `balanced` and `tokens` off;
  `budget.context_pack` true / false overrides any profile) and the message is not an app-written prompt
  (`preinject::eligible`). It is stored with the message (the `preinject` note, empty skill and memory
  lists when nothing else was attached), so a resume, a branch or a rewind that keeps the message replays
  the same bytes. A rewind to the very start leaves a session with no user message: its next first
  message gets a fresh pack (identical while the repository state is). A compaction that hides the first
  message does not bring a second pack. The page's text, `/undo`, session search and the review briefing
  use `preinject::user_text` / `strip` as for a skill block.
- **Repository text is untrusted, and the pack does not taint the session.** File names, the README and
  commit subjects can say anything. Defences: the element's wording calls them data and not an
  instruction; control, bidi and zero-width characters become `?`; the block's own tags and the
  element's (`repo-context`, `readme`, `tree`, `changes`, `commits`) are written `&lt;` where repository
  text has them (`preinject::escape_tags`, applied also over the cache's content, so a hand-edited
  cache file cannot forge them); the README goes past `scan_context` (the system prompt's
  project-context scan: an injection phrase makes it a `[BLOCKED ...]` note); it is read as a plain file
  inside the repository only (no link followed; a link as `README.md` is not read), 8 KB of it at most;
  the tree does not descend into a symlink or junction. **Decision: no taint** (implementer, confirmed
  by the CP10.2 review). What taint gates (Phase 6): a flagged command is never smart-approved (always
  asks in Manual / AcceptEdits / Auto, Yolo warns), an `Outside` write asks, and `memory` /
  `skill_manage` writes are staged for the user. Tainting on a README head would set all three for
  practically every session in a repository under the default profile (nearly every repository has a
  README): self-improvement would be staged always and the taint warning would stop meaning anything.
  The README is written by the same people as `AGENTS.md` / `CLAUDE.md`, which the system prompt
  already loads as *instructions* (a stronger channel than a labelled "data, not instructions" block)
  after the same `scan_context`, without tainting; the pack does not lower that bar. Project skills
  (Phase 8) taint on delivery for a different reason: a skill is a procedure the model is told to
  follow, with scripts it may run. The pack also reads nothing the session could not read itself with
  `read_file` inside its own folder, which never taints either. The **residual risk** is real and
  shared with `AGENTS.md`: an injection in a hostile repository's first 2,000 README characters or its
  last 5 commit subjects reaches the model past the threat scan's patterns only; hardline, the path
  policy and the mode's own approvals (Manual and AcceptEdits ask for flagged commands; Auto asks the
  judge) still guard what the model then does. Turn the pack off (`budget.context_pack: false`) for
  repositories you do not trust, as you would not open them with `AGENTS.md` loading either.
- **Cached per HEAD, bounded.** Key: repository root + HEAD commit + a hash of `git status --short
  --branch`. The branch line and the status lines are always read fresh; the tree, project-file names,
  README head and commit subjects come from the cache when the key matches. A commit, a branch change
  (the status text carries it), an edited or added tracked file or an untracked file at the top of an
  untracked folder is a new key. A new file *inside* an already-untracked folder does not change the
  status text, so the cached tree can miss it until the next change; the model has `search_files`. No
  HEAD (no commit yet), no `git`, or a status that failed or timed out: nothing is cached (every session
  recomputes the tree and README, a few milliseconds). 32 files at most (the oldest by modified time
  go), each at most 64 KB, written as a temporary file then renamed; an absent, unreadable, oversized,
  corrupt or wrong-version file is recomputed and replaced. The folder is `<data>/cache/context-pack/`,
  safe to delete.
- **Capped.** `TOTAL_CHARS` is 14,000 (about 3,500 tokens). Per part: 5 commit subjects of 100
  characters, 25 status lines (1,800 characters), 2,000 README characters, a tree of at most 300 lines /
  9,000 characters, 40 entries a folder (`... N more`), depth 3, the deepest level that fits (the tree
  falls back to depth 2 or 1 and says so). The order of cutting when something is too big: the tree's
  end, then the README's tail; every cut says so (`[... N more lines cut]`). A walk reads at most 20,000
  entries and 8 s; a huge or flat folder cannot starve the folders after it (each folder is read for its
  first 40 entries only).
- **`git` never hangs or runs repository code.** Each call has a 4 s limit (the whole collection 8 s, the
  turn waits at most 12 s), its output is read up to 256 KB, stdin is closed; when git exits but a
  program it started still holds the output pipe, the reader is abandoned at the limit, not joined.
  A repository's own `.git/config` (not brought by a clone, but by a copied folder or an archive) has
  two routes to make `git status` run a program, both closed and tested with a real config:
  `core.fsmonitor` (`-c core.fsmonitor=false`) and a `.gitattributes` `filter=<x>` whose
  `filter.<x>.clean` / `.process` command `status` runs on a file whose stat data changed (verified:
  plain `git status` ran it). Attributes are read from git's empty tree (`GIT_ATTR_SOURCE`, git 2.42+;
  an older git ignores the variable and keeps this route open; a SHA-256 repository makes the variable
  an error and the git parts are left out). Side effect: a stat-dirty file whose content differs from
  the index only through a filter or `.gitattributes` line-ending rule (a Git LFS file touched since
  the last `git status`) shows as modified in the pack. Also `--no-optional-locks` (nothing written,
  so no `post-index-change` hook), no pager, `color.ui=false`, `log --no-show-signature --format=%s` (no
  `gpg.program`), and every inherited `GIT_*` variable dropped (`GIT_DIR`, `GIT_CONFIG_*`,
  `GIT_EXTERNAL_DIFF`...). Hooks do not run for `status` / `log` / `rev-parse`; `diff.external` and
  textconv are for `diff` only; `core.sshCommand` / credential helpers for network commands only;
  `include.path` only adds config of the same kinds. System and global config are kept (the user's
  `safe.directory`, `core.autocrlf`): another owner's repository is still refused by git, and the git
  parts are left out. For comparison, the page's coding rail (`desktop/src/git.rs`, `/api/git/status`)
  runs a plain `git status` in the same folder when the page shows it. Without `git` on PATH the pack
  still has the tree and README.
- **The pack repeats nothing the system prompt has.** `AGENTS.md` / `CLAUDE.md` / `.hermes.md` are
  already loaded into the system prompt's project context (the first found wins), so the pack lists
  their names and sizes only and says the prompt carries the first one found. This deviates from the
  plan's "the head of AGENTS.md/CLAUDE.md/README": a second copy would cost thousands of characters on
  every first message for nothing.
- **`read_file` with `paths`.** At most 20 files per call (the rest each answered with an error), 160,000
  characters of file content in all (one file may use 100,000 of them; once less than 2,000 are left the
  next file is refused with "ask again"), `offset` / `limit` apply to every file, duplicates are read
  once, `path` and `paths` together read both. The result is `{files: [...], files_read, files_failed}`;
  an entry is the single read's result plus its `path`, an error is that entry's own `success: false` /
  `error` (the call is an error only when every file failed). Secrets are masked per file by its name
  (`.env`, `*.env`...), the read-before-write record is kept per fully read file, and a file read from
  outside the working folder taints the session as a single read does, per file *actually read* (inside
  `files::read_files`, so a background review's own reads, `SIDE_READ`, do not); a file that failed taints
  nothing. The old single-`path` call has the same bytes as before (a test pins its result, its
  not-found error and the missing-`path` error). `read_file` results are never spilled to a file
  (`spill_oversized` exempts it), which is why the total is bounded here.
- **`search_files` `group_by_file`.** Opt-in, `output_mode` `content` only: `{groups: [{path,
  match_count, text}], total_count}` with `text` in grep's format (`N:line` for a match, `N-line` for
  context, `--` between ranges, neighbouring windows merged); `limit`, `offset`, `truncated` and
  `next_offset` count matches as before and the budget profile's context factor applies. Secrets are
  masked per group. The default shape is unchanged.
- **The todo wording** (`requests`): "update the todo list only at phase boundaries" rides in the budget
  guidance, frozen with the session; the `todo_list` tool is unchanged, so a model that ignores the
  sentence still works.

## Short tool schemas and `hermes-tools-guide` (Phase 11 CP11.1)

- **The tool list text changes for every session once, on upgrade.** The descriptions of
  `skill_manage`, `session_search`, `memory` and `delegate_task` shrank (and end with `Details:
  skill_view hermes-tools-guide`), so the first request after the upgrade is a cache miss, as at
  Phase 3, CP10.1 and CP10.2. The system prompt's text is unchanged. A session that already exists
  keeps its stored prompt; only the tool schemas (sent every request) differ.
- **Measured honestly: about 3.1 KB (~780 tokens), not the plan's "8k".** The plan's sizes (6.4 KB for
  `skill_manage`...) were the pretty-printed files; as serialised into a request the four were 3.5 /
  3.4 / 3.7 / 2.5 KB (13.1 KB), most of `skill_manage`'s being its six-way `anyOf` structure, which is
  frozen. The four now take 10.0 KB (9.5 KB before the review put back behaviour rules the
  short text must keep: no PR numbers/dates in skills, "don't repeat the call", what not to store,
  archive entries one line). The ~8k saving of Phase 11 comes from CP11.2 (deferring tools),
  not from shorter text. Table: `docs/backend/progress/phase-11.md`.
- **Only `description` strings changed.** Names, types, `required`, enums, defaults and the `anyOf`
  shapes are pinned by `crates/prompts/tests/fixtures/tool_shapes.json` (the old schemas with every
  description removed): a call, a staging record, an ACP card or a review contract written against
  the old schema is still valid. If you change a shape on purpose, regenerate that fixture and say so.
- **What stays in the short text is what decides how a call is formed** (required parameters, enums,
  limits, the rules that keep junk out of memory and skills, the one-batch rule, the `note_read` call by itself, the 60/64-character skill limits, the
  `delete` sole-op rule, what a path may start with). A test lists phrases the four must still say.
  Rationale, examples and rare edge cases are in the skill. A parameter added to one of the four
  without a word in the skill fails `the_tools_guide_is_bundled_and_documents_every_parameter...`.
- **`hermes-tools-guide` is an ordinary bundled skill**, not prompt text: embedded from
  `assets/skills/autonomous-ai-agents/` (not `skills/`, which mirrors Hermes), seeded into the user's
  skills folder with the others, listed by `skills_list` and in the index (one line under `balanced`,
  a name under `tokens`, one more in a category count under `requests`), loaded with `skill_view`. It
  is in the seeding manifest, so the curator never proposes changes to it, but the agent's own
  `skill_manage` can still patch it (as any bundled skill, existing rule): a user who edits it keeps
  their text across upgrades (the manifest hash check), which also means an app upgrade does not
  refresh an edited guide. **It is never retrieved by the local index** (`skills::TOOLS_GUIDE`,
  skipped in `LocalIndex::query`): its text names memory, sessions, search and subagents, so ordinary
  coding messages ("fix the memory leak in the session manager": 9.6, threshold 5.0) would have had
  12,000 characters of it pre-injected under `requests`, and the review's related skills would have
  carried it too. Only `skill_view` loads it.
- **The skill carries the old long text word for word** ("Original description" in each section), so
  nothing was lost; when a Hermes update changes one of the four schemas, merge it there.

## Deferred tool schemas, `tool_search` (Phase 11 CP11.2)

- **This is the one place the tool list changes mid-session, on purpose, and it breaks the prompt
  cache.** Only in `tokens` (`budget.tool_schemas: deferred`): the request's `tools` are the core tools +
  `tool_search`; each time the model loads tools, the list grows (loaded tools are appended after
  `tool_search`, in load order) and the provider's prefix cache is missed from the `tools` array on,
  once per load. Accepted because the user said tokens matter more than cache hits in this profile and
  the list is ~12 KB (~3,100 tokens) shorter on every request. The system prompt never changes
  (frozen). Do not add a second place where a list changes mid-session; do not turn this on for
  `requests` (its point is the cache: one request per task).
- **A session is deferred when its frozen system prompt holds a *sealed* `# Deferred tools` block**
  (`hermes_prompts::deferred::is_deferred_prompt`), nothing else: no per-session flag, no side file. The
  profile decides it once, when the session's first prompt is built. **A compaction or a model switch
  keeps the session's kind of list** (`PromptTools::of_session`): they rebuild the prompt with the
  then-current profile's budget wording, but the block stays in a deferred session and stays out of a
  plain one (a flip would add or drop ~11 KB of tools under the model; `turns.rs`
  `a_compaction_or_a_model_switch_keeps_the_kind_of_tool_list`, all three directions).
- **The seal.** The block's last line is `<!-- End deferred tools <16 hex> -->`, an FNV-1a 64 of the
  prompt text before the block (`seal`, `find_block`). The block ends the stable tier, which is only the
  identity and fixed guidance, so a *copy* of the block (a repository's AGENTS.md or docs quoting it, a
  skill, a memory entry) never counts: it cannot make a `balanced` / `requests` session deferred nor be
  stripped from the review's prompt. **Never change `fnv64`** or the line's shape: every stored deferred
  session would turn into a plain one.
- **The loaded set is a function of the conversation** (`deferred::loaded_in`): every *answered*
  `tool_search` call (its `names`, at most 8, exact) and every answered direct call to a deferred tool
  loads that tool. So a restart, a branch and a rewind rebuild the same list with nothing stored (a rewind
  drops the later loads), and a compaction keeps loads because it hides messages from the model but
  keeps them in the conversation (a test asserts the `tool_search` call was hidden and the tool is
  still listed). **Consequence: `tool_search`'s argument handling and `loaded_in` must stay the same
  function** (`requested_names` + the 8-name rule): change one and an old session's list changes on
  resume. A call nobody answered (withdrawn, cancelled) loads nothing.
- **A deferred tool called before it was loaded is not run.** The model has not seen its schema; the
  answer is an error carrying the schema ("deferred tool ... not run ... call it again"), and the call
  counts as a load, so the retry costs the same one round trip `tool_search` would have. Auto-running it
  was rejected: arguments formed without the schema are wrong often enough (a small local model's
  `delegate_task` or `patch`). It is answered by `UnhandledToolCalls` (the tool operation only runs tools
  in the list), so `ValidateToolCalls` must keep seeing deferred names as valid (no invalid-name strike,
  no "does not exist").
- **Gating still holds.** The wrapper sits *inside* `GuardedTools`: a tool of a switched-off toolset is
  not listed, not run, and `tool_search` says "turned off" instead of returning its schema; an MCP
  tool of a server that is not connected is not in the inner list so it is "unknown". A loaded name
  that is switched off later is filtered at list time; switched on again, it appears with a schema the
  model never saw (rare; the model can call `tool_search` again, which says "already loaded").
- **`skill_manage` is absent for the main agent in `tokens`** and for its subagents; the review and
  the curator keep it (they use `ReviewTools`, whose list is the whole `SessionTools` list: the
  wrapper is only on the main agent's chain). Everything in the frozen prompt that told the model to
  call it is rewritten or dropped for these sessions (`PromptTools`, `hermes_prompts::deferred::
  without_skill_manage` for the skills block, the gated `SKILLS_GUIDANCE` / memory-routing sentence, the
  `memory` schema's "via skill_manage" clause), and the deferred block says the review saves skills.
  A test greps the prompt and every core description for `skill_manage`; a new prompt sentence that names
  the tool must be gated the same way. The review's fork gets the prompt with the block stripped
  (`strip_block`): its own instructions and tool list say the opposite.
- **Subagents** of a deferred session do not use `tool_search`: they get the whole small set (everything
  but `delegate_task`, `clarify`, `memory`, and `skill_manage`) with a plain prompt. They make few
  requests; the machinery would cost a round trip for tools they often need (web, session search).
- **Which MCP tools are core is recorded in the block** (`<!-- core MCP tools: a, b, c -->`, the first
  `budget.deferred_mcp_keep` connected when the session was made, server-name order; default 3: a small
  server stays whole, a large one's first three are core). Every other MCP tool is deferred, *including
  the tools of a server that connects later* (MCP servers connect in the background at start-up, so a
  session made or resumed early can see a server arrive mid-session): they are not named in the block,
  `tool_search`'s `query` finds them and `names` loads them. So a late server, a reconnect or a changed
  `deferred_mcp_keep` never moves a tool out of a running session's list (counting "the first N" anew
  every request did: a server whose name sorts first pushed a core tool out unseen).
- **Estimates.** The page's context breakdown (`context.breakdown`, `Runtime::context_breakdown`) counts
  what a deferred session sends (`Runtime::session_tool_definitions`: core + `tool_search` + loaded).
  `compress::tool_definition_tokens()`, the fixed part of the compression trigger's estimate before the
  provider has reported usage once, still counts all 18 schemas (~3k tokens high for a deferred session,
  until the first usage report): conservative, left as is.
- **`tool_search` is not in `hermes_prompts::tools::tool_names()`**, and `crates/desktop`'s
  `every_tool_belongs_to_one_toolset` does not know it: it is not a toolset tool (it cannot be switched
  off). The page and ACP show it as a generic tool call (ACP title "Load tools: ...").


## The provider fallback chain (Phase 12)

- **A switch sends the conversation to another provider.** That is the user's explicit choice (they listed
  it), and the notice names the provider each time. It is why only some failures switch (`hermes_llm::fallback::
  classify`) and why **401 / 403 never do**: a wrong key on the main provider would otherwise be hidden behind a
  working fallback, and the conversation would go to a provider the user did not pick for that failure. A user
  with only a local model (Ollama) who adds a hosted provider has chosen to send their conversation there when
  the local server is down; the Settings text says so. No key is ever read from the chain's entries: an entry
  names a provider configured under Providers, whose own endpoint and key are used.
- **`fallback_providers` is dropped entry by entry, with a warning in `logs\desktop.log`** (unknown or key-less
  provider, a model absent from a fixed model list, the main model itself, repeats, more than 3). A chain that
  "does nothing" is usually one of these: look in the log. A wrong model id on a discovering provider is not
  checked ahead; it shows as a skipped fallback ("does not serve the model") when it is needed.
- **The next turn tries the primary again**, so while the primary is down each turn spends one failed request
  (and its `http_retries`) before the fallback answers. Only a daily quota is remembered: the provider's own
  "N of N" 429, or our configured limit, keeps the primary refused until its reset (in memory: a restart asks
  again). To stop asking a dead primary, switch the model (or remove it from use) rather than waiting.
- **Prompt caching restarts on the new provider**, and the notice says so. A long conversation re-read on the
  fallback costs its whole input once; later requests of the turn are cached there. The fallback's context
  window may be smaller: the conversation is compressed for it first (the one allowed exception to "never
  rewrite history"), and a fallback it still does not fit is skipped. The compress step judges a fallback by the
  estimate of the whole request (characters / 4), not by the last usage the primary reported.
- **A half-delivered response is not re-asked on the fallback.** If the stream breaks after text reached the
  page, the turn ends with the error as before: switching would answer the question twice. Only a failure
  before the first chunk moves the request.
- **Changed retry rules** (`should_retry`): a daily-quota 429 (any of the wordings in `fallback.rs`, a
  Retry-After of an hour or more) and a missing model are no longer retried by the provider layer; before, the
  missing model cost `http_retries` extra requests, and a quota 429 whose body lacked the old markers was slept
  out up to the retry cap.
- **Model-not-found is read from the provider's words**, not the status alone (a plain 404 is a wrong URL, the
  user's configuration to fix, and does not switch). The URL the provider layer prefixes to a 404 is ignored,
  so a host named `models.*` is not "model not found". "model" and "not found" / "does not exist" must be in
  the same sentence, within 120 characters ("tool call id not found ... the model" is an ordinary 400).
- **The review, curator, titles, approval judge and `delegate_task` children do not use the chain** (and the
  compression summary uses the fallback only while the turn runs on it). A primary at its quota still makes
  those jobs wait or fail as before; the governor's "background jobs wait under 20%" keeps them from
  spending the last requests.
- **The page's Fallbacks sub-page is the page's own** (`fallback-models-field.tsx`, the `fallback_providers`
  key): it writes the extra routing keys its editor keeps (`base_url`...), which this backend ignores. It was
  hidden by `hide.css` until Phase 12 and **was not opened in a rebuilt page** (no webapp build in the
  worktree). Read from Hermes's source at `HERMES_COMMIT` instead: the sub-page (`config-subpages.ts`, fields
  `['fallback_providers']`) renders `FallbackModelsField` for that key, whose only backend calls are
  `GET /api/model/options` (served; the Auxiliary page uses it, `slug` = our provider id, which is what
  `fallback.rs` matches) and the ordinary config autosave. `scripts/perf/fallback-check.mjs` writes the key
  through `config.set` like the page's autosave. If the editor misbehaves in the app, re-add
  `nav-config:model:fallbacks` to `hide.css`; the key still works from the file.

## Shared messages (Phase 13a, `Conversation(Vec<Arc<Message>>)`)

- **Change a message through `Arc::make_mut`, never by assigning through the list.** `messages_mut()` gives
  `&mut Vec<Arc<Message>>`: `Arc::make_mut(&mut messages[i]).metadata... = ..` copies the message when a snapshot
  (the agent loop's `TurnSession`, a review's view, a `HistoryReplaced` event) still holds it, and edits it in
  place otherwise. `session::tests::a_snapshot_shares_the_messages_and_does_not_see_later_changes` is the
  contract.
- **`make_mut` on a message you just cloned an `Arc` of copies it.** `TurnRuntime::append` hands an `Arc` clone of
  the merged message to the store's writer queue (Phase 13b). While that batch is open (up to ~5 ms) the next chunk's
  `push` copies the message once instead of editing it in place; that copy replaced 13a's per-chunk JSON
  serialisation and commit on the turn's thread (the writer serialises only the last version per batch). Don't hold
  any other clone across the next `push`.
- **A projection must give back the very same `Arc` when it changes nothing**: `agent_visible_arc`,
  `messages_for_provider`, the repair passes. If you add a pass that rebuilds every message (a `.map(|m|
  m.clone())` or `agent_visible_content()` unconditionally) the request path is deep-copying again and nothing
  fails, so each pass has a "needs work" test first (`has_content_hidden_from_agent`, `has_mergeable_text`, the
  `unanswered` check) and `inference::tests::the_request_projection_matches_the_copying_one_and_shares_what_it_can`
  checks with `Arc::ptr_eq`. `has_content_hidden_from_agent` must stay in step with `filter_for_audience`.
- **`Conversation::to_vec()` and `into_iter()` copy.** `to_vec()` is for the page's transcript functions and the
  store import (a user's click); `into_iter()` moves a message nobody else holds and copies a shared one. Not for
  per-request code. Mind the name clash: `conversation.to_vec()` is a deep copy (`Vec<Message>`), but
  `conversation.messages().to_vec()` is the slice's own `to_vec`, a pointer copy (`Vec<Arc<Message>>`); the type
  of the binding tells you which one you got.
- **The repair passes are pinned to Goose's copying originals.** `conversation/pre13a_repair.rs` (test only) is
  the pre-13a `fix_conversation` over plain messages; `conversation::tests::
  copy_on_write_repair_matches_the_copying_passes` runs both over 4,000 generated conversations and wants the same
  messages, the same issues in the same order, an unchanged input, and every issue kind exercised (it also checks
  `has_content_hidden_from_agent` against `agent_visible_content` and the request-side merge). If you change a pass
  on purpose, change its copy in `pre13a_repair.rs` the same way.
- **Byte-identical requests are the contract** (prompt caching, ground rule 8). The turns suite under the default,
  `requests` and `tokens` profiles was compared request by request with the pre-13a output (see
  `docs/backend/progress/phase-13.md`); repeat that comparison when you touch a repair pass.

## The store's writer (Phase 13b, `crates/store/src/writer.rs`)

- **Every `Store` read waits for the writes sent before it; you don't flush for read-after-write.** The read
  methods go through `Store::read()`, which first settles the writer queue. A new read method must use
  `self.read()`, never lock the read connection directly, or it reads stale rows: `Runtime::reload_if_stale`
  compares the store's count with the live conversation and would "reload" a turn's history back to an older one.
- **A new write method goes through the writer**: `self.write(session, move |conn| ..)` (waits for its commit,
  ordered after every queued write), or a `StoreOp` for `Store::queue`. Never open a transaction inside it (the
  writer's batch is the transaction, the write runs in a savepoint) and never write on the read connection (it is
  `query_only`). The closure is `Fn`: a batch whose commit fails runs it again alone. Never `COMMIT`/`ROLLBACK` in
  it either; if its error ends the transaction anyway (SQLite does on a full disk or an I/O error), the writer sees
  `is_autocommit` and reruns every write of the batch alone, none of them twice. Tag it with the session it
  touches (`None` when it touches no session or several): the upsert coalescing in `writer::superseded` relies on it.
- **`Store::flush*` blocks; `Store::queue` doesn't.** From async code, flush through `spawn_blocking` (`flush_turn`
  in `runtime/src/lib.rs`). `queue` can be called under the conversation's lock (it is, in `append` and
  `update_message`, so two writers to one session keep their order).
- **A turn's end must flush before the page hears it.** Every `TurnFinished` in `runtime/src/lib.rs` is preceded by
  `flush_turn` + `after_save`; a new kind of turn (anything that queues writes and then reports "done") needs the
  same, or a kill right after "done" loses its last ~5 ms and a failed write is reported to the next turn instead.
- **`Err` from `Store::queue` is usually not about that write.** It is an earlier queued write of the same session
  that failed (taken: reported once). The turn stops on it, as it did on a synchronous error.
- **Another connection sees queued writes only after a flush**: the GPUI app, a test that opens a second `Store` or
  a `rusqlite::Connection` on the same file. After a turn has finished that is automatic; elsewhere call
  `store.flush()` first.
- **An in-memory store has one connection** (two `:memory:` connections are two databases): the writer locks it per
  batch, so a test that holds `store.writer.write.lock()` keeps the writer from starting (the writer tests use that to
  fill a batch on purpose).

## Never block an async worker (Phase 13c, `crates/tools/src/blocking_tests.rs`)

- **The rule is old (AGENTS.md rule 7); CP13b made it matter more.** A `Store` read now waits for the writer's pending
  commit, so a store read from async code can hold a worker for a commit, not just for a SELECT. The runtime has 4
  workers: four blocked ones stop the heartbeat, every other request and the turns' event loops. A blocked worker is
  invisible in a test that runs on many threads.
- **What counts as blocking in an `async fn` / `async {}`**: `std::fs`, `Path::exists/is_dir/metadata` on a path that may
  be slow (a network drive), `std::process::Command` (the linters, git), any `Store::*` / `rusqlite` call, a
  `SkillStore` / `MemoryStore` / `LocalIndex` method (they read their folders; `SkillStore::find` and `catalog*` walk
  the skills folders every time), a threat scan the first time (`hermes_memory::threats` compiles its patterns lazily:
  ~0.5 s in a debug build; `provenance::SourceNote::new` goes through it), `thread::sleep`, `reqwest::blocking`, a
  `Mutex` guard held while doing any of these. Also the sync helpers an async fn calls.
- **The fix**: `tokio::task::spawn_blocking(move || ...).await` with owned data moved in (clone the `Arc`s and `String`s
  first), a `JoinError` turned into an error value (`unwrap_or_else`, never `unwrap`), or a tokio equivalent
  (`tokio::fs`, `tokio::process`). Never move a `std` mutex guard into the closure and never hold one across the
  `.await`: take the lock inside the closure, from an `Arc<Mutex<..>>` you cloned (`SessionTools::hints`,
  `turn_checkpoints`). Do the cheap in-memory check first and spawn only when there is work (`limit_output_off_worker`,
  `spill_oversized_off_worker`, `Web::truncate_off_worker` copy nothing for a short result).
- **The gateway has a helper for it**: `Gateway::blocking(|g| ...)` (`gateway/mod.rs`) runs a closure with the `Arc<Gateway>`
  on the blocking pool and answers a failed thread as an RPC error. `dispatch`, `mutate`, `rest`, `session_route`, the
  turn's event loop use it for every store read, file read and repository call. A method that needs it must take
  `self: &Arc<Self>`. In a REST route use `Gateway::rest_blocking`: a failed thread answers 500, never `None` (that
  is the 404 "No such API endpoint" the page reads as "feature absent"). What is left on the workers is listed in the CP13c table of
  `docs/backend/progress/phase-13.md` (single-file reads of the small settings files, `is_file` stats).
- **After Stop, a tool call must finish without waiting.** The agent loop races a check against Stop with `biased` priority for
  the call (`runtime/agent_loop.rs`, `AgentLoop::pass`; `agent/machine.rs` before Phase 13e): when Stop fires, the call is polled once more and, if it still has an `.await` to
  get through (a `spawn_blocking` hop), it is dropped and the chat gets "Tool call was interrupted before completing"
  instead of the output a stopped command had printed (`stopping_mid_command_keeps_the_output_it_had_printed` caught
  this when the hints and checkpoint follow-ups of `SessionTools::call` first became `spawn_blocking`). So follow-up
  work after a tool's own work (hints, `record_write`, `limit_output_off_worker`, `spill_oversized_off_worker`) goes
  through `hermes_tools::off_worker_unless_stopped(cancel, ..)`: on the blocking pool normally; after Stop, inline
  if it has not started, else waited for on this thread (a std channel, no `.await`), so the call completes in the poll
  that follows Stop. Checking `is_cancelled()` once before the hop is not enough: Stop can land while the hop is running
  (`stop_during_a_follow_up_keeps_the_calls_result` shows a plain hop losing the result). Work before the call may hop
  freely. Not shielded, on purpose: `lint_off_worker` (`write_file` / `patch`) and `truncate_off_worker`
  (`web_extract`): those tools already had real `.await`s (`tokio::fs`, the network) where Stop drops them, as before
  Phase 13; only the terminal and `run_script` are built to answer after Stop.
- **Order still holds**: a tool that must run before another call (a checkpoint before a write) is awaited, not
  fire-and-forget; a blocking closure takes the conversation lock like any other caller (it is a `parking_lot` mutex,
  not held across an `.await`).
- **The test**: `blocking_tests.rs` runs the memory, staged-memory, `session_search`, `skills_list` / `skill_manage` /
  `skill_view` tools on a `current_thread` runtime (one thread: a blocked worker stops everything) next to a timer that
  ticks every 20 ms. A test-only hook (`crate::slow::set(session_id, delay)`, called by `test_pause` inside each
  blocking closure) makes the work take 300 ms; the longest gap between ticks must stay under 150 ms. The control test
  (`a_blocked_worker_is_seen_by_the_timer`) sleeps inline and must see the stall. Use a session id of your own per test:
  the hook table is global. To cover a new tool, call `test_pause(&session_id)` first in its closure and add a case.
  It measures wall-clock time: under a heavily loaded machine the 150 ms tolerance can in principle fail; a real block
  shows 300 ms or more, so re-run before suspecting the code.
- **A test that stops a script must wait for the script, not a timer.** PowerShell's start-up time varies a lot
  between machines and days (security scanning of `-EncodedCommand`, a cold .NET):
  `stopping_the_turn_kills_the_script_and_removes_its_scratch_folder` sent Stop after a fixed 1.5 s and began
  failing on HEAD too, before the script had printed anything (CP13d review). It now waits for a marker file the
  script writes after its first line (deadline 90 s), then checks that Stop ended the two-minute sleep within 30 s
  and kept the output. Do the same in new tests of that kind.

## The session actor (Phase 13d, `crates/runtime/src/session_actor.rs`)

- **A session's control state has one owner: its actor's thread.** Who runs the session (the run slot and the
  turn's cancel token and steer box), its agentic review's slot, the review counters, which `OpenSession` is
  loaded, and the page's state (queue, live, pending model pick, effort, revision, held images) are changed only
  by `Command`s (`SessionHandle::ask` / `ask_async`). Don't add a `Mutex` field to `OpenSession` or a
  `HashMap<session_id, ..>` to `Gateway` for new per-session state: add a field to `SessionState` / `PageState`
  and a command. The data (`record`, `conversation`, `context`) stays on `OpenSession` (see the CP13d notes).
- **A handler never blocks**: no store, file, network or process work, no `.await`, no call to any
  `SessionHandle` (its own or another session's: refused as `Gone::Reentrant`, a debug assertion), no lock that a
  caller may hold while it waits for an answer. The one outside lock a handler takes is the review scheduler's
  `states` (`ClaimReview` -> `Scheduler::is_busy`): **never ask an actor while holding `states`** (lock order:
  actor, then `states`). File work that belongs to a claim (the `TurnLock`) is done by the caller.
- **Ask with no lock held that a handler takes.** Today that is only `states`; holding the conversation or record
  lock while asking is safe (no handler takes them), but don't make a habit of it.
- **Wait for every command.** A command's effect is in `SessionHandle::status()` once its answer arrived; a
  fire-and-forget one would leave readers (`is_running`, `has_queued`, `session_info`) seeing the old state. The
  `RunGuard` waits for its `Release` in `Drop` for exactly this: the gateway starts the next queued prompt right
  after `TurnFinished`, and must find the slot free.
- **Check-then-act on another caller's state is one command.** "Queue it if a turn runs" is
  `enqueue(.., only_if_running: true)`, "keep the model pick if a turn runs" is `set_pending_route`, "drop it if
  idle" is `detach(true)`, "start a review unless ..." is `ClaimReview`. Reading `is_running()` and then sending a
  plain command leaves a gap a turn's end can fall into (the queued prompt nobody runs).
- **Stop never queues.** `SessionHandle::cancel` cancels the published token directly. Keep it that way: a
  `Command::Cancel` would wait behind whatever is queued.
- **The run slot is RAII.** Hold the `RunGuard` for exactly as long as the session is busy and drop it where the
  slot must be free (before `TurnFinished`); every early `return` / `?` frees it. Don't keep one in a struct that
  outlives the turn, and don't store it in the actor (its `Drop` asks the actor).
- **Reads never start an actor; writes may.** `Runtime::existing_session_handle` / `SessionRegistry::get` for
  reads (the archive sweep asks 100,000 ids), `session_handle` / `get_or_spawn` to set something (an effort may be
  picked for a session never loaded). A thread per session that was ever used in this process: closing keeps it
  (the page state survives, as the gateway maps did); only a delete ends it.
- **A stopped actor answers `Gone`, it never hangs.** Map it to an error or a safe default at the call site (see
  the CP13d notes); don't `unwrap` it. An actor whose loop died (a panic outside a handler; delivering an answer
  is caught too) is dropped by `SessionRegistry::get` / `get_or_spawn` on the session's next use, its last
  published turn cancelled, and a fresh actor started: without that, every later call for the session would
  answer `Gone` until the app restarts.
- **Nothing heavy is dropped on the actor's thread either.** A value a handler throws away runs its `Drop` there:
  `Attach` hands a losing caller's `OpenSession` (its tools) back instead of dropping it. Keep it that way for
  new commands that replace a value with `Drop` work.
- **`Runtime::open` starts the actor only after the store has the session**, so an unknown id (a stale page
  asking for a deleted session's messages) leaves no thread behind.
- **Tests**: `session_actor/tests.rs` (order, many threads, a panicking handler, a stopped actor, commands queued
  when the thread ends, re-entrancy, lifecycle, the one-thread runtime, the run slot and Stop's priority path, the
  review pause and the review pair raced 300 times, the page state). A test that needs an `OpenSession` uses
  `session::testing::open_session`.

## The agent loop (Phase 13e, `crates/runtime/src/agent_loop.rs`)

- **One pass, one action, and every pass starts at the top.** The checks are asked in a fixed order
  (budget, validate, tools, unanswered calls, `/steer`, length, empty, provider error, compression, inference) and the
  first that acts ends the pass. Do not "fall through" to the next check after one acted, or skip back to the middle:
  after tools ran the iteration budget must be asked again before the next request, a repaired tool call is
  re-validated against a fresh snapshot, and so on. A new rule is a new check in `PASS` (and in `AgentLoop::check`)
  at the place its condition must be judged, returning `Ok(None)` when it has nothing to do.
- **`Some(Outcome)` with no effects is not `None`.** `ValidateToolCalls` returns `Retry(InvalidToolName)` with no
  effects to end the pass so that `UnhandledToolCalls` answers the invented names in the next one; turning that into
  `None` would let tools / unhandled run in the same pass with the old snapshot.
- **Each pass loads a fresh snapshot, and `load` takes the one-shot output cap** (`TurnState::max_tokens`, set by the
  truncated-tool-call retry). A pass that does something else first (a compression) consumes it; that is how it has
  always been: keep it, or the request bytes change.
- **Counters live in `TurnState`, not in message metadata.** The one note on a message the loop reads is the inference
  step's empty-response marker (`operations.inference.empty_response` on a hidden assistant row); it is stored in
  `sessions.db`, so do not drop or rename it. Other operation notes (`preinject`, `compress`, `command`, `references`,
  `foreground_subagent`) are data other code reads (search, transcripts, compaction), not loop plumbing.
- **Stop**: see "After Stop, a tool call must finish without waiting" (Phase 13c section). A check that finishes in the
  poll that sees Stop keeps its effects (they are applied), the pass ends as cancelled, and `finalize` runs; a check
  still waiting is dropped and named as the interrupted one, whose leftovers (`finalize_cancellation`: the inference
  step's streamed text and usage) are saved first. An error from a check after Stop is a Stop, not a failure.
- **Tool names are unique in a request.** `ToolOperation::available_tools` refuses a name two providers (or one provider
  twice) offer ("multiple tool providers registered 'x'"); Goose's machine had a second check across its steps, gone
  with them. Keep the one in `tool.rs` (`a_tool_name_offered_twice_is_refused`): a provider rejects a duplicated tool.
- **Tests**: `agent_loop_tests.rs` checks each rule's `Next` at its boundary under the three profiles' knobs, and its
  `whole_loop` module the pass order and Stop over a real `TurnRuntime`; the turn
  behaviour is `turns.rs` (unchanged by CP13e). A change to the loop should be compared with a golden trace (every
  request body, agent event and queued store write per test, before and after): the method is in
  `docs/backend/progress/phase-13.md`, CP13e.

## The typed gateway dispatch (Phase 13f, `crates/desktop/src/gateway/rpc.rs`)

- **Never read a method's params with serde's struct deserializer.** The page sends loose JSON and the handlers always read
  `params["f"].as_str()...`: a missing field, `null` or absent params, extra fields and a number where a string was
  expected were never an error. Serde would reject `null`, read an array positionally, and turn "invalid" into a failed
  parse; `Request::parse` answers a failed parse as **-32601 "Method not found"** (the page then hides the feature), so a
  params type that can fail makes a served method vanish. Each params type implements `FromParams` (the same reads) and
  is wrapped in `P<T>`; `every_served_method_parses_from_any_params` is the guard. A new field keeps the rule: absent and
  wrong-typed are the same, a default is spelled out.
- **A method that passes the params on to a long function** (`session.create`, `prompt.submit`, `session.mutate`, the
  branches, ...) carries `Raw`, the value as sent (they read 10-30 fields and mutate it: `enqueue_as` writes
  `session_id` / `submission_id` into it, which panics on a non-object: that is how it always was). Typing them is a
  separate change with its own golden record.
- **`RpcError::server("session busy")` is `Busy`** (same code and text, plus `data.reason`): the page's canonical protocol
  reads the reason, and the old `handle` added it for that exact message whatever wrote it. Build errors with
  `RpcError::server` / `invalid` / `failed`, not the variants, so this holds. `Gateway::call` still answers the ACP server a
  `(code, message)` tuple (`acp/rpc.rs` has its own `RpcError`; they are different types).
- **`dispatch` loads `Settings` before it parses**, for every name, as before; an unknown name costs that file read.
- **The area files are `impl Gateway` blocks in child modules.** A private item of `gateway/mod.rs` (`Gateway`'s fields,
  `blocking`, `emit`, `session_id`) is visible to the children; a handler the `dispatch` arm calls, or another area uses, is
  `pub(super)`. The moved code starts with `use super::*;` (it reads as it did in one file). `Gateway::blocking` and
  `rest_blocking` (CP13c) are in `mod.rs` / `rest.rs`; the off-worker rules did not change.
- **`scripts/contract.mjs` reads `gateway/rpc.rs`** (`rename = "<method>"`) to say which contract methods are served.

## `hermes_core` (native UI Phase 1, `crates/core`)

- **Two kinds of event delivery.** `Core::subscribe()` is a channel (the native UI); a `Listener`
  (`Core::listen`) is called in the publisher's thread, in order (the gateway's `Hub`). The page's
  frames must keep their order with the ones the gateway sends itself (`message.start` before the
  first `message.delta`, `approval.settled` before the next `tool.complete`), so the hub is a
  listener, not a subscriber. A listener never blocks and never publishes from `deliver`.
- **A question counts as asked only if a listener showed it** (`publish` returns `true`). The hub
  shows a session's events only when a socket follows it, so an approval in a session nobody has
  open is denied at once, as before. A `subscribe()` channel shows everything while its receiver
  lives: a front end that subscribes and never answers holds approvals open until the turn is
  stopped.
- **The app lock is per data folder** (`<data>/app.lock`, held open with no sharing like
  `turnlock`). The ACP agent does not take it (it runs beside the desktop app); the native app
  must take it before it opens its window (when it starts using a `Core`, Phase 3). Tests and the
  perf instance use their own data folders, so they never meet the user's app.
- **The turn runs in the core** (`hermes_core::turns`, CP1.2): `Core::submit` with a front end's
  `TurnHooks`. A new stream event goes in `hermes_core::stream` and its frame in `hub.rs`; a page
  frame that needs the store (like `session.info`) goes in a hook, not in the hub listener (a
  listener runs on the turn's task and must not block). The loop calls every hook on a blocking
  thread and awaits it before the next event, so the frames keep the order they had when the loop
  was the gateway's (`message.start`, then `session.info`; `TurnEnded`, then `message.complete`).
  The hooks of the turn that ran are the ones its queued follow-up starts with (the page's socket
  that sent the first prompt keeps following, as before).
- **The queue is JSON in the session's actor** (`PageState::queue`): each item is a `SubmitTurn`
  written with serde under the page's names (`submission_id`, `text`, `shown`, `images`,
  `truncate`), because `dequeue` looks prompts up by `submission_id`. Read it with `Core::queue`,
  never `handle.page().queue` directly; renaming a `SubmitTurn` field breaks the lookup.
- **Queue changes reach a front end twice**: as `CoreEvent::QueueChanged` and through the
  operation's hooks (`changed`: the page's `session.info`). The hub ignores the event.
- **A queued prompt that can't start** (an attached image gone, no provider) publishes
  `QueuedFailed` with its admission and the error, then `Idle`; no `TurnStarted`/`TurnEnded`
  follows for it. The page hears it through the `queued_failed` hook (`message.complete` with the
  error); the hub ignores the event.
- **Queue races the old gateway had, kept as they were:** a steer whose turn ended between the
  check and the steer, an edit (`replace_queued`) or a redirect landing just as the turn ends, put
  the prompt back in the queue with no turn left to run it (it runs after the next prompt). Rare
  (a window of one actor round trip); a fix is an `only_if_running` enqueue that then submits.
- **A terminal's output is not a `CoreEvent`**: `Shell::attach` gives the snapshot and a per-shell
  broadcast stream; a newer attachment supersedes the older (`is_current`). `Terminals::new`
  takes the app's version (`TERM_PROGRAM_VERSION`), not the core's.
- **A question's waiting future cleans up after itself.** If the turn drops it (its cancel token
  fires without `Core::stop`, an editor closes the thread), the open question is removed and
  `Settled` published, so `is_waiting` and `session.resume`'s `prompts` never show a dead card.
- **One config record, shared** (CP1.3): `Core` owns the `ConfigRecord` (`desktop-config.json`);
  the page's `Rest` holds the same `Arc` (`Rest::with_config`), so a `config.set`, a Settings page
  and `Core::apply_settings` see one copy. `Rest::new(data_dir)` (a record of its own) is for
  tests only: two records over one file overwrite each other.
- **`apply_settings` does not talk to the page.** `Core::apply_settings` puts the record on the
  runtime and nothing else; the gateway's wrapper then sends `session.info` for the followed
  sessions (the approval mode shows in the status bar) and calls `Core::sweep_archive`. A native
  front end that shows the approval mode refreshes after calling it. Call it after every change
  to the record (`ConfigRecord::set_value` / `save` do not apply anything themselves).
- **`Core::new` applies no Settings; `Core::start(background)` does** (CP1.3 review): the record
  on the runtime, the auto-archive sweep, the scheduled curator (`background`: not for ACP) and
  the switched-off skills. Every front end calls it once after `Core::new`, off the async workers;
  without it a native app would run with manual approvals and every skill on, whatever Settings
  say.
- **Settings are read when needed, not cached**: `known_context`, `turn_options`, `provider_for`
  and the others load `config.json` each call (a small file; the gateway did the same), so a
  change made through the endpoints or by hand applies on the next call. Only a provider's answer
  about a model (its window, vision) is cached (`Core::forget_model_facts` after an endpoint
  changes).
- **The curator reports as events**: `CuratorStarted` / `CuratorFinished { ok, lines }` carry no
  session (`CoreEvent::session_id()` is empty for them); the hub ignores them and the page still
  polls `/api/actions/curator-run` (`Core::actions`). A front end that listens for sessions'
  events must not index by that id. Every `CuratorStarted` gets its `CuratorFinished`, also when
  the run panics (`ok: false`); only a runtime shutting down mid-run ends without one.
- **A deferred model pick calls back**: `Core::switch_model` takes a `changed` callback it calls
  at the point the session's model changes (or the pick is deferred), so the gateway's
  `session.info` keeps its place before the default is saved. `Core::apply_pending_route` returns
  whether it moved the session; the caller announces it.

## The model catalog (Phase 14, `crates/llm/src/canonical/`)

- **It is a trimmed copy.** `data/canonical_models.json` holds 1,371 rows of 27 catalog providers (the script lists 32 names; `mistral`, `qwen`, `meta-llama`, `amazon` and
  `microsoft` have no rows of their own) and 10 fields, made from
  Goose's full models.dev catalog (8,744 rows, last in git at `a7c24dc`) by `scripts/trim-model-registry.py`. A lookup
  under a provider that is not in the script's `PROVIDERS` finds nothing and falls through to `name_builder.rs`'s
  inference rules, silently. So: a new name in `discovery::catalog_provider_for`, `map_provider_name`,
  `infer_provider_from_model`, `extract_provider_prefix` or `is_meta_provider` must be added to `PROVIDERS` and the JSON
  regenerated from the full catalog; a new field read from `CanonicalModel` must be added to the script's `trim`.
- **What the app looks up**: the model picker, context lengths and prices come from `discovery::list_models`, which uses
  `catalog_provider_for(base_url)` (a fixed host table; every local or unknown endpoint is `"openai"`), never the endpoint's
  id; `formats/openai.rs` / `model.rs` use the fixed `"openai"`. The endpoint id the user typed (`Provider::get_name`)
  reaches only `model_info_for_provider_model` (`fetch_model_info`), of which only `resolved_model` (always `None`) is read.
  So an endpoint named like a dropped gateway (`poe`, `requesty`) changes nothing visible.
- **Checking a catalog change**: build `crates/llm/examples/registry_probe.rs` before and after (it compiles at
  `a7c24dc` too), run both on the same provider and model lists, `cmp` the outputs.
- No remote refresh: Goose's models.dev fetch/cache was never called here and is deleted; nothing writes a catalog file
  into the data folder.

## Test folders that do not go away (2026-11-17, `crates/testdir`)

**What happened.** The user's `%TEMP%` filled with ~50,000 folders named `.tmpXXXXXX` and 300 GB
of disk. Each held a complete test data directory: `sessions.db`, `memory.db`, `index.db`,
`requests.json`, `running/` and a full copy of the bundled skills (about 4 MB, 690 files). They
were made by *tests* (the app itself creates none; its only temp folders are the lint scratch
folder and the skill-batch backup, both removed when done). One run of the `turns` suite left ~340
of them (1.3 GB), the `acp` test ~60 (216 MB), `hermes_tools` ~60 (28 MB): a few hundred runs by
a person and by the agents did the rest.

**Why.** `tempfile::TempDir` removes its folder in `Drop` with one `remove_dir_all` and ignores the
result. On Windows `remove_dir_all` stops at the first file that is still open, and SQLite opens
its files without delete-sharing, so a runtime, store or thread still holding `index.db` kept the
whole folder, skills included. Two things made it common: struct fields drop in declaration order
and the fixtures declared the `TempDir` BEFORE the runtime or stores that use it (so the folder was
removed while the files were open); and some owners outlive the test (the ledger thread keeps
`requests.json` open until the process exits).

**Rules**
- In `hermes_tools`, `hermes_runtime` and `hermes-desktop` tests, make folders with
  `hermes_testdir::tempdir()` (a drop-in for `tempfile::tempdir()`; the type is
  `hermes_testdir::TestDir`), never `tempfile` directly. It deletes everything it can, retries
  for ~1.5 s while a file's owner lets go, and prints `hermes_testdir: could not remove ...` when
  something is left, so a leak shows in the test output. Production code keeps using `tempfile`.
- In a fixture struct, declare the `TestDir` field LAST (after the runtime, stores, servers: they
  drop first and release their files). A comment on the field says why.
- Its folder name stays `tempfile`'s default `.tmpXXXXXX` on purpose: the local skill retrieval
  reads the workspace folder's name as words, and a prefix such as `hermes-test-` made "hello"
  attach the `test-driven-development` skill (four `turns` scenarios failed).
- Do NOT point `TEMP` at another folder to catch leaks: `hermes_tools`'s taint test treats paths
  under `%TEMP%` as scratch and fails when the whole test area is under it.
- What still stays (~0.4 MB per affected test: the database files a thread holds until the test
  process exits, ~85 MB per `turns` run) is swept with
  `powershell -NoProfile -File scripts/clean-test-temp.ps1 -Delete` (dry run without `-Delete`;
  it only touches `.tmp` + 6 characters folders directly under `%TEMP%` older than 10 minutes).
  Run it after test runs, and before reporting disk numbers.
- A leak check: run a suite and count `.tmp*` folders in `%TEMP%` before and after (the 2026-11-17
  numbers: `turns` 326 folders / 1.31 GB before, 212 / 84 MB after; `hermes_tools` 59 → 0; the
  runtime lib 7 → 0; `acp` 59 / 216 MB → 22 / 7 MB; `hermes-desktop` lib 8 → 3).
- A test that spawns a process on its folder must end the process AND its children, and wait,
  before the folder is removed (2026-11-18): `acp.rs`'s `Agent` only had `kill_on_drop`, which
  kills the process but not the `git init` it started on the skills folder; that git re-created
  `skills/.git` after `hermes_testdir` had cleaned up, one empty-looking `.tmpXXXXXX` per test
  (45 in a few runs). `Agent::drop` now runs `taskkill /T /F` and waits. `acp` leaves 0 folders.

## The shell sandbox (Phase 15, `crates/tools/src/sandbox.rs`, `sandbox_labels.rs`)

- **`terminal.sandbox` is config-only** (`desktop-config.json`, `"terminal": {"sandbox": "low",
  "sandbox_grants": ["D:\\data"]}`), like `terminal.env_passthrough`: no Settings field. `off` (the
  default, also for any unknown value) is tokio's spawn as before: no label, no note, no variable.
  The level is read at each spawn. `/sandbox` shows the level, the grants and the folders labelled;
  `/sandbox remove-labels` puts them back.
- **Git Bash does not start at Low.** Every MSYS program (Git's `bash.exe` and everything in Git's
  `usr\bin`: `ls`, `sed`, MSYS `whoami`...) dies at start with `NtCreateDirectoryObject(\BaseNamedObjects\
  msys-2.0S5-<hash>): 0xC0000022` (exit 0xC0000142). `\BaseNamedObjects` itself is labelled Low
  (a Low process *can* create a new directory there), but that directory is shared by every MSYS
  program of the same Git install (the hash is of the install path) and already exists, labelled
  Medium, whenever any unsandboxed MSYS program runs (the user's Git Bash, an `off` terminal):
  opening it for write is "no write up". Native programs (`git.exe`, `cargo`, PowerShell,
  `whoami.exe`) run. A copy of Git's `usr\bin` in another folder (another hash, so its own
  directory, made Low) does start at Low (checked by hand 2026-11-17), but is not used.
  **Decision (CP15.2): under `low` every command runs in PowerShell**, also when the terminal is Git
  Bash: the terminal (a bash started before `low` was turned on is stopped: it ran unsandboxed;
  the folder carries over, exported variables don't), background processes, and `run_script`
  refuses `language: bash` up front. Each result says `sandbox=low: ran in PowerShell, not bash`
  (the `sandbox` field), so a bash-syntax failure is understandable. The system prompt and the
  tool descriptions still say bash (rule 8: they are frozen for the session); only the results
  say otherwise. Git commands that run `sh` (hooks, an editor, `git difftool`) fail at Low too.
- **What `low` may write:** the session's workspace (the repository root, else the folder it
  started in), its scratch folder (`TEMP` / `TMP`), the shared cache folder
  (`<data>\sandbox-cache`) and the grants, each labelled Low by the first `low` command
  (`sandbox_labels::prepare`, idempotent, on a blocking thread), plus whatever Windows already
  labels Low (`AppData\LocalLow`). Refused (said in the result's `sandbox` field, the command
  still runs), after the path is made canonical (so 8.3 short names, `\\?\`, case and `..` don't
  dodge it): drive roots; the profile, the data folder, `APPDATA`, `LOCALAPPDATA` and every folder
  on `PATH` (and any folder containing one of them: `C:\Users`, `AppData`); `Windows`, `Program
  Files`, `ProgramData`, `PUBLIC`, `~\.ssh`, `~\.cargo`, `~\.rustup`, the Start Menu (Startup)
  and PowerShell's profile folders in Documents (and anything in or around them); any other folder
  under `C:\Users` than your own profile; anything inside the data folder but scratch and cache;
  and folders that already have a label we didn't set. Why the extra places (CP15.2 review): a
  Low-writable folder is one a sandboxed command can plant a file in, and these are where a
  planted file would later run at the user's own level. The same holds, by design, for the
  workspace: a sandboxed command can edit `build.rs`, `package.json` scripts or `.git\hooks`, which
  run unsandboxed when *you* build or commit. The sandbox stops writes outside the workspace, not
  what you later run from it. A session started in the profile itself, or in a folder containing
  the data folder, gets no writable workspace.
- **Labels are set one object at a time through a handle (`SetKernelObjectSecurity`: what
  `SetFileSecurityW` does after opening the file), never `SetNamedSecurityInfoW` or `icacls`.**
  Those re-run Windows' inheritance over the tree and rewrite DACLs on the way (they
  mark them auto-inherited, `D:AI`; `SetNamedSecurityInfoW` also turned inherited entries into
  explicit ones on this machine's folders, whose DACLs are not marked auto-inherited), so the
  descriptors could not be put back. Our walk gives every file and folder inside the inherited
  label (`(ML;ID;NW;;;LW)`, `(ML;OICIID;NW;;;LW)`) and nothing else; removal takes those off
  deepest first and puts the folder's own label back from the record. Every object is opened with
  `FILE_FLAG_OPEN_REPARSE_POINT` (a link is opened as itself), skipped if it is a reparse point,
  and skipped unless the handle's real path (`GetFinalPathNameByHandleW`) is still under the
  recorded folder; the label is then read and set through that same handle. So a folder that a
  sandboxed command swaps for a junction mid-walk (it can, inside a Low folder) never gets a label
  applied through it, and removal never follows one either: a recorded folder that is now a link
  or leads elsewhere is reported as changed and forgotten, untouched. The walk also stops at labels
  that aren't ours. Checked again in the CP15.2 review: `icacls /setintegritylevel (OI)(CI)low` on
  a fresh folder turned `D:(...)` into `D:AI(...)` on every object below it.
  Read descriptors with `GetFileSecurityW` too: `GetNamedSecurityInfoW` shows inheritance it
  computes (`ID` flags) that isn't what is stored.
- **One difference after removal:** an object that had no SACL at all reads `S:NO_ACCESS_CONTROL`
  afterwards (an empty SACL: no label, no audit, the same meaning). Removing it needs
  `SeSecurityPrivilege` (`SACL_SECURITY_INFORMATION`), which the user doesn't hold. The tests
  compare everything else exactly.
- **The first `low` command in a large workspace is slow:** the walk reads and sets the label of
  every file (a `target\` with 100k files takes seconds to a minute). It runs once per folder (the
  record marks it complete; a walk cut short is finished by the next command). The record's lock
  is held meanwhile, so another session's first `low` command waits (the walk runs on a blocking
  thread, not the async workers; there is no time limit). The lock is this process's mutex plus an
  exclusive lock on `sandbox-labels.lock` in the data folder, since the desktop app and
  `hermes-acp.exe` share the record; the record is written to a temporary file and renamed.
- **Hard-linked files are not labelled.** A label belongs to the file, not to one of its names: a
  file hard-linked into the workspace (pnpm's store, some build caches) would become Low-writable
  under its other names too, outside the workspace. Files with more than one name are left as
  they are (read-only to commands, which can still delete and recreate them), and the first `low`
  command says how many.
- **Caches** (tested at Low on this machine, CP15.2): cargo builds from crates it already has but
  can't download into `~\.cargo\registry` ("Access is denied"); npm fails outright (`EPERM` on
  `%LOCALAPPDATA%\npm-cache`); uv fails outright ("Failed to initialize cache"). So under `low`
  `CARGO_HOME`, `npm_config_cache`, `YARN_CACHE_FOLDER`, `UV_CACHE_DIR`, `PIP_CACHE_DIR`,
  `GOCACHE`, `GOMODCACHE` point into `<data>\sandbox-cache` (shared by the sessions, so crates are
  downloaded once, not per session; pip and go were not installed here and are set on their
  documentation). The user's `~\.cargo\config.toml` is copied along, without `credentials.toml`
  and without any `token`, `secret-key` or `secret-key-subject` key (a credential should not live
  in a second, Low-writable place and outlive its rotation; a config that isn't TOML is not
  copied). npm's and pip's own config files are not copied (only their cache folders move).
- **A blocked write** shows in the result's `sandbox` field as "blocked by sandbox: <path>; grant the
  folder in Settings (terminal.sandbox_grants in desktop-config.json) or move the work into the
  workspace", when the output has "Access is denied", "Access to the path", "Permission denied",
  "os error 5", `EPERM` or `UnauthorizedAccessException` on a path outside the writable folders
  (a locked file in the workspace says "Access is denied" too: no hint then). Read from the
  terminal's and `run_script`'s output; a background process's output is not read for it.
- **`whoami` in a test is Git's** when Git's `usr\bin` is on PATH: call
  `%SystemRoot%\System32\whoami.exe` to read a token's label (`/groups`: Low is `S-1-16-4096`,
  Medium `S-1-16-8192`, High `S-1-16-12288`; the SIDs don't depend on the Windows language).
- **Handles.** The `low` spawn makes its pipes non-inheritable, makes the child's ends inheritable only
  around `CreateProcessAsUserW` and passes them in a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. A
  `std::process` spawn on another thread in that window can still inherit them (std holds its own
  lock, which we can't take; Windows requires the listed handles to be inheritable): the cost is a
  pipe that ends late, not a leak. It does happen: `cargo test -p hermes_tools` (many `off` shells
  spawning beside the `low` tests) once lost a `run_script` output to it. So the readers don't
  depend on EOF: the bash path reads to its sentinel; the PowerShell path, under `low`, stops
  reading 1 s after PowerShell exits; `run_script` keeps what it read when its 5 s drain gives up
  (it used to return nothing). Closing the window itself needs a broker process (or
  `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS` with a helper process that never spawns anything).
  `sandboxed_spawns_leak_no_handles` counts the process's handles over 30 runs each of a command,
  a piped-stdin spawn and a failing `CreateProcessAsUserW`, in a child test process.
- **Tests label only their own folders.** Under `cfg(test)`, `ensure_low` panics on any folder
  outside the temp folder; every test that labels holds an `Unlabel` guard that runs
  `remove_all` on its own record when it ends (also on a panic). The level and the grants are
  process-wide, so tests set them per shell (`set_sandbox_override`, `set_grants_override`).
