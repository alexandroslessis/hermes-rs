# Pitfalls

Things that cost hours once. Read this before debugging; check here first when something is odd.

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
