# Canonical model registry

Model metadata (context limit, max output, prices, reasoning and tool-call flags, input
modalities) looked up by `(provider, model)`. `name_builder.rs` normalizes provider model names
(e.g. `claude-sonnet-4-5-20250929`) to catalog ids (`anthropic/claude-sonnet-4.5`).

The catalog is `data/canonical_models.json`, compiled in (zstd-compressed by `build.rs`). It is a
**trimmed copy** of Goose's catalog (models.dev data): only the providers and fields this app
reads. Regenerate it from a full catalog with `scripts/trim-model-registry.py`; the provider list
and the field list are documented at the top of that script. The last full copy is in git at
`a7c24dc` (`crates/llm/src/canonical/data/canonical_models.json`).

`examples/registry_probe.rs` prints everything the app reads for a list of (provider, model)
pairs; run it before and after a catalog change and `diff` the outputs.
