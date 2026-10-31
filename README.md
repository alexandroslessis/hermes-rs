# hermes-rs

A Windows desktop re-implementation of the [Hermes agent](https://github.com/NousResearch/hermes-agent)
in Rust: an agent with persistent memory and a self-improvement loop (skills, background
review, curator), a native UI built from [Zed](https://github.com/zed-industries/zed)'s GPUI and
`ui` crates, and OpenAI-compatible model providers.

Status: **M0 (foundations)**. See [PLAN.md](PLAN.md) for the roadmap and [FEATURES.md](FEATURES.md)
for scope.

## Build and run

Requirements: Windows 10/11 x64, Visual Studio Build Tools with the C++ workload and the
Windows 11 SDK, and [rustup](https://rustup.rs). `rust-toolchain.toml` selects Rust 1.98.1
automatically (rustup downloads it on first use).

```powershell
cargo run -p hermes-rs            # the app (debug build; first build takes several minutes)
cargo run -p hermes-rs --release  # optimized build
cargo test                        # our crates' tests
```

Talk to a real model through the provider layer (dev example):

```powershell
$env:HERMES_BASE_URL = "http://localhost:1234/v1"; $env:HERMES_MODEL = "your-model"
cargo run -p hermes_llm --example chat -- "Say hello"
```

## Layout

| Path | What |
|---|---|
| `crates/app` | The desktop app (`hermes-rs.exe`) |
| `crates/agent` | Agent state machine, skills discovery, compaction (from Goose) |
| `crates/llm` | Messages, OpenAI-compatible provider, model catalog (from Goose) |
| `vendor/zed` | GPUI, `ui`, `theme` and support crates copied from Zed (`scripts/vendor_zed.py`) |
| `prompts/hermes` | Hermes prompts and tool schemas (`scripts/extract_hermes_prompts.py`) |
| `skills` | Bundled skills from Hermes |
| `assets` | Fonts, icons and themes from Zed |

## License

GPL-3.0-or-later. Contains code from Zed (GPL-3.0 / Apache-2.0), Goose (Apache-2.0) and Hermes
(MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [vendor/SOURCES.md](vendor/SOURCES.md).
