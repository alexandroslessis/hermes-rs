# hermes-rs

A Windows desktop re-implementation of the [Hermes agent](https://github.com/NousResearch/hermes-agent)
in Rust: an agent with persistent memory and a self-improvement loop (skills, background
review, curator), Hermes's own desktop frontend, and OpenAI-compatible model providers.

The app is `crates/desktop` (`hermes-desktop.exe`): Hermes's own desktop frontend, built from the
pinned Hermes commit, in a Tauri window over the Rust backend. The earlier GPUI app (`crates/app`)
is frozen. See [GAPS.md](GAPS.md) for what's still missing, [FEATURES.md](FEATURES.md) for scope
and [PORTING.md](PORTING.md) for what comes from where in Hermes and how to update to a newer
Hermes. ([PLAN.md](PLAN.md) is the GPUI-era roadmap, kept as history.)

**Working on the code (people or coding agents): start with [AGENTS.md](AGENTS.md)**, then the
guides in [`docs/`](docs/): architecture, conventions, workflow, testing, pitfalls, the playbook
for each remaining gap, and releasing.

## Build and run

Requirements: Windows 10/11 x64, Visual Studio Build Tools with the C++ workload and the
Windows 11 SDK, and [rustup](https://rustup.rs). `rust-toolchain.toml` selects Rust 1.98.1
automatically (rustup downloads it on first use).

```powershell
powershell -File scripts/build-hermes-webapp.ps1  # Hermes's frontend, once per HERMES_COMMIT (Node 26+ in tools\node)
cargo run -p hermes-desktop                       # the app (debug build; first build takes several minutes)
cargo run -p hermes-desktop --release             # optimized build
cargo test                                        # our crates' tests
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
| `crates/desktop` | The app (`hermes-desktop.exe`): serves Hermes's frontend, speaks its protocol (see its README) |
| `crates/app` | The GPUI app (`hermes-rs.exe`), frozen |
| `crates/runtime` | Sessions and turns: assembles the agent, persists, streams events to the UI |
| `crates/tools` | Agent tools: terminal, background processes, files, patch, search, todo, clarify |
| `crates/prompts` | System-prompt builder and tool schemas (`prompts/tools`) |
| `crates/store` | SQLite sessions, messages, usage |
| `crates/agent` | Agent state machine, skills discovery, compaction (from Goose) |
| `crates/llm` | Messages, OpenAI-compatible provider, model catalog and discovery (from Goose) |
| `vendor/zed` | GPUI, `ui`, `theme` and support crates copied from Zed (`scripts/vendor_zed.py`) |
| `HERMES_COMMIT` | The Hermes commit this tree ports ([PORTING.md](PORTING.md)) |
| `prompts/hermes` | Hermes prompts and tool schemas (`scripts/extract_hermes_prompts.py`) |
| `skills` | Bundled skills from Hermes |
| `assets` | Fonts, icons and themes from Zed |

## License

GPL-3.0-or-later. Contains code from Zed (GPL-3.0 / Apache-2.0), Goose (Apache-2.0) and Hermes
(MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [vendor/SOURCES.md](vendor/SOURCES.md).
