//! hermes-rs: automatic model discovery for OpenAI-compatible providers.
//!
//! `GET {base_url}/models` lists every model a provider serves. OpenRouter (and some others)
//! include context length, pricing and capabilities per model; when a field is missing, the
//! bundled canonical catalog fills it in by matching the model id.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_client::ApiClient;
use crate::canonical::{map_to_canonical_model, CanonicalModelRegistry, Modality};
use crate::errors::ProviderError;
use crate::openai_compatible::handle_response_openai_compat;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<usize>,
    pub max_output_tokens: Option<usize>,
    /// USD per million input tokens.
    pub input_price: Option<f64>,
    /// USD per million output tokens.
    pub output_price: Option<f64>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub supports_vision: Option<bool>,
}

/// The provider name used to look models up in the canonical catalog, from the base URL.
pub fn catalog_provider_for(base_url: &str) -> &'static str {
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    [
        ("openrouter.ai", "openrouter"),
        ("api.openai.com", "openai"),
        ("api.deepseek.com", "deepseek"),
        ("api.x.ai", "xai"),
        ("api.mistral.ai", "mistral"),
        ("api.groq.com", "groq"),
        ("api.together.xyz", "togetherai"),
        ("api.fireworks.ai", "fireworks-ai"),
        ("generativelanguage.googleapis.com", "google"),
        ("api.moonshot.ai", "moonshotai"),
    ]
    .iter()
    .find(|(domain, _)| host == *domain || host.ends_with(&format!(".{domain}")))
    .map(|(_, provider)| *provider)
    .unwrap_or("openai")
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn usize_field(entry: &Value, paths: &[&[&str]]) -> Option<usize> {
    paths.iter().find_map(|path| {
        let value = path.iter().try_fold(entry, |value, key| value.get(key))?;
        number(value).filter(|n| *n > 0.0).map(|n| n as usize)
    })
}

/// Reads the fields different servers put in their `/models` entries.
fn parse_entry(entry: &Value) -> Option<DiscoveredModel> {
    let id = entry.get("id")?.as_str()?.to_string();
    let per_token_to_per_million =
        |value: &Value| number(value).filter(|n| *n >= 0.0).map(|n| n * 1_000_000.0);
    let supported: Option<Vec<&str>> = entry
        .get("supported_parameters")
        .and_then(Value::as_array)
        .map(|params| params.iter().filter_map(Value::as_str).collect());
    let input_modalities: Option<Vec<&str>> = entry
        .pointer("/architecture/input_modalities")
        .and_then(Value::as_array)
        .map(|modalities| modalities.iter().filter_map(Value::as_str).collect());
    Some(DiscoveredModel {
        name: entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_length: usize_field(
            entry,
            &[
                &["context_length"],
                &["max_model_len"],
                &["context_window"],
                &["top_provider", "context_length"],
            ],
        ),
        max_output_tokens: usize_field(
            entry,
            &[
                &["top_provider", "max_completion_tokens"],
                &["max_output_tokens"],
            ],
        ),
        input_price: entry
            .pointer("/pricing/prompt")
            .and_then(per_token_to_per_million),
        output_price: entry
            .pointer("/pricing/completion")
            .and_then(per_token_to_per_million),
        supports_tools: supported.as_ref().map(|params| params.contains(&"tools")),
        supports_reasoning: supported
            .as_ref()
            .map(|params| params.contains(&"reasoning")),
        supports_vision: input_modalities.map(|modalities| modalities.contains(&"image")),
        id,
    })
}

/// Fills fields the endpoint didn't report from the bundled canonical catalog.
fn enrich(model: &mut DiscoveredModel, provider: &str, registry: &CanonicalModelRegistry) {
    let Some(canonical) =
        map_to_canonical_model(provider, &model.id, registry).and_then(|canonical_id| {
            let (provider, name) = canonical_id.split_once('/')?;
            registry.get(provider, name)
        })
    else {
        return;
    };
    model.name.get_or_insert_with(|| canonical.name.clone());
    if model.context_length.is_none() && canonical.limit.context > 0 {
        model.context_length = Some(canonical.limit.context);
    }
    model.max_output_tokens = model.max_output_tokens.or(canonical.limit.output);
    model.input_price = model.input_price.or(canonical.cost.input);
    model.output_price = model.output_price.or(canonical.cost.output);
    model.supports_tools.get_or_insert(canonical.tool_call);
    if let Some(reasoning) = canonical.reasoning {
        model.supports_reasoning.get_or_insert(reasoning);
    }
    model
        .supports_vision
        .get_or_insert(canonical.modalities.input.contains(&Modality::Image));
}

pub fn parse_models_response(
    json: &Value,
    provider: &str,
) -> Result<Vec<DiscoveredModel>, ProviderError> {
    if let Some(error) = json.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(ProviderError::Authentication(message.to_string()));
    }
    let entries = json.get("data").and_then(Value::as_array).ok_or_else(|| {
        ProviderError::RequestFailed("Missing 'data' array in models response".into())
    })?;
    let mut models: Vec<DiscoveredModel> = entries.iter().filter_map(parse_entry).collect();
    if let Ok(registry) = CanonicalModelRegistry::bundled() {
        for model in &mut models {
            enrich(model, provider, &registry);
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// Lists every model the provider serves, with whatever metadata is known.
pub async fn list_models(
    client: &ApiClient,
    base_url: &str,
) -> Result<Vec<DiscoveredModel>, ProviderError> {
    let response = client
        .response_get("models")
        .await
        .map_err(|error| ProviderError::RequestFailed(error.to_string()))?;
    let json = handle_response_openai_compat(response).await?;
    parse_models_response(&json, catalog_provider_for(base_url))
}

/// Ollama's OpenAI-compatible `/v1/models` lists bare ids. Its native API (`root` is the server
/// without `/v1`) knows the rest: `/api/show` gives a model's trained context window, the
/// `num_ctx` it was configured with, and its capabilities; `/api/ps` the context a loaded model
/// actually runs with. Does nothing when the server isn't Ollama.
pub async fn enrich_from_ollama(root: &ApiClient, models: &mut [DiscoveredModel]) {
    let is_ollama = match root.api_get("api/version").await {
        Ok(response) => {
            response.status.is_success()
                && response
                    .payload
                    .as_ref()
                    .is_some_and(|payload| payload.get("version").is_some())
        }
        Err(_) => false,
    };
    if !is_ollama {
        return;
    }
    let running: std::collections::HashMap<String, usize> = match root.api_get("api/ps").await {
        Ok(response) => response
            .payload
            .as_ref()
            .and_then(|payload| payload.get("models"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|model| {
                Some((
                    model.get("name")?.as_str()?.to_string(),
                    model.get("context_length")?.as_u64()? as usize,
                ))
            })
            .collect(),
        Err(_) => Default::default(),
    };
    for model in models.iter_mut() {
        let payload = serde_json::json!({ "model": model.id });
        if let Ok(response) = root.api_post("api/show", &payload).await {
            if let Some(show) = response.payload.filter(|_| response.status.is_success()) {
                apply_ollama_show(model, &show, running.get(&model.id).copied());
            }
        }
    }
}

fn apply_ollama_show(model: &mut DiscoveredModel, show: &Value, running: Option<usize>) {
    let trained = show
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|info| {
            info.iter()
                .find(|(key, _)| key.ends_with(".context_length"))
                .and_then(|(_, value)| value.as_u64())
        })
        .map(|tokens| tokens as usize);
    let configured = show
        .get("parameters")
        .and_then(Value::as_str)
        .and_then(|parameters| {
            parameters.lines().find_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next() == Some("num_ctx"))
                    .then(|| parts.next()?.parse::<usize>().ok())
                    .flatten()
            })
        });
    // What the server runs the model with beats what the catalog guessed from its name.
    if let Some(context) = running.or(configured).or(trained) {
        model.context_length = Some(context);
    }
    if let Some(capabilities) = show.get("capabilities").and_then(Value::as_array) {
        let has = |name: &str| capabilities.iter().any(|c| c.as_str() == Some(name));
        model.supports_tools = Some(has("tools"));
        model.supports_reasoning = Some(has("thinking"));
        model.supports_vision = Some(has("vision"));
    }
    // A local server costs nothing per token.
    model.input_price.get_or_insert(0.);
    model.output_price.get_or_insert(0.);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ollama_show_fills_context_and_capabilities() {
        let show = json!({
            "parameters": "temperature 0.7\nnum_ctx 16384",
            "model_info": {"qwen3.context_length": 40960},
            "capabilities": ["completion", "tools", "thinking"],
        });
        let mut model = DiscoveredModel {
            id: "qwen3:8b".into(),
            ..Default::default()
        };
        apply_ollama_show(&mut model, &show, None);
        assert_eq!(model.context_length, Some(16384));
        assert_eq!(model.supports_tools, Some(true));
        assert_eq!(model.supports_reasoning, Some(true));
        assert_eq!(model.supports_vision, Some(false));
        assert_eq!(model.input_price, Some(0.));
        // A loaded model's real context wins.
        apply_ollama_show(&mut model, &show, Some(32768));
        assert_eq!(model.context_length, Some(32768));
        // Without num_ctx, the trained window.
        let show = json!({"model_info": {"llama.context_length": 131072}});
        apply_ollama_show(&mut model, &show, None);
        assert_eq!(model.context_length, Some(131072));
    }

    #[test]
    fn parses_openrouter_rich_fields() {
        let response = json!({"data": [{
            "id": "anthropic/claude-sonnet-4.5",
            "name": "Anthropic: Claude Sonnet 4.5",
            "context_length": 1000000,
            "pricing": {"prompt": "0.000003", "completion": "0.000015"},
            "architecture": {"input_modalities": ["text", "image"]},
            "top_provider": {"max_completion_tokens": 64000},
            "supported_parameters": ["tools", "reasoning", "temperature"],
        }]});
        let models = parse_models_response(&response, "openrouter").unwrap();
        let model = &models[0];
        assert_eq!(model.context_length, Some(1_000_000));
        assert_eq!(model.max_output_tokens, Some(64_000));
        assert!((model.input_price.unwrap() - 3.0).abs() < 1e-9);
        assert!((model.output_price.unwrap() - 15.0).abs() < 1e-9);
        assert_eq!(model.supports_tools, Some(true));
        assert_eq!(model.supports_reasoning, Some(true));
        assert_eq!(model.supports_vision, Some(true));
    }

    #[test]
    fn bare_ids_are_listed_and_enriched_from_the_catalog() {
        let response =
            json!({"data": [{"id": "gpt-4o"}, {"id": "my-local-finetune"}, {"id": "gpt-4o"}]});
        let models = parse_models_response(&response, "openai").unwrap();
        assert_eq!(models.len(), 2);
        let known = models.iter().find(|m| m.id == "gpt-4o").unwrap();
        assert!(
            known.context_length.is_some(),
            "catalog should know gpt-4o: {known:?}"
        );
        assert_eq!(known.supports_tools, Some(true));
        let unknown = models.iter().find(|m| m.id == "my-local-finetune").unwrap();
        assert_eq!(unknown.context_length, None);
    }

    #[test]
    fn provider_is_inferred_from_the_base_url() {
        assert_eq!(
            catalog_provider_for("https://openrouter.ai/api/v1"),
            "openrouter"
        );
        assert_eq!(catalog_provider_for("http://localhost:1234/v1"), "openai");
    }

    #[test]
    fn error_payloads_surface_as_errors() {
        let response = json!({"error": {"message": "invalid key"}});
        assert!(parse_models_response(&response, "openai").is_err());
    }
}
