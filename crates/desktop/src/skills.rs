//! Capabilities ▸ Skills: the installed skills, their on/off switches, and reading, creating
//! and editing them from the app.
//!
//! Hermes: `hermes_cli/web_routers/skills.py` (`/api/skills*`), `hermes_cli/skills_config.py`
//! (`skills.disabled`, kept in `desktop-config.json` under Hermes's own key). The skill hub
//! (`/api/skills/hub/*`, `tools/skills_hub*.py`) isn't ported: its routes answer with no sources
//! and no official catalog, and installing says so.

use std::collections::BTreeSet;

use hermes_tools::{SkillOp, SkillStore, SkillsConfig};
use serde_json::{Value, json};

use crate::rest::{Rest, query_param};

const DISABLED_KEY: &str = "skills.disabled";

/// The disabled skills as Settings stores them (`skills.disabled`).
fn disabled(rest: &Rest) -> BTreeSet<String> {
    rest.config_value(DISABLED_KEY)
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Puts the stored switches on the runtime's skill store (at startup and after a toggle).
pub fn apply_config(store: &SkillStore, rest: &Rest) {
    store.set_config(SkillsConfig {
        disabled: disabled(rest),
        ..store.config()
    });
}

fn not_found(name: &str) -> (u16, Value) {
    (404, json!({"detail": format!("Skill '{name}' not found.")}))
}

/// `(status, body)` for an `/api/skills*` route, or `None` for Hermes's 404.
pub fn route(
    store: &SkillStore,
    rest: &Rest,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
) -> Option<(u16, Value)> {
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let text = |key: &str| body[key].as_str().unwrap_or_default().trim().to_string();
    match (method, path) {
        ("GET", "/api/skills") => Some((200, list(store))),
        ("PUT", "/api/skills/toggle") => {
            let name = text("name");
            if name.is_empty() {
                return Some((400, json!({"detail": "name is required"})));
            }
            let enabled = body["enabled"].as_bool().unwrap_or(true);
            let mut names = disabled(rest);
            if enabled {
                names.remove(&name);
            } else {
                names.insert(name.clone());
            }
            rest.set_config_value(DISABLED_KEY, json!(names));
            apply_config(store, rest);
            Some((200, json!({"ok": true, "name": name, "enabled": enabled})))
        }
        ("GET", "/api/skills/content") => {
            let name = query_param(query, "name").unwrap_or_default();
            let Some(skill) = store.find_any(&name) else {
                return Some(not_found(&name));
            };
            let path = skill.skill_md();
            Some(match std::fs::read_to_string(&path) {
                Ok(content) => (
                    200,
                    json!({
                        "name": name,
                        "content": content.trim_start_matches('\u{feff}'),
                        "path": path.display().to_string(),
                    }),
                ),
                Err(_) => (
                    404,
                    json!({"detail": format!("Skill '{name}' has no SKILL.md.")}),
                ),
            })
        }
        ("POST", "/api/skills") => Some(write(
            store,
            SkillOp {
                action: "create".into(),
                name: text("name"),
                content: Some(body["content"].as_str().unwrap_or_default().to_string()),
                category: Some(text("category")).filter(|c| !c.is_empty()),
                ..Default::default()
            },
        )),
        ("PUT", "/api/skills/content") => Some(write(
            store,
            SkillOp {
                action: "edit".into(),
                name: text("name"),
                content: Some(body["content"].as_str().unwrap_or_default().to_string()),
                ..Default::default()
            },
        )),
        _ if path.starts_with("/api/skills/hub/") => hub(method, path),
        _ => None,
    }
}

/// Every installed skill, disabled ones included, as Hermes's `GET /api/skills` lists them.
fn list(store: &SkillStore) -> Value {
    let disabled = store.config().disabled;
    let skills: Vec<Value> = store
        .catalog_all()
        .into_iter()
        .map(|skill| {
            let (views, uses) = store.usage(&skill.name);
            // Hermes's provenance: hub > bundled > external > agent (learned or hand-made, the
            // ones the user may edit or delete). Nothing here comes from the hub.
            let provenance = if store.is_bundled(&skill.dir) {
                "bundled"
            } else if skill.external {
                "external"
            } else {
                "agent"
            };
            json!({
                "name": skill.name,
                "description": skill.description,
                "category": skill.category.unwrap_or_default(),
                "enabled": !disabled.contains(&skill.name),
                "usage": views + uses,
                "provenance": provenance,
            })
        })
        .collect();
    json!(skills)
}

/// A create or edit from the app, through `skill_manage`'s checks.
fn write(store: &SkillStore, op: SkillOp) -> (u16, Value) {
    let result = store.user_write(op);
    if result["success"] == true {
        return (200, result);
    }
    let error = result["error"]
        .as_str()
        .unwrap_or("The skill couldn't be saved.");
    let status = if error.to_lowercase().contains("not found") {
        404
    } else {
        400
    };
    (status, json!({"detail": error}))
}

/// The skill hub, not ported: no sources, an empty official catalog, nothing to install.
fn hub(method: &str, path: &str) -> Option<(u16, Value)> {
    Some(match (method, path) {
        ("GET", "/api/skills/hub/official") => (200, json!({"skills": []})),
        ("GET", "/api/skills/hub/sources") => (
            200,
            json!({"sources": [], "index_available": false, "featured": [], "installed": {}}),
        ),
        ("GET", "/api/skills/hub/search") => (
            200,
            json!({"results": [], "source_counts": {}, "timed_out": [], "installed": {}}),
        ),
        (
            "POST",
            "/api/skills/hub/install" | "/api/skills/hub/uninstall" | "/api/skills/hub/update",
        ) => (
            501,
            json!({"detail": "Installing skills from the hub isn't available in hermes-rs yet."}),
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str =
        "---\nname: deploy\ndescription: Ships the app.\n---\n\n# Deploy\n\nSteps.\n";

    fn setup() -> (tempfile::TempDir, SkillStore, Rest) {
        let dir = tempfile::tempdir().unwrap();
        let store = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        let rest = Rest::new(dir.path().to_path_buf());
        (dir, store, rest)
    }

    fn call(
        store: &SkillStore,
        rest: &Rest,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: Value,
    ) -> (u16, Value) {
        route(
            store,
            rest,
            method,
            path,
            query,
            body.to_string().as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn create_list_disable_edit_and_read_back() {
        let (dir, store, rest) = setup();
        let (status, _) = call(
            &store,
            &rest,
            "POST",
            "/api/skills",
            None,
            json!({"name": "deploy", "content": SKILL, "category": "devops"}),
        );
        assert_eq!(status, 200);
        let (_, listed) = call(&store, &rest, "GET", "/api/skills", None, json!(null));
        assert_eq!(listed[0]["name"], "deploy");
        assert_eq!(listed[0]["enabled"], true);
        assert_eq!(listed[0]["provenance"], "agent");

        // Off: gone for the agent, still listed, still editable; kept across a restart.
        call(
            &store,
            &rest,
            "PUT",
            "/api/skills/toggle",
            None,
            json!({"name": "deploy", "enabled": false}),
        );
        assert!(store.find("deploy").is_none());
        let (_, listed) = call(&store, &rest, "GET", "/api/skills", None, json!(null));
        assert_eq!(listed[0]["enabled"], false);
        let edited = SKILL.replace("Steps.", "New steps.");
        let (status, _) = call(
            &store,
            &rest,
            "PUT",
            "/api/skills/content",
            None,
            json!({"name": "deploy", "content": edited}),
        );
        assert_eq!(status, 200);
        let (_, read) = call(
            &store,
            &rest,
            "GET",
            "/api/skills/content",
            Some("name=deploy"),
            json!(null),
        );
        assert!(read["content"].as_str().unwrap().contains("New steps."));
        let reopened = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        apply_config(&reopened, &Rest::new(dir.path().to_path_buf()));
        assert!(reopened.find("deploy").is_none());

        // A disabled skill's name stays taken.
        let (status, _) = call(
            &store,
            &rest,
            "POST",
            "/api/skills",
            None,
            json!({"name": "deploy", "content": SKILL}),
        );
        assert_eq!(status, 400);

        call(
            &store,
            &rest,
            "PUT",
            "/api/skills/toggle",
            None,
            json!({"name": "deploy", "enabled": true}),
        );
        assert!(store.find("deploy").is_some());
    }

    #[test]
    fn unknown_skills_and_the_hub_answer_plainly() {
        let (_dir, store, rest) = setup();
        let (status, _) = call(
            &store,
            &rest,
            "GET",
            "/api/skills/content",
            Some("name=nope"),
            json!(null),
        );
        assert_eq!(status, 404);
        let (status, official) = call(
            &store,
            &rest,
            "GET",
            "/api/skills/hub/official",
            None,
            json!(null),
        );
        assert_eq!(
            (status, official["skills"].as_array().unwrap().len()),
            (200, 0)
        );
        let (status, _) = call(
            &store,
            &rest,
            "POST",
            "/api/skills/hub/install",
            None,
            json!({"identifier": "official/x/y"}),
        );
        assert_eq!(status, 501);
    }
}
