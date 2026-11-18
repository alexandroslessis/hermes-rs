//! Settings ▸ Model ▸ Auxiliary as Hermes's routes answer it: `GET /api/model/auxiliary`
//! (`AuxiliaryModelsResponse`), `POST /api/model/set` with `scope: "auxiliary"`
//! (`ModelAssignmentRequest`), and ours, `GET/POST /api/model/auxiliary/local`
//! (diverged, backend v2 Phase 4: the page reaches them only through patch 0023). The logic
//! (config.json, the jobs, probing local servers) is `hermes_core::aux_models`.
//!
//! Hermes: `hermes_cli/web_routers/models.py` (`get_auxiliary_models`),
//! `hermes_cli/web_server_config.py` (`_apply_aux_assignment_sync`, `_AUX_TASK_SLOTS`).

use std::path::Path;

use hermes_core::aux_models::{self, Assigned, Assignment, AuxError, EffortChange};
use serde_json::{Value, json};

pub use hermes_core::aux_models::{WELL_KNOWN_LOCAL, runtime_model, runtime_models};

/// An error as the page reads it: 400 or 500 and `{detail}`.
fn failure(error: AuxError) -> (u16, Value) {
    let status = match error {
        AuxError::Invalid(_) => 400,
        AuxError::Failed(_) => 500,
    };
    (status, json!({"detail": error.message()}))
}

/// `GET /api/model/auxiliary` (`AuxiliaryModelsResponse`): the main model and each job's route.
pub fn list(data_dir: &Path) -> Value {
    let listed = aux_models::list(data_dir);
    let tasks: Vec<Value> = listed
        .tasks
        .iter()
        .map(|task| {
            let mut row = json!({
                "task": task.slot,
                "provider": task.provider,
                "model": task.model,
                "base_url": task.base_url,
                "reasoning_effort": task.effort,
                "local_endpoint": task.local_endpoint,
            });
            if let Some((label, hint)) = task.label {
                row["label"] = json!(label);
                row["hint"] = json!(hint);
            }
            row
        })
        .collect();
    json!({
        "main": {"provider": listed.main_provider, "model": listed.main_model},
        "tasks": tasks,
    })
}

/// The `reasoning_effort` key: absent keeps the job's effort, `null` clears it, a level sets it.
fn effort_of(body: &Value) -> EffortChange {
    match body.get("reasoning_effort") {
        None => EffortChange::Keep,
        Some(Value::Null) => EffortChange::Clear,
        Some(Value::String(level)) => EffortChange::Level(level.clone()),
        Some(other) => EffortChange::Other(other.to_string()),
    }
}

/// The answer to an assignment that went through.
fn assigned(done: &Assigned) -> Value {
    match done {
        Assigned::Reset => json!({"ok": true, "scope": "auxiliary", "reset": true}),
        Assigned::Tasks {
            slots,
            provider,
            model,
        } => json!({
            "ok": true,
            "scope": "auxiliary",
            "tasks": slots,
            "provider": provider,
            "model": model,
        }),
    }
}

/// `POST /api/model/set` with `scope: "auxiliary"` (`ModelAssignmentRequest`): a job's model,
/// every job's (`task` empty), or all back to the main model (`task: "__reset__"`). `provider`
/// `"auto"` makes a job follow the session's model again. `reasoning_effort` absent keeps the
/// job's effort, `null` clears it, a level sets it.
pub fn assign(data_dir: &Path, body: &Value) -> (u16, Value) {
    let request = Assignment {
        task: body["task"].as_str().unwrap_or_default().to_string(),
        provider: body["provider"].as_str().unwrap_or_default().to_string(),
        model: body["model"].as_str().unwrap_or_default().to_string(),
        effort: effort_of(body),
    };
    match aux_models::assign(data_dir, &request) {
        Ok(done) => (200, assigned(&done)),
        Err(error) => failure(error),
    }
}

/// `GET /api/model/auxiliary/local` (ours, not Hermes's): the local servers that answer and their
/// models, so the page can offer "Use a local model for background jobs".
pub async fn local_list(data_dir: &Path, well_known: &[(&str, &str, &str)]) -> (u16, Value) {
    let servers = aux_models::local_servers(data_dir, well_known).await;
    (
        200,
        json!({"servers": servers.iter().map(|s| json!({
            "provider": s.id, "name": s.name, "base_url": s.base_url,
            "models": s.models, "configured": s.configured,
        })).collect::<Vec<_>>()}),
    )
}

/// `POST /api/model/auxiliary/local` (ours): sets every side job to a local model (see
/// `hermes_core::aux_models::use_local`). 400 when no local server answers: nothing is changed.
pub async fn use_local(
    data_dir: &Path,
    body: &Value,
    well_known: &[(&str, &str, &str)],
) -> (u16, Value) {
    let used = aux_models::use_local(
        data_dir,
        body["provider"].as_str(),
        body["model"].as_str(),
        well_known,
    )
    .await;
    match used {
        Ok(used) => {
            let mut answer = assigned(&used.assigned);
            answer["base_url"] = json!(used.base_url);
            answer["added_provider"] = json!(used.added_provider);
            (200, answer)
        }
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> hermes_testdir::TestDir {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"providers":[{"id":"local","name":"Local","base_url":"http://127.0.0.1:11434/v1"},
                             {"id":"remote","name":"Remote","base_url":"https://example.com/v1"}],
                "provider":"local","model":"big","window":{"w":1},
                "aux_models":{"curator":{"provider":"remote","model":"c"}}}"#,
        )
        .unwrap();
        dir
    }

    fn row(list: &Value, slot: &str) -> Value {
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["task"] == slot)
            .unwrap()
            .clone()
    }

    #[test]
    fn the_list_has_hermes_s_rows_and_a_picked_model_shows_in_it() {
        let dir = home();
        let listed = list(dir.path());
        assert_eq!(listed["main"], json!({"provider": "local", "model": "big"}));
        let slots: Vec<&str> = listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["task"].as_str().unwrap())
            .collect();
        assert_eq!(
            slots,
            [
                "compression",
                "approval",
                "title_generation",
                "background_review"
            ]
        );
        assert_eq!(row(&listed, "compression")["provider"], "auto");
        assert_eq!(row(&listed, "compression").get("label"), None);
        assert_eq!(
            row(&listed, "background_review")["label"],
            "Background review"
        );
        assert!(row(&listed, "background_review")["hint"].is_string());

        let body = json!({"scope": "auxiliary", "task": "compression", "provider": "remote",
                          "model": "small", "reasoning_effort": "low"});
        let (status, answer) = assign(dir.path(), &body);
        assert_eq!(status, 200);
        assert_eq!(
            answer,
            json!({"ok": true, "scope": "auxiliary", "tasks": ["compression"],
                   "provider": "remote", "model": "small"})
        );
        let compression = row(&list(dir.path()), "compression");
        assert_eq!(compression["reasoning_effort"], "low");
        assert_eq!(compression["base_url"], "https://example.com/v1");
        assert_eq!(compression["local_endpoint"], false);
        // null clears the effort; a missing key (above, "keep") is not null.
        assign(
            dir.path(),
            &json!({"task": "compression", "provider": "local", "model": "tiny", "reasoning_effort": null}),
        );
        assert_eq!(
            row(&list(dir.path()), "compression")["reasoning_effort"],
            Value::Null
        );
    }

    #[test]
    fn reset_and_bad_requests_answer_as_hermes_does() {
        let dir = home();
        let (status, body) = assign(dir.path(), &json!({"task": "__reset__"}));
        assert_eq!(
            (status, body),
            (
                200,
                json!({"ok": true, "scope": "auxiliary", "reset": true})
            )
        );
        let refused = |body: Value| assign(dir.path(), &body);
        let (status, body) = refused(json!({"task": "vision", "provider": "local", "model": "m"}));
        assert_eq!(
            (status, body["detail"].clone()),
            (400, json!("unknown auxiliary task: vision"))
        );
        assert_eq!(
            refused(json!({"task": "compression", "provider": "nowhere", "model": "m"})).0,
            400
        );
        assert_eq!(
            refused(json!({"task": "compression", "provider": "local", "model": ""})).0,
            400
        );
        let (status, body) = refused(
            json!({"task": "compression", "provider": "local", "model": "m", "reasoning_effort": "max"}),
        );
        assert_eq!(status, 400);
        assert!(body["detail"].as_str().unwrap().ends_with("(got \"max\")"));
        let (_, body) = refused(
            json!({"task": "compression", "provider": "local", "model": "m", "reasoning_effort": 7}),
        );
        assert!(body["detail"].as_str().unwrap().ends_with("(got 7)"));
    }

    #[tokio::test]
    async fn the_local_routes_answer_with_the_page_s_fields() {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            json!({"providers": []}).to_string(),
        )
        .unwrap();
        // Nothing listens: the list is empty and the click is a 400 that changes nothing.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/v1");
        let known = [("ollama", "Ollama", url.as_str())];
        assert_eq!(
            local_list(dir.path(), &known).await,
            (200, json!({"servers": []}))
        );
        let (status, body) = use_local(dir.path(), &json!({}), &known).await;
        assert_eq!(status, 400);
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("No local model server")
        );
    }
}
