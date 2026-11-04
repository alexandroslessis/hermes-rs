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
  the Skills tab; this backend has no agent plugins, MCP connectors or switchable toolsets (other
  `?tab=` values fall back to Skills), and the page no longer downloads the plugin catalog. The
  skills routes are `src/skills.rs`; the hub's are answered empty there (not ported).

- **0006-skills-open-on-installed.patch** (`app/capabilities/catalog/`: `use-catalog-filters.ts`,
  `catalog-browser.tsx`, `catalog-data.ts`): Skills opens with the Installed filter on (the hub
  can't install yet), and the public catalog (~100k rows) is downloaded only once the filter is
  off or a search is typed. Hermes's `catalog-browser.test.tsx` cases that expect the full
  catalog on open fail by design.

- **0007-window-controls.patch** (`lib/browser-connection.ts`, `app/shell/wslg-window-controls.tsx`,
  `components/pane-shell/geometry.ts`, `global.d.ts`): the window is frameless (Hermes's own title
  bar, in the theme's colors, as in its Electron app), so the page paints the min/max/close it
  already has for WSLg (`WslgWindowControls`), reserves their 138px on the right of the title
  bar, and floating panes dodge them. The panes' and overlays' drag strips over the controls
  rectangle are left out (in Electron they sit under native buttons; here they would take the
  page's buttons' clicks: WebView2 resolves drag regions above the DOM). The buttons are styled
  as Hermes's Windows overlay: transparent on the bar, the theme's foreground and hover tint. The buttons call `window.hermesRsWindow`, which the server
  injects (`server.rs`, `POST /api/hermes-rs/window`); `window.hermesDesktop` stays undefined,
  since the page reads its presence as "Electron". Dragging and double-click maximize are
  WebView2's own handling of the page's `app-region: drag` areas. Known gap: the maximize glyph
  doesn't switch to "restore" (it reads Electron's `isMaximized`).

Startup bundle (JavaScript reached before the first paint), Hermes `c419cf7c`: 9.3 MB in 145
files before 0003/0004, 6.6 MB in 109 after.

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
