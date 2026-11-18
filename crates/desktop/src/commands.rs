//! The composer's `/` menu in Hermes's JSON: the catalog (`commands.catalog`), completions as the
//! user types (`complete.slash`), and what running a pick answers (`slash.exec`,
//! `command.dispatch`: `exec` output, a prompt to `send`, or a loaded `skill`). The menu itself
//! (which commands, which skills, what each does) is `hermes_core::commands`.
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.slash`), `tui_gateway/command_discovery.py`.

use hermes_core::commands::{CommandEntry, CommandOutcome, Origin};
use serde_json::{Value, json};

/// What a typed message was as a command (`hermes_core::commands`: the transcript reads it).
pub use hermes_core::commands::{review_instructions, typed_command};

/// `commands.catalog`: the coding commands, and the skills with their use counts (`uses`).
pub fn catalog(entries: &[CommandEntry], uses: impl Fn(&str) -> u64) -> Value {
    let listed: serde_json::Map<String, Value> = entries
        .iter()
        .filter(|entry| entry.origin != Origin::Builtin)
        .map(|entry| {
            (
                entry.name.clone(),
                json!({"usage": uses(&entry.name), "origin": entry.origin.as_str()}),
            )
        })
        .collect();
    // The bare `/` list: every skill and bundle as `[command, description]`.
    let pairs: Vec<[String; 2]> = entries
        .iter()
        .map(|entry| [format!("/{}", entry.name), entry.description.clone()])
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

/// `complete.slash`: the matching entries as completion items.
pub fn completions(matches: &[CommandEntry]) -> Value {
    let items: Vec<Value> = matches
        .iter()
        .map(|entry| {
            json!({
                "text": format!("/{}", entry.name),
                "display": format!("/{}", entry.name),
                "meta": entry.description,
                "kind": if entry.origin == Origin::Builtin { "command" } else { "skill" },
            })
        })
        .collect();
    json!({"items": items, "replace_from": 1})
}

/// A command's answer as Hermes's `slash.exec` / `command.dispatch` result.
pub fn outcome(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Output(output) => json!({"type": "exec", "output": output}),
        CommandOutcome::Send { message, display } => {
            json!({"type": "send", "message": message, "display": display})
        }
        CommandOutcome::Skill {
            name,
            message,
            display,
        } => json!({"type": "skill", "name": name, "message": message, "display": display}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, origin: Origin) -> CommandEntry {
        CommandEntry {
            name: name.into(),
            description: format!("About {name}"),
            origin,
        }
    }

    #[test]
    fn the_catalog_lists_every_entry_and_counts_the_skills() {
        let entries = [
            entry("plan", Origin::Builtin),
            entry("deploy", Origin::Local),
            entry("ship", Origin::Bundle),
        ];
        let catalog = catalog(&entries, |name| if name == "deploy" { 3 } else { 0 });
        assert_eq!(catalog["pairs"][0], json!(["/plan", "About plan"]));
        assert_eq!(catalog["skill_count"], 2);
        assert_eq!(
            catalog["skills"]["deploy"],
            json!({"usage": 3, "origin": "local"})
        );
        assert_eq!(catalog["skills"]["ship"]["origin"], "bundle");
        assert!(catalog["skills"].get("plan").is_none());
    }

    #[test]
    fn completions_and_answers_are_hermes_shapes() {
        let items = completions(&[entry("plan", Origin::Builtin), entry("x", Origin::External)]);
        assert_eq!(items["replace_from"], 1);
        assert_eq!(items["items"][0]["kind"], "command");
        assert_eq!(items["items"][1]["kind"], "skill");
        assert_eq!(items["items"][1]["text"], "/x");
        assert_eq!(
            outcome(CommandOutcome::Output("hi".into())),
            json!({"type": "exec", "output": "hi"})
        );
        let send = outcome(CommandOutcome::Send {
            message: "m".into(),
            display: "/plan".into(),
        });
        assert_eq!(send["type"], "send");
        let skill = outcome(CommandOutcome::Skill {
            name: "deploy".into(),
            message: "m".into(),
            display: "/deploy".into(),
        });
        assert_eq!(skill["name"], "deploy");
    }
}
