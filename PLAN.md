# hermes-rs — Plan

A Windows desktop app that re-implements the Hermes agent in Rust: an agent backend with
persistent memory and a self-improvement loop (skills, background review, curator), a native GPUI
interface built from Zed's UI code, and OpenAI-compatible model providers.

The feature scope is decided in [FEATURES.md](FEATURES.md) (K = v1, L = later, D = dropped).

## Ground rules

- **Windows only, desktop only.** No CLI, TUI, ACP or API server. Dev-only `examples/` binaries
  are allowed for testing; they are not shipped.
- **Self-contained.** Code from Zed, Goose and Hermes is *copied in* and adapted. Nothing depends
  on those repos or on their published crates (`gpui`, `goose-*`). Ordinary crates.io libraries
  (tokio, serde, rusqlite, reqwest, …) are fine.
- **License: GPL-3.0-or-later**, because Zed's UI crates are GPL. Goose (Apache-2.0) and Hermes
  (MIT) code can legally live inside a GPL project; their notices go in `THIRD_PARTY_NOTICES.md`.
- **Pinned sources.** All copies come from these snapshots, recorded in `vendor/SOURCES.md`:

  | Repo | Commit | License |
  |---|---|---|
  | `../hermes-agent` | `c419cf7c` | MIT |
  | `../goose` | `b23d21ca` | Apache-2.0 |
  | `../zed` | `ba63a022` | GPUI: Apache-2.0, UI crates: GPL-3.0 |

- **Toolchain:** Rust 1.98.1 (same as Zed), pinned in `rust-toolchain.toml`, MSVC Build Tools 2026
  + Windows 11 SDK 10.0.26100.

## Rust in Python/C++ terms

| Rust | Closest Python / C++ equivalent |
|---|---|
| crate | a package (Python) / a library or executable target (C++) |
| `Cargo.toml` | `pyproject.toml` / `CMakeLists.txt` |
| `Cargo.lock` | `uv.lock` / a pinned `requirements.txt` |
| workspace | a monorepo of packages sharing one lockfile and one `target/` build dir |
| `cargo build` / `cargo run` / `cargo test` | build + run + pytest in one tool |
| `rust-toolchain.toml` | `.python-version`: picks the compiler version per directory |
| trait | an abstract base class / a C++20 concept |
| `async fn` + tokio | `async def` + asyncio |
| `Result<T, E>`, `?` | exceptions, but returned as values; `?` means "re-raise if error" |
| ownership / borrowing | C++ `unique_ptr` and references, checked by the compiler |

## What we take from each repo

### Zed (UI): three tiers

Measured with `cargo metadata` over Zed's workspace (Windows-only deps):

| Pieces | Zed crates pulled in | Lines | Plan |
|---|---|---|---|
| `gpui` + Windows platform | 19 → ~15 after dropping macOS/Linux/iOS backends | ~160k | **Tier 1: copy verbatim** |
| + `ui`, `theme`, `icons`, `menu`, `component`, `assets` | 31 | ~215k | **Tier 1: copy verbatim** |
| + `markdown` | 59 | ~330k | **Tier 2: copy and cut loose** |
| + `picker`, `ui_input` | 95 | ~600k | **Tier 2: copy and cut loose** |
| + `editor` | 103 | ~800k | **Tier 3: avoid for now** |

- **Tier 1** goes into `vendor/zed/` under the original crate names, with only mechanical edits
  (workspace paths, removing non-Windows targets). That keeps later re-syncs with Zed simple.
- **Tier 2** crates are copied and their heavy dependencies swapped out. For example, `markdown`
  only uses Zed's `language` crate to syntax-highlight code blocks, and `settings` for fonts. We
  replace those with a small tree-sitter highlighter and our own theme settings. `picker` only needs
  `workspace` for modal registration.
- **Tier 3:** Zed's `editor` drags in its project, LSP, git and collaboration layers. The composer
  starts from GPUI's own text-input primitives (`gpui/examples/input.rs`, `EntityInputHandler`),
  styled with Zed's `ui`. If we later want a full code editor (for example, for editing skills), we
  revisit copying `editor`.
- `agent_ui` (Zed's chat panel) is used as **reference for layout and components**: thread view,
  tool-call cards, diff rendering, message editor. Individual components are copied and adapted
  into `crates/app` as we build each screen.

### Goose (agent backend)

Copied into our own crates and adapted:
- `goose-agent` → `crates/agent`: the loop as a state machine of `Operation`s, cancellation,
  subagents, skill discovery.
- `goose-provider-types` → `crates/llm`: `Provider` trait, message/conversation model, retry,
  **only the OpenAI chat-completions format** (`formats/openai.rs`, with its DeepSeek/Kimi/
  OpenRouter quirk handling). Anthropic/Google/Databricks/Responses formats are left out.
- `goose-providers::openai_compatible` (+ `api_client`) → `crates/llm`.
- `goose-context-management` → `crates/agent` (compaction).
- Selected operations from `goose/src/agents/state_machine/ops_*.rs`: tool approval, retry, empty
  response, max turns, steer, compaction, subagent.

### Hermes (behavior)

Copied as text/data, re-implemented in Rust:
- Prompts → `prompts/`: identity, guidance blocks, memory/skill tool descriptions, the three
  background-review prompts, curator prompt, compression summary template. Loaded at compile time
  with `include_str!` (embeds the file into the binary, like a resource).
- Tool schemas → re-declared in Rust with the same descriptions.
- Dangerous-command pattern table → `prompts/approval_patterns.toml`.
- Bundled skills → `skills/` (the Windows-compatible subset of the 44).
- The semantics in Hermes's contract tests (memory, skill_manage, session search, prompt
  stability, role alternation) → our Rust tests.

## Architecture

```
hermes-rs/
├── Cargo.toml              # workspace
├── rust-toolchain.toml     # Rust 1.98.1
├── crates/
│   ├── app/                # the .exe: windows, views (chat, composer, sidebar, settings, memory & skills)
│   ├── agent/              # state machine + Hermes operations (from goose-agent)
│   ├── llm/                # Provider trait, messages, OpenAI-compatible client (from goose)
│   ├── store/              # SQLite: sessions, messages, usage, FTS5 search, pending writes
│   ├── tools/              # terminal, process, files, search, web, todo, clarify, vision, MCP
│   ├── learn/              # MemoryStore, SkillStore (git), review fork, curator
│   └── prompts/            # embeds prompts/ and skills/ into the binary
├── vendor/zed/             # Tier 1 Zed crates, near-verbatim
├── prompts/                # Hermes prompt text (MIT)
├── skills/                 # bundled skills (MIT)
└── assets/                 # fonts, icons, themes (from Zed)
```

The backend crates don't depend on GPUI. They run on a tokio thread pool and send `AgentEvent`s
(text delta, tool call started/finished, approval needed, usage…) over a channel that the UI
subscribes to. That keeps the UI responsive, and lets the backend be tested without a window.

### Hermes concepts → implementation

| Hermes | hermes-rs |
|---|---|
| Byte-stable system prompt | Built once at session creation, stored on the session row, returned verbatim by one `prompt_parts` operation. Only compaction rebuilds it. |
| Mid-session injections | Appended to a user message or tool result, never to the system prompt |
| Iteration budget, retries, empty responses | Adapted Goose operations |
| Nudge counters | Derived from the stored conversation (marker on the last reviewed message) |
| Background review | After a turn ends: detached task runs a second state machine on a session clone, with the same prompt and tools (prompt-cache reuse), memory/skill tools only, 16 steps max. Destructive memory ops are staged. |
| Curator | Periodic task (on app idle, max once a day): consolidate overlapping skills, archive unused ones, write a report |
| Skill rollback | Skills dir is a git repo (`git2` crate); one commit per `skill_manage` batch, revert from the UI |
| Approvals | Adapted Goose `ToolApprovalOperation` + Hermes pattern table + allowlist → approval card in chat |
| Session search | SQLite FTS5 with the built-in `trigram` tokenizer, 4 modes |
| Secrets | API keys in Windows Credential Manager (`keyring` crate), settings in `%APPDATA%\hermes-rs\config.toml` |
| Data | `%APPDATA%\hermes-rs\` (memories, skills, sessions.db, logs); one-way import from `~/.hermes` |

## Milestones

Each milestone ends with something you can run and tests that pass.

**M0: Foundations** ✅ (2026-10-31)
1. Toolchain installed and pinned. ✅
2. Workspace skeleton, license, third-party notices, `vendor/SOURCES.md`. ✅
3. Hermes prompts (70 files incl. all tool schemas) and 37 bundled skills copied in. ✅
4. Tier 1 Zed crates vendored (23 crates); `cargo run -p hermes-rs` opens a themed window using
   Zed `ui` components. ✅
5. Goose agent/LLM crates copied and compiling; Goose's own tests pass (494, 7 ignored on
   Windows), plus an end-to-end streaming + tool-call test against a mock server. The
   `hermes_llm` `chat` example is ready for a real endpoint, but no local model server was
   available to try it. ✅

Known gaps carried forward: Zed's JSON themes aren't loaded yet (built-in One Dark is used), and
Goose's `detect_image_path` doesn't recognize Windows paths.

**M1: Agent backend, headless.** Session store, frozen tiered prompt, core operations, tools
(terminal, process, files, patch, search, todo, clarify), event stream. Tests run against a mock
OpenAI server, plus a compatibility run against LM Studio/Ollama/OpenRouter.

**M2: Chat UI.** Markdown (Tier 2 port), streaming chat view, tool cards, thinking blocks,
composer (multiline, history, slash and skill autocomplete), sessions sidebar, tabs, approval and
clarify cards, todo panel, status bar, onboarding + settings (provider, keys, models). After M2 the
app is usable day to day.

**M3: Memory and recall.** Memory store with Hermes semantics, frozen snapshot, threat scan,
pending-write approvals, session search, memory editor screen.

**M4: Self-improvement.** Skills (`skills_list`/`skill_view`/`skill_manage`), git-backed rollback,
skill slash commands, background review, `/refine`, `/learn`, curator, skills browser screen.
Behavior tuning starts here and continues with real use.

**M5: Breadth.** Compression, delegation, MCP, web search/extract, vision and image paste,
context files + subdirectory hints, `@` references, checkpoints/`/rollback`, `/diff`, steer/queue,
auxiliary model routing, usage and context breakdown.

**M6: Polish.** Command palette (Tier 2 `picker`), find in transcript, themes and fonts, diff
review pane, import from `~/.hermes`, installer (MSI), auto-update.

Rough effort: M0 a few days, M1 1–2 weeks, M2 3–5 weeks (the UI is the largest piece), M3 1–2
weeks, M4 2–3 weeks, M5 2–4 weeks, M6 2–3 weeks.

## Risks

- **GPUI internals.** It's pre-1.0 and lightly documented. Copying it at a fixed commit protects us
  from upstream churn, but we own any bugs. Zed's own app is the reference for how to use it.
- **Compile times.** Rust plus ~215k lines of vendored Zed means slow first builds (expect
  5–15 minutes); incremental rebuilds are much faster. Keeping Tier 3 out matters here.
- **Composer quality.** A good multiline editor is real work without Zed's `editor`. If the GPUI
  input approach falls short, the fallback is a Tier 2 copy of `editor` with project/LSP cut out.
- **Behavior parity.** Memory and skill quality depend on prompt tuning against real use, not on
  code. The Hermes prompts give us a strong starting point.
