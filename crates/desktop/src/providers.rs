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
}

/// A model on a particular provider (the GPUI app's `ModelChoice`).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
}

/// The side-job models (Settings ▸ Auxiliary models in the GPUI app).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuxModels {
    #[serde(default)]
    pub titles: Option<ModelChoice>,
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

    pub fn runtime(&self) -> ProviderConfig {
        ProviderConfig {
            id: self.id.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key(),
        }
    }
}
