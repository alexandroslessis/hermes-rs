//! Capabilities ▸ Skills as Hermes's routes answer them (`/api/skills*`): the page's JSON, the
//! `{detail}` errors and the skill hub's "nothing here" answers. The logic (the list, the switches
//! kept in the config record, reading and writing a skill) is `hermes_core::skills`.
//!
//! Hermes: `hermes_cli/web_routers/skills.py` (`/api/skills*`), `hermes_cli/skills_config.py`
//! (`skills.disabled`). The skill hub (`/api/skills/hub/*`, `tools/skills_hub*.py`) isn't ported:
//! its routes answer with no sources and no official catalog, and installing says so.

use hermes_core::skills::{self, SkillError};
use hermes_tools::SkillStore;
use serde_json::{Value, json};

use crate::rest::{Rest, query_param};

/// An error as the page reads it: 400 or 404 and `{detail}`.
fn failure(error: SkillError) -> (u16, Value) {
    let status = match error {
        SkillError::Invalid(_) => 400,
        SkillError::NotFound(_) => 404,
    };
    (status, json!({"detail": error.message()}))
}

/// A skill write's answer: the store's own result, or the error.
fn written(result: Result<Value, SkillError>) -> (u16, Value) {
    match result {
        Ok(result) => (200, result),
        Err(error) => failure(error),
    }
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
            let enabled = body["enabled"].as_bool().unwrap_or(true);
            Some(
                match skills::toggle(store, rest.config_record(), &name, enabled) {
                    Ok(()) => (200, json!({"ok": true, "name": name, "enabled": enabled})),
                    Err(error) => failure(error),
                },
            )
        }
        ("GET", "/api/skills/content") => {
            let name = query_param(query, "name").unwrap_or_default();
            Some(match skills::content(store, &name) {
                Ok(skill) => (
                    200,
                    json!({"name": skill.name, "content": skill.content, "path": skill.path}),
                ),
                Err(error) => failure(error),
            })
        }
        ("POST", "/api/skills") => Some(written(skills::create(
            store,
            &text("name"),
            body["content"].as_str().unwrap_or_default(),
            Some(text("category").as_str()),
        ))),
        ("PUT", "/api/skills/content") => Some(written(skills::edit(
            store,
            &text("name"),
            body["content"].as_str().unwrap_or_default(),
        ))),
        _ if path.starts_with("/api/skills/hub/") => hub(method, path),
        _ => None,
    }
}

/// Every installed skill, disabled ones included, as Hermes's `GET /api/skills` lists them.
fn list(store: &SkillStore) -> Value {
    json!(
        skills::list(store)
            .into_iter()
            .map(|skill| json!({
                "name": skill.name,
                "description": skill.description,
                "category": skill.category,
                "enabled": skill.enabled,
                "usage": skill.usage,
                "provenance": skill.provenance,
            }))
            .collect::<Vec<_>>()
    )
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
    use hermes_tools::SkillsConfig;

    const SKILL: &str =
        "---\nname: deploy\ndescription: Ships the app.\n---\n\n# Deploy\n\nSteps.\n";

    fn setup() -> (hermes_testdir::TestDir, SkillStore, Rest) {
        let dir = hermes_testdir::tempdir().unwrap();
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
    fn the_routes_answer_with_hermes_shapes() {
        let (_dir, store, rest) = setup();
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
        assert_eq!(listed[0]["category"], "devops");
        assert!(listed[0]["usage"].is_u64());

        let (status, toggled) = call(
            &store,
            &rest,
            "PUT",
            "/api/skills/toggle",
            None,
            json!({"name": "deploy", "enabled": false}),
        );
        assert_eq!(
            (status, toggled),
            (200, json!({"ok": true, "name": "deploy", "enabled": false}))
        );
        assert!(store.find("deploy").is_none());
        let (status, body) = call(&store, &rest, "PUT", "/api/skills/toggle", None, json!({}));
        assert_eq!((status, body), (400, json!({"detail": "name is required"})));

        let (status, read) = call(
            &store,
            &rest,
            "GET",
            "/api/skills/content",
            Some("name=deploy"),
            json!(null),
        );
        assert_eq!(status, 200);
        assert_eq!(read["name"], "deploy");
        assert!(read["path"].as_str().unwrap().ends_with("SKILL.md"));
        // A disabled skill's name stays taken: a refused write is a 400 with {detail}.
        let (status, body) = call(
            &store,
            &rest,
            "POST",
            "/api/skills",
            None,
            json!({"name": "deploy", "content": SKILL}),
        );
        assert_eq!(status, 400);
        assert!(body["detail"].is_string());
    }

    #[test]
    fn unknown_skills_and_the_hub_answer_plainly() {
        let (_dir, store, rest) = setup();
        let (status, body) = call(
            &store,
            &rest,
            "GET",
            "/api/skills/content",
            Some("name=nope"),
            json!(null),
        );
        assert_eq!(
            (status, body),
            (404, json!({"detail": "Skill 'nope' not found."}))
        );
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
