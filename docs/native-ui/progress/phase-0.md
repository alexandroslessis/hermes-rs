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
