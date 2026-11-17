mod model;
mod name_builder;
mod registry;

pub use model::{CanonicalModel, Limit, Modalities, Modality, Pricing};
pub use name_builder::{
    canonical_name, map_provider_name, map_to_canonical_model, strip_openrouter_variant,
    strip_version_suffix,
};
pub use registry::CanonicalModelRegistry;

/// Catalog pricing is not valid for local inference: models served via ollama or a
/// local runtime are actually free to run, so any catalog price would be misleading.
///
/// Azure Foundry is different: it's a meta-provider that proxies models from third-party
/// providers (Anthropic, OpenAI, Meta, etc.). `map_to_canonical_model` already infers the
/// real underlying provider from the model name (e.g. "claude-sonnet-5" -> anthropic,
/// "gpt-5" -> openai) and resolves its public catalog price. That price is a reasonable
/// estimate of the real cost even though it's not guaranteed to match exactly, since Azure
/// billing can vary by deployment region, SKU, offer, and contract/discounts. Callers should
/// treat this as `CostSource::Estimated` rather than `CostSource::ProviderReported`.
fn should_clear_catalog_pricing(provider: &str) -> bool {
    matches!(provider, "ollama" | "local")
}

pub fn maybe_get_canonical_model(provider: &str, model: &str) -> Option<CanonicalModel> {
    maybe_get_canonical_model_with_variant_fallback(provider, model).map(|(canonical, _)| canonical)
}

/// Resolve model metadata and report whether an OpenRouter variant used its base row.
pub fn maybe_get_canonical_model_with_variant_fallback(
    provider: &str,
    model: &str,
) -> Option<(CanonicalModel, bool)> {
    let registry = CanonicalModelRegistry::bundled().ok()?;

    let (canonical_id, is_variant_fallback) =
        match map_to_canonical_model(provider, model, &registry) {
            Some(id) => (id, false),
            None if provider == "openrouter" => (
                map_to_canonical_model(provider, strip_openrouter_variant(model)?, &registry)?,
                true,
            ),
            None => return None,
        };
    let mut canonical = if let Some((canon_provider, canon_model)) = canonical_id.split_once('/') {
        registry.get(canon_provider, canon_model).cloned()?
    } else {
        return None;
    };

    if is_variant_fallback {
        // Variants share capabilities, but endpoint output caps and prices can differ.
        canonical.limit.output = None;
        // `:thinking` is the one tag that changes capabilities: it serves the reasoning mode.
        if model.ends_with(":thinking") {
            canonical.reasoning = Some(true);
        }
        canonical.cost = if model.ends_with(":free") {
            Pricing {
                input: Some(0.0),
                output: Some(0.0),
                ..Pricing::default()
            }
        } else {
            Pricing::default()
        };
    } else if should_clear_catalog_pricing(provider) {
        canonical.cost = Pricing::default();
    } else if name_builder::is_meta_provider(provider) && canonical.cost.has_no_usable_rate() {
        // A meta-provider model can infer to a first-party catalog entry that carries literal
        // 0.0 prices (open-weights publishers such as meta-llama do). The host's own catalog
        // row carries the rate it actually charges to proxy that model, so prefer it. Where
        // there is no such row, report nothing: billing paid proxied inference as free is
        // worse than showing no estimate at all.
        canonical.cost = host_catalog_pricing(provider, model, &registry).unwrap_or_default();
    }

    Some((canonical, is_variant_fallback))
}

/// Pricing from the meta-provider's own catalog rows (`azure/*`, `databricks/*`,
/// `amazon-bedrock/*`), which price proxied inference directly rather than by inferring the
/// upstream publisher. Only the rate is taken: model identity and capabilities stay on the
/// canonical entry that `map_to_canonical_model` resolved.
fn host_catalog_pricing(
    provider: &str,
    model: &str,
    registry: &CanonicalModelRegistry,
) -> Option<Pricing> {
    let host = name_builder::map_provider_name(provider);
    let stripped = name_builder::strip_version_suffix(model);
    let cost = registry
        .get(host, &stripped)
        .or_else(|| registry.get(host, model))
        .or_else(|| registry.get(host, &stripped.to_ascii_lowercase()))?
        .cost
        .clone();
    (!cost.has_no_usable_rate()).then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case::test_case("deepseek/deepseek-v4-flash:free"; "free")]
    #[test_case::test_case("deepseek/deepseek-v4-flash-0731:free"; "dated free")]
    #[test_case::test_case("deepseek/deepseek-v4-flash-0731:nitro"; "dated nitro")]
    #[test_case::test_case("deepseek/deepseek-v4-flash:floor"; "floor")]
    #[test_case::test_case("deepseek/deepseek-v4-flash:online"; "online")]
    #[test_case::test_case("deepseek/deepseek-v4-flash:batch"; "batch")]
    fn openrouter_variants_resolve_to_base_model(model: &str) {
        let canonical = maybe_get_canonical_model("openrouter", model).unwrap();
        let base = maybe_get_canonical_model("openrouter", "deepseek/deepseek-v4-flash").unwrap();
        assert_eq!(canonical.id, base.id);
        assert_eq!(canonical.limit.context, base.limit.context);
        assert_eq!(canonical.reasoning, base.reasoning);
        assert_eq!(canonical.modalities.input, base.modalities.input);
    }

    #[test]
    fn openrouter_exact_free_model_keeps_its_catalog_entry() {
        let model = "google/gemma-4-31b-it:free";
        let row = CanonicalModelRegistry::bundled()
            .unwrap()
            .get("openrouter", model)
            .cloned()
            .unwrap();
        let (canonical, is_variant_fallback) =
            maybe_get_canonical_model_with_variant_fallback("openrouter", model).unwrap();
        assert!(!is_variant_fallback);
        assert_eq!(canonical.id, row.id);
        assert!(row.limit.output.is_some());
        assert_eq!(canonical.limit.output, row.limit.output);
    }

    #[test]
    fn openrouter_thinking_variant_is_reasoning() {
        let base = maybe_get_canonical_model("openrouter", "qwen/qwen-plus-2025-07-28").unwrap();
        assert_eq!(base.reasoning, Some(false));

        let (thinking, is_variant_fallback) = maybe_get_canonical_model_with_variant_fallback(
            "openrouter",
            "qwen/qwen-plus-2025-07-28:thinking",
        )
        .unwrap();
        assert!(is_variant_fallback);
        assert_eq!(thinking.id, base.id);
        assert_eq!(thinking.reasoning, Some(true));
    }

    #[test]
    fn non_openrouter_colon_model_does_not_resolve_to_base_model() {
        assert!(maybe_get_canonical_model("deepseek", "deepseek-v4-flash").is_some());
        assert!(maybe_get_canonical_model("deepseek", "deepseek-v4-flash:free").is_none());
    }

    #[test_case::test_case("deepseek/deepseek-v4-flash:free"; "free")]
    #[test_case::test_case("deepseek/deepseek-v4-flash-0731:free"; "dated free")]
    fn openrouter_free_fallback_has_zero_cost(model: &str) {
        let paid = maybe_get_canonical_model("openrouter", "deepseek/deepseek-v4-flash").unwrap();
        assert!(paid.cost.input.is_some_and(|price| price > 0.0));
        assert!(paid.cost.output.is_some_and(|price| price > 0.0));

        let free = maybe_get_canonical_model("openrouter", model).unwrap();
        assert_eq!(free.id, paid.id);
        assert_eq!(free.limit.context, paid.limit.context);
        assert_eq!(free.cost.input, Some(0.0));
        assert_eq!(free.cost.output, Some(0.0));
    }

    #[test_case::test_case("deepseek/deepseek-v4-flash:batch"; "batch")]
    #[test_case::test_case("deepseek/deepseek-v4-flash:nitro"; "nitro")]
    fn openrouter_variant_fallback_has_no_catalog_price(model: &str) {
        let canonical = maybe_get_canonical_model("openrouter", model).unwrap();
        assert_eq!(canonical.cost.input, None);
        assert_eq!(canonical.cost.output, None);
    }

    #[test_case::test_case("ollama")]
    #[test_case::test_case("local")]
    fn local_models_have_no_catalog_cost(provider: &str) {
        let cloud = maybe_get_canonical_model("mistralai", "ministral-3b")
            .expect("ministral-3b should resolve for its first-party provider");
        assert!(cloud.cost.input.is_some_and(|price| price > 0.0));
        assert!(cloud.cost.output.is_some_and(|price| price > 0.0));

        let canonical = maybe_get_canonical_model(provider, "ministral-3b")
            .expect("ministral-3b should resolve for local inference");
        assert_eq!(canonical.id, cloud.id);
        assert_eq!(canonical.cost.input, None);
        assert_eq!(canonical.cost.output, None);
        assert_eq!(canonical.limit.context, cloud.limit.context);
    }

    #[test]
    fn azure_foundry_models_use_inferred_provider_pricing() {
        let canonical = maybe_get_canonical_model("azure_foundry", "gpt-5")
            .expect("gpt-5 should resolve through the Azure catalog");
        assert_eq!(canonical.limit.context, 400_000);
        let openai = maybe_get_canonical_model("openai", "gpt-5")
            .expect("gpt-5 should resolve for its first-party provider");
        assert_eq!(canonical.cost.input, openai.cost.input);
        assert_eq!(canonical.cost.output, openai.cost.output);
        assert!(canonical.cost.input.is_some_and(|price| price > 0.0));
        assert!(canonical.cost.output.is_some_and(|price| price > 0.0));
    }

    #[test]
    fn azure_foundry_models_use_host_catalog_pricing_without_a_publisher_entry() {
        let canonical = maybe_get_canonical_model("azure_foundry", "llama-3.3-70b-instruct")
            .expect("llama-3.3-70b-instruct should resolve through the Azure catalog");
        assert_eq!(canonical.id, "azure/llama-3.3-70b-instruct");
        assert_eq!(canonical.cost.input, Some(0.71));
        assert_eq!(canonical.cost.output, Some(0.71));
        assert!(canonical.limit.context > 0);
    }

    #[test]
    fn cloud_provider_retains_cost() {
        let canonical = maybe_get_canonical_model("anthropic", "claude-sonnet-4-5-20250929")
            .expect("claude-sonnet-4.5 should resolve");
        assert!(canonical.cost.input.is_some());
        assert!(canonical.cost.output.is_some());
    }

    #[test]
    fn anthropic_opus_5_resolves() {
        let canonical = maybe_get_canonical_model("anthropic", "claude-opus-5")
            .expect("claude-opus-5 should resolve");
        assert_eq!(canonical.limit.context, 1_000_000);
        assert_eq!(canonical.limit.output, Some(128_000));
        assert!(canonical.cost.input.is_some());
        assert!(canonical.cost.output.is_some());
    }

    #[test]
    fn anthropic_opus_5_5_resolves() {
        let canonical = maybe_get_canonical_model("anthropic", "claude-opus-5-5")
            .expect("claude-opus-5-5 should resolve");
        assert_eq!(canonical.id, "anthropic/claude-opus-5.5");
        assert_eq!(canonical.limit.context, 1_000_000);
        assert_eq!(canonical.limit.output, Some(128_000));
        assert_eq!(canonical.cost.input, Some(4.0));
        assert_eq!(canonical.cost.output, Some(20.0));
    }

    #[test]
    fn openai_gpt_6_sol_and_luna_resolve() {
        for (model, input_cost) in [("gpt-6-sol", 2.0), ("gpt-6-luna", 0.1)] {
            let canonical = maybe_get_canonical_model("openai", model)
                .unwrap_or_else(|| panic!("{model} should resolve"));
            assert_eq!(canonical.limit.context, 1_050_000);
            assert_eq!(canonical.limit.output, Some(128_000));
            assert_eq!(canonical.cost.input, Some(input_cost));
            assert!(canonical.tool_call);
        }
    }

    #[test]
    fn kimi_code_k3_resolves_with_reasoning_and_context_limit() {
        let canonical = maybe_get_canonical_model("kimi_code", "k3")
            .expect("kimi_code/k3 should resolve via kimi-code-plan-cn provider mapping");
        assert_eq!(canonical.limit.context, 1_048_576);
        assert_eq!(canonical.reasoning, Some(true));
    }
}
