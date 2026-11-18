# Hermes native, phase by phase

The implementation plan for [README.md](README.md). Read that first, then `AGENTS.md`,
`docs/ARCHITECTURE.md`, `docs/TESTING.md` and `docs/PITFALLS.md`.

Each phase ends in something the user can try, with tests green and docs true. Phases split into
**checkpoints**, run like the backend phases ([docs/backend/PHASES.md](../backend/PHASES.md), *How
the work is run*): one model implements up to the checkpoint and stops without committing; a fresh
reviewer reviews, fixes and commits as `native p<N> cp<x>: ...`. Don't start a checkpoint before the
previous one is committed. Within a checkpoint, do the steps in order. Which model does what is in
*How the work is run* below; the `checkpoint` skill reads it from there.

When the whole plan is done, the app ships as **v0.5**.

Rules that matter most here:
- **Before Phase 0, the user confirms the rule changes** in README.md (*Rule changes*), and
  `AGENTS.md` is updated to match.
- **Never touch the user's app or data.** Build with `CARGO_TARGET_DIR=target/claude`. The native
  app under test runs with `HERMES_RS_HOME=target/native-test`; captures are of that instance only.
- **The Tauri app keeps working** through every phase: `cargo test -p hermes-desktop`, the ACP
  tests and the `scripts/perf/*-check.mjs` live checks stay green.
- **`crates/app` keeps building** until Phase 9 removes it (ground rule: `cargo build --workspace`).
- **Views never call the runtime.** They call `hermes_core` through the app's models.
- **No Zed code in `crates/native` or `hermes_core`.** GPUI only through `gpui-kit`.
- **Cite t3code** in the header of each module that copies a layout:
  `//! t3code: \`apps/web/src/components/Sidebar.tsx\` (thread rows, status dots)`.
- **Every checkpoint that adds UI** adds layout assertions, gallery entries and approved snapshots,
  and ends with a numbered checklist for the user.

Sizes: S ≈ a day, M ≈ 2–4 days, L ≈ a week or more (agent-days, rough).

---

## Phase map

| # | Phase | Size | Depends on |
|---|---|---|---|
| 0 | Spike and dev loop: GPUI Kit on Windows, layout dump, snapshots, gallery — ✅ 2026-11-18 | M | — |
| 1 | `hermes_core`: the backend's application layer, extracted from `crates/desktop` — ✅ 2026-11-18 | L | — |
| 2 | Design system and app frame: themes, tokens, design layer, window, palette, keymap | M–L | 0 |
| 3 | Sidebar: projects and threads | M | 1 (CP1.1), 2 |
| 4 | Thread view: timeline, markdown, tool calls, streaming | L | 3 |
| 5 | Composer: input, mentions, commands, attachments, model, approvals, questions | L | 4 |
| 6 | Right panel: diff, files, terminal | L | 1, 4 |
| 7 | Hermes screens: settings, memory, skills, staging, usage, import | L | 1, 2 |
| 8 | First run, polish, accessibility, installer | M | 3–7 |
| 9 | Switch over and retire `crates/app` and `vendor/zed` | S–M | 8 + user's decision |

Phases 0 and 1 are independent and can run at the same time (different crates). Phases 6 and 7
can run in parallel once 4 is done.

---

## How the work is run: models, checkpoints, sessions

Same protocol as the backend plan (orchestrator, implementer, reviewer, progress notes, lanes;
[docs/backend/PHASES.md](../backend/PHASES.md), *How the work is run*), run by the same
`checkpoint` skill with these differences:

| | Backend plan | This plan |
|---|---|---|
| Worktree | `..\hermes-rs-p<N>` | `..\hermes-rs-n<N>` |
| Branch | `backend/p<N>-<slug>` | `native/p<N>-<slug>` |
| Progress note | `docs/backend/progress/phase-<N>.md` | `docs/native-ui/progress/phase-<N>.md` |
| Commit | `backend p<N> cp<x>: …` | `native p<N> cp<x>: …` |
| Tests at a checkpoint | the checkpoint's | the checkpoint's, plus `scripts/native/check.ps1` (from CP0.2 on) |

### Why mix models (what it saves and what it doesn't)

Choosing a cheaper model doesn't use fewer tokens; it makes each token cheaper (Sonnet 5.5 is
half Opus 5.5's price per token, Haiku 5.5 a fraction of Sonnet's; plan usage limits weigh models
the same way). What uses fewer tokens is structure:

- **Fresh subagents with small contexts** instead of one long session that carries everything it
  ever read. Each implementer and reviewer starts from the plan, the progress note and the diff.
- **A thin orchestrator.** It lives for a whole phase, so it is the session whose context grows
  longest: it reads only reports of under 300 words, never diffs or code. That is why it runs on
  Sonnet: long-lived and routine.
- **Precise checkpoints.** Files, steps and tests named in the plan mean less exploring.
- **Text before images** (README, *The dev loop*).

Where a cheap model backfires: hard work done by a weaker implementer comes back with problems the
Opus reviewer then rewrites, paying for the work twice. So hard checkpoints are implemented by
Opus, and the reviewer is a fresh Opus by default.

### Roles and models

- **Orchestrator: Sonnet 5.5.** The session the user talks to; runs the `checkpoint` skill.
- **Implementer:** per the table below. Sonnet for most; **Opus** where the work is design-heavy,
  touches GPUI or Win32 internals, or is concurrency; **Haiku** only for small, mechanical
  checkpoints with tests that pin the result.
- **Reviewer:** a fresh **Opus** subagent, except where the table says Sonnet (small checkpoints
  implemented by Haiku).
- **Visual check: Haiku 5.5**, between implementer and reviewer, on any checkpoint whose
  `check.ps1` reports snapshot differences. It looks at the `*.diff.png` / `*.actual.png` crops
  only and writes a verdict per image ("expected: new hover colour", "unexpected: label clipped")
  under the checkpoint in the progress note. The reviewer reads the verdicts, not the images, unless
  one says "unexpected". At a phase's **last** checkpoint, a **Sonnet** visual check looks at each
  new screen once (cropped, both themes) against its t3code reference.

| Checkpoint | Implement | Review | Why |
|---|---|---|---|
| CP0.1 GPUI Kit spike | **Opus** | Opus | Unknown API, a go/no-go verdict |
| CP0.2 Dev loop | **Opus** | Opus | Element tree internals, Windows capture |
| CP1.1 Core, sessions, turns, events | **Opus** | Opus | Concurrency, event plumbing, three front ends |
| CP1.2 Workspace features | Sonnet | Opus | Mechanical moves behind CP1.1's pattern |
| CP1.3 Settings and Hermes features | Sonnet | Opus | Same |
| CP2.1 Themes and tokens | Sonnet | Opus | Data mapping |
| CP2.2 Design layer and frame | Sonnet | Opus | Components on GPUI Kit; title bar is the risky part |
| CP3.1 Sidebar (Phase 3 is one checkpoint) | Sonnet | Opus | |
| CP4.1 Timeline and markdown | **Opus** | Opus | Virtual list, streaming, incremental markdown: the hardest view |
| CP4.2 Tool calls and work log | Sonnet | Opus | |
| CP4.3 Header | Sonnet | Opus | |
| CP5.1 Input and sending | Sonnet | Opus | |
| CP5.2 Mentions, commands, attachments | Sonnet | Opus | |
| CP5.3 Approvals and questions | Sonnet | Opus | |
| CP6.1 Diff | Sonnet | Opus | |
| CP6.2 Files | Sonnet | Opus | |
| CP6.3 Terminal | **Opus** | Opus | Emulator state, PTY, renderer |
| CP7.1 Settings | Sonnet | Opus | |
| CP7.2 Memory, skills, staging | Sonnet | Opus | |
| CP7.3 Usage | Haiku | Sonnet | One page over existing core calls and design components |
| CP8.1 First run, polish, accessibility (Phase 8 is one checkpoint) | Sonnet | Opus | |
| CP9.1 Switch over | Sonnet | Opus | |
| CP9.2 Delete the old code | Haiku | Sonnet | Deletions; `cargo build --workspace` pins the result |
| CP9.3 Docs for the new app | Sonnet | Opus | |

The user may override any of these per run ("implement CP4.2 with Opus").

---

## Phase 0 — Spike and dev loop

**Goal:** prove GPUI Kit does what this plan needs on this machine, and build the checking tools
before any real screen exists.

### CP0.1 — GPUI Kit spike (M)

1. Add `crates/native` (package `hermes-native`, binary `hermes-native`) with `gpui-kit` pinned
   exactly (`=`) to the current release. Record the version and the GPUI snapshot it pins (the
   `gpui-pre` description names the Zed commit) in `vendor/SOURCES.md` under a new *GPUI Kit*
   section.
2. `cargo build --workspace`: check both GPUI copies (vendored for `crates/app`, GPUI Kit's) build
   and link together. If Cargo refuses (a duplicate `links` key), stop and report: the fallback
   (`crates/app` out of `default-members`) is the user's call.
3. A throwaway window with, on fake data:
   - a **virtual list** of 2,000 messages of mixed heights, scrolling smoothly, pinned to the
     bottom while new text streams into the last one (a timer appending words);
   - **markdown** with headings, lists, tables, links and fenced code in 4 languages, highlighted;
   - **text selection** inside a message and inside a code block, and copy;
   - a **multiline text input** with Windows IME (test with the Windows emoji panel, `Win+.`, and
     a Japanese or Greek IME if installed), undo, and paste;
   - a **resizable** three-pane layout (sidebar, main, right panel);
   - a **Zed theme** file (one from `vendor/zed/assets/themes/`) mapped onto GPUI Kit's theme by
     hand, enough to see the mapping is possible.
4. Measure cold start to first frame and idle memory; note them in
   `docs/native-ui/progress/phase-0.md`.

**Done:** a written verdict in `progress/phase-0.md`: each item above works, works with a workaround
(which), or doesn't. If markdown selection, IME or the virtual list doesn't work, stop: the user
decides whether to continue on GPUI.

### CP0.2 — The dev loop (M)

1. **Element dump.** A debug feature (`--features dev`) that walks the rendered frame and writes
   each named element (`debug_selector` / `id`) with its bounds, background and text colour, and
   state (hovered, focused, disabled) as compact text. Find what GPUI Kit's UI-test support
   already gives and use that first.
2. **Layout assertion helpers** for tests (`crates/native/src/testing.rs`): `same_row(a, b)`,
   `same_height(..)`, `inside(child, parent)`, `no_overflow(id)`, `color_of(id) == token`. Usable
   from `#[gpui::test]` tests with fixture data.
3. **Image capture.** Decide how (README, *The dev loop*): off-screen rendering if GPUI Kit's test
   support has it, else an off-screen window of our own instance captured with Windows Graphics
   Capture or `PrintWindow`. Output: PNG of one element's bounds (a crop), not the window.
4. **Snapshot harness.** `cargo test -p hermes-native --features snapshots` renders each registered
   gallery story, compares with `crates/native/snapshots/<name>.png` (a per-pixel threshold), and on
   a difference writes `<name>.actual.png` and `<name>.diff.png` into `target/claude/snapshots/`.
   `UPDATE_SNAPSHOTS=1` accepts the new image.
5. **Gallery binary** `hermes-native-gallery`: a story list on the left, the story on the right,
   theme and light/dark switches. Stories register with one macro so the snapshot harness finds
   them.
6. **`scripts/native/check.ps1`**: build, tests, snapshots; prints only failures and the paths of
   diff images.
7. Docs: `docs/TESTING.md` gets a *Native UI* section (the three kinds of check, how to accept a
   snapshot, how a subagent reviews diff images); `docs/PITFALLS.md` anything surprising from the
   spike.

**Done:** a sample story (a button in its five states) with a layout test, a snapshot, and a
deliberately broken variant that the harness catches.

---

## Phase 1 — `hermes_core`

**Goal:** one UI-agnostic crate that every front end calls: the Tauri gateway, the ACP agent and the
native app. A pure refactor for the existing front ends: no protocol change.

Approach for every module: split it into **logic** (typed inputs and outputs, Rust errors) that
moves to `crates/core` (package `hermes_core`), and **shape** (Hermes's JSON field names, JSON-RPC
errors) that stays in `crates/desktop`. Unit tests move with the logic; the gateway keeps its
protocol tests. Header lines citing Hermes stay with the logic and the `PORTING.md` rows point at the
new paths.

### CP1.1 — Core, sessions, turns, events, questions (L)

1. `crates/core`: `Core { runtime, handle, ... }` built from `RuntimeConfig`, owning what
   `Gateway` owns today that isn't protocol (side stores, the hub's pending questions).
2. **Events:** `Core::subscribe() -> Receiver<CoreEvent>` with typed events for a session's
   stream (text and reasoning deltas, tool call start/progress/end, turn end, error, title,
   usage), approvals and questions (`ApprovalRequested`, `ClarifyRequested`, `Settled`), background
   processes, review and curator results. Built from `RuntimeEvent` and `hub.rs`.
3. **Questions:** the `UserBridge` implementation moves here; answers by `Core::answer_approval` /
   `answer_clarify`. The gateway turns events into `approval.request` etc. as today.
4. **Sessions and turns:** list (with project grouping and status), create, resume, rename, delete,
   archive, branch, move to project, submit a prompt (with the queue behind a running turn), cancel,
   `/` command dispatch. From `gateway/sessions.rs`, `gateway/turn.rs`, `branch.rs`, `archive.rs`,
   `workspace.rs`, `projects.rs`, `gateway/projects.rs`, `transcript.rs` (the stored-message reading;
   Hermes's `TranscriptMessage` shape stays in desktop).
5. **Cross-app lock:** a lock file in the data folder taken by whichever app starts first
   (desktop, native, ACP excepted since it runs beside the desktop app today: check how they share
   now and keep that working). The second app shows "Hermes is already open" and exits.

**Done:** `cargo test -p hermes_core -p hermes-desktop`, `cargo test -p hermes_runtime --test turns`,
the ACP tests and all live checks green; `crates/desktop` lost the moved logic.

### CP1.2 — Workspace features (M)

Git review and git reads (`git_review.rs`, `git.rs`), files (`files.rs`, `open_file.rs`), terminal
(`terminal.rs`: PTY sessions as a typed API with output as events), attachments (`attachments.rs`,
`gateway/attachments.rs`), `@` path completion (`paths.rs`), the command catalog (`commands.rs`,
`gateway/slash.rs` minus the shapes), export (`export.rs`), background processes and subagents
(`gateway/delegation.rs`).

**Carried over from CP1.1 (reviewed 2026-11-18; do these first, and implement them with Opus:
it is CP1.1's concurrency work).** CP1.1 left the turn in the gateway (progress note, "What stayed
and why"). Phases 4 and 5 cannot start without:

1. **Turns in core:** `Core::submit(session, SubmitTurn { text, images, ... }) -> Started | Queued`
   with a typed queue (list, remove, steer, redirect), the turn's event loop (`gateway/turn.rs`
   `submit` / `run_queued`, the title, the heartbeat) running in core and publishing the stream
   events itself, plus `TurnStarted` and a `TurnEnded` carrying the persisted rows and the error
   text. Today only the gateway's loop publishes the stream: a front end with a `Core` and no
   `Gateway` sees no `TextDelta` at all. The gateway keeps the admission gate, `message.start`,
   `session.info` and `message.complete` as frames built from those events (the queue items keep
   the page's params beside the typed fields, or `PageState::queue` becomes typed).
2. **`/` command dispatch from Rust:** `Core::run_command(session, command, args) -> outcome`
   (what `gateway/slash.rs` does minus its JSON), with the catalog above.

Done for these: the RPC-table fixture unchanged, `queue-check`, `stop-check`, `coding-commands-check`
green, and a core test that submits, queues behind a running turn and cancels against the mock
provider with no `Gateway`.

**Status (CP1.2 implementer, 2026-11-18):** both done. `hermes_core::turns` (`Core::submit`,
`SubmitTurn`, the typed queue: `queue`, `remove_queued`, `steer_queued`, `replace_queued`,
`redirect`; the loop with title and heartbeat; `TurnStarted`, `TurnEnded { end: TurnEnd }`,
`QueueChanged`, `Idle`, `Subagent` events). The page's frames that need the store come from the
gateway's `TurnHooks` (`gateway/turn.rs` `PageTurn`), not from the events (a listener must not
block). `Core::run_command` / `dispatch_command` with the catalog in `hermes_core::commands`; what
needs Settings (context windows, a session's provider, `/import-hermes`) is a `CommandEnv` until
CP1.3. Tests: `crates/core/src/turns/tests.rs`. Details and doubts: `progress/phase-1.md`, CP1.2.

### CP1.3 — Settings and Hermes features (M)

Providers and endpoints (`providers.rs`, `endpoints.rs`), auxiliary models, fallbacks, profiles
(`aux_models.rs`, `fallback.rs`, `profiles.rs`), settings (`gateway/settings.rs` logic), toolsets,
skills, maintenance and curator (`toolsets.rs`, `skills.rs`, `maintenance.rs`), staging
(`staging.rs`), learning (`learning.rs`), usage and the request ledger (`gateway/usage.rs`), import
from Hermes (`hermes_import.rs`).

**Status (CP1.3 implementer, 2026-11-18):** done. The logic of every module listed is in
`hermes_core` (`config`, `providers`, `endpoints`, `aux_models`, `fallback`, `profiles`,
`settings`, `usage`, `toolsets`, `skills`, `maintenance` with the curator's `CuratorStarted` /
`CuratorFinished` events, `staging`, `learning`, `hermes_import`); `crates/desktop` keeps Hermes's
JSON, statuses and RPC errors. `CoreCommands` is the `CommandEnv` over the core's own Settings, so
`Core::run_command` needs no gateway. Details and doubts: `progress/phase-1.md`, CP1.3.

**Reviewed 2026-11-18 (Opus):** accepted. `Core::start(background)` added: the one call a front end
makes after `Core::new` (Settings on the runtime, the auto-archive sweep, the scheduled curator,
the switched-off skills); the gateway uses it, so the native app (Phase 3) calls it the same way.

**Done for Phase 1:** `docs/ARCHITECTURE.md` shows `hermes_core` between the front ends and the
runtime; `PORTING.md` paths updated; no behaviour change visible in the Tauri app (the user's
checklist: a short pass over the main screens).

---

## Phase 2 — Design system and app frame

### Reference screenshots from t3code (user-supplied, 2026-11-18)

The user will provide screenshots of t3code so the design matches the original as closely as
possible. They are the visual ground truth for Phase 2, and later phases reuse them.

- **Where:** `docs/native-ui/reference/t3code/`, committed. Names say what and which theme:
  `<screen-or-component>-<light|dark>[-<state>].png` (e.g. `sidebar-dark.png`,
  `button-variants-light.png`, `titlebar-dark-maximised.png`; no theme suffix means dark). Whoever
  adds a file adds a line to `reference/t3code/README.md` saying what it shows.
- **The set is partial, by the user's choice** (2026-11-18: dark theme, frame, sidebar, thread,
  panels, composer model picker; see the folder's `README.md` for the list and what is not
  covered). The orchestrator does not ask the user for more. **A subagent that needs a screenshot
  that is missing takes it itself** from t3code (installed; source in `C:\Users\alexandros\repos\t3code`; the user allows
  using their instance and data, 2026-11-18, but nothing they'd notice may stay changed: put
  settings back, don't create or delete threads/projects; close only what the agent started), crops to the element, saves it in `reference/t3code/` with the naming rule and adds a
  row to the folder's `README.md`. This covers the light theme, button variants and sizes, title
  bar, command palette, toast, tooltip, badge, kbd and empty states. Where t3code itself can't be
  run, the implementer says so in the progress note and works from its source.
- **CP2.1 (tokens):** the implementer reads the t3code CSS values first (`index.css`) and uses
  the screenshots to *check* them (sample a few pixel colours per token, background, border,
  muted text, accent, from the images), recording any token whose screenshot colour differs from
  the CSS in the progress note. Font choice (step 4) is judged against the screenshots too.
- **CP2.2 (components, frame):** each design component and the frame gets its screenshot named in
  its module header next to the t3code source line, and a gallery story with the same states and
  the same size as the reference (same window width, same scale), so the two can be compared side
  by side.
- **Checking:** the implementer reads only the text (CSS, source) plus the *few* screenshots its
  checkpoint names. The **visual check** (Sonnet at CP2.1 and CP2.2) compares each new snapshot
  crop with its reference screenshot and reports per image `matches | differs: <what, where>`
  (spacing, size, radius, colour, weight, alignment). `differs` lines are fixed or explained by the
  reviewer; they are not accepted as baselines silently. Keeps the "text before images" rule: only
  those pairs are looked at, cropped.
- **Not pixel-perfect by design:** differences from fonts, text rendering and the platform's
  anti-aliasing are expected; layout, spacing, sizes, radii and colours should match.

### CP2.1 — Themes and tokens (M)

1. `design/tokens.rs`: the token set, named after t3code's CSS variables (`background`,
   `foreground`, `card`, `popover`, `primary`, `muted`, `muted_foreground`, `accent`, `border`,
   `input`, `ring`, `sidebar*`, `message_surface`, `success`, `warning`, `error`, `info`), spacing
   (4px grid), radii (`sm`, `md`, `lg` = 8px, `xl`), control heights (20/24/28/32/36), font sizes.
2. Default light and dark themes from t3code's `index.css` (`:root`, `.dark`) values, converted
   to sRGB.
3. `theme/zed.rs`: serde structs for Zed's theme family JSON (only the fields we map), and the
   mapping onto our tokens (and through them onto GPUI Kit's theme). Bundled themes in
   `crates/native/assets/themes/` with their licence files; a user folder
   (`<data>/themes/`) read at start and on change.
4. Fonts: an interface font and a monospace font with open licences bundled, or the Windows
   system fonts (Segoe UI Variable, Cascadia Mono). Decide in the checkpoint from the spike's
   rendering; record the decision.

### CP2.2 — Design layer and frame (M)

1. `design/`: `Button` (t3code's variants: default, outline, ghost, ghost-muted, destructive; its
   sizes), `IconButton`, `ListRow` (leading icon, label, trailing meta, selected and hover states),
   `SectionHeader`, `Panel`, `Badge`, `Kbd`, `Tooltip`, `Toast`, `EmptyState`, `MiddleTruncate`
   (paths). Each wraps GPUI Kit's component where one exists and is a gallery story with its states.
2. Window frame: custom title bar matching t3code's chrome (Windows caption buttons, snap layouts
   on hover of maximise), DPI changes, minimum size, window position and size remembered.
3. `workspace.rs`: sidebar (resizable, collapsible), main area, right panel (resizable, closed by
   default), bottom drawer, all sizes in `native-ui.json`.
4. `keymap.rs` + actions + `palette.rs`: the command palette (fuzzy over actions, threads and
   projects; `Ctrl+K` and `Ctrl+Shift+P`), t3code's default shortcuts where they don't clash with
   Windows.
5. Toasts (bottom right, stacked, auto-dismiss) for core events that have no other place.

**Done:** the empty app frame with palette and themes; all design components in the gallery with
snapshots; layout tests for the frame at three window sizes.

---

## Phase 3 — Sidebar (M)

`sidebar/`, from t3code's `Sidebar.tsx` and `components/sidebar/*`:

1. Projects as collapsible folder headers (name, folder path on hover, new-thread button), threads
   under them sorted by last activity, a "No project" group last.
2. Thread rows: title (middle-truncated), relative time, status (running spinner, waiting for
   approval, unread, error), hover actions (rename, archive, delete with confirmation), context
   menu, keyboard navigation (up/down, Enter, F2 rename, Delete).
3. Pinned threads at the top; filter box (`Ctrl+F` in the sidebar) by title.
4. Add project (folder picker), rename, remove; drag a thread onto a project (move to project).
5. Footer: settings, usage, theme toggle.
6. Empty states: `NoProjectsHero` (add a folder) and `NoActiveThreadState`.

**Done:** sidebar against a fixture store and against the real core with the user's data copied
into `target/native-test`; layout tests (row heights, alignment of status dots); snapshots of each
row state.

---

## Phase 4 — Thread view (L)

`thread/`, from t3code's `ChatView.tsx`, `chat/ChatCanvas.tsx`, `ChatMarkdown.tsx`,
`CommandBlock.tsx`.

### CP4.1 — Timeline and markdown (L)

1. Virtual list of turns; history loads from the core, newest at the bottom, older pages on scroll.
2. User messages (t3code's message surface, attachments as thumbnails), assistant messages as
   markdown: GPUI Kit's markdown view, extended where needed (code block header with language and
   copy button, GitHub-style alerts, file links that open in the right panel, tables that scroll
   horizontally).
3. Streaming: deltas append to the last block only; incremental markdown parsing of the tail;
   bottom pinning with a "jump to latest" button when the user scrolled up.
4. Reasoning blocks collapsed by default with their duration.
5. Selection inside messages and code; copy message (as markdown); copy code.

### CP4.2 — Tool calls and the work log (M)

1. Tool call cards: command (the command, exit code, collapsible output with ANSI colours),
   file edit (path, +/− counts, inline mini diff, open in the diff tab), file read and search
   (compact one-liners), web fetch, delegate/subagent (nested, collapsible), memory and skill
   writes (with a link to the screen), errors.
2. t3code's grouping: consecutive tool calls fold into one "worked for 1m 12s, 8 steps" row while
   a turn runs and after it ends; expandable.
3. Turn footer: changed files of the turn (`ChangedFilesTree`), usage of the turn, retry and
   branch-from-here actions.

### CP4.3 — Header (S)

Thread title (editable), project and folder, git branch and status (from core git reads), model
in use, right panel toggles, the thread's actions menu (export, branch, archive).

**Done:** a recorded fixture session (every event kind, from a mock provider run) replays in the
gallery; timeline layout tests; snapshots of every card kind; a live turn against the mock
provider in `scripts/perf/` streams correctly in our instance.

---

## Phase 5 — Composer (L)

`thread/composer/`, from t3code's `chat/ChatComposer.tsx` and `chat/Composer*`.

### CP5.1 — Input and sending (M)

1. Multiline input (GPUI Kit's), grows to a maximum height then scrolls; Enter sends,
   Shift+Enter newline; IME; undo; draft kept per thread in `native-ui.json`.
2. Send and stop; while a turn runs, sending queues (shown above the input, removable).
3. Footer controls: model and effort picker (the core's model list), mode picker (our Manual, Auto
   and Plan modes), context usage indicator.

### CP5.2 — Mentions, commands, attachments (M)

1. `@` path completion (the core's completion), inserted as an inline chip.
2. `/` command menu from our catalog, with arguments; commands that answer locally (`/usage`,
   `/diff`) show their answer in the timeline as today.
3. Attachments: paste an image, drop files, pick files; thumbnails strip; size limits from the
   core.

### CP5.3 — Approvals and questions (M)

1. A pending approval replaces the composer's top with t3code's approval panel: the command (or
   edit), why it was flagged, the four choices (once, session, always, deny) with shortcuts.
2. Clarify questions: the choices as buttons, free text when allowed.
3. Sidebar status and a Windows notification when a background thread waits for the user.

**Done:** layout tests and snapshots for each composer state; a mock-provider turn that asks for an
approval and a question, answered from the composer in our instance.

---

## Phase 6 — Right panel (L)

`panel/`, from t3code's `RightPanelTabs.tsx`, `DiffPanel.tsx`, `components/diffs/*`,
`components/files/*`, `ThreadTerminalDrawer.tsx`.

### CP6.1 — Diff (M)

Changed files tree (status letters, +/− counts), unified diff with syntax highlighting, scope
switch (this turn, this thread, working tree against `HEAD`), revert a file or a hunk where the core
supports it, open in editor.

### CP6.2 — Files (M)

Project file tree (lazy, `.gitignore` aware, fuzzy file finder `Ctrl+P`), read-only file view with
highlighting and line numbers, images, markdown preview; "Open in editor" (the user's configured
editor: Zed, VS Code, or the system default).

### CP6.3 — Terminal (L)

The core's PTY sessions rendered by our terminal view over `alacritty_terminal`: colours from the
theme, selection and copy, paste, scrollback, resize, links. Shown as a right panel tab and as
t3code's bottom drawer, several terminals per thread.

**Done:** snapshots of each panel with fixture repos; the terminal passes a scripted session
(colours, a full-screen program like `less` or `vim` if present, resize).

---

## Phase 7 — Hermes screens (L)

`settings/` and feature screens, laid out like t3code's settings (a left list of sections, a
scrolling page of rows with label, description and control).

### CP7.1 — Settings (M)

General, Appearance (theme, fonts, density), Providers and endpoints, Models (default, auxiliary
models, fallbacks), Budget and request limits, Tools and toolsets, Sandbox, Keybindings
(read-only list first), Archived threads, Maintenance (curator schedule, run now), Data (folder,
export, import from Hermes), About.

### CP7.2 — Memory, skills, staging (M)

Memory (MEMORY.md and USER.md entries by tier, notes, edit and delete as the user), Skills (list,
search, enable, view a skill, curator status per skill), Staging (writes waiting for the user:
review the diff, apply, discard), review results (what the background review learned, linked from
the toasts).

### CP7.3 — Usage (S)

The request ledger and costs, t3code's usage page layout: per day, per model, per purpose (main
turn, review, curator, side jobs).

---

## Phase 8 — First run, polish, accessibility, installer (M)

1. First run: no endpoint configured → the endpoint setup (t3code's welcome page layout); no
   project → add a folder.
2. Accessibility: GPUI Kit's AccessKit support switched on; every control named; full keyboard
   navigation (Tab order, focus rings from the `ring` token); check with Narrator.
3. Polish pass over every screen in the gallery with the subagent review, both themes, at 100%,
   150% and 200% scaling.
4. Start-up and memory against the Tauri app's numbers (the user asked for no performance work for
   now: measure and report only).
5. Installer: `docs/RELEASING.md` and the installer script ship `hermes-native.exe` beside the
   Tauri app (both installed, the user picks).

**Done:** the user uses it for real work for a few days; a list of what they hit becomes the last
checkpoint's work.

---

## Phase 9 — Switch over and retire (S–M)

Only on the user's decision, after Phase 8. The end of this phase is the **v0.5** release.

### CP9.1 — Switch over (S)

`hermes-native` becomes the shipping app (binary name, shortcuts, installer default, version
0.5.0). The Tauri app: retired or kept as a fallback (decided by the user later). If retired:
`crates/desktop` keeps the ACP agent and loses the page server, gateway, patches, plugin and
webapp build.

### CP9.2 — Delete the old code (S)

Delete `crates/app`, `vendor/zed/`, `vendor/async-task/`, `crates/markdown`, `crates/highlight`,
`crates/themes`, `scripts/vendor_zed.py`, the Zed block in `vendor/SOURCES.md`, and the
`[patch.crates-io]` entry if nothing else needs it. `cargo build --workspace` and every test suite
pass.

### CP9.3 — Docs for the new app (S)

`AGENTS.md`, `docs/ARCHITECTURE.md`, `FEATURES.md`, `GAPS.md`, `docs/RELEASING.md` rewritten for
the new app; the ground-rule exceptions for the Tauri page removed if it is retired.

---

## Decisions (user, 2026-11-17)

- GPUI stays the framework; Zed themes should keep working.
- Structure and look follow t3code: "what if t3code was built by the Zed team".
- Depend on Zed as little as possible, entirely if possible (here: GPUI only, through GPUI Kit).
- Visual checks favour text (layout dumps, snapshot diffs) over screenshots, to keep tokens down.
- The three rule changes in README.md are approved (written into AGENTS.md).
- A Sonnet orchestrator, Sonnet implementers, Opus for hard checkpoints and for review, Haiku for
  small mechanical work and visual triage (the table in *How the work is run*).
- The finished app ships as v0.5.

Later (not needed before Phase 0): whether text selection across messages is needed (revisit after
CP4.1); whether the Tauri app is retired or kept as a fallback (decide at CP9.1).
