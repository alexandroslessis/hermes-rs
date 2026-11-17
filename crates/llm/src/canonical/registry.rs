use super::CanonicalModel;
use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};

/// The catalog compiled into the binary (`data/canonical_models.json`, zstd-compressed by
/// `build.rs`), parsed once on first use.
static BUNDLED: Lazy<Result<CanonicalModelRegistry>> = Lazy::new(|| {
    zstd::decode_all(
        include_bytes!(concat!(env!("OUT_DIR"), "/canonical_models.json.zst")).as_slice(),
    )
    .context("Failed to decompress bundled canonical models JSON")
    .and_then(|json| CanonicalModelRegistry::from_json(std::str::from_utf8(&json)?))
});

#[derive(Debug, Clone)]
pub struct CanonicalModelRegistry {
    models: HashMap<(String, String), CanonicalModel>,
}

impl CanonicalModelRegistry {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    pub fn bundled() -> Result<&'static CanonicalModelRegistry> {
        BUNDLED.as_ref().map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub fn from_json(content: &str) -> Result<Self> {
        let models: Vec<CanonicalModel> =
            serde_json::from_str(content).context("Failed to parse canonical models JSON")?;
        if models.is_empty() {
            bail!("canonical model catalog is empty");
        }

        let mut seen = HashSet::new();
        let mut registry = Self::new();
        for model in models {
            if !seen.insert(model.id.clone()) {
                bail!("duplicate canonical model id: {}", model.id);
            }
            let (provider, model_name) = model
                .id
                .split_once('/')
                .with_context(|| format!("invalid canonical model id: {}", model.id))?;
            registry.register(provider, model_name, model.clone());
        }
        Ok(registry)
    }

    pub fn register(&mut self, provider: &str, model: &str, canonical_model: CanonicalModel) {
        self.models
            .insert((provider.to_string(), model.to_string()), canonical_model);
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<&CanonicalModel> {
        self.models.get(&(provider.to_string(), model.to_string()))
    }

    pub fn get_all_models_for_provider(&self, provider: &str) -> Vec<CanonicalModel> {
        self.models
            .iter()
            .filter(|((p, _), _)| p == provider)
            .map(|(_, model)| model.clone())
            .collect()
    }

    pub fn all_models(&self) -> Vec<&CanonicalModel> {
        self.models.values().collect()
    }

    pub fn count(&self) -> usize {
        self.models.len()
    }
}

impl Default for CanonicalModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_duplicate_and_malformed_catalogs() {
        assert!(CanonicalModelRegistry::from_json("[]").is_err());
        let model = r#"{"id":"openai/test","name":"Test","tool_call":true}"#;
        assert!(CanonicalModelRegistry::from_json(&format!("[{model},{model}]")).is_err());
        assert!(CanonicalModelRegistry::from_json(
            r#"[{"id":"invalid","name":"Test","tool_call":true}]"#
        )
        .is_err());
    }
}
