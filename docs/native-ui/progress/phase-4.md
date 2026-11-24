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

## CP4.2 — Tool calls and the work log
Worktree: C:\Users\alexandros\repos\hermes-rs-n4   Branch: native/p4-thread   Model: Sonnet 5.5 (subagent)
Uncommitted, on top of CP4.1 (aeb5428). Resumed once after a rate-limit stop; the worktree was re-checked
against the brief and finished.

### Files
- New in `crates/native/src/thread/`: `tools.rs` (no GPUI: `ToolCard` from a call's name, args and result;
  `Output`, `Detail`, `ChangedFile`, the changed-files tree), `ansi.rs` (ANSI to styled lines), `diff.rs`
  (mini diff: unified-diff parser, `similar`-based diff of two strings, counts as t3code spells them),
  `work.rs` (`WorkGroup`, `ToolEntry`, group header summary, `Footer`, `Usage`), `cards.rs` (the rows
  drawn: group, card per kind, output list, diff, subagent tasks, footer and its tree), `tests/tools.rs`.
- Edited: `model.rs` (rows `Work` and `Footer` replace the placeholder `Tool`; grouping, tool events,
  progress, subagent events, usage, footer, toggles, Retry, Branch, `restart`), `data.rs` (`HistoryItem::Tool`
  carries id, args, result, time; `TurnFooter`), `view.rs` (dispatch, 1 s ticker while a group counts),
  `recording.rs` (`Recorded::Subagent`, `Usage`, rows on `TurnEnded`; the recorder runs a turn with a call of
  every tool kind), `mod.rs` (`OpenDiffTab`, `OpenScreen` actions), `model/backend.rs` (`Backend::retry`,
  `Backend::branch`, `FixtureBackend::thread_ops`), `model/mod.rs` (opens a branch once listed),
  `workspace.rs` (answers the two actions with the panel and a toast), `design/icons.rs` (5 icons),
  `gallery/thread.rs` + `mod.rs` (6 stories), `fixtures/thread-session.json` (regenerated), `Cargo.toml`
  (`similar`; dev: `hermes_tools` for YOLO in the recorder).
- Docs: AGENTS.md row, ARCHITECTURE.md, TESTING.md §6 (work log paragraph), PITFALLS.md (4 bullets),
  PHASES.md (status), native-ui/README.md (module table), reference/t3code/README.md (no new screenshot).

### What it does
1. **Cards** (`tools.rs` + `cards.rs`), from the real event and stored shapes (`tools.rs` header lists them):
   command (`$ command`, exit code when not 0, duration, output list with ANSI colours, cwd, notes such as
   timeout / background / saved output file), file edit (`patch`: counts and diff from the result's unified
   diff, or from the arguments while it runs; `write_file`: all lines added; "Open diff" action), read and
   search (one line + count; the result opens as plain text), web (`web_search` and `web_extract`; a page
   blocked or failed makes the card failed with the reason), delegate (tasks as collapsible rows with their
   tools and summary; live from `Subagent` events, from `tool_trace` when stored), memory and skill write
   (the tool's message + an "Open ..." button = `OpenScreen`), generic (name, args, result as they are).
   Failures: `success: false` / `error` in the result, or a plain-text answer from one of our own tools.
2. **Work group.** Tool calls with nothing between them are one row: header "Worked for 1m 12s · 8 steps"
   ("Working for 12s" while one runs, redrawn each second), a failure mark when the latest call failed,
   a chevron; opened, the calls as lines (a max-368 px list, last 150 drawn). One call is the card itself.
   Stored runs are grouped when a page is turned into rows and joined across page edges.
3. **Turn footer.** Changed files as t3code's tree (folders first, single folders joined, natural sort,
   `+N -M`, collapse-all, "Open diff" per file and for all), the turn's usage and duration (live turns),
   Retry and "Branch from here". Live turns push it at `TurnEnded`; from the store, a turn that changed files
   gets one, and the newest answered turn always (it carries the actions).
4. **Retry / Branch.** `Backend::retry` = `Core::rewind_to_row(prompt row)` then `submit` of the typed text;
   the thread model then reads the thread again from the store. `Backend::branch` = `Core::branch(ThroughRow(last
   reply))`; `ThreadEvent::Branched` makes `AppModel` list and open the new thread. Failures are error
   notices and the buttons come back.

### Tests (all `CARGO_TARGET_DIR=target/claude`)
- `cargo test -p hermes-native`: 249 passed, 2 ignored (Phase 3's seed helpers). CP4.1 had 191 + 29 new then; this checkpoint adds about 57: `thread::ansi` (7: colours, carry across lines, 256/true colour, dropped sequences and carriage returns, dim/underline/italic, empty input, 10,000 lines parsed in one pass), `thread::diff` (5), `thread::tools` (16: every card kind from its real result shape, failures, interrupted, changed files, the tree and its rows), `thread::work` (4: group labels live and ended, steps, stored spans, footer folders), `thread::data` (+1 footers), `thread::tests::tools` (24: grouping and what ends a run, header counting, a stopped turn, history runs cut by a page, lazy parse, layout of the group and a card opened and closed, error colour and exit code, diff and "open diff", memory and skill links, delegate tasks, 10,000-line output, live output touching only its card, a 100-event timing at 20 vs 2,000 rows, the footer tree and actions, Retry and Branch to the backend and their failures, disabled buttons, no files card, the frame opening a branch and answering the two actions). Updated CP4.1 tests: row counts now include footers, history totals 144/84.
- **The live check** is `thread::recording::tests::the_recorded_session_is_what_the_real_core_sends`: a real `Core` + wiremock provider, a call of every tool kind (11 calls), the events equal `fixtures/thread-session.json` (deterministic over 5+ runs), live cards equal stored cards. TESTING.md says why it is a test and not a `*-check.mjs`.
- `cargo test -p hermes-native --features snapshots` (via `scripts/native/check.ps1`): "native check: all ok", after accepting the 16 snapshots below.
- `cargo test -p hermes_core`: 139 passed. `cargo build --workspace`: passes. `cargo clippy -p hermes-native --all-targets --features snapshots --no-deps`: clean; `cargo fmt -p hermes-native`: applied.
- Timing (debug, headless): mean per progress event + frame 15 to 20 ms in a 20-row and in a 2,000-row thread (the assertion is `mean(2000) < 3 x mean(20) + 10 ms`; `--nocapture` prints it); 10 frames of an opened 10,000-line command card: well under the 3 s bound.
- `scripts/clean-test-temp.ps1`: dry run 3 folders (0.5 MB), `-Delete` removed them, second dry run 0.

### Snapshots
New (12), `crates/native/snapshots/`: `thread-tools-{command,edit,lookup,agent,group,footer}-t3-{light,dark}.png`
(4 states each: command ok / failed / running with live output / closed; patch / write / patch closed / failed
read; read / search / blocked web fetch / good web fetch; delegate / memory / skill / generic; run folded / open /
running / stopped; footer tree / folders closed / usage only / buttons waiting). Pre-existing baselines that
changed (4), because the tool placeholder line became a card and the newest answered turn got a footer:
`thread-t3-{light,dark}.png`, `thread-live-t3-{light,dark}.png` (the diffs were written by the run before
accepting, as `target/claude/snapshots/thread-t3-light.diff.png`, `thread-t3-dark.diff.png`,
`thread-live-t3-light.diff.png`, `thread-live-t3-dark.diff.png`; the next snapshot run overwrites them).
`UPDATE_SNAPSHOTS=1` was used for these 16 only; no other baseline changed. I looked at two of the new sheets
(command and footer, dark): lines aligned, colours and counts as intended; the Haiku check should look at all
16 changed or new images.

### Deviations
- **Reasoning breaks a run.** t3code folds thoughts into the group; ours keeps CP4.1's reasoning row (it owns a
  streaming `TextViewState`) so a run is calls only. A model that thinks between calls gets several groups.
- **No virtual inner list for an opened group**: a plain scrolling list capped at 368 px drawing the last 150
  calls ("N earlier steps not shown"). A command's output is virtual (`uniform_list`, 16 lines, follows the
  tail while it runs); long lines are clipped, not wrapped (fixed row height).
- **History has no usage or duration** (the store keeps neither): only live turns' footers show them. History
  group durations are the messages' times (second resolution) plus the last command's own duration.
- **Edit counts come from the diff, not the file**: `write_file` does not say what was there, so it counts every
  line as added; a running `patch` shows only the changed lines (no context) until its result arrives.
- **Retry** re-sends the typed text only (attached images are not re-sent: `rewind_to_row` returns the text).
- `Recorded::TurnEnded` gained `user_row` and `last_row` (serde default); the fixture was regenerated.
- **Missing core operations:** none for what was asked. Facts the cards work around: `CoreEvent::ToolCompleted`
  has no error flag (cards read it from the JSON, PITFALLS), `Usage` is per model call (summed here), usage is
  not stored per turn, and `AgentEvent::SubagentProgress` is not published as a `CoreEvent` (the `Subagent`
  events carry the same tools).
- Web-fetch success, a generic tool and the "running" states in the stories are made up from the documented
  result shapes (the real core blocks the recorder's fetch at a local address, and no tool the cards do not
  know ran); every recorded call is real.

### For the reviewer
1. `ToolCard::finish`'s failure rule (JSON `success: false` / string `error`, or non-JSON from a tool in
   `answers_json`) is a heuristic; check it against tools that answer in plain text on purpose.
2. `Output` is parsed when a card opens and on progress while open; a stored 10,000-line command is parsed in
   the click handler (test `ten_thousand_lines...`: fast, debug build). Raw text is cut at 400,000 bytes.
3. `render_row` now borrows the row (no clone); `RowKind` and `Row` are no longer `Clone`.
4. The 1 s ticker in `ThreadView` lives with the view and notifies only while `ThreadModel::counting()`.
5. A run cut by a history page: the later row keeps its key and gets the earlier entries first; the list's
   `prepend` count is one less and the merged row is re-measured. Group times for stored runs are per entry.
6. `ThreadModel::restart` (after Retry) drops `live` and the rows and reads the newest page; a turn that is
   already streaming again is then `partial` and reconciles at its end, as in CP4.1.
7. A live turn always gets a footer; a stored one only with files or as the newest answered turn, so a reopened
   thread shows fewer footers than it did live.
8. `hermes_tools` is a dev-dependency of `hermes-native` (YOLO mode in the recorder), nothing else uses it.
9. The recorder's commands run through Git Bash (`printf`); without it the recorder test fails.

### User checklist (CP4.2)
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n4`): `$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`,
`$env:HERMES_RS_HOME="$PWD\target\native-test"`.
1. `.\target\claude\debug\hermes-native.exe --fixture --dev`, open "halo", type anything in the DEV strip, Enter:
   reasoning, a few words, then ONE row "Working for ..." that becomes "Worked for ... · 11 steps", then the
   reply and a footer ("2 changed files +6 -1", Retry, Branch from here).
2. Click the group row: eleven lines (command, command, read, search, edit, write, memory, skill, web, delegate,
   read). Click the first command: `$ printf ...` and its output with green "pass" and red "fail". Click the
   second: "exit 1" in red.
3. Click the patch line: a diff, green and red lines, "Open diff": the right panel opens and a toast says Phase 6.
4. Click the delegate line, then its task: the subagent's tool (`read_file notes.md`) and its summary.
5. Click the memory or skill line, then "Open memory" / "Open skill ...": a toast says Phase 7.
6. In the footer: click the `docs` folder (closes), the double-chevron button (opens or closes all), a file (toast).
7. "Branch from here": a thread "... (branch)" appears in the sidebar and opens (fixture data). "Retry": the
   thread is read again.
8. Open "Fix the sidebar width": a one-line `read_file` card where CP4.1 had the placeholder.
9. Real core (optional, as CP4.1 item 11, `scripts/perf/mock_llm.py`): a sent prompt streams; reopen the thread:
   the cards come back from the store, the footer only on the last turn.

### Visual check (Haiku 5.5)
No `target/claude/snapshots/*.diff.png` files were present, so the four changed baselines were checked as images. The dark t3code references were not opened, so no dark line below is a reference match.
- thread-tools-command-t3-dark: matches — four states (ok, exit 1 in red, running with live output, closed) with green pass and red fail lines; expected | legible.
- thread-tools-command-t3-light: expected | legible, contrast fine, nothing clipped.
- thread-tools-edit-t3-dark: matches — +/- counts, path header, "Open diff", green and red mini-diff lines; expected | ok.
- thread-tools-edit-t3-light: expected | ok, diff colours readable.
- thread-tools-lookup-t3-dark: matches — read/search lines, blocked fetch in red, file-not-found in red; expected | ok.
- thread-tools-lookup-t3-light: expected | ok.
- thread-tools-agent-t3-dark: matches — delegate nesting with subtask and tool line, memory and skill buttons; expected | the generic JSON card is cut at the top, inside its own scroll area, which is intended.
- thread-tools-agent-t3-light: expected | same scroll cut as dark, intended.
- thread-tools-group-t3-dark: matches — "Worked for ... steps" row, opened list of 11 lines, failure mark, stopped state; expected | ok.
- thread-tools-group-t3-light: expected | ok.
- thread-tools-footer-t3-dark: matches — changed-files tree (folder, nested files, +/- counts), usage line, Retry and Branch from here; expected | ok.
- thread-tools-footer-t3-light: expected | ok, tree and usage legible.
- thread-t3-dark: matches — reasoning closed/open, two-bubble user message, code block with language and copy, table, alerts, red failure line; expected | ok.
- thread-t3-light: expected | ok.
- thread-live-t3-dark: matches — code cut mid-stream, provider error, 200-row scroll with "Scroll to end"; expected | the floating "Scroll to end" button sits over code text, as intended.
- thread-live-t3-light: expected | ok.

### Review (Opus 5.5)
Verdict: good work with three real bugs (failure heuristic, Retry losing a turn, dark-theme alphas), fixed here; main had not moved (no merge).

**Findings and fixes**
1. **Failure heuristic: false positives.** `tool_result_text` joins a result's text blocks with `\n`, so the guardrails' notes (loop warning, "identical call") follow the JSON, and an oversized result becomes a plain-text "saved in ..." note: both made `serde_json::from_str` fail and the "plain text from our tool = harness error" rule marked a good call failed. Our tools all set `CallToolResult::is_error` (`ToolOutput::error`), and a harness failure is `Err`. **Fix (additive, contained):** `CoreEvent::ToolCompleted { is_error }` (`stream.rs`) and `transcript::ToolEntry::is_error` from `transcript::tool_result_is_error` (the stored message keeps the flag; a core test round-trips it through JSON). The Tauri hub ignores the field (`..`); no protocol shape changed. `ToolCard::finish(text, is_error)`: failed = the flag, or JSON `success: false` / non-empty `error` (timed-out command, old rows); `leading_json` reads the first JSON value and keeps the trailing notes as the command's note. `answers_json` is gone. Non-zero exit stays "ran, exit N". The recorder test now also asserts exactly two failed cards (blocked fetch via `results[].error`, the missing read via the flag) and that the flag is recorded; fixture regenerated (`is_error`, serde default).
2. **Retry was not atomic.** `rewind_to_row` then `submit`: a submit that refused (no provider, unknown session) after the rewind lost the turn and the prompt. **Fix:** `CoreBackend::retry` reads the typed prompt from the store (refuses a row that is not a typed prompt), resolves the provider first ("Nothing was changed: ..."), then one `Core::submit` with `truncate: Truncate::BeforeRow(row)` (the core refuses while busy without cutting). Same user-visible semantics as the Tauri app's rewind + resend. New real-core test (in `the_core_backend_reads_...`): refused retry leaves 142 messages; a retry cuts and resends "q69", the old reply is gone.
3. **`Core::submit`/`branch` from a GPUI background thread panicked** ("no reactor running": the turn is `tokio::spawn`ed). That also hit CP4.1's `--dev` submit. Fixed with `core.handle().enter()` in `submit`, `retry`, `branch` (PITFALLS).
4. **Retry on any footer** would rewind every later turn. Now only the newest footer may Retry and not while a turn runs (`ThreadModel::can_retry`, the button disabled); Branch stays on every footer. Test `only_the_newest_turn_can_retry_and_not_while_a_turn_runs`.
5. **History/live parity.** A reopened thread lost the footers of answered turns without files. t3code keeps per-turn meta under each answer, so history now gives every answered turn a footer (Branch; Retry on the newest), as live does. Usage/duration remain live-only (not stored). Row totals in tests updated (214/154).
6. **Dark-theme colours.** `with_alpha` replaces alpha: `border.with_alpha(180)` was white 70% (the bright card borders in the dark sheets), `accent.with_alpha(110/150)` a 43%/59% white hover. Now `token.hsla().opacity(..)`; diff colours were Tailwind v3 hex, now the palette's `success_foreground` / `destructive_foreground` (t3code's v4 emerald/red 700/400); the stat font was "Consolas", now the mono token.

**t3code comparison** (`TimelineWorkGroupRows.tsx`, `MessagesTimeline.tsx` `TurnFoldTimelineRow`, `WorkLog.tsx`, `CommandBlock.tsx`, `CommandOutputPanel.tsx`, `ChangedFilesTree.tsx`, `DiffStatLabel.tsx`; `app-thread-dark.png`, `app-codeblock_closeup-dark.png`): fixed: the group row is the turn fold (no icon, muted label turning foreground on hover, 14 px chevron right after it, `px-1`, `pt-1 pb-2` with an `border/60` hairline); the command block is `border-border bg-muted/50` with output in muted text and a copy button (copies the command and the whole output as plain text, built on click: the reachable form of clipped lines); changed-files card `py-2` header in foreground, `dark:bg-input/20`, tree counts in `DiffStatLabel`'s aligned 3xs columns, the file spacer only in a tree with folders. Deferred: t3code folds the whole turn (reasoning, interim text) under one "Worked for" and shows tool groups ("Read 3 files") inside; ours folds runs of calls (the spec's wording) and a reasoning row ends a run, since the reasoning row owns a live `TextViewState` (a structural change, Phase 8 polish); `PierreEntryIcon` per-file icons and `MiddleTruncate` in the tree; virtualised group list and tree (>150 calls show "N earlier steps not shown", >250 tree rows "N more rows", recoverable through Open diff in Phase 6); a diff past 60 lines likewise.
The two "unexpected" notes: the generic card cut at the top is the thread following its tail in a 648 px story (the card is taller), not a card bug; "Scroll to end" floating over content is t3code's placement. Both left.

**Checked, no change:** per-delta cost (progress touches one entry, `index_of` scans from the end, the 1 s ticker notifies only while `counting()` and the list re-renders visible rows only; the 2,000-row timing test covers tool progress); ANSI parser (malformed/truncated escapes dropped, 256/truecolor, `\r`, 400 KB cap, parsed once on open); views call no core (only `model/backend.rs` does); action seams only dispatch `OpenDiffTab`/`OpenScreen`; no store schema change. The live/stored card equality is meaningful now that the error flag travels through the store.

**Left for later:** changed files are summed from edit results, not a git diff (edit-then-revert shows counts, renames/deletes by `terminal` are invisible; t3code reads checkpoint diffs: Phase 6 with the diff tab); `live.delegate` holds one delegate call, so two concurrent `delegate_task` calls would mix subagent events; a subagent event for a task with neither id nor index is dropped; retried prompts do not re-send attached images.

**Tests:** `cargo test -p hermes-native` 251 passed (2 ignored); snapshots: 14 baselines re-accepted after looking at them (`thread-tools-*` both themes, `thread-live-t3-*`: the colour, fold-row and copy-button changes above); `scripts/native/check.ps1`: "native check: all ok"; `cargo test -p hermes_core` 140 passed; `cargo test -p hermes-desktop` all passed (130 + 68, the RPC-table fixture unchanged); `cargo build --workspace` passes.
