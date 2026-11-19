# Phase 4 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-n4   Branch: native/p4-thread

## CP4.1 — Timeline and markdown
Worktree: C:\Users\alexandros\repos\hermes-rs-n4   Branch: native/p4-thread   Model: Opus 5.5 (subagent)
Cut from `main` at 153c1d0. Uncommitted. Resumed once after a rate-limit stop (state re-checked
against the brief, then finished).

### Files
- New `crates/native/src/thread/`: `mod.rs` (the `OpenFileInPanel` action), `data.rs` (no GPUI:
  `HistoryItem`/`HistoryPage`, `items_from_entries` over `hermes_core::transcript::entries`, paging,
  `file_target`, `duration_label`, `tool_summary`), `model.rs` (`ThreadModel`: rows, history pages, the
  live turn, the virtual list's state), `view.rs` (`ThreadView`), `markdown.rs` (Kit `TextView` set-up:
  code header, alerts, sideways tables, file links; the test parse counter `probe`), `recording.rs`
  (the recorded turn, its replay, and the recorder test), `tests.rs`.
- New `crates/native/fixtures/`: `thread-session.json` (a turn recorded through the real core),
  `thumb-sunset.png`, `thumb-stripes.png` (synthetic 160x120 images for thumbnails).
- New `gallery/thread.rs` (stories `thread`, `thread-live`).
- Edited: `model/backend.rs` (`Backend::history`, `Backend::submit`, `CoreBackend` with
  `--dev-provider`, `FixtureBackend` histories, recorded pages and sends, replay on send),
  `model/fixture.rs` (histories), `model/mod.rs` (`AppModel` makes the open thread's `ThreadModel` and
  forwards its events), `workspace.rs` (the thread view in the main area, `OpenFileInPanel` opens the
  right panel), `app.rs`/`main.rs` (`--dev`, `--dev-provider`), `design/icons.rs` (6 icons),
  `testing.rs` (`no_overflow` ignores rows inside `timeline-list`), `gallery/mod.rs`, `lib.rs`,
  `Cargo.toml`/`Cargo.lock` (`base64`, `hermes_llm`; dev `wiremock`: all already in the lock).
- Docs: AGENTS.md (crates/native row), ARCHITECTURE.md, TESTING.md §6 (thread paragraph),
  PITFALLS.md (5 bullets), PHASES.md (status), native-ui/README.md (module table),
  reference/t3code/README.md (two rows corrected, no new screenshot).

### What it does
1. **Timeline.** GPUI Kit's `MessageScroller` (GPUI `list`, tail following) with one row per item, in
   t3code's content lane (46rem = 736 px, centred). History comes from the store through
   `Backend::history` (`store().load_messages` → `transcript::entries` → items; reasoning split off
   before its reply; a user message's images decoded from the stored base64), newest page (60 items)
   first; when the list's top comes within 8 rows of the first loaded row, the page before it is read and
   prepended (`MessageScrollerState::prepend` keeps the reading position).
2. **Messages.** User: right-aligned bubble (`message_surface`, 16 px radius, 12 px padding, max 80%),
   images as a 2-column grid of 101x76 thumbnails (rounded, bordered, tooltip with the name) above the
   text. Reply: markdown, full lane width. Both show the copy button and the time on hover (t3code's
   footer). Markdown extensions (`markdown.rs`): code block header (language + copy, t3code's code
   colours), GitHub alerts (`NOTE`/`TIP`/`IMPORTANT`/`WARNING`/`CAUTION` as Kit block surfaces with
   t3code's icon, label and colours), tables scroll sideways, file links (`path`, `path#L12`, `path:12`,
   `C:\..`, `file://`) dispatch `OpenFileInPanel` (the workspace opens the right panel, writes "File to
   open: path:line" in it and a toast: the file view is Phase 6); web links open in the browser.
3. **Streaming.** `CoreEvent`s for the open thread reach its `ThreadModel` (forwarded by `AppModel`):
   text and reasoning deltas are pushed onto the last row's `TextViewState` only (Kit re-parses only the
   last block with the appended text) and only that row is re-measured; a tool call is a placeholder
   line (`read_file  notes.md`, spinner until done; CP4.2 builds the cards); statuses and errors are
   notice rows; `TurnEnded` closes the reasoning's duration and leaves the streamed rows as they are.
   Bottom pinning is the list's tail following; scrolled up, an outline "Jump to latest" button sits at
   the bottom and resumes following. **Spike costs fixed:** rows are a `Vec` the model owns (no
   `Rc<Vec>` for `make_mut` to copy when a message finishes), and a streamed row keeps only its
   `TextViewState` (no second `String`; "copy" reads Kit's source).
4. **Reasoning** rows: brain icon, "Thought for 2.8s" (live: "Thinking"; from history: "Thought", the
   store keeps no duration), chevron; collapsed by default, a click opens the muted markdown body.
5. **Selection and copy.** Kit's selection within a message and within code (Ctrl+C), the hover copy
   button (the message's markdown), the code header's copy button (the code alone).
6. **Wiring.** Selecting a thread in the sidebar (or the palette, or a toast) makes the `ThreadModel`
   and the workspace makes its `ThreadView` in the main area; closing, archiving or deleting it drops
   both; `NoActiveThreadState` stays when nothing is open. Views never call the core: `ThreadModel` asks
   the `Backend`.
7. **Dev only.** `hermes-native --dev` adds a "DEV ONLY" strip with an input under the open thread;
   Enter sends through `Backend::submit` (`Core::submit` with `NoHooks`, the thread's provider from
   Settings, or `--dev-provider <url>`, e.g. `scripts/perf/mock_llm.py` at
   `http://127.0.0.1:47900/v1`). With `--fixture --dev` a sent prompt replays the recorded turn.

### Tests (all `CARGO_TARGET_DIR=target/claude`)
- `cargo test -p hermes-native`: 191 passed, 2 ignored (Phase 3's copy and seed helpers). New, 29:
  - `thread::data` (5): entries to items with images by row, paging, file links vs web links,
    durations, tool summaries. `thread::markdown` (2): link actions, alert colours.
  - `thread::recording` (2): **`the_recorded_session_is_what_the_real_core_sends`** (real `Core` on a
    `hermes_testdir` folder, `wiremock` provider streaming reasoning, text, a `read_file` call, then the
    final markdown in 9 chunks; asserts every kind is present and the events equal the fixture file;
    deterministic over 3+ runs), and replay round trip.
  - `thread::tests` (20): bubble and reply against the lane + bubble colour = `message_surface`; code
    header (lang and copy on one row, at the right, within 36 px; copy copies only the code);
    **2,000-message thread paints 1..60 rows** (asserted <60) and opens at the newest row; **older
    pages** load on wheel scroll-up (pages asked: `None, 140, 80, 20`, all 200 rows, oldest first);
    **pinning + jump to latest** (follows 40 deltas, the streaming row inside the timeline; wheel up →
    not following, button present; click → following, button gone, row back in view); **tail-only
    parse** (`markdown::probe`: after 6 paragraphs, 30 deltas into the 6th parse only the 6th, the 5
    earlier ones zero times); **finishing copies nothing** (300-row thread: every earlier row's text is
    the same allocation after a stream + `TurnEnded`, the streamed row keeps the same `TextViewState`
    entity, its copy text is the streamed source); recorded turn replays into user / reasoning (360 ms,
    closed) / reply / tool done / final reply; reasoning closed with "Thought for 2.8s", opens and closes
    on click; live reasoning duration 2,800 ms from first delta to the reply; copy message (reply as
    markdown = the fixture's source, user message); **selection** in prose and in code
    (`#[gpui_kit::test]`, see deviations); dev prompt (Enter sends, user row shown, failure → notice);
    notices for status and error; events of another thread ignored; thumbnails (2, side by side,
    101x76, inside the bubble); **sidebar → thread wiring** through `AppModel` and `Workspace` (thread
    view inside `main`, a turn's delta lands in it, `OpenFileInPanel` opens the panel with
    `src/stream.rs:42`, closing drops model and view); **`CoreBackend::history` on a real core** (143
    items paged 83..143 then 0..60, image bytes decoded equal, reasoning before reply, the session not
    opened in the runtime); the streaming timing below.
- `cargo test -p hermes-native --features snapshots`: all pass after accepting the 4 new baselines.
- `scripts/native/check.ps1`: "native check: all ok".
- `cargo clippy -p hermes-native --all-targets --features snapshots --no-deps`: clean. `cargo fmt -p
  hermes-native`: applied.
- `cargo test -p hermes_core`: 139 passed. `cargo build --workspace`: passes (webapp dist copied from the
  main checkout's `target/hermes-src`, as in earlier phases).
- Live: `hermes-native.exe --fixture --dev --quit-after 6 --dump` with `HERMES_RS_HOME=target\native-test`:
  opened, dumped, exited 0 (dump deleted afterwards).
- `scripts/clean-test-temp.ps1`: dry run 1 folder (0.0 MB), `-Delete` removed it, second dry run 0.

### Snapshots
New (4), in `crates/native/snapshots/`: `thread-t3-light.png`, `thread-t3-dark.png` (reply with
reasoning closed, reasoning open, user message with two images + primes code, the rich reply: table,
code, list, user message, failed turn), `thread-live-t3-light.png`, `thread-live-t3-dark.png` (the
recorded turn whole, cut mid-stream inside the code block, ending in a provider error, a 200-row thread
scrolled up with "Jump to latest"). Pre-existing baselines: none changed (git shows no modified file in
`snapshots/`). I looked at the dark sheets and one light state, downscaled: the code block matches
`app-codeblock_closeup-dark.png` closely (near-black block, highlighted Python, language + copy at the
top right); nothing clipped. The Haiku visual check should look at all four.

### Streaming / performance (debug build, headless, real DirectWrite text, no GPU)
`thread::tests::streaming_into_a_two_thousand_row_thread_stays_cheap_per_frame` (200 deltas, each
followed by `run_until_parked` + a full draw; `--nocapture` prints it):
- 20-row thread: mean 15.2 ms, p95 19.9 ms per delta+frame.
- 2,000-row thread: mean 15.3 ms, p95 20.1 ms, max 20.5 ms; rows painted at the end: 1 (the reply fills
  the viewport). An earlier run of the 2,000 case alone: mean 24.5, p95 36.3, max 98 ms (machine noise).
- So a frame's cost does not depend on the thread's length; the assertion is `mean(2000) < 3 x
  mean(20) + 10 ms`. Allocations were not counted (no counting allocator); the structural checks above
  (same allocations, same entity, tail-only parse) stand in for it. Release-build numbers: not measured.

### Deviations
- **Code block header**: Kit places code-block actions at the top right only, so the language label sits
  beside the copy button; t3code has a language icon at the left and wrap + copy at the right. No wrap
  toggle.
- **Reasoning durations** from history are unknown (the store keeps none): "Thought". Live ones are
  measured from the first reasoning delta to the next non-reasoning event.
- **History paging** is by item index over the whole stored transcript: `CoreBackend::history` reads all
  messages from the store for every page (the store has no ranged read; schema frozen). Fine for
  thousands of messages; a cache is the next step if a very long thread is slow to open.
- **One open thread model**: events of a thread that is not open are not kept; opening it later shows
  what the store has (a turn still running elsewhere shows its stored part, then new deltas).
- **Selection test under `TestAppContext`** (fake text): under `headless_app()` (real text) a drag
  selects nothing; recorded in PITFALLS.
- **No status event in the recording** (compaction/fallback cannot be triggered by a small mock turn);
  notices are covered by a synthetic-event test.
- **No new t3code screenshot** (no way to drive t3code's window here); light theme, alerts, tables,
  attachments follow the source; noted in the reference README.
- Jump-to-latest is our outline design `Button` ("Jump to latest" with an arrow), not Kit's built-in
  round icon button (deterministic id for tests).

### For the reviewer
1. `Body::text` uses Kit's doc-hidden `gpui_kit::base::text::Text::get_text` to read a streamed row's
   source (the alternative is keeping a second copy, which the brief asked to remove). Watch it on a
   GPUI Kit upgrade.
2. `markdown::extensions()` always registers a block parser that returns `None` (it records parses only
   under `cfg(test)`): one closure call per block per parse in release. Cheap, but it exists only for the
   test.
3. `AppModel::on_event` forwards to the thread model before its own handling; `TurnEnded`'s `Outcome::Error`
   shows `end.error`, else `end.text` when nothing streamed. Check this against the Tauri page's error
   card wording (we show the runtime's account as is).
4. `CoreBackend::submit` uses `NoHooks` (no title model, so a new session gets no auto-title from the dev
   strip) and `Core::turn_options`; Phase 5 should bring proper hooks. The dev strip is only built with
   `--dev`.
5. History rows are stateless Kit `TextView::markdown`s keyed `("message", key)`: parsed when they scroll
   into view (sync for < 4 KB, async above: a big message may pop in a frame later). Scrolling smoothness
   over 2,000 real rows is for the user's hands.
6. The scrolled-up story scrolls by calling `scroll_to_item` from inside the story's render once (a flag
   in window state): fine for a story, not a pattern.
7. `testing::no_overflow` now also skips rows inside `timeline-list` (as `threads`).
8. Thumbnails decode whatever image format the stored MIME names; unknown MIME types are skipped.
9. `with_open` (stories) still does not make a thread model; only `AppModel::open` does.

### Visual check (Haiku 5.5)
- thread-t3-dark: matches | differs — user bubble is dark grey, right-aligned, rounded; reasoning row collapsed with brain icon and chevron; code header shows language and copy icon. Differs: no wrap-toggle button in the code header (t3code shows one); header is smaller.
- thread-t3-light: expected — bubble light grey, text legible, tables and code clipped nothing.
- thread-live-t3-dark: matches | differs — alert, table and code layout match; "Jump to latest" sits over the bottom of the last code block (expected: floating button).
- thread-live-t3-light: expected — alerts, links, error text in red legible; no overlap apart from the floating button.

### Review (Opus 5.5)
Fresh reviewer, same worktree; `main` had not moved (no merge). Verdict: **good, with five fixes
made**; nothing found that needs the implementer back.

Findings and fixes:
1. **Unsafe links (fixed, security).** Every link that was not a file went to `cx.open_url`, i.e. the
   Windows shell: `ms-msdt:`, `search-ms:`, `ms-settings:`, `file:C:/x.exe` (one slash: not caught by
   `file_target`) or a bare `#anchor` from the model's text would have been handed to `ShellExecute`.
   `markdown::link_action` now opens only `http(s):`/`mailto:` (and `www.` as `https://`), sends file
   paths to the panel and ignores the rest (`LinkAction::Ignore`); unit test lists the schemes.
   Remote images in markdown fetch nothing (the app sets no HTTP client); `file://` images are not
   granted file access by Kit. Phase 6's file view must show a linked file, never run it (PITFALLS).
2. **A turn opened in the middle was never reconciled (fixed).** Deltas that went by before the thread
   was opened are lost, and nothing replaced the half reply when the turn ended (until the thread was
   reopened); a turn that started before the first page was read could double its prompt. The model now
   marks such a turn `partial` (no `TurnStarted` seen, or seen before the history was `Ready`) and on its
   `TurnEnded` reads the newest page again in place of the rows before the end's notice (`Load::Reload`;
   a generation counter drops an older page still in flight, and no older page is asked for until it
   lands). A turn seen whole is untouched (no extra read; asserted). Replays (gallery) never reload.
   Test `a_turn_seen_from_the_middle_is_read_back_from_the_store_when_it_ends`.
3. **Theme change left stale row heights (fixed).** GPUI's list re-measures on a width change by
   itself, not when fonts change; themes can change fonts and sizes. `ThreadView` observes the theme
   global (`theme::Active`, now `pub(crate)`) and calls `remeasure()` (proportional anchor kept).
4. **Parse counter in production (fixed).** The block parser is now registered only under `cfg(test)`;
   release parses with alerts and the parser revision alone. Tail-only parsing is still proven by
   `streaming_parses_only_the_last_block_again`.
5. **History page cost (measured, part fixed).** `CoreBackend::history` on a stored 2,000-message
   thread (1,000 turns of ~750 chars, 10 images of 300 KB), debug build: **~520 ms per page**, of which
   `load_messages` ~230 ms; the rest was mostly base64-decoding *every* image of the thread for each
   page. Now only the page's images are decoded (`history_page`; items are built twice, cheap). Still a
   whole-thread read per page, on a background thread: fine for opening a thread (one read), slow-ish
   when scrolling to the top of a very long one (~33 reads). **Follow-up, not now:** a ranged/cached read
   in `hermes_core` (the store's schema stays frozen; a `LIMIT/OFFSET` read method is additive).
6. **"Jump to latest" over the code block (decided: by design, restyled).** t3code's own button
   (`TimelineOverlays.tsx`, `ScrollToEndButton`) floats over the rows too; ours now follows it: "Scroll
   to end", chevron, `xs`. Baselines `thread-live-t3-{light,dark}` re-accepted after looking at both
   crops (only the button changed). The code header's language label is set mono explicitly (`font-mono
   text-2xs`; no pixel change: it inherited the block's mono font). **Deferred:** the wrap toggle (Kit's
   code blocks always soft-wrap, which is t3code's default; a no-wrap mode needs a Kit change) and the
   language at the left (Kit places actions top-right only; see PITFALLS).
7. **Tests added for promised behaviour:** an older page put in front leaves the row in view at the same
   pixel (`an_older_page_does_not_move_the_rows_in_view`); 30 px up stops following, wheeling back to the
   bottom follows again and hides the button (in the pinning test).

Checked and fine: deltas touch only the last row (`index_of` finds it at the end; one
`remeasure_items`; no notify of `AppModel` per delta, so the sidebar does not re-render); `render_row`
clones only the visible rows' `Row`s (Arc-backed strings); no O(n) per delta left. The 15 ms debug
figure is a whole headless frame (layout of a growing reply), not thread-length dependent: the 20 vs
2,000-row comparison is the meaningful part. Page edges (`page(before = start)`) neither drop nor repeat
items; stored prefixes are stable (rows are appended; a compaction appends its handoff) except after a
rewind/retry (`Runtime` `replace_messages`), which the native UI cannot do yet: whoever adds retry
(CP4.2's turn footer) must reload the thread model afterwards. Copy message on a streamed row is
exactly the pushed markdown (`finishing...` and the replay test compare with formatting). Views do not
call the core (`thread/view.rs` uses no `hermes_core`); `--dev` strip and `Backend::submit` are reachable
only with `--dev`; no store schema change; no Zed code outside gpui-kit; `TestDir` fields last.

**Selection in a real window: not verified.** The thread view sets selection up exactly as the CP0.1
spike did (`TextView::markdown(id, text).selectable(true)` per message inside Kit's `MessageScroller`;
streamed rows `TextViewState::markdown(..).selectable(true)`), plus code actions, alerts and link
handlers, none of which takes the pointer. The spike's own selection test also ran under
`TestAppContext`, and phase-0.md records mouse selection as "needs user", not as hand-checked. I tried
to drive our own `--fixture` instance with Win32 messages (`SendMessage`/`PostMessage` of mouse and F12
key messages): no effect could be observed (PITFALLS), and `SendInput` would take the user's pointer, so
I did not. The user's checklist item 7 is the check.

Left: release-build timings; a snapshot of a status ("Compacting") and a "Stopped." notice (unit-tested,
not in a baseline); the hover footer's branch/reply icons (CP4.2's turn footer).

### User checklist (CP4.1)
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n4`):
`$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`, then
`$env:HERMES_RS_HOME="$PWD\target\native-test"`.
1. `.\target\claude\debug\hermes-native.exe --fixture`: click "halo" in the sidebar: the main area shows
   "halo" in a grey bubble at the right, "Thought for 2.8s >" and "Halo! How can I help you today?".
2. Click "Thought for 2.8s": the reasoning opens in grey, indented; click again: it closes.
3. Open "ayo": a bubble with two small images above the text (hover one: its name), then the primes code
   with "python" and a copy icon at its top right. Click the copy icon, paste in Notepad: only the code.
4. Open "Fix the sidebar width": the yellow "Warning" and green "Tip" alerts, a wide table (narrow the
   window: it scrolls sideways, the page does not), a code block, a red "Your request was not
   processed..." line at the end.
5. In that reply click the `workspace.rs` link: the right panel opens and says "File to open:
   crates/native/src/workspace.rs:89", a toast says the file view arrives in Phase 6. The "t3code
   source" link opens your browser. (Negative, from the review: a `javascript:` or `ms-settings:`
   link in a reply opens nothing; covered by a unit test, no fixture has one.)
6. Hover a reply: a copy icon and the time appear under it; click copy, paste: the reply's markdown.
7. Drag-select words in a reply and inside a code block, Ctrl+C, paste: the selected text. **Not
   verified in a real window by anyone yet** (tests run it on fake text only; see the review). Also
   try it in a reply that is still streaming (`--fixture --dev`) and after a reasoning row was opened.
8. `--fixture --dev`: open "halo", type in the "DEV ONLY" strip at the bottom, Enter: your text appears
   at the right, then "Thinking", a reply, a `read_file notes.md` line and a long markdown reply stream
   in, the list staying at the bottom.
9. Do it again and scroll up with the wheel while it streams: the list stays where you put it and a
   "Scroll to end" button appears; click it: back at the bottom, following. Scroll up a little again,
   then wheel back down to the very bottom: the button goes and the list follows again.
10. Negative case: with `--fixture --dev`, the strip's Enter on an empty input sends nothing.
11. Real core (optional): seed a folder as in Phase 3's checklist, run `python scripts/perf/mock_llm.py`
    (`~/.local/bin/uv.exe run --no-project scripts/perf/mock_llm.py`), then
    `.\target\claude\debug\hermes-native.exe --dev --dev-provider http://127.0.0.1:47900/v1` with
    `HERMES_RS_HOME` at the seeded folder: send a prompt in a thread: the mock's markdown streams in;
    close and reopen the thread: the same reply comes back from the store.
