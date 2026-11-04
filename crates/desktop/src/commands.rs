//! The composer's `/` menu: Hermes asks for the catalog (`commands.catalog`), completions as the
//! user types (`complete.slash`), and runs the pick (`slash.exec`, then `command.dispatch`).
//! This backend's slash commands are the runtime's skills and bundles: running one answers
//! Hermes's `skill` dispatch, whose message (the loaded skill) the page then sends as the prompt.
//! Hermes's built-in commands the desktop runs itself (`/new`, `/model`, …) never reach here.
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.slash`), `tui_gateway/command_discovery.py`.

use hermes_runtime::Runtime;
use hermes_tools::SkillStore;
use serde_json::{Value, json};

/// A skill or bundle the `/` menu offers: `(name, description, origin)`.
fn entries(skills: &SkillStore) -> Vec<(String, String, &'static str)> {
    let mut out: Vec<_> = skills
        .bundles()
        .into_iter()
        .map(|bundle| (bundle.slug, bundle.description, "bundle"))
        .collect();
    out.extend(skills.catalog().into_iter().map(|skill| {
        let origin = if skill.external { "external" } else { "local" };
        (skill.name, skill.description, origin)
    }));
    out
}

/// `commands.catalog`: no registry commands of our own, the skills with their use counts.
pub fn catalog(skills: &SkillStore) -> Value {
    let entries = entries(skills);
    let listed: serde_json::Map<String, Value> = entries
        .iter()
        .map(|(name, _, origin)| {
            let (_, uses) = skills.usage(name);
            (name.clone(), json!({"usage": uses, "origin": origin}))
        })
        .collect();
    // The bare `/` list: every skill and bundle as `[command, description]`.
    let pairs: Vec<[String; 2]> = entries
        .iter()
        .map(|(name, description, _)| [format!("/{name}"), description.clone()])
        .collect();
    json!({
        "pairs": pairs,
        "sub": {},
        "canon": {},
        "commands": {},
        "categories": [],
        "skill_count": listed.len(),
        "skills": listed,
    })
}

/// `complete.slash`: the skills whose name starts with (else contains) what follows the `/`.
/// Arguments after the name get no completions.
pub fn complete(skills: &SkillStore, text: &str) -> Value {
    let Some(query) = text.strip_prefix('/') else {
        return json!({"items": [], "replace_from": 1});
    };
    if query.contains(char::is_whitespace) {
        return json!({"items": [], "replace_from": 1});
    }
    let query = query.to_lowercase();
    let mut matches: Vec<_> = entries(skills)
        .into_iter()
        .filter_map(|(name, description, _)| {
            let lower = name.to_lowercase();
            let rank = if lower.starts_with(&query) {
                0
            } else if lower.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, name, description))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let items: Vec<Value> = matches
        .into_iter()
        .map(|(_, name, description)| {
            json!({
                "text": format!("/{name}"),
                "display": format!("/{name}"),
                "meta": description,
                "kind": "skill",
            })
        })
        .collect();
    json!({"items": items, "replace_from": 1})
}

/// Runs `/name argument`: Hermes's `skill` dispatch for a skill or bundle, `None` for anything
/// else (the page reports it as not a command here).
pub fn dispatch(runtime: &Runtime, name: &str, argument: &str) -> Option<Value> {
    let name = name.trim_start_matches('/');
    let invocation = format!("/{name} {argument}").trim_end().to_string();
    let message = runtime.skill_command(&invocation)?;
    Some(json!({
        "type": "skill",
        "name": name,
        "message": message,
        // What the chat shows for the (long) loaded skill.
        "display": invocation,
    }))
}

/// `slash.exec`'s `command` (`name argument`, without the slash).
pub fn exec(runtime: &Runtime, command: &str) -> Option<Value> {
    let command = command.trim().trim_start_matches('/');
    let (name, argument) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    dispatch(runtime, name, argument.trim())
}

/// The command a stored message was typed as (`/name instruction`) when it loads a skill or a
/// bundle; `None` for an ordinary message. The transcript and titles show the command.
pub fn typed_command(text: &str) -> Option<String> {
    let (command, argument) = hermes_tools::describe_bundle_invocation(text)
        .or_else(|| hermes_tools::skills::describe_invocation(text))?;
    Some(format!("/{command} {argument}").trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(skills: &[(&str, &str)]) -> (SkillStore, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("hermes-desktop-skills-{}", uuid::Uuid::new_v4()));
        for (name, description) in skills {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: {description}\n---\n\nDo the {name} thing.\n"
                ),
            )
            .unwrap();
        }
        (SkillStore::new(root.clone(), Default::default()), root)
    }

    #[test]
    fn completions_rank_prefix_matches_first() {
        let (store, root) = store_with(&[
            ("plan", "Plan work"),
            ("explain-plan", "Explain"),
            ("review", "Review"),
        ]);
        let result = complete(&store, "/pla");
        let texts: Vec<&str> = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["/plan", "/explain-plan"]);
        assert_eq!(result["items"][0]["kind"], "skill");
        assert_eq!(result["items"][0]["meta"], "Plan work");
        // Past the name there's nothing to complete.
        assert!(
            complete(&store, "/plan now").as_object().unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(catalog(&store)["skill_count"], 3);
        assert_eq!(catalog(&store)["pairs"][0][0], "/explain-plan");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_loaded_skill_reads_back_as_its_command() {
        let (store, root) = store_with(&[("plan", "Plan work")]);
        let message = store.invocation_message("plan", "the release").unwrap();
        assert_eq!(
            typed_command(&message).as_deref(),
            Some("/plan the release")
        );
        assert_eq!(typed_command("just a message"), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
