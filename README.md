# hermes-rs

A Windows desktop re-implementation of the [Hermes agent](https://github.com/NousResearch/hermes-agent)
in Rust: an agent with persistent memory and a self-improvement loop (skills, background
review, curator), a native UI built from [Zed](https://github.com/zed-industries/zed)'s GPUI and
`ui` crates, and OpenAI-compatible model providers.

Status: **M1 (headless agent backend) done; M2 (chat UI) next**. See [PLAN.md](PLAN.md) for the
roadmap and [FEATURES.md](FEATURES.md) for scope.

## Build and run

Requirements: Windows 10/11 x64, Visual Studio Build Tools with the C++ workload and the
Windows 11 SDK, and [rustup](https://rustup.rs). `rust-toolchain.toml` selects Rust 1.98.1
automatically (rustup downloads it on first use).

```powershell
cargo run -p hermes-rs            # the app (debug build; first build takes several minutes)
cargo run -p hermes-rs --release  # optimized build
cargo test                        # our crates' tests
```

Run the agent headless against a real endpoint (dev example; OpenRouter by default, works in
the current folder, sessions stored in `%APPDATA%\hermes-rs-dev`):

```powershell
$env:OPENROUTER_API_KEY = "sk-or-..."
cargo run -p hermes_runtime --example agent -- --models
$env:HERMES_MODEL = "anthropic/claude-sonnet-4.5"
cargo run -p hermes_runtime --example agent -- "List the files here and summarize the README"
```

For another OpenAI-compatible server set `HERMES_BASE_URL` (e.g. `http://localhost:1234/v1`) and
`HERMES_API_KEY`. Ctrl+C stops the turn.

## Layout

| Path | What |
|---|---|
| `crates/app` | The desktop app (`hermes-rs.exe`) |
| `crates/runtime` | Sessions and turns: assembles the agent, persists, streams events to the UI |
| `crates/tools` | Agent tools: terminal, background processes, files, patch, search, todo, clarify |
| `crates/prompts` | System-prompt builder and tool schemas (`prompts/tools`) |
| `crates/store` | SQLite sessions, messages, usage |
| `crates/agent` | Agent state machine, skills discovery, compaction (from Goose) |
| `crates/llm` | Messages, OpenAI-compatible provider, model catalog and discovery (from Goose) |
| `vendor/zed` | GPUI, `ui`, `theme` and support crates copied from Zed (`scripts/vendor_zed.py`) |
| `prompts/hermes` | Hermes prompts and tool schemas (`scripts/extract_hermes_prompts.py`) |
| `skills` | Bundled skills from Hermes |
| `assets` | Fonts, icons and themes from Zed |

## License

GPL-3.0-or-later. Contains code from Zed (GPL-3.0 / Apache-2.0), Goose (Apache-2.0) and Hermes
(MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [vendor/SOURCES.md](vendor/SOURCES.md).
