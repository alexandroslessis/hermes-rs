//! Capabilities ▸ Skills: the installed skills, their on/off switches, and reading, creating
//! and editing them from the app.
//!
//! Hermes: `hermes_cli/web_routers/skills.py` (`/api/skills*`), `hermes_cli/skills_config.py`
//! (`skills.disabled`, kept in `desktop-config.json` under Hermes's own key). The skill hub
//! (`/api/skills/hub/*`, `tools/skills_hub*.py`) isn't ported: the gateway's routes answer with no
//! sources and no official catalog, and installing says so.
//!
//! Typed calls and [`SkillError`]; the page's routes and JSON are `crates/desktop/src/skills.rs`.

use std::collections::BTreeSet;

use hermes_tools::{SkillOp, SkillStore, SkillsConfig};
use serde_json::Value;

use crate::config::ConfigRecord;

const DISABLED_KEY: &str = "skills.disabled";

/// Why a skill call failed. The gateway answers 400 and 404 for these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillError {
    /// The request is wrong (a missing name, a refused write).
    Invalid(String),
    /// No such skill (or it has no SKILL.md).
    NotFound(String),
}

impl SkillError {
    pub fn message(&self) -> &str {
        match self {
            SkillError::Invalid(m) | SkillError::NotFound(m) => m,
        }
    }
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SkillError {}

/// The disabled skills as Settings stores them (`skills.disabled`).
fn disabled(config: &ConfigRecord) -> BTreeSet<String> {
    config
        .value(DISABLED_KEY)
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
pub fn apply_config(store: &SkillStore, config: &ConfigRecord) {
    store.set_config(SkillsConfig {
        disabled: disabled(config),
        ..store.config()
    });
}

/// One installed skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub enabled: bool,
    /// Views plus uses.
    pub usage: u64,
    /// `"bundled"`, `"external"` or `"agent"` (learned or hand-made: the ones the user may edit
    /// or delete). Hermes's order is hub > bundled > external > agent; nothing here is from the
    /// hub.
    pub provenance: &'static str,
}

/// Every installed skill, disabled ones included.
pub fn list(store: &SkillStore) -> Vec<SkillInfo> {
    let disabled = store.config().disabled;
    store
        .catalog_all()
        .into_iter()
        .map(|skill| {
            let (views, uses) = store.usage(&skill.name);
            let provenance = if store.is_bundled(&skill.dir) {
                "bundled"
            } else if skill.external {
                "external"
            } else {
                "agent"
            };
            SkillInfo {
                enabled: !disabled.contains(&skill.name),
                usage: views + uses,
                provenance,
                description: skill.description,
                category: skill.category.unwrap_or_default(),
                name: skill.name,
            }
        })
        .collect()
}

/// Switches a skill on or off and puts the change on the store.
pub fn toggle(
    store: &SkillStore,
    config: &ConfigRecord,
    name: &str,
    enabled: bool,
) -> Result<(), SkillError> {
    if name.is_empty() {
        return Err(SkillError::Invalid("name is required".into()));
    }
    let mut names = disabled(config);
    if enabled {
        names.remove(name);
    } else {
        names.insert(name.to_string());
    }
    config.set_value(DISABLED_KEY, serde_json::json!(names));
    apply_config(store, config);
    Ok(())
}

/// A skill's SKILL.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillContent {
    pub name: String,
    pub content: String,
    pub path: String,
}

/// Reads a skill's SKILL.md (a BOM stripped), disabled skills included.
pub fn content(store: &SkillStore, name: &str) -> Result<SkillContent, SkillError> {
    let Some(skill) = store.find_any(name) else {
        return Err(SkillError::NotFound(format!("Skill '{name}' not found.")));
    };
    let path = skill.skill_md();
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(SkillContent {
            name: name.to_string(),
            content: content.trim_start_matches('\u{feff}').to_string(),
            path: path.display().to_string(),
        }),
        Err(_) => Err(SkillError::NotFound(format!(
            "Skill '{name}' has no SKILL.md."
        ))),
    }
}

/// A new skill from the app, through `skill_manage`'s checks. Returns the store's result (a
/// `{success, ...}` tool result).
pub fn create(
    store: &SkillStore,
    name: &str,
    content: &str,
    category: Option<&str>,
) -> Result<Value, SkillError> {
    write(
        store,
        SkillOp {
            action: "create".into(),
            name: name.to_string(),
            content: Some(content.to_string()),
            category: category.filter(|c| !c.is_empty()).map(str::to_string),
            ..Default::default()
        },
    )
}

/// An edit of a skill's SKILL.md from the app, through `skill_manage`'s checks.
pub fn edit(store: &SkillStore, name: &str, content: &str) -> Result<Value, SkillError> {
    write(
        store,
        SkillOp {
            action: "edit".into(),
            name: name.to_string(),
            content: Some(content.to_string()),
            ..Default::default()
        },
    )
}

fn write(store: &SkillStore, op: SkillOp) -> Result<Value, SkillError> {
    let result = store.user_write(op);
    if result["success"] == true {
        return Ok(result);
    }
    let error = result["error"]
        .as_str()
        .unwrap_or("The skill couldn't be saved.");
    if error.to_lowercase().contains("not found") {
        Err(SkillError::NotFound(error.to_string()))
    } else {
        Err(SkillError::Invalid(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str =
        "---\nname: deploy\ndescription: Ships the app.\n---\n\n# Deploy\n\nSteps.\n";

    fn setup() -> (hermes_testdir::TestDir, SkillStore, ConfigRecord) {
        let dir = hermes_testdir::tempdir().unwrap();
        let store = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        let config = ConfigRecord::load(dir.path());
        (dir, store, config)
    }

    #[test]
    fn create_list_disable_edit_and_read_back() {
        let (dir, store, config) = setup();
        create(&store, "deploy", SKILL, Some("devops")).unwrap();
        let listed = list(&store);
        assert_eq!(listed[0].name, "deploy");
        assert!(listed[0].enabled);
        assert_eq!(listed[0].provenance, "agent");
        assert_eq!(listed[0].category, "devops");

        // Off: gone for the agent, still listed, still editable; kept across a restart.
        toggle(&store, &config, "deploy", false).unwrap();
        assert!(store.find("deploy").is_none());
        assert!(!list(&store)[0].enabled);
        let edited = SKILL.replace("Steps.", "New steps.");
        edit(&store, "deploy", &edited).unwrap();
        assert!(
            content(&store, "deploy")
                .unwrap()
                .content
                .contains("New steps.")
        );
        let reopened = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        apply_config(&reopened, &ConfigRecord::load(dir.path()));
        assert!(reopened.find("deploy").is_none());

        // A disabled skill's name stays taken.
        assert!(matches!(
            create(&store, "deploy", SKILL, None),
            Err(SkillError::Invalid(_))
        ));

        toggle(&store, &config, "deploy", true).unwrap();
        assert!(store.find("deploy").is_some());
    }

    #[test]
    fn unknown_skills_and_empty_names_are_refused() {
        let (_dir, store, config) = setup();
        assert!(matches!(
            content(&store, "nope"),
            Err(SkillError::NotFound(_))
        ));
        assert!(matches!(
            edit(&store, "nope", SKILL),
            Err(SkillError::NotFound(_))
        ));
        assert_eq!(
            toggle(&store, &config, "", true),
            Err(SkillError::Invalid("name is required".into()))
        );
    }
}
