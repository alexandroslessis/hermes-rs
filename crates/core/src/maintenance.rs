//! Command Center ▸ Maintenance: the skills curator (status, pause, run now, and its schedule) and
//! the built-in memory files (sizes, reset), with the action status the page follows a run by.
//!
//! Hermes: `hermes_cli/web_routers/status.py` (`/api/curator*`, `/api/actions/{name}`),
//! `hermes_cli/web_routers/ops.py` (`/api/memory`, `/api/memory/reset`), `agent/curator.py`
//! (`maybe_run_curator`: the schedule).
//!
//! A curator run reports through [`CoreEvent::CuratorStarted`] / [`CoreEvent::CuratorFinished`]
//! (it belongs to no session) and through [`Actions`], which the page polls by name. The page's
//! JSON is `crates/desktop/src/maintenance.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hermes_memory::{MemoryStore, Target};
use hermes_runtime::budget::CuratorMode;
use hermes_runtime::{CuratorModel, Runtime, TurnOptions};
use hermes_tools::SkillStore;
use hermes_tools::curator::{ARCHIVE_AFTER_DAYS, INTERVAL_HOURS, MIN_IDLE_HOURS, STALE_AFTER_DAYS};

use crate::events::{CoreEvent, EventBus};
use crate::providers::Settings;

/// The curator's action name, as Hermes's (`curator-run`).
pub const CURATOR_RUN: &str = "curator-run";

/// The built-in memory files and the key each is reported and reset by.
pub const MEMORY_FILES: [(&str, &str); 2] = [("MEMORY.md", "memory"), ("USER.md", "user")];

#[derive(Clone, Default)]
struct ActionState {
    running: bool,
    lines: Vec<String>,
    exit_code: Option<i32>,
}

/// A background action's state (`ActionStatusResponse`, without the page's field names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStatus {
    pub name: String,
    pub running: bool,
    /// 0 after a good run, 1 after a failed one; `None` while it has never finished.
    pub exit_code: Option<i32>,
    /// Its last lines.
    pub lines: Vec<String>,
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

    /// Its last `limit` lines; an unknown name is a finished, empty one.
    pub fn status(&self, name: &str, limit: usize) -> ActionStatus {
        let state = self
            .0
            .lock()
            .ok()
            .and_then(|actions| actions.get(name).cloned())
            .unwrap_or_default();
        let skip = state.lines.len().saturating_sub(limit);
        ActionStatus {
            name: name.to_string(),
            running: state.running,
            exit_code: state.exit_code,
            lines: state.lines[skip..].to_vec(),
        }
    }
}

/// The curator's switches and schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct CuratorStatus {
    pub paused: bool,
    pub interval_hours: i64,
    pub last_run_at: Option<String>,
    pub min_idle_hours: i64,
    pub stale_after_days: i64,
    pub archive_after_days: i64,
}

/// The curator's state.
pub fn curator_status(skills: &SkillStore) -> CuratorStatus {
    let state = skills.curator_state();
    CuratorStatus {
        paused: state.paused,
        interval_hours: INTERVAL_HOURS,
        last_run_at: state.last_run_at,
        min_idle_hours: MIN_IDLE_HOURS,
        stale_after_days: STALE_AFTER_DAYS,
        archive_after_days: ARCHIVE_AFTER_DAYS,
    }
}

/// Pauses or resumes the scheduled curator.
pub fn set_curator_paused(skills: &SkillStore, paused: bool) {
    let mut state = skills.curator_state();
    state.paused = paused;
    skills.save_curator_state(&state);
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

/// Runs the curator in the background under the `curator-run` action, publishing
/// `CuratorStarted` and `CuratorFinished`; `false` when one runs.
pub fn run_curator(
    handle: &tokio::runtime::Handle,
    runtime: Arc<Runtime>,
    actions: Arc<Actions>,
    events: Arc<EventBus>,
    data_dir: PathBuf,
) -> bool {
    if !actions.start(CURATOR_RUN) {
        return false;
    }
    events.publish(CoreEvent::CuratorStarted);
    let spawner = handle.clone();
    handle.spawn(async move {
        // The run in a task of its own, so a panic in it still ends the action and pairs
        // `CuratorStarted` with a `CuratorFinished` (else "running" would stay forever and no
        // run could start again until a restart).
        let run = spawner.spawn(async move {
            let model = curator_model(&runtime, &data_dir);
            match runtime.run_curator(model).await {
                Ok(outcome) => {
                    let mut lines: Vec<String> =
                        outcome.summary.lines().map(str::to_string).collect();
                    if let Some(report) = outcome.report {
                        lines.push(format!("Report: {}", report.display()));
                    }
                    (true, lines)
                }
                Err(error) => (false, vec![format!("{error:#}")]),
            }
        });
        let (ok, lines) = match run.await {
            Ok(done) => done,
            Err(error) => (false, vec![format!("The curator stopped: {error}")]),
        };
        actions.finish(CURATOR_RUN, lines.clone(), ok);
        events.publish(CoreEvent::CuratorFinished { ok, lines });
    });
    true
}

/// The scheduled curator (Hermes's `maybe_run_curator`): checked every ten minutes, it runs when
/// a week has passed, it isn't paused, and nothing has been used for two hours. It archives idle
/// skills; under the `requests` profile it also asks the model once per duplicate cluster or low
/// performer and stages what it proposes (`curator_model`); the other profiles keep archiving only.
pub fn schedule_curator(
    handle: &tokio::runtime::Handle,
    runtime: Arc<Runtime>,
    actions: Arc<Actions>,
    events: Arc<EventBus>,
    data_dir: PathBuf,
) {
    let spawner = handle.clone();
    handle.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tick.tick().await;
            if runtime.curator_due() {
                run_curator(
                    &spawner,
                    runtime.clone(),
                    actions.clone(),
                    events.clone(),
                    data_dir.clone(),
                );
            }
        }
    });
}

/// The built-in memory files' sizes in bytes (a missing file is 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryFiles {
    pub memory: u64,
    pub user: u64,
}

/// Built-in memory only (no external providers here): the files' sizes.
pub fn memory_status(data_dir: &Path) -> MemoryFiles {
    let folder = data_dir.join("memories");
    let size = |file: &str| {
        std::fs::metadata(folder.join(file))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    MemoryFiles {
        memory: size(MEMORY_FILES[0].0),
        user: size(MEMORY_FILES[1].0),
    }
}

/// Which memory files to reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetTarget {
    All,
    Memory,
    User,
}

impl ResetTarget {
    /// `all`, `memory` or `user` (any case, trimmed); `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "all" => Some(ResetTarget::All),
            "memory" => Some(ResetTarget::Memory),
            "user" => Some(ResetTarget::User),
            _ => None,
        }
    }
}

/// Deletes the files and erases their entries from `memory.db` (`MemoryStore::reset`; since
/// Phase 9 deleting the files alone would leave the text in the database); returns the names of
/// the files that existed. Blocking: call it off the async workers. The error is the store's.
pub fn reset_memory(
    memory: &MemoryStore,
    target: ResetTarget,
) -> Result<Vec<&'static str>, String> {
    let mut deleted = Vec::new();
    for (which, wanted) in [
        (Target::Memory, ResetTarget::Memory),
        (Target::User, ResetTarget::User),
    ] {
        if target != ResetTarget::All && target != wanted {
            continue;
        }
        if memory.reset(which)? {
            deleted.push(which.file_name());
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_report_running_then_their_lines() {
        let actions = Actions::default();
        assert!(actions.start("a"));
        assert!(!actions.start("a"));
        assert!(actions.status("a", 10).running);
        actions.finish("a", vec!["one".into(), "two".into()], true);
        let status = actions.status("a", 1);
        assert!(!status.running);
        assert_eq!(status.exit_code, Some(0));
        assert_eq!(status.lines, ["two"]);
        assert!(actions.status("unknown", 5).lines.is_empty());
        actions.finish("b", vec![], false);
        assert_eq!(actions.status("b", 5).exit_code, Some(1));
    }

    #[test]
    fn memory_status_and_reset() {
        let dir = hermes_testdir::tempdir().unwrap();
        let folder = dir.path().join("memories");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("MEMORY.md"), "abc").unwrap();
        std::fs::write(folder.join("USER.md"), "x").unwrap();
        assert_eq!(
            memory_status(dir.path()),
            MemoryFiles { memory: 3, user: 1 }
        );
        let memory = MemoryStore::open(&folder, dir.path().join("memory.db"), Default::default());
        assert_eq!(memory.entries(Target::User), ["x"]);
        assert_eq!(ResetTarget::parse("nope"), None);
        assert_eq!(ResetTarget::parse(" User "), Some(ResetTarget::User));
        assert_eq!(
            reset_memory(&memory, ResetTarget::User).unwrap(),
            ["USER.md"]
        );
        assert!(folder.join("MEMORY.md").exists());
        assert!(!folder.join("USER.md").exists());
        // Erased, not just hidden: no copy is left for a later access to restore.
        assert!(memory.entries(Target::User).is_empty());
        assert_eq!(
            reset_memory(&memory, ResetTarget::All).unwrap(),
            ["MEMORY.md"]
        );
        let left: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".bak.dropped."))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn pausing_the_curator_is_kept() {
        let dir = hermes_testdir::tempdir().unwrap();
        let skills = SkillStore::new(dir.path().join("skills"), Default::default());
        assert!(!curator_status(&skills).paused);
        set_curator_paused(&skills, true);
        let status = curator_status(&skills);
        assert!(status.paused);
        assert_eq!(status.interval_hours, INTERVAL_HOURS);
    }
}
