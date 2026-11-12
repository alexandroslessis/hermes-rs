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

- **0021-daily-request-limit.patch** (`app/settings/custom-endpoints-settings.tsx`,
  `types/hermes.ts`, `app/shell/hooks/use-statusbar-items.tsx`): a "Daily request limit" field in
  the custom endpoint form, under the model and context window (English text, not an i18n key:
  a key would mean editing every locale file). It sends `daily_request_limit` (a number, thousands
  separators dropped, or null to clear) and reads it back from the endpoint list; `endpoints.rs`
  stores it in `config.json` and `Gateway::apply_settings` hands it to the request ledger. A page
  without the patch just doesn't send the key, which the backend leaves alone. The status bar's
  allowance gauge, when it is our request quota (`id: "requests"`), opens Settings ▸ Providers
  instead of the Billing tab, which `hide.css` hides. **Written 2026-11-12 and checked with `git apply` against the pinned
  source, but not built into a webapp or tried in the page** (a webapp build needs npm
  packages): run `build-hermes-webapp.ps1` and look at Settings ▸ Providers before relying on it.

- **0022-budget-profile-setting.patch** (`app/settings/constants.ts`, `app/settings/config-subpages.ts`):
  lists `budget.profile` under Settings ▸ Model ▸ Main, labelled "Budget profile". Hermes's Settings
  page shows only the config keys its `SECTIONS` list names, so this is what lets the backend's schema
  field (a select of requests / balanced / tokens with a description, `rest.rs` `config_schema`) appear.
  A save goes through the page's normal config save, which `Rest::save_config` filters (an unknown
  profile or a 0 cap is dropped) and `apply_settings` hands to the runtime. Without the patch the
  profile still works through `config.set` and the config file. **Written 2026-11-12 and checked with
  `git apply --check` against the pinned source, but not built into a webapp or tried in the page**
  (a webapp build needs npm packages): run `build-hermes-webapp.ps1` and look at Settings ▸ Model ▸
  Main before relying on it.

- **0023-local-aux-models-button.patch** (`api/models.ts`, `app/settings/model-settings.tsx`): a
  "Use a local model for background jobs" button in Settings ▸ Model ▸ Auxiliary, beside "Reset all
  to main". It calls `assignLocalAuxiliaryModels` (`POST /api/model/auxiliary/local`, a route of
  ours: Hermes has no such call, an older backend answers 404 and the page shows its usual load
  error), which `aux_models::use_local` answers: it finds a local server (a local provider in
  Settings, else Ollama on :11434 or LM Studio on :1234, added as a provider), picks a chat model
  and sets compression, approval, titles and review to it; a 400 with a plain `detail` when no
  server answers. The page then refreshes its list. English text, not an i18n key (a key would mean
  editing every locale file). **Written 2026-11-12 and checked with `git apply --check` against the
  pinned source, but not built into a webapp or tried in the page** (a webapp build needs npm
  packages): run `build-hermes-webapp.ps1` and look at Settings ▸ Model ▸ Auxiliary before relying
  on it. The backend route is checked live by `scripts/perf/local-aux-check.mjs`.

- **0022 and 0023 were never in any build before 2026-11-12 (Phase 6 review).** They are in
  `diff --git` format, which the build's `git apply` silently skipped inside the hermes-rs repo
  (docs/PITFALLS.md); the script now applies both formats and stops on a skipped file. Since then
  they apply in order and type-check with the rest of the page (`tsc -p . --noEmit` in the build
  workspace), but were not looked at in the page.

- **0024-staging-pending-component.patch** (new files `api/staging.ts`,
  `components/staging-pending.tsx`), **0025-memory-pending-section.patch**
  (`app/settings/memory/memory-layers.tsx`), **0026-skills-pending-section.patch**
  (`app/capabilities/catalog/skill-catalog.tsx`): "Waiting for your review" at the top of
  Settings ▸ Memory & Context ▸ Persistent memory (memory items) and of Capabilities ▸ Skills (skill
  items: new skills, patches, curator merges), decision D3: no new panel. Hermes's plugin API has no
  slot in either screen (`plugin.js` can only register into the areas Hermes defines, and
  injecting DOM into React-owned screens breaks on re-render), so the screens get a small component
  each. `StagingPending kind="memory"|"skill"` lists `GET /api/staging` (ours, `src/staging.rs`)
  filtered by kind: per item the summary, the reason ("this turn read a web page"), the sources, the
  whole diff in a scrollable box, and Apply / Reject (`POST /api/staging/<id>/apply|reject`).
  It renders nothing when the kind has no items, and also when the route is missing (an older
  backend). It lists again when the screen opens, on window focus and after every Apply / Reject;
  an applied skill refreshes the skills list.
  - Everything shown is attacker-influenced text and is rendered as React text only (diff lines in
    `<pre>` spans coloured by their first character; never `dangerouslySetInnerHTML`); hidden
    characters already arrive spelled out as `<U+XXXX>`; nothing is cut (the box scrolls).
  - A stale item (what it was written against has changed) shows "Out of date ..." and its error
    and offers Reject only; a pending item whose Apply failed keeps its error on the card (the
    server's sentence: threat scan, limit, name taken) and offers Apply again and Reject.
  - 0024 adds the files, 0025 / 0026 only mount it (0025 in `MemoryLayersSlot`, so the Command
    Center's memory summary is unchanged; 0026 in `SkillCatalog`'s `notice` fragment, so it sits
    under the filter bar and doesn't switch off the hub loader, which tests `!notice`). English
    text, not i18n keys.
  - **Old queues.** Hermes's `memory.write_approval` / `skills.write_approval` queues
    (`pending/memory/`, `skills/.pending/`) have no screen in Hermes's page and none was written
    for them; the page shows `staging/` only. The backend now routes the foreground `memory` and
    `skill_manage` calls of a session whose write approval is on into `staging/` too (reason
    "write approval is on"), so the Settings switch "Ask before the agent saves to memory" feeds the
    one list. Items already in the old queues stay where they are (`/memory` lists them).
  - Errors: the page's REST client throws `Error("409: {\"detail\": ...}")`, so the component
    JSON-parses the message and shows `detail` (the server's whole sentence; Hermes's
    `readableError` would cut anything over 180 characters).
  **Written 2026-11-12; built, type-checked (`tsc -p . --noEmit`, no error in the patched files) and
  driven in the page on the perf instance 2026-11-12 (Phase 6 review)** with
  `scripts/perf/staging-check.mjs --dom` and by hand: both sections render first, the hostile diff
  is inert text, stale = Reject only, Apply of a skill from the page creates it (toast, card gone,
  skill listed), a refused Apply shows the 409's sentence. Not looked at as a screenshot (layout by
  DOM boxes only); no i18n keys.

- **0027-review-mode-setting.patch** (`app/settings/constants.ts`): lists `budget.review_mode`
  after `budget.profile` in the Model section, labelled "Review mode" (Phase 7). As with 0022,
  Hermes's Settings page only shows keys its `SECTIONS` list names; the subpage already takes
  `budget.` (0022). The field is the backend schema's select (`rest.rs`: "", idle_once, nudges,
  agentic, with a description); "" is the profile's own mode (the default stored in the config), and
  an unknown value is dropped by `Rest::save_config`. Without the patch the mode still works through
  `config.set` and the config file. **Written 2026-11-13 and checked with `git apply --verbose`
  in order 0001..0027 against the pinned source (no skipped file), but not built into a webapp or
  tried in the page** (a webapp build needs the npm workspace): run `build-hermes-webapp.ps1` and
  look at Settings ▸ Model ▸ Main before relying on it.

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
