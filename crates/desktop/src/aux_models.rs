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
//!
//! Hermes-rs, diverged (backend v2 Phase 4): `GET/POST /api/model/auxiliary/local` ([`local_list`],
//! [`use_local`]) are ours, not Hermes's contract: one click puts every side job on a local model
//! server so they stop costing requests on the main provider's quota. The page reaches them only
//! through patch 0023; Hermes's own routes above keep their shapes.

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

// ---- one click: a local model for every background job ---------------------------------------

/// Local servers probed when none is configured: `(id, name, base URL)`. Ollama's and LM Studio's
/// default ports (the OpenAI-compatible `/v1` of each).
pub const WELL_KNOWN_LOCAL: [(&str, &str, &str); 2] = [
    ("ollama", "Ollama", "http://localhost:11434/v1"),
    ("lm-studio", "LM Studio", "http://localhost:1234/v1"),
];

/// How long a local server gets to list its models.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A local model server that answered: configured in Settings ▸ Providers, or found on a
/// well-known port.
#[derive(Debug, Clone)]
pub struct LocalServer {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Chat models it serves (embedding models left out), sorted.
    pub models: Vec<String>,
    /// Whether the provider is already in `config.json`.
    pub configured: bool,
    /// The model the endpoint starts with, when it has one.
    pub default_model: Option<String>,
}

/// The chat models of a server (embedding models left out, sorted), or `None` when it doesn't
/// answer within [`PROBE_TIMEOUT`].
async fn probe(provider: hermes_runtime::ProviderConfig) -> Option<Vec<String>> {
    let listed = tokio::time::timeout(PROBE_TIMEOUT, provider.list_models()).await;
    let Ok(Ok(models)) = listed else {
        return None;
    };
    Some(chat_models(models.into_iter().map(|m| m.id).collect()))
}

fn chat_models(mut ids: Vec<String>) -> Vec<String> {
    ids.retain(|id| !id.to_lowercase().contains("embed"));
    ids.sort();
    ids
}

fn same_server(a: &str, b: &str) -> bool {
    let key = |url: &str| {
        let url = url.trim_end_matches('/').to_lowercase().replace("127.0.0.1", "localhost");
        url.strip_suffix("/v1").map(str::to_string).unwrap_or(url)
    };
    key(a) == key(b)
}

/// The local servers that answer: local providers from Settings, then `well_known` ones not
/// already configured. Servers that are down are left out.
pub async fn local_servers(data_dir: &Path, well_known: &[(&str, &str, &str)]) -> Vec<LocalServer> {
    // Reading config.json and the key store is blocking work (ground rule 7).
    let dir = data_dir.to_path_buf();
    let well_known: Vec<(String, String, String)> = well_known
        .iter()
        .map(|(id, name, url)| ((*id).into(), (*name).into(), (*url).into()))
        .collect();
    let candidates = tokio::task::spawn_blocking(move || {
        let settings = Settings::load(&dir);
        let mut candidates = Vec::new();
        for provider in settings.providers.iter().filter(|p| p.is_local()) {
            // Without discovery the listed models are the provider's own; the probe still
            // tells whether the server is up.
            let models = (!provider.discover_models).then(|| chat_models(provider.models.clone()));
            let server = LocalServer {
                id: provider.id.clone(),
                name: provider.name.clone(),
                base_url: provider.base_url.clone(),
                models: Vec::new(),
                configured: true,
                default_model: provider.default_model.clone(),
            };
            candidates.push((server, provider.runtime(), models));
        }
        for (id, name, base_url) in well_known {
            if settings.providers.iter().any(|p| same_server(&p.base_url, &base_url)) {
                continue;
            }
            let runtime = hermes_runtime::ProviderConfig {
                id: id.clone(),
                base_url: base_url.clone(),
                api_key: None,
            };
            let server = LocalServer {
                id,
                name,
                base_url,
                models: Vec::new(),
                configured: false,
                default_model: None,
            };
            candidates.push((server, runtime, None));
        }
        candidates
    })
    .await
    .unwrap_or_default();
    // Probed side by side: a hung server costs one PROBE_TIMEOUT, not one per server.
    let probed = futures::future::join_all(
        candidates
            .into_iter()
            .map(|(server, runtime, fixed)| async move {
                let listed = probe(runtime).await?;
                let models = fixed.unwrap_or(listed);
                (!models.is_empty()).then_some(LocalServer { models, ..server })
            }),
    )
    .await;
    probed.into_iter().flatten().collect()
}

/// `GET /api/model/auxiliary/local` (ours, not Hermes's): the local servers that answer and their
/// models, so the page can offer "Use a local model for background jobs".
pub async fn local_list(data_dir: &Path, well_known: &[(&str, &str, &str)]) -> (u16, Value) {
    let servers = local_servers(data_dir, well_known).await;
    (
        200,
        json!({"servers": servers.iter().map(|s| json!({
            "provider": s.id, "name": s.name, "base_url": s.base_url,
            "models": s.models, "configured": s.configured,
        })).collect::<Vec<_>>()}),
    )
}

/// `POST /api/model/auxiliary/local` (ours): sets every side job (compression, approval, titles,
/// review) to a local model. `provider` / `model` pick the server and its model; absent, the first
/// server that answers and its starting model (the endpoint's own, else the first listed
/// chat model). A server found on a well-known port that isn't in Settings yet is added as a
/// provider (`ollama` / `lm-studio`). 400 when no local server answers: nothing is changed.
pub async fn use_local(
    data_dir: &Path,
    body: &Value,
    well_known: &[(&str, &str, &str)],
) -> (u16, Value) {
    let wanted = body["provider"].as_str().map(str::trim).filter(|s| !s.is_empty());
    let servers: Vec<LocalServer> = local_servers(data_dir, well_known)
        .await
        .into_iter()
        .filter(|s| wanted.is_none_or(|w| s.id == w))
        .collect();
    let Some(server) = servers.first() else {
        return (
            400,
            json!({"detail": "No local model server answered. Start Ollama or LM Studio (or add a local endpoint under Settings > Providers) and try again."}),
        );
    };
    let model = match body["model"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) if server.models.iter().any(|m| m == model) => model.to_string(),
        Some(model) => {
            return (
                400,
                json!({"detail": format!("{} doesn't serve a model called {model}", server.name)}),
            );
        }
        None => server
            .default_model
            .clone()
            .filter(|m| server.models.contains(m))
            .unwrap_or_else(|| server.models[0].clone()),
    };
    let server = server.clone();
    let dir = data_dir.to_path_buf();
    // config.json writes (and the key store) off the async workers (ground rule 7).
    tokio::task::spawn_blocking(move || {
        let mut provider_id = server.id.clone();
        if !server.configured {
            let (status, answer) = crate::endpoints::save(
                &dir,
                &json!({"id": server.id, "name": server.name, "base_url": server.base_url, "model": model}),
            );
            if status != 200 {
                return (status, answer);
            }
            // The id the new provider got (a taken one is numbered).
            if let Some(id) = answer["id"].as_str() {
                provider_id = id.to_string();
            }
        }
        let (status, mut answer) = assign(
            &dir,
            &json!({"scope": "auxiliary", "task": "", "provider": provider_id, "model": model}),
        );
        if status == 200 {
            answer["base_url"] = json!(server.base_url);
            answer["added_provider"] = json!(!server.configured);
        }
        (status, answer)
    })
    .await
    .unwrap_or_else(|error| (500, json!({"detail": format!("{error}")})))
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

    // ---- "Use a local model for background jobs" -------------------------------------------

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn models_server(ids: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        let data: Vec<Value> = ids.iter().map(|id| json!({"id": id, "object": "model"})).collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data})))
            .mount(&server)
            .await;
        server
    }

    fn closed_port_url() -> String {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        format!("http://127.0.0.1:{port}/v1")
    }

    #[tokio::test]
    async fn one_click_sets_every_job_to_a_configured_local_server() {
        let server = models_server(&["nomic-embed-text", "qwen-small", "zed"]).await;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/v1", server.uri());
        std::fs::write(
            dir.path().join("config.json"),
            json!({"providers": [
                {"id": "remote", "name": "Remote", "base_url": "https://example.com/v1"},
                {"id": "mine", "name": "Mine", "base_url": url}],
                "provider": "remote", "model": "big", "window": {"w": 1}})
            .to_string(),
        )
        .unwrap();
        // Only the local server is offered; the embedding model is left out.
        let (_, listed) = local_list(dir.path(), &[]).await;
        assert_eq!(listed["servers"].as_array().unwrap().len(), 1);
        assert_eq!(listed["servers"][0]["provider"], "mine");
        assert_eq!(listed["servers"][0]["models"], json!(["qwen-small", "zed"]));
        let (status, answer) = use_local(dir.path(), &json!({}), &[]).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["added_provider"], false);
        let list = list(dir.path());
        for slot in ["compression", "approval", "title_generation", "background_review"] {
            let r = row(&list, slot);
            assert_eq!((r["provider"].as_str(), r["model"].as_str()), (Some("mine"), Some("qwen-small")), "{slot}");
            assert_eq!(r["local_endpoint"], true);
        }
        // The main model and the rest of the config are untouched.
        assert_eq!(list["main"], json!({"provider": "remote", "model": "big"}));
        assert_eq!(read_config(dir.path())["window"], json!({"w": 1}));
        // A model the server doesn't have is refused and changes nothing.
        let (status, _) = use_local(dir.path(), &json!({"model": "nope"}), &[]).await;
        assert_eq!(status, 400);
        let (status, _) = use_local(dir.path(), &json!({"model": "zed"}), &[]).await;
        assert_eq!(status, 200);
        assert_eq!(row(&list_of(&dir), "title_generation")["model"], "zed");
    }

    #[tokio::test]
    async fn a_server_on_a_well_known_port_is_added_as_a_provider() {
        let server = models_server(&["llama3"]).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), json!({"providers": [], "window": {"w": 1}}).to_string()).unwrap();
        let url = format!("{}/v1", server.uri());
        let known = [("ollama", "Ollama", url.as_str()), ("lm-studio", "LM Studio", "http://127.0.0.1:1/v1")];
        let (status, answer) = use_local(dir.path(), &json!({}), &known).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["added_provider"], true);
        let settings = Settings::load(dir.path());
        assert_eq!(settings.provider("ollama").unwrap().base_url, url);
        assert_eq!(settings.aux_models.compression.unwrap().provider, "ollama");
        assert_eq!(read_config(dir.path())["window"], json!({"w": 1}));
        // The next time it's a configured server, not a second provider.
        let (_, answer) = use_local(dir.path(), &json!({}), &known).await;
        assert_eq!(answer["added_provider"], false);
        assert_eq!(Settings::load(dir.path()).providers.len(), 1);
    }

    #[tokio::test]
    async fn with_no_local_server_up_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        // A configured local server that is down, and a well-known one that is down too.
        std::fs::write(
            dir.path().join("config.json"),
            // The second lists its models itself: down is down all the same.
            json!({"providers": [{"id": "mine", "name": "Mine", "base_url": closed_port_url()},
                                 {"id": "fixed", "name": "Fixed", "base_url": closed_port_url(),
                                  "discover_models": false, "models": ["m"]}],
                   "aux_models": {"compression": {"provider": "mine", "model": "keep"}}})
            .to_string(),
        )
        .unwrap();
        let known_url = closed_port_url();
        let known = [("ollama", "Ollama", known_url.as_str())];
        let (status, answer) = use_local(dir.path(), &json!({}), &known).await;
        assert_eq!(status, 400, "{answer}");
        assert!(answer["detail"].as_str().unwrap().contains("No local model server"));
        assert_eq!(Settings::load(dir.path()).providers.len(), 2);
        assert_eq!(Settings::load(dir.path()).aux_models.compression.unwrap().model, "keep");
        assert!(Settings::load(dir.path()).aux_models.titles.is_none());
        assert!(local_list(dir.path(), &known).await.1["servers"].as_array().unwrap().is_empty());
    }

    /// A server that takes the connection and never answers costs one probe timeout, even
    /// when there are two of them (probed side by side).
    #[tokio::test]
    async fn hung_servers_are_bounded_by_the_probe_timeout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), json!({"providers": []}).to_string()).unwrap();
        // Listening (the OS completes the handshake) but nobody reads or answers.
        let hung: Vec<std::net::TcpListener> =
            (0..2).map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap()).collect();
        let urls: Vec<String> = hung
            .iter()
            .map(|l| format!("http://127.0.0.1:{}/v1", l.local_addr().unwrap().port()))
            .collect();
        let known = [("ollama", "Ollama", urls[0].as_str()), ("lm-studio", "LM Studio", urls[1].as_str())];
        let started = std::time::Instant::now();
        let (status, _) = use_local(dir.path(), &json!({}), &known).await;
        assert_eq!(status, 400);
        let elapsed = started.elapsed();
        assert!(elapsed < PROBE_TIMEOUT + std::time::Duration::from_secs(2), "{elapsed:?}");
        assert!(Settings::load(dir.path()).providers.is_empty());
    }
}
