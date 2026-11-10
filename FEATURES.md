# Hermes feature inventory

Every user-facing Hermes feature, taken from `../hermes-agent` @ `c419cf7c` (the docs under
`website/docs/user-guide/`, `reference/slash-commands.md`, `toolsets.py` and the source). Each row
has a recommendation for hermes-rs as a **Windows-only desktop app** focused on the agent backend,
self-improvement and persistent memory.

**Rec:** **K** = keep in v1 · **L** = later (keep the design open, build after v1) · **D** = drop.
Edit the `Decision` column (or just tell me the IDs to change). Blank means "go with Rec".

## A. Agent core

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| A1 | Agent loop | Tool-calling loop with iteration budget (500, shared with subagents), interrupt, one grace call after budget | K | |
| A2 | Prompt-cache invariant | System prompt byte-stable per session; only compression may rewrite history | K | |
| A3 | Tiered system prompt | stable (identity, guidance) → context (project files) → volatile (skills index, memory, time, env) | K | |
| A4 | Model-specific guidance | Extra prompt blocks for GPT/Gemini/Qwen/DeepSeek etc. (tool-use enforcement, parallel calls, task completion) | K | |
| A5 | Retry / error classification | Backoff, rate-limit handling, empty-response recovery, truncation continuation | K | |
| A6 | Context compression | Auto at 50% of window: prune old tool results, then LLM summary; manual `/compress [here N \| focus]` | K | |
| A7 | Prepared compaction | Precompute the next summary between turns near the threshold | L | |
| A8 | Interrupt & redirect | Stop mid-turn; send a new message that interrupts | K | |
| A9 | `/steer` | Mid-run note delivered after the next tool call, no new turn | K | |
| A10 | `/queue` + busy mode | Queue messages while the agent works; choose queue/steer/interrupt | K | |
| A11 | Reasoning display & effort | Show/hide thinking; per-model reasoning effort | K | |
| A12 | `/btw` side question | One-shot side question about the conversation without interrupting it | L | |
| A13 | `/retry`, `/undo` | Resend last message; remove last exchange | K | |
| A14 | `/branch` (fork) | Copy session into an independent branch | L | |
| A15 | Mixture of Agents (`/moa`) | Run one prompt through several models, then aggregate | D | |
| A16 | `/goal` + `/subgoal` (Ralph loop) | Standing goal; an aux judge model decides after each turn whether to continue | L | |
| A17 | `/plan` | Write a markdown plan to `.hermes/plans/`, no execution | L | |
| A18 | `/review` | Independent reviewer subagent on the work just done | L | |
| A19 | `/init` | Generate/update `AGENTS.md` from a repo scan | L | |
| A20 | Fast mode (`/fast`) | OpenAI priority processing / Anthropic fast mode | D | |
| A21 | Codex app-server runtime | Alternative runtime for OpenAI Codex | D | |

## B. Learning loop: memory & skills (your focus)

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| B1 | Built-in memory | `MEMORY.md` (agent notes, ~2200 chars) + `USER.md` (user profile, ~1375 chars), `§`-delimited entries | K | |
| B2 | `memory` tool | Atomic `operations[]` add/patch/remove; budget checked on final result; full list on overflow | K | |
| B3 | Frozen memory snapshot | Memory injected at session start; mid-session writes hit disk only | K | |
| B4 | Memory security scan | Injection/exfiltration patterns rejected (memory re-enters every prompt) | K | |
| B5 | Duplicate prevention | Near-duplicate entries refused | K | |
| B6 | Memory write approval | Optional gate: stage writes for user approval (`/memory pending/approve/reject`) | K | |
| B7 | Skills (SKILL.md) | agentskills.io format, frontmatter, `references/ templates/ scripts/ assets/` | K | |
| B8 | Progressive disclosure | Index (name + ≤60-char description) in prompt; full skill via `skill_view` | K | |
| B9 | `skill_manage` tool | Atomic create/patch/write_file/remove_file/delete with rollback, read-before-write guard, description lint | K | |
| B10 | Skill slash commands | `/<skill-name>` loads a skill as a user message | K | |
| B11 | Skill bundles | `/<bundle>` preloads several skills | L | |
| B12 | Background review (self-improvement) | After a turn, a cache-parity fork reviews the conversation with tuned prompts and updates memory/skills | K | |
| B13 | Review controls | Interval, cheaper review model, token cap, disable, notifications of what was learned | K | |
| B14 | `/refine` | Run the review now instead of waiting for the trigger | K | |
| B15 | Skill write approval | Optional gate for agent skill writes | K | |
| B16 | Curator | Periodic background skill consolidation: merge overlaps, archive stale, pin, usage telemetry, reports | L | K |
| B17 | Skill rollback | Hermes: curator backups + audit ledger. hermes-rs: git-backed skills dir, one commit per batch | K | |
| B18 | `/learn` | Distill a skill from a directory, URL or the workflow just done | K | |
| B19 | Learning journey (`/journey`) | Timeline/graph of learned skills and memories | L | |
| B20 | Bundled skills | 44 built-in skills (copied over, filtered to ones that make sense on Windows) | K | |
| B21 | Optional skills catalog | 149 optional skills installable on demand | L | |
| B22 | Skills Hub | Install/search/publish skills from online registries (GitHub, ClawHub, skills.sh…) with security scan | D | |
| B23 | Project-local skills | Skills inside a repo, with a trust prompt | L | |
| B24 | External skill directories | Extra skill roots from config | K | |
| B25 | Session search | `session_search` tool: FTS5 discover/scroll/read/browse over past sessions | K | |
| B26 | External memory providers | Honcho, Mem0, Supermemory, Hindsight, etc. plugins | D | |

## C. Tools

| ID | Tool(s) | What it is | Rec | Decision |
|---|---|---|---|---|
| C1 | `terminal` | Run shell commands (on Windows: PowerShell and/or Git Bash) | K | |
| C2 | `process_manage` | Background processes: start, poll, kill, notify on completion | K | |
| C3 | `read_file`, `write_file`, `patch`, `search_files` | File I/O, fuzzy-match patching, ripgrep-style search | K | |
| C4 | Write guards + lint | Refuse writes to sensitive paths; post-write lint hints | K | |
| C5 | `todo_list` | Task list the agent maintains (shown above the composer) | K | |
| C6 | `clarify` | Ask the user a question with choices | K | |
| C7 | `web_search` | Web search (needs a backend: SearXNG/Brave/Tavily/…) | K | |
| C8 | `web_extract` | Fetch a URL → clean markdown | K | |
| C9 | `delegate_task` | Subagents: single/batch, 3 concurrent, depth 2, background mode, roles | K | |
| C10 | MCP client | stdio + HTTP MCP servers, per-server tool filtering | K | |
| C11 | `vision_analyze` + image paste | Analyze images with a vision-capable model | K | |
| C12 | `execute_code` | Agent writes Python that calls tools over RPC | L | |
| C13 | `browser_exec`/`browser_cdp`/`browser_dialog` | Browser automation over CDP | L | |
| C14 | `browser_vault` | Credential vault for browser logins | D | |
| C15 | `computer_use` | Desktop control (mouse/keyboard/screenshots) | D | |
| C16 | `image_generate`, video gen | FAL.ai etc. | D | |
| C17 | `text_to_speech` | TTS providers | D | |
| C18 | `x_search` | Search X/Twitter via xAI | D | |
| C19 | Tool search (deferred tools) | Hide rarely-used tool schemas behind a search tool | L | |
| C20 | `cronjob_manage` | Agent-managed cron jobs | D | |
| C21 | `kanban_*` (13 tools) | Multi-agent kanban board | D | |
| C22 | `manage_connections` | Connector OAuth accounts (Google, GitHub apps…) | D | |
| C23 | LSP integration | Language-server diagnostics fed to the agent | L | |
| C24 | Document extraction | PDF/DOCX → text | L | |

## D. Sessions & context

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| D1 | SQLite session store | Sessions, messages, usage, titles; WAL; FTS5 | K | |
| D2 | Auto titles | Aux-model generated session titles | K | |
| D3 | Resume / list / rename / delete sessions | | K | |
| D4 | Context files | `AGENTS.md`, `.hermes.md`, `CLAUDE.md`, `.cursorrules` from git root → cwd, size-capped, injection-scanned | K | |
| D5 | Subdirectory hints | `AGENTS.md` found in subdirs appended to tool results as the agent explores | K | |
| D6 | SOUL.md | Agent identity/personality file, first in the system prompt | K | |
| D7 | `/personality` presets | Per-session personality overlays | L | |
| D8 | `@` context references | `@file`, `@folder`, `@diff`, `@url` expanded inline | K | |
| D9 | Checkpoints & `/rollback` | Shadow-git snapshots of the working dir before file edits; diff/restore/single-file restore | K | |
| D10 | `/diff` | Show git changes in the workspace | K | |
| D11 | Git worktrees | Run sessions in isolated worktrees | L | |
| D12 | `/usage`, `/insights`, context breakdown | Token/cost per session, context-window breakdown by category | K | |
| D13 | Session auto-prune | Delete old ended sessions | L | |
| D14 | Import from other agents | Import memory/skills from Claude Code, Codex, etc. | L | |
| D15 | Import from Hermes | One-way import of `~/.hermes` memory, skills, SOUL.md | K | |

## E. Models & providers

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| E1 | OpenAI-compatible provider | Any `/v1/chat/completions` endpoint (OpenAI, OpenRouter, Ollama, LM Studio, vLLM, DeepSeek…) with quirk handling | K | |
| E2 | Model picker + favorites | Switch model per chat; per-model effort presets | K | |
| E3 | Model metadata | Context length, capabilities (from endpoint or models.dev) | K | |
| E4 | Auxiliary model routing | Separate (cheaper) model for titles, compression, review, vision | K | |
| E5 | Fallback providers | Auto-failover to a backup model | L | |
| E6 | Credential pools | Rotate several keys for one provider | D | |
| E7 | Provider routing (OpenRouter sort/whitelist) | | L | |
| E8 | Native Anthropic / Gemini / Bedrock / Codex Responses adapters | Non-OpenAI wire formats | D | |
| E9 | Nous Portal login, billing, `/topup`, subscription | | D | |
| E10 | Local model management | Ollama/llama.cpp helpers, wait-for-idle-GPU for reviews | L | |

## F. Safety

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| F1 | Dangerous-command approvals | ~56-pattern table; modes manual/smart/off; per-session YOLO | K | |
| F2 | Learned allowlist | "Always allow" for a command pattern | K | |
| F3 | Smart approvals | Aux model judges whether a flagged command is actually risky | L | |
| F4 | Secret redaction | Redact keys/tokens from tool output and logs | K | |
| F5 | Threat scanning of context/memory/skills | Prompt-injection patterns | K | |
| F6 | Docker/SSH/Modal/Daytona/Vercel terminal backends | Remote/sandboxed execution | D | |
| F7 | Docker egress proxy | | D | |
| F8 | Managed scope / secret scopes / profiles isolation | Multi-profile security plumbing | D | |

## G. Automation

| ID | Feature | Rec | Decision |
|---|---|---|---|
| G1 | Cron scheduler (`/cron`, delivery to platforms) | D | |
| G2 | `/heartbeat`, `/loop` (recurring prompts in a session) | D | |
| G3 | `/bg` background session | L | |
| G4 | Suggested automations, blueprints | D | |
| G5 | Kanban board (multi-profile/multi-project) | D | |
| G6 | Batch processing / trajectory generation (RL) | D | |
| G7 | Event hooks (shell/plugin hooks at lifecycle points) | L | |

## H. Integrations & extensibility

| ID | Feature | Rec | Decision |
|---|---|---|---|
| H1 | Messaging gateway (Telegram, Discord, Slack, WhatsApp, Signal, ~20 platforms), Bot Mode, `/handoff` | D | |
| H2 | ACP server (use Hermes inside Zed/VS Code) | D | **Changed to L, 2026-11-10, built for Zed the same day:** `hermes-desktop.exe --acp`, see [docs/acp/](docs/acp/README.md) |
| H3 | OpenAI-compatible API server | D | |
| H4 | Web dashboard / remote backend / multi-connection desktop | D | |
| H5 | Plugin system (Python plugins: tools, hooks, context engines) | D | |
| H6 | Plugin catalog | D | |
| H7 | Spotify, Feishu, Yuanbao and other connectors | D | |

## I. Media & voice

| ID | Feature | Rec | Decision |
|---|---|---|---|
| I1 | Image attach / paste / drag-drop | K | |
| I2 | Voice dictation (speech-to-text) | L | |
| I3 | Read replies aloud (TTS) | D | |
| I4 | Wake word ("Hey Hermes") | D | |
| I5 | Live voice conversations | D | |

## J. Desktop app (UI)

| ID | Feature | What it is | Rec | Decision |
|---|---|---|---|---|
| J1 | Chat view | Streaming markdown, code blocks with copy, live tool-call cards, thinking blocks | K | |
| J2 | Composer | Multiline, history (↑/↓), queue editing, slash-command + skill autocomplete, `@` references, drag-drop files | K | |
| J3 | Session sidebar | List/search/rename/pin/delete, group by project | K | |
| J4 | Tabs | Multiple sessions open, Ctrl+T/Ctrl+Tab/Ctrl+W | K | |
| J5 | Multiple windows / pop-out session | | L | |
| J6 | Status bar | Model, context % meter + breakdown popover, YOLO toggle, cache hit rate, tok/s | K | |
| J7 | Approval & clarify dialogs | Inline approve/deny/always; clarify choices | K | |
| J8 | Todo/task panel above composer | | K | |
| J9 | Diff view for file edits | Inline diffs in tool cards + review pane | K | |
| J10 | File browser | Browse/preview working directory | L | |
| J11 | Embedded terminal pane | Zed's `terminal`/`terminal_view` crates | L | |
| J12 | Memory & skills manager | View/edit MEMORY.md/USER.md, browse/edit skills, pending approvals, git history of skills | K | |
| J13 | Settings UI | Providers/keys, models, aux models, review settings, approvals, appearance | K | |
| J14 | Command palette (Ctrl+P / Ctrl+Shift+P) | Zed's `command_palette`/`picker` | K | |
| J15 | Themes + fonts | Zed's theme system (light/dark, its bundled themes) | K | |
| J16 | Find in transcript (Ctrl+F) | | K | |
| J17 | Conversation timeline rail, reading-position memory | | L | |
| J18 | Artifacts gallery | Index of files/images produced by sessions | L | |
| J19 | Preview rail / in-app browser / annotate | | D | |
| J20 | Live subagents view | | L | |
| J21 | Git review & worktrees pane | | L | |
| J22 | Memory graph | | L | |
| J23 | Quick entry (global hotkey), HUD mode | | D | |
| J24 | Simple/Advanced interface mode, layout editor | | D | |
| J25 | Onboarding wizard | First-run provider/model setup | K | |
| J26 | Auto-update | | L | |

## K. Platform & ops

| ID | Feature | Rec | Decision |
|---|---|---|---|
| K1 | Profiles (multiple isolated homes) | D | |
| K2 | `config.yaml` + `.env` secrets | K (config file + Windows Credential Manager for keys) | |
| K3 | Package manager (`pm`), `hermes update`, installer scripts | D (replaced by an MSI/installer later) | |
| K4 | Skins, pets, battery, kaomoji indicators | D | |
| K5 | i18n / language packs | D | |
| K6 | Telemetry, debug upload, trace upload | D | |
| K7 | Logs (`hermes logs`) | K (local log file + "open logs" in settings) | |
