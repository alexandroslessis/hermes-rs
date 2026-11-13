//! Command Center ▸ Maintenance: the skills curator (status, pause, run now, and its schedule) and
//! the built-in memory files (sizes, reset), with the action status the page follows a run by.
//!
//! Hermes: `hermes_cli/web_routers/status.py` (`/api/curator*`, `/api/actions/{name}`),
//! `hermes_cli/web_routers/ops.py` (`/api/memory`, `/api/memory/reset`), `agent/curator.py`
//! (`maybe_run_curator`: the schedule).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hermes_memory::{MemoryStore, Target};
use hermes_runtime::budget::CuratorMode;
use hermes_runtime::{CuratorModel, Runtime, TurnOptions};
use hermes_tools::SkillStore;
use crate::providers::Settings;
use hermes_tools::curator::{ARCHIVE_AFTER_DAYS, INTERVAL_HOURS, MIN_IDLE_HOURS, STALE_AFTER_DAYS};
use serde_json::{Value, json};

/// The curator's action name, as Hermes's (`curator-run`).
pub const CURATOR_RUN: &str = "curator-run";

/// The built-in memory files and the key each is reported and reset by.
const MEMORY_FILES: [(&str, &str); 2] = [("MEMORY.md", "memory"), ("USER.md", "user")];

#[derive(Clone, Default)]
struct ActionState {
    running: bool,
    lines: Vec<String>,
    exit_code: Option<i32>,
}

/// Background actions the page follows by name (`/api/actions/{name}`).
#[derive(Default)]
pub struct Actions(Mutex<HashMap<String, ActionState>>);

impl Actions {
    /// Marks `name` running; `false` when it already is.
    pub fn start(&self, name: &str) -> bool {
        let Ok(mut actions) = self.0.lock() else {
            return false;
        };
        if actions.get(name).is_some_and(|a| a.running) {
            return false;
        }
        actions.insert(
            name.to_string(),
            ActionState {
                running: true,
                ..Default::default()
            },
        );
        true
    }

    pub fn finish(&self, name: &str, lines: Vec<String>, ok: bool) {
        if let Ok(mut actions) = self.0.lock() {
            actions.insert(
                name.to_string(),
                ActionState {
                    running: false,
                    lines,
                    exit_code: Some(if ok { 0 } else { 1 }),
                },
            );
        }
    }

    /// `ActionStatusResponse`: its last `limit` lines; an unknown name is a finished, empty one.
    pub fn status(&self, name: &str, limit: usize) -> Value {
        let state = self
            .0
            .lock()
            .ok()
            .and_then(|actions| actions.get(name).cloned())
            .unwrap_or_default();
        let skip = state.lines.len().saturating_sub(limit);
        json!({
            "name": name,
            "running": state.running,
            "exit_code": state.exit_code,
            "pid": null,
            "lines": state.lines[skip..],
        })
    }
}

/// `GET /api/curator`.
pub fn curator_status(skills: &SkillStore) -> Value {
    let state = skills.curator_state();
    json!({
        "enabled": true,
        "paused": state.paused,
        "interval_hours": INTERVAL_HOURS,
        "last_run_at": state.last_run_at,
        "min_idle_hours": MIN_IDLE_HOURS,
        "stale_after_days": STALE_AFTER_DAYS,
        "archive_after_days": ARCHIVE_AFTER_DAYS,
    })
}

/// `PUT /api/curator/paused`.
pub fn set_curator_paused(skills: &SkillStore, paused: bool) -> Value {
    let mut state = skills.curator_state();
    state.paused = paused;
    skills.save_curator_state(&state);
    json!({"ok": true, "paused": paused})
}

/// The model the per-cluster curator (backend v2 Phase 8) asks, when the knobs say it runs
/// (`budget.curator_mode`: the `requests` profile's): the curator's own model if one is set in
/// `config.json` (`aux_models.curator`), else the background review's (Settings ▸ Model ▸
/// Auxiliary), else the main model. Its requests count as `curator` in the ledger and the governor
/// holds them back below 20% of the day's quota. `None` under the other profiles: today's curator
/// runs without a model here (the deterministic archive pass only).
pub fn curator_model(runtime: &Runtime, data_dir: &Path) -> Option<CuratorModel> {
    if runtime.budget().curator_mode != CuratorMode::PerCluster {
        return None;
    }
    let settings = Settings::load(data_dir);
    let chosen = settings
        .aux_models
        .curator
        .as_ref()
        .or(settings.aux_models.review.as_ref())
        .and_then(|choice| {
            let provider = settings.provider(&choice.provider)?;
            Some((provider, choice.model.clone(), choice.effort.clone()))
        });
    let (provider, model, effort) = match chosen {
        Some(chosen) => chosen,
        None => (settings.active()?, settings.model.clone()?, None),
    };
    if !provider.ready() || model.trim().is_empty() {
        return None;
    }
    Some(CuratorModel {
        provider: provider.runtime(),
        model,
        options: TurnOptions {
            context_length: provider.context_length,
            effort,
            ..Default::default()
        },
    })
}

/// Runs the curator in the background under the `curator-run` action; `false` when one runs.
pub fn run_curator(runtime: Arc<Runtime>, actions: Arc<Actions>, data_dir: PathBuf) -> bool {
    if !actions.start(CURATOR_RUN) {
        return false;
    }
    tokio::spawn(async move {
        let model = curator_model(&runtime, &data_dir);
        match runtime.run_curator(model).await {
            Ok(outcome) => {
                let mut lines: Vec<String> = outcome.summary.lines().map(str::to_string).collect();
                if let Some(report) = outcome.report {
                    lines.push(format!("Report: {}", report.display()));
                }
                actions.finish(CURATOR_RUN, lines, true);
            }
            Err(error) => actions.finish(CURATOR_RUN, vec![format!("{error:#}")], false),
        }
    });
    true
}

/// The scheduled curator (Hermes's `maybe_run_curator`): checked every ten minutes, it runs when
/// a week has passed, it isn't paused, and nothing has been used for two hours. It archives idle
/// skills; under the `requests` profile it also asks the model once per duplicate cluster or low
/// performer and stages what it proposes (`curator_model`); the other profiles keep archiving only.
pub fn schedule_curator(runtime: Arc<Runtime>, actions: Arc<Actions>, data_dir: PathBuf) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tick.tick().await;
            if runtime.curator_due() {
                run_curator(runtime.clone(), actions.clone(), data_dir.clone());
            }
        }
    });
}

/// `GET /api/memory`: built-in memory only (no external providers here); the files' sizes.
pub fn memory_status(data_dir: &Path) -> Value {
    let folder = data_dir.join("memories");
    let files: serde_json::Map<String, Value> = MEMORY_FILES
        .iter()
        .map(|(file, key)| {
            let size = std::fs::metadata(folder.join(file))
                .map(|m| m.len())
                .unwrap_or(0);
            (key.to_string(), json!(size))
        })
        .collect();
    json!({"active": "", "providers": [], "builtin_files": files})
}

/// `POST /api/memory/reset` `{target: all|memory|user}`: deletes the files and erases their
/// entries from `memory.db` (`MemoryStore::reset`; since Phase 9 deleting the files alone would
/// leave the text in the database). Blocking: call it off the async workers.
pub fn reset_memory(memory: &MemoryStore, body: &Value) -> (u16, Value) {
    let target = body["target"]
        .as_str()
        .unwrap_or("all")
        .trim()
        .to_lowercase();
    if !["all", "memory", "user"].contains(&target.as_str()) {
        return (
            400,
            json!({"detail": "target must be all, memory, or user"}),
        );
    }
    let mut deleted = Vec::new();
    for (which, key) in [(Target::Memory, "memory"), (Target::User, "user")] {
        if target != "all" && target != key {
            continue;
        }
        match memory.reset(which) {
            Ok(true) => deleted.push(which.file_name()),
            Ok(false) => {}
            Err(detail) => return (500, json!({"detail": detail})),
        }
    }
    (200, json!({"ok": true, "deleted": deleted}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_report_running_then_their_lines() {
        let actions = Actions::default();
        assert!(actions.start("a"));
        assert!(!actions.start("a"));
        assert_eq!(actions.status("a", 10)["running"], true);
        actions.finish("a", vec!["one".into(), "two".into()], true);
        let status = actions.status("a", 1);
        assert_eq!(status["running"], false);
        assert_eq!(status["exit_code"], 0);
        assert_eq!(status["lines"], json!(["two"]));
        assert_eq!(actions.status("unknown", 5)["lines"], json!([]));
    }

    #[test]
    fn memory_status_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("memories");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("MEMORY.md"), "abc").unwrap();
        std::fs::write(folder.join("USER.md"), "x").unwrap();
        assert_eq!(
            memory_status(dir.path())["builtin_files"],
            json!({"memory": 3, "user": 1})
        );
        let memory = MemoryStore::open(&folder, dir.path().join("memory.db"), Default::default());
        assert_eq!(memory.entries(Target::User), ["x"]);
        assert_eq!(reset_memory(&memory, &json!({"target": "nope"})).0, 400);
        let (status, body) = reset_memory(&memory, &json!({"target": "user"}));
        assert_eq!((status, body["deleted"].clone()), (200, json!(["USER.md"])));
        assert!(folder.join("MEMORY.md").exists());
        assert!(!folder.join("USER.md").exists());
        // Erased, not just hidden: no copy is left for a later access to restore.
        assert!(memory.entries(Target::User).is_empty());
        let (_, body) = reset_memory(&memory, &json!({"target": "all"}));
        assert_eq!(body["deleted"], json!(["MEMORY.md"]));
        let left: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".bak.dropped."))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn pausing_the_curator_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let skills = SkillStore::new(dir.path().join("skills"), Default::default());
        assert_eq!(curator_status(&skills)["paused"], false);
        set_curator_paused(&skills, true);
        assert_eq!(curator_status(&skills)["paused"], true);
        assert_eq!(curator_status(&skills)["interval_hours"], INTERVAL_HOURS);
    }
}
