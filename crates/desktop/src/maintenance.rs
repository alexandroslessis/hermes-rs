//! Command Center ▸ Maintenance as Hermes's routes answer it: the curator (`/api/curator*`), the
//! action status the page follows a run by (`/api/actions/{name}`) and the built-in memory files
//! (`/api/memory`, `/api/memory/reset`). The logic (the curator's run and schedule, the memory
//! files) is `hermes_core::maintenance`.
//!
//! Hermes: `hermes_cli/web_routers/status.py` (`/api/curator*`, `/api/actions/{name}`),
//! `hermes_cli/web_routers/ops.py` (`/api/memory`, `/api/memory/reset`).

use std::path::Path;

use hermes_core::maintenance::{self, ActionStatus, ResetTarget};
use hermes_memory::MemoryStore;
use hermes_tools::SkillStore;
use serde_json::{Value, json};

pub use hermes_core::maintenance::{Actions, CURATOR_RUN};

/// `ActionStatusResponse`.
pub fn action_status(status: &ActionStatus) -> Value {
    json!({
        "name": status.name,
        "running": status.running,
        "exit_code": status.exit_code,
        "pid": null,
        "lines": status.lines,
    })
}

/// `GET /api/curator`.
pub fn curator_status(skills: &SkillStore) -> Value {
    let status = maintenance::curator_status(skills);
    json!({
        "enabled": true,
        "paused": status.paused,
        "interval_hours": status.interval_hours,
        "last_run_at": status.last_run_at,
        "min_idle_hours": status.min_idle_hours,
        "stale_after_days": status.stale_after_days,
        "archive_after_days": status.archive_after_days,
    })
}

/// `PUT /api/curator/paused`.
pub fn set_curator_paused(skills: &SkillStore, paused: bool) -> Value {
    maintenance::set_curator_paused(skills, paused);
    json!({"ok": true, "paused": paused})
}

/// `GET /api/memory`: built-in memory only (no external providers here); the files' sizes.
pub fn memory_status(data_dir: &Path) -> Value {
    let files = maintenance::memory_status(data_dir);
    json!({
        "active": "",
        "providers": [],
        "builtin_files": {"memory": files.memory, "user": files.user},
    })
}

/// `POST /api/memory/reset` `{target: all|memory|user}`. Blocking: call it off the async workers.
pub fn reset_memory(memory: &MemoryStore, body: &Value) -> (u16, Value) {
    let target = body["target"].as_str().unwrap_or("all");
    let Some(target) = ResetTarget::parse(target) else {
        return (
            400,
            json!({"detail": "target must be all, memory, or user"}),
        );
    };
    match maintenance::reset_memory(memory, target) {
        Ok(deleted) => (200, json!({"ok": true, "deleted": deleted})),
        Err(detail) => (500, json!({"detail": detail})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_memory::Target;

    #[test]
    fn the_action_status_has_hermes_fields() {
        let actions = Actions::default();
        assert!(actions.start("a"));
        assert_eq!(action_status(&actions.status("a", 10))["running"], true);
        actions.finish("a", vec!["one".into(), "two".into()], true);
        assert_eq!(
            action_status(&actions.status("a", 1)),
            json!({"name": "a", "running": false, "exit_code": 0, "pid": null, "lines": ["two"]})
        );
        assert_eq!(
            action_status(&actions.status("unknown", 5))["lines"],
            json!([])
        );
    }

    #[test]
    fn memory_status_and_reset_answer_with_hermes_shapes() {
        let dir = hermes_testdir::tempdir().unwrap();
        let folder = dir.path().join("memories");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("MEMORY.md"), "abc").unwrap();
        std::fs::write(folder.join("USER.md"), "x").unwrap();
        assert_eq!(
            memory_status(dir.path()),
            json!({"active": "", "providers": [], "builtin_files": {"memory": 3, "user": 1}})
        );
        let memory = MemoryStore::open(&folder, dir.path().join("memory.db"), Default::default());
        assert_eq!(memory.entries(Target::User), ["x"]);
        assert_eq!(reset_memory(&memory, &json!({"target": "nope"})).0, 400);
        let (status, body) = reset_memory(&memory, &json!({"target": "user"}));
        assert_eq!((status, body["deleted"].clone()), (200, json!(["USER.md"])));
        let (_, body) = reset_memory(&memory, &json!({}));
        assert_eq!(body["deleted"], json!(["MEMORY.md"]));
    }

    #[test]
    fn the_curator_status_has_hermes_fields() {
        let dir = hermes_testdir::tempdir().unwrap();
        let skills = SkillStore::new(dir.path().join("skills"), Default::default());
        assert_eq!(curator_status(&skills)["paused"], false);
        assert_eq!(
            set_curator_paused(&skills, true),
            json!({"ok": true, "paused": true})
        );
        let status = curator_status(&skills);
        assert_eq!(status["paused"], true);
        assert_eq!(status["enabled"], true);
        assert!(status["interval_hours"].is_i64());
    }
}
