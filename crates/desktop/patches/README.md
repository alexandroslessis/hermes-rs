# Frontend patches

`scripts/build-hermes-webapp.ps1` applies every `*.patch` here (in name order) to Hermes's
exported source before building the webapp. A patch that no longer applies stops the build:
refresh it against the new Hermes commit (edit the exported file, then diff it against
`git -C ../hermes-agent show <commit>:<path>`).

- **0001-queued-prompt-bubble.patch** (`use-message-stream/gateway-event/message-stream.ts`):
  a queued prompt the backend starts had no user bubble (none until the session was reloaded):
  with the canonical protocol the backend owns the queue and no event draws one. The backend's
  `message.start` names the started admission as `turn_id` (sent before the `session.info` that
  retires its card); the patch turns that queue card into the user's bubble, id
  `user-<admission>` so the turn's `persisted_turn` binds its stored row. Redirects and missed
  steers re-run from the queue are drawn by the composer already, so the backend doesn't list
  them as cards (`shown`). Tested live 2026-11-04: a queued prompt shows its bubble when its
  turn starts, once (also after a reload); Enter-steer shows once.

- **0002-native-folder-dialog.patch** (`lib/desktop-fs.ts`): choosing a folder (a project's, a
  session's) asks the backend first, which opens Windows' own folder dialog
  (`POST /api/hermes-rs/select-folder`, `rfd`); a backend without that route keeps Hermes's web
  picker (served by `/api/fs/list`).

- **0003-lazy-locales.patch** (`i18n/catalog.ts`, `i18n/registry.ts`), performance: only English
  is in the startup bundle; the other eight bundled languages (~1.5 MB of the ~9 MB parsed
  before the first paint) are separate chunks loaded on first use. `baseCatalog` serves English
  until the language arrives, then bumps `$appLocaleVersion`, which re-resolves every
  translator. A non-English user sees English for the moment the chunk takes to load.
  Hermes's tests that read `TRANSLATIONS.<lang>` synchronously (`catalog-completeness`, parts of
  `context`, `runtime`, `languages`, `registry`) fail by design; the behavior they check holds
  once the chunk is loaded (verified with a `waitFor` variant, 2026-11-04).

- **0004-unbundled-plugins.patch** (`contrib/plugins.ts`), performance: the bundled plugins this
  backend can't serve (hermes-bots, kanban, radio; ~550 KB at startup) are left out of the
  eager plugin glob, so they're neither loaded nor listed in Capabilities ▸ Plugins. A new
  bundled plugin is picked up as usual; add it to the exclusion if its backend is missing.

- **0005-skills-only-capabilities.patch** (`app/capabilities/index.tsx`): Capabilities shows only
  the Skills and Tools tabs; this backend has no agent plugins or MCP connectors (other `?tab=`
  values fall back to Skills), and the page no longer downloads the plugin catalog. The skills
  routes are `src/skills.rs` (the hub's are answered empty there, not ported); the toolsets' are
  `src/toolsets.rs`.

- **0006-skills-open-on-installed.patch** (`app/capabilities/catalog/`: `use-catalog-filters.ts`,
  `catalog-browser.tsx`, `catalog-data.ts`): Skills opens with the Installed filter on (the hub
  can't install yet), and the public catalog (~100k rows) is downloaded only once the filter is
  off or a search is typed. Hermes's `catalog-browser.test.tsx` cases that expect the full
  catalog on open fail by design.

- **0007-window-controls.patch** (`lib/browser-connection.ts`, `app/shell/wslg-window-controls.tsx`,
  `components/pane-shell/geometry.ts`, `pane-shell/tree/renderer/{tree-group,narrow-overlays}.tsx`,
  `app/overlays/overlay-view.tsx`, `global.d.ts`): the window is frameless (Hermes's own title bar,
  in the theme's colors, as in its Electron app), so the page paints the min/max/close it already
  has for WSLg (`WslgWindowControls`), reserves their 138px on the right of the title bar, and
  floating panes dodge them.
  - The buttons call `window.hermesRsWindow`, which the server injects (`server.rs`,
    `POST /api/hermes-rs/window`). It comes before `window.hermesDesktop?.windowControls`: the
    webapp defines `window.hermesDesktop` itself, a shim whose window controls only throw.
  - Drag regions: WebView2 resolves them above the DOM, a later one overriding an earlier one. The
    panes' drag strip over the controls rectangle is left out (in Electron it sits under the native
    buttons; here it took the page's buttons' clicks), and the overlays' full-width band (Settings)
    gets a no-drag corner over the buttons as its last child, so the band and its close button keep
    their place.
  - Styled as Hermes's Windows overlay: transparent on the bar, the theme's foreground and hover
    tint.
  - Dragging and double-click maximize are WebView2's own handling of the page's `app-region: drag`
    areas. Checked 2026-11-04 with real mouse clicks in the window: minimize, maximize, restore,
    also with Settings open; maximized, the bar isn't clipped.
  - Known gap: the maximize glyph doesn't switch to "restore" (it reads Electron's `isMaximized`).

- **0008-save-dialog.patch** (`app/session/hooks/use-prompt-actions/slash-desktop-commands.ts`):
  `/save [json|md|html] [name] [redact]` hands the exported conversation to
  `window.hermesRsSaveTextFileAs` when the webapp's bridge has no `saveTextFileAs` (it has none
  in browser mode): the server opens Windows' Save As dialog and writes the file
  (`POST /api/hermes-rs/save-file`; the content comes from `session.export`, `src/export.rs`).

- **0009-providers-open-on-custom-endpoints.patch** (`app/settings/index.tsx`): Settings ▸
  Providers opens on Custom endpoints instead of Accounts. hermes-rs's providers are
  OpenAI-compatible endpoints (`src/endpoints.rs`); the Accounts (sign-in) and API keys (Hermes's
  provider catalog) sub-pages are hidden by `hide.css`.
- **0010-native-zoom.patch** (`lib/browser-zoom.ts`): the text size (Ctrl +/−, Appearance) zooms
  the window natively (`window.hermesRsWindow.setZoom` → WebView2's zoom, as Electron's
  `webContents` zoom) instead of CSS `zoom` on the root, which misplaces every popover, menu and
  tooltip (they open over their trigger, or cropped at the window's edge).
- **0011-paste-files.patch** (`app/chat/composer/index.tsx`): files copied in Explorer and pasted
  into the composer attach as a drop does (Hermes's paste takes images and text only).
- **0012-new-endpoint-button.patch** (`app/settings/custom-endpoints-settings.tsx`): a "New
  endpoint" button on the endpoint list. Hermes's form opens on the default endpoint and offers
  "New endpoint" only beside it, so details typed in to add one could overwrite the endpoint open
  in the form.

Startup bundle (JavaScript reached before the first paint), Hermes `c419cf7c`: 9.3 MB in 145
files before 0003/0004, 6.6 MB in 109 after.

- **0013-open-file-links.patch** (`app/artifacts/index.tsx`, `lib/browser-desktop-bridge.ts`):
  Hermes's browser build can't open the server's files: the Artifacts gallery (which sees our
  connection as remote) downloads a copy, and `openExternal` drops `file:` links. Here the
  backend is on the user's machine, so both ask `POST /api/hermes-rs/open-file`
  (`open_file.rs`) first, which opens the file with its default app, or shows it selected in
  Explorer when it's a program or script (a link never runs one); without the route, or for a
  file that's gone, the gallery keeps Hermes's download. Tested live 2026-11-05
  (`scripts/perf/artifacts-check.mjs`).

- **0014-move-to-home.patch** (`app/chat/session-drag.ts`,
  `app/chat/sidebar/session-actions-menu.tsx`, `store/projects.ts`): a session can be moved to
  the sidebar's Home bucket — dragged onto its row, or the row's own "Move to project"
  submenu. Hermes excludes Home from both ("no folder to move into"); here moving there is a
  deviation the user asked for, and it targets the bare home workspace (the backend's default
  cwd), which is what Hermes's own tree files into Home ("no cwd, bare home, deleted
  workspace"). The store hunk fetches that cwd for the Home target; the session's own current
  bucket (the tree snapshot's owner map) stays a non-target. Hermes's `session-drag.test.ts`
  "is not a target for Home — there is no folder to move into" fails by design: it checks the
  exclusion this patch removes, which is the deviation the user asked for. Tested live
  2026-11-06 (`scripts/perf/workspace-check.mjs`: the move, the tree filing the session into
  Home).

- **0015-open-in-default-app.patch** (`app/right-sidebar/file-actions.tsx`,
  `store/file-actions.ts`, `i18n/types.ts`, every `i18n` locale, `locales/_keys.desktop.json`):
  "Open in default app" at the top of both file trees' right-click menu. The page sees our
  backend as remote (no Electron bridge, so `nativeHostFiles` is missing), which hides
  Reveal/Rename/Delete and offers only copy-path and Download; the action asks
  `POST /api/hermes-rs/open-file` (`open_file.rs`, the route patch 0013 added for the Artifacts
  gallery) with the row's plain absolute path — the backend opens the file with its default app,
  a folder in Explorer, and shows a program or script selected in Explorer instead (a menu never
  runs one). A backend without the route (a real remote Hermes) reports the failure as a toast.
  The review/git tree keeps Hermes's menu shape (its "Open file" opens the in-app preview).
  Tested live 2026-11-08 (`scripts/perf/file-tree-open-check.mjs`).

- **0016-reveal-in-explorer.patch** (`app/right-sidebar/file-actions.tsx`,
  `store/file-actions.ts`): "Reveal in File Explorer" beside "Open in default app" in the file
  trees' right-click menu. The label reuses Hermes's `fileMenu.revealExplorer` (translated in
  every locale); the action asks `POST /api/hermes-rs/reveal` (`open_file.rs` `reveal_plan`: the
  path shown selected in its containing folder, `explorer.exe /select`), which the Electron
  bridge's `revealPath` has no shim answer for, so Hermes's local-only Reveal item hides. The
  review/git tree keeps Hermes's menu shape. Tested live 2026-11-08
  (`scripts/perf/file-tree-open-check.mjs`).

- **0017-live-tool-output.patch** (`store/tool-progress.ts`,
  `components/assistant-ui/tool/fallback.tsx`,
  `app/session/hooks/use-message-stream/index.ts`,
  `app/session/hooks/use-message-stream/interrupted-status-sync.test.tsx`): the running terminal
  call's output is shown live. The backend relays each captured line (masked, the accumulated
  tail) as a `tool.progress` event while the command works (`shell.rs`); the page already shows
  the progress message beside the running row's timer (`ToolRunningMeta`), so this patch adds the
  clicked-open row's rendering — a "stdout — live" block under the command row that grows until
  the finished result's own stdout block replaces it — and `formatToolProgress` takes the
  multi-line tail's last line, so the one-line header ticker doesn't show the whole output. It
  also accepts the stopped call's real completion past the Stop latch: the backend answers a stop
  mid-command with the output it had printed (`interrupted: true` in its result), and that
  completion is production, not a late frame to drop — it may only patch the bubble that still
  owns the call, never seed a new one, so the partial output paints immediately instead of after
  the chat rehydrates. Hermes relays live progress for MCP tools only
  (`agent/tool_progress_relay.py`); extending it to the terminal is the feature. Tested live
  2026-11-08 (`scripts/perf/tool-stream-check.mjs`).

- **0018-tree-arrow-selection.patch** (`app/right-sidebar/files/tree.tsx`): the tree's arrow
  keys move the visible selection. react-arborist's own arrow handling moves only its focus
  cursor — a state nothing styles follows (`node.isSelected` paints the highlight), and its
  rows are `tabIndex: -1`, so the keys looked dead. A capture-phase handler on the wrapper
  (before arborist's own) walks the visible rows — placeholder rows skipped — and keeps the
  highlight on the cursor: ↑/↓ step through the rows clamped at the ends; → unfolds a closed
  folder and steps into an open one; ← folds an open folder and climbs from a closed one or a
  file. Shift-arrows stay arborist's multi-selection; F2/Enter keep the inline rename; Space
  keeps activate. The click's `node.focus()` keeps the cursor on the row a click selected or
  toggled, so the first arrow continues from it instead of jumping to the tree's start.
  Tested live 2026-11-09 (`scripts/perf/keyboard-nav-check.mjs`).

- **0019-overlay-nav-arrow-keys.patch** (`app/overlays/overlay-split-layout.tsx`): ↑/↓ walk
  the overlay nav rails (roving focus, wrap-around). The rows are plain buttons — Tab reached
  them, arrows did nothing — and every left nav (Settings, Command Center, the Skills facets)
  renders through this `OverlaySidebar`. The disclosure carets and the icon-only footer are
  skipped (a section row reveals its branch on select; footer buttons aren't nav rows), and
  rows hidden by `hide.css` via `offsetParent`. Arrows only move focus — Enter/Space still
  activate, the hash never changes, so a stray chord never navigates a surface the user
  didn't aim at. Tested live 2026-11-08 (`scripts/perf/keyboard-nav-check.mjs`).

- **0020-rename-returns-focus.patch** (`app/right-sidebar/file-actions.tsx`): exiting the
  inline rename with the keyboard returns DOM focus to the tree (VS Code's rename does).
  Without it the input unmounts with the row and focus drops on the body — no arrow key
  fired anywhere in the tree until the user clicked back in. A blur-origin finish (the user
  clicked elsewhere) leaves focus there. Tested live 2026-11-09.

## To do

- **Skill hub** (install from Capabilities ▸ Skills ▸ Discover): `tools/skills_hub*.py` and
  `tools/skills_guard.py` (~4,800 lines: official, GitHub, skills.sh, ClawHub, LobeHub and index
  sources, plus the install-time security scan). Until then install answers 501.

- **Smaller startup bundle, next candidates** (source-map attribution of what loads before the
  first paint): `@xterm/xterm` + `addon-webgl` (~430 KB, only the terminal needs them),
  `katex` (~235 KB, math), `@icons-pack/react-simple-icons` (~245 KB, brand icons),
  CodeMirror (~260 KB, the code editor). Each is pulled in by a static import chain from
  startup code; making it lazy means finding that chain.

- **Steering a queued card shows the message only after the reply.** `prompt.steer_queued`
  hands the text to the running turn and retires the card; nothing draws a bubble (Hermes
  upstream too). A first attempt (5ed51ba, reverted) made the card steer the composer's way
  (`steerQueuedNow`, then `prompt.cancel`) and left the Steer button dead in the app. Next try:
  draw the bubble from `prompt.steer_queued`'s success in `queue-slot.tsx`, and test it live.
