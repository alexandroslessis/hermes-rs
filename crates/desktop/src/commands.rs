//! The composer's `/` menu: Hermes asks for the catalog (`commands.catalog`), completions as the
//! user types (`complete.slash`), and runs the pick (`slash.exec`, then `command.dispatch`).
//! This backend's slash commands are the coding commands (`/plan`, `/init`, `/review`, `/learn`)
//! and the runtime's skills and bundles: running a skill answers
//! Hermes's `skill` dispatch, whose message (the loaded skill) the page then sends as the prompt.
//! Hermes's built-in commands the desktop runs itself (`/new`, `/model`, …) never reach here.
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.slash`), `tui_gateway/command_discovery.py`.

use hermes_runtime::Runtime;
use hermes_tools::SkillStore;
use serde_json::{Value, json};

/// Hermes's coding commands this backend runs (`coding_command`), as `hermes_cli/commands.py`
/// describes them.
const CODING_COMMANDS: [(&str, &str); 4] = [
    ("plan", "Write a markdown implementation plan to .hermes/plans/ without executing anything"),
    ("init", "Generate or update AGENTS.md project instructions from a repo scan"),
    ("review", "Spawn an independent subagent to review the work just discussed (PR, code, docs)"),
    ("learn", "Learn a reusable skill from anything you describe (dirs, URLs, this chat, notes)"),
];

/// A command the `/` menu offers: `(name, description, origin)`: the coding commands, then the
/// skills and bundles.
fn entries(skills: &SkillStore) -> Vec<(String, String, &'static str)> {
    let mut out: Vec<_> = CODING_COMMANDS
        .iter()
        .map(|(name, description)| (name.to_string(), description.to_string(), "builtin"))
        .collect();
    out.extend(
        skills
            .bundles()
            .into_iter()
            .map(|bundle| (bundle.slug, bundle.description, "bundle")),
    );
    out.extend(skills.catalog().into_iter().map(|skill| {
        let origin = if skill.external { "external" } else { "local" };
        (skill.name, skill.description, origin)
    }));
    // A skill named like a coding command can't be run by name (the command wins, as in Hermes).
    let builtin = |name: &str| CODING_COMMANDS.iter().any(|(command, _)| *command == name);
    out.retain(|(name, _, origin)| *origin == "builtin" || !builtin(name));
    out
}

/// `commands.catalog`: the coding commands, and the skills with their use counts.
pub fn catalog(skills: &SkillStore) -> Value {
    let entries = entries(skills);
    let listed: serde_json::Map<String, Value> = entries
        .iter()
        .filter(|(_, _, origin)| *origin != "builtin")
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
        .filter_map(|(name, description, origin)| {
            let lower = name.to_lowercase();
            let rank = if lower.starts_with(&query) {
                0
            } else if lower.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, name, description, origin))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let items: Vec<Value> = matches
        .into_iter()
        .map(|(_, name, description, origin)| {
            json!({
                "text": format!("/{name}"),
                "display": format!("/{name}"),
                "meta": description,
                "kind": if origin == "builtin" { "command" } else { "skill" },
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
    use hermes_prompts::{coding, review};
    let (command, argument) = hermes_tools::describe_bundle_invocation(text)
        .or_else(|| hermes_tools::skills::describe_invocation(text))
        // The coding commands send a prompt of their own (`coding_command`).
        .or_else(|| coding::plan_request(text).map(|task| ("plan".to_string(), task)))
        .or_else(|| coding::init_request(text).map(|notes| ("init".to_string(), notes)))
        .or_else(|| review::learn_request(text).map(|request| ("learn".to_string(), request)))?;
    Some(format!("/{command} {argument}").trim_end().to_string())
}

/// `/plan`, `/init`, `/learn`: Hermes's `send` dispatch, a prompt the page sends as a normal turn
/// (the agent does the work with its own tools), shown as the command typed. `/review` sends
/// itself (`/review [instructions]`): the turn starts its reviewer (`Gateway::submit`). `None`
/// for another command; `cwd` and `existing_agents` are the session's folder and its AGENTS.md.
pub fn coding_command(
    name: &str,
    argument: &str,
    cwd: &str,
    existing_agents: Option<&str>,
) -> Option<Value> {
    use hermes_prompts::{coding, review};
    let argument = argument.trim();
    let message = match name.trim_start_matches('/') {
        "plan" => coding::plan_message(argument),
        "init" => coding::init_message(cwd, existing_agents, argument),
        "learn" => review::learn_message(argument),
        "review" => format!("/review {argument}").trim_end().to_string(),
        _ => return None,
    };
    let display = format!("/{} {argument}", name.trim_start_matches('/'))
        .trim_end()
        .to_string();
    Some(json!({"type": "send", "message": message, "display": display}))
}

/// The instructions of a `/review [instructions]` message, `None` for another message.
pub fn review_instructions(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("/review")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_commands_send_their_prompt_and_show_the_command() {
        let plan = coding_command("plan", " add a cache ", "C:/r", None).unwrap();
        assert_eq!(plan["type"], "send");
        assert_eq!(plan["display"], "/plan add a cache");
        let message = plan["message"].as_str().unwrap();
        assert!(message.starts_with("[/plan — plan mode]"));
        // Stored, it reads back as the command.
        assert_eq!(typed_command(message).as_deref(), Some("/plan add a cache"));

        let init = coding_command("/init", "", "C:/r", Some("# Mine")).unwrap();
        assert!(init["message"].as_str().unwrap().contains("MERGE DISCIPLINE"));
        assert_eq!(typed_command(init["message"].as_str().unwrap()).as_deref(), Some("/init"));

        let learn = coding_command("learn", "the build steps", "", None).unwrap();
        assert_eq!(typed_command(learn["message"].as_str().unwrap()).as_deref(), Some("/learn the build steps"));

        let review = coding_command("review", "edge cases", "", None).unwrap();
        assert_eq!(review["message"], "/review edge cases");
        assert!(coding_command("status", "", "", None).is_none());
    }

    #[test]
    fn review_messages_are_recognised() {
        assert_eq!(review_instructions("/review"), Some(""));
        assert_eq!(review_instructions("/review  look at tests "), Some("look at tests"));
        assert_eq!(review_instructions("/reviewer x"), None);
        assert_eq!(review_instructions("please /review"), None);
    }

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
            ("planner", "Plan work"),
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
        // The `/plan` command, then the prefix-matching skill, then the one containing it.
        assert_eq!(texts, ["/plan", "/planner", "/explain-plan"]);
        assert_eq!(result["items"][0]["kind"], "command");
        assert_eq!(result["items"][1]["kind"], "skill");
        assert_eq!(result["items"][1]["meta"], "Plan work");
        // Past the name there's nothing to complete.
        assert!(
            complete(&store, "/plan now").as_object().unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // A skill named like a command (`review`) yields to it.
        assert_eq!(catalog(&store)["skill_count"], 2);
        assert_eq!(catalog(&store)["pairs"][0][0], "/plan");
        let reviews = complete(&store, "/review")["items"].as_array().unwrap().len();
        assert_eq!(reviews, 1);
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
