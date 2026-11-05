# Conventions

How code in this repo is written. The goal: a change by any agent reads as if the same person
wrote the whole tree. When this file and the surrounding code disagree, match the surrounding
code and mention it.

## Where new code goes

- **A page feature** (something Hermes's frontend calls): a new module in `crates/desktop/src/`
  named after the feature (`files.rs`, `git_review.rs`, `branch.rs`, `toolsets.rs`), declared in
  `main.rs`. The module holds the logic as plain functions taking what they need (`&Path` data
  dir, `&Value` body, a store handle) and returning `Value` or `(u16, Value)`. `gateway.rs` gets
  only a thin arm: one line in `dispatch`, or one prefix block in `rest`. `gateway.rs` is already
  3,300 lines; don't grow it with logic.
- **Agent behaviour** (what happens in a turn, new tool semantics): `crates/runtime` (turn
  machinery) or `crates/tools` (a tool). Expose a small method on `Runtime` and call it from the
  gateway; don't reach into runtime internals from the desktop crate.
- **Prompt text**: `prompts/hermes/…` (extracted from Hermes) or `crates/prompts/src/*.rs`
  (assembling it). Never paraphrase a Hermes prompt inline in Rust; extract it (see
  [WORKFLOW.md](WORKFLOW.md#prompts)).
- **Per-session or per-feature persistent data**: a JSON side file in the data folder with a tiny
  struct that loads it, mutates under a `Mutex`, and writes it back whole (pattern:
  `branch.rs` `Links`, `hermes_import.rs`). Not a store schema change.
- **Settings**: Hermes's Settings page reads/writes `desktop-config.json` keys named exactly like
  Hermes's `config.yaml` keys (`memory.memory_char_limit`, `approvals.mode`). Read them with
  `self.rest.config_value("a.b")`; if the runtime needs them, apply them in `apply_settings()`.
  Defaults and descriptions go in `rest.rs`'s tables, copied from Hermes's `hermes_cli/config.py`
  `DEFAULT_CONFIG`.

## Module header

Every module starts with a `//!` doc comment that says, in plain sentences:

1. What it is from the user's point of view (which screen / command / tool).
2. How it works in one or two sentences, and any decision that differs from Hermes, with the
   reason.
3. A `Hermes:` line naming the files (and functions) it ports. Example from `branch.rs`:

```rust
//! `/branch`: a new session that starts with this one's conversation (up to a message, or all
//! of it), listed under it in the sidebar. The store's schema (shared with the GPUI app) has no
//! parent column, so the links live in the data folder's `branches.json`.
//!
//! Hermes: `tui_gateway/methods_session_branch.py` (`session.branch`, `session.branch_whole`,
//! `session.branch_stored`); Hermes copies the visible user/assistant text only, this copies the
//! messages as stored (tool calls and results too), so the branch keeps the working context.
```

## Comments

- Comments say **why**, not what: the constraint, the Hermes behaviour being matched, the bug it
  avoids. `// A changed endpoint may have a new window or models.` is the style.
- Full sentences, plain words, no jargon the user wouldn't know. Name the page element the user
  sees (`Settings ▸ Memory`, `Capabilities ▸ Tools`) using `▸`.
- Doc comments (`///`) on every `pub` item and on non-obvious private ones.
- No commented-out code, no TODO without a GAPS.md entry.

## Rust style

- Edition 2024, stable toolchain. `anyhow::Result` + `.context("doing x")` inside modules;
  convert to the protocol's error at the gateway boundary.
- Errors to the page: RPC `Err((SERVER_ERROR, "Readable sentence."))`, REST
  `(4xx/500, json!({"detail": "Readable sentence."}))`. Use `format!("{error:#}")` for anyhow
  chains. Messages are user-facing: say what failed and what to do.
- `serde_json::Value` + `json!` for protocol payloads (shapes are Hermes's, not ours; a typed
  struct per payload adds code without catching anything the live check doesn't). Typed structs
  for our own files and runtime APIs.
- Read params defensively: `params["x"].as_str().unwrap_or_default()`; required ones through a
  helper that returns `INVALID_PARAMS` (`session_id(params)?`).
- No `unwrap()`/`expect()` outside tests except on truly impossible states (and then say why).
  Locks: `.lock().unwrap_or_else(|e| e.into_inner())`.
- Blocking work in async: `tokio::task::spawn_blocking`. Never hold a lock across `.await`.
- Don't shadow a function name with a local variable (`let detail = …` when `detail()` is in
  scope gives the confusing E0618 "expected function").
- Keep functions short enough to read; extract helpers rather than nesting matches four deep.
- Windows first: paths through `Path`/`PathBuf`, never string concatenation with `/`; child
  processes get `CREATE_NO_WINDOW` (see existing `Command` uses) so no console flashes.

## Formatting

- The tree is **not** rustfmt-clean (≈175 diffs in `hermes-desktop` alone). Do **not** run
  `cargo fmt` over the workspace or a whole existing file inside a feature commit: it buries the
  change in noise.
- New files: write them rustfmt-style (`cargo fmt -- --check path/to/new.rs` should be quiet,
  max width 100). Edits to existing files: match the surrounding lines.
- A formatting pass, if ever wanted, is its own commit touching nothing else.
- Clippy: `cargo clippy -p <crate>` on what you changed; fix new warnings in your code. Known
  pre-existing errors in `hermes_tools` (`recall.rs`, `skill_manage.rs` redundant clones) are not
  yours to fix inside a feature commit.
- JS check scripts: ES modules, 2-space indent, no semicolons (match `scripts/perf/*.mjs`).
- Markdown: wrap at ~100 columns, sentence-case headings, tables for maps.

## Dependencies

- Prefer what's already in `Cargo.lock` (tokio, axum, tower-http, serde_json, anyhow, rusqlite,
  parking_lot, portable-pty, keyring, uuid, chrono, regex, walkdir, tempfile/wiremock for tests).
- A new crate needs a reason in the commit body. Never a git dependency, never Hermes/Goose/Zed
  crates: copy the code and cite it.
- Workspace dependencies are declared in the root `Cargo.toml` `[workspace.dependencies]` when
  shared by more than one crate.

## Naming

- Modules and files: the feature, lowercase (`git_review.rs`, `hermes_import.rs`).
- Functions mirror the action the page asks for (`list`, `save`, `validate`, `activate`, `delete`,
  `route` for a sub-router).
- Constants for magic values with a doc comment saying where the number comes from (Hermes's
  default, a measured limit).
- Side files: kebab-case JSON in the data folder (`hermes-imports.json`).
- Check scripts: `scripts/perf/<feature>-check.mjs`.
- Patches: `NNNN-short-description.patch`, next free number.

## Commits

- One feature (or one fix) per commit, building and tested. Subject: `Area: what changed`
  in sentence case, or a short feature name (`Memory manager: the Star map and Settings ▸ Memory`,
  `/branch, /btw and /bg`, `App icon: Hermes's dark icon, sharp in the taskbar`).
- Body: what the user can now do, then how (the decisions, what differs from Hermes and why),
  then a **Checks:** paragraph listing the tests and live checks that passed, then the trailer the
  session gives you (`Co-Authored-By: …`).
- Never commit secrets, the data folder, `target/`, or build outputs. Don't push unless asked.
- Docs updated in the same commit as the feature (see ground rule 10 in AGENTS.md).

## User-facing text

- Labels, errors and descriptions shown in the app match Hermes's tone: short, plain, no
  exclamation marks. Reuse Hermes's own strings where the page shows ours next to theirs.
- When something isn't available, say so and what to do instead
  ("Archiving isn't available in hermes-rs; delete the session instead.").
