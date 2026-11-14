//! Settings ▸ Model ▸ Fallbacks: the providers a request moves to when the main one cannot serve
//! it (backend v2 Phase 12, `crates/runtime/src/fallback.rs`; not from Hermes, whose
//! `hermes_cli/fallback_config.py` normalises the same key but switches on many more failures).
//!
//! The chain is the config record's `fallback_providers` (`desktop-config.json`, the key Hermes's
//! page edits with its "Fallbacks" sub-page): a list of `{provider, model}`, in the order tried.
//! The page may write extra routing keys (`base_url`, `api_key`, `key_env`...) beside them; they
//! are ignored: an entry names a provider the user already configured (Settings ▸ Providers), and
//! that provider's own endpoint and key are what the request uses. A key is never read from, or
//! written into, the chain.
//!
//! Read leniently, as a hand-edited file may be wrong: an entry that is unusable is dropped with
//! a logged warning and the rest of the chain still works, never a failed turn. Dropped:
//! - not an object (a `"provider/model"` string is accepted, as Hermes's editor does) or a missing
//!   provider or model;
//! - a provider that is not configured, or is not ready (no key, not a local server);
//! - a model the provider's fixed list (`discover_models` off) does not contain;
//! - the primary itself (same provider and model: it would only repeat the failing request), and
//!   any repeat of an earlier entry;
//! - everything past [`hermes_runtime::fallback::MAX_FALLBACKS`] entries.
//!
//! A model another provider lists is not checked here (asking the network at every turn is not
//! worth it): a wrong id fails at the provider with "model not found", which moves on to the next
//! entry.

use hermes_runtime::fallback::{FallbackModel, MAX_FALLBACKS};
use serde_json::Value;

use crate::providers::{ProviderSettings, Settings};

/// A usable chain entry before it is turned into a runtime model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub provider: String,
    pub model: String,
}

/// The config record's `fallback_providers` against the configured providers: the usable entries
/// in order, and one warning line per entry dropped. `primary` is the session's `(provider,
/// model)`.
pub fn validate(
    value: &Value,
    settings: &Settings,
    primary: Option<(&str, &str)>,
) -> (Vec<Entry>, Vec<String>) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut warnings = Vec::new();
    let Some(list) = value.as_array() else {
        if !value.is_null() {
            warnings.push("fallback_providers is not a list; ignored".to_string());
        }
        return (entries, warnings);
    };
    for (position, raw) in list.iter().enumerate() {
        let n = position + 1;
        let (provider, model) = match raw {
            Value::Object(object) => (
                object.get("provider").and_then(Value::as_str),
                object.get("model").and_then(Value::as_str),
            ),
            Value::String(text) => match text.split_once('/') {
                Some((provider, model)) => (Some(provider), Some(model)),
                None => (None, None),
            },
            _ => (None, None),
        };
        let (provider, model) = (
            provider.unwrap_or_default().trim(),
            model.unwrap_or_default().trim(),
        );
        if provider.is_empty() || model.is_empty() {
            warnings.push(format!(
                "fallback entry {n} needs a provider and a model; dropped"
            ));
            continue;
        }
        let Some(configured) = settings.provider(provider) else {
            warnings.push(format!(
                "fallback entry {n}: provider \"{provider}\" is not configured; dropped"
            ));
            continue;
        };
        if !configured.ready() {
            warnings.push(format!(
                "fallback entry {n}: provider \"{provider}\" has no API key; dropped"
            ));
            continue;
        }
        if !model_possible(configured, model) {
            warnings.push(format!(
                "fallback entry {n}: provider \"{provider}\" does not list the model \"{model}\"; dropped"
            ));
            continue;
        }
        if primary == Some((provider, model)) {
            warnings.push(format!(
                "fallback entry {n} is the main model ({provider} / {model}); dropped"
            ));
            continue;
        }
        if entries
            .iter()
            .any(|entry| entry.provider == provider && entry.model == model)
        {
            warnings.push(format!(
                "fallback entry {n} repeats an earlier one ({provider} / {model}); dropped"
            ));
            continue;
        }
        if entries.len() == MAX_FALLBACKS {
            warnings.push(format!(
                "fallback entry {n} is past the limit of {MAX_FALLBACKS}; dropped"
            ));
            continue;
        }
        entries.push(Entry {
            provider: provider.to_string(),
            model: model.to_string(),
        });
    }
    (entries, warnings)
}

/// A provider whose model list is fixed (not discovered) only serves those models.
fn model_possible(provider: &ProviderSettings, model: &str) -> bool {
    if provider.discover_models {
        return true;
    }
    let listed: Vec<&String> = provider
        .models
        .iter()
        .chain(provider.default_model.as_ref())
        .collect();
    listed.is_empty() || listed.iter().any(|listed| listed.as_str() == model)
}

/// The runtime's chain for a turn: [`validate`]d entries with their providers, windows and
/// vision support (the warnings are logged).
pub fn resolve(
    value: &Value,
    settings: &Settings,
    primary: Option<(&str, &str)>,
    context: &dyn Fn(&str, &str) -> Option<usize>,
    vision: &dyn Fn(&str, &str) -> bool,
) -> Vec<FallbackModel> {
    let (entries, warnings) = validate(value, settings, primary);
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            let provider = settings.provider(&entry.provider)?.runtime();
            Some(FallbackModel {
                context_length: context(&entry.provider, &entry.model),
                supports_vision: vision(&entry.provider, &entry.model),
                provider,
                model: entry.model,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> Settings {
        // Local endpoints are "ready" without a key, so the test needs no credential store.
        serde_json::from_value(json!({
            "providers": [
                {"id": "main", "name": "Main", "base_url": "http://127.0.0.1:1/v1"},
                {"id": "backup", "name": "Backup", "base_url": "http://127.0.0.1:2/v1"},
                {"id": "third", "name": "Third", "base_url": "http://localhost:3/v1"},
                {"id": "fixed", "name": "Fixed", "base_url": "http://127.0.0.1:4/v1",
                 "discover_models": false, "models": ["small", "big"]},
                {"id": "keyless", "name": "Keyless", "base_url": "https://api.example.com/v1",
                 "api_key_env": "HERMES_RS_TEST_NO_SUCH_KEY"}
            ],
            "provider": "main",
            "model": "m"
        }))
        .unwrap()
    }

    fn ids(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| format!("{}/{}", entry.provider, entry.model))
            .collect()
    }

    #[test]
    fn a_good_chain_keeps_its_order() {
        let value = json!([
            {"provider": "backup", "model": "b1"},
            {"provider": "third", "model": "t1"},
        ]);
        let (entries, warnings) = validate(&value, &settings(), Some(("main", "m")));
        assert_eq!(ids(&entries), ["backup/b1", "third/t1"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn routing_keys_and_keys_in_an_entry_are_ignored() {
        // Hermes's editor keeps hand-written routing; here the configured provider is used.
        let value = json!([{
            "provider": "backup", "model": "b1",
            "base_url": "https://elsewhere.example", "api_key": "sk-secret", "key_env": "X"
        }]);
        let (entries, _) = validate(&value, &settings(), None);
        assert_eq!(ids(&entries), ["backup/b1"]);
        let models = resolve(&value, &settings(), None, &|_, _| Some(32_768), &|_, _| {
            false
        });
        assert_eq!(models[0].provider.base_url, "http://127.0.0.1:2/v1");
        assert_eq!(models[0].provider.api_key, None);
        assert_eq!(models[0].context_length, Some(32_768));
    }

    #[test]
    fn a_provider_slash_model_string_is_accepted() {
        let (entries, _) = validate(&json!(["backup/org/model-x"]), &settings(), None);
        assert_eq!(ids(&entries), ["backup/org/model-x"]);
    }

    #[test]
    fn unusable_entries_are_dropped_with_a_warning_each() {
        let value = json!([
            {"provider": "nobody", "model": "x"},          // unknown provider
            {"provider": "backup"},                        // no model
            {"provider": "", "model": "x"},                // no provider
            42,                                            // not an entry
            {"provider": "keyless", "model": "k"},         // no key
            {"provider": "fixed", "model": "huge"},        // not in its fixed list
            {"provider": "fixed", "model": "big"},         // fine
        ]);
        let (entries, warnings) = validate(&value, &settings(), None);
        assert_eq!(ids(&entries), ["fixed/big"]);
        assert_eq!(warnings.len(), 6, "{warnings:?}");
        assert!(warnings[0].contains("not configured"));
        assert!(warnings[4].contains("no API key"));
        assert!(warnings[5].contains("does not list"));
    }

    #[test]
    fn the_primary_and_repeats_are_dropped() {
        let value = json!([
            {"provider": "main", "model": "m"},      // the primary: a self-reference
            {"provider": "main", "model": "other"},  // same provider, another model: allowed
            {"provider": "backup", "model": "b1"},
            {"provider": "backup", "model": "b1"},   // repeat
        ]);
        let (entries, warnings) = validate(&value, &settings(), Some(("main", "m")));
        assert_eq!(ids(&entries), ["main/other", "backup/b1"]);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("main model"));
        assert!(warnings[1].contains("repeats"));
    }

    #[test]
    fn the_chain_is_capped() {
        let value = json!([
            {"provider": "backup", "model": "1"}, {"provider": "backup", "model": "2"},
            {"provider": "backup", "model": "3"}, {"provider": "backup", "model": "4"},
        ]);
        let (entries, warnings) = validate(&value, &settings(), None);
        assert_eq!(entries.len(), MAX_FALLBACKS);
        assert!(warnings[0].contains("limit"));
    }

    #[test]
    fn absent_or_odd_values_mean_no_fallback() {
        for value in [
            json!(null),
            json!([]),
            json!({}),
            json!("backup/x"),
            json!(7),
        ] {
            let (entries, _) = validate(&value, &settings(), None);
            assert!(entries.is_empty(), "{value}");
        }
        let (_, warnings) = validate(&json!({}), &settings(), None);
        assert_eq!(warnings.len(), 1);
    }
}
