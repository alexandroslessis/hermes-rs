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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub providers: Vec<ProviderSettings>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
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
