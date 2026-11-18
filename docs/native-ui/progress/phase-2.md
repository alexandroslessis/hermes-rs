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
