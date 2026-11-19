# Phase 2 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-n2   Branch: native/p2-design

## CP2.1 — Themes and tokens
Worktree: C:\Users\alexandros\repos\hermes-rs-n2   Branch: native/p2-design   Model: Sonnet 5.5 (subagent)
Cut from `main` at 329f7d7. Uncommitted. Resumed once after a rate-limit stop; steps 1-4 re-checked.

### Files
- New: `crates/native/src/design/{mod,tokens}.rs` (colour tokens as a `Palette`, light and dark from
  t3code's CSS, spacing, radii, control heights, font sizes); `src/theme/{mod,zed,kit,folder,fonts}.rs`;
  `src/gallery/tokens.rs` (the swatch story); `assets/themes/{one,ayu}/` (JSON + MIT licence);
  `assets/fonts/inter/` (4 TTF, `OFL.txt`, `README.txt`).
- Replaced: `src/themes.rs` and `src/zed_theme.rs` are gone; `ThemeChoice`/`apply_theme` live in
  `theme/mod.rs` (+ `T3Dark`, `T3Light`), the Zed mapping is typed serde structs in `theme/zed.rs`.
- Edited: `lib.rs`, `main.rs`, `bin/gallery.rs` (`--theme t3-dark|t3-light`), `spike.rs`,
  `gallery/{mod,view,button,snapshots}.rs` and `dev/dump.rs` (import path only), `Cargo.toml`
  (+`serde` derive, `tracing`, `dirs`; dev `hermes_testdir`), `Cargo.lock`, `vendor/SOURCES.md`,
  `docs/TESTING.md`, `docs/PITFALLS.md`, `docs/native-ui/README.md`.

### What it does
- **Tokens:** 56 colour names (58 after review) after t3code's variables; page scope plus the `sidebar_*` tokens from
  the `[data-app-sidebar]` scope. Alpha stays alpha. Tailwind oklch values and `color-mix()` results were evaluated
  with a script and written as hex with the derivation in comments.
- **Zed mapping:** `parse_family` reads a family file; `MAPPING` (token <- Zed key) over the
  appearance's default palette, so missing/null/broken keys fall back. Whole `style` goes to Kit's
  `highlight` (with the `editor_*` workaround kept and tested).
- **Kit:** `KIT_COLORS` table, `kit::config`, `theme::apply` (also fonts, text sizes, code colours).
  Views read `cx.palette()` (our tokens, `DesignTheme`) or Kit's theme.
- **User folder:** `<data>/themes/*.json` (data = `HERMES_RS_HOME` else `%APPDATA%\hermes-rs`),
  scanned by `Themes::load`, polled once a second by `watch_user_themes` (size+mtime fingerprint);
  bad files give a `Problem` and a `tracing::warn`. The folder is never created. Not wired into a
  running window yet (CP2.2's workspace calls `Themes::load` and `watch_user_themes`).

### Font decision
**Inter bundled (OFL, static Regular/Bold/Italic/BoldItalic, 1.4 MB) for the interface; Consolas
(system) for code, then Cascadia Mono, Lucida Console.** Reason: t3code uses Inter and its
screenshots are Inter; in a side-by-side render at 14 px (Inter, Segoe UI Variable Text, Segoe UI,
IBM Plex Sans; Consolas, Lilex, JetBrains Mono NF, Cascadia Mono; temporary story, deleted) Segoe
is narrower and draws weight 500 as SemiBold, Inter 500 as Regular and 600 as Bold. t3code's mono
stack resolves to Consolas on Windows. Fallbacks: Segoe UI Variable Text, Segoe UI. Limitation:
no real 500/600 (needs Inter Variable; not tried in GPUI). Record is in `theme/fonts.rs`.
The Inter files were copied from t3code's installed client bundle (read-only), version 4.001.

### Tests (all run, `CARGO_TARGET_DIR=target/claude`)
- `cargo test -p hermes-native`: 44 passed. With `--features snapshots`: 47 passed.
  New: tokens (hex round-trip, alpha compositing, CSS values, light/dark parity of every token,
  contrast, 4 px grid and control heights), Zed (a real theme maps; both bundled families parse;
  mapping names real tokens and keys; missing/null/broken keys fall back; broken files are errors),
  Kit (every key we write is kept by Kit; config carries tokens, fonts, highlight), folder (missing,
  order, malformed skipped with reason, unreadable bytes, change detection add/edit/break/remove,
  all with `hermes_testdir::tempdir()`, `TestDir` last), theme (list order and override, every bundled
  theme builds, apply sets both token sets, highlight not inherited, watcher end to end under a
  GPUI test clock), fonts (resolution, fallbacks, bundled files are TrueType), story layout test
  (every token has a chip with its colour, heights 20/24/28/32/36).
- `scripts/native/check.ps1`: "native check: all ok". `cargo clippy -p hermes-native --all-targets
  --features snapshots`: clean. `cargo fmt -p hermes-native`: applied. `cargo build --workspace`: passes
  (copied `target/hermes-src/apps/desktop/dist-webapp` from the main checkout, as in CP0.1).

### Token vs screenshot (`app-thread-dark.png`, pixels sampled)
Match: canvas #000000; border #141414 (white 8% over black); user bubble #1e1e1e (`message_surface`);
muted text #a1a1a1; success #00d492; selected sidebar row ~#1f1f1f (row_active 11% over the sidebar).
Differs:
- Sidebar background #030304 (dithered ~#030303), CSS #000000.
- Composer #0e0e0e and right panel #0f0f0f, CSS `card` #111111 (looks like the glass utilities,
  popover at ~80-84% over black; the token keeps the CSS value).
- Panel edge #252525 and its divider #1b1b1b: border over a card, higher than a plain 8% (not a token).
- Sidebar row metadata #8c8c8c vs `sidebar_secondary_label` #8f8f8f (antialiasing).
- Text max pixel #dcdcdc vs foreground #f5f5f5: thin glyphs never reach full colour, inconclusive.
Not observable in the dark set: `primary`, `ring`, error, warning, info (CSS values used). Light theme
by CSS only; no light screenshot taken and t3code was not run. No new reference screenshots, so no
README row.

### Snapshots (new baselines, my own story; the button baselines are unchanged)
- `crates/native/snapshots/tokens-swatches-light.png` (3620x1252)
- `crates/native/snapshots/tokens-swatches-dark.png` (3620x1252)
Each holds the two states (light palette left, dark right); the two files differ only in the gallery
theme behind the story, which the story paints over. I looked at the dark state once, downscaled:
four columns of chips with names, five control heights, four radii, a specimen in Inter and Consolas,
nothing clipped. The light state was not looked at: the visual check should.

### Deviations
- Module paths: `theme/` (not only `theme/zed.rs`) holds the list, Kit mapping, folder and fonts;
  `themes.rs`/`zed_theme.rs` are removed rather than kept.
- Gruvbox not bundled (placeholder licence); One and Ayu are.
- File watching is polling, not a watcher crate.
- t3code's "Default Light/Dark" are named that, and the Kit-default choices are now labelled
  "GPUI Kit light/dark".

### For the reviewer
1. Fonts: is bundling Inter (1.4 MB, static, no 500/600) right, versus Segoe UI Variable? Inter Variable would be exact but needs a woff2 to TTF conversion and a GPUI variable-axis test.
2. Sidebar and card dark values follow the CSS but the screenshots are #030304 / #0e0e0e; should the tokens follow the pixels?
3. `sidebar_*` take the `[data-app-sidebar]` scope; other tokens the page scope. Confirm that is the right split for CP2.2/Phase 3.
4. `kit::KIT_COLORS` choices (a Kit button is t3code's outline variant; links use `info_foreground`; selection is primary at 30%) are judgement calls, never seen on screen yet.
5. `theme::watch_user_themes` calls `cx.update` from a detached task; fine while the app lives, untested at quit.
6. The swatch snapshots hold both palettes in each file, so the "-light/-dark" suffix is meaningless for this story.
7. Light theme and `primary`/`ring` are unverified against pixels.
8. Not done by design: choosing light/dark by system, persisting the theme choice (CP2.2 settings).

### Visual check (Sonnet 5.5)
Cropped the right (dark) and left (light) halves of both baselines; compared the dark chips with `app-thread-dark.png` (the other two t3code dark images not opened; the thread image shows the same surfaces).
- tokens-swatches-dark.png (dark palette): matches — background/sidebar/terminal_background near black like the canvas, card/popover/border/input/message_surface dark greys as in the thread (user bubble, composer, right panel), muted_foreground mid grey, success green, primary/ring blue. Not observable in the screenshots: primary, ring, info, warning, error.
- tokens-swatches-dark.png (light palette half): unexpected: none; light tokens plausible (dark foreground on near-white, muted grey readable).
- tokens-swatches-light.png (light palette): matches the CSS-only light theme (no light reference screenshot exists); readable.
- tokens-swatches-light.png (dark palette half): matches, same as the dark file.
- tokens-swatches-dark.png layout: expected — all 56 tokens present in 4 columns with names, five control heights, four radii, Inter and Consolas specimens; nothing clipped or overlapping.
- tokens-swatches-light.png layout: expected — same as above.
- Note: the two files differ only in the thin backdrop around the story panel (white vs #0a0a0a), as the progress note says.

### Review (Opus 5.5)
Main had not moved (329f7d7); no merge. `/code-review high` run on the diff plus my own pass.

**Token conversion: correct.** Tailwind's oklch values read from t3code's installed bundle
(`server.asar`) and converted by an independent oklch -> OKLab -> linear sRGB -> sRGB script
(clipped to gamut, as Chromium draws them): all 22 Tailwind colours used (zinc 25-900, neutral
100/400/950, red/blue/emerald/amber 400/500/700) and both `--primary` values match the hex in
`tokens.rs`; `color-mix()` results (dark card #111111, message surface #1e1e1e, error #fb414a,
light message surface #ebebed, code backgrounds) and every alpha (3/4/8/11/36/45%, row tints
8/11/7%, surfaces 8/16%, terminal 20/25%) re-derived and match. One rounding slip: light
`message_action_hover` was mixed from rounded hex (#325fdc); exact is #3160db, fixed. Every
token exists in both themes (struct literals; test kept).

**Sidebar scope: the split is right.** `[data-app-sidebar]` sits on the left sidebar's root
(`AppSidebarLayout.tsx`), and `:root, [data-app-sidebar]` recompute `--contrast-*` there, so the
dark row tints are #f1f3f7 at 8/11/7% as written. Gaps fixed: the scope also redeclares `--input`
(white 18% dark), now `sidebar_input`; `sidebar_row_active` is the *open* thread's row and
`sidebar_row_selected` multi-selection (`SidebarThreadRow.tsx`), the doc comments had them the
other way, and Kit's `list.active.background` now takes `sidebar_row_active`.

**Screenshot differences.** Sidebar #030304: t3code's `--surface-grain` (3.5% fractal noise on
the body); `app-empty-dark.png` and `sidebar.png` show the same 02-04 noise, the main canvas is
exact #000. Not a token; the grain is CP2.2/Phase 3's call. Composer #0e0e0e: a real token gap,
`--surface-raised` (#111) drawn as glass at `--glass-opacity` 80% (`ComposerSurface.tsx`); added
`surface_raised` and `GLASS_OPACITY`, with a test deriving #0e0e0e. Right panel #0f0f0f and the
#252525 panel edge: no CSS source found in `RightPanelTabs.tsx`; left for CP2.2's panel (#0f0f0f
equals `code_background`, card 90% over black, possibly the same mix). Spot check of
`app-empty-dark.png` and `sidebar.png`: canvas #000000, composer #0e0e0e, open row ~#1e1e1e
(= `sidebar_row_active` over the grain), consistent.

**Other fixes.** Licence: One's `LICENSE` is GitHub Inc.'s (Atom), not Zed Industries';
`SOURCES.md` and the `BUNDLED` comment corrected (files and licences otherwise real; Gruvbox
exclusion sound, Zed's file has placeholders). Zed files: one theme with an unknown appearance
or no name no longer drops its whole family (skipped, warned; the file errors only when nothing
is usable); a UTF-8 BOM is ignored; files over 4 MB are a Problem, not read. A highlight Kit's
strict deserializer rejects is dropped with a warning instead of making the theme unappliable.
The watcher's task is now held in a global (dropped at quit, a second call replaces it), tested
by dropping it. `apply_kit_default` restores Kit's own fonts, sizes, radii and code colours
(they leaked from the previous t3/Zed theme). `apply_theme` reads the installed list (user
overrides win); bundled families parse once; `Fonts::install` caches its answer (the story
listed every system font per render). Logs reached no subscriber: `log_to_stderr` in both
binaries. Kit radii come from `radius::LG`/`XL` (comment said `lg` = 12, it is t3code's `xl`).
README now says the folder is not wired into a window until CP2.2.

**Checked, fine.** Tests use `hermes_testdir`, `TestDir` last; no Zed code (theme JSON is data);
t3code cited in headers; crates/app, crates/desktop untouched; Inter 4.001 confirmed in the
files, OFL present, font load failure falls back.

**Left.** The spike still starts in GPUI Kit's dark (throwaway, CP2.2 replaces it). Snapshots
still render under Kit's themes, so the tokens story is stored twice and no baseline shows Kit
components under t3code's theme: CP2.2 should snapshot its design components with T3Light/T3Dark.
Zed files with comments or trailing commas are refused (strict JSON). No light screenshot.

Tests: `cargo test -p hermes-native` 49 passed; `--features snapshots` 52 passed after accepting
the two token baselines (two new chips, the hover fix; both halves looked at); button baselines
unchanged; `check.ps1` all ok; clippy clean; `cargo build --workspace` passes.

## CP2.2 — Design layer and frame
Worktree: C:\Users\alexandros\repos\hermes-rs-n2   Branch: native/p2-design   Model: Sonnet 5.5 (subagent)
On top of CP2.1 (fcb29f5). Uncommitted.

### Files
- New, `crates/native/src/design/`: `button.rs` (Button, IconButton; 5 variants, 5 sizes, on Kit's unstyled `base::Button`), `list_row.rs` (ListRow, ThreadRow with status dot and project monogram), `section_header.rs`, `panel.rs` (card and edged pane, the `dropdown-glass` surface), `badge.rs`, `kbd.rs` (Kbd, KbdGroup, `chord_label`), `tooltip.rs`, `toast.rs` (the `Toasts` model with an injected clock, and the `Toast` view), `empty_state.rs`, `middle_truncate.rs` (port of `splitForMiddleTruncate`), `icons.rs` (`AppAssets`: Kit's 101 icons plus 11 compiled in).
- New, the frame: `ui_state.rs` (`native-ui.json`), `titlebar.rs`, `workspace.rs` (+ `workspace_tests.rs`), `keymap.rs`, `palette.rs`, `app.rs`. `main.rs` opens the app; the CP0.1 spike is `--spike`.
- New stories: `gallery/design.rs`, `gallery/frame.rs`. `fake.rs` gained palette threads, projects and notices.
- Edited: `tokens.rs` (+ `panel_surface`, `panel_edge`, `PANEL_ALPHA`), `theme/zed.rs` (derives them), `gallery/{mod,snapshots,tokens,button}.rs` (`Story::themes`, `Themes::{Kit,T3}`; the token story has 16 rows per column since it now has 60 tokens), `testing.rs` and `snapshots.rs` (`AppAssets`), `lib.rs`, docs (README, TESTING, PITFALLS, ARCHITECTURE, AGENTS.md, reference README).

### What it does
- **Design layer.** Every component takes colours from `cx.design_theme().palette`, is a story with all states, and names its t3code source and reference screenshot in its header. Button variants default, outline, ghost, ghost-muted, destructive; sizes micro 20, xs 24, sm 28, default 32, lg 36 (heights asserted); 1 px border always, disabled at 64%, selected = the hover fill, focus ring inset 2 px. Not copied: press scale, inset highlight shadows.
- **CP2.1 leftovers.** The right panel's #0f0f0f and its #252525 edge are t3code's `dropdown-glass` (popover at 18% mixed with popover at 80% = 83.6% alpha, edge = foreground at 10%): tokens `panel_surface` / `panel_edge`, with a test that derives #0e0e0e and #252525. Components are now snapshotted under the t3code themes (`Story::themes`).
- **Window frame.** No system title bar (`appears_transparent`); `titlebar.rs` draws the 40 px bar: drag zone, panel toggles, caption buttons 46 px wide with `window_control_area(Min/Max/Close)`, so GPUI's non-client hit test answers `HTMINBUTTON/HTMAXBUTTON/HTCLOSE` (that is what makes the Windows 11 snap layouts flyout appear over maximise) and `HTCAPTION` over the drag zone (move, edge snap, double-click maximise by the OS). Minimum size 640x480 (`window_min_size`). Position, size and maximised state are written to `native-ui.json` as the window changes (debounced 500 ms, and at close) and restored by `restore_window`, which centres the window if its saved spot is no longer on any display. DPI: GPUI handles `WM_DPICHANGED` itself and the UI is in logical pixels; nothing to add.
- **Workspace.** Sidebar (256, 208 to 640, collapsible: its toggle moves to the title bar), main (min 320 wide), right panel (closed by default, 420, 280 to 720), bottom drawer (closed, 280, 120 to 640), all in `native-ui.json`. `frame_layout` is a pure function (the right panel hides itself when even the sidebar's minimum cannot make room; the sidebar yields first). Borders are our own 6 px handles with `on_drag`/`on_drag_move`; each moves only its own size; double-click resets. Ground rule 6 respected (side file only).
- **Keymap and palette.** `keymap.rs` is one table (id, title, GPUI keys, action): Ctrl+K, Ctrl+Shift+P, Ctrl+N, Ctrl+B, Ctrl+Alt+B, Ctrl+J, Ctrl+Alt+Shift+A, F6 and Shift+F6, Ctrl+Comma; t3code's defaults with mod = Ctrl. Tests check parse, uniqueness and no Windows-reserved key. The palette (`Ctrl+K` all, `Ctrl+Shift+P` actions only) matches with our own fuzzy scorer (word starts, runs, gaps; each word must match title or subtitle), groups Actions, Threads, Projects, bolds matched characters; Up/Down/Ctrl+N/Ctrl+P/Enter/Esc.
- **Toasts.** `Toasts` model (clock passed in): newest 3 visible, newest at the bottom, default 5 s, errors 10 s, loading stays, hover on the stack pauses all timers. Drawn bottom right (32 px inset, 360 wide, 12 gap) by the workspace, which re-arms one timer task. Source: `ToastSpec`, fed by `fake::notices()` and the main area's "Show a notice" button; Phase 3 maps `CoreEvent`s to it (no `hermes_core` dependency yet).
- **User themes folder** is wired: `app.rs` loads `<data>/themes` and watches it; the theme follows the system until one is picked.

### Tests (all with `CARGO_TARGET_DIR=target/claude`)
- `cargo test -p hermes-native`: 119 passed. With `--features snapshots`: 122 passed. New: tokens (panel colours), icons, button/badge/kbd/toast/middle-truncate/list-row logic, `ui_state` (round trip, missing, malformed in five ways, one bad part, clamping, BOM; `hermes_testdir::tempdir()`), keymap (5), palette matcher (7), toast model (6), design story layouts (every state of 20 stories under both t3code themes), frame: sizing rules at 3 sizes x 5 combinations x 4 stored sizes, `restore_window`, rendered frame at 640x480, 1100x700 and 1920x1080 (closed and with right panel and drawer open), collapse by Ctrl+B at the three sizes, shortcuts, the CP0.1 border-drag test for sidebar, right panel and drawer at two sizes, reset, persistence round trip into a fresh window, palette open/filter/move/run/close, thread result to toast, toast stacking position, dismiss, hover hold and expiry, theme cycle.
- `scripts/native/check.ps1`: "native check: all ok". `cargo clippy -p hermes-native --all-targets` (with and without `--features snapshots`): clean. `cargo fmt -- --check`: clean. `cargo build --workspace`: passes. `hermes-native.exe --quit-after 6 --dump` against `target/native-test`: opens, dumps the frame, writes `native-ui.json` (window position recorded).

### Snapshots
New (42), all in `crates/native/snapshots/`, each as `-t3-light.png` and `-t3-dark.png`: `button-default`, `button-outline`, `button-ghost`, `button-ghost-muted`, `button-destructive`, `button-sizes`, `icon-button`, `list-row`, `thread-row`, `thread-row-status`, `section-header`, `panel`, `badge`, `kbd`, `tooltip`, `toast`, `empty-state`, `middle-truncate`, `title-bar`, `palette`, `app-frame`.
Pre-existing that changed: `tokens-swatches-light.png` and `tokens-swatches-dark.png` (two more chips, `panel_surface` and `panel_edge`; 16 rows per column); diffs: `target/claude/snapshots/tokens-swatches-light.diff.png`, `target/claude/snapshots/tokens-swatches-dark.diff.png` (made against the old baselines). `button-states-*` unchanged. I looked once at `thread-row-t3-dark`, `button-outline-t3-dark`, `toast-t3-dark`, `app-frame-t3-dark`, `palette-t3-dark` and `title-bar-t3-dark`: nothing clipped; the thread rows resemble `button-session-variants-selected.png` (monogram tile, bold title, selected fill). The light baselines were not looked at: the visual check should.

### Screenshots added
None. t3code was not running and my tools cannot drive its window (no input automation), so the light theme, tooltip, badge, kbd, toast, palette and empty states have no reference of their own; they follow the CSS and source named in each header. The reference README rows that were wrong were corrected.

### What the window frame can't do (or can't prove) on Windows
- The snap flyout, caption drag, edge snap and double-click maximise are done by Windows from the hit-test answers; none can run in the headless test platform (no non-client messages). Layout and the control areas are tested, behaviour is on the user's checklist.
- A drag zone cannot contain buttons (GPUI returns the first control area under the pointer), so the toggles are siblings of the drag zone; the title is the only draggable content.
- Caption button hover colours rely on GPUI feeding non-client mouse messages into its hover tracking (Kit's own title bar relies on the same); not verified by eye.
- DPI changes and multi-monitor restore are GPUI's and unexercised here. Window position uses the logical pixels GPUI reports; mixed-DPI monitors may restore a few pixels off.
- No window shadow or rounded corners are drawn by us for the frameless window; Windows 11 adds its own.

### Deviations
- Own fuzzy matcher instead of `nucleo-matcher` (not in `Cargo.lock`, no need).
- Own resize handles instead of Kit's `h_resizable` (see PITFALLS).
- Tooltips are GPUI's `.tooltip(..)` with our bubble, not Kit's (the app window does use Kit's `Root`).
- Shortcut set is the subset whose actions exist; later phases add the rest to the table.
- Middle minimum 320 (the task's number); t3code's is 640.
- Fake sidebar rows and an empty state in the main area are placeholders for Phases 3 and 4.

### For the reviewer
1. Caption buttons: please confirm by hand (checklist 5 to 7) that hover, press, the snap flyout and double-click maximise work; if hover colours do not follow the pointer, the fix is likely GPUI's non-client hover, not ours.
2. `Button` wraps `gpui_kit::base::Button`, not Kit's styled `Button` (its variants, sizes and border logic could not express t3code's). An acceptable reading of "wraps Kit's component"?
3. `frame_layout` hides the right panel below about 820 px and squeezes the sidebar to make it fit; t3code uses a sheet. Fine for now?
4. `Workspace::window_moved` sanitises and schedules a save on every bounds change: at most one write per 500 ms while dragging.
5. The theme choice is stored by name; a theme file removed from `<data>/themes` falls back to the system default at the next start.
6. Palette selection is the `selected` index; mouse hover only tints (does not move it).
7. The light theme components are checked by layout tests and not yet by eye.
8. `ToastSpec` is not mapped from any real core event yet.
9. `Palette` story and `app-frame` story create new entities on every render (fine for snapshots, not a pattern for live views).

### User checklist (Phase 2)
Build and run (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n2`):
`$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`, then
`$env:HERMES_RS_HOME="$PWD\target\native-test"; .\target\claude\debug\hermes-native.exe`
(your real data is not touched; delete `target\native-test\native-ui.json` to start fresh).
1. A frameless window opens: a 40 px title bar across the top, a sidebar with "Hermes", Search, five fake thread rows, the main area with "What should we build in ...?", no right panel and no drawer.
2. Press `Ctrl+B`: the sidebar collapses, its toggle and "Hermes" appear at the left of the title bar; again: it comes back.
3. Drag the sidebar's right edge (below the 40 px top row, where the cursor becomes a resize arrow) left and right: it stops at 208 px and at 640 px (or where the middle area would go under 320 px). Double-click the edge: back to 256. Nothing else changes size.
4. Click the right-panel toggle in the title bar (or `Ctrl+Alt+B`): a panel opens at the right; drag its left edge: it stops at 280 and 720. `Ctrl+J` opens a drawer under the main area; drag its top edge. Shrink the window to the smallest: the right panel hides itself and the middle keeps 320 px.
5. Hover the three caption buttons: minimise and maximise get a grey fill, close turns red. Click maximise: the window maximises and the icon becomes "restore".
6. Rest the pointer on the maximise button for a second: the Windows 11 snap layouts flyout appears. Pick a layout.
7. Drag the empty part of the title bar to move the window; drag it to the top edge (maximise) and to a side (snap); double-click it to maximise and restore.
8. Move and resize the window, close it, start it again: same place and size (maximised if it was). See `target\native-test\native-ui.json`.
9. Press `Ctrl+K`: the palette opens at the top centre. Type `tog`: "Toggle sidebar", "Toggle right panel" and "Toggle terminal drawer" with the matching letters bold; Down/Up move, Enter runs, Esc closes; a click outside closes. Type `ayo`: a thread result; Enter shows a toast.
10. `Ctrl+Shift+P`: the palette lists only actions. Both open and close from the keyboard alone.
11. Click "Show a notice" three or four times: toasts stack at the bottom right, the newest lowest, at most three; they disappear after 5 s (errors after 10 s); the pointer over them holds them; the x dismisses.
12. `Ctrl+Alt+Shift+A` cycles the theme (Default Light, Default Dark, One Dark, ...). Restart: the last one is kept. Put a Zed theme file into `target\native-test\themes\` while it runs: it joins the cycle within a second or two.
13. Windows light/dark: close the app, set `"theme": null` in `target\native-test\native-ui.json` (a running app would write its theme back), start it, switch Windows' app mode (Settings > Personalisation > Colours): the app follows.
14. Run `.\target\claude\debug\hermes-native-gallery.exe`: the new stories are listed; look at Button (hover, press, Tab for the focus ring: a blue inset ring for keyboard focus only, none after a click), Thread row (status icons: dashed circle "Working", check circle "Done"), Toast, Palette, in light and dark. In the app, click a title-bar toggle: no ring stays on it.
15. `hermes-native.exe --spike` still opens CP0.1's window.

### Visual check (Sonnet 5.5)
Method: dark and light baselines of all 21 stories viewed (stacked sheets, 2x images); `target/claude/snapshots/*.diff.png` were not present, so tokens-swatches was judged directly.

References (t3code, dark):
- thread-row-t3-dark: differs — the title is bold on every row (reference: bold only on the selected row, regular otherwise); no branch icon at the right of the branch line; monogram colour is one green for every project (reference tints per project: structure purple, engine green, hermes-rs blue). Spacing, tile size, selected fill and radius match.
- thread-row-status-t3-dark: differs — status shown as a coloured dot + word; reference "Done" is a check-circle icon + word. Same title-weight and monogram notes as above. Colours legible.
- list-row-t3-dark: matches — fill and radius of the hover/selected row agree with the sidebar rows; no direct reference for the "New thread" row.
- icon-button-t3-dark: matches — ghost icon buttons, hover fill, focus ring, disabled agree with the topbar toggles/search-row icons.
- title-bar-t3-dark: differs — a 1 px divider runs under the title bar; the reference topbar (app-empty-dark, app-terminal-open) has none. Caption buttons (46 wide, red close hover) and panel toggles match.
- tooltip-t3-dark: matches — dark rounded bubble, same text weight as "Toggle terminal drawer (Ctrl+J)".
- app-frame-t3-dark: differs — (1) the divider under the title bar, as above; (2) the right panel is an edge-to-edge pane with a header, the reference's is a floating rounded glass card at the top right; (3) the drawer has a "Terminal" header and divider, the reference drawer is bare; (4) sidebar search row lacks the filter and add-folder icons. All four are placeholders for later phases; sidebar width, divider, row rhythm and collapsed toggle position agree.
- button-default/outline/ghost/ghost-muted/destructive/sizes-t3-dark: matches — 1 px border, radius, fill ladder per state, inset focus ring, 64% disabled.
- panel-t3-dark: matches — surface and edge (#0e0e0e/#252525 glass) agree with the thread-details card.
- tokens-swatches-dark: matches — two new chips panel_surface and panel_edge present, 16 rows per column, nothing clipped; (light also fine).
- section-header-t3-dark: expected (no reference except sidebar "Settled 7 >": chevron is at the left here, at the right in t3code: differs, minor).

No reference, judged: badge, kbd, toast, palette, empty-state, middle-truncate (the "…" is followed by a double gap before the tail: slightly odd, minor), button-states: expected — states distinct, text legible, no clipping or overlap, dark and light.
All 21 light baselines: expected — readable; warning/success/info text has enough contrast on its tinted fills; borders visible; ghost hover fills are faint but distinct.

### Review (Opus 5.5)
Main had not moved (329f7d7 under fcb29f5); no merge. Own pass over the whole diff (no `/code-review` run).

**Verdict: done criteria met.** Every listed component is a story with its states under both t3code
themes (Button 5 variants x 5 states + sizes, IconButton, ListRow, ThreadRow, SectionHeader, Panel,
Badge, Kbd, Tooltip, Toast, EmptyState, MiddleTruncate); custom title bar with `window_control_area`
caption buttons (checked against `gpui-pre-windows` `handle_hit_test_msg`: Min/Max/Close give
`HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE`, non-client moves are fed to GPUI as `MouseMove`, so the hover
colours do follow the pointer); sidebar/right panel/drawer with own handles and `native-ui.json`;
keymap + palette (`Ctrl+K`, `Ctrl+Shift+P`); toasts bottom right; frame tests at 640x480, 1100x700,
1920x1080; `hermes-native.exe` opens the frame, `--spike` the CP0.1 window; the themes folder is
wired (`app.rs`). Kit's `base::Button` keeps Enter/Space activation, Tab order, disabled and the
accessible name (its own tests; our focus handle goes in through its `track_focus`), so wrapping the
unstyled base is acceptable. The right panel hiding below ~820 px is fine until Phase 6's sheet.

**Fixed (bugs).**
- Focus ring drawn on any focus, so every clicked button (the title-bar toggles) kept a blue ring:
  now `focus-visible` (`window.last_input_was_keyboard()`), as t3code. The gallery's
  `Interaction::Focus` sends a bare key first so the "focused" state still shows the ring; the
  "pressed" state no longer does (baselines checked).
- Toasts: dismissing the last toast with the pointer over it left the stack "held" forever (the
  stack vanishes, no hover-out arrives), so later toasts never expired. `dismiss` ends the hold when
  the stack empties; test added.
- `native-ui.json` was written on the UI thread from the debounce task: now `background_spawn`
  (ground rule 7). New test proves the debounced write happens on its own (not before 500 ms, after it).
- Sidebar resize handle overlapped the sidebar header's drag zone in the top 40 px (Windows answers
  `HTCAPTION` there, so the resize cursor showed but the press moved the window): starts below it.
- `restore_window` clamps the saved size to the screen the window lands on (a size saved on a bigger
  monitor could open taller than the screen).
- Palette backdrop was a raw `hsla(0,0,0,0.4)`: now t3code's `dialog-backdrop` (background at
  60%/64%). Palette lists at most `MAX_RESULTS` (200) rows (not virtual); tests for Greek and emoji
  titles and a 10,000-entry search.

**Visual check's differs.** Fixed: the 1 px line under the title bar (t3code's topbar has none);
section-header chevron moved to the right of the count (t3code's folder header); status shown as
t3code's icons (`TOP_STATUS_ICONS`: check circle Done, dashed circle Working, dot-dashed Waiting in
muted, alert Failed; `CircleDot`/`CircleDotDashed` added to `icons.rs`), word in medium weight.
Re-snapshotted and looked at: button x5, thread-row, thread-row-status, section-header, title-bar,
app-frame (both themes; only my changes differ).
Deferred, with reasons: *title weight* — t3code is `font-medium` on every unsettled row and
`font-normal` when settled (not "bold only when selected"); static Inter has no 500, so it draws
Bold; the lighter row in the reference is a settled thread (Phase 3 has settled state; Inter
Variable is CP2.1's open question). *Per-project monogram colours* — t3code's colour is per-project
data (`ProjectIconColor`, 18 Tailwind colours, user-picked): Phase 3 with the project model and its
tokens; the current tint is a name hash (the frame does show three tints). *"Branch icon"* — the
icon at the right of the reference's branch line is the model/provider icon (the `trailing` slot),
Phase 3; t3code's card has no branch glyph there. *Middle-truncate gap* — GPUI's text ellipsis
leaves a ragged gap; t3code clips the head and puts "…" on the tail only when the head overflows,
which needs measuring the head: Phase 3/4 when paths matter. *Right panel as a floating glass card,
drawer header, search-row filter/add-folder icons*: placeholders of Phases 6 and 3.

**Checked, fine.** t3code source and reference named in every new module header; no Zed code; colours
from tokens except the Windows close-button reds and white text on the destructive/close fills
(platform/t3code constants, documented); tests use `hermes_testdir`, `TestDir` last; `crates/app`,
`crates/desktop`, ACP untouched; keymap avoids Windows-owned keys (Alt+F4, Win, Ctrl+Alt+Del,
Ctrl+Shift+Esc). `ui_state` falls back per part and clamps; atomic tmp+rename.

**Left.** Ctrl+Alt+B is AltGr+B on layouts that use AltGr (not on Greek/US); the palette backdrop
over the title bar still moves the window on a press (control areas win under overlays, PITFALLS);
the debounced background write and the close-time `save_now` could in theory race on the tmp file
(only a warning, the close-time write normally lands last); toast actions are not wired (no core
events yet); window position uses `rcNormalPosition` (workspace coordinates), so a taskbar at the
top or left may shift the restored window by its height.

Checklist corrected (step 3 grab point, step 9 lists three toggles, step 13 must be done with the
app closed, step 14 focus ring). Docs: PITFALLS (control areas under overlays, focus-visible, fake
clock, the toast hold), AGENTS.md status line, Phase 2 ✅ in PHASES.md. GAPS.md and PORTING.md need
no row (they track the Tauri app and Hermes ports; this phase ports t3code, cited in module headers).

Tests: `cargo test -p hermes-native` 123 passed; `--features snapshots` 126 passed after accepting the
20 baselines above; `scripts/native/check.ps1` all ok; clippy (`--all-targets --features snapshots`)
clean; `cargo fmt` applied; `cargo build --workspace` passes; `hermes-native.exe --quit-after 5`
with `HERMES_RS_HOME=target\native-test` opened and wrote `native-ui.json`.
