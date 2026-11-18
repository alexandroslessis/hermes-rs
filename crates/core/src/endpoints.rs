//! Settings ▸ Providers ▸ Custom endpoints: the OpenAI-compatible endpoints this app talks to,
//! added, tested, edited, made the default and removed from the app. They are the providers in
//! `config.json` (shared with the GPUI app); keys are in Windows Credential Manager under the
//! provider's id, where the GPUI app keeps them too.
//!
//! Hermes: `hermes_cli/web_routers/models.py` (`/api/providers/custom-endpoints*`) and
//! `hermes_cli/custom_providers.py`; the page is `app/settings/custom-endpoints-settings.tsx`.
//! hermes-rs speaks the Chat Completions API only, so another `api_mode` is refused.
//!
//! Typed inputs and outputs and [`EndpointError`]; the page's JSON (field names, the `{detail}`
//! answers and their statuses) is `crates/desktop/src/endpoints.rs`.

use std::path::Path;

use hermes_runtime::ProviderConfig;
use serde_json::{Map, Value, json};

use crate::providers::{ProviderSettings, Settings, save_default};

const KEYRING_SERVICE: &str = "hermes-rs";

/// Why an endpoint call failed. The gateway answers 400, 404 and 500 for these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    /// The request is wrong (a missing name, a bad URL, another API).
    Invalid(String),
    /// No endpoint with that id.
    NotFound(String),
    /// `config.json` or the key store could not be written.
    Failed(String),
}

impl EndpointError {
    pub fn message(&self) -> &str {
        match self {
            EndpointError::Invalid(m) | EndpointError::NotFound(m) | EndpointError::Failed(m) => m,
        }
    }
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for EndpointError {}

fn failed(message: impl Into<String>) -> EndpointError {
    EndpointError::Failed(message.into())
}

/// `config.json` as a JSON object, to edit without losing what this app doesn't know.
fn read_config(data_dir: &Path) -> Map<String, Value> {
    std::fs::read(data_dir.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_config(data_dir: &Path, config: &Map<String, Value>) -> Result<(), EndpointError> {
    let bytes = serde_json::to_vec_pretty(config).unwrap_or_default();
    std::fs::write(data_dir.join("config.json"), bytes)
        .map_err(|e| failed(format!("Could not save config.json: {e}")))
}

fn stored_key(id: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, id)
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.is_empty())
}

fn store_key(id: &str, key: Option<&str>) -> Result<(), EndpointError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, id)
        .map_err(|e| failed(format!("Credential Manager: {e}")))?;
    match key.filter(|k| !k.is_empty()) {
        Some(key) => entry
            .set_password(key)
            .map_err(|e| failed(format!("Could not save the API key: {e}"))),
        None => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(failed(format!("Could not remove the API key: {e}"))),
        },
    }
}

/// One endpoint as Settings lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// The model it starts with.
    pub model: String,
    pub models: Vec<String>,
    pub discover_models: bool,
    pub context_length: Option<usize>,
    pub daily_request_limit: Option<u32>,
    pub has_api_key: bool,
    /// The key with its middle masked, for the edit form.
    pub api_key_preview: Option<String>,
    /// It is the default for new sessions.
    pub is_current: bool,
    /// The key comes from an environment variable, not the credential store.
    pub from_environment: bool,
}

/// The default provider and model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Current {
    pub provider: String,
    pub model: String,
    pub base_url: String,
}

/// Every endpoint and which one is the default.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointList {
    pub current: Current,
    pub endpoints: Vec<EndpointInfo>,
}

fn endpoint(provider: &ProviderSettings, settings: &Settings) -> EndpointInfo {
    let key = provider.api_key();
    let current = settings.active().is_some_and(|p| p.id == provider.id);
    let model = provider
        .default_model
        .clone()
        .or_else(|| current.then(|| settings.model.clone()).flatten())
        .or_else(|| provider.models.first().cloned())
        .unwrap_or_default();
    EndpointInfo {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        model,
        models: provider.models.clone(),
        discover_models: provider.discover_models,
        context_length: provider.context_length,
        daily_request_limit: provider.daily_request_limit,
        has_api_key: key.is_some(),
        api_key_preview: key.as_deref().map(hermes_tools::redact::mask),
        is_current: current,
        from_environment: provider.api_key_env.is_some() && stored_key(&provider.id).is_none(),
    }
}

/// The endpoints and the default one.
pub fn list(data_dir: &Path) -> EndpointList {
    let settings = Settings::load(data_dir);
    let current = settings.active();
    EndpointList {
        current: Current {
            provider: current.map(|p| p.id.clone()).unwrap_or_default(),
            model: settings.model.clone().unwrap_or_default(),
            base_url: current.map(|p| p.base_url.clone()).unwrap_or_default(),
        },
        endpoints: settings
            .providers
            .iter()
            .map(|p| endpoint(p, &settings))
            .collect(),
    }
}

/// An id for a new endpoint, from its name: lowercase letters, digits and dashes, unique.
fn new_id(name: &str, taken: &[String]) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if slug.is_empty() {
        "endpoint".to_string()
    } else {
        slug
    };
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

fn refuse_other_apis(api_mode: Option<&str>) -> Result<(), EndpointError> {
    match api_mode.unwrap_or_default() {
        "" | "chat_completions" => Ok(()),
        other => Err(EndpointError::Invalid(format!(
            "hermes-rs speaks the Chat Completions API (OpenAI-compatible) only; \
             \"{other}\" isn't available. Choose Auto-detect or Chat Completions."
        ))),
    }
}

/// An endpoint to add or edit. Text is taken as typed (trimmed here).
#[derive(Debug, Clone, Default)]
pub struct EndpointInput {
    /// The endpoint to edit; empty (or one that doesn't exist) adds a new one.
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    /// The fixed model list; `None` leaves the stored one.
    pub models: Option<Vec<String>>,
    pub discover_models: bool,
    /// The window to assume; 0 or `None` is unset.
    pub context_length: Option<u64>,
    /// `None`: the page did not send it, the stored limit stays (an older page must not wipe it).
    /// `Some(None)`: cleared. `Some(Some(n))`: set.
    pub daily_request_limit: Option<Option<u32>>,
    /// Replaces the stored key; `None` (or blank) keeps it.
    pub api_key: Option<String>,
    /// Make it, and its model, the default for new sessions.
    pub make_default: bool,
    /// Anything but "" / `chat_completions` is refused.
    pub api_mode: Option<String>,
}

/// Adds an endpoint (no `id`) or edits one; returns its id.
pub fn save(data_dir: &Path, input: &EndpointInput) -> Result<String, EndpointError> {
    refuse_other_apis(input.api_mode.as_deref())?;
    let (name, base_url, model) = (
        input.name.trim().to_string(),
        input.base_url.trim().to_string(),
        input.model.trim().to_string(),
    );
    if name.is_empty() || base_url.is_empty() {
        return Err(EndpointError::Invalid(
            "An endpoint needs a name and a base URL.".into(),
        ));
    }
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(EndpointError::Invalid(
            "The base URL starts with http:// or https://.".into(),
        ));
    }
    let mut config = read_config(data_dir);
    let mut providers: Vec<Value> = config
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let taken: Vec<String> = providers
        .iter()
        .filter_map(|p| p["id"].as_str().map(str::to_string))
        .collect();
    let wanted_id = input.id.trim().to_string();
    let index = providers
        .iter()
        .position(|p| !wanted_id.is_empty() && p["id"] == wanted_id.as_str());
    let id = match index {
        Some(_) => wanted_id,
        None => new_id(&name, &taken),
    };
    let mut entry = index
        .map(|i| providers[i].clone())
        .unwrap_or_else(|| json!({}));
    entry["id"] = json!(id);
    entry["name"] = json!(name);
    entry["base_url"] = json!(base_url.trim_end_matches('/'));
    entry["discover_models"] = json!(input.discover_models);
    entry["default_model"] = if model.is_empty() {
        Value::Null
    } else {
        json!(model)
    };
    if let Some(models) = &input.models {
        entry["models"] = json!(models);
    }
    entry["context_length"] = input
        .context_length
        .filter(|n| *n > 0)
        .map_or(Value::Null, |n| json!(n));
    // Only when the page sent it: an older page that has no such field must not wipe the limit.
    if let Some(limit) = input.daily_request_limit {
        entry["daily_request_limit"] = limit.filter(|n| *n > 0).map_or(Value::Null, |n| json!(n));
    }
    if let Some(key) = input.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
        store_key(&id, Some(key.trim()))?;
    }
    match index {
        Some(i) => providers[i] = entry,
        None => providers.push(entry),
    }
    config.insert("providers".into(), json!(providers));
    write_config(data_dir, &config)?;
    if input.make_default {
        let model = if model.is_empty() {
            Settings::load(data_dir)
                .provider(&id)
                .and_then(|p| p.models.first().cloned())
                .unwrap_or_default()
        } else {
            model
        };
        save_default(data_dir, &id, &model).map_err(|e| failed(format!("{e:#}")))?;
    }
    Ok(id)
}

/// Removes the endpoint and its stored key. The default moves to the first endpoint left.
pub async fn delete(data_dir: &Path, id: &str) -> Result<(), EndpointError> {
    let was_default = Settings::load(data_dir).provider.as_deref() == Some(id);
    let mut config = read_config(data_dir);
    let mut providers: Vec<Value> = config
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let before = providers.len();
    providers.retain(|p| p["id"] != id);
    if providers.len() == before {
        return Err(EndpointError::NotFound(format!("No endpoint {id}.")));
    }
    config.insert("providers".into(), json!(providers));
    write_config(data_dir, &config)?;
    let _ = store_key(id, None);
    // The default moves to the first endpoint left, with a model it serves.
    if was_default {
        let settings = Settings::load(data_dir);
        match settings.providers.first() {
            Some(next) => {
                let model = starting_model(next, &settings).await;
                let _ = save_default(data_dir, &next.id, &model);
            }
            None => {
                let mut config = read_config(data_dir);
                config.insert("provider".into(), Value::Null);
                config.insert("model".into(), Value::Null);
                let _ = write_config(data_dir, &config);
            }
        }
    }
    Ok(())
}

/// The model an endpoint starts with as the default: its saved one, else its first configured
/// one, else the current model when it's already the default, else the first it lists.
async fn starting_model(provider: &ProviderSettings, settings: &Settings) -> String {
    if let Some(model) = provider
        .default_model
        .clone()
        .or_else(|| provider.models.first().cloned())
    {
        return model;
    }
    if settings.provider.as_deref() == Some(provider.id.as_str())
        && let Some(model) = settings.model.clone().filter(|m| !m.is_empty())
    {
        return model;
    }
    provider
        .model_ids()
        .await
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// The endpoint and its model became the default for new sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activated {
    pub provider: String,
    pub model: String,
}

/// Makes the endpoint and its starting model the default for new sessions.
pub async fn activate(data_dir: &Path, id: &str) -> Result<Activated, EndpointError> {
    let settings = Settings::load(data_dir);
    let Some(provider) = settings.provider(id) else {
        return Err(EndpointError::NotFound(format!("No endpoint {id}.")));
    };
    let model = starting_model(provider, &settings).await;
    save_default(data_dir, id, &model).map_err(|e| failed(format!("{e:#}")))?;
    Ok(Activated {
        provider: id.to_string(),
        model,
    })
}

/// What asking an endpoint for its models found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    /// It answered: the URL that worked (maybe with `/v1` added) and its models, sorted.
    Reached { url: String, models: Vec<String> },
    /// Neither the URL nor the URL with `/v1` answered; the last error.
    Unreachable { error: String },
}

/// Asks the endpoint's `/models` (with the key typed, else the stored one of endpoint `id`),
/// trying the URL with `/v1` added when the URL as given doesn't answer.
pub async fn validate(
    data_dir: &Path,
    id: &str,
    base_url: &str,
    api_key: Option<&str>,
    api_mode: Option<&str>,
) -> Result<Validation, EndpointError> {
    refuse_other_apis(api_mode)?;
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    if base_url.is_empty() {
        return Err(EndpointError::Invalid(
            "Enter the endpoint's base URL.".into(),
        ));
    }
    let api_key = api_key
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .or_else(|| {
            Settings::load(data_dir)
                .provider(id)
                .and_then(|p| p.api_key())
        });
    let mut candidates = vec![base_url.clone()];
    if !base_url.ends_with("/v1") {
        candidates.push(format!("{base_url}/v1"));
    }
    let mut last_error = String::new();
    for url in candidates {
        let probe = ProviderConfig {
            id: "probe".into(),
            base_url: url.clone(),
            api_key: api_key.clone(),
        };
        match probe.list_models().await {
            Ok(models) => {
                let mut ids: Vec<String> = models.into_iter().map(|m| m.id).collect();
                ids.sort();
                return Ok(Validation::Reached { url, models: ids });
            }
            Err(e) => last_error = format!("{e:#}"),
        }
    }
    Ok(Validation::Unreachable { error: last_error })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, url: &str, model: &str) -> EndpointInput {
        EndpointInput {
            name: name.into(),
            base_url: url.into(),
            model: model.into(),
            discover_models: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_daily_request_limit_is_saved_listed_cleared_and_not_wiped_by_an_older_page() {
        let dir = hermes_testdir::tempdir().unwrap();
        let edit = |model: &str, limit: Option<Option<u32>>| {
            save(
                dir.path(),
                &EndpointInput {
                    id: "srv".into(),
                    daily_request_limit: limit,
                    ..input("Srv", "http://localhost:1/v1", model)
                },
            )
            .unwrap();
        };
        let limit = || {
            let settings = Settings::load(dir.path());
            settings.provider("srv").unwrap().daily_request_limit
        };
        edit("m", Some(Some(800)));
        assert_eq!(limit(), Some(800));
        assert_eq!(list(dir.path()).endpoints[0].daily_request_limit, Some(800));
        // A page that has no such field (it does not send the key) leaves the limit alone.
        edit("m2", None);
        assert_eq!(limit(), Some(800));
        // Cleared (the page's null, 0 and nonsense arrive here as `Some(None)`).
        edit("m", Some(None));
        assert_eq!(limit(), None);
        // A zero is not a limit either.
        edit("m", Some(Some(800)));
        edit("m", Some(Some(0)));
        assert_eq!(limit(), None);
    }

    #[tokio::test]
    async fn add_edit_default_and_delete_keep_the_rest_of_the_config() {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"theme": "One Dark", "providers": [{"id": "ollama", "name": "Ollama", "base_url": "http://localhost:11434/v1"}], "provider": "ollama", "model": "qwen"}"#,
        )
        .unwrap();
        // A new endpoint with fixed models, made the default.
        let id = save(
            dir.path(),
            &EndpointInput {
                models: Some(vec!["m1".into(), "m2".into()]),
                discover_models: false,
                context_length: Some(32768),
                make_default: true,
                ..input("My Server", "http://localhost:8080/v1/", "m1")
            },
        )
        .unwrap();
        assert_eq!(id, "my-server");
        let settings = Settings::load(dir.path());
        assert_eq!(settings.provider.as_deref(), Some("my-server"));
        assert_eq!(settings.model.as_deref(), Some("m1"));
        let mine = settings.provider("my-server").unwrap();
        assert_eq!(mine.base_url, "http://localhost:8080/v1");
        assert_eq!(
            (mine.discover_models, mine.context_length),
            (false, Some(32768))
        );
        let config: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(config["theme"], "One Dark");

        // Editing keeps the id; another api_mode is refused; the list shows both.
        save(
            dir.path(),
            &EndpointInput {
                id: "my-server".into(),
                ..input("Renamed", "http://localhost:8080/v1", "m2")
            },
        )
        .unwrap();
        assert_eq!(
            Settings::load(dir.path())
                .provider("my-server")
                .unwrap()
                .name,
            "Renamed"
        );
        let refused = save(
            dir.path(),
            &EndpointInput {
                api_mode: Some("anthropic_messages".into()),
                ..input("x", "http://x", "")
            },
        );
        assert!(matches!(refused, Err(EndpointError::Invalid(_))));
        let listed = list(dir.path());
        assert_eq!(listed.endpoints.len(), 2);
        assert_eq!(listed.current.provider, "my-server");

        // Making Ollama the default, then deleting the other endpoint.
        let done = activate(dir.path(), "ollama").await.unwrap();
        assert_eq!(done.provider, "ollama");
        delete(dir.path(), "my-server").await.unwrap();
        assert!(matches!(
            delete(dir.path(), "my-server").await,
            Err(EndpointError::NotFound(_))
        ));
        assert_eq!(Settings::load(dir.path()).providers.len(), 1);
    }

    #[test]
    fn a_bad_endpoint_is_refused_with_its_reason() {
        let dir = hermes_testdir::tempdir().unwrap();
        for (name, url) in [("", "http://x"), ("x", ""), ("x", "ftp://x")] {
            assert!(matches!(
                save(dir.path(), &input(name, url, "")),
                Err(EndpointError::Invalid(_))
            ));
        }
        assert!(!dir.path().join("config.json").exists());
    }

    #[test]
    fn new_ids_are_slugs_and_unique() {
        let taken = vec!["openrouter".to_string()];
        assert_eq!(new_id("OpenRouter", &taken), "openrouter-2");
        assert_eq!(new_id("LM Studio (local)", &taken), "lm-studio-local");
        assert_eq!(new_id("!!!", &taken), "endpoint");
    }
}
