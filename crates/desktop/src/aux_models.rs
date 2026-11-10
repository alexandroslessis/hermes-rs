//! Settings ▸ Model ▸ Auxiliary: which model runs each side job. Four of Hermes's slots are jobs
//! hermes-rs runs: compression (context summaries), approval (judging flagged commands under
//! smart approvals), title generation and the background review (memories and skills after a
//! turn). Each follows the session's model ("auto") until a model is picked for it, with its own
//! reasoning effort if one is chosen.
//!
//! The choices live in `config.json` `aux_models` (the GPUI app's place and names: `compression`,
//! `approval`, `titles`, `review`), each `{provider, model, effort?}`, so both apps agree. Hermes's
//! other slots (vision, MCP, the skills hub, voice, kanban, the curator's consolidation) are jobs
//! this backend doesn't run with a model; their rows are hidden (`hide.css`).
//!
//! Hermes: `hermes_cli/web_routers/models.py` (`get_auxiliary_models`),
//! `hermes_cli/web_server_config.py` (`_apply_aux_assignment_sync`, `_AUX_TASK_SLOTS`).

use std::path::Path;

use serde_json::{Value, json};

use crate::providers::{ModelChoice, Settings};

/// One side job: Hermes's slot name, its key in `config.json` `aux_models`, and, for a slot the
/// page doesn't list itself, the label and hint it shows (as for a plugin's task).
struct Task {
    slot: &'static str,
    key: &'static str,
    label: Option<(&'static str, &'static str)>,
}

const TASKS: [Task; 4] = [
    Task {
        slot: "compression",
        key: "compression",
        label: None,
    },
    Task {
        slot: "approval",
        key: "approval",
        label: None,
    },
    Task {
        slot: "title_generation",
        key: "titles",
        label: None,
    },
    Task {
        slot: "background_review",
        key: "review",
        label: Some((
            "Background review",
            "Reviews finished turns for memories and skills worth saving",
        )),
    },
];

/// Hermes's reasoning levels (`hermes_constants.VALID_REASONING_EFFORTS`, plus `none`).
const EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

/// `GET /api/model/auxiliary` (`AuxiliaryModelsResponse`): the main model and each job's route.
pub fn list(data_dir: &Path) -> Value {
    let settings = Settings::load(data_dir);
    let tasks: Vec<Value> = TASKS
        .iter()
        .map(|task| {
            let choice = settings.aux_models.get(task.key);
            let provider = choice.and_then(|c| settings.provider(&c.provider));
            let mut row = json!({
                "task": task.slot,
                "provider": choice.map_or("auto", |c| c.provider.as_str()),
                "model": choice.map_or("", |c| c.model.as_str()),
                "base_url": provider.map_or("", |p| p.base_url.as_str()),
                "reasoning_effort": choice.and_then(|c| c.effort.clone()),
                "local_endpoint": provider.is_some_and(|p| p.is_local()),
            });
            if let Some((label, hint)) = task.label {
                row["label"] = json!(label);
                row["hint"] = json!(hint);
            }
            row
        })
        .collect();
    json!({
        "main": {"provider": settings.provider, "model": settings.model},
        "tasks": tasks,
    })
}

/// `POST /api/model/set` with `scope: "auxiliary"` (`ModelAssignmentRequest`): a job's model,
/// every job's (`task` empty), or all back to the main model (`task: "__reset__"`). `provider`
/// `"auto"` makes a job follow the session's model again. `reasoning_effort` absent keeps the
/// job's effort, `null` clears it, a level sets it.
pub fn assign(data_dir: &Path, body: &Value) -> (u16, Value) {
    let task = body["task"].as_str().unwrap_or_default();
    let mut config = read_config(data_dir);
    let Some(object) = config.as_object_mut() else {
        return (500, json!({"detail": "config.json isn't a JSON object"}));
    };
    let aux = object.entry("aux_models").or_insert_with(|| json!({}));
    if !aux.is_object() {
        *aux = json!({});
    }
    if task == "__reset__" {
        for t in &TASKS {
            aux[t.key] = Value::Null;
        }
        return match write_config(data_dir, &config) {
            Ok(()) => (
                200,
                json!({"ok": true, "scope": "auxiliary", "reset": true}),
            ),
            Err(error) => (500, json!({"detail": error})),
        };
    }
    let targets: Vec<&Task> = if task.is_empty() {
        TASKS.iter().collect()
    } else {
        match TASKS.iter().find(|t| t.slot == task) {
            Some(t) => vec![t],
            None => {
                return (
                    400,
                    json!({"detail": format!("unknown auxiliary task: {task}")}),
                );
            }
        }
    };
    let provider = body["provider"].as_str().unwrap_or_default().trim();
    let model = body["model"].as_str().unwrap_or_default().trim();
    let follow_main = provider.is_empty() || provider == "auto";
    if !follow_main {
        if Settings::load(data_dir).provider(provider).is_none() {
            return (
                400,
                json!({"detail": format!("Unknown provider {provider}")}),
            );
        }
        if model.is_empty() {
            return (400, json!({"detail": "Choose a model for this task"}));
        }
    }
    let effort = match body.get("reasoning_effort") {
        None => Effort::Keep,
        Some(Value::Null) => Effort::Clear,
        Some(Value::String(level)) if level.is_empty() => Effort::Clear,
        Some(Value::String(level)) if EFFORTS.contains(&level.as_str()) => {
            Effort::Set(level.clone())
        }
        Some(other) => {
            return (
                400,
                json!({"detail": format!("reasoning_effort must be one of: {} (got {other})", EFFORTS.join(", "))}),
            );
        }
    };
    for t in &targets {
        let previous = aux[t.key]["effort"].as_str().map(str::to_string);
        if follow_main {
            aux[t.key] = Value::Null;
            continue;
        }
        let effort = match &effort {
            Effort::Keep => previous,
            Effort::Clear => None,
            Effort::Set(level) => Some(level.clone()),
        };
        let mut choice = json!({"provider": provider, "model": model});
        if let Some(effort) = effort {
            choice["effort"] = json!(effort);
        }
        aux[t.key] = choice;
    }
    match write_config(data_dir, &config) {
        Ok(()) => (
            200,
            json!({
                "ok": true,
                "scope": "auxiliary",
                "tasks": targets.iter().map(|t| t.slot).collect::<Vec<_>>(),
                "provider": if follow_main { "auto" } else { provider },
                "model": model,
            }),
        ),
        Err(error) => (500, json!({"detail": error})),
    }
}

enum Effort {
    Keep,
    Clear,
    Set(String),
}

/// A job's model for the runtime, when one is picked and its provider still exists.
/// `context` gives a model's window when it's known.
pub fn runtime_model(
    settings: &Settings,
    choice: Option<&ModelChoice>,
    context: &dyn Fn(&str, &str) -> Option<usize>,
) -> Option<hermes_runtime::AuxModel> {
    let choice = choice?;
    let provider = settings.provider(&choice.provider)?;
    Some(hermes_runtime::AuxModel {
        provider: provider.runtime(),
        model: choice.model.clone(),
        context_length: context(&choice.provider, &choice.model),
        effort: choice.effort.clone(),
    })
}

/// The turn's side-job models (compression, background review, approval).
pub fn runtime_models(
    settings: &Settings,
    context: &dyn Fn(&str, &str) -> Option<usize>,
) -> hermes_runtime::AuxModels {
    let aux = &settings.aux_models;
    hermes_runtime::AuxModels {
        compression: runtime_model(settings, aux.compression.as_ref(), context),
        review: runtime_model(settings, aux.review.as_ref(), context),
        approval: runtime_model(settings, aux.approval.as_ref(), context),
    }
}

fn read_config(data_dir: &Path) -> Value {
    std::fs::read(data_dir.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}

/// Writes `config.json` back whole: only `aux_models` changed, the rest (providers, the GPUI
/// app's settings) is kept as read.
fn write_config(data_dir: &Path, config: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(data_dir.join("config.json"), bytes)
        .map_err(|e| format!("Saving config.json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
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

    fn row<'a>(list: &'a Value, slot: &str) -> &'a Value {
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["task"] == slot)
            .unwrap()
    }

    #[test]
    fn every_job_follows_the_main_model_until_one_is_picked() {
        let dir = home();
        let list = list(dir.path());
        assert_eq!(list["main"], json!({"provider": "local", "model": "big"}));
        let slots: Vec<&str> = list["tasks"]
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
        assert_eq!(row(&list, "compression")["provider"], "auto");
        assert_eq!(
            row(&list, "background_review")["label"],
            "Background review"
        );
        assert!(
            runtime_models(&Settings::load(dir.path()), &|_, _| None)
                .compression
                .is_none()
        );
    }

    #[test]
    fn a_picked_model_with_its_effort_reaches_the_runtime() {
        let dir = home();
        let body = json!({"scope": "auxiliary", "task": "compression", "provider": "remote", "model": "small", "reasoning_effort": "low"});
        assert_eq!(assign(dir.path(), &body).0, 200);
        let list = list(dir.path());
        let compression = row(&list, "compression");
        assert_eq!(
            (
                compression["provider"].as_str(),
                compression["model"].as_str()
            ),
            (Some("remote"), Some("small"))
        );
        assert_eq!(compression["reasoning_effort"], "low");
        assert_eq!(compression["base_url"], "https://example.com/v1");
        assert_eq!(compression["local_endpoint"], false);
        let models = runtime_models(&Settings::load(dir.path()), &|_, model| {
            (model == "small").then_some(32_000)
        });
        let aux = models.compression.unwrap();
        assert_eq!(
            (
                aux.model.as_str(),
                aux.context_length,
                aux.effort.as_deref()
            ),
            ("small", Some(32_000), Some("low"))
        );
        assert!(models.review.is_none());
        // Picking another model without an effort keeps the job's effort; null clears it.
        assign(
            dir.path(),
            &json!({"task": "compression", "provider": "local", "model": "tiny"}),
        );
        assert_eq!(
            row(&list_of(&dir), "compression")["reasoning_effort"],
            "low"
        );
        assign(
            dir.path(),
            &json!({"task": "compression", "provider": "local", "model": "tiny", "reasoning_effort": null}),
        );
        assert_eq!(
            row(&list_of(&dir), "compression")["reasoning_effort"],
            Value::Null
        );
        // The rest of config.json is untouched, the GPUI app's curator choice included.
        let config = read_config(dir.path());
        assert_eq!(config["window"], json!({"w": 1}));
        assert_eq!(config["aux_models"]["curator"]["model"], "c");
    }

    fn list_of(dir: &tempfile::TempDir) -> Value {
        list(dir.path())
    }

    #[test]
    fn titles_and_review_use_the_gpui_app_s_names() {
        let dir = home();
        assign(
            dir.path(),
            &json!({"task": "title_generation", "provider": "local", "model": "t"}),
        );
        assign(
            dir.path(),
            &json!({"task": "background_review", "provider": "local", "model": "r"}),
        );
        let settings = Settings::load(dir.path());
        assert_eq!(settings.aux_models.titles.unwrap().model, "t");
        assert_eq!(settings.aux_models.review.unwrap().model, "r");
    }

    #[test]
    fn auto_and_reset_go_back_to_the_main_model() {
        let dir = home();
        assign(
            dir.path(),
            &json!({"task": "", "provider": "remote", "model": "m"}),
        );
        assert!(
            list_of(&dir)["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["model"] == "m")
        );
        assign(
            dir.path(),
            &json!({"task": "approval", "provider": "auto", "model": ""}),
        );
        assert_eq!(row(&list_of(&dir), "approval")["provider"], "auto");
        assert_eq!(row(&list_of(&dir), "compression")["model"], "m");
        let (status, body) = assign(
            dir.path(),
            &json!({"task": "__reset__", "provider": "local", "model": "big"}),
        );
        assert_eq!((status, body["reset"].clone()), (200, json!(true)));
        assert!(
            list_of(&dir)["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["provider"] == "auto")
        );
        assert_eq!(
            read_config(dir.path())["aux_models"]["curator"]["model"],
            "c"
        );
    }

    #[test]
    fn bad_requests_are_refused() {
        let dir = home();
        let refused = |body: Value| assign(dir.path(), &body).0;
        assert_eq!(
            refused(json!({"task": "vision", "provider": "local", "model": "m"})),
            400
        );
        assert_eq!(
            refused(json!({"task": "compression", "provider": "nowhere", "model": "m"})),
            400
        );
        assert_eq!(
            refused(json!({"task": "compression", "provider": "local", "model": ""})),
            400
        );
        assert_eq!(
            refused(
                json!({"task": "compression", "provider": "local", "model": "m", "reasoning_effort": "max"})
            ),
            400
        );
        assert_eq!(row(&list_of(&dir), "compression")["provider"], "auto");
    }
}
