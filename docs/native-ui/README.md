# Hermes native: a Rust UI in the shape of t3code

Status: **planned (2026-11-17), nothing built.** This folder is the plan and, once built, the
reference: read it before changing the code, and keep it true.

| File | What it is |
|---|---|
| [README.md](README.md) | This file: the goal, the decisions, the architecture, the look, the dev loop |
| [PHASES.md](PHASES.md) | The implementation, phase by phase: checkpoints, files, tests, done criteria |

Read [AGENTS.md](../../AGENTS.md) first. Its ground rules apply here, with the three changes listed
under *Rule changes* below (confirmed by the user 2026-11-17 and written into AGENTS.md).

**Release:** when this app reaches parity and replaces the Tauri app, it ships as **v0.5**.

## The goal

**"What if t3code had been built by the Zed team."** A native Windows app, in Rust, on GPUI:

- **t3code's structure:** projects and their threads in a left sidebar, the conversation in the
  middle, a composer at the bottom, a right panel with tabs (diff, files, terminal), a command
  palette, settings as a page with its own navigation. Same density, same restraint, same
  shadcn-style controls (zinc neutrals, 8px radius, Lucide icons).
- **Zed's qualities:** instant (no spinner for anything local, no layout shift, smooth scrolling of
  long conversations), keyboard-first (every action has a name, a keybinding and a palette entry),
  excellent text (crisp, selectable, real highlighted code everywhere), Zed themes.
- **Hermes's features:** what our backend has and t3code doesn't (memory, skills, the curator and
  review, staging, budgets, fallbacks, the sandbox) as first-class screens, not bolted on.

It replaces the Tauri app (Hermes's React frontend) once it reaches parity, and the frozen GPUI app
(`crates/app`) after that. Until then all three build; the Tauri app stays the one the user runs.

### Why not the earlier GPUI app

`crates/app` stalled because every small thing (text selection, a button's alignment, a hover
colour) was hand-made: it was built on Zed's `ui`/`theme` crates plus our own widgets (a 1,600-line
text area). This plan changes the three things that caused that:

1. **A component library** (GPUI Kit, below) instead of Zed's crates and home-made widgets.
2. **A thin design layer of our own** on top, so sizes, spacing and states are decided once.
3. **A dev loop that checks the UI without a person**: layout assertions in tests, image
   snapshots, a component gallery (see *The dev loop*).

## Decisions

| Decision | Choice | Why |
|---|---|---|
| UI framework | **GPUI**, through **GPUI Kit** (Longbridge's `gpui-kit` / `gpui-component`, Apache-2.0) | GPUI is built for an editor: text, long lists and highlighting are its strength. GPUI Kit adds 75+ components: inputs, virtual lists, tables, dock and resizable panels, menus, popovers, tooltips, markdown and HTML views, a tree-sitter code view, themes, AccessKit accessibility, UI test support. Its look is shadcn-based, the same family as t3code's. |
| Zed code | **None of ours.** GPUI itself arrives only as GPUI Kit's pinned dependency. | See *Zed dependency policy*. |
| Layout and behaviour | t3code's (`../t3code`, MIT, `apps/web/src`), copied as structure, sizes and tokens, never as code | It is the closest existing app to the use case (a local coding-agent harness). |
| Backend access | **In-process**, through a new UI-agnostic crate `hermes_core` (Phase 1) | No JSON-RPC to ourselves. The Tauri gateway and the ACP agent become adapters over the same crate. |
| Crate and binary | `crates/native`, package `hermes-native`, binary `hermes-native.exe` | Renamed to the shipping name when it replaces the Tauri app (Phase 9). |
| Data | The same data folder as the other apps (`%APPDATA%\hermes-rs`, `sessions.db` schema frozen) | The user keeps their sessions. A cross-app lock stops two apps from using the data at once (CP1.1). |
| Platform | Windows only (like the rest of the project) | GPUI Kit is cross-platform, but nothing here is tested elsewhere. |

### Rule changes (confirmed 2026-11-17, in AGENTS.md)

1. **Ground rule 3, "copy, never depend":** an exception for `gpui-kit` and the GPUI snapshot crates
   it pins (`gpui-pre*`, published by GPUI Kit's maintainer from Zed's source). We pin exact
   versions (`=x.y.z`) and upgrade on purpose, like a Hermes update. Vendoring them instead is
   possible (a `vendor_gpui_kit.py` like `vendor_zed.py`), but copies a large, fast-moving library
   for no gain in stability over an exact pin and `Cargo.lock`.
2. **Ground rule 1, "port Hermes's protocol":** applies to the Tauri page only. The native UI has
   no protocol; it calls `hermes_core`.
3. **"Don't screenshot the app":** stays true for the user's app. For the native UI the agent may
   capture **its own instance** (data in `target/native-test`, never `%APPDATA%`), as cropped
   images, to check layout (see *The dev loop*). Rule 9's live checks for the native UI are the
   layout and snapshot tests, not `scripts/perf/*-check.mjs`.

## Zed dependency policy

The user's goal is to depend on Zed as little as possible. GPUI is Zed's, so "nothing" isn't
reachable while we use GPUI; the minimum is GPUI itself, and only through GPUI Kit.

| Need | Today (`crates/app`) | Native app |
|---|---|---|
| UI framework | `vendor/zed/gpui*`, `scheduler`, `sum_tree`, `collections`, `refineable`, `http_client`, async-task fork | `gpui-kit` from crates.io (it pins and re-exports GPUI) |
| Components | `vendor/zed/ui`, `ui_macros`, `component`, `menu`, `icons` + our own widgets | GPUI Kit components + our design layer (`crates/native/src/design/`) |
| Themes | `vendor/zed/theme`, `syntax_theme` + `crates/themes` | Our own reader for Zed's **theme JSON format** (serde structs written here) mapped onto GPUI Kit's theme. Theme files ship with their licence files. No Zed code. |
| Markdown | `crates/markdown` (from Zed's, uses `ui`, `theme`, `sum_tree`, `mermaid_render`) | GPUI Kit's markdown view, with our extensions (see CP4.1) |
| Code highlighting | `crates/highlight` (tree-sitter) | GPUI Kit's tree-sitter highlighting; `crates/highlight`'s grammar list as the reference for which languages to enable |
| Fuzzy matching | `vendor/zed/fuzzy` | `nucleo-matcher` (crates.io, from Helix), or what is already in `Cargo.lock` |
| Terminal emulation | — | `alacritty_terminal` (crates.io, Apache-2.0) + our own GPUI renderer. Zed's `terminal_view` may be read for reference, not copied. |
| Logging | `zlog`, `ztracing` | `tracing` (already used everywhere else) |

When `crates/app` is retired (Phase 9) the whole `vendor/zed/` tree, the async-task fork,
`crates/markdown`, `crates/highlight`, `crates/themes` and `scripts/vendor_zed.py` go with it.
Until then the workspace builds **two** GPUI copies (vendored for `crates/app`, GPUI Kit's for
`crates/native`). CP0.1 checks that this works (a duplicate `links =` key between them would make
Cargo refuse; the fallback is moving `crates/app` out of the default workspace members).

## Architecture

```
┌────────────────────────── hermes-native.exe ─────────────────────────────┐
│ main thread: GPUI app ── window ── Workspace view                        │
│   ├─ Sidebar (projects → threads)                                        │
│   ├─ ThreadView (header, timeline, composer)                             │
│   ├─ RightPanel (tabs: Diff, Files, Terminal, Memory…)                   │
│   ├─ TerminalDrawer, CommandPalette, Settings page, toasts               │
│   └─ design layer (Button, IconButton, ListRow, Panel…) over GPUI Kit    │
│          │ calls (async, on the tokio runtime)   ▲ events (channel)      │
│          ▼                                        │                      │
│ tokio runtime ── hermes_core::Core ───────────────┘                      │
│   sessions, turns, approvals and questions (UserBridge), git review,     │
│   files, terminal (PTY), attachments, settings, skills, memory, staging, │
│   curator, usage, commands, export, import                               │
│          ▼                                                               │
│   hermes_runtime / hermes_tools / hermes_memory / hermes_store / …       │
└──────────────────────────────────────────────────────────────────────────┘
       crates/desktop (Tauri page gateway, ACP agent) = adapters over hermes_core
```

- **`hermes_core`** (Phase 1) is the backend's application layer: typed Rust calls and one event
  stream, no JSON, no protocol. It is extracted from `crates/desktop`, where most of that logic
  lives today, mixed with Hermes's JSON shapes (`git_review.rs`, `files.rs`, `terminal.rs`,
  `attachments.rs`, `skills.rs`, `learning.rs`, `endpoints.rs`, `aux_models.rs`, `fallback.rs`,
  `transcript.rs`, `hub.rs`, `gateway/sessions.rs`, `gateway/turn.rs`…). Each module splits into
  the logic (moved to `hermes_core`) and the protocol shape (stays in `crates/desktop`).
- **Threads between GPUI and tokio.** GPUI owns the main thread and its own executor; the runtime
  needs tokio. `Core` holds a `tokio::runtime::Handle`; views call `core.spawn(...)` and get results
  back through GPUI's `cx.spawn` + a oneshot channel. Events from the runtime arrive on one channel
  that a GPUI task drains into the models (`Entity<ThreadState>` per open thread). Ground rule 7
  still holds inside `hermes_core`.
- **State.** One GPUI entity per concern: `AppState` (projects, thread list, settings snapshot),
  `ThreadState` per open thread (messages, streaming part, pending approval, composer draft),
  `PanelState` (right panel tabs). Views read entities and never call the runtime directly.
- **Persistence of UI state** (sidebar width, open tabs, collapsed projects, drafts) in a side file
  `native-ui.json` in the data folder (ground rule 6), written debounced.

## The look: t3code's structure, Zed's feel

Sources in t3code (`../t3code/apps/web/src`) to read before each screen; every native module that
copies a layout cites its source in its header, like the Hermes port map
(`//! t3code: \`components/Sidebar.tsx\` (...)`).

| Screen | t3code reference | Native module |
|---|---|---|
| App frame, sidebar layout | `components/AppSidebarLayout.tsx`, `components/ui/sidebar.tsx` | `workspace.rs` |
| Sidebar | `components/Sidebar.tsx`, `components/sidebar/*` (`SidebarThreadRow`, `SidebarProjectFolderHeader`), `ThreadStatusIndicators.tsx` | `sidebar/` |
| Thread header, branch bar | `components/chat/ChatHeader.tsx`, `BranchToolbar.tsx`, `GitActionsControl.tsx` | `thread/header.rs`, `header_data.rs` (CP4.3: the title bar's middle; git state and the model as chips, no separate branch bar) |
| Timeline | `components/ChatView.tsx`, `components/chat/ChatCanvas.tsx`, `ChatMarkdown.tsx`, `CommandBlock.tsx`, `chat/ChangedFilesTree.tsx` | `thread/` (CP4.1: `view.rs`, `model.rs`, `markdown.rs`; CP4.2: `tools.rs`, `work.rs`, `cards.rs`, `ansi.rs`, `diff.rs`) |
| Composer | `components/chat/ChatComposer.tsx`, `Composer*` (model and effort, command menu, pending approval, user input panel, attachments) | `thread/composer/` |
| Right panel | `components/RightPanelTabs.tsx`, `rightPanelStore.ts`, `DiffPanel.tsx`, `components/diffs/*`, `components/files/*` | `panel/` |
| Terminal drawer | `components/ThreadTerminalDrawer.tsx` | `terminal/` |
| Command palette | `components/CommandPalette*.tsx` | `palette.rs` |
| Settings | `routes/_chat.settings.*.tsx`, `components/settings/*` | `settings/` |
| Usage | `components/usage/*` | `settings/usage.rs` |
| Empty and first-run states | `NoProjectsHero.tsx`, `NoActiveThreadState.tsx`, `onboarding/*` | `onboarding.rs` |
| Tokens (colours, radius, sizes) | `index.css` (`:root`, `.dark`), `components/ui/button.tsx` (size scale) | `design/tokens.rs` |

**What we keep from t3code:** the frame, sidebar, thread view, composer, right panel tabs (diff,
files, terminal), terminal drawer, command palette, settings page, usage page, toasts, keyboard
shortcuts, the control sizes (micro 20px, xs 24px, sm 28px, default 32px), `--radius: 0.5rem`, the
zinc neutral palette as the default light/dark theme, Lucide icons (GPUI Kit ships them).

**What we drop:** everything about other providers and accounts (Codex, Claude, Cursor, providers
list, Clerk, cloud, pairing, devices, mobile), pull requests and issues (GitHub/GitLab), the browser
preview panel and the device panel, voice, the guest/share links, worktree-per-thread environments.
Some may come back later; none is in this plan.

**What we add (Hermes features):** memory (MEMORY.md, USER.md, tiers, notes), skills (list,
view, enable, curator status), staging (writes waiting for the user), learning signals and review
results, budgets and the request ledger, provider fallbacks, auxiliary models, toolsets, the
sandbox, import from the original Hermes, `/` commands from our catalog.

**The Zed half, concretely:**
- Every user-visible action is a GPUI `action!` with a name; the palette lists them with their
  keybindings; keybindings live in one table (`keymap.rs`), later user-editable.
- No loading state for local data: lists render from what the core already has; only model calls
  show progress.
- The timeline is a virtual list with variable heights; streaming appends without re-laying out
  earlier messages; scroll stays pinned to the bottom while streaming unless the user scrolls up.
- Monospace for code, paths and commands; text selection works inside every message and code block;
  every message and code block has a copy button.
- Panels resize with exact handles and remember their size; nothing jumps when a panel opens.
- Themes: Zed theme JSON files load as-is (bundled One and Ayu + the user folder
  `<data>/themes/`, re-read when a file changes; `theme/`), mapped onto t3code's tokens
  (`design/tokens.rs`); t3code's own light and dark are the defaults. Light and dark follow the
  system unless set (the setting is CP2.2's). The folder reading and watching are built and
  tested (CP2.1) and, since CP2.2, called by the app (`app.rs`): the user's themes folder is read at
  start and watched while it runs; the theme follows the system's light or dark until one is
  picked (it is then remembered in `native-ui.json`). Fonts: Inter bundled, Consolas for code (`theme/fonts.rs`).

## The dev loop

Most of the earlier pain was round trips: change, rebuild, look, repeat. Checks happen in this
order, cheapest first. Image tokens cost about one per 750 pixels (a full 1920×1080 window is about
2,800 tokens, a 400×200 component crop about 100), so images are small, rare and looked at by a
subagent.

1. **Design layer by construction.** Views only use `design/` components with the token scale, so
   most misalignment can't happen.
2. **Layout assertions (text, every change).** A debug mode dumps the element tree with names
   (`debug_selector`), bounds, colours and state as compact text. Tests assert on it: "toolbar
   buttons share top and height", "no label overflows", "hovered row uses the hover colour". These
   run in `cargo test -p hermes-native`.
3. **Image snapshots (every change, no tokens unless something changed).** The gallery renders each
   component (as a grid of its states: normal, hover, pressed, focused, disabled) and each screen
   with fixture data to PNG, and compares with approved baselines in `crates/native/snapshots/`. A
   difference produces a cropped diff image; only that is looked at.
4. **The gallery** (`hermes-native-gallery.exe`, like Zed's storybook and GPUI Kit's `story`): every
   component and screen with fake data, both themes. Polishing happens there, not in the full app.
5. **A subagent looks at images** and answers in text ("row 3: icon 2px below the label"), so
   images never enter the orchestrating session. Full-screen captures only at the end of a
   checkpoint.
6. **The user's checklist** at the end of each checkpoint, for what only a person can judge (feel
   of hover, scrolling, animation).

How images are made (decided in CP0.2): **off-screen, no window.** GPUI's test platform renders
through `gpui-pre-wgpu`'s headless renderer (Vulkan or DirectX 12), with the real DirectWrite text
system; GPUI Kit's own image tests skip Windows only because GPUI wires that renderer on Linux.
The harness crops each story to its element. Never the user's window, and no `PrintWindow`
fallback was needed. How to run and read the checks: [docs/TESTING.md](../TESTING.md) §6.

## Effort

The earlier GPUI app was about 16,000 lines and covered less. Expect 25,000–40,000 lines for parity
with the Tauri app, plus the `hermes_core` extraction (a refactor of about 10,000 lines of
`crates/desktop`). [PHASES.md](PHASES.md) splits it into ten phases of one to three checkpoints.

## Risks

| Risk | What we do |
|---|---|
| GPUI Kit or GPUI changes its API | Exact pins; upgrades are their own commits with the gallery and snapshots as the check |
| Two GPUI copies in one workspace don't build together | CP0.1 checks first; fallback: `crates/app` out of `default-members`, or retire it early |
| Windows-specific GPUI bugs (IME, DPI changes, custom title bar, focus) | CP0.1 spike covers IME and DPI; title bar in CP2.2 |
| Selecting text across several messages | Not built in GPUI: selection works within a message; copy buttons per message and per thread. Revisit only if the user needs it |
| Terminal emulator is large | Its own checkpoint (CP6.3); the drawer can ship later than the rest |
| Off-screen rendering for snapshots | Works (CP0.2: wgpu headless renderer on Windows); fallback if a GPUI update breaks it: capture of our own on-screen instance |
| `hermes_core` extraction breaks the page or ACP | Pure refactor: `gateway/rpc_table_tests.rs`, the ACP tests and the live checks must stay green at every checkpoint |
