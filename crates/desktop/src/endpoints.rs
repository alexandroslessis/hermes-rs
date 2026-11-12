//! Settings ▸ Providers ▸ Custom endpoints: the OpenAI-compatible endpoints this app talks to,
//! added, tested, edited, made the default and removed from the app. They are the providers in
//! `config.json` (shared with the GPUI app); keys are in Windows Credential Manager under the
//! provider's id, where the GPUI app keeps them too.
//!
//! Hermes: `hermes_cli/web_routers/models.py` (`/api/providers/custom-endpoints*`) and
//! `hermes_cli/custom_providers.py`; the page is `app/settings/custom-endpoints-settings.tsx`.
//! hermes-rs speaks the Chat Completions API only, so another `api_mode` is refused.

use std::path::Path;

use hermes_runtime::ProviderConfig;
use serde_json::{Map, Value, json};

use crate::providers::{ProviderSettings, Settings, save_default};

const KEYRING_SERVICE: &str = "hermes-rs";

type Answer = (u16, Value);

fn error(status: u16, detail: impl Into<String>) -> Answer {
    (status, json!({"detail": detail.into()}))
}

/// `config.json` as a JSON object, to edit without losing what this app doesn't know.
fn read_config(data_dir: &Path) -> Map<String, Value> {
    std::fs::read(data_dir.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_config(data_dir: &Path, config: &Map<String, Value>) -> Result<(), Answer> {
    let bytes = serde_json::to_vec_pretty(config).unwrap_or_default();
    std::fs::write(data_dir.join("config.json"), bytes)
        .map_err(|e| error(500, format!("Could not save config.json: {e}")))
}

fn stored_key(id: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, id)
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.is_empty())
}

fn store_key(id: &str, key: Option<&str>) -> Result<(), Answer> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, id)
        .map_err(|e| error(500, format!("Credential Manager: {e}")))?;
    match key.filter(|k| !k.is_empty()) {
        Some(key) => entry
            .set_password(key)
            .map_err(|e| error(500, format!("Could not save the API key: {e}"))),
        None => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(error(500, format!("Could not remove the API key: {e}"))),
        },
    }
}

/// One endpoint as the page lists it (`CustomEndpoint`).
fn endpoint(provider: &ProviderSettings, settings: &Settings) -> Value {
    let key = provider.api_key();
    let current = settings.active().is_some_and(|p| p.id == provider.id);
    let model = provider
        .default_model
        .clone()
        .or_else(|| current.then(|| settings.model.clone()).flatten())
        .or_else(|| provider.models.first().cloned())
        .unwrap_or_default();
    json!({
        "id": provider.id,
        "name": provider.name,
        "base_url": provider.base_url,
        "model": model,
        "models": provider.models,
        "discover_models": provider.discover_models,
        "context_length": provider.context_length,
        "daily_request_limit": provider.daily_request_limit,
        "api_mode": "chat_completions",
        "has_api_key": key.is_some(),
        "api_key_preview": key.as_deref().map(hermes_tools::redact::mask),
        "is_current": current,
        "source": if provider.api_key_env.is_some() && stored_key(&provider.id).is_none() {
            "environment"
        } else {
            "config"
        },
    })
}

/// `GET /api/providers/custom-endpoints` (`CustomEndpointsResponse`).
pub fn list(data_dir: &Path) -> Value {
    let settings = Settings::load(data_dir);
    let current = settings.active();
    json!({
        "current": {
            "provider": current.map(|p| p.id.clone()).unwrap_or_default(),
            "model": settings.model.clone().unwrap_or_default(),
            "base_url": current.map(|p| p.base_url.clone()).unwrap_or_default(),
        },
        "endpoints": settings.providers.iter().map(|p| endpoint(p, &settings)).collect::<Vec<_>>(),
    })
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

fn refuse_other_apis(body: &Value) -> Result<(), Answer> {
    match body["api_mode"].as_str().unwrap_or_default() {
        "" | "chat_completions" => Ok(()),
        other => Err(error(
            400,
            format!(
                "hermes-rs speaks the Chat Completions API (OpenAI-compatible) only; \
                 \"{other}\" isn't available. Choose Auto-detect or Chat Completions."
            ),
        )),
    }
}

/// `POST /api/providers/custom-endpoints` (`CustomEndpointUpdate`): adds an endpoint (no `id`)
/// or edits one; `api_key` replaces the stored key (absent keeps it); `make_default` makes it
/// and its model the default for new sessions.
pub fn save(data_dir: &Path, body: &Value) -> Answer {
    if let Err(answer) = refuse_other_apis(body) {
        return answer;
    }
    let text = |key: &str| body[key].as_str().unwrap_or_default().trim().to_string();
    let (name, base_url, model) = (text("name"), text("base_url"), text("model"));
    if name.is_empty() || base_url.is_empty() {
        return error(400, "An endpoint needs a name and a base URL.");
    }
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return error(400, "The base URL starts with http:// or https://.");
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
    let wanted_id = text("id");
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
    entry["discover_models"] = json!(body["discover_models"].as_bool().unwrap_or(true));
    entry["default_model"] = if model.is_empty() {
        Value::Null
    } else {
        json!(model)
    };
    if let Some(models) = body["models"].as_array() {
        entry["models"] = json!(models.iter().filter_map(Value::as_str).collect::<Vec<_>>());
    }
    entry["context_length"] = body["context_length"]
        .as_u64()
        .filter(|n| *n > 0)
        .map_or(Value::Null, |n| json!(n));
    // Only when the page sent it: an older page that has no such field must not wipe the limit.
    if let Some(limit) = body.get("daily_request_limit") {
        entry["daily_request_limit"] = limit
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .map_or(Value::Null, |n| json!(n));
    }
    if let Some(key) = body["api_key"].as_str().filter(|k| !k.trim().is_empty()) {
        if let Err(answer) = store_key(&id, Some(key.trim())) {
            return answer;
        }
    }
    match index {
        Some(i) => providers[i] = entry,
        None => providers.push(entry),
    }
    config.insert("providers".into(), json!(providers));
    if let Err(answer) = write_config(data_dir, &config) {
        return answer;
    }
    if body["make_default"] == true {
        let model = if model.is_empty() {
            Settings::load(data_dir)
                .provider(&id)
                .and_then(|p| p.models.first().cloned())
                .unwrap_or_default()
        } else {
            model
        };
        if let Err(e) = save_default(data_dir, &id, &model) {
            return error(500, format!("{e:#}"));
        }
    }
    let mut answer = list(data_dir);
    answer["ok"] = json!(true);
    answer["id"] = json!(id);
    (200, answer)
}

/// `DELETE /api/providers/custom-endpoints/{id}`: the endpoint and its stored key. The default
/// moves to the first endpoint left.
pub async fn delete(data_dir: &Path, id: &str) -> Answer {
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
        return error(404, format!("No endpoint {id}."));
    }
    config.insert("providers".into(), json!(providers));
    if let Err(answer) = write_config(data_dir, &config) {
        return answer;
    }
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
    (200, list(data_dir))
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

/// `POST /api/providers/custom-endpoints/{id}/activate`: the endpoint and its model become the
/// default for new sessions.
pub async fn activate(data_dir: &Path, id: &str) -> Answer {
    let settings = Settings::load(data_dir);
    let Some(provider) = settings.provider(id) else {
        return error(404, format!("No endpoint {id}."));
    };
    let model = starting_model(provider, &settings).await;
    match save_default(data_dir, id, &model) {
        Ok(()) => (200, json!({"ok": true, "provider": id, "model": model})),
        Err(e) => error(500, format!("{e:#}")),
    }
}

/// `POST /api/providers/custom-endpoints/validate`: asks the endpoint's `/models` (with the
/// key typed, else the stored one), trying the URL with `/v1` added when the URL as given
/// doesn't answer. `CustomEndpointValidationResponse`.
pub async fn validate(data_dir: &Path, body: &Value) -> Answer {
    if let Err(answer) = refuse_other_apis(body) {
        return answer;
    }
    let base_url = body["base_url"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return error(400, "Enter the endpoint's base URL.");
    }
    let id = body["id"].as_str().unwrap_or_default();
    let api_key = body["api_key"]
        .as_str()
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
                return (
                    200,
                    json!({
                        "ok": true,
                        "reachable": true,
                        "message": format!("Reached {url}: {} models.", ids.len()),
                        "models": ids,
                        "resolved_base_url": url,
                        "transport_checked": "chat_completions",
                    }),
                );
            }
            Err(e) => last_error = format!("{e:#}"),
        }
    }
    (
        200,
        json!({
            "ok": false,
            "reachable": false,
            "message": format!("Couldn't list the endpoint's models: {last_error}"),
            "models": [],
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daily_request_limit_is_saved_listed_cleared_and_not_wiped_by_an_older_page() {
        let dir = tempfile::tempdir().unwrap();
        let save_body = |body: Value| {
            let (status, answer) = save(dir.path(), &body);
            assert_eq!(status, 200, "{answer}");
        };
        let limit = || {
            let settings = Settings::load(dir.path());
            settings.provider("srv").unwrap().daily_request_limit
        };
        save_body(json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                         "daily_request_limit": 800}));
        assert_eq!(limit(), Some(800));
        assert_eq!(list(dir.path())["endpoints"][0]["daily_request_limit"], 800);
        // A page that has no such field (it does not send the key) leaves the limit alone.
        save_body(json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m2"}));
        assert_eq!(limit(), Some(800));
        // null, 0 and nonsense clear it.
        for cleared in [json!(null), json!(0), json!("many"), json!(-5)] {
            save_body(json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                             "daily_request_limit": 800}));
            save_body(json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                             "daily_request_limit": cleared}));
            assert_eq!(limit(), None, "{cleared}");
        }
    }

    #[tokio::test]
    async fn add_edit_default_and_delete_keep_the_rest_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"theme": "One Dark", "providers": [{"id": "ollama", "name": "Ollama", "base_url": "http://localhost:11434/v1"}], "provider": "ollama", "model": "qwen"}"#,
        )
        .unwrap();
        // A new endpoint with fixed models, made the default.
        let (status, body) = save(
            dir.path(),
            &json!({"name": "My Server", "base_url": "http://localhost:8080/v1/", "model": "m1",
                    "models": ["m1", "m2"], "discover_models": false, "context_length": 32768,
                    "make_default": true}),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["id"], "my-server");
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
        let (status, _) = save(
            dir.path(),
            &json!({"id": "my-server", "name": "Renamed", "base_url": "http://localhost:8080/v1", "model": "m2"}),
        );
        assert_eq!(status, 200);
        assert_eq!(
            Settings::load(dir.path())
                .provider("my-server")
                .unwrap()
                .name,
            "Renamed"
        );
        assert_eq!(
            save(
                dir.path(),
                &json!({"name": "x", "base_url": "http://x", "api_mode": "anthropic_messages"})
            )
            .0,
            400
        );
        let listed = list(dir.path());
        assert_eq!(listed["endpoints"].as_array().unwrap().len(), 2);
        assert_eq!(listed["current"]["provider"], "my-server");

        // Making Ollama the default, then deleting the other endpoint.
        let (status, body) = activate(dir.path(), "ollama").await;
        assert_eq!((status, body["provider"].clone()), (200, json!("ollama")));
        assert_eq!(delete(dir.path(), "my-server").await.0, 200);
        assert_eq!(delete(dir.path(), "my-server").await.0, 404);
        assert_eq!(Settings::load(dir.path()).providers.len(), 1);
    }

    #[test]
    fn new_ids_are_slugs_and_unique() {
        let taken = vec!["openrouter".to_string()];
        assert_eq!(new_id("OpenRouter", &taken), "openrouter-2");
        assert_eq!(new_id("LM Studio (local)", &taken), "lm-studio-local");
        assert_eq!(new_id("!!!", &taken), "endpoint");
    }
}
