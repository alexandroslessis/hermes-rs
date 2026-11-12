//! The model providers and default model from the hermes-rs settings (`config.json` in the data
//! folder, shared with the GPUI app); keys from Windows Credential Manager or an environment
//! variable, as the GPUI app stores them.

use std::path::Path;

use hermes_runtime::ProviderConfig;
use serde::Deserialize;

const KEYRING_SERVICE: &str = "hermes-rs";

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSettings {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    // Settings ▸ Providers ▸ Custom endpoints (`endpoints.rs`); the GPUI app ignores them.
    /// The model the endpoint starts with when made the default.
    #[serde(default)]
    pub default_model: Option<String>,
    /// The endpoint's models when it doesn't list them (`discover_models` off).
    #[serde(default)]
    pub models: Vec<String>,
    /// Ask the endpoint's `/models` for its models (else `models` is the list).
    #[serde(default = "discover_by_default")]
    pub discover_models: bool,
    /// The context window to assume for the endpoint's models, when it doesn't say.
    #[serde(default)]
    pub context_length: Option<usize>,
    /// How many requests per day the provider allows (Settings ▸ Providers); the request ledger
    /// and its governor use it. `None` = not set (a limit the provider's 429 states is learned).
    /// Read leniently: a hand-edited value that isn't a positive whole number counts as unset,
    /// rather than failing the whole file (which would lose every provider).
    #[serde(default, deserialize_with = "positive_u32_or_none")]
    pub daily_request_limit: Option<u32>,
}

fn discover_by_default() -> bool {
    true
}

fn positive_u32_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0))
}

/// A model on a particular provider (the GPUI app's `ModelChoice`).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    /// The job's own reasoning effort (Settings ▸ Model ▸ Auxiliary); ours, the GPUI app has none.
    #[serde(default)]
    pub effort: Option<String>,
}

/// The side-job models (Settings ▸ Model ▸ Auxiliary, `aux_models.rs`; the GPUI app's names).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuxModels {
    #[serde(default)]
    pub compression: Option<ModelChoice>,
    #[serde(default)]
    pub review: Option<ModelChoice>,
    #[serde(default)]
    pub approval: Option<ModelChoice>,
    #[serde(default)]
    pub titles: Option<ModelChoice>,
}

impl AuxModels {
    /// A job's choice by its `config.json` key.
    pub fn get(&self, key: &str) -> Option<&ModelChoice> {
        match key {
            "compression" => self.compression.as_ref(),
            "review" => self.review.as_ref(),
            "approval" => self.approval.as_ref(),
            "titles" => self.titles.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub providers: Vec<ProviderSettings>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub aux_models: AuxModels,
}

impl Settings {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderSettings> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// The provider new turns use: the chosen one, else the first.
    pub fn active(&self) -> Option<&ProviderSettings> {
        self.provider
            .as_deref()
            .and_then(|id| self.provider(id))
            .or(self.providers.first())
    }
}

/// Makes `model` on `provider` the default for new sessions, keeping the rest of `config.json`
/// (the GPUI app's settings) as it is.
pub fn save_default(data_dir: &Path, provider: &str, model: &str) -> anyhow::Result<()> {
    let path = data_dir.join("config.json");
    let mut config: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let object = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config.json isn't an object"))?;
    // The endpoint that stops being the default keeps its model (else it lists none, and its
    // edit form opens without one).
    let previous = object
        .get("provider")
        .and_then(|p| p.as_str())
        .map(str::to_string);
    let previous_model = object
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    if let (Some(previous), Some(previous_model)) = (previous, previous_model)
        && previous != provider
        && !previous_model.is_empty()
        && let Some(entries) = object.get_mut("providers").and_then(|p| p.as_array_mut())
        && let Some(entry) = entries.iter_mut().find(|e| e["id"] == previous.as_str())
        && entry["default_model"].as_str().is_none_or(str::is_empty)
    {
        entry["default_model"] = previous_model.into();
    }
    object.insert("provider".into(), provider.into());
    object.insert("model".into(), model.into());
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    Ok(())
}

impl ProviderSettings {
    pub fn is_local(&self) -> bool {
        let rest = self
            .base_url
            .split_once("://")
            .map_or(self.base_url.as_str(), |(_, rest)| rest);
        let host = rest.split('/').next().unwrap_or_default();
        let host = host.split(':').next().unwrap_or_default();
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1"
    }

    pub fn api_key(&self) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, &self.id)
            .ok()
            .and_then(|entry| entry.get_password().ok())
            .or_else(|| {
                self.api_key_env
                    .as_deref()
                    .and_then(|name| std::env::var(name).ok())
                    .filter(|v| !v.is_empty())
            })
    }

    /// Callable: a key, or a server on this machine that needs none.
    pub fn ready(&self) -> bool {
        self.is_local() || self.api_key().is_some()
    }

    /// The model ids the picker offers: the endpoint's own list (asked when `discover_models`),
    /// else the configured ones; both when it lists some and others are configured.
    pub async fn model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = if self.discover_models && self.ready() {
            self.runtime()
                .list_models()
                .await
                .map(|models| models.into_iter().map(|m| m.id).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for model in self.models.iter().chain(self.default_model.as_ref()) {
            if !model.is_empty() && !ids.contains(model) {
                ids.push(model.clone());
            }
        }
        ids
    }

    pub fn runtime(&self) -> ProviderConfig {
        ProviderConfig {
            id: self.id.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key(),
        }
    }
}
