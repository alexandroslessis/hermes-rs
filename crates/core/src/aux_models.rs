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
//! Hermes-rs, diverged (backend v2 Phase 4): [`local_servers`] / [`use_local`] are ours, not
//! Hermes's contract: one click puts every side job on a local model server so they stop costing
//! requests on the main provider's quota. The page reaches them only through patch 0023; Hermes's
//! own routes keep their shapes.
//!
//! Typed inputs and outputs and [`AuxError`]; the page's JSON is `crates/desktop/src/aux_models.rs`.

use std::path::Path;

use serde_json::{Value, json};

use crate::providers::{ModelChoice, Settings};

/// One side job: Hermes's slot name, its key in `config.json` `aux_models`, and, for a slot the
/// page doesn't list itself, the label and hint it shows (as for a plugin's task).
pub struct Task {
    pub slot: &'static str,
    pub key: &'static str,
    pub label: Option<(&'static str, &'static str)>,
}

pub const TASKS: [Task; 4] = [
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
pub const EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

/// Why an assignment failed. The gateway answers 400 and 500 for these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuxError {
    /// The request is wrong (an unknown task, provider or effort, no model).
    Invalid(String),
    /// `config.json` could not be read as an object or written.
    Failed(String),
}

impl AuxError {
    pub fn message(&self) -> &str {
        match self {
            AuxError::Invalid(m) | AuxError::Failed(m) => m,
        }
    }
}

impl std::fmt::Display for AuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for AuxError {}

/// One job's route as Settings shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub slot: &'static str,
    /// The provider id, or "auto" (follows the session's model).
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub effort: Option<String>,
    pub local_endpoint: bool,
    pub label: Option<(&'static str, &'static str)>,
}

/// The main model and each job's route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxList {
    pub main_provider: Option<String>,
    pub main_model: Option<String>,
    pub tasks: Vec<TaskRow>,
}

/// The main model and each job's route.
pub fn list(data_dir: &Path) -> AuxList {
    let settings = Settings::load(data_dir);
    let tasks = TASKS
        .iter()
        .map(|task| {
            let choice = settings.aux_models.get(task.key);
            let provider = choice.and_then(|c| settings.provider(&c.provider));
            TaskRow {
                slot: task.slot,
                provider: choice.map_or("auto", |c| c.provider.as_str()).to_string(),
                model: choice.map_or("", |c| c.model.as_str()).to_string(),
                base_url: provider.map_or("", |p| p.base_url.as_str()).to_string(),
                effort: choice.and_then(|c| c.effort.clone()),
                local_endpoint: provider.is_some_and(|p| p.is_local()),
                label: task.label,
            }
        })
        .collect();
    AuxList {
        main_provider: settings.provider,
        main_model: settings.model,
        tasks,
    }
}

/// What to do with a job's reasoning effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffortChange {
    /// Keep the job's effort.
    Keep,
    /// Clear it.
    Clear,
    /// Set a level (checked against [`EFFORTS`]).
    Level(String),
    /// Anything else the caller was given (shown in the refusal).
    Other(String),
}

/// A job's model, every job's (`task` empty), or all back to the main model (`task`
/// `"__reset__"`). `provider` `"auto"` (or empty) makes a job follow the session's model again.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub task: String,
    pub provider: String,
    pub model: String,
    pub effort: EffortChange,
}

/// What an assignment did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assigned {
    /// Every job back to the main model.
    Reset,
    /// These jobs now use `provider` ("auto" or an id) and `model`.
    Tasks {
        slots: Vec<&'static str>,
        provider: String,
        model: String,
    },
}

/// Applies an [`Assignment`] to `config.json`, keeping the rest of it.
pub fn assign(data_dir: &Path, request: &Assignment) -> Result<Assigned, AuxError> {
    let task = request.task.as_str();
    let mut config = read_config(data_dir);
    let Some(object) = config.as_object_mut() else {
        return Err(AuxError::Failed("config.json isn't a JSON object".into()));
    };
    let aux = object.entry("aux_models").or_insert_with(|| json!({}));
    if !aux.is_object() {
        *aux = json!({});
    }
    if task == "__reset__" {
        for t in &TASKS {
            aux[t.key] = Value::Null;
        }
        write_config(data_dir, &config).map_err(AuxError::Failed)?;
        return Ok(Assigned::Reset);
    }
    let targets: Vec<&Task> = if task.is_empty() {
        TASKS.iter().collect()
    } else {
        match TASKS.iter().find(|t| t.slot == task) {
            Some(t) => vec![t],
            None => {
                return Err(AuxError::Invalid(format!("unknown auxiliary task: {task}")));
            }
        }
    };
    let provider = request.provider.trim();
    let model = request.model.trim();
    let follow_main = provider.is_empty() || provider == "auto";
    if !follow_main {
        if Settings::load(data_dir).provider(provider).is_none() {
            return Err(AuxError::Invalid(format!("Unknown provider {provider}")));
        }
        if model.is_empty() {
            return Err(AuxError::Invalid("Choose a model for this task".into()));
        }
    }
    let effort = match &request.effort {
        EffortChange::Keep => Effort::Keep,
        EffortChange::Clear => Effort::Clear,
        EffortChange::Level(level) if level.is_empty() => Effort::Clear,
        EffortChange::Level(level) if EFFORTS.contains(&level.as_str()) => {
            Effort::Set(level.clone())
        }
        EffortChange::Level(level) => {
            return Err(refuse_effort(&Value::String(level.clone()).to_string()));
        }
        EffortChange::Other(shown) => return Err(refuse_effort(shown)),
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
    write_config(data_dir, &config).map_err(AuxError::Failed)?;
    Ok(Assigned::Tasks {
        slots: targets.iter().map(|t| t.slot).collect(),
        provider: if follow_main { "auto" } else { provider }.to_string(),
        model: model.to_string(),
    })
}

fn refuse_effort(shown: &str) -> AuxError {
    AuxError::Invalid(format!(
        "reasoning_effort must be one of: {} (got {shown})",
        EFFORTS.join(", ")
    ))
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
        let url = url
            .trim_end_matches('/')
            .to_lowercase()
            .replace("127.0.0.1", "localhost");
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
            if settings
                .providers
                .iter()
                .any(|p| same_server(&p.base_url, &base_url))
            {
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
    let probed = futures::future::join_all(candidates.into_iter().map(
        |(server, runtime, fixed)| async move {
            let listed = probe(runtime).await?;
            let models = fixed.unwrap_or(listed);
            (!models.is_empty()).then_some(LocalServer { models, ..server })
        },
    ))
    .await;
    probed.into_iter().flatten().collect()
}

/// What putting every job on a local server did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedLocal {
    pub assigned: Assigned,
    pub base_url: String,
    /// The server wasn't in Settings; it was added as a provider.
    pub added_provider: bool,
}

/// Sets every side job (compression, approval, titles, review) to a local model. `provider` /
/// `model` pick the server and its model; absent, the first server that answers and its starting
/// model (the endpoint's own, else the first listed chat model). A server found on a well-known
/// port that isn't in Settings yet is added as a provider (`ollama` / `lm-studio`). Invalid when
/// no local server answers: nothing is changed.
pub async fn use_local(
    data_dir: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    well_known: &[(&str, &str, &str)],
) -> Result<UsedLocal, AuxError> {
    let wanted = provider.map(str::trim).filter(|s| !s.is_empty());
    let servers: Vec<LocalServer> = local_servers(data_dir, well_known)
        .await
        .into_iter()
        .filter(|s| wanted.is_none_or(|w| s.id == w))
        .collect();
    let Some(server) = servers.first() else {
        return Err(AuxError::Invalid(
            "No local model server answered. Start Ollama or LM Studio (or add a local endpoint under Settings > Providers) and try again.".into(),
        ));
    };
    let model = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) if server.models.iter().any(|m| m == model) => model.to_string(),
        Some(model) => {
            return Err(AuxError::Invalid(format!(
                "{} doesn't serve a model called {model}",
                server.name
            )));
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
            let saved = crate::endpoints::save(
                &dir,
                &crate::endpoints::EndpointInput {
                    id: server.id.clone(),
                    name: server.name.clone(),
                    base_url: server.base_url.clone(),
                    model: model.clone(),
                    discover_models: true,
                    ..Default::default()
                },
            );
            // The id the new provider got (a taken one is numbered).
            provider_id = saved.map_err(|error| match error {
                crate::endpoints::EndpointError::Failed(m) => AuxError::Failed(m),
                other => AuxError::Invalid(other.message().to_string()),
            })?;
        }
        let assigned = assign(
            &dir,
            &Assignment {
                task: String::new(),
                provider: provider_id,
                model,
                effort: EffortChange::Keep,
            },
        )?;
        Ok(UsedLocal {
            assigned,
            base_url: server.base_url.clone(),
            added_provider: !server.configured,
        })
    })
    .await
    .unwrap_or_else(|error| Err(AuxError::Failed(format!("{error}"))))
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

    fn assignment(task: &str, provider: &str, model: &str, effort: EffortChange) -> Assignment {
        Assignment {
            task: task.into(),
            provider: provider.into(),
            model: model.into(),
            effort,
        }
    }

    fn pick(dir: &Path, task: &str, provider: &str, model: &str) {
        assign(dir, &assignment(task, provider, model, EffortChange::Keep)).unwrap();
    }

    fn row(list: &AuxList, slot: &str) -> TaskRow {
        list.tasks.iter().find(|r| r.slot == slot).unwrap().clone()
    }

    #[test]
    fn every_job_follows_the_main_model_until_one_is_picked() {
        let dir = home();
        let list = list(dir.path());
        assert_eq!(
            (list.main_provider.as_deref(), list.main_model.as_deref()),
            (Some("local"), Some("big"))
        );
        let slots: Vec<&str> = list.tasks.iter().map(|r| r.slot).collect();
        assert_eq!(
            slots,
            [
                "compression",
                "approval",
                "title_generation",
                "background_review"
            ]
        );
        assert_eq!(row(&list, "compression").provider, "auto");
        assert_eq!(
            row(&list, "background_review").label.map(|l| l.0),
            Some("Background review")
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
        let done = assign(
            dir.path(),
            &assignment(
                "compression",
                "remote",
                "small",
                EffortChange::Level("low".into()),
            ),
        )
        .unwrap();
        assert_eq!(
            done,
            Assigned::Tasks {
                slots: vec!["compression"],
                provider: "remote".into(),
                model: "small".into()
            }
        );
        let compression = row(&list(dir.path()), "compression");
        assert_eq!(
            (compression.provider.as_str(), compression.model.as_str()),
            ("remote", "small")
        );
        assert_eq!(compression.effort.as_deref(), Some("low"));
        assert_eq!(compression.base_url, "https://example.com/v1");
        assert!(!compression.local_endpoint);
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
        pick(dir.path(), "compression", "local", "tiny");
        assert_eq!(
            row(&list(dir.path()), "compression").effort.as_deref(),
            Some("low")
        );
        assign(
            dir.path(),
            &assignment("compression", "local", "tiny", EffortChange::Clear),
        )
        .unwrap();
        assert_eq!(row(&list(dir.path()), "compression").effort, None);
        // The rest of config.json is untouched, the GPUI app's curator choice included.
        let config = read_config(dir.path());
        assert_eq!(config["window"], json!({"w": 1}));
        assert_eq!(config["aux_models"]["curator"]["model"], "c");
    }

    #[test]
    fn titles_and_review_use_the_gpui_app_s_names() {
        let dir = home();
        pick(dir.path(), "title_generation", "local", "t");
        pick(dir.path(), "background_review", "local", "r");
        let settings = Settings::load(dir.path());
        assert_eq!(settings.aux_models.titles.unwrap().model, "t");
        assert_eq!(settings.aux_models.review.unwrap().model, "r");
    }

    #[test]
    fn auto_and_reset_go_back_to_the_main_model() {
        let dir = home();
        pick(dir.path(), "", "remote", "m");
        assert!(list(dir.path()).tasks.iter().all(|r| r.model == "m"));
        pick(dir.path(), "approval", "auto", "");
        assert_eq!(row(&list(dir.path()), "approval").provider, "auto");
        assert_eq!(row(&list(dir.path()), "compression").model, "m");
        let done = assign(
            dir.path(),
            &assignment("__reset__", "local", "big", EffortChange::Keep),
        )
        .unwrap();
        assert_eq!(done, Assigned::Reset);
        assert!(list(dir.path()).tasks.iter().all(|r| r.provider == "auto"));
        assert_eq!(
            read_config(dir.path())["aux_models"]["curator"]["model"],
            "c"
        );
    }

    #[test]
    fn bad_requests_are_refused() {
        let dir = home();
        let refused = |task: &str, provider: &str, model: &str, effort: EffortChange| {
            matches!(
                assign(dir.path(), &assignment(task, provider, model, effort)),
                Err(AuxError::Invalid(_))
            )
        };
        assert!(refused("vision", "local", "m", EffortChange::Keep));
        assert!(refused("compression", "nowhere", "m", EffortChange::Keep));
        assert!(refused("compression", "local", "", EffortChange::Keep));
        assert!(refused(
            "compression",
            "local",
            "m",
            EffortChange::Level("max".into())
        ));
        assert!(refused(
            "compression",
            "local",
            "m",
            EffortChange::Other("7".into())
        ));
        assert_eq!(row(&list(dir.path()), "compression").provider, "auto");
        // The refusal names the allowed levels and what it got.
        let error = assign(
            dir.path(),
            &assignment(
                "compression",
                "local",
                "m",
                EffortChange::Level("max".into()),
            ),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "reasoning_effort must be one of: none, minimal, low, medium, high, xhigh (got \"max\")"
        );
    }

    // ---- "Use a local model for background jobs" -------------------------------------------

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn models_server(ids: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        let data: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "object": "model"}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data})),
            )
            .mount(&server)
            .await;
        server
    }

    fn closed_port_url() -> String {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        format!("http://127.0.0.1:{port}/v1")
    }

    #[tokio::test]
    async fn one_click_sets_every_job_to_a_configured_local_server() {
        let server = models_server(&["nomic-embed-text", "qwen-small", "zed"]).await;
        let dir = hermes_testdir::tempdir().unwrap();
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
        let servers = local_servers(dir.path(), &[]).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "mine");
        assert_eq!(servers[0].models, ["qwen-small", "zed"]);
        let used = use_local(dir.path(), None, None, &[]).await.unwrap();
        assert!(!used.added_provider);
        let list = list(dir.path());
        for slot in [
            "compression",
            "approval",
            "title_generation",
            "background_review",
        ] {
            let r = row(&list, slot);
            assert_eq!(
                (r.provider.as_str(), r.model.as_str()),
                ("mine", "qwen-small"),
                "{slot}"
            );
            assert!(r.local_endpoint);
        }
        // The main model and the rest of the config are untouched.
        assert_eq!(
            (list.main_provider.as_deref(), list.main_model.as_deref()),
            (Some("remote"), Some("big"))
        );
        assert_eq!(read_config(dir.path())["window"], json!({"w": 1}));
        // A model the server doesn't have is refused and changes nothing.
        assert!(matches!(
            use_local(dir.path(), None, Some("nope"), &[]).await,
            Err(AuxError::Invalid(_))
        ));
        use_local(dir.path(), None, Some("zed"), &[]).await.unwrap();
        assert_eq!(
            row(&self::list(dir.path()), "title_generation").model,
            "zed"
        );
    }

    #[tokio::test]
    async fn a_server_on_a_well_known_port_is_added_as_a_provider() {
        let server = models_server(&["llama3"]).await;
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            json!({"providers": [], "window": {"w": 1}}).to_string(),
        )
        .unwrap();
        let url = format!("{}/v1", server.uri());
        let known = [
            ("ollama", "Ollama", url.as_str()),
            ("lm-studio", "LM Studio", "http://127.0.0.1:1/v1"),
        ];
        let used = use_local(dir.path(), None, None, &known).await.unwrap();
        assert!(used.added_provider);
        assert_eq!(used.base_url, url);
        let settings = Settings::load(dir.path());
        assert_eq!(settings.provider("ollama").unwrap().base_url, url);
        assert_eq!(settings.aux_models.compression.unwrap().provider, "ollama");
        assert_eq!(read_config(dir.path())["window"], json!({"w": 1}));
        // The next time it's a configured server, not a second provider.
        let again = use_local(dir.path(), None, None, &known).await.unwrap();
        assert!(!again.added_provider);
        assert_eq!(Settings::load(dir.path()).providers.len(), 1);
    }

    #[tokio::test]
    async fn with_no_local_server_up_nothing_changes() {
        let dir = hermes_testdir::tempdir().unwrap();
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
        let error = use_local(dir.path(), None, None, &known).await.unwrap_err();
        assert!(error.message().contains("No local model server"));
        assert_eq!(Settings::load(dir.path()).providers.len(), 2);
        assert_eq!(
            Settings::load(dir.path())
                .aux_models
                .compression
                .unwrap()
                .model,
            "keep"
        );
        assert!(Settings::load(dir.path()).aux_models.titles.is_none());
        assert!(local_servers(dir.path(), &known).await.is_empty());
    }

    /// A server that takes the connection and never answers costs one probe timeout, even
    /// when there are two of them (probed side by side).
    #[tokio::test]
    async fn hung_servers_are_bounded_by_the_probe_timeout() {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            json!({"providers": []}).to_string(),
        )
        .unwrap();
        // Listening (the OS completes the handshake) but nobody reads or answers.
        let hung: Vec<std::net::TcpListener> = (0..2)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let urls: Vec<String> = hung
            .iter()
            .map(|l| format!("http://127.0.0.1:{}/v1", l.local_addr().unwrap().port()))
            .collect();
        let known = [
            ("ollama", "Ollama", urls[0].as_str()),
            ("lm-studio", "LM Studio", urls[1].as_str()),
        ];
        let started = std::time::Instant::now();
        assert!(use_local(dir.path(), None, None, &known).await.is_err());
        let elapsed = started.elapsed();
        assert!(
            elapsed < PROBE_TIMEOUT + std::time::Duration::from_secs(2),
            "{elapsed:?}"
        );
        assert!(Settings::load(dir.path()).providers.is_empty());
    }
}
