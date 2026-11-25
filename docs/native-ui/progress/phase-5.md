# Phase 5 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-n5   Branch: native/p5-composer

## CP5.1 — Input and sending
Worktree: C:\Users\alexandros\repos\hermes-rs-n5   Branch: native/p5-composer   Model: Sonnet 5.5 (subagent)
Cut from `main` at 0b2d005. Uncommitted.

### Files
- New `crates/native/src/thread/composer/`: `mod.rs`, `data.rs` (no GPUI: `SendRequest`, `Sent`, `QueuedItem`, `QueueOp`, `Picks`/`Pick`,
  `Mode`, `EFFORTS`, `Controls`, `ModelChoices` + filter, `Drafts`), `model.rs` (`ComposerModel`, `ComposerEvent`, `Target`, `Choices`),
  `view.rs` (`ComposerView`, the queue strip, the model/effort and mode menus, `hero` = the new-thread screen), tests: `tests.rs`,
  `wiring_tests.rs`, `real_tests.rs`.
- New `gallery/composer.rs` (stories `composer`, `composer-menus`, `composer-new`).
- Edited: `model/backend.rs` (Backend: `controls`, `draft_controls`, `model_choices`, `create_thread`, `send`, `stop`, `queue_op`, `pick`,
  `set_default_model`; `CoreBackend` implements them; `NativeHooks`; `FixtureBackend` records every `ComposerOp` and plays the queue),
  `model/mod.rs` (`AppModel` makes the composer beside the thread's model, forwards events, creates-opens-sends for a new thread),
  `thread/model.rs` (`expect_prompt` / `forget_prompt` / `show_user`: the user's row shows when the turn with that admission id starts),
  `thread/view.rs` (takes the composer; the dev strip is gone), `workspace.rs` (composer views, drafts, pruning, the hero),
  `ui_state.rs` (`drafts`), `design/icons.rs` (7 icons), `app.rs`/`main.rs` (flag docs; `--dev` now only enables `--dev-send` and the
  fixture replay), `Cargo.toml` (`hermes_tools` moved to dependencies: `ApprovalMode`), `thread/tests.rs` (dev-strip test became a
  `ThreadModel::submit` test).
- Docs: AGENTS.md row, ARCHITECTURE.md row, TESTING.md §6 (composer paragraph), PITFALLS.md (6 bullets), PHASES.md status,
  native-ui/README.md module row, reference/t3code/README.md (no new screenshot).

### What it does
1. **Input.** Kit's `TextareaState` auto-grow 2 to 9 rows (about t3code's `max-h-52`, then it scrolls), `submit_on_enter`: Enter sends, Shift+Enter
   newline, Ctrl+Enter steers a running turn. An Enter during an IME composition is ignored (`marked_text_range`). Undo/redo are Kit's.
   Draft per thread (`native-ui.json` `drafts`, key = thread id or `new:<project>`), written by the workspace through its debounced
   save (500 ms), at most 100 drafts of 100,000 characters, an empty one removed, one of an unlisted thread/project dropped after 7 days
   (archived threads are unlisted, hence the week; a delete does not remove the draft at once).
2. **New thread.** The "No active thread" state of a project (`Selection::Draft`) is t3code's `DraftHeroHeadline`: headline, "or start
   without a project", the composer. Enter creates the session (`Backend::create_thread`: the project's folder, else the home folder, as the
   Tauri app; Settings' provider/model or the one picked), the app model opens it once listed, then its composer sends the first prompt
   with the picks made on the screen. A refusal (no provider) keeps the text and toasts.
3. **Send / stop / queue.** `Backend::send` -> `Core::submit` (Started / Queued) or `Core::steer`. While a turn runs the button is Stop when
   the input is empty, else the queue icon. The strip above the card lists `Core::queue` (read on `QueueChanged`, `TurnStarted`,
   `TurnEnded`, `Idle`, `QueuedFailed`; what another front end queued shows too), each with Send now (`steer_queued`), Edit (the text goes
   to the input, the next send is `replace_queued`, Cancel gives the draft back), Remove. Over 3 items: first two + "Show all N queued".
   A refused send keeps the draft and toasts; `QueuedFailed` puts the text back with a toast.
4. **Footer.** Mode chip (Manual, Plan, Accept edits, Auto, YOLO: all five `ApprovalMode`s, as the ACP menu; t3code's segmented
   icon menu with the hovered mode's name and description), context meter (a bar; tooltip `18k / 200k (9%)`; from
   `Core::usage_snapshot`, shown once a call has reported input tokens), model chip `model Effort` that opens t3code's picker: search +
   the endpoints' models (`Core::model_options`, read-only, asked when the picker opens) + "Reasoning effort" (Hermes's eight levels).
   Model pick on a thread = `Core::switch_model(session_only)` (deferred by the core while a turn runs; the new `NativeHooks` apply it
   between turns); on the new-thread screen it is also saved as the default (`providers::save_default`). Effort = `pick_effort`, read by
   `turn_options` at the next turn. Mode = `Runtime::set_approval_mode` at once (a new thread: with its first send).

### Tests (all `CARGO_TARGET_DIR=target/claude`)
- `cargo test -p hermes-native --lib`: 336 passed, 2 ignored. Composer: 54
  (10 data, 31 `tests.rs`, 9 `wiring_tests.rs`, 4 `real_tests.rs`). The real-core tests: real `Core` on a `hermes_testdir` folder with
  `config.json`, wiremock provider whose first reply is held; send, queue two, remove one (the core's own queue checked), the queued one
  runs and its row appears at its `TurnStarted`, Stop, model/effort/mode picked mid-turn (model applies after the turn, mode at once),
  a new thread from the new-thread screen (folder, answer, mode). Stable over 6 runs.
- `scripts/native/check.ps1`, `cargo test -p hermes_core`, `cargo build --workspace`, `scripts/perf/native-thread-check.ps1`,
  `scripts/clean-test-temp.ps1`: results in "Final checks" below.

### Snapshots
New (6), `crates/native/snapshots/`: `composer-t3-{light,dark}.png` (idle, typed, grown to the maximum, running with Stop and the
meter, queue of three + text), `composer-menus-t3-{light,dark}.png` (model/effort picker, permissions menu, narrow 352 px card with a
long model name), `composer-new-t3-{light,dark}.png` (the new-thread screen). No pre-existing baseline changed (no `.diff.png`).
I looked at the dark idle sheet, the dark menus and the light new-thread image only; a Haiku check should look at all six.

### Deviations
- **Context meter in the footer** (t3code shows it in the thread details card, which the native UI has no equivalent of): a bar + tooltip.
- **No attach button, speech button, stash menu, checkout/branch row under the card** (attachments are CP5.2, the branch is in the header).
- **Modes**: five, not three: Manual, Plan, Accept edits, Auto, YOLO (the runtime's `ApprovalMode`; Accept edits is the default
  of `approvals.mode: manual`, YOLO is "Full access"). The picker is the permissions segment menu, not inside the model popup.
- **Model picker**: no per-provider tabs, favourites or Ctrl+N shortcuts, no arrow-key navigation (search, then Enter takes the first
  match; the list is clicked). Up to 200 models drawn; a search narrows the rest.
- **Redirect** (`Core::redirect`, interrupt and send this) has no control: t3code has none (it has "Send now" = steer and Edit).
- The user's message row appears when the turn starts, not at the Enter (a few ms with a local core; a queued prompt appears when it
  runs, which is what the queue strip is for).
- A thread's model pick is the thread's; only the new-thread screen's pick changes the default model.
- Mode shown for a thread never sent from this run = Settings' `approvals.mode` (the runtime forgets a session's own mode when it is
  unloaded); picks made in this run are remembered by `CoreBackend`. *(Review: replaced; the mode is read from the runtime,
  `Runtime::loaded_approval_mode`, else the approvals store; see the review below.)*
- No title model in `NativeHooks` (a new thread gets the core's provisional title from its first message).

### Missing core operations
None blocking. Facts: no `Core` call says a session's approval mode while unloaded (see above); `model_options` asks the endpoints over
the network (done on a background thread; offline it lists the configured models only); a session's context usage exists
(`usage_snapshot`) but only the last call's input tokens, persisted in the store.

### For the reviewer
1. `ComposerModel::send` clears nothing itself: the view clears on `Sent` if the text is unchanged; a failed send leaves it. Check the
   double-Enter guard (`sending`) against a slow `create_thread`.
2. `Idle` schedules a second controls read 300 ms later (a read between the core taking a pending model pick and applying it showed the
   old model until the next event; the test clock is manual and the real-core test advances it). *(Review: removed, see below.)*
3. `REOPEN_GUARD` (250 ms) is a time heuristic against `on_mouse_down_out` closing a menu on the same click that would toggle it.
   *(Review: removed, see below.)*
4. `CoreBackend::send` refuses when the session's provider cannot be resolved ("Nothing is changed"), before applying picks; the
   old `submit` used by `--dev-send` stays lenient.
5. `NativeHooks::settings` resolves the provider per turn from the record (so a model moved to another provider is followed); `retry`
   and `branch` still take their provider from `session_provider`. Retry now uses `NativeHooks` too.
6. Drafts of deleted threads stay up to a week (see 1. in "What it does"); deleting could remove them at once if `AppModel` told the
   workspace (`ModelEvent` has one variant that the sidebar tests match irrefutably, so I did not add one).
7. The queue strip's "Edit" turns the next send into `replace_queued`, which goes to the back of the queue (the core's semantics), not in place.
8. `hermes_tools` is now a normal dependency of `hermes-native` (`ApprovalMode`), used by `model/backend.rs` only.
9. Pre-existing bug fixed in passing: `CoreBackend::submit` had a stray carriage return in a comment ("As in \retry"), which made a line break.

### Final checks
`scripts/native/check.ps1`: "native check: all ok". `cargo test -p hermes-native`: 336 passed, 2 ignored. `cargo test -p hermes_core`: 140 passed. `cargo build --workspace`: ok (webapp dist copied from the main checkout, as in earlier phases, then removed). `cargo clippy -p hermes-native --all-targets --no-deps`: clean; `cargo fmt` also reformatted four CP4.2 files, reverted (not mine). `scripts/perf/native-thread-check.ps1`: "native-thread-check: ok" (streamed and stored reply equal). `scripts/clean-test-temp.ps1`: dry run 5 folders (1.4 MB), `-Delete` removed them, second dry run 0.

### User checklist (CP5.1)
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n5`): `$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`,
`$env:HERMES_RS_HOME="$PWD\target\native-test"` (a synthetic folder, never `%APPDATA%`).
A. `.\target\claude\debug\hermes-native.exe --fixture --dev` (recorded turn replays on send):
 1. Open "halo": the card at the bottom, "Ask anything...", `Accept edits` at the left, `fixture-model Medium` and a grey arrow at the right.
 2. Type a few words: the arrow turns blue. Enter: the words appear as your message, the recorded turn streams, the input is empty.
 3. Shift+Enter makes a new line; type 12 lines: the card grows and stops growing, then scrolls inside.
 4. While the turn streams: the blue button is a square (Stop) when the input is empty; type something: it becomes the queue icon;
    Enter queues it above the card (dashed bubble, Send now, pencil, x). The fixture does not queue for real; the real core does (B).
 5. Click `fixture-model Medium`: a picker with a search box, four models and "Reasoning effort"; type "llama", Enter: the chip changes.
 6. Click `Accept edits`: five icons (lock, clipboard, pencil, shield, crossed shield); hover each: its name and description; click Plan.
 7. Escape or a click outside closes either menu.
 8. Type text in "halo", open "ayo", type other text, go back: each keeps its own. Quit and start again: still there.
 9. In the sidebar click a project's "new thread" button: "What should we build in structure?" with the card; Enter sends (the fixture
    opens a "new-N" thread).
B. Real core, optional (as Phase 4's item 11: seeded folder, `scripts/perf/mock_llm.py` via `--dev-provider`): send a prompt, send two more
   while it streams: both show above the card; remove one; Stop; the turn stops.
C. **Hand-only, nobody has done these in a real window:** a Japanese or Greek IME (Enter that confirms a composition must not send);
   Win+. emoji panel into the input; Ctrl+V of text (a multi-line paste), Ctrl+Z after typing; the feel of the menus and of
   dragging a selection in the input.

### Visual check (Haiku 5.5)
References opened: `reference/t3code/composer-model-picker.png`, `composer-model-picker-permission.png`, `app-thread-dark.png`, `app-empty-dark.png`. Snapshots viewed as composites and as 2x crops of the dark sheets.
- composer-t3-light: expected — idle, typed, grown (scrolls at max), running with Stop, queue of three; text legible, nothing clipped beyond the intended scroll.
- composer-t3-dark: differs — the input placeholder is "Ask anything..." where t3code has "Ask anything. @ files, $ skills, / commands"; the idle send button is a lighter grey than t3code's dark disc (app-thread-dark). Card radius, border and padding look close to t3code.
- composer-menus-t3-light: expected — model picker (search, list, effort column, check on the selected effort) and the permissions popup overlap the card as designed; the narrow card truncates "openro..." as intended.
- composer-menus-t3-dark: differs — the model list has no Ctrl+N shortcut badges (declared deviation); the permissions popup shows five icons, t3code four (declared); the popup border is faint on black. Item heights and the check mark match composer-model-picker.png.
- composer-new-t3-light: differs — no "Current checkout / branch / Full access" row under the card (declared deviation); headline and "or start without a project" subline match app-empty-dark.png; card legible.
- composer-new-t3-dark: differs — the card border is almost invisible on black where t3code's is faintly visible; placeholder text differs as in composer-t3-dark.

### Review (Opus 5.5)
Verdict: **good after fixes.** Main had not moved (no merge). The send path, queue and drafts were mostly right; four
real bugs and the two timing heuristics were fixed, and the mode source was made exact.

Findings and fixes:
1. **Edit of a queued prompt lost its row** (bug). `replace_queued` was given no admission id, so the core named the edit
   itself: the thread kept waiting for the old id (a leaked expectation) and never showed the edited prompt when it ran;
   a `QueuedFailed` for it could not give the text back. Now the composer names the new admission
   (`QueueOp::Replace { new_id }`), expects it before the op, forgets the old one on success and the new one on failure.
   Test: the edit's row shows once, under the new id; the replaced id shows nothing.
2. **Double Enter on the new-thread screen made two threads** (bug). `sending` dropped when `create_thread` returned,
   but the text stays in the input until the new thread is listed and opened. It now stays "sending" until the screen
   is replaced (a failed create still clears it). Test: second Enter = one `Create`, the send button disabled.
3. **Send now on a prompt another front end queued showed no row** (the text came only from this composer's own map);
   it now falls back to the queue's text. Test.
4. **The mode could be shown wrong.** The `CoreBackend` cache of picks outlived the runtime's memory of them (a session
   unloaded by a move starts from Settings again) and missed `/yolo`; `Runtime::approval_mode` says Accept edits for any
   unloaded session. Added `Runtime::loaded_approval_mode` (additive, nothing else calls it); the footer shows the
   session's own mode while loaded, else the mode it will start in from the approvals store (smart = Auto, off = YOLO,
   else Accept edits), exactly `SessionTools::mode`. So the mode is never unknown and needs no "unknown" mark.
   `Mode::from_settings` and the cache are gone.
5. **The 300 ms second read after `Idle`** is gone. The core publishes `Idle` before `between_turns` applies a
   deferred pick, with no event after; `CoreBackend` now keeps a pick made mid-turn (shared with `NativeHooks`) until its
   `between_turns` has applied it, and `controls` shows it meanwhile. `pick` holds that lock across `switch_model` and
   `between_turns` across `apply_pending_route`, so the two never interleave. The real-core test now asserts the footer
   never shows the old model through the turn's end, then that an idle pick applies at once. Picks also bump the read
   generation (a read in flight from before a pick is read again) and effort/mode picks re-read on success.
6. **`REOPEN_GUARD` (250 ms)** is gone: `on_mouse_down_out` fires in the capture phase, before the chip sees the press,
   so the panel records where that press went down and the chip ignores the click whose `down.position` is the same
   press (`chip_clicked`). The test's 300 ms sleep is gone.
7. **Tokens.** The card outline was raw `hsla`; it is now the theme's foreground at t3code's 5% / 8% (measured: ours
   #1d1d1d on #111111, t3code's #1a1a1a on #0e0e0e in `app-empty-dark.png`: the same step; the Haiku "invisible" is
   t3code's own faintness). The two popups used `--border` and `--popover`; t3code's popovers are `dropdown-glass`, so
   they now use `panel_edge` / `panel_surface` (the edge is visible on black). Baselines regenerated and looked at
   (dark menus); no tracked baseline changed.
8. **Drafts.** Added a total bound (`MAX_TOTAL_BYTES`, 2 MB, oldest dropped) so 100 big drafts cannot make each
   debounced save copy and write 40 MB; a hand-edited file's draft is cut to `MAX_CHARS` on read. Tests: total bound,
   unicode/emoji/CRLF round trip, a cut never splits a character. Quit flush exists (`save_now` from
   `on_window_should_close`; the wiring test calls it and reloads). Malformed file: `UiState::load` per-part fallback.
9. A 1 MB paste test: laid out, sent whole, input cleared, well under the 20 s bound (headless debug).

Judged and kept:
- **Five modes, not the plan's three.** t3code offers four permission levels (supervised, auto-accept edits, auto,
  full access) and the runtime has five `ApprovalMode`s; Accept edits is what `approvals.mode: manual` means, so dropping
  it would leave the default mode unshowable. Five = t3code's four + Plan, as the ACP menu. Kept.
- Placeholder "Ask anything..." kept: `@` and `/` arrive in CP5.2; naming them now would lie. The idle send disc
  (#2d2d2d vs t3code #292929) is within a step; kept. No Ctrl+N badges and no checkout/branch row: declared deviations,
  accepted (no keyboard model switching yet; the branch is in the header; "Full access" is the mode chip).
- A failure after `create_thread` but before the first send leaves an empty thread, opened, with the text back in its
  composer and a toast: clear, and rare (the provider is resolved before the thread is made). No cleanup.
- If the new thread were never listed, the new-thread screen stays "sending" until left: acceptable.
- Ground rules: the views never call the core (grep), t3code cited with the reference screenshots, no raw colours left
  in `composer/`, no Zed code, `TestDir` last, every core call on a background task, the dev strip is gone, no schema
  change. `hermes-desktop` (Tauri gateway and ACP) unchanged apart from the additive runtime method; its tests run.

Checks after the fixes (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes-native` 340 passed, 2 ignored (composer 58);
`--features snapshots` 343 passed (the six composer baselines regenerated by me after the token change and looked at; no
tracked baseline changed); `scripts/native/check.ps1` "native check: all ok"; `cargo test -p hermes_core` 140 passed;
`cargo test -p hermes-desktop` 130 + 68 passed (Tauri gateway and ACP; webapp dist copied from the main checkout and
removed after); `cargo build --workspace` ok; `scripts/perf/native-thread-check.ps1` ok; `cargo clippy -p hermes-native
--all-targets` clean (`hermes_runtime`'s findings are older than this checkpoint); `clean-test-temp.ps1`: 1 folder
removed, then 0.

Left: the hand-only checks in C (IME, emoji panel, paste, the feel of the menus) are still the user's.

## CP5.2 — Mentions, commands, attachments
Worktree: C:\Users\alexandros\repos\hermes-rs-n5   Branch: native/p5-composer   Model: Sonnet 5.5 (subagent)
Cut from CP5.1 (ba704b3). Uncommitted.

### Files
- New in `crates/native/src/thread/composer/`: `mention.rs` (no GPUI: `trigger` = where the caret asks for a menu, `Suggestion`, `Chip`,
  `reference_chips`, the `/` entries `CommandSpec`/`merged`/`filter_commands`, `AppCommand`), `attach.rs` (no GPUI: limits, `prepare_path` /
  `prepare_bytes`: size and kind checks, decode and shrink to a 256 px PNG thumbnail, duplicate keys, `format_size`), `suggest.rs` (the drawer,
  keys, what a pick does), `strip.rs` (the attachment strip, the chip element), `tests/input.rs` (47 tests).
- Edited: `model.rs` (attachments, thumbnails, `preparing`, catalog, `request_mentions`, command running, `send` split into `send_prompt`),
  `view.rs` (fields, capture-phase key actions, paste/drop/pick, `content_of`, placeholder, paperclip), `data.rs` (`SendRequest.display`,
  `.attachments`), `model/backend.rs` (trait: `complete_path`, `commands`, `run_command`, `prepare_attachment`; `CoreBackend::send` saves
  images through `image_from_base64` and copies files through `Core::attach_file`; the fixture records and scripts all of it),
  `thread/model.rs` (`expect_prompt_with`, `show_user_with`, `RowKind::Command`, `show_command_note`), `thread/view.rs` (`command_row`),
  `workspace.rs` (composer events: `App(id)` runs the frame command), `gallery/composer.rs` + `mod.rs` (3 stories), `Cargo.toml` (`image` is a
  normal dependency with png/jpeg/gif/webp/bmp/tiff), `real_tests.rs`, `wiring_tests.rs`, `thread/tests.rs`.
- Docs: TESTING.md section 6, PITFALLS.md (4 bullets), PHASES.md status, ARCHITECTURE.md and native-ui/README.md rows, reference/t3code/README.md (no new screenshot).

### What it does
1. **`@`.** The word before the caret that starts with `@` (not an email, not inside a chip) opens a drawer above the card (t3code's, over the
   card width, at most 288 px, scrolling, the highlighted row kept in view) filled by the core path completion (`paths::complete`, in the thread folder, or
   the project folder on the new-thread screen). Arrows (wrap), Enter, Tab, Escape (stays away until the word changes), click. A file or
   folder becomes an atomic Kit inline token (deleted by one Backspace, undone as a unit); `@file:`/`@url:` prefixes are typed on; Tab on a
   folder walks into it (`@src/`). The text under the chip is the reference the runtime expands (`@file:src/a.rs`, quoted when it has a
   space): the same text the Tauri page sends, so drafts (`native-ui.json` unchanged), copy and the prompt need no other format; pasted or
   loaded references become chips again; an old plain draft loads as it was.
2. **`/`.** At the start of the text: the core catalog (`Core::commands`: coding commands, skills, bundles) plus the reports
   `Core::run_command` answers but does not list (`/status /context /usage /insights /diff /tools /memory /refine`) plus the app commands `/new /model
   /settings /terminal`, with argument hints (ours) in a bar once the name is typed. Enter runs a command without arguments and completes one
   with; Tab always completes. `Core::run_command` through `Backend::run_command`: `Output` becomes a `RowKind::Command` card in the timeline
   (screen only, the core does not store it; nothing goes to the model), `Send`/`Skill` are sent as a normal prompt with `display` as the user
   row, `NotACommand` (or a name the loaded catalog lacks, or a path) is an ordinary prompt, a refusal keeps the text and toasts. App commands: `/new`
   -> `chat.new`, `/terminal` -> `terminal.toggle`, `/settings` -> `settings.open` (the existing stub toast: Phase 7), `/model` opens the picker.
3. **Attachments.** Paste (a clipboard image or Explorer files, only when there is no text), drop on the card (outline while dragging), the
   paperclip (the system file dialog behind a `FilePicker` seam). Read off the UI thread (`Backend::prepare_attachment`): the core 16 MiB limit
   and image kinds with its sentences as toasts ("huge.bin: The file is larger than 16 MiB."), folders, unreadable and broken files refused,
   duplicates noticed, 100 per message (t3code's; the core has none; review: was 20), photos decoded with a 512 MiB cap and shrunk. Strip above the input: 64 px image
   tiles with a remove button, rows with name and size for other files, wraps and scrolls over 166 px (review: was 152). Sent with the prompt: images saved by the
   core under `images` and sent as `SubmitTurn::images`, files copied into `.hermes/attachments` and referenced with `@file:` appended to the text;
   the user row shows the image thumbnails when its turn starts; attachments-only sends are allowed; a refused send keeps them.
4. Placeholder: "Ask anything. @ files, / commands".

### Tests (CARGO_TARGET_DIR=target/claude)
- Unit: `mention.rs` (12), `attach.rs` (8). Behaviour and layout: `tests/input.rs` 47 (menus, chips, commands, attachments, drawer/chip/strip layout at 640/1100/1920,
  clipboard and `FileDrop` events through the real window), `wiring_tests.rs` +2, `thread/tests.rs` +2 (command card in the lane, live thumbnails).
- Real core + wiremock (`real_tests.rs`, +2): a prompt with `@file:notes.txt`, an attached PNG and an attached text file: the provider request holds the
  expansions and `[Attached image: ...]`, the file is copied into `.hermes/attachments`, the image is saved in `images`, the stored user message has the image,
  the row has one thumbnail; and a broken image is refused with a sentence naming the file.
- Results of the full runs: "Final checks" below.

### Snapshots
New (6), `crates/native/snapshots/`: `composer-mentions-t3-{light,dark}.png` (`@` drawer, chips, chips on a 352 px card), `composer-commands-t3-{light,dark}.png`
(`/` drawer, argument hint, a command answer in the timeline over the composer), `composer-attachments-t3-{light,dark}.png` (images, files, many, narrow).
Changed pre-existing (6): `composer-t3-{light,dark}.png`, `composer-menus-t3-{light,dark}.png`, `composer-new-t3-{light,dark}.png` (the paperclip at the footer left
and the new placeholder; the model chip floor is 64 px); their `.diff.png` were in `target/claude/snapshots/` before the update. I looked at the dark mentions, commands and
attachments sheets; a Haiku check should look at all twelve.

### Deviations
- No t3code capture of the drawer, chip or strip (window not driven; source only): chip colour is the theme `info` (t3code `--context-chip-mention` is not in our tokens). *(Review: now `info_foreground`, ink only, see below.)*
- The drawer is anchored to the card (as t3code), not the caret; no `$ skills` (skills are `/` commands); no tile preview dialog on click; non-image files show in the strip but
  not in the sent row (their `@file:` reference shows when the history is read again); command cards are not stored (as the core output).
- A reference typed by hand stays text until pasted or reloaded (t3code chips it at the next space).
- Attachments are not kept in drafts (a file may be gone tomorrow). Ctrl+Enter with attachments queues instead of steering.
- Model chip floor 96 -> 64 px and the footer gained the paperclip: pre-existing composer baselines changed.

### Missing core operations
`Core::commands` has no argument hints or the reports (`/usage` ...), and `attachments` no count limit: all native-side (`mention.rs`, `attach.rs`).
`Core::attach_file` needs a stored session, so files are copied at send time, not when added.

### For the reviewer
1. `ComposerView::pasting` is consumed only by `Change`; an empty-clipboard Ctrl+V leaves it set until the next edit (harmless: it chips complete references).
2. `sync_trigger` runs on every notify of the input (observe) and copies the text; fine to 1 MB (test: 2.3 s), could read only the caret neighbourhood.
3. `reference_chips` skips texts over 200 kB (regex cost); `content_of` is used by `set_text`, so Restore/Edit get chips too.
4. `CoreBackend::send` saves images and copies files before applying picks and before `submit`: a later refusal leaves the copies behind (`.hermes/attachments`, `images`).
5. `run_command` blocks a GPUI background thread on the core runtime (`block_on`), as `delete`/`move` already do.
6. The command card is not a message: copy gives its text; no actions row.
7. The count limit 20 is a guess; the core accepts any number.

### User checklist (CP5.2), added to the CP5.1 list
Build as in CP5.1 and run `.\target\claude\debug\hermes-native.exe --fixture --dev`.
 1. In "halo" type `@`: a list of files above the card; type `ma`, Down/Up, Enter: a blue chip with the file name; Backspace deletes it whole.
 2. Type `/`: the commands; `/usage` + Enter: a grey card with the answer appears in the thread (nothing is sent); `/diff ` shows the argument hint.
 3. `/new` starts a new thread; `/settings` shows the Phase 7 toast; `/model` opens the model picker.
 4. Click the paperclip, pick a PNG and a text file: a thumbnail and a file row appear; the x removes them; Enter sends: the message shows the thumbnail.
 5. **Hand-only:** copy an image (browser, screenshot tool) and Ctrl+V in the composer; drag files from Explorer onto the card (it gets an outline); the Windows file dialog.
 6. Pick a file over 16 MiB: a toast with the core sentence.

### Final checks (CP5.2)
`scripts/native/check.ps1 -Update`: "native check: all ok" (`cargo test -p hermes-native` 414 passed with the snapshot run, 2 ignored; the 12 baselines above written, no other
baseline changed). `cargo test -p hermes_core`: 140 passed. `cargo build --workspace`: ok (webapp dist copied from the main checkout, then removed).
`scripts/perf/native-thread-check.ps1`: ok. `scripts/clean-test-temp.ps1`: dry run 23 folders (6.2 MB), `-Delete` removed 23, second dry run 0.
Stories were sized after `every_design_story_is_registered_and_fits_its_window` (a scrolling drawer list trips `no_overflow`, so the `/` story shows `/in`).

### Visual check (Haiku 5.5)
References opened: `reference/t3code/composer-model-picker.png`, `composer-model-picker-permission.png`, `app-thread-dark.png`. (No t3code capture of the mention or command drawer exists; those are judged on legibility and layout only.)
- composer-mentions-t3-light: expected — drawer row and blue chips legible; drawer highlight low-contrast on white but readable.
- composer-mentions-t3-dark: differs — chips use theme `info` blue, not t3code's chip tint (declared); drawer row height and icon not checked against a reference.
- composer-commands-t3-light: expected — highlighted `/init` row, argument-hint bar, usage card; the usage card's text is faint grey on near-white but legible.
- composer-commands-t3-dark: differs — command rows show no kbd element; no t3code command capture to compare against.
- composer-attachments-t3-light: unexpected — faint red corner marks on the thumbnail tile edges, and a stray dash above the placeholder in the many-tiles panel; check for a leftover focus or drop ring.
- composer-attachments-t3-dark: unexpected — same red corner marks on the tiles and stray dash above the placeholder in the many-tiles panel.
- composer-t3-light: changed-as-expected — paperclip added at footer left; placeholder now "Ask anything. @ files, / commands".
- composer-t3-dark: changed-as-expected — same paperclip and placeholder change; send disc unchanged.
- composer-menus-t3-light: changed-as-expected — paperclip and new placeholder; narrow card model chip now truncates to "o..." (was "openro...") from the 64 px floor, a consequence of the declared change.
- composer-menus-t3-dark: changed-as-expected — same as light; model list and permissions popup match composer-model-picker.png and composer-model-picker-permission.png except the declared five icons and no Ctrl+N badges.
- composer-new-t3-light: changed-as-expected — paperclip and new placeholder; "Accept edits" label no longer clipped at the card's bottom edge (fixed as a side effect).
- composer-new-t3-dark: changed-as-expected — same paperclip and placeholder; card matches app-thread-dark.png apart from the declared missing "$ skills" and checkout row.

### Review (Opus 5.5)
Verdict: **good after fixes.** Main had not moved (no merge). The mention, command and attachment paths were sound;
the fixes are leftovers after a refused send, a few send-path edges, the visual defects the Haiku check found, and a
footer that could push the send button out of a narrow card.

Checked and right as it was:
- **Mention safety.** The text under a chip is the plain reference (`@file:src/a.rs`, quoted when needed); the
  runtime expands it relative to the session's folder (`hermes_tools::references::resolve`: canonicalised, symlinks
  followed, refused outside the folder or on credential files). The completion root is the thread's folder (the new
  thread's is the project's, the same folder `create_thread` uses). So the native app can mention exactly what the
  Tauri page can; a path outside the folder is offered by completion (as there) but refused by the runtime.
- Stale `@` answers are dropped (`mention_seq`); the core's completion is capped (30 items, 20,000 files). Commands:
  every `CommandOutcome`/`CommandError` variant is handled (`Output` = a screen-only `RowKind::Command`, nothing stored
  or sent; `Send`/`Skill` sent with `display` as the row; `NotACommand` and paths an ordinary prompt; errors a toast
  with the text kept). A `/` mid-text opens nothing. t3code's command rows have no kbd (Haiku item c): nothing to do.
- Attachments: images are checked by magic bytes again by the core at send (`image_from_base64`), files and images go
  only to the data folder's `images` and the thread's `.hermes/attachments` (unique names). Decoding is off the UI
  thread; a tile being read has no remove button, so there is nothing to cancel.
- Ground rules: no view calls the core, tokens only, t3code cited in every new module, `TestDir` last, no schema change.
  `sync_trigger` copies the text per caret move: measured 0.8 ms on a 1.1 MB draft (debug); kept, with a bound test.

Fixed:
1. **Leftovers after a refused send.** `CoreBackend::send` now records every file it writes (`SavedAttachments`) and
   removes them when anything after fails (a later attachment, the mode pick, `submit`). Real-core test: a text file is
   copied, then the image (broken after it was added) is refused: `.hermes/attachments` and `images` hold nothing new,
   the strip keeps both. An image that grew past 16 MiB is refused from its size, not read whole first.
2. **Attachments added while a send was on its way were dropped** (`attachments.clear()` on success): only the sent
   ids are removed now. Restored attachments of a failed first send move the id counter past theirs (ids came from the
   new-thread screen's composer and could collide).
3. A steered command prompt showed the loaded skill's text in the row, and a failed first send gave back that text
   instead of `/name args`: both use `display` now.
4. **Count limit** 20 -> 100: t3code's `PROVIDER_SEND_TURN_MAX_ATTACHMENTS` (`packages/contracts/src/chatAttachment.ts`).
5. **Visual (a), red corners:** not a ring: the tile's `overflow_hidden` clips to its box, not its rounded corners, so
   the thumbnail's square corners showed past the border. The image is rounded itself now.
   **The stray dash:** the top 2 px of the first file row's icon under two rows of tiles (the strip cut at 152 px).
   `MAX_HEIGHT` is 166: the cut shows a third of a tile or most of a row, which reads as "scrolls".
6. **Visual (b), chip colour:** t3code's `ContextChip` is ink only (no fill), its file accent solved to 5:1 contrast per
   theme; the chip now uses `info_foreground` (the palette's blue text at that contrast) with no fill, filled only when
   selected.
7. **Visual (d), footer:** at 352 px the send button stuck out of the card (with the context meter) and the model chip
   read `o...`. Now the model chip gives way first, the mode chip only after it (its label floor 32 = icon), and a chip's
   label shows only with at least 36 px (else the chip is its icon; the tooltip names it): a wrap with an empty first
   item drops a label that needs more onto a clipped second line. Tests: send inside the card at 352-1100, label
   either >= 36 px and centred or hidden, the mode's name whole at a 330 px window.
8. Baselines: the six pre-existing ones changed only by the paperclip, the placeholder and the footer chips (item e,
   compared with HEAD's); re-accepted after the fixes above, with the six new ones.

Left, with reasons:
- The sent row shows non-image files only after a reload: the reference names are made by the core inside the send
  (`unique_path`), and `Sent` does not carry them; adding that is beyond this checkpoint. Declared above.
- Command cards are screen-only (as the core's output); `/ctx` (an alias the core answers) is not in the merged list,
  so with the catalog loaded it is sent as text.
- The hand-only checks (IME next to a chip, real clipboard images, Explorer drag, the Windows dialog) are still the user's.

Checks after the fixes (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes-native` 415 passed, 2 ignored;
`--features snapshots`: the 12 composer baselines differed (all by these fixes, looked at: rounded tiles, the 166 px
cut, ink chips, icon-only model chip at 352 px with the send inside), accepted with `UPDATE_SNAPSHOTS=1`, no other
story changed; `scripts/native/check.ps1` "native check: all ok"; `cargo test -p hermes_core` 140 passed;
`scripts/perf/native-thread-check.ps1` ok; `cargo build --workspace` ok (webapp dist copied from the main checkout,
removed after). Core, runtime and desktop untouched by the review, so `hermes-desktop` was not re-run.
`clean-test-temp.ps1`: 24 folders removed, then 0.

User checklist addition: in a narrow window (about 640 px with the right panel open) the model chip shows only its
icon (hover names the model), "Accept edits" stays whole, the send button stays inside the card.

## CP5.3 — Approvals and questions
Worktree: C:\Users\alexandros\repos\hermes-rs-n5   Branch: native/p5-composer   Model: Sonnet 5.5 (subagent)
Cut from CP5.2 (94f65d9). Uncommitted.

### Files
- New: `thread/composer/ask.rs` (no GPUI: `Choice`, `Approval` + `subject()` command/edit, `Question`, `Request`, `Asks` queue built from the core's events, `Nav` = what each key does,
  `Answer`), `thread/composer/panel.rs` (the drawer: approval and question views, keys, focus), `notify.rs` (`Policy`, `Attention`, `Notifier`, `OsNotifier`: `FlashWindowEx` +
  `tauri-winrt-notification`), tests `thread/composer/tests/asking.rs`, `notify_tests.rs`, `wiring_tests.rs::asking`, `real_tests.rs` (+3), `scripts/perf/native-approval-check.ps1`.
- Edited: `model/backend.rs` (`Backend::answer`; fixture `ask` / `ComposerOp::Answer`), `model/mod.rs` (`AppModel::asks`, `ModelEvent::Waiting` / `Resumed`, composer seeded on open,
  sidebar `asking` mark), `thread/composer/{model,view,mod}.rs`, `thread/model.rs` (`pending_edit`), `thread/cards.rs` (`diff_lines` public), `workspace.rs` (hold + announce, `set_notifier`),
  `ui_state.rs` (`notifications`), `app.rs`/`main.rs`/`dev_send.rs` (`--dev-answer`), sidebar (`ThreadItem::asking`, `RowStatus::Input`, `Status::Input`),
  `design/{button,icons,list_row}.rs` (`Button::hint`, two icons, Input status), `gallery/{composer,sidebar,mod}.rs` (stories `composer-asks`, `sidebar-waiting`), `testing.rs` (`ask-body` scrolls),
  `Cargo.toml` (Windows-only `windows`, `raw-window-handle`, `tauri-winrt-notification`: all already in `Cargo.lock`, no new crate), `scripts/perf/mock_llm.py` ("please ask"), docs.

### What it does
1. **Approval panel** (t3code's composer banner shape: a drawer 12 px narrower than the card, tucked 16 px under its rounded top, warning-tinted): "Command approval" / "File change approval",
   the command in a mono block (max 160 px, scrolls) or, for `write_file` / `patch`, the path and the diff of the *running call* from the timeline (`ThreadModel::pending_edit`, drawn by the tool cards'
   `diff_lines`, 8 lines), "Flagged: <the runtime's reason>", and the choices the core offered: Deny (Esc, 4), Allow for this session (2), Always allow (3), Allow once (Enter, 1); Manual and Plan offer
   once and deny only. Several requests queue, oldest first, "1 of N". Buttons disable while an answer is on its way.
2. **Question panel**: the question, the choices as rows (radio, or checkbox for multi-select) with their digit, a free-text field always (the core adds "Other"; with no choices it is the only way,
   and the cursor starts there), Skip (Esc: empty answer = skipped) and Send. Enter in the field sends text, else the picks.
3. **Keys.** The panel takes the focus when a request appears unless the composer holds text or a menu is open. Panel focused: Enter = allow once (Tauri), Escape = deny / skip, digits, Up/Down/Tab move,
   Space toggles. In the composer's own input: Escape (empty) denies; Enter never approves (deviation from Tauri: an Enter meant for a message must not allow a command). A key is one press; the model ignores
   a second answer while one is in flight, and the next request is a new id.
4. **While a request waits the input stays usable**: a typed message is queued behind the waiting turn as always (t3code: "decline and redirect"; the core has no such operation, recorded). The placeholder says so.
5. **Requests for other threads are kept**: `AppModel` holds every thread's queue; a composer made on open is seeded from it. `Settled`, `TurnEnded` and `Idle` remove; the 300 s runtime timeout arrives as `Settled`.
   Answers go through `ComposerModel::answer` (one in flight per request; `Ok(false)` "no longer open" closes the panel quietly; an error keeps it with a toast).
6. **Sidebar**: the existing status (Phase 3) is now driven from the queue: "Approval" (warning shield), or **"Input"** (new, t3code's) when only questions wait; the folder dot follows; it clears when the queue empties.
7. **Notifications** (`notify.rs`, wired in `workspace.rs`): when a request arrives the frame holds it 400 ms; then, if the thread still waits and (the window is not in front or the thread is not the open one) and
   the thread was not announced for this wait: flash the taskbar (only if the window is behind; `FLASHW_ALL | FLASHW_TIMERNOFG`) and show a Windows toast "<title> / Approve a command: ... (and N more)". Clicking the toast
   opens the thread and activates the window. `notifications` (default on) in `native-ui.json`. No notifier in `--fixture` or with `--dev-answer`.

### Tests (CARGO_TARGET_DIR=target/claude): results in "Final checks"
Unit: `ask.rs` 10, `notify.rs` 4. Behaviour + layout: `tests/asking.rs` 24, `wiring_tests::asking` 5, `notify_tests` 8, `real_tests` +3 (real core, scripted wiremock provider: a question and a flagged `chmod 777` in the test's temp folder,
answered through the composer model, allow and deny; stop closes a waiting panel), sidebar tests adjusted (`ModelEvent` has three variants now).
Live: `scripts/perf/native-approval-check.ps1`, documented in TESTING.md section 6 and `scripts/perf/README.md`.

### Snapshots
New (4): `composer-asks-t3-{light,dark}.png` (approval of a command, of an edit with its diff, question with choices, with free text and an answer typed, a second request queued, a narrow 352 px card),
`sidebar-waiting-t3-{light,dark}.png` (Approval, Input, a closed folder's dot). No pre-existing baseline changed. I looked at the dark composer-asks sheet (crops of the question-text and approval-edit states); the light ones and the sidebar sheets are for the Haiku check.

### Deviations
- **No t3code capture** of an approval or question exists and I did not drive the user's t3code window: built from source (`ComposerBanner.tsx`, `ComposerPendingApprovalActions.tsx`, `TimelinePendingApprovalRow.tsx`, `ComposerPendingUserInputPanel.tsx`) and the Tauri page's card.
  t3code shows an approval as a timeline row with inline buttons and the question as the composer's top drawer; we show both as the drawer (the brief).
- Four buttons visible, not t3code's two plus a "More" menu; "Always allow" has no confirm dialog (Tauri asks, it persists to config): the tooltip says it adds to the allowlist. Reviewer: decide whether it needs the dialog.
- Question text is plain (t3code renders markdown); no per-question header or collapse; one question at a time as the core sends them; no secret masking (the core has no such flag).
- The clarify free-text field is the panel's own, not the composer's input (t3code reuses the composer editor); the composer input keeps the draft.
- The toast is headed "Windows PowerShell" until the installer registers an application id (Phase 8).
- Multi-select answers are joined with ", " (the core splits on commas: a choice containing a comma would split).

### Missing core operations
None blocking. Facts: no diff in `ApprovalRequested` (taken from the timeline's running card, so absent if the call's event was not seen, e.g. a thread opened mid-turn); no "secret" flag on clarify; no "decline and redirect";
`Core::open_questions` exists but is not used (the app model sees every event since it subscribes).

### For the reviewer
1. `ComposerView::sync_ask` decides the focus on each model notify; check it cannot steal focus wrongly and that the `typing` guard (text or a menu open) is enough (an open `@` drawer is not checked).
2. `Nav` multi-select: Enter with nothing picked toggles the highlighted row; Enter with picks sends. Is that the expected feel?
3. `AppModel` and the composer each run `Asks::on_event`; they could diverge only if events were lost. Seeding on open is the only link.
4. Panel `mb(-16)` + `pb(16)` emulates t3code's `-mb-[calc(1rem+1px)]`; the buttons got an extra `pb(8)` so they do not touch the card. Look at the snapshots.
5. `Policy` keys on the thread, not the request: a second request arriving after the first was announced while the thread still waits is not announced again (by design: "never spam").
6. `OsNotifier` cannot be tested headless; `FlashWindowEx` takes the HWND through `raw-window-handle` from the GPUI `Window`; never exercised in a real window by me (hand-only).
7. `Button::hint` is new in the design layer (no story of its own; it shows in `composer-asks`).
8. `testing::no_overflow` now skips everything under a probed `ask-body`.

### User checklist (CP5.3), and the whole Phase 5
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n5`): `$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`, `$env:HERMES_RS_HOME="$PWD\target\native-test"` (synthetic, never `%APPDATA%`).
A. Fixture (`.\target\claude\debug\hermes-native.exe --fixture --dev`): the CP5.1 and CP5.2 lists above still hold; the fixture never asks, so see the panels in the gallery:
   `.\target\claude\debug\hermes-native-gallery.exe` (stories "Composer: approvals and questions" and "Sidebar: threads that wait").
B. With the mock provider (as Phase 4's item 11: a seeded folder, `python scripts\perf\mock_llm.py --port 47900`, the app started with `--dev --dev-provider http://127.0.0.1:47900/v1`, a thread open):
   1. Type `please ask`, Enter. A "Question" drawer appears over the card with "blue" and "green"; the panel has the keys. Press `2`: it closes at once.
   2. A warning-tinted "Command approval" drawer: `chmod 777 live-check-note.txt`, "Flagged: chmod 777", Deny, Allow for this session, Always allow, Allow once. Press Esc: it closes; the reply says `command=denied`.
   3. Repeat and click "Allow once" (or press Enter): `command=ran`.
   4. While it waits, type a message and Enter: it queues above the card and the drawer stays. Escape in the empty input denies.
   5. Open another thread while a request waits in the first: its row shows the shield ("Approval") or "Input"; open it: the drawer is there.
   6. Click Stop while a request waits: the drawer closes.
   7. (Review) Repeat and click "Always allow" (or press `3`): the buttons give way to a sentence "Always allow commands flagged “chmod 777”? ... permanent allowlist ..."
      with Cancel (Esc) and a red Always allow (Enter). Press Esc: back to the four buttons, nothing denied. Then Deny, so nothing is persisted in your test data.
   8. (Review) Press Enter at the very instant the drawer appears (within half a second): nothing happens; a moment later Enter allows. With the sidebar's search focused
      when a request appears, the drawer does not take the keyboard.
C. Automated: `powershell -NoProfile -File scripts\perf\native-approval-check.ps1` (prints `native-approval-check: ok`) and `scripts\perf\native-thread-check.ps1` (now checks the prompt arrived whole).
D. **Hand-only, nobody has done these in a real window:** the Windows toast and the taskbar flash (minimise the window, send `please ask` from another thread, then click the toast: the thread opens and the window comes to the front;
   the toast is headed "Windows PowerShell" by Windows, its title reads "Hermes: <thread title>" and its text "A command needs your approval. Open Hermes to answer." with no command in it);
   Enter held on an approval must not approve the next one (tested headless with a synthetic repeat, not with a real keyboard); IME and the emoji panel in the composer and in the answer field; a real clipboard image paste, a drag from Explorer, the Windows file dialog (CP5.2);
   the narrow footer at about 640 px with the right panel open (CP5.1 and CP5.2).

### Phase 5 Done list, and what covers it
*Layout tests and snapshots for each composer state:* `thread/composer/tests.rs`, `tests/input.rs`, `tests/asking.rs`, `wiring_tests.rs`; snapshots `composer`, `composer-menus`, `composer-new`, `composer-mentions`, `composer-commands`, `composer-attachments`, `composer-asks`, `sidebar-waiting`.
*A mock-provider turn that asks for an approval and a question, answered from the composer in our instance:* `scripts/perf/native-approval-check.ps1` (live, our built app) and `real_tests` (real core).

### Final checks (CP5.3)
`scripts/native/check.ps1`: "native check: all ok" (build with dev, `cargo test -p hermes-native` 469 passed + 2 ignored, snapshots 471; 4 new baselines written by me with `UPDATE_SNAPSHOTS=1`, none of the others changed).
`cargo test -p hermes_core`: 140 passed. `cargo build --workspace`: ok (webapp dist copied from the main checkout, then removed). `scripts/perf/native-thread-check.ps1`: ok.
`scripts/perf/native-approval-check.ps1`: "native-approval-check: ok" (both runs: question answered `blue`, command `ran` / `denied`, panels in the dumps, streamed = stored).
Found on the way: `native-thread-check.ps1` passes `--dev-send 'hello from the live check'` through `Start-Process -ArgumentList` unquoted, so the app sent only "hello" (the check still passed: the mock ignores the text). The new script quotes the prompt; the old one is left as it was.
`scripts/clean-test-temp.ps1` result is in the final report.

### Visual check (Sonnet 5.5)
Opened: t3code source (`ComposerPendingApprovalActions.tsx`, `ComposerPendingUserInputPanel.tsx`, `TimelinePendingApprovalRow.tsx`, `ComposerBanner.tsx`), references `app-sidebar_closeup-dark.png` (status look). No t3code capture of the approval panel exists. Dark and light viewed in per-panel crops.
- composer-asks-t3-dark (command approval): matches (warning-tinted drawer tucked under card, mono block, Deny muted + outline + primary, "Flagged:" reason); expected: Deny/Esc, "Allow for this session 2", "Always allow 3", "Allow once ⏎" aligned on one baseline, primary filled.
- composer-asks-t3-dark (file change): differs — t3code shows buttons inline in a timeline row, we show a drawer (documented); expected: red/green diff with line numbers readable, "Flagged: file edit", two buttons.
- composer-asks-t3-dark (question, choices): expected — radio rows with digit chips right-aligned, free-text field, Skip Esc, Send (dimmed while empty, reads as disabled); (free text): expected.
- composer-asks-t3-dark ("1 of 2"): expected — top right, low contrast but legible.
- composer-asks-t3-dark (narrow 352 px): unexpected — buttons wrap to two ragged right-aligned rows (Always allow starts left of Allow for this session; Deny sits beside row one only); model label collapses to icon (as in composer-new narrow, expected). Legible, no clipping.
- composer-asks-t3-light: expected in all six states — orange tint with dark orange text/shield, diff tints legible, white mono block, primary contrast fine; disabled Send is pale but readable; same narrow wrap as dark (unexpected, see above).
- sidebar-waiting-t3-dark: differs (minor) — t3code's "Done" is a green check-circle; ours "Input" is blue help-circle and "Approval" amber shield (new, no capture); expected: icon + text right-aligned to the same edge as "Working" and the time marks, folder dot "3" amber.
- sidebar-waiting-t3-light: expected — Input blue, Approval dark orange, contrast good, aligned.
- composer-t3, composer-menus-t3, composer-new-t3, composer-mentions-t3, composer-commands-t3, composer-attachments-t3 (dark and light): expected, nothing unexpected seen (checked at contact-sheet scale).

### Review (Opus 5.5)
2026-11-25. `main` had not moved (nothing to merge). Verdict: **good shape, safety gaps fixed; Phase 5 done.**

**Approval safety (fixed).**
1. *Keys not meant as answers.* The panel took the keyboard whenever the composer's input was empty, from anywhere (the sidebar's search, a
   dialog), and answered any key at once; a held Enter's auto-repeat answered the next request appearing under it (Tauri's `approval-keys.ts`
   ignores repeats). Now: a held key (`KeyDownEvent::is_held`) never answers (arrows may repeat); for `panel::KEY_GUARD` = 500 ms after a request
   appears its answer keys (Enter, Escape, Space, digits) are swallowed, by the executor's clock so tests drive it; the panel takes the focus only
   from the composer's own empty input, its previous panel/field, or nothing. Clicks are not guarded (t3code and Tauri do not either).
   Tests: `a_key_in_the_first_moments_of_a_request_answers_nothing`, `a_held_enter_does_not_approve_the_next_request`,
   `the_panel_does_not_take_the_keyboard_from_outside_the_composer`. Enter in the composer's input never approves (existing test, kept).
2. *What is shown is what runs.* The command, the edit's path and the reason were drawn raw: an ANSI escape, `\r`, a right-to-left override or a
   zero-width character could hide or reorder the tail. `ask::visible` escapes control, bidi, zero-width/format characters (`\x1b`, `\u{202e}`),
   keeps newlines (multi-line commands show as lines; the block was already scrollable, max 160 px, so no truncation); a warning line says when
   something was escaped or when the text holds non-ASCII characters (lookalikes). Unit test + `hidden_and_control_characters_in_a_command_are_shown_escaped`
   + snapshot state `hidden-chars`.
3. *Edit diff.* The request carries no tool-call id, so `ThreadModel::pending_edit` matches the *running* card by tool and path (as the core asks it):
   the best available; a finished or other-path card never matches. When none matches the panel showed nothing; it now says
   "The diff of this change is not available here; only the path is known." (state `edit-no-diff`, test extended).
4. *"Always allow".* Checked the runtime (`SessionTools::check_command`): "always" persists the **reason** (`description` before the taint
   suffix, e.g. `chmod 777`, `recursive delete`) to the permanent allowlist file, for every thread from then on; "session" remembers the same key
   in that session's tools only (= this thread); Manual/Plan (`once_only`) offer neither and honour neither. That is broad, and the Tauri page puts
   it behind a confirm dialog ("Always allow this command?", "adds the pattern to your permanent allowlist ... won't ask again ... in this session or
   any future one"). Added the same step in the drawer: the button or `3` (or Enter on it) replaces the buttons with that sentence (our store's
   wording: `panel::always_words`) and Cancel (Esc: back, not a denial) / a destructive "Always allow" (Enter or click). `Nav::confirm_always`;
   tests updated (four buttons, digits) and `always_allow_says_what_it_persists_and_cancel_goes_back`; state `always-confirm`.
   `--dev-answer always` answers through the model and skips it (dev only).
5. Answer routing was right: answers go by `prompt_id`; one in flight per id; `Ok(false)` closes quietly; `Settled`/`TurnEnded`/`Idle` close; out-of-order
   answers and answers after `Settled` reach nobody (existing tests). Deny/Escape = `deny`, Skip = empty answer.

**Notifications (fixed).** The toast is built by `tauri-winrt-notification` through the XML DOM (`SetInnerText`), so `<`, `&` and quotes cannot
inject markup (checked its source, 0.8.1). But its text held the first 100 characters of the command (a command line may carry a token; toasts show
on the lock screen and stay in the notification centre) and its title was the bare thread title under the "Windows PowerShell" app name. Now the
title is "Hermes: <thread title>" and the text says only what waits ("A command needs your approval. Open Hermes to answer."; never the command,
path or question). `Toast::show` (COM/WinRT) moved off the UI thread to a short-lived thread; `FlashWindowEx` is a plain call. Policy (400 ms hold,
once per wait, focus/open-thread rules, settled-before-hold makes none, `notifications` default on, malformed falls back) reviewed: fine. The
"Windows PowerShell" app id stays until Phase 8 (installer), recorded.

**Visual check items.** (a) Narrow wrap: the four buttons are now two pairs that never split, Deny + "Allow for this session" and "Always allow" +
"Allow once", so a narrow card shows two right-aligned rows with the primary bottom-right (test `on_a_narrow_card_the_buttons_wrap_as_two_right_aligned_pairs`;
one row on the wider cards, as before). (b) t3code's `ComposerPendingApprovalActions` is used only by `TimelinePendingApprovalRow` and
`ChildThreadApprovals` (grep): t3code's composer shows no approval panel, so the drawer stays a documented deviation; its words (Decline / Approve /
"Always allow this session" in a More menu) are Codex's decisions, ours are Hermes's four (the Tauri page's labels), kept. (c) Sidebar: t3code's
pills are "Pending Approval" amber and "Awaiting Input" indigo, dot + text (`Sidebar.logic.ts::resolveThreadStatusPill`). Ours keep Phase 3's
icon + short label; Approval is amber (`warning_foreground`), aligned; Input is `info_foreground` blue (the same hue as Working, a different
icon): there is no indigo token and adding one changes the token sheet's snapshot and every theme mapping, left for the theme work (Phase 7).
(d) "1 of N" now foreground at 80%, medium weight (was muted on the warning tint).

**Live checks.** `native-thread-check.ps1` did send only "hello" (`Start-Process -ArgumentList` joins without quoting): the prompt now carries its
quotes, `dev_send` prints `sending to <id>: "<prompt>"`, and the script fails unless the whole prompt arrived (PITFALLS bullet).

**Other checks.** Views never call the core (grep of `panel.rs`, `view.rs`, `ask.rs`, `notify.rs`); no raw colours in the new code; no Zed code; no
new crate (`windows`, `raw-window-handle`, `tauri-winrt-notification` were in `Cargo.lock`; `--locked` build ok); no store schema change; the core
and runtime were not changed (so no `hermes-desktop` run needed). The runtime never asks a secret through clarify (no such request type), so an
unmasked answer field is acceptable. Pending questions live in memory only: after a restart there is no ghost panel (the runtime's turn is gone
too). `AppModel::asks` is bounded by the runtime's open requests (each ends in `Settled`, a turn end or idle).

**Left as is (and why).** Question text is plain (not markdown) and not escaped (the agent's own words, not something that runs). Clicks are not
time-guarded. The diff is matched by tool+path (no call id in the core's event: a core change, not this checkpoint). Input's colour (above).
`OsNotifier` and the real keyboard repeat remain hand-only (checklist D).

**Final checks (review).** `cargo test -p hermes-native`: 477 passed + 2 ignored. `scripts/native/check.ps1`: "native check: all ok" (snapshots:
only `composer-asks-t3-{dark,light}` changed, by this review: the pairs, "1 of N", three new states; looked at in crops, both themes, then
accepted). `cargo test -p hermes_core`: 140 passed. `cargo build --workspace --locked`: ok (webapp dist copied from the main checkout, then
removed). `native-approval-check.ps1`: ok (question `blue`, command `ran` / `denied`, the whole prompt arrived). `native-thread-check.ps1`: ok,
now with `sending to ...: "hello from the live check"`. `scripts/clean-test-temp.ps1`: 11 folders, 2.8 MB, removed.
