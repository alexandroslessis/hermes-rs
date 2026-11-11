# hermes-rs — guide for coding agents

Read this file first, whole. It is the hub: what this project is, the rules that apply to every
change, the commands, and where everything else is written down. The detailed guides are in
[`docs/`](docs/); read the one for the task at hand before writing code.

## What this project is

A Windows desktop app that runs **Hermes** (NousResearch's agent, `../hermes-agent`, Python) on a
**Rust backend**. The app is `crates/desktop` (`hermes-desktop.exe`):

- **Frontend:** Hermes's *own* desktop frontend (React), built from the pinned Hermes commit in
  `HERMES_COMMIT` by `scripts/build-hermes-webapp.ps1`, shown in a Tauri (WebView2) window. We do
  not write UI. We change Hermes's frontend only through small, documented patches
  (`crates/desktop/patches/`) and a hiding plugin (`crates/desktop/plugins/hermes-rs/`).
- **Backend:** ours. `crates/desktop` serves the page and speaks the protocol Hermes's frontend
  expects (JSON-RPC over `/api/ws` + REST under `/api`); behind it are the agent runtime
  (`crates/runtime`), tools (`crates/tools`), memory (`crates/memory`), the SQLite store
  (`crates/store`), prompts (`crates/prompts`), and the provider/agent-loop layer taken from Goose
  (`crates/llm`, `crates/agent`).
- **Use case it's built for:** a local coding/agent harness on the user's own machine, talking to
  an OpenAI-compatible endpoint (Ollama, LM Studio, llama.cpp, OpenRouter…). It replaces Claude
  Code / opencode / Goose for the user. Nothing hosted, no server mode, no accounts.
- `crates/app` is an older native GPUI app: **frozen**. Don't add features to it, but don't break
  its build either (`cargo build --workspace` must pass).

## Read next, by task

| Task | Read |
|---|---|
| Any code change | [docs/CONVENTIONS.md](docs/CONVENTIONS.md), [docs/WORKFLOW.md](docs/WORKFLOW.md) |
| Understanding the code | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Testing anything | [docs/TESTING.md](docs/TESTING.md) |
| Implementing a missing feature | [GAPS.md](GAPS.md) (what's missing) → [docs/GAPS-PLAYBOOK.md](docs/GAPS-PLAYBOOK.md) (how to do each one) |
| Something behaves oddly | [docs/PITFALLS.md](docs/PITFALLS.md) — read it *before* debugging; most surprises are in it |
| Updating to a newer Hermes | [PORTING.md](PORTING.md) |
| Changing Hermes's frontend | [crates/desktop/patches/README.md](crates/desktop/patches/README.md) |
| Building the installer / shipping | [docs/RELEASING.md](docs/RELEASING.md); the user's step-by-step: [docs/INSTALLER-TUTORIAL.md](docs/INSTALLER-TUTORIAL.md) |
| Performance work | [scripts/perf/README.md](scripts/perf/README.md) |
| Backend v2 (request-frugal agent, self-improvement, security, Rust cleanup) | [docs/backend/PHASES.md](docs/backend/PHASES.md) — planned 2026-11-11; phase by phase, with checkpoints |
| ACP (using the agent from Zed / t3code: `hermes-acp.exe`) | [docs/acp/README.md](docs/acp/README.md), then [docs/acp/PHASES.md](docs/acp/PHASES.md) — built 2026-11-10 (Zed; t3code later) |
| opencode plugin (Hermes memory, skills, self-improvement inside opencode: `hermes-opencode.exe` + TS shim) | [docs/opencode/README.md](docs/opencode/README.md), then [docs/opencode/PHASES.md](docs/opencode/PHASES.md) — planned 2026-11-11, not built |

`PLAN.md` and `FEATURES.md` are from the GPUI era (2026-10/11). `FEATURES.md`'s keep/later/drop
scope still holds; `PLAN.md`'s milestones are history. `GAPS.md` is the live to-do list.

## Ground rules (every change)

1. **Protocol: port Hermes. Backend: ours.**
   - *Protocol:* the page's protocol (JSON-RPC methods, REST routes, event shapes) stays exactly
     what Hermes's frontend expects. Find how Hermes does it (its frontend's calls, the contract,
     its Python) and port the shapes. A wrong shape is worse than none (rule 5).
   - *Backend:* the agent's behaviour (loop, tools, review, curator, memory, prompts) is ours and
     may deviate from Hermes where that helps (see [docs/backend/PHASES.md](docs/backend/PHASES.md)).
     When we deviate, the module doc says so and why, and the `PORTING.md` row is marked
     `diverged`, so a Hermes update knows not to re-port it. See
     [docs/WORKFLOW.md](docs/WORKFLOW.md).
2. **Cite the source.** A module ported from Hermes says where from in its header
   (`//! Hermes: \`tools/process_registry.py\` (...)`). These lines are the port map that makes
   Hermes updates tractable. Add a row to `PORTING.md` for a new module.
3. **Copy, never depend.** Code from Hermes, Goose or Zed is copied in and adapted; never add a
   dependency on their repos or published crates. Ordinary crates.io crates are fine — prefer one
   already in `Cargo.lock`.
4. **Don't touch Hermes's tree.** `../hermes-agent` is read-only. The frontend is rebuilt from it
   by `git archive`; changes go in `crates/desktop/patches/` (rare, small, documented) or
   `plugins/hermes-rs/` (hiding).
5. **Unimplemented means "not here".** A method we don't serve answers JSON-RPC `-32601`, a route
   we don't serve answers 404 `"No such API endpoint"`; the page treats both as "older backend,
   feature absent" and hides or degrades. A *wrong* answer is worse than none. Hide UI for
   features we won't serve (`hide.css` / `plugin.js`).
6. **Shared store schema is frozen.** `sessions.db` is shared with the GPUI app. Don't change its
   schema; keep new per-session data in a JSON side file in the data folder (like
   `branches.json`, `hermes-imports.json`). Additive store *methods* are fine.
7. **Never block the async workers.** File, git, SQLite and process work from an async handler
   goes through `tokio::task::spawn_blocking`.
8. **Prompt caching.** Never rewrite past messages or the system prompt mid-session (compression is
   the one exception). Settings that change what the model sees apply from the next turn or the
   next session.
9. **Tests and a live check for every feature.** Unit tests in the module; a runtime turn test when
   the agent loop is involved; a `scripts/perf/*-check.mjs` live check against a separate
   instance for anything the page calls. See [docs/TESTING.md](docs/TESTING.md).
10. **Leave the docs true.** A feature commit also updates `GAPS.md` (✅ + date), `PORTING.md`
    (map row), `scripts/perf/README.md` (new check), `crates/desktop/patches/README.md` (new
    patch), and any doc it makes stale.

## Working with the user

- The user (Alexandros) is strong in Python, intermediate in C++, new to Rust. When explaining
  Rust, relate it to Python/C++ (trait ≈ abstract base class / concept, `Arc<Mutex<T>>` ≈ a
  shared_ptr to a locked object, crate ≈ package, cargo ≈ pip + venv + make).
- **Never touch the user's running app or data.** Their app runs from `target/debug` or
  `target/release` with data in `%APPDATA%\hermes-rs`. Build with
  `CARGO_TARGET_DIR=target/claude` (theirs locks `target/debug`), test against your own instance
  (`scripts/perf/perf.ps1`, data in `target/perf`, port 47810), and only ever stop processes whose
  path is under `target\claude` (`perf.ps1 -Do stop` does exactly that).
- Don't screenshot or drive the user's app. Automated checks run against your own instance through
  its DevTools port (text and DOM, not screenshots). After a UI-affecting change, end with a
  numbered checklist for the user: where to click, what should happen.
- An API key for real-model tests may be in the environment variable `hermes_rs_or_api_key`
  (OpenRouter). Never print it, log it or write it to a file. Prefer the mock providers.
- Commit when a feature is done and checked; push only when asked. Commit messages end with the
  `Co-Authored-By:` trailer the session tells you to use.
- The user asked for "no performance checks for the time being" (2026-11-04): don't start perf
  investigations unprompted, but don't regress the hot paths either.

## Commands

```powershell
# Once per HERMES_COMMIT (and after editing a patch): Hermes's frontend → target/hermes-src
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-hermes-webapp.ps1   # run from PowerShell, not Git Bash

$env:CARGO_TARGET_DIR = "target/claude"
cargo build -p hermes-desktop                    # the app (debug)
cargo test -p hermes-desktop                     # desktop backend unit tests
cargo test -p hermes_runtime --test turns        # agent-loop tests against a mock HTTP provider
cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts
cargo build --workspace                          # everything, including the frozen GPUI app

# Your own instance for live checks (see docs/TESTING.md)
$env:HERMES_PERF_EXE = "$PWD\target\claude\debug\hermes-desktop.exe"
powershell -File scripts/perf/perf.ps1 -Do start
tools/node/node.exe scripts/perf/<feature>-check.mjs 47810
powershell -File scripts/perf/perf.ps1 -Do stop
```

Tools on this machine: Node 26 in `tools/node` (not on PATH: call `tools/node/node.exe`); Python
via `~/.local/bin/uv.exe run --no-project ...` (`python` on PATH is the Microsoft Store stub);
`git`; no `gh`. Package names: `hermes-desktop`, `hermes_runtime`, `hermes_tools`,
`hermes_memory`, `hermes_store`, `hermes_prompts`, `hermes_llm`, `hermes_agent`.

## Where things are

| Path | What |
|---|---|
| `crates/desktop/src/` | The backend the page talks to: one module per feature (see ARCHITECTURE) |
| `crates/desktop/patches/` | Our patches to Hermes's frontend, applied in name order by the webapp build |
| `crates/desktop/plugins/hermes-rs/` | `hide.css` + `plugin.js`: hides features we don't serve |
| `crates/runtime/` | Sessions, turns, delegation/subagents, compression, review, curator, MCP |
| `crates/tools/` | The agent's tools (terminal, files, search, skills, web, vision, checkpoints…) |
| `crates/memory/` | MEMORY.md / USER.md store and the memory tool |
| `crates/store/` | SQLite: sessions, messages, usage, projects |
| `crates/prompts/` + `prompts/` | Prompt text and tool schemas (`prompts/hermes/` is extracted from Hermes) |
| `crates/llm`, `crates/agent` | From Goose: messages, OpenAI-compatible provider, the agent state machine |
| `scripts/perf/` | Your instance (`perf.ps1`), mock providers, live checks (`*-check.mjs`), perf tools |
| `target/hermes-src/` | Hermes exported at `HERMES_COMMIT` (+ patches): read the frontend source here |
| `../hermes-agent` | The Hermes clone (read-only reference) |
| `skills/`, `assets/skills/` | Bundled skills (from Hermes), embedded in the binary |
