# What hermes-desktop is still missing

A review of Hermes (`c419cf7c`: the desktop frontend, its gateway contract, the Python backend's
tools and commands) against hermes-rs (`crates/desktop`, the runtime and tool crates), 2026-11-04.

**Use case it's ranked for:** a local harness on this machine, talking to an OpenAI-compatible
endpoint, replacing Goose / opencode / Claude Code. Nothing hosted or always-on.

**How it was checked:** the 257 JSON-RPC methods in `apps/shared/src/gateway-contract.openrpc.json`
against what `gateway.rs` answers (46 today); the 155 `/api/...` paths in the frontend source
against `gateway.rs` / `rest.rs` / `skills.rs` / `server.rs`; Hermes's core tools (`toolsets.py`)
and slash commands (`desktop-slash-registry.json`, `desktop-slash-commands.ts`) against the runtime;
what `plugins/hermes-rs` and `hide.css` hide; FEATURES.md's keep/later/drop marks.

Effort: **S** = an afternoon, mostly wiring what the runtime already has · **M** = a few days, new
backend code · **L** = a port of a Hermes subsystem.

---

## 1. Blocking daily use

| # | Gap | Evidence | Already in hermes-rs | Effort |
|---|---|---|---|---|
| 1.1 | ✅ **Done 2026-11-04** (Settings ▸ Providers ▸ Custom endpoints: add, test, edit, make default, delete; keys in Credential Manager). **No way to set up a provider in the app.** Adding an OpenAI-compatible endpoint, its API key, or choosing which models show needs the GPUI app's `config.json` / Credential Manager. | Settings ▸ Providers and Keys hidden; `/api/providers/custom-endpoints*`, `/api/providers/validate`, `/api/env*` unanswered | `providers.rs` (settings, keyring, model lists), the model picker | M |
| 1.2 | ✅ **Done 2026-11-04** (images: upload, paste, drop, sent natively when the model lists image input or Settings ▸ Chat ▸ Attachments says native, else named by path for `vision_analyze`; files: uploaded, copied to `.hermes/attachments`, read through `@file:`; PDFs attach as files, not rendered pages). **Attachments don't work**: images, files, PDFs, paste, drag-and-drop. | `image.attach`, `image.attach_bytes`, `file.attach`, `pdf.attach`, `clipboard.paste`, `/api/chat/image-upload`, `/api/chat/file-upload` unanswered; the gateway has no attachment handling | `Runtime::start_turn_with_images`, `vision_analyze` | M |
| 1.3 | ✅ **Done 2026-11-05** (read-text, write-text, mkdir, git-root, ticketed download and ranged media stream; credential files refused). **File preview and editing in the side panel.** Opening a file the agent touched, viewing an image, saving an edit. | `/api/fs/read-text` (13 call sites), `/api/fs/write-text`, `/api/fs/read-data-url`, `/api/files/ticket` + `/stream`, `/api/files/mkdir`, `/api/fs/download` | `/api/fs/list`, `paths.rs` | S–M |
| 1.4 | ✅ **Done 2026-11-05** (all of `web_git.py`: review list and diffs per scope, stage / unstage / revert, commit and push, commit-message context, branches, worktrees, the `gh` ship flow). **Git review panel**: the diff of the session's changes, staging. | `/api/git/review/diff`, `/api/git/review/stage`, `/api/git/file-diff`, `/api/git/branches` | `git.rs` (status, rev-parse), `runtime::commands::diff` | M |
| 1.5 | ✅ **Done 2026-11-05** (PowerShell in a pseudo-console through `portable-pty`; reattach with replay, resize, close, 15-minute retention, 16 shells). **Interactive terminal** (your own shell in the panel; only the agent's process output works). | `/api/host-terminal` (a PTY over a socket) | `process.rs` (agent processes) | M (needs a PTY crate) |
| 1.6 | ✅ **Done 2026-11-04.** **Typed `/undo` and `/retry` fail.** The restore button works; the typed commands call another method. | `session.rewind` unanswered (the button uses `session.mutate rewind`) | rewind in `gateway.rs` | S |
| 1.7 | ✅ **Done 2026-11-04** (on by default; Settings ▸ Safety ▸ Checkpoints). **Checkpoints are never taken**, so nothing can be rolled back; `/rollback` and its UI don't work. | Desktop never calls `checkpoints().set_enabled`; Settings ▸ Safety ▸ Checkpoints hidden; `rollback.list/diff/restore` unanswered | `Checkpoints`, `Runtime::rollback`, `runtime::commands::rollback` | S |
| 1.8 | **MCP servers can't be added or used.** | Connectors tab hidden (by us); `mcp.servers.*`, `/api/mcp/servers*`, `reload.mcp` unanswered; the desktop never configures the runtime's MCP manager | `runtime::mcp::McpManager` (stdio + HTTP, configure, reconnect, status, tools) | M |

## 2. Commands and agent features that are half there

| # | Gap | Evidence | Already in hermes-rs | Effort |
|---|---|---|---|---|
| 2.1 | ✅ **Done 2026-11-04** (`/history` is the page's own). Built-in commands answered "not a command": `/compress`, `/status`, `/diff`, `/usage`, `/insights`, `/context`, `/refine`, `/memory` (pending writes), `/tools`, `/history`. | `session.compress`, `session.status`, `command.dispatch` only knows skills | `Runtime::compress`, `diff`, `usage_report`, `insights_report`, `refine`, `context_breakdown`, memory approval store | S |
| 2.2 | ✅ **Done 2026-11-05** (`subagent.start/tool/thinking/progress/complete` events; `subagent.list`, `tail`, `steer`, `interrupt`; `delegation.status`, `delegation.pause`. Not ported: `spawn_tree.*` and `agents.list`, which the page doesn't call). **Live subagents**: delegation runs, but the page can't list, follow, steer or stop subagents. | `subagent.list` returns empty; `subagent.tail/steer/interrupt`, `agents.list`, `spawn_tree.*` unanswered | `runtime/src/delegate.rs` | M |
| 2.3 | ✅ **Done 2026-11-05** (`/btw`: one request over a transcript snapshot, no tools, answer as `btw.complete`; `/bg`: a background subagent you can follow, steer and stop, result as `background.complete`). `/btw` (side question) and `/bg` (background prompt). | `prompt.btw`, `prompt.background` | the turn machinery | M |
| 2.4 | ✅ **Done 2026-11-05** (`session.branch`, `branch_whole`, `branch_stored`, `session.create` with messages; parent links in `branches.json`, listed under the parent). `/branch` / fork a session from a message. | `session.branch*` | store, rewind | M |
| 2.5 | ✅ **Done 2026-11-04** (Save As dialog, JSON/Markdown/HTML, redact). Save / export a conversation (`/save`). | `session.save`, `session.export` | store | S |
| 2.6 | ✅ **Done 2026-11-05** (`/plan`, `/init`, `/learn` send their prompt as a turn; `/review` starts a reviewer subagent on the recent conversation, then the model presents its findings). Coding-workflow commands from Hermes's registry: `/plan` (plan to `.hermes/plans/`), `/init` (write AGENTS.md from a repo scan), `/review` (subagent reviews the work), `/learn` (skill from anything). | `command.dispatch` | prompts in `prompts/hermes`, delegation, skill_manage | S each (mostly prompts) |
| 2.7 | `/goal` + `/subgoal` (keep working until a goal is met) and `/loop` (re-run a prompt on an interval). | contract `goal`/`loop` commands | turn machinery | M |
| 2.8 | ✅ **Done 2026-11-05** (Capabilities ▸ Tools: ten toolsets, each switchable from the next turn, subagents included; kept in `config.json`; Claude Code-style per-command permissions not done). Tool switches (turn tools off, Claude Code-style permissions). | Tools tab hidden (by us); `tools.list/configure`, `toolsets.list`, `/api/tools/toolsets*` | fixed tool set in `SessionTools` | M |
| 2.9 | Auxiliary model routing in the UI (compression, review, curator, approval models; only titles are read). | Settings ▸ Model ▸ Auxiliary hidden; `providers::AuxModels` has `titles` only | aux routing in the runtime config (GPUI app sets it) | S–M |
| 2.10 | ✅ **Done 2026-11-04** (Command Center ▸ Maintenance; the curator also runs on its schedule again, which the desktop never did). Curator and background-review controls. | `/api/curator`, `/api/curator/paused`, `/api/curator/run` | `runtime::curator`, `set_background_review` | S |
| 2.11 | ✅ **Done 2026-11-04** for insights (`usage.bars` turned out to be Nous subscription limits: not needed). Usage bars in the status bar, insights page. | `usage.bars`, `insights.get`, `/api/analytics/usage` | `usage_report`, `insights_report`, store usage table | S |

## 3. Sessions, memory, settings

| # | Gap | Evidence | Already in hermes-rs | Effort |
|---|---|---|---|---|
| 3.1 | ✅ **Done 2026-11-05** (the Star map: learned skills and every memory entry as nodes, opened, edited, deleted; Settings ▸ Memory: on/off, sizes, write approval, applied to the store; earlier: file sizes and reset in Command Center ▸ Maintenance). Pending writes stay in the chat's approval cards; no external memory providers. Memory manager: view and edit MEMORY.md / USER.md, pending writes, reset. | Settings ▸ Memory hidden; `/api/memory`, `/api/memory/reset` | `hermes_memory` store, approvals | S–M |
| 3.2 | ✅ **Done 2026-11-04.** Session search in the sidebar. | `/api/sessions/search` | `session_search` tool, store search | S |
| 3.3 | Archive / hide sessions. | `session.archive`, `session.set_hidden` | store (needs a flag kept outside the shared schema, or a schema bump the GPUI app tolerates) | S |
| 3.4 | Sessions settings: retention / auto-prune, export, import. | Settings ▸ Sessions hidden | store delete | S–M |
| 3.5 | Logs done 2026-11-04 (the app now also logs to `logs\\desktop.log`; Command Center shows it); About still hidden. Logs viewer, version and about. | Settings ▸ Advanced / About hidden; `/api/logs` | tracing output | S |
| 3.6 | Workspace settings (default folder). | Settings ▸ Workspace hidden | `/api/fs/default-cwd` | S |
| 3.7 | Artifacts gallery (links and files the agent produced, across sessions). | sidebar entry hidden (by us) | mostly frontend over session messages | S |
| 3.8 | Model fallbacks (another model when one fails). | Settings ▸ Model ▸ Fallbacks hidden | retry/classification in `llm` | M |

## 4. Known small issues

- Steering a queued message shows it only after the reply (`crates/desktop/patches/README.md`).
- Tokens/s isn't shown for sessions from before the stats file existed.
- `/timeline` (conversation timeline rail) is unanswered.
- The maximize button's glyph doesn't switch to "restore".
- Projects: `projects.archive`, `remove_folder`, `set_primary`, `for_cwd`, `get` unanswered.
- `/api/profiles/sessions/sidebar`, `/api/sessions/:id/messages` (read-only transcript) unanswered.
- The context gauge's window falls back to 256k when a provider doesn't list the model's context.

## 5. Later, if wanted

| Gap | Note | Effort |
|---|---|---|
| Skill hub install (Discover ▸ Install) | `tools/skills_hub*.py` + `skills_guard.py`, ~4,800 lines; start with official + GitHub sources and the scanner | L |
| Browser automation tools (`browser_exec` over CDP) | for web tasks; WebView2/Chrome via CDP | L |
| `execute_code` (Python sandbox tool) | the terminal covers most of it | M |
| Tool search (deferred tools) | matters with many MCP tools | M |
| Git worktrees (per-session branches) | `/api/git/worktrees`, `worktree/add` | M |
| Scheduled jobs while the app runs (`/cron`) | local-only version of Hermes's cron | M |
| Voice: dictation, read aloud | STT/TTS providers | L |
| Local model management (download/run GGUFs) | Ollama already covers this | L |
| LSP integration | diagnostics for the agent | L |
| Multiple windows / pop-out session | | M |

## 6. Not needed for a local harness

Hosted / server / account features, deliberately out: the messaging gateway and Bot Mode
(`groups.*`, `bot_relay.*`, `handoff.*`, `pairing`, `webhooks`), wake word and always-on (`wake.*`),
remote display (`display.*`), Nous Portal billing, subscription and free tier, credential vault,
telemetry (`shared_metrics.*`), web access / remote connections (`webapp-access`), kanban, pets,
Python plugins and the plugin catalog, SaaS connector accounts (`connectors.*` OAuth), Codex runtime,
image generation, computer use, cloud profiles.

## Suggested order

1. **Quick wins (S):** 1.6 typed undo/retry, 1.7 checkpoints + rollback, 2.1 built-in commands,
   2.5 save/export, 2.10 curator, 2.11 usage, 3.2 search, 3.5 logs/about.
2. **Make it self-sufficient:** 1.1 provider and key setup, then 1.2 attachments.
3. **Coding workflow:** 1.3 file preview/edit, 1.4 git review, 1.5 terminal, 2.6 `/plan` `/init` `/review`.
4. **Extensibility:** 1.8 MCP, 2.8 tool switches, 2.2 live subagents.
5. Then sections 3 and 4, and 5 as wanted.

- Added 2026-11-05: 1.3 file preview/edit is now done (read-text, write-text, mkdir, git-root, ticketed download and ranged media stream; credential files refused).


