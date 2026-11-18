//! What a typed message was as a command: the transcript and titles show `/name instruction` for a
//! message that loaded a skill, a bundle or a coding command's prompt, and `/review` starts its
//! reviewer. The `/` menu's catalog and dispatch stay in `crates/desktop/src/commands.rs` until
//! CP1.2 moves them.
//!
//! Hermes: `hermes_cli/commands.py` (the coding commands), `tools/skills_tool.py` (the skill
//! invocation a message carries).

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

/// The instructions of a `/review [instructions]` message, `None` for another message.
pub fn review_instructions(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("/review")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_takes_its_instructions_and_nothing_else_is_review() {
        assert_eq!(review_instructions("/review"), Some(""));
        assert_eq!(
            review_instructions("  /review the parser "),
            Some("the parser")
        );
        assert_eq!(review_instructions("/reviewer"), None);
        assert_eq!(review_instructions("please /review"), None);
    }

    #[test]
    fn an_ordinary_message_is_no_command() {
        assert_eq!(typed_command("hello there"), None);
    }
}
