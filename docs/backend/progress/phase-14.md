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

## CP14.2 — model registry and notices
Worktree: C:\Users\alexandros\repos\hermes-rs-p14   Branch: backend/p14-trim   Model: Sonnet 5.5 (subagent)
On top of CP14.1 (a7c24dc). Not committed.

### Decision and why
What the code reads (every user of `canonical::`, all inside `hermes_llm`): `model.rs` (`with_canonical_limits`: output cap,
reasoning, image input; `openai_reasoning_for_model`), `formats/openai.rs` (`reasoning_efforts`), `base.rs`
(`model_info_for_provider_model`), `context_limit.rs` (`limit.context`) and `discovery.rs` (what the model picker and the
context-length lookup show: `name`, `limit.context`, `limit.output`, `cost.input`, `cost.output`, `tool_call`, `reasoning`,
image input). Provider names looked up: `discovery::catalog_provider_for` (openrouter, openai (also the default for every
local endpoint), deepseek, xai -> x-ai, mistral, groq, togetherai, fireworks-ai, google, moonshotai), the fixed `"openai"` of
`model.rs` / `openai.rs`, and the endpoint id the user typed (`Provider::get_name`); an id the catalog doesn't know falls to the
inference rules of `name_builder.rs` (anthropic, openai, google, meta-llama, mistralai, deepseek, qwen, x-ai, ai21, cohere,
zhipuai, the `extract_provider_prefix` list) and, for meta-providers, the host rows of `host_catalog_pricing`.
No code read these, so they are deleted: `models_dev.rs` (the models.dev converter), the remote-catalog fetch/cache in
`registry.rs`, `canonical_mapping_report.json`, `recommended_models_from_registry`, `provider_wire_name`, `ModelMapping`,
`Provider::fetch_recommended_models` / `fetch_recommended_model_info` / `map_to_canonical_model` / `skip_canonical_filtering`
(CP14.1 had listed them as test-only), and the fields nothing read: `family`, `attachment`, `temperature`, `knowledge`,
`release_date`, `last_updated`, `open_weights`, `thinking_mode`, `modalities.output`, cache prices and price tiers
(`PricingTier`, `ThinkingMode`, `Pricing::estimate_cost`; a turn's cost is computed elsewhere).
Chosen: **a trimmed embedded JSON**, not "fetch `/models` and cache": discovery already fetches `/models` and uses the catalog
only as fallback for what the endpoint leaves out, so dropping it would change answers; the trim keeps them.
`scripts/trim-model-registry.py FULL.json OUT.json` makes it reproducibly (providers and fields listed at its top; the full
catalog stays in git at a7c24dc). The zstd step in `build.rs` stays; `registry.rs` is a plain `Lazy` now (the `RwLock` only
existed to swap in a fetched catalog) and `bundled()` returns `&'static CanonicalModelRegistry`.
Providers kept: 36 of 232 catalog providers, 1,371 of 8,744 rows (the names above, every `map_provider_name` output, the hosts
azure / databricks / amazon-bedrock / google-vertex, `lmstudio`). Dropped: the ~200 gateways and aggregators.
Fields kept (10): `id`, `name`, `tool_call`, `reasoning`, `reasoning_efforts`, `modalities.input`, `cost.input`,
`cost.output`, `limit.context`, `limit.output`. Catalog JSON 5,778,517 B -> 335,439 B (zstd inside the binary 150,101 ->
19,819 B); `canonical_mapping_report.json` (140,532 B) deleted.

### Sizes (release `hermes-desktop.exe`, `CARGO_TARGET_DIR=target/claude`, the stub `dist-webapp` of CP14.1)
Before CP14.2 (the CP14.1 result): 42,993,152 B. After: 42,801,664 B (-191,488 B). Debug: 64,111,616 -> 63,899,648 B (-211,968).
The binary barely moves: the old JSON was already compressed in the exe (150 kB). The gain is elsewhere: the registry parsed at
the first lookup was 5.8 MB of JSON / 8,744 models, now 0.34 MB / 1,371 (startup time, resident memory: not measured, no perf
checks per the hold) and 5.9 MB less in the repo. Rust lines in `crates/llm/src`: +34 / -965.

### Equivalence evidence (the hard requirement)
Oracle = the code and data of a7c24dc (`git archive a7c24dc | tar -x` into a scratch folder, built there with its own target
dir). `crates/llm/examples/registry_probe.rs` (new, committed; compiles at both commits) prints, for every (provider, model) pair
of two input files, everything our code reads: `map_to_canonical_model`; `maybe_get_canonical_model_with_variant_fallback`
(id, name, variant flag, context, output cap, input/output price, reasoning, efforts, tool_call, input modalities);
`ContextLimitResolver::resolve_local`; `ModelConfig::with_canonical_limits` (max tokens, reasoning, vision) and
`is_reasoning_model`; `model_info_for_provider_model`; the discovery enrichment (`parse_models_response`); and, for `openai`,
`is_openai_reasoning_model` and `openai_reasoning_effort_for_thinking` at all five efforts. A line is written only when it
differs from the answer for a model that is in no catalog.
- Providers (57): the 36 kept catalog providers plus every name the code can map or infer (`ollama`, `lm-studio`, `local`,
  `custom`, `xai`, `xai_oauth`, `azure_openai`, `azure_foundry`, `aws_bedrock`, `gcp_vertex_ai`, `gemini_oauth`, `chatgpt_codex`,
  `databricks_v2`, `zhipu`, `moonshot`, `together`, `novita`, `opencode_go`, `opencode_zen`, `ollama_cloud`, `kimi_code`,
  `muse_code`, `tetrate`, `bedrock`, `mistral`, `meta-llama`, `qwen`, `vllm`, `unknown-provider`, empty).
- Models (38,501): for EVERY row of the old catalog its bare name, its full id (OpenRouter style), `:free`, `:thinking`,
  `-20250101`, a `databricks-` prefix, upper case, its display name and `-high` (effort suffix).
- Result: 57 x 38,501 = 2,194,557 pairs, 603,933 of them resolve to something; the old and the new output are
  **byte-identical** (`cmp`, 336,692,718 B each; run twice, the second time on the final tree). So context length, output cap,
  prices, reasoning, efforts, vision and tool flags, the model-picker enrichment and the variant / free / local-cost /
  meta-provider-host rules resolve the same for every pair, and unknown models still fall to `DEFAULT_CONTEXT_LIMIT` (128,000).
- Control, the 205 dropped catalog providers used as provider names (x 12,304 names and ids): 994,917 old lines vs 990,765 new;
  987,542 identical, 4,152 lost, 3,223 changed. Of the 7,375 differing pairs, 7,373 are pairs where the old answer came from the
  dropped provider's OWN catalog row (a direct hit, e.g. an endpoint literally id'd `poe`); those ids now go through the inference
  rules like any unknown id. The other 2 (`opper`/`gpt-5.3-codex-xhigh`, `poe`/`openai/o3-mini-low`) are the same cause:
  `with_canonical_limits` retries the name without its effort suffix, and that retry hit the gateway's row (vision flag differs).
  An endpoint in the app never has such an id unless the user names it so; `catalog_provider_for` never returns one.
- Unit tests that pin values (claude-opus-5 1,000,000 / 128,000, gpt-5 400,000 via Azure, gpt-6-sol 1,050,000, kimi k3, the
  OpenRouter variant and free rules...) pass unchanged except the ones of deleted code (below).

### Done
- Catalog trimmed; `canonical_mapping_report.json`, `models_dev.rs` deleted; `registry.rs`, `canonical.rs`, `model.rs` (slim
  `CanonicalModel`, `Pricing {input, output}`), `base.rs` (unused trait methods) simplified.
- `scripts/trim-model-registry.py`, `crates/llm/examples/registry_probe.rs`; `canonical/README.md` rewritten (it described a
  `build_canonical_models` binary that is not in this repo).
- Notices: `THIRD_PARTY_NOTICES.md` row for models.dev, `PORTING.md` (Goose row: diverged, what is gone), `vendor/SOURCES.md`
  (catalog row), `README.md` (crate table).
- Tests removed with their code: `estimate_cost` x3, `recommended_*` x2, `provider_wire_name`, the `fetch_recommended_models`
  test and its `ModelInventoryProvider`, the `models_dev.rs` tests; two assertions on `temperature` / `thinking_mode` (the first
  replaced by the output-cap assertion of the same row, the second dropped).

### Tests (`CARGO_TARGET_DIR=target/claude`)
`hermes_llm` 476 passed / 7 ignored lib (was 486: the 10 tests of deleted code) + 1 + 4, `hermes_agent` 7, `hermes_runtime`
lib 265, `--test turns` 258 passed / 7 ignored, `hermes_tools` 337, `hermes_memory` 101, `hermes_store` 34, `hermes_prompts` 37,
`hermes-desktop` 191 + acp 68. `cargo build --workspace` OK; no warnings in `hermes_llm`. `scripts/clean-test-temp.ps1 -Delete`
run (389 folders). No perf checks. No live check: nothing the page calls changed (the probe shows the same answers).

### For the reviewer
- Check `scripts/trim-model-registry.py`'s provider list against `name_builder.rs` (`map_provider_name`,
  `infer_provider_from_model`, `extract_provider_prefix`, `is_meta_provider`) and `discovery::catalog_provider_for`: a provider
  the code can reach that is missing from the list is the one way this checkpoint can change an answer. The probe proves
  equality only for the 57 provider names and the model spellings listed above.
- Decide whether dropping the ~200 gateway providers is acceptable for an endpoint a user names exactly like one (`poe`,
  `kilo`, ...): its direct-hit rows are gone and the inference rules answer instead (control above).
- Re-run the proof: build `registry_probe` at a7c24dc and here, give both the same provider list and model list (instructions at
  the top of the example; my input files were made from the old catalog by a throw-away script, not committed), `cmp` the outputs.
- The full catalog can no longer be refreshed from this repo: Goose's `build_canonical_models` was never here and the
  models.dev converter is deleted (nothing called it). To update: take a newer Goose `canonical_models.json`, run the trim
  script. Acceptable?
- `THIRD_PARTY_NOTICES.md`: models.dev is MIT as far as I know; I added the row but no licence text under `licenses/` (no network
  use). Please check the licence and add the text if you want it.
- `registry_probe.rs` is an example binary kept for the proof; remove it if you want no dev tooling in the crate.
- `CanonicalModelRegistry::bundled()` now returns `&'static` instead of a read guard; callers compile unchanged.

### Review (Opus 5.5)
`main` had not moved (nothing to merge). `/code-review` not run (manual pass).
Checked:
- **Which provider names reach the catalog in the running app** (traced, not taken from the note): the model picker,
  context lengths and prices come from `discovery::list_models` -> `parse_models_response(.., catalog_provider_for(base_url))`
  (10 fixed names; every local or unknown host is `"openai"`); `formats/openai.rs` and `model.rs` look up the fixed `"openai"`.
  `with_canonical_limits`, `ContextLimitResolver` / `Provider::get_context_limit` and `ModelInfo` have no caller outside
  `hermes_llm` (and its tests); the endpoint id the user typed reaches only `fetch_model_info`, of which `inference.rs` reads
  `resolved_model`, always `None`. The GPUI app only uses `DiscoveredModel`. Statically, every `registry.get(p, ..)` in
  `name_builder.rs` / `canonical.rs` uses `p` = `map_provider_name(provider)`, an `infer_provider_from_model` /
  `extract_provider_prefix` output or a meta-provider host; all are in the script's list.
- **Independent proof**: the a7c24dc tree from `git archive` into a scratch folder (outside the repo, own target dir) with the
  probe copied in; built both; 27 provider names (the 10 discovery names, `ollama`, `lm-studio`, `lmstudio`, `local`,
  `llama-cpp`, `vllm`, `anthropic`, `azure`, `azure_openai`, `bedrock`, `aws_bedrock`, `databricks`, `gcp_vertex_ai`,
  `custom`, `unknown-provider`, `openrouter-2`, `my-openrouter`) x 61,680 model names (84 real-world spellings such as
  `gpt-4o`, `claude-sonnet-4`, `qwen3:30b`, `llama3.1:8b`, `google/gemini-2.5-pro`, `deepseek/deepseek-r1:free`,
  `z-ai/glm-4.6`, `accounts/fireworks/models/...`, plus every catalog row's bare and full id, each also with `:free`,
  `:thinking`, `-high`, `-20250101`): 527,327 resolving lines, outputs **byte-identical** (281,176,670 B each).
- **Gateway-named endpoints (the `poe` case)**: 15 dropped gateways (`poe`, `requesty`, `vercel`, `cerebras`, `deepinfra`,
  `huggingface`, ...) x the 84 real names: 536 old lines vs 510 new; the differences are those ids' own rows (e.g. `poe` /
  `anthropic/claude-sonnet-4` had Poe's 983,040 context). **Acceptable**: only the unread `ModelInfo` fields would see it; the
  page's list for such an endpoint has always been looked up under `catalog_provider_for(its URL)` = `"openai"`, never
  under `poe`, so nothing the user sees changes.
- **Deleted code**: `load_cached_catalog` / `fetch_remote_catalog` / `RemoteCatalog::install` had no caller at a7c24dc in any
  crate (CP13c's audit row 22 already said so), so no fetch was lost and no `models_dev_api.json` / `.etag` was ever written
  to a data folder. `recommended_*`, `provider_wire_name`, `ModelMapping`, `Provider::fetch_recommended_*` /
  `map_to_canonical_model` / `skip_canonical_filtering`: no user in desktop, ACP, runtime, `crates/app`, scripts or docs.
  The desktop's `"release_date": ""` (`rest.rs`) is a page field, unrelated. Serde: `CanonicalModel` is only read from the
  embedded JSON, never stored or sent, so the dropped fields change no protocol or store shape.
- **Reproducibility**: `scripts/trim-model-registry.py` on `git show a7c24dc:.../canonical_models.json` regenerates the
  committed file byte-for-byte (`cmp`). JSON number round-trip is exact (Python `repr` of a float is the shortest exact form).
- **Sizes**: the embedded `canonical_models.json.zst` is 150,101 B at a7c24dc and 19,819 B now (both re-built here); the
  release exe in `target/claude` is 42,801,664 B as the note says; the before size not re-built. Build and first-lookup
  parse are over 0.34 MB instead of 5.8 MB.
- Ground rules: no protocol, RPC or store change; `PORTING.md` Goose row and `vendor/SOURCES.md` say diverged/trimmed.
Fixes:
- **Licence text added**: models.dev's `LICENSE` is MIT, "Copyright (c) 2025 models.dev" (checked upstream); the text is now
  `licenses/MIT-models.dev.txt` (the project's convention, like `MIT-hermes-agent.txt`) and the `THIRD_PARTY_NOTICES.md` row
  points at it. (The full catalog shipped since the Goose copy without this notice.)
- **The count in the note was wrong**: the script lists 32 names, and the trimmed file has rows of **27** catalog providers
  (`mistral`, `qwen`, `meta-llama`, `amazon`, `microsoft` have no rows of their own), not 36. Harmless for the answers.
- Docs: PITFALLS "The model catalog" (new: trimmed copy, what reaches it, how to add a provider, how to check a change),
  ARCHITECTURE crate row, PHASES Phase 14 marked done with sizes and notes.
Kept as they are: `registry_probe.rs` (documented tool for the next catalog change; it builds with the crate's examples);
no catalog refresh path (agreed: take a newer Goose catalog and rerun the script).
Tests after the review (`CARGO_TARGET_DIR=target/claude`, `--locked`): `hermes_llm` 476 / 7 ignored lib + 1 + 4, `hermes_agent` 7,
`hermes_runtime` lib 265, `--test turns` 258 passed / 7 ignored, `hermes_tools` 337, `hermes_memory` 101, `hermes_store` 34,
`hermes_prompts` 37, `hermes-desktop` 191 + acp 68; `cargo build --workspace --locked` OK. `clean-test-temp.ps1 -Delete` run;
the scratch oracle tree removed. No perf or live checks (user hold; nothing the page calls changed).
User checklist (the app, after merging and rebuilding):
1. Settings > Providers or the model picker on your usual endpoint (OpenRouter, Ollama, LM Studio): the models list with
   the same context lengths as before (e.g. an OpenRouter Claude or Gemini model shows its usual window).
2. With an OpenRouter model, send a message and open the usage view (`/usage` or the usage panel): the cost estimate is
   shown as before, and a `:free` model still costs 0.
3. Nothing else should look different; no new files appear in `%APPDATA%\hermes-rs`.
