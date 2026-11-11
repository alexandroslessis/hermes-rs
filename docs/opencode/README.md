# Hermes for opencode: memory, skills and self-improvement as an opencode plugin

Status: **planned (2026-11-11), nothing built.** This folder is the plan and, once built, the
reference: read it before changing the code, and keep it true.

| File | What it is |
|---|---|
| [README.md](README.md) | This file: the goal, the decisions, the architecture, the sidecar protocol, the feature map |
| [PHASES.md](PHASES.md) | The implementation, phase by phase: tasks, files, functions, tests, done criteria |

Read [AGENTS.md](../../AGENTS.md) first: every ground rule there (port Hermes, cite the source,
copy never depend, never block async workers, frozen store schema, leave the docs true) applies
here too.

## The goal

[opencode](https://opencode.ai) stays exactly what it is: its agent loop, its tools (bash, edit,
read, grep, LSP…), its providers and auth, its TUI, its permissions. We add what Hermes has and
opencode doesn't, using the hermes-rs code that already implements it:

| Hermes feature | What the user sees in opencode |
|---|---|
| **Persistent memory** (`MEMORY.md`, `USER.md`) | Every new opencode session starts knowing what earlier sessions saved; the model has a `memory` tool |
| **Session recall** | A `session_search` tool over past opencode sessions (FTS5) |
| **Skills** (Hermes's library + ones the agent writes) | The skills index in the system prompt; `skills_list`, `skill_view`, `skill_manage` tools |
| **Background review** (self-improvement) | Every N turns, a hidden review run saves memories and writes/patches skills from what just happened; a toast says what changed |
| **Curator** | Idle skills go stale, then to the archive; optional consolidation into umbrella skills |
| **`/refine`, `/learn`** | opencode commands that run a review on request |

What we do **not** bring: Hermes's agent loop, its file/shell/web tools, its providers
(`crates/llm`, `crates/agent`, the turn code in `crates/runtime`), its compression, its UI.
opencode has all of those.

## Why Rust can't be the whole plugin

opencode loads plugins as **TypeScript/JavaScript modules** only (files in `.opencode/plugins/`,
`~/.config/opencode/plugins/`, or npm/local packages listed under `plugins` in
`opencode.jsonc`). There is no native-plugin path. And an MCP server alone (which opencode can run,
and which could be pure Rust) can only add tools. It cannot inject memory into the system prompt,
see a turn end, or start a review: those need plugin **hooks**.

So the work splits in two:

- **`hermes-opencode.exe`** (Rust, new crate `crates/opencode`). This is where all the logic
  lives: the memory store, the skills store, the curator state, the session index, the nudge
  counters, the prompts.
- **The shim** (TypeScript, about 200–300 lines, `crates/opencode/plugin/`). It registers the
  hooks and tools and forwards each to the sidecar. It holds no logic of its own. Python analogy:
  a `ctypes`-style binding whose real work happens in another process. C++ analogy: a header of
  thin wrappers around RPC calls.

## Decisions (made while planning; the user may override any of them)

| # | Decision | Why | Rejected alternative |
|---|---|---|---|
| D1 | **A sidecar process.** The shim spawns `hermes-opencode.exe` once per opencode server and talks **newline-delimited JSON-RPC 2.0 over its stdin/stdout**, hand-written with `serde_json` like `crates/desktop/src/acp/` (no SDK). | A crash in Rust can't take opencode down. Builds as an ordinary exe. It's the same protocol style the repo already knows from ACP. | **napi-rs native addon** loaded into opencode's JS runtime: per-platform `.node` builds, a panic kills opencode, and a Bun vs Node ABI question. **Pure MCP server**: no hooks (see above). |
| D2 | **Model calls go through opencode, never through the sidecar.** Reviews and curator consolidation run as opencode sessions, driven by the shim (`ctx.session.*`) with the user's opencode provider and model. The sidecar only supplies prompts and executes the Hermes tools. | One set of providers and keys (opencode's). The sidecar doesn't need `crates/llm` or the agent loop. Reviews show the same model the user picked. | The sidecar calls an OpenAI-compatible endpoint with `hermes_runtime::review`: duplicates provider config and drifts from what the user chose in opencode. |
| D3 | **Tools are registered through the plugin** (`ctx.tool.transform` → `editor.add`), each `execute` forwarding to the sidecar's `tool.call`. | Hooks and tools share one process and one state (the nudge counters must see `memory`/`skill_manage` calls). The tools look native in opencode. | Registering them through an MCP server: a second process, and the counters can't see the calls without extra plumbing. (MCP mode can still come later for other clients; PHASES Phase 7.) |
| D4 | **Shared memory and skills, separate transcripts.** By default the sidecar uses the desktop app's data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`) for `memories/` and `skills/`, so opencode and the desktop app are **one agent with one memory**. opencode transcripts go to their own `opencode/sessions.db` (same schema, opened with `hermes_store::Store::open`). Plugin option `home` overrides the folder. | The user wants one agent that learns everywhere. Putting opencode sessions in the desktop's `sessions.db` would flood its sidebar and offer to resume sessions the desktop runtime can't continue faithfully. A separate file keeps the frozen schema untouched (ground rule 6). | One shared `sessions.db`: possible later behind an option. A fully private data folder: two disconnected memories. |
| D5 | **The memory snapshot is frozen per session**, as in Hermes: the sidecar renders the memory/user blocks on the session's first model call and returns the same bytes for every later call of that session. Writes made during the session show up in the *next* session. | Prompt caching (ground rule 8) and Hermes's behaviour (`MemorySnapshot` is taken at session start). | Re-rendering every call: breaks the provider's prompt cache on every memory write. |
| D6 | **Hermes's skills stay Hermes's.** The sidecar owns `<home>/skills/` and serves it with Hermes's own tools (`skills_list`, `skill_view`, `skill_manage`) and Hermes's index in the system prompt. We don't copy skills into opencode's skill folders. | Usage tracking, pinning, archiving and the curator all depend on views and writes going through `SkillStore`. A copy would drift. | Writing skills into `~/.config/opencode/skills/` so opencode's native `skill` tool loads them: no usage tracking, two copies to keep in sync. |
| D7 | **One curator at a time, across processes.** A lock file `<home>/skills/.curator.lock` is used by both the desktop gateway and the sidecar. The desktop app keeps its own schedule. The sidecar runs the curator only when the lock is free and `curator_due` says so. | Two processes must not rewrite the same skills folder at once (the same concern as ACP D8). | Never running the curator from opencode: users who only use opencode would never get curation. |
| D8 | **Background writes stay "unattended".** A review run uses `MemoryStore::unattended_tool` (adds go through; patches and removals are staged), exactly as the desktop review does. Staged writes are approved in the desktop app's Memory screen, or with the `/hermes-pending` command (Phase 6). | Hermes's `_background_delete_gate`: a background model must not silently delete what the user saved. | Applying everything: a review run could wipe memory. |
| D9 | **Stdout is the protocol** (the same rule as ACP D6). The sidecar logs to `<home>/logs/opencode.log` and stderr. The shim forwards stderr to opencode's log at debug level. | One stray line breaks the shim's parser. | — |
| D10 | **The shim is opencode plugin API V2, Promise style** (`Plugin.define({ id, async setup(ctx) })`). | V2 is the current API: `ctx.session.hook("context")`, `ctx.tool.transform`, `ctx.agent.transform`, `ctx.event.subscribe`, `ctx.storage`. V1's `server()` is ignored by V2. | V1 (`tool.execute.before` style), or Effect-style plugins: more API to track for no gain in a thin shim. |

Things deliberately **left out** (say so in the module docs when you get there): Hermes's
compression (opencode compacts with its own logic), the desktop app's approval UI, Hermes's
`clarify`/`todo`/checkpoint tools (opencode has its own), profiles, and any UI beyond toasts and
commands.

## Architecture

```
opencode server process (Bun/JS)
│
├─ opencode agent loop ── its own tools, providers, permissions (untouched)
│
└─ plugin "hermes" (crates/opencode/plugin/hermes.ts)       ← thin shim, no logic
     setup(ctx):
       spawn hermes-opencode.exe ───────────────── stdio JSON-RPC ─────┐
       ctx.tool.transform   → memory, session_search, skills_list,     │
                              skill_view, skill_manage                 │
       ctx.agent.transform  → hermes-review, hermes-curator agents     │
       ctx.session.hook("context")    → system parts (memory, skills)  │
       ctx.session.hook("prompt")     → turn.started (+ /learn)        │
       ctx.tool.hook("execute.after") → tool.observed                  │
       ctx.session.hook("compaction") → keep memory note               │
       ctx.event.subscribe            → turn end, title, deletes       │
       review/curator runner: ctx.session.create/prompt/delete         │
                                                                       ▼
hermes-opencode.exe (Rust, crates/opencode)
   rpc.rs       JSON-RPC loop (stdin → dispatch → stdout)
   context.rs   frozen per-session system parts  ← hermes_prompts::system, hermes_memory
   tools.rs     tool.call dispatch               ← hermes_memory::tool, hermes_tools::{skills,skill_manage,recall}
   transcript.rs opencode messages → hermes_llm Message → opencode/sessions.db  ← hermes_store
   nudges.rs    per-session counters             ← hermes_runtime::review::Nudges (shared logic)
   review.rs    review prompt + result summary   ← hermes_prompts::review, hermes_runtime::review::summarize
   curator.rs   transitions, lock, prompt        ← hermes_tools::curator, hermes_prompts::review::curator_message
      │
      └── <home>/memories/  <home>/skills/  <home>/opencode/sessions.db  <home>/logs/opencode.log
          (shared with hermes-desktop.exe)   (opencode only)
```

### How a turn flows

1. The user sends a message. The **`prompt` hook** calls `turn.started {sessionID}`. The sidecar
   ticks the memory nudge (`Nudges::tick_memory`). If the text is `/learn …` (Hermes's
   `learn_request`), the shim also queues an on-demand review.
2. Before every model call, the **`context` hook** calls `session.context {sessionID, agent, directory}`
   and pushes the returned text parts onto `event.system`. The first call of a session builds and
   freezes them (D5). Later calls return the cached copy. Each call also counts one model call
   for the skill nudge.
3. The model calls `memory` / `skill_manage` / … → the tool's `execute` calls `tool.call`. The
   sidecar runs the Hermes tool (in `spawn_blocking`) and returns its JSON as a string.
   **`execute.after`** reports every tool name (ours and opencode's) with `tool.observed`, so the
   counters reset as in `Nudges::finish_turn`.
4. The session goes idle (event; exact name pinned in Phase 0). The shim calls
   `turn.finished {sessionID}` and then `transcript.sync` with the session's messages
   (`ctx.session.context`). The sidecar stores them and answers whether a review is due:
   `{review: {memory: bool, skills: bool, prompt: "..."}}` or `{review: null}`.
5. **Review run** (shim): fork the session if `ctx` exposes forking (best: same prefix, so the
   provider's prompt cache is reused, as Hermes does). Otherwise create a child session titled
   `hermes: review` and send it the transcript summary followed by the review prompt. Either way
   it runs with agent **`hermes-review`**, whose tools are only `memory` (unattended) and the
   skill tools. Its tool calls reach the sidecar marked `review: true`. When it finishes, the shim
   calls `review.finished`, the sidecar answers with a one-line summary
   (`hermes_runtime::review::summarize`), the shim shows a toast and deletes the review session.
6. **Curator** (shim timer, every 30 min while idle): `curator.check` → the sidecar takes the lock,
   applies the deterministic transitions, and, only if consolidation is on, returns a prompt that
   the shim runs like a review with agent **`hermes-curator`**. Then `curator.finished`.

## The sidecar protocol

JSON-RPC 2.0, one object per line, both directions over stdio. The shim sends requests; the sidecar
answers. The sidecar sends no requests of its own. It may send `log` notifications
(`{"method":"log","params":{"level","message"}}`), which the shim forwards to opencode's log.
Errors use `-32601` (unknown method), `-32602` (bad params), and `-32000` (tool/store failure,
with a readable `message`).

| Method | Params | Result | Rust side |
|---|---|---|---|
| `initialize` | `{protocol: 1, home?, directory, projectID?, options}` | `{protocol: 1, version, tools: [{name, description, parameters}], agents: {review: {prompt, tools}, curator: {...}}}` | open stores, seed bundled skills in the background (`SkillStore::seed_bundled_in_background`) |
| `session.context` | `{sessionID, agent, directory, model?}` | `{system: [string], frozen: bool}` | memory snapshot + `memory_guidance` + skills `index_prompt()` (+ context files? no: opencode loads AGENTS.md itself) |
| `turn.started` | `{sessionID, text}` | `{learn?: string}` | `Nudges::tick_memory`; `hermes_prompts::review::learn_request` |
| `tool.call` | `{sessionID, name, args, review: bool}` | `{output: string, title?: string}` | `MemoryStore::tool` / `unattended_tool`, `SkillStore::{list_tool, view_tool, manage_with}`, `session_search` |
| `tool.observed` | `{sessionID, name}` | `{}` | counter resets (`memory`, `skill_manage`) |
| `turn.finished` | `{sessionID}` | `{review: null \| {memory, skills, prompt}}` | `Nudges::finish_turn` (call-count variant) |
| `transcript.sync` | `{sessionID, title?, directory, parentID?, messages: [OpencodeMessage]}` | `{stored: n}` | convert → `Store::replace_messages`; FTS is updated by the store |
| `review.request` | `{sessionID, focus?}` | `{memory, skills, prompt}` | `/refine`: `review_message(true, true, focus)` |
| `review.finished` | `{sessionID, reviewSessionID, messages}` | `{summary: string \| null}` | `summarize` |
| `session.deleted` | `{sessionID}` | `{}` | drop counters and frozen context; keep the transcript (Hermes keeps history) |
| `curator.check` | `{now?}` | `{ran: bool, summary?, consolidate?: {prompt}}` | lock + `apply_automatic_transitions` + `curator_candidates` + `curator_message` |
| `curator.finished` | `{messages}` | `{summary, report?}` | report as in `Runtime::write_curator_report` |
| `pending.list` / `pending.resolve` | `{}` / `{id, approve: bool}` | the staged writes / result | `MemoryStore::{pending, approve, discard}`, `SkillStore::{pending, approve, discard}` |
| `shutdown` | `{}` | `{}` | flush, release the curator lock, exit |

`OpencodeMessage` is whatever `ctx.session.context` returns, passed through **unchanged**. The
conversion lives in Rust (`transcript.rs`), so an opencode format change is a Rust change with a
unit test, not a shim change.

## Feature map (what's reused, what's new)

| Piece | Reused as-is | Changed in place (small, shared with desktop) | New in `crates/opencode` |
|---|---|---|---|
| Memory store and tool | `hermes_memory::MemoryStore` (store + `tool`/`unattended_tool`) | — | per-session frozen snapshot cache |
| System prompt parts | `hermes_prompts::system::memory_guidance`, `tool_guidance_block` | make the two `pub` (today private) | assembling the parts list |
| Skills | `hermes_tools::skills::SkillStore` (`index_prompt`, `list_tool`, `view_tool`, usage) | — | — |
| Skill writes | `SkillStore::manage_with` (`skill_manage.rs`) | — | `WriteOrigin` for review runs |
| Recall | `hermes_tools::recall` | today `SessionTools::session_search` is `pub(crate)` and needs a whole `SessionTools` (shell, hints…): split out a `pub fn session_search(store: &Store, current: &str, args: &Value)` the method calls | — |
| Session index | `hermes_store::Store` (+ `search.rs`) | — | `transcript.rs` (opencode → `Message`) |
| Nudges | `hermes_runtime::review::Nudges` | move `Nudges` (and `summarize`) to a small module that doesn't need the turn runtime, e.g. `hermes_prompts::review` or a new `hermes_runtime::nudges` behind no extra deps; add `finish_turn_calls(&[Vec<String>])` | — |
| Review prompt | `hermes_prompts::review::{review_message, learn_message, learn_request}` | — | — |
| Curator | `hermes_tools::curator` (transitions, candidates, state), `hermes_prompts::review::curator_message` | the cross-process lock (D7) used by `crates/desktop/src/maintenance.rs` too | the `curator.check` flow |
| Agent loop, providers, tools | — | — | none: opencode's |

**Licence:** `crates/opencode` is GPL-3.0-or-later like the rest of the workspace. The shim is
loaded by MIT opencode but runs no GPL code in-process; the sidecar is a separate program. Ship
the sidecar with `THIRD_PARTY_NOTICES.md` and the licence, as the installer does.

## Risks and open questions (settled in Phase 0 unless noted)

1. **Turn-end signal.** The V2 docs name the event stream but not a "session idle" event. Find
   the event (or `ctx.session` status) that marks the end of a turn. Fallback: debounce, where a
   turn has ended when no `context` call or tool call has arrived for 2 s after the last assistant
   message completes.
2. **Forking from a plugin.** The `fork` hook exists, so the server can fork. Check whether `ctx`
   (a server client) can request it. If not, use child session + rendered transcript (step 5).
3. **Review session visibility.** Review/curator sessions must not show in the user's session list
   or be indexed by us. Options: `parentID` (child sessions are hidden in the TUI), deletion after
   the run, and the sidecar skipping `transcript.sync` for sessions it started.
4. **Tool permission for the review agent.** `ctx.agent.transform` must restrict `hermes-review` to
   our tools. Confirm that agents can carry a tool allow-list, or deny the rest with the
   permission `evaluate` hook keyed on `agent`.
5. **Shim ↔ binary location.** Default: the shim looks for `hermes-opencode.exe` next to itself,
   then on `PATH`, then `options.binary`. The installer drops both into
   `%LOCALAPPDATA%\hermes-rs\opencode\`.
6. **opencode API drift.** The plugin API changed once already (V1 → V2). Keep the shim thin, pin
   the `@opencode-ai/plugin` version in `package.json`, and record the opencode version each
   phase was checked against in PHASES.md.
7. **Windows path quirks** in session `directory` values (drive-letter case, `/` vs `\`): normalise
   in Rust, the same way `crates/tools/src/paths.rs` does.
