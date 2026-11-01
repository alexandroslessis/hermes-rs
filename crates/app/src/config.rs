//! App settings in `%APPDATA%\hermes-rs\config.json`; API keys in Windows Credential Manager.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use hermes_runtime::ProviderConfig;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "hermes-rs";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// An environment variable to read the key from when Credential Manager has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl ProviderSettings {
    /// A server on this machine (Ollama, LM Studio, llama.cpp): it works without an API key.
    pub fn is_local(&self) -> bool {
        let rest = self
            .base_url
            .split_once("://")
            .map_or(self.base_url.as_str(), |(_, rest)| rest);
        let authority = rest.split('/').next().unwrap_or_default();
        let host = match authority.strip_prefix('[') {
            Some(ipv6) => ipv6.split(']').next().unwrap_or_default(),
            None => authority.split(':').next().unwrap_or_default(),
        };
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub providers: Vec<ProviderSettings>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Working directory for new sessions.
    #[serde(default)]
    pub last_cwd: Option<PathBuf>,
    #[serde(default)]
    pub theme: Option<String>,
    /// Reasoning effort per model (Hermes keeps a preset per model the same way).
    #[serde(default)]
    pub efforts: std::collections::HashMap<String, String>,
    /// Stage the agent's memory writes for approval (Hermes's `memory.write_approval`).
    #[serde(default)]
    pub memory_write_approval: bool,
    /// Stage the agent's skill writes for approval (Hermes's `skills.write_approval`).
    #[serde(default)]
    pub skill_write_approval: bool,
    /// Review sessions in the background to save memories and improve skills (Hermes's
    /// `background_review.enabled`, on by default).
    #[serde(default = "default_true")]
    pub background_review: bool,
    /// Let the curator merge overlapping agent-written skills with the model (Hermes's
    /// `curator.consolidate`, off by default; the idle-skill archiving always runs).
    #[serde(default)]
    pub curator_consolidate: bool,
    /// The sidebar group new sessions start in: a project id, or `auto:` and a folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_project: Option<String>,
    /// The session sidebar's width, set by dragging its edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    /// Models starred in the model selector (Zed's favorite models).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorite_models: Vec<FavoriteModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteModel {
    pub provider: String,
    pub model: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: vec![ProviderSettings {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: Some("hermes_rs_or_api_key".into()),
            }],
            provider: Some("openrouter".into()),
            model: None,
            last_cwd: None,
            theme: None,
            efforts: Default::default(),
            memory_write_approval: false,
            skill_write_approval: false,
            favorite_models: Vec::new(),
            selected_project: None,
            background_review: true,
            curator_consolidate: false,
            sidebar_width: None,
        }
    }
}

impl AppConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        // Editors like Notepad (and PowerShell) save UTF-8 with a byte-order mark.
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(config) => config,
            Err(error) => {
                tracing::error!("ignoring unreadable {}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn active_provider(&self) -> Option<&ProviderSettings> {
        let id = self.provider.as_deref()?;
        self.providers.iter().find(|provider| provider.id == id)
    }
}

fn entry(provider_id: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYRING_SERVICE, provider_id)?)
}

pub fn stored_api_key(provider_id: &str) -> Option<String> {
    entry(provider_id).ok()?.get_password().ok()
}

pub fn store_api_key(provider_id: &str, key: &str) -> Result<()> {
    if key.is_empty() {
        match entry(provider_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    } else {
        entry(provider_id)?
            .set_password(key)
            .context("saving the API key to Windows Credential Manager")
    }
}

/// Reads a user environment variable from the registry too, so a variable set after the app
/// started still works.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            windows_registry::CURRENT_USER
                .open("Environment")
                .ok()?
                .get_string(name)
                .ok()
                .filter(|v| !v.is_empty())
        })
}

pub fn api_key(provider: &ProviderSettings) -> Option<String> {
    stored_api_key(&provider.id).or_else(|| provider.api_key_env.as_deref().and_then(env_var))
}

pub fn runtime_provider(provider: &ProviderSettings) -> ProviderConfig {
    ProviderConfig {
        id: provider.id.clone(),
        base_url: provider.base_url.clone(),
        api_key: api_key(provider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_servers_need_no_key() {
        let provider = |base_url: &str| ProviderSettings {
            id: "p".into(),
            name: "P".into(),
            base_url: base_url.into(),
            api_key_env: None,
        };
        assert!(provider("http://localhost:11434/v1").is_local());
        assert!(provider("http://127.0.0.1:1234/v1").is_local());
        assert!(provider("http://[::1]:8080/v1").is_local());
        assert!(!provider("https://openrouter.ai/api/v1").is_local());
        assert!(!provider("http://localhost.example.com/v1").is_local());
    }
}

fn default_true() -> bool {
    true
}
