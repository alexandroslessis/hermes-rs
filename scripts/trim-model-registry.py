"""Trim the full canonical model catalog to what hermes-rs reads (CP14.2).

Input: the full catalog (Goose's `canonical_models.json`, built from models.dev; the last
full copy is in git at a7c24dc: `git show a7c24dc:crates/llm/src/canonical/data/canonical_models.json`).
Output: `crates/llm/src/canonical/data/canonical_models.json`, compact, only the providers and
fields below. Deterministic: rows keep the input order.

    ~/.local/bin/uv.exe run --no-project python scripts/trim-model-registry.py FULL.json \
        crates/llm/src/canonical/data/canonical_models.json

Providers kept = every catalog provider our code can look up:
  * the names `discovery::catalog_provider_for` returns (after `map_provider_name`),
  * the real publishers `name_builder` infers a model name back to (and its `extract_provider_prefix` list),
  * the meta-provider hosts priced by `host_catalog_pricing` (azure, databricks, amazon-bedrock, google-vertex),
  * every other name `map_provider_name` can produce (so a provider id that is one of those still
    resolves its own rows).
A catalog provider outside this list is only reached when a user names an endpoint exactly like it
(then the lookup falls through to the inference rules, as for any unknown id).

Fields kept = the ones read: id, name, tool_call, reasoning, reasoning_efforts,
modalities.input, cost.input, cost.output, limit.context, limit.output.
"""
import json
import sys

PROVIDERS = {
    # catalog_provider_for (+ map_provider_name: xai -> x-ai)
    "openrouter", "openai", "deepseek", "x-ai", "mistral", "groq", "togetherai",
    "fireworks-ai", "google", "moonshotai",
    # name_builder::infer_provider_from_model and extract_provider_prefix
    "anthropic", "meta-llama", "mistralai", "qwen", "ai21", "cohere", "zhipuai",
    "amazon", "nvidia", "microsoft", "perplexity",
    # meta-provider hosts
    "azure", "databricks", "amazon-bedrock", "google-vertex",
    # the rest of map_provider_name's outputs
    "novita-ai", "opencode-go", "opencode", "ollama-cloud", "kimi-code-plan-cn", "meta",
    "lmstudio",
}


def trim(model):
    out = {"id": model["id"], "name": model["name"]}
    for key in ("reasoning", "reasoning_efforts"):
        if key in model:
            out[key] = model[key]
    out["tool_call"] = model.get("tool_call", False)
    modal = model.get("modalities", {}).get("input")
    if modal is not None:
        out["modalities"] = {"input": modal}
    cost = {k: model["cost"][k] for k in ("input", "output") if k in model.get("cost", {})}
    if cost:
        out["cost"] = cost
    limit = {k: model["limit"][k] for k in ("context", "output") if k in model.get("limit", {})}
    if limit:
        out["limit"] = limit
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf8") as f:
        models = json.load(f)
    kept = [trim(m) for m in models if m["id"].split("/", 1)[0] in PROVIDERS]
    with open(dst, "w", encoding="utf8", newline="\n") as f:
        json.dump(kept, f, ensure_ascii=False, separators=(",", ":"))
        f.write("\n")
    print(f"{len(models)} -> {len(kept)} models", file=sys.stderr)


main()
