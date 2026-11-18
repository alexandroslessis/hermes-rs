//! Settings ▸ Providers ▸ Custom endpoints as Hermes's routes answer them
//! (`/api/providers/custom-endpoints*`): the page's JSON and its `{detail}` errors. The logic
//! (config.json, the key store, probing an endpoint) is `hermes_core::endpoints`.
//!
//! Hermes: `hermes_cli/web_routers/models.py` (`/api/providers/custom-endpoints*`) and
//! `hermes_cli/custom_providers.py`; the page is `app/settings/custom-endpoints-settings.tsx`.

use std::path::Path;

use hermes_core::endpoints::{self, EndpointError, EndpointInput, Validation};
use serde_json::{Value, json};

type Answer = (u16, Value);

/// An error as the page reads it: 400, 404 or 500 and `{detail}`.
fn failure(error: EndpointError) -> Answer {
    let status = match error {
        EndpointError::Invalid(_) => 400,
        EndpointError::NotFound(_) => 404,
        EndpointError::Failed(_) => 500,
    };
    (status, json!({"detail": error.message()}))
}

/// `GET /api/providers/custom-endpoints` (`CustomEndpointsResponse`).
pub fn list(data_dir: &Path) -> Value {
    let listed = endpoints::list(data_dir);
    json!({
        "current": {
            "provider": listed.current.provider,
            "model": listed.current.model,
            "base_url": listed.current.base_url,
        },
        "endpoints": listed.endpoints.iter().map(|e| json!({
            "id": e.id,
            "name": e.name,
            "base_url": e.base_url,
            "model": e.model,
            "models": e.models,
            "discover_models": e.discover_models,
            "context_length": e.context_length,
            "daily_request_limit": e.daily_request_limit,
            "api_mode": "chat_completions",
            "has_api_key": e.has_api_key,
            "api_key_preview": e.api_key_preview,
            "is_current": e.is_current,
            "source": if e.from_environment { "environment" } else { "config" },
        })).collect::<Vec<_>>(),
    })
}

fn text(body: &Value, key: &str) -> String {
    body[key].as_str().unwrap_or_default().to_string()
}

/// The page's body as a typed endpoint.
fn input(body: &Value) -> EndpointInput {
    EndpointInput {
        id: text(body, "id"),
        name: text(body, "name"),
        base_url: text(body, "base_url"),
        model: text(body, "model"),
        models: body["models"].as_array().map(|models| {
            models
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        }),
        discover_models: body["discover_models"].as_bool().unwrap_or(true),
        context_length: body["context_length"].as_u64(),
        // Only when the page sent it; null, 0 and nonsense clear it.
        daily_request_limit: body.get("daily_request_limit").map(|limit| {
            limit
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
        }),
        api_key: body["api_key"].as_str().map(str::to_string),
        make_default: body["make_default"] == true,
        api_mode: body["api_mode"].as_str().map(str::to_string),
    }
}

/// `POST /api/providers/custom-endpoints` (`CustomEndpointUpdate`): adds an endpoint (no `id`)
/// or edits one; `api_key` replaces the stored key (absent keeps it); `make_default` makes it
/// and its model the default for new sessions.
pub fn save(data_dir: &Path, body: &Value) -> Answer {
    match endpoints::save(data_dir, &input(body)) {
        Ok(id) => {
            let mut answer = list(data_dir);
            answer["ok"] = json!(true);
            answer["id"] = json!(id);
            (200, answer)
        }
        Err(error) => failure(error),
    }
}

/// `DELETE /api/providers/custom-endpoints/{id}`: the endpoint and its stored key. The default
/// moves to the first endpoint left.
pub async fn delete(data_dir: &Path, id: &str) -> Answer {
    match endpoints::delete(data_dir, id).await {
        Ok(()) => (200, list(data_dir)),
        Err(error) => failure(error),
    }
}

/// `POST /api/providers/custom-endpoints/{id}/activate`: the endpoint and its model become the
/// default for new sessions.
pub async fn activate(data_dir: &Path, id: &str) -> Answer {
    match endpoints::activate(data_dir, id).await {
        Ok(done) => (
            200,
            json!({"ok": true, "provider": done.provider, "model": done.model}),
        ),
        Err(error) => failure(error),
    }
}

/// `POST /api/providers/custom-endpoints/validate`: asks the endpoint's `/models` (with the
/// key typed, else the stored one), trying the URL with `/v1` added when the URL as given
/// doesn't answer. `CustomEndpointValidationResponse`.
pub async fn validate(data_dir: &Path, body: &Value) -> Answer {
    let checked = endpoints::validate(
        data_dir,
        body["id"].as_str().unwrap_or_default(),
        body["base_url"].as_str().unwrap_or_default(),
        body["api_key"].as_str(),
        body["api_mode"].as_str(),
    )
    .await;
    match checked {
        Ok(Validation::Reached { url, models }) => (
            200,
            json!({
                "ok": true,
                "reachable": true,
                "message": format!("Reached {url}: {} models.", models.len()),
                "models": models,
                "resolved_base_url": url,
                "transport_checked": "chat_completions",
            }),
        ),
        Ok(Validation::Unreachable { error }) => (
            200,
            json!({
                "ok": false,
                "reachable": false,
                "message": format!("Couldn't list the endpoint's models: {error}"),
                "models": [],
            }),
        ),
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::providers::Settings;

    #[test]
    fn the_daily_request_limit_reaches_the_list_and_an_older_page_does_not_wipe_it() {
        let dir = hermes_testdir::tempdir().unwrap();
        let save_body = |body: Value| {
            let (status, answer) = save(dir.path(), &body);
            assert_eq!(status, 200, "{answer}");
        };
        let limit = || {
            let settings = Settings::load(dir.path());
            settings.provider("srv").unwrap().daily_request_limit
        };
        save_body(
            json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                         "daily_request_limit": 800}),
        );
        assert_eq!(limit(), Some(800));
        assert_eq!(list(dir.path())["endpoints"][0]["daily_request_limit"], 800);
        // A page that has no such field (it does not send the key) leaves the limit alone.
        save_body(
            json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m2"}),
        );
        assert_eq!(limit(), Some(800));
        // null, 0 and nonsense clear it.
        for cleared in [json!(null), json!(0), json!("many"), json!(-5)] {
            save_body(
                json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                             "daily_request_limit": 800}),
            );
            save_body(
                json!({"id": "srv", "name": "Srv", "base_url": "http://localhost:1/v1", "model": "m",
                             "daily_request_limit": cleared}),
            );
            assert_eq!(limit(), None, "{cleared}");
        }
    }

    #[tokio::test]
    async fn the_routes_answer_with_hermes_shapes_and_statuses() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (status, body) = save(
            dir.path(),
            &json!({"name": "My Server", "base_url": "http://localhost:8080/v1/", "model": "m1",
                    "models": ["m1", "m2"], "discover_models": false, "context_length": 32768,
                    "make_default": true}),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            (body["ok"].clone(), body["id"].clone()),
            (json!(true), json!("my-server"))
        );
        assert_eq!(body["current"]["provider"], "my-server");
        let mine = &body["endpoints"][0];
        assert_eq!(mine["api_mode"], "chat_completions");
        assert_eq!(mine["base_url"], "http://localhost:8080/v1");
        assert_eq!(mine["source"], "config");
        assert_eq!(mine["is_current"], true);
        assert_eq!(mine["context_length"], 32768);
        // Another API, a missing name, an unknown id.
        let (status, body) = save(
            dir.path(),
            &json!({"name": "x", "base_url": "http://x", "api_mode": "anthropic_messages"}),
        );
        assert_eq!(status, 400, "{body}");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("Chat Completions")
        );
        assert_eq!(save(dir.path(), &json!({"name": "x"})).0, 400);
        let (status, body) = activate(dir.path(), "nope").await;
        assert_eq!(
            (status, body["detail"].clone()),
            (404, json!("No endpoint nope."))
        );
        let (status, body) = activate(dir.path(), "my-server").await;
        assert_eq!(
            (status, body["provider"].clone()),
            (200, json!("my-server"))
        );
        assert_eq!(delete(dir.path(), "my-server").await.0, 200);
        assert_eq!(delete(dir.path(), "my-server").await.0, 404);
        // No base URL to probe is a 400, not an unreachable answer.
        assert_eq!(validate(dir.path(), &json!({"base_url": " "})).await.0, 400);
    }
}
