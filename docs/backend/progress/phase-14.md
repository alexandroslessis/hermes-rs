# Phase 14 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-p14   Branch: backend/p14-trim

## CP14.1 — delete unused code
Worktree: C:\Users\alexandros\repos\hermes-rs-p14   Branch: backend/p14-trim   Model: Sonnet 5.5 (subagent)
Cut from `main` at ceddf45 (Phase 13 merged). Not committed.
Done:
- **Before picture** (clean tree at ceddf45, `CARGO_TARGET_DIR=target/claude`): `hermes-desktop.exe` debug 64,207,360 B, release
  43,018,752 B; `crates/llm` 25,014 lines, `crates/agent` 4,336 lines. `cargo bloat` and `cargo udeps` are not installed (not
  installed by me). NOTE: the worktree had no `target/hermes-src` (Hermes's webapp), so both exes were built with a one-file
  stub `dist-webapp/index.html` (gitignored, under `target/`); absolute sizes are lower than a real build, the before/after
  difference is what counts.
- **wasm**: every `cfg(target_arch = "wasm32")` branch removed (`llm`: `base.rs`, `errors.rs`, `retry.rs` (the `gloo_timers`
  sleep); `agent`: `inference.rs`, `operation.rs`, `subagent.rs`, `tool.rs`, `skills`); `maybe_send.rs` deleted, `MaybeSend` /
  `MaybeSync` replaced by `Send` / `Sync` (same bounds natively; the blanket impls were `Send`/`Sync`); `#[async_trait]` plain.
- **`goose_mode.rs`** deleted (users: `Provider::update_mode`, deleted with it; no other user in desktop, ACP, app, tests).
- **`permission.rs`** deleted: `PermissionConfirmation`, `PrincipalType` had no user outside `Provider::handle_permission_confirmation`
  (deleted). `Permission` stays, moved into `conversation/message.rs`: it is a field of `ActionRequiredData::ToolConfirmationResponse`
  (same serde shape, so any stored message still reads).
- **Unused `Provider` trait methods** removed: `configure_oauth`, `update_mode`, `thinking_effort_support`,
  `subscribe_thinking_effort_support`, `set_thinking_effort`, `apply_model_selection`, `permission_routing` (+ `PermissionRouting`),
  `handle_permission_confirmation`.
- **`canonical/catalog.rs`** (Goose's provider setup catalog) deleted with `canonical/data/provider_metadata.json` (56 KB);
  `ProviderMetadata`, `ProviderDeprecation`, `ConfigKey` (base.rs) deleted; none had a user outside llm.
- **Other zero-user pub items** (found by a script that counts each `pub` name across all crates: 76 with no use anywhere, even in
  tests, then a second and third round after the cascade): ~60 builder/`with_*` methods on `ModelConfig`, `ApiClient`, `Message`,
  `TokenUsage`..., `retry_operation`, `capped_retry_wait` (+ its test), `TransportPolicy::SameOrigin`, `ThinkingEffortSupport`,
  `ThinkingEffortCapability`/`Option`, `ProviderDescriptor`, `TokenState`, `ToolCallResult`, `OpenAiCompatibleProvider::build_request_for_model`.
- **`crates/agent`**: the `skills`, `compaction`, `subagent` modules deleted (no user in any crate; the runtime has its own
  compression and delegation), and `ToolOperation`'s typed-tool registration (`with_sync_tool`/`with_async_tool`, `RegisteredTool*`,
  rmcp `ToolBase` helpers); only `with_provider` remains (the runtime uses only that). Dependencies dropped: `agent`: `include_dir`,
  `minijinja`, `regex`, `serde`, `serde_yaml`; `llm`: `strum`, `urlencoding`, dev `rcgen`, `tokio-rustls` (none referenced).
- Docs: `PORTING.md` (Trimmed note on the Goose row), `README.md`, `THIRD_PARTY_NOTICES.md` wording.
Lines: `crates/llm` 25,014 -> 23,431 (-1,583); `crates/agent` 4,336 -> 1,957 (-2,379); `git diff --shortstat HEAD`: 48 files,
189 insertions, 6,718 deletions (includes the removed JSON and `Cargo.lock`, -185 lines).
Sizes after: debug 64,111,616 B (-95,744), release 42,993,152 B (-25,600). Almost no change: the linker already dropped the unused
code; the weight is the 5.8 MB embedded `canonical_models.json` (CP14.2's target).
Files: see `git status` (crates/llm/src, crates/agent/src and Cargo.toml, Cargo.lock, PORTING.md, README.md, THIRD_PARTY_NOTICES.md).
Tests (`CARGO_TARGET_DIR=target/claude`): `cargo test -p` each of `hermes_llm` (485 passed, 7 ignored lib; was 489: 4 tests of deleted
code went; +1 +4 integration), `hermes_agent` (7; was 45: the compaction/skills/subagent tests went with their modules),
`hermes_runtime` (lib 265, `--test turns` 258 passed / 7 ignored, same as before), `hermes_tools` 337, `hermes_memory` 101,
`hermes_store` 34, `hermes_prompts` 37, `hermes-desktop` (191 unit + 68 acp). All green. `cargo build --workspace` OK (also builds the
frozen GPUI app). `cargo check --workspace` has no warnings in our crates. No live check run (nothing the page calls changed).
`scripts/clean-test-temp.ps1 -Delete` run after the tests (284 folders removed).
Request counts: unchanged (`turns` same pass count; no request-building code was touched apart from an unused method).
Deviations from the plan:
- **Responses-API paths in `formats/openai.rs`: not deleted.** There is no Responses request path in our copy: only
  `is_openai_responses_model` (a model-name matcher for o-series / gpt-5 / gpt-6) and `extract_reasoning_effort`. Both are used by
  `model.rs` (reasoning effort, max tokens) and `cache_semantics.rs` and change the request body, so removing them would break the
  byte-identical rule. Only the name is misleading; renaming left for CP14.2 or later.
- `permission.rs` is deleted but `Permission` was moved, not dropped (stored-message shape).
- `mcp_utils.rs` kept: used by `message.rs`, `conversation.rs`, `pre13a_repair.rs`, `formats/openai.rs`.
- `cargo bloat` / `cargo udeps` skipped (not installed); the unused-item search was a script over all crates plus the compiler.
For the reviewer:
- Items found by the script have zero references anywhere (name-based, so a coincidentally shared name would only have kept an
  item, never deleted a used one); but dynamic use through serde (a type named only in stored JSON) is not seen by name matching:
  check `Permission` (kept), and that no deleted type was a `#[serde(...)]` field of a stored/protocol shape (the compiler would
  have failed, but a default-ignored field would not). `git diff` of `message.rs` / `token_usage.rs` / `model.rs` is worth a read.
- `cargo fmt -p hermes_llm -p hermes_agent` was run once and touched `observer.rs` and `tests/request_observer.rs`; those two were
  restored from HEAD. `inference.rs` has some rustfmt re-wrapping next to the real changes (shorter lines after the `MaybeSend` rename).
- `agent/src/tool.rs`: the call loop's provider lookup was rewritten so the `Err(tool_call)` arm no longer needs the removed
  `registered` provider; same results (an error call answers `Err(error.clone())`), `join_all` order unchanged. Test
  `a_tool_name_offered_twice_is_refused` still passes.
- Sizes barely moved by design; the 5.8 MB registry is CP14.2.
- `target/hermes-src` is a stub in this worktree (see Done); run `scripts/build-hermes-webapp.ps1` before anything that serves the page.
- Remaining dead-ish code not removed (judgement calls): pub items used only by their own tests (e.g. `canonical::ModelMapping`,
  `recommended_models_from_registry`, `Provider::fetch_recommended_*`); they belong with CP14.2's registry work.

### Review (Opus 5.5)
`main` had not moved (nothing to merge). `/code-review` not run (manual pass, per the brief).
Checked:
- **Users of deleted items**: every deleted module/pub item grepped across all crates, docs and scripts; no cargo features
  exist in our crates, so `cargo build --workspace` + every crate's test build (incl. `crates/app`, the ACP bin and the
  llm examples) is complete coverage for Rust users. Removed `Provider` trait methods: no implementor in any crate or test
  overrode them (an override of a removed method would not compile).
- **Serde shapes**: `message.rs`, `token_usage.rs`, `model.rs`, `conversation.rs` lose only methods, consts and the unused
  `TokenState` / `ToolCallResult` types; no field or variant of a stored or protocol type changed. `Permission` is
  byte-for-byte the old enum (`#[serde(rename_all = "snake_case")]`, same variants and order). `MessageMetadata.compaction_summary`
  (field) stays; only its builder went.
- **Request bodies**: reasoned from code, not dumped (the `turns` suite has no body dump): the only edits on the request path are
  the `MaybeSend`→`Send` bound of `response_to_streaming_message` and the removal of the unused `stream_for_model` /
  `build_request_for_model`; `create_request` and everything it calls are untouched. `turns` 258/7 as before (many of its
  tests assert on request bodies).
- **`tool.rs` call loop**: equivalent (the removed `registered` provider was always empty; an `Err` call still answers its
  own error, `join_all` order unchanged, the duplicate-name refusal is in `available_tools`, untouched).
- **Not deleting `is_openai_responses_model` / `extract_reasoning_effort`: agreed.** Both feed the request body:
  `create_request` → `extract_reasoning_effort` (wire model name, `reasoning_effort`), `model.rs` (max-tokens field,
  reasoning default), `cache_semantics.rs`. Only the name is a Goose leftover.
- **Dependencies**: dropped ones have no `use`; `cargo build --workspace --locked` passes (lock consistent).
- Formatting churn: every file with rustfmt hunks also has a real change (`inference.rs`'s loop body was mis-indented at HEAD;
  the re-indent is kept); nothing to revert.
Fixes:
- **Retry-After cap (a latent bug the trim would have buried)**: `MAX_RETRY_WAIT_SECS` (600 s, Hermes's `RETRY_AFTER_CAP_S`,
  GAPS.md "Done 2026-11-06") lived only in Goose's unused free function `retry_operation`; the live loop
  (`ProviderRetry::with_retry_config`, used by `OpenAiCompatibleProvider`) slept the Retry-After as given, so a 429 with a
  hint between 600 s and an hour (an hour or more is a daily quota and is not retried) froze the turn that long per attempt.
  Deleting `retry_operation` + `capped_retry_wait` removed the only trace. Now the cap is applied in the live loop; new test
  `the_provider_retry_loop_sleeps_at_most_the_retry_wait_cap` (paused tokio clock, a 1800 s hint sleeps 600 s). No request
  body changes; only the sleep length for such hints.
- `tool.rs`: the call loop's `(Ok, None) => unreachable!()` arm replaced by one `match` on `Result<(provider, call), error>`.
- Left-over empties removed: `impl MessageUsage {}`, `impl GoogleErrorCode {}` and the now unused `GoogleErrorCode`.
- Root `Cargo.toml`: workspace entries `include_dir`, `minijinja`, `urlencoding` removed (no crate uses them any more).
- `vendor/SOURCES.md`: the goose-agent row named `skills/source.rs`, the context-management row pointed at the deleted
  `crates/agent/src/compaction/`; both updated.
Tests after the review (`CARGO_TARGET_DIR=target/claude`): `hermes_llm` 486 (+1) / 7 ignored lib + 1 + 4, `hermes_agent` 7,
`hermes_runtime` lib 265, `--test turns` 258 passed / 7 ignored, `hermes_tools` 337, `hermes_memory` 101, `hermes_store` 34,
`hermes_prompts` 37, `hermes-desktop` 191 + acp 68 (this checkpoint touches no desktop code). `cargo build --workspace --locked`
OK. `cargo clippy -p hermes_llm -p hermes_agent` clean (with `--all-targets`, redundant-clone lints in `conversation.rs` tests,
pre-existing, untouched). `clean-test-temp.ps1 -Delete` run.
Left: the progress note's sizes and line counts not re-measured (my changes are a few lines); `cargo bloat`/`udeps` still not
installed; test-only pub items (above) left for CP14.2.
