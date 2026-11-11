# Hermes for opencode, phase by phase

Each phase ends in something the user can try in opencode, with its tests green and its docs
true. Do them in order. Don't start a phase before the previous one is committed. Within a phase,
do the steps in order and run the phase's tests before moving on.

Before Phase 0, read: [README.md](README.md) (decisions, architecture, protocol),
[AGENTS.md](../../AGENTS.md), [docs/acp/README.md](../acp/README.md) (the same stdio JSON-RPC
pattern, already built), `crates/desktop/src/acp/` (copy its line loop and logging setup), and
from the reused crates: `crates/memory/src/{store,tool}.rs`, `crates/tools/src/{skills,skill_manage,recall,curator}.rs`,
`crates/prompts/src/{system,review}.rs`, `crates/runtime/src/{review,curator}.rs`.

Rules that matter most here:
- **Nothing on stdout but protocol lines** (README D9). A test checks it.
- **Never block the async workers.** Every store, file or git call from a handler goes through
  `tokio::task::spawn_blocking` (ground rule 7). `SkillStore` commits to git; `Store` is SQLite.
- **Changes to shared crates stay small** and keep the desktop app's behaviour byte-for-byte
  (same prompts, same tool output). Each one gets its own commit with a test, and the desktop
  tests (`cargo test -p hermes-desktop`, `-p hermes_runtime --test turns`) must stay green.
- **The shim holds no logic.** If a `.ts` function grows an `if` that isn't about plumbing, move
  the decision into Rust and return it in the RPC result.
- **Don't touch the user's data or opencode setup.** Tests use `HERMES_RS_HOME` set to a temp
  folder, and an opencode config in a temp `XDG_CONFIG_HOME` / project folder. Build into
  `CARGO_TARGET_DIR=target/claude`.
- When behaviour isn't settled here, look at Hermes (`../hermes-agent/agent/background_review.py`,
  `agent/curator.py`, `tools/memory_tool.py`, `tools/skill_manager_tool.py`,
  `tools/session_search_tool.py`), then at how the desktop app does it, then ask the user.

---

## Phase 0: Spike. Pin down the opencode API and the loop

**Goal:** a throwaway shim proves every hook we need exists and behaves as README assumes, and
`hermes-opencode.exe` answers `initialize`. Nothing user-visible yet beyond a log line.

1. **Install opencode** in an isolated place (with the user's OK: it's a download):
   `npm i -g opencode-ai` into `tools/node`, or the Windows release zip under `target/claude/opencode/`.
   Record the version in this file (top of the phase).
2. **Get the plugin types.** `npm pack @opencode-ai/plugin` into a scratch folder and read its
   `.d.ts`. Write down in README "Risks" the answers to:
   - the event that ends a turn (`session.idle`? `session.status` with `idle`?) and its payload;
   - whether `ctx.session` can fork, delete, and list children, and whether `create` takes
     `parentID` and an `agent`;
   - whether `ctx.agent.transform` can give an agent a tool allow-list (or the permission
     `evaluate` hook sees the agent name);
   - the shape `ctx.session.context` returns (save one real transcript as a fixture:
     `crates/opencode/tests/fixtures/opencode-session.json`);
   - whether `ctx.tool.transform` tool `input` takes JSON Schema (our `prompts/tools/*.json` are
     JSON Schema) or needs a converter;
   - how the shim can show a toast (`ctx.tui`? an event?) and register a slash command.
3. **Spike shim** (`target/claude/spike/hermes-spike.ts`, not committed): registers every hook in
   README's architecture diagram and logs each call with its payload to a file. Run opencode
   against a mock or local model for two turns with a tool call. Keep the log as the reference for
   step 2's answers.
4. **Crate skeleton** `crates/opencode` (package `hermes-opencode`, lib + bin
   `hermes-opencode`), workspace member, deps: `hermes_memory`, `hermes_tools`, `hermes_store`,
   `hermes_prompts`, `hermes_llm` (for `Message` in transcripts), `tokio`, `serde_json`,
   `anyhow`, `tracing`. Not `hermes_runtime`, `hermes_agent` or `hermes-desktop`.
   - `src/main.rs`: logging to `<home>/logs/opencode.log` + stderr (copy `acp`'s
     `init_logging`), a multi-thread tokio runtime (2 workers), `rpc::serve(stdin, stdout)`.
   - `src/rpc.rs`: line loop, parse, dispatch by method, `-32601` for unknown methods. Answer
     `initialize` with `{protocol: 1, version}`. Exit when stdin closes.
   - Module headers: `//! Hermes: ...` where a module ports Hermes, plus a note that the shim side
     is ours.

**Tests:** `crates/opencode/tests/rpc.rs`. Spawn the built exe, send `initialize`, assert the
answer. Send garbage, assert `-32700`. Send an unknown method, assert `-32601`. Close stdin,
assert a clean exit. **T-stdout:** nothing but JSON lines on stdout.

**Done when:** the README "Risks" section is updated with real answers (or a changed design where an
assumption failed), the skeleton builds with `cargo build --workspace`, and its tests pass.
Commit, and tell the user what Phase 0 found before starting Phase 1. A failed assumption may
change the plan.

---

## Phase 1: Persistent memory

**Goal:** in opencode, the model sees `MEMORY.md`/`USER.md` from the desktop app's data folder and
can save new memories with the `memory` tool. A new session sees them.

1. **Shared-crate change:** in `crates/prompts/src/system.rs` make `memory_guidance` and
   `tool_guidance_block` `pub` (no behaviour change). Commit separately.
2. **`src/home.rs`**: resolve the data folder: `options.home` → `HERMES_RS_HOME` →
   `%APPDATA%\hermes-rs`. Read the desktop app's memory settings (char limits, enabled stores,
   `write_approval`) the same way `crates/desktop` builds `MemoryConfig`. Find that code and call or
   copy it with a citation, so both processes agree.
3. **`src/context.rs`**: `SessionContexts` (a `Mutex<HashMap<SessionId, Arc<Vec<String>>>>`;
   Python analogy: a dict cache guarded by a lock). `get_or_build(session_id)` renders, once per
   session: the memory snapshot (`MemoryStore::snapshot`, both blocks) and the memory guidance.
   Returns the same `Arc` on later calls (README D5).
4. **`src/tools.rs`**: `initialize` returns the `memory` tool schema
   (`hermes_prompts::tools`). `tool.call` with `name: "memory"` runs `MemoryStore::tool` in
   `spawn_blocking`, keeping a `TurnFailures` per session, and returns the JSON as a string.
5. **The shim**, `crates/opencode/plugin/`:
   - `package.json` (`"name": "opencode-hermes"`, pinned `@opencode-ai/plugin`), `hermes.ts`,
     `README.md` (install steps).
   - `sidecar.ts`: spawn, line-buffered reader, `call(method, params)` returning a Promise keyed by
     id, restart once on crash, then disable the plugin with a logged error (opencode keeps
     working without Hermes).
   - `setup(ctx)`: `initialize`, then register tools from the returned schemas, the `context` hook
     (`event.system.push({type: "text", text})` for each part), and cleanup that sends `shutdown`.
6. **Install doc** (`plugin/README.md`): build the exe, point `opencode.jsonc` at the plugin
   folder (`"plugins": ["C:/…/crates/opencode/plugin"]`), optional `options.home`.

**Tests:**
- Rust unit tests: the context is frozen across calls even after a `memory` write; the frozen
  bytes match what the desktop system prompt would contain for the same memory (use
  `hermes_prompts::system` directly as the oracle); the memory tool round-trips through RPC.
- `crates/opencode/tests/rpc.rs`: `session.context` + `tool.call memory add` + a new session's
  context contains the entry.
- **Live check** `scripts/opencode/memory-check.mjs`: start opencode headless (`opencode serve`
  or `opencode run`, whichever Phase 0 found scriptable) with the plugin and a mock provider from
  `scripts/perf/` (OpenAI-compatible), and assert that the request body the mock receives contains
  the memory block.

**Done when:** tests green. User checklist: save a memory in the desktop app → new opencode
session → ask "what do you remember about me?" → it answers from memory; tell opencode "remember
that I prefer tabs" → the desktop app's Memory screen shows it.

---

## Phase 2: Session recall

**Goal:** opencode transcripts are indexed, and the model can search and read past sessions with
`session_search`.

1. **Shared-crate change:** in `crates/tools/src/recall.rs`, pull the body of
   `SessionTools::session_search` into `pub fn session_search(store: &Store, current: Option<&str>, args: &Value) -> ToolOutput`.
   The method calls it. The desktop output stays the same (its tests prove it). Separate commit.
2. **`src/transcript.rs`**: `fn convert(messages: &Value) -> Vec<hermes_llm::…::Message>`, which
   maps opencode parts to Hermes messages: text → text, tool part (call + result) → tool request +
   tool response, reasoning → dropped (Hermes doesn't index thinking), file → a `[file: path]`
   text stub. Unit tests over the Phase 0 fixture.
3. **`transcript.sync`**: open `<home>/opencode/sessions.db` once (`Store::open`, WAL). Create the
   session record on first sight (`NewSession` with title, cwd = `directory`), then
   `replace_messages`, in `spawn_blocking`. Skip sessions the shim marks as review/curator runs.
4. **Shim:** on the turn-end event, `ctx.session.context({sessionID})` → `transcript.sync`. On a
   title event, sync the title. On a delete event, `session.deleted`.
5. **Option** `recallDesktopSessions` (default off): also search the desktop `sessions.db`
   (opened read-only). Results say which app the session came from.

**Tests:** conversion fixtures; sync then `session_search {query}` finds a word from the fixture;
re-sync of a longer transcript replaces, not duplicates; a review session is not stored.

**Done when:** tests green. User checklist: chat about "flamingo" in one opencode session → new
session → "what did we say about flamingos?" → the model calls `session_search` and answers.

---

## Phase 3: Skills

**Goal:** Hermes's skills library works in opencode: the index in the prompt, `skills_list`,
`skill_view`, `skill_manage`, with usage tracked and every write committed to the skills history.

1. **`src/skills.rs`**: one `SkillStore` over `<home>/skills/` with the desktop's
   `SkillsConfig`. `seed_bundled_in_background` on `initialize`, which is a no-op when the desktop
   app already seeded.
2. **Context:** add `SkillStore::index_prompt()` to the frozen parts, in Hermes's order (memory,
   then skills; check `build_system_prompt` for the exact order and spacing).
3. **Tools:** `skills_list` → `list_tool`, `skill_view` → `view_tool`, `skill_manage` →
   `manage_with(args, WriteOrigin::Agent)` (or the matching origin), all in `spawn_blocking`.
4. **Name clash:** opencode has its own `skill` tool. Ours are named as Hermes names them
   (`skills_list`, `skill_view`), so they don't collide. Note in `plugin/README.md` that both
   exist.
5. **Hermes slash-skill invocation** (`/<skill-name> instruction` in Hermes): in the `prompt`
   hook, ask the sidecar (`skill.invocation {text}`) and, if it matches, replace the prompt text
   with `SkillStore::invocation_message`. Only if Phase 0 found the `prompt` hook can edit text.
   Otherwise drop this step and note it.

**Tests:** RPC tests for each tool against a temp skills folder; the index appears in the frozen
context; a `skill_manage create` lands in git history (`SkillStore::history`).

**Done when:** tests green. User checklist: "list your skills" → Hermes's bundled skills; ask
for a skill's content → `skill_view`; "make a skill for X" → it appears in the desktop app's
Skills screen.

---

## Phase 4: Background review (self-improvement)

**Goal:** every 10 user turns (memory) or 10 model calls without `skill_manage` (skills), a
hidden review runs on the user's opencode model, saves memories and writes skills, and a toast
says what changed. Same prompts and gates as Hermes.

1. **Shared-crate change:** make `Nudges` usable without the turn runtime. Move `Nudges`,
   `MEMORY_NUDGE_INTERVAL`, `SKILL_NUDGE_INTERVAL` and `summarize`'s text-only core into a module
   with no `hermes_agent` dependency (e.g. `crates/prompts/src/review.rs` next to the prompts, or
   `hermes_runtime::nudges` if the runtime crate's deps are acceptable; prefer the former). Add
   `finish_turn_calls(&mut self, calls: &[Vec<&str>]) -> bool` taking tool names per model call.
   `finish_turn(&[Message])` becomes a wrapper. Desktop behaviour unchanged. Separate commit.
2. **`src/nudges.rs`**: per-session `Nudges` plus the current turn's per-call tool names. `context`
   hook calls start a new model call, `tool.observed` appends a name, `turn.started` →
   `tick_memory`, `turn.finished` → `finish_turn_calls` → `{review: {memory, skills, prompt}}` with
   `review_message(memory, skills, None)`. Sessions opened mid-history start with
   `Nudges::resumed(user_turns)` from the synced transcript.
3. **Agents:** `initialize` returns the `hermes-review` agent definition (description, allowed
   tools: `memory`, `skills_list`, `skill_view`, `skill_manage`). The shim registers it with
   `ctx.agent.transform`, hidden from the agent picker if possible.
4. **Shim review runner** (`review.ts`), in the order Phase 0 found possible:
   fork the session (cache-friendly), else create a child session (`parentID`, agent
   `hermes-review`, title `hermes: review`) and send it the transcript summary, then
   `ctx.session.prompt({sessionID: child, text: prompt})`. Tool calls from that session carry
   `review: true` (the shim knows the child id) → the sidecar uses `unattended_tool` (README D8) and
   the review origin for skills. When it finishes: `review.finished` → toast with the summary
   (nothing when null) → delete the child session. Only one review per session at a time; a
   newer one cancels the older (as `ReviewJob.cancel` does). A review never triggers a review.
5. **`/refine [focus]` and `/learn …`**: register opencode commands if the API allows (Phase 0),
   else recognise them in the `prompt` hook. Both call `review.request` / `learn_message` and run
   the same runner, with `requested: true` (summary shown even when nothing changed).
6. **Options:** `review: true|false`, `reviewModel` (default: the session's model), intervals
   (default Hermes's 10/10, read from the desktop settings if set there).

**Tests:**
- Rust: nudge sequences give the same answers as the desktop `Nudges` tests (copy those tests
  against `finish_turn_calls`); review-marked `memory remove` is staged, not applied; summary
  text for a fixture review transcript.
- Live check `scripts/opencode/review-check.mjs` with a scripted mock provider: 10 turns → the mock
  receives a request whose last user message is the review prompt and whose tools are only ours.
  The mock answers with a `memory add` call, and `MEMORY.md` contains it afterwards.

**Done when:** tests green. User checklist: set the memory interval to 2 for the check → two turns
telling opencode a preference → a toast "Memory: saved …" → the desktop Memory screen shows it;
`/refine focus on testing habits` → a toast even if nothing changed.

---

## Phase 5: Curator

**Goal:** opencode-only users get skill curation. It never runs twice at once across opencode
and the desktop app.

1. **Shared-crate change (D7):** `hermes_tools::curator::CuratorLock`: a lock file
   `<home>/skills/.curator.lock` held with an OS file lock (`std::fs::File::try_lock`, stable since
   Rust 1.89; check the toolchain), released on drop. `Runtime::run_curator` takes it too
   (failing with "the curator is already running" when held). Separate commit with a two-process
   test.
2. **`src/curator.rs`**: `curator.check` → if `curator_due(now)` and the lock is free:
   `commit_outside_changes`, `apply_automatic_transitions`, update `CuratorState`, and, when
   consolidation is on and there are candidates, return `{consolidate: {prompt: curator_message(candidates)}}`
   while keeping the lock until `curator.finished` (or a 30-minute timeout). Write the report the
   way `Runtime::write_curator_report` does (move it to `hermes_tools::curator` if it's reusable
   without the runtime; otherwise copy it with a citation).
3. **Shim:** a timer every 30 minutes, only when no opencode session is busy. It runs the
   consolidation prompt with agent `hermes-curator` (skill tools only; deletes allowed, as in
   Hermes's curator) through the same runner as reviews, then a toast with the summary.
4. **Idle rule:** Hermes waits until the app has been idle 2 h (`MIN_IDLE_HOURS`). The sidecar
   tracks the last `turn.started` across all sessions and applies the same rule.

**Tests:** the lock test; transitions over a temp skills folder with backdated usage; the RPC
flow with consolidation on/off; `curator.check` refuses while the desktop-side lock is held.

**Done when:** tests green. User checklist: (with a backdated test skill) leave opencode idle →
the skill shows as stale in the desktop Skills screen; the desktop app and opencode open together
→ only one curator report per interval.

---

## Phase 6: Hardening and polish

1. **Pending writes:** `/hermes-pending` command lists staged memory/skill writes, and
   `/hermes-pending approve <id>` / `discard <id>` resolve them (`pending.*` RPCs).
2. **Compaction:** in the `compaction` hook, add a short note that persistent memory exists and
   unsaved facts should be saved before detail is lost (check what Hermes does around compression
   for memory, and port that instead if it differs).
3. **Crash safety:** the shim restarts the sidecar once, re-sends `initialize`, and loses only the
   in-memory counters (acceptable: they rebuild from transcripts via `Nudges::resumed`).
4. **Performance:** `session.context` must answer from cache in < 1 ms after the first call.
   `transcript.sync` must not block a turn (the shim never awaits it on the hot path).
5. **Multiple opencode servers** (two terminals): each spawns its own sidecar on the same home.
   SQLite WAL + busy timeout handles the store, the curator lock handles curation, and memory writes
   are already atomic renames with drift checks (`MemoryStore::write_file`, drift backup).
6. **Docs:** `PORTING.md` rows for every module in `crates/opencode` (with Hermes sources), an
   `AGENTS.md` "Read next" row, `docs/ARCHITECTURE.md` section, `GAPS.md` entries for what's left
   out.

**Done when:** a day of real use by the user without a sidecar crash or a duplicated review.

---

## Phase 7 (optional): Packaging and other clients

1. **Installer:** ship `hermes-opencode.exe` and the plugin folder with the desktop installer,
   plus a one-click "Enable in opencode" that adds the plugin entry to the user's
   `opencode.jsonc` (with their confirmation).
2. **npm package** `opencode-hermes` that downloads/locates the exe, so `opencode plugin add`
   works. Decide licence wording first (the shim can be MIT, the exe is GPL).
3. **MCP mode** (`hermes-opencode.exe --mcp`, via the `rmcp` server feature already in the
   workspace): the tools only (memory, recall, skills) for clients that have no plugin system.
   No hooks means no frozen memory prompt and no reviews, so document that.

---

## Estimated size

| Phase | Rust (new) | Shared-crate changes | Shim (TS) |
|---|---|---|---|
| 0 | ~200 | — | spike only |
| 1 | ~400 | ~10 | ~200 |
| 2 | ~350 | ~40 | ~30 |
| 3 | ~200 | — | ~20 |
| 4 | ~350 | ~80 | ~150 |
| 5 | ~250 | ~80 | ~40 |
| 6 | ~150 | — | ~60 |

Roughly 2,000 lines of new Rust and 500 of TypeScript. Most of the behaviour is the ~6,000 lines
of hermes-rs memory/skills/recall/curator/prompt code that already exists.
