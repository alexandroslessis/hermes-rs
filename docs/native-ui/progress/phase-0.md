# Phase 0 progress

## CP0.1 — GPUI Kit spike
Worktree: C:\Users\alexandros\repos\hermes-rs-n0   Branch: native/p0-spike   Model: Opus 5.5 (subagent)
Cut from `main` at 02c8d50. Uncommitted.

### Verdict

**Go: GPUI Kit does what the plan needs.** Nothing on the stop list failed: markdown selection,
the input (including the IME protocol) and the virtual list all work. Two things need a
workaround (code highlighting goes through `syntect`, and the Zed theme mapping writes a second
spelling of six keys), and the OS-level interactions are waiting for the user's hands.

| Item | Verdict | How it was checked |
|---|---|---|
| `gpui-kit` pinned `=0.10.0`, recorded in `vendor/SOURCES.md` | works | — |
| Two GPUI copies (vendored for `crates/app`, `gpui-pre` 0.3.12 for `crates/native`) | works | `cargo build --workspace` passes; no `links` clash between the GPUI copies. They never share a binary. |
| Virtual list, 2,000 mixed-height messages, pinned to the bottom while the last one streams | works | GPUI Kit's `MessageScroller` (GPUI `ListState` with `FollowMode::Tail`, a "Jump to latest" button). Test: 85 stream ticks plus a finished message turning into a new row, still following the tail. Capture of our window: last row pinned while streaming. Scrolling *smoothness* by wheel/drag: **not verified by hand: needs user**. |
| Markdown: headings, lists, task lists, tables, links, quotes, fenced code in 4 languages, highlighted | works with workaround | `TextView::markdown`. Test: table cells, list items, heading and link text are painted. Capture: highlighted TypeScript block, heading, quote, link, task list in One Light. Rust, Python and Bash go through the same `syntect` path but were not each looked at. **Workaround:** `syntect` instead of tree-sitter (below). |
| Text selection and copy, inside a message and inside a code block | works (headless); by mouse **not verified by hand: needs user** | Test: pointer drag over prose, then over the code block, selects text; the `Copy` action puts it on the (test platform's) clipboard. The real mouse and the Windows clipboard need the user. Selection is per message (each message is its own `TextView`), as the README expects. |
| Multiline input: typing, newline, undo, paste, IME | works (headless); OS IME **not verified by hand: needs user** | GPUI Kit's `Textarea` (`auto_grow(2, 8)`). Test: type, Enter (newline), Ctrl+Z, Ctrl+A + Ctrl+V, then an IME preedit (`にほ`, marked range set) committed as `日本` through `ElementInputHandler`, the same handler the Windows platform calls. Win+. emoji panel, a Japanese or Greek IME: needs the user. |
| Resizable three-pane layout | works (renders); dragging **not verified by hand: needs user** | `h_resizable` + `resizable_panel` with size ranges (sidebar 160–420, right 200–520). Capture shows the three panes. |
| Zed theme mapped by hand onto GPUI Kit's theme | works with workaround | `zed_theme.rs`: a 60-row table from Zed `style` keys to GPUI Kit colour keys, plus `players[0]` for selection and caret; Zed's `style` passes through as GPUI Kit's `highlight` section (same format). Tests: One Dark and One Light map, every colour parses, applying it changes `cx.theme().background`. Capture: One Light applied. **Workaround:** see "GPUI Kit's syntect build" below. |

### Measurements (release build, `target/claude/release/hermes-native.exe`, this machine)

| What | Value |
|---|---|
| `main` to first frame drawn | 507–532 ms (4 runs) |
| Idle, 10 s after start (`--no-stream`) | working set 133–137 MB, private 161 MB |
| Streaming (3 words / 60 ms) | working set 137 MB, private 165 MB; ~0.4 s CPU over ~9 s (≈4% of a core) |
| Debug build, first frame | ~3.3 s |

Counted from the start of `main` (`first-frame-ms` on stderr, from `window.on_next_frame`), so the
loader's time before `main` is not included. Not investigated (no performance work for now); a
guess for the half second is GPU/DirectWrite start-up and the `syntect` syntax set, which loads on
the first highlighted block.

### Done
- `crates/native` (package `hermes-native`, binary `hermes-native`, version 0.4.0 like the
  desktop app): `main.rs` (flags `--no-stream`, `--quit-after <s>`, `--theme
  <dark|light|one-dark|one-light>`, prints `first-frame-ms`), `spike.rs` (the window and its
  headless tests), `fake.rs` (deterministic fake conversation), `zed_theme.rs` (the mapping),
  `build.rs` (8 MB main-thread stack).
- `vendor/SOURCES.md`: *GPUI Kit* section (0.10.0, `gpui-pre` 0.3.12 = Zed `d2a4fb6`).
- `AGENTS.md`: `crates/native/` row and `hermes-native` in the package names.
- `Cargo.lock`: +289 packages (GPUI Kit, `gpui-pre*`, `syntect`…).

### Files
`crates/native/{Cargo.toml,build.rs,src/main.rs,src/spike.rs,src/fake.rs,src/zed_theme.rs}` (new),
`Cargo.lock`, `vendor/SOURCES.md`, `AGENTS.md`, this note.

### Tests
- `cargo test -p hermes-native`: 10 passed (3 runs, no flakes): 4 `zed_theme`, 1 `fake`, 5 headless
  `spike` tests (`the_list_follows_the_bottom_while_text_streams`,
  `the_composer_types_undoes_pastes_and_composes`, `the_zed_theme_applies`,
  `markdown_selection_copies_prose_and_code`, `markdown_tables_lists_and_links_are_painted`).
- `cargo build --workspace`: passes (needed `target/hermes-src/apps/desktop/dist-webapp`, which a
  fresh worktree lacks; copied from the main checkout's build, not rebuilt).
- `cargo test -p hermes-desktop`: passes (run because the lock file changed, see `smallvec`).
- `cargo clippy -p hermes-native --all-targets`: clean. `cargo fmt -p hermes-native -- --check`:
  clean.
- Captures of our own window only (PrintWindow on the process we started from `target\claude`,
  `HERMES_RS_HOME=target/native-test`): default dark while streaming, One Light (cropped).

### Deviations from the plan
- **Highlighting through `syntect`, not tree-sitter.** GPUI Kit's `tree-sitter*` features pull
  `tree-sitter` 0.26; `crates/highlight` (used by the frozen `crates/app`) has 0.27; both declare
  `links = "tree-sitter"`, so Cargo refuses the workspace. This is a duplicate `links` key, but not
  between the GPUI copies (those coexist), so I did not stop: `syntect` is a supported GPUI Kit
  backend and covers the four languages. Tree-sitter can come back when `crates/app` and
  `crates/highlight` go (Phase 9), or if `crates/highlight` moves to 0.26. The user may prefer
  otherwise (the plan's README says "GPUI Kit's tree-sitter highlighting").
- **Theme file path:** the plan says `vendor/zed/assets/themes/`; the files are in
  `assets/themes/` (copied there by `vendor_zed.py`). Used `assets/themes/one/one.json`.
- The `hermes-native` binary *is* the spike for now (no separate spike binary); Phase 2 replaces
  its contents.

### For the reviewer
1. **`smallvec` went down 1.16.1 → 1.15.2 for the whole workspace**: `serde-saphyr` (via
   `rust-i18n` ← `gpui-component`) caps it below 1.16 (`cargo update -p smallvec --precise
   1.16.1` is refused). `crates/app`'s vendored GPUI and everything else now build on 1.15.2;
   `cargo build --workspace` and `cargo test -p hermes-desktop` pass. The other existing
   packages that changed version were re-resolved together with the new ones:
   `proc-macro-crate` 2.0.2 → 2.0.0 and 3.4.0 → 3.5.0, `toml` 0.8.2 → 0.8.23, `toml_datetime`
   0.6.3 → 0.6.11, `toml_parser` 1.0.4 → 1.1.5, `toml_edit` 0.23.7 gone (other `toml_edit`
   versions present). All build-time crates (proc macros, manifest parsing).
2. **`async-task` patch reaches GPUI Kit too**: the workspace `[patch.crates-io]` swaps in Zed's
   fork for `gpui-pre-scheduler` as well. Builds and tests pass; when `crates/app` is retired and
   the patch goes, GPUI Kit's GPUI will run on stock `async-task` 4.7 (what it was published
   against). Recorded in `vendor/SOURCES.md`.
3. **`wgpu` pinned to 30.0.1 in the lock** (`cargo update -p wgpu --precise 30.0.1`): with the
   `test-support` dev-dependency, the lock resolution includes the optional `gpui-fast*` crates,
   and `gpui-fast-wgpu` wants `=30.0.1` while 30.0.2 had been picked. `gpui-fast` is in
   `Cargo.lock` but never compiled (`cargo tree -i gpui-fast` prints nothing).
4. **GPUI Kit's syntect build**: without the `tree-sitter` feature it compiles a
   `HighlightThemeStyle` (`highlighter/wasm_stub.rs`) whose `editor.*` fields lack the serde
   renames, so `editor.background` etc. in any theme's `highlight` section are silently ignored
   (Kit's own default theme too). `zed_theme::without_nulls` writes both spellings. An upstream
   bug worth reporting; CP2.1 should keep the workaround or a test that catches it
   (`one_dark_maps_onto_a_kit_theme` does).
5. **Debug builds overflowed the main thread's 1 MB stack** before the first frame
   (`thread 'main' has overflowed its stack`). `build.rs` links bins with `/STACK:8388608`, as
   Zed's binary does. Tests were unaffected (test threads). Candidate for `docs/PITFALLS.md` in
   CP0.2 (step 7), with the next item.
6. **`#[gpui_kit::test]` and a glob import**: `use gpui_kit::*` (directly or through
   `use super::*`) brings GPUI's `test` macro, which shadows Rust's `#[test]` that
   `#[gpui_kit::test]` expands to: "recursion limit reached while expanding `#[test]`". The test
   module imports `core::prelude::v1::test` explicitly.
7. **What the headless tests do not prove** (GPUI Kit's own TESTING.md says the same): the
   clipboard is the test platform's, the IME is driven through the input handler (no Windows IME,
   no candidate window), there are no pixels. Hence the user checklist below.
8. `Textarea`: Enter inserts a newline (CP5.1 needs Enter-to-send and Shift+Enter for newline on
   top). The streamed message's text is kept twice (in the view's `TextViewState` and a `String`
   copy to move into the list when it ends): fine for a spike, not a pattern for CP4.1.
9. The `TextView::markdown(id, text)` rows parse when they scroll into view; whether that is
   smooth with fast wheel scrolling over 2,000 rows is the user's check (item 2 below), and CP4.1's
   concern.

### User checklist (spike window)
Build and start (PowerShell, from the worktree):
```powershell
$env:CARGO_TARGET_DIR = "target/claude"; cargo build -p hermes-native --release
$env:HERMES_RS_HOME = "$PWD\target\native-test"; .\target\claude\release\hermes-native.exe
```
1. The window opens in under a second with three panes; the middle one shows the end of the
   conversation and a message growing word by word at the bottom, which stays in view.
2. Scroll up fast with the wheel through a few hundred messages: no stutter, no blank rows, rows
   don't jump. The "Following the bottom" line on the right turns `false` and a "Jump to latest"
   button appears; click it: back at the bottom, following again.
3. Drag-select a few words in a message, Ctrl+C, paste into Notepad: the same words. Do the same
   inside a code block (grey box with a language label).
4. Click the input at the bottom, type two lines (Enter makes a new line), Ctrl+Z (undoes), paste
   something copied from another app (Ctrl+V).
5. Press Win+. and pick an emoji: it lands in the input. If a Japanese or Greek keyboard is
   installed, switch to it (Win+Space) and type: the candidate window appears next to the caret
   and the committed text lands in the input.
6. Drag the two borders between the panes: each pane resizes, stops at its limits, nothing
   flickers.
7. Click "One Dark", "One Light", "Default light", "Default dark" on the right: the whole window
   recolours each time, code colours included.
8. Optional: start with `--theme one-light`, and with `--no-stream --quit-after 10` to see it close
   by itself.

### Review (Opus 5.5)

Read the plan, the note, `crates/native` whole, the `Cargo.lock` package diff and the
`vendor/SOURCES.md`/`AGENTS.md` changes. `main` had not moved since the branch was cut.

**Verdict: the "Go" stands, and the note is honest.** Each CP0.1 item has evidence; the items only
hands can prove (wheel smoothness, real mouse and Windows clipboard, OS IME / Win+., dragging
pane borders) are marked "needs user", not claimed. Done criteria checked: `gpui-kit = "=0.10.0"`
(dependency and dev-dependency); the SOURCES.md section names the version, `gpui-pre` 0.3.12 and
Zed `d2a4fb6` (matches `gpui-pre`'s description); `cargo build --workspace` passes with both GPUI
copies; the lock's only removed/changed existing packages are the seven the note lists
(`smallvec`, `proc-macro-crate` ×2, `toml`, `toml_datetime`, `toml_edit`, `toml_parser`).
No Zed code in `crates/native` (`build.rs` repeats Zed's `/STACK` linker flag, one line, cited);
no temp folders; no async blocking.

Findings and fixes:
1. **Zed theme map dropped the link colours.** `link.foreground`, `link.hover.foreground`,
   `link.active.foreground` are not GPUI Kit keys (its keys are `link`, `link.hover`,
   `link.active`); Kit's deserializer dropped them silently, so links kept Kit's colour. The test
   `every_mapped_zed_colour_parses` skipped missing keys, so it could not see this. Fixed the keys;
   the test now fails when a mapped key that Zed sets is not kept.
2. **The tail-following test only checked a flag.** `the_list_follows_the_bottom_while_text_streams`
   now also checks the viewport's top row is within the last 50, that streamed text was painted,
   that the first message was not, and that fewer than 50 user rows were painted (the list is
   virtual). Note: Kit's `test_support::texts` sees `TextView`s only, not plain labels.
3. **Undo test was weak** (`assert_ne`); now asserts the result is a shorter prefix of the typed
   text.
4. **`--quit-after -1` (or NaN) panicked** in `Duration::from_secs_f64`; now ignored via
   `try_from_secs_f64`.

Deviations, judged:
- `syntect` instead of tree-sitter: acceptable for the spike (the clash is between tree-sitter
  versions, not the GPUI copies, so the plan's stop rule does not apply) and clearly documented.
  **Decision for the user:** the README says "GPUI Kit's tree-sitter highlighting"; keep `syntect`
  until Phase 9 retires `crates/highlight`, or move `crates/highlight` to tree-sitter 0.26.
- `smallvec` 1.15.2, the `async-task` patch reaching `gpui-pre-scheduler`, the `wgpu` 30.0.1 lock
  pin, the 8 MB stack, `assets/themes/` path: all fine and recorded; `cargo build --workspace` and
  `cargo test -p hermes-desktop` pass.

Left as is (spike-only, CP4.1's concern): `Rc::make_mut` on the message list may clone all 2,000
messages when a streamed message finishes while the row renderer still holds the `Rc`; the
streamed text is kept twice (the note says so).

Tests after the fixes: `cargo test -p hermes-native` 10 passed (3 runs); clippy and fmt clean;
`cargo build --workspace` passes; `cargo test -p hermes-desktop` 193 + 68 passed.

## CP0.2 — The dev loop
Worktree: C:\Users\alexandros\repos\hermes-rs-n0   Branch: native/p0-spike   Model: Opus 5.5 (subagent)
On top of fdd2906 (CP0.1). Uncommitted. Resumed once after an API outage; every step re-checked.

### Done (steps 1–7)
1. **Element dump** (`src/dev/`). GPUI Kit's observation comes first: the `.test_support()`
   registry gives path, bounds, visibility, focus, disabled and the a11y label, and Kit's controls
   register themselves. On top of it, our `.probe()` / `.probe_text(text)` wrapper
   (`dev/probe.rs`, the same Element shape as Kit's `Observed`) records the computed background
   and border, the inherited text colour, hitbox hover and the width of the text on one line.
   Unprobed elements get `bg` from `Window::painted_quads()`: the topmost quad of exactly their
   bounds with a visible fill. The text output is a tree, one line per element.
   With `--features dev`, both binaries take `--dump <file>` (written 0.5 s after start), and F12
   writes `layout-dump.txt` in `HERMES_RS_HOME`. Checked live on our own gallery and spike windows
   (`target/native-test/{gallery,spike}-dump.txt`). Without `dev`, `.probe()` returns the element
   unchanged.
2. **Layout checks** (`src/testing.rs`). `Layout::of(window)` plus `Checks`, which collects
   failures from `same_row`, `same_height`, `inside`, `no_overflow`, `background` (equals a
   token), `text_color`, `width_within` and `state(hovered, focused, disabled)`. `assert()`
   reports every failure together with the dump. Also `color_of`, `text_color_of` and `bounds`.
   Fixtures: `headless_app()` (a HeadlessAppContext with the **real** DirectWrite text system),
   `open_window`, `draw` and `layout`. The helpers also work in `#[gpui_kit::test]`, where text is
   the fixed-width fake.
3. **Image capture: decided off-screen, with no window.** GPUI's test platform renders through
   `gpui-pre-wgpu`'s `WgpuHeadlessRenderer` (it picked the RTX 3060 over Vulkan here) with real
   DirectWrite text. Kit's TESTING.md says Windows has no headless renderer, but that is only
   because GPUI wires it for Linux. No PrintWindow or WGC fallback was needed. Images are cropped
   to the story element's bounds, at scale 2.
4. **Snapshot harness** (`src/gallery/snapshots.rs`, `--features snapshots`). Each story is
   rendered in each state, in Kit light and Kit dark, with the states side by side and 4 px
   magenta gaps. Each image is compared with `crates/native/snapshots/<story>-<theme>.png`; a
   pixel counts as changed when any channel moves by more than 8. On a difference the harness
   writes `<name>.actual.png` and `<name>.diff.png` to `target/claude/snapshots/`.
   `UPDATE_SNAPSHOTS=1` accepts. A missing baseline fails and writes only `.actual.png`.
5. **Gallery** `hermes-native-gallery` (`src/bin/gallery.rs`, `src/gallery/view.rs`). It has a
   story list, the selected story's states in columns, and GPUI Kit / Zed One and Light / Dark
   switches. Stories register in one `register_stories![...]` list. I did not use `inventory`
   because link-time registration from an rlib is fragile. The harness reaches the hover, press
   and focus states through `Interaction` (Kit's pointer helpers, and `window.focus_next` for
   focus).
6. **`scripts/native/check.ps1`** (`-Update` accepts). It builds `--features dev --bins`, then
   runs `cargo test -p hermes-native` and the `--features snapshots` tests. It prints only
   failures, the paths of diff and new images, and one summary line. I tested the failure output
   by swapping in another baseline, then restored it.
7. **Docs.**
   - `docs/TESTING.md` §6 *Native UI*: the three kinds of check, accepting snapshots, the subagent
     review, the gallery.
   - `docs/PITFALLS.md` *Native UI*: the 8 MB stack and test-macro glob from CP0.1, headless =
     fake text on Windows, off-screen rendering through wgpu, the gpu-allocator lock edit, Kit's
     resizable panels, the `focused()` panic, a button's quads and semi-transparent fills.
   - `docs/native-ui/README.md`: the capture decision and the risk row.
   - `vendor/SOURCES.md`.
   - `AGENTS.md`: the check command and the `crates/native` row.

Other changes: the crate is now a library plus two binaries (`src/lib.rs`). `ThemeChoice` and
`apply_theme` moved out of the spike into `src/themes.rs`. The sample story is
`src/gallery/button.rs`.

### Done criterion
The sample story is *Button: five states*: Kit's default button in the normal, hover, pressed,
focused and disabled states. It has:
- **A layout test**, `the_button_in_its_five_states_lays_out_and_colours_right`. In both themes it
  checks `inside`, `no_overflow` and `same_row`. It checks that the background equals the
  `button`, `button_hover` and `button_active` tokens, the caption's text colour, the
  hovered/focused/disabled flags, and the same height in every state. It first asserts that the
  three tokens differ, so the colour checks mean something.
- **Snapshot tests**: `every_story_matches_its_snapshot`, plus
  `rendering_a_story_twice_gives_the_same_pixels`.
- **A broken variant**, `SQUEEZED_STORY` (the button forced to 64 px). The layout checks catch it
  (`a_squeezed_button_is_caught_by_the_layout_checks`: `inside(caption, button)` and
  `no_overflow(button)` fail), and so does the snapshot comparison
  (`a_squeezed_button_is_caught_by_the_snapshot_comparison`). It is never registered in the
  gallery.
- **A test of the dump format**, `the_dump_names_each_element_with_bounds_colours_and_state`.

### Pane resize (the user's CP0.1 report)
- **Reproduced:** the new test `dragging_a_border_past_its_limit_leaves_the_other_side_pane_alone`
  failed at first. Dragging the right border 900 px to the left grew the sidebar from 240 to 265.
- **Cause, in GPUI Kit** (`gpui-base` `resizable/mod.rs` `resize_panel_at_handle`, and
  `panel.rs`):
  - when a pane shrinks, Kit takes space from panes beyond the dragged border's neighbour;
  - it never stops the growing pane at its maximum;
  - every panel has `flex-grow: 1`, so the space the capped right pane can't take is shared by the
    other panes, the sidebar included.
- **Fix (small, in the spike):** the side panes are `.fixed()` and the middle pane has
  `size_range(320..)` (`MAIN_MIN_WIDTH`).
- **Result:** the test now passes. The sidebar stays 240, the right pane stays at or under 520,
  the middle pane stays at or over 320, and the three fill the window. Dragging the sidebar
  border leaves the right pane's width alone.
- Written up in PITFALLS for Phase 2's frame.

Right-click: GPUI Kit has a built-in context menu (`ContextMenuExt::context_menu`, a PopupMenu, in
`gpui-component/src/menu/context_menu.rs`). TextView just doesn't attach one. As instructed, this
is not in PITFALLS; Phase 4/5 attach a context menu to messages.

### Snapshots (new baselines, ready to commit)
- `crates/native/snapshots/button-states-light.png` (1316×112)
- `crates/native/snapshots/button-states-dark.png` (1316×112)

The final run reported no differences (no `.diff.png`). I checked the images only by their numbers
(88 and 98 distinct colours, gap columns where expected), not by eye. Both are new, so the Haiku
visual check should look at them once.

### Tests
- `cargo test -p hermes-native`: 14 passed (2 runs). With `--features snapshots`: 17 passed
  (2 runs).
- `scripts/native/check.ps1`: "native check: all ok", exit 0 (about 26 s on a warm build).
- `cargo build --workspace`: passes.
- `cargo clippy -p hermes-native --all-targets`, with and without `snapshots`: clean.
- `cargo fmt -p hermes-native -- --check`: clean.
- `cargo test -p hermes-desktop`: not run. The lock change only touches the `windows` version of
  `gpu-allocator` and adds the two optional dependencies of `hermes-native`; the desktop app uses
  neither.

### Deviations
- Snapshot names carry the theme: `<story>-light.png` and `<story>-dark.png`. The plan says
  `<name>.png`.
- The gallery's theme switch covers the two existing families (Kit default and Zed One), each in
  light and dark. The real theme list is CP2.1.
- Layout tests for stories use `HeadlessAppContext` with a plain `#[test]`, for real text width,
  rather than `#[gpui::test]`. The helpers work there too: the pane test uses `#[gpui_kit::test]`.

### For the reviewer
1. **`Cargo.lock` hand edit:** `gpu-allocator 0.28.0` now uses `windows 0.62.2` (it was 0.57.0).
   Without this, the DX12 code in `wgpu-hal` 30 doesn't compile. `cargo update -p gpu-allocator`
   chose 0.58 and also moved the `iana-time-zone` and `windows-targets` entries, so I edited the
   one line by hand. Cargo accepts it (`>=0.53,<=0.62`). Is a hand-edited lock acceptable, or
   should the version be pinned another way?
2. **New direct dependency `gpui-pre-wgpu =0.3.12`**, plus `image` 0.25 (png), both only under
   `snapshots`. It falls inside the ground-rule-3 exception (a `gpui-pre*` crate that GPUI Kit
   pins) and is recorded in SOURCES.md. It builds wgpu and naga in the test profile; the first
   build takes about a minute.
3. **Snapshots depend on the GPU and driver** (Vulkan on the NVIDIA card here). The tolerance is 8
   per channel and no changed pixels are allowed; repeated runs gave identical images. Another
   machine or a driver update may need `-Update`. Neither the wgpu backend (`WGPU_BACKEND`) nor
   the adapter is pinned.
4. **Focus in `dump.rs`:** it reads Kit's focus flag from the snapshot's `Debug` text, because
   Kit's `focused()` panics on a missed binding and the field is private. It's a hack, but it is
   contained in one function.
5. **Hover and text colour limits:** for unprobed elements, hover means the pointer is inside the
   bounds (occlusion is ignored); probed elements use the hitbox. `fg` exists only for probed
   elements: GPUI exposes glyph colours only with `bench-support`, which pulls in criterion.
6. **How `.probe()` composes:** it is `Observed<Probe<E>>`. Under `cargo test`, the binaries
   compile the library without `cfg(test)` but with Kit's `test-support` unified, so `Probed<E>`
   is plain `E` there. That is consistent; I checked that it builds both ways.
7. **F12 has no automated test**, because it needs a real window and key. `--dump` was run live on
   our own gallery and spike windows.

### User checklist
Build (PowerShell, in the worktree):
`$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --features dev --bins`, then
`$env:HERMES_RS_HOME="$PWD\target\native-test"`.
1. Run `.\target\claude\debug\hermes-native-gallery.exe`. A window opens with "Button: five
   states" on the left and five labelled buttons on the right.
2. Point at the "hover" button: it tints. Hold the mouse down on "pressed": a darker tint. Press
   Tab: a focus ring appears on a button. The "disabled" button is faded and ignores the mouse.
3. Click "Zed One", then "Dark", "Light" and "GPUI Kit": the whole window recolours each time.
4. Press F12. The console prints `layout dump: ...\target\native-test\layout-dump.txt`. Open the
   file: one line per element, with bounds and colours.
5. Run `.\target\claude\debug\hermes-native.exe` and drag the right pane's border far to the
   left. The right pane stops at its maximum, the left pane doesn't change, and the middle pane
   keeps at least about 320 px. Drag the left border far to the right: the right pane doesn't
   change.
6. Run `powershell -NoProfile -File scripts/native/check.ps1`. It prints
   `native check: all ok`.

### Visual check (Sonnet 5.5)
- button-states-light: expected — five "Save changes" buttons separated by magenta gaps, left to right: normal (white, thin border), hover (light grey fill), pressed (slightly darker grey), focused (white with a thick grey ring), disabled (faded text, faint border). Labels are legible and unclipped, spacing is even and nothing overlaps. Hover and pressed differ only slightly but are distinguishable.
- button-states-dark: expected — the same five states on a near-black background: normal (dark fill, thin border), hover and pressed (progressively lighter fill), focused (light ring), disabled (dim grey text). Labels are legible and unclipped, spacing is even and nothing overlaps.

### Review (Opus 5.5)

Read AGENTS.md, the plan (*How the work is run*, Phase 0), the README's dev loop, this note, the
whole diff and every new file in `crates/native` and `scripts/native`. `main` had not moved since
the branch was cut (no merge needed). `/code-review` was not run as a skill; the pass below is by
reading and running.

**Verdict: CP0.2's Done criterion is met, and the note is true.** Point by point, with what I ran:
- *Element dump* (`dev/`): built on Kit's `.test_support()` registry first, our `.probe()` adds
  colours, hover and text width. `the_dump_names_each_element_with_bounds_colours_and_state` pins
  the format; I ran our own gallery with `--dump` (`target/native-test/review-gallery-dump.txt`):
  one line per element, tree-indented, bounds, `bg`/`fg`, labels.
- *Layout helpers* (`testing.rs`): `same_row`, `same_height`, `inside`, `no_overflow`,
  `background`, `text_color`, `width_within`, `state`, `color_of`, `text_color_of`, `bounds`;
  usable from `#[gpui_kit::test]` (the pane test) and `headless_app()` (the button test).
- *Image capture*: off-screen through `gpui-pre-wgpu`'s headless renderer, cropped to the
  `story` element. *Snapshot harness*: `--features snapshots`, tolerance 8 per channel and zero
  changed pixels, `.actual.png`/`.diff.png` in `target/claude/snapshots/`, `UPDATE_SNAPSHOTS=1`.
  No temp folders are made (outputs go under the target dir), so nothing can leak.
- *Gallery* with one `register_stories![..]` list read by both the gallery and the harness.
- *`check.ps1`*: builds into `target/claude`, sets `HERMES_RS_HOME=target/native-test`, starts and
  stops no process, prints only failures, image paths and a summary.
- *Broken variant caught by both*: besides the two tests on `SQUEEZED_STORY`, I squeezed the real
  story to 64 px and ran `check.ps1`: the layout test failed (`inside(caption, button)`,
  `no_overflow(button)`, with the dump) and the snapshot test wrote two `.diff.png`; restored, all ok.
- *Pane resize*: with `.fixed()` and the middle pane's `size_range` removed,
  `dragging_a_border_past_its_limit_leaves_the_other_side_pane_alone` fails (`width_within(sidebar):
  265 not in 240..=240`), so it pins the user's report; with the fix it passes.
- *`Cargo.lock` hand edit* (`gpu-allocator` → `windows 0.62.2`): `cargo build --workspace
  --locked` passes and leaves the lock unchanged, so it is a resolution Cargo accepts, not a
  corrupt lock. Only `wgpu-hal` (via `gpui-pre-wgpu`, i.e. `hermes-native`) uses `gpu-allocator`;
  the frozen app does not. Acceptable; PITFALLS says to keep it on a lock refresh.
- *Ground rules*: no Zed code; the new `gpui-pre-wgpu` is a `gpui-pre*` crate pinned `=0.3.12`, inside
  AGENTS.md rule 3's exception and recorded in SOURCES.md (PHASES.md's "GPUI only through
  `gpui-kit`" is stretched by this one, test-only, feature-gated crate; noted, not changed). No
  t3code layout is copied yet, so nothing to cite. Visual check: 2 expected, 0 unexpected.

Findings and fixes:
1. **`same_height`, `color_of`, `text_color_of` were never exercised, and no helper's failure
   message was.** The button test compares heights across windows by hand. Added two unit tests in
   `testing.rs` on a hand-made dump: every helper passes on a good layout, and each one (plus
   "no element named", "2 elements named" and `assert()`'s dump) reports a bad one.
2. Not fixed, minor: a live window's dump header can print a meaningless pointer position when the
   pointer has never been over the window (`pointer 33981,32783` in my gallery run); hover for
   unprobed elements is then simply false. Snapshot `Err(_)` on opening a baseline also covers a
   corrupt PNG, reported as "no baseline". Both harmless.
3. Not fixed, by design: snapshots depend on GPU and driver (no backend or adapter pinned); another
   machine may need `-Update`. Rendering twice gives identical pixels here, and a tolerance of 8
   with zero changed pixels allowed is strict enough that a glyph or colour change shows.

Tests after the review: `cargo test -p hermes-native` 16 passed; `--features snapshots` 19 passed;
`scripts/native/check.ps1` "native check: all ok"; `cargo build --workspace --locked` passes;
`cargo test -p hermes-desktop` 193 + 68 passed; clippy (`--all-targets --features snapshots`)
and fmt clean.

## Phase 0 — user checklist

Phase 0 is done: GPUI Kit works for this plan (CP0.1), and the checking tools exist (CP0.2).
Build once (PowerShell, in the worktree or, after the merge, the main checkout):

```powershell
$env:CARGO_TARGET_DIR = "target/claude"; cargo build -p hermes-native --features dev --bins
$env:HERMES_RS_HOME = "$PWD\target\native-test"
```

Already done by you on CP0.1: dragging the pane borders (item 6 of CP0.1's list), which found the
resize bug fixed in CP0.2, and right-clicking a message (no context menu yet: Phases 4/5 add one).
Still open unless you have done them (CP0.1's list above, items 1–5 and 7):

1. Run `.\target\claude\debug\hermes-native.exe`. Three panes; the middle one follows a message
   growing at the bottom.
2. Scroll up fast with the wheel through a few hundred messages: no stutter, no blank rows; "Jump
   to latest" brings you back to the bottom.
3. Drag-select words in a message and inside a code block, Ctrl+C, paste into Notepad: same text.
4. In the input: type two lines, Ctrl+Z, paste from another app.
5. Win+. and pick an emoji: it lands in the input. With a Japanese or Greek keyboard (Win+Space),
   the candidate window sits next to the caret.
6. Drag the right border far to the left: the right pane stops at its maximum, the left pane does
   not change, the middle keeps about 320 px. Drag the left border far right: the right pane does
   not change. (The fix for your report.)
7. Click the four theme buttons on the right: the whole window recolours, code colours included.
8. Run `.\target\claude\debug\hermes-native-gallery.exe`: "Button: five states" on the left, five
   buttons on the right. Point at "hover" (tint), hold the mouse on "pressed" (darker), press Tab
   (focus ring), "disabled" is faded and ignores the mouse. "Zed One" / "Dark" / "Light" /
   "GPUI Kit" recolour the window.
9. In the gallery, press F12: the console prints `layout dump: ...\target\native-test\layout-dump.txt`;
   the file has one line per element with bounds and colours.
10. Run `powershell -NoProfile -File scripts/native/check.ps1`: it prints `native check: all ok`
    (about 15–30 s when built).

**Decision for you: code highlighting.** The plan says GPUI Kit's tree-sitter highlighting; the
spike uses its `syntect` backend because Kit's tree-sitter 0.26 cannot sit in one workspace with
`crates/highlight`'s 0.27 (frozen GPUI app). Options: (a) keep `syntect` until Phase 9 removes
`crates/app` and `crates/highlight`, then switch (recommended: no work now, the four languages
highlight); (b) move `crates/highlight` to tree-sitter 0.26 now (touches the frozen app's build).
