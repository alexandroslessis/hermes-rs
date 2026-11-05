//! Hermes's coding-workflow commands (MIT), each the message a normal turn sends: `/plan`
//! (`agent/plan_prompt.py`), `/init` (`hermes_cli/init_command.py`) and the `/review` reviewer's
//! briefing (`agent/review_engine.py`). The module-level texts come from `prompts/hermes/`
//! (`scripts/extract_hermes_prompts.py`); the glue inside Hermes's functions is copied here.

macro_rules! hermes {
    ($module:literal, $name:literal) => {
        include_str!(concat!("../../../prompts/hermes/", $module, "/", $name, ".txt"))
    };
}

const PLAN_MODE_RULES: &str = hermes!("agent.plan_prompt", "PLAN_MODE_RULES");
const PLAN_CRAFT: &str = hermes!("agent.plan_prompt", "PLAN_CRAFT");
const INIT_QUALITY_BAR: &str = hermes!("hermes_cli.init_command", "QUALITY_BAR");
const REVIEW_GOAL: &str = hermes!("agent.review_engine", "REVIEW_GOAL");

/// How many recent user/assistant messages the reviewer gets (`DEFAULT_CONTEXT_MESSAGES`).
pub const REVIEW_CONTEXT_MESSAGES: usize = 10;
/// Each message's cap in the reviewer's excerpt (`_MESSAGE_CHAR_CAP`).
const REVIEW_MESSAGE_CHARS: usize = 12_000;

/// `build_plan_prompt`: plan only, saved under `.hermes/plans/`; an empty task is inferred from
/// the conversation.
pub fn plan_message(task: &str) -> String {
    let task = task.trim();
    let task_block = if task.is_empty() {
        "No explicit task was given with /plan — infer the task from the current conversation \
context (the thing we have been discussing or working toward). If the conversation does not imply \
a task, ask a brief clarifying question.\n"
            .to_string()
    } else {
        format!("Task to plan:\n{task}\n")
    };
    format!("[/plan — plan mode]\n\n{PLAN_MODE_RULES}\n{task_block}\n{PLAN_CRAFT}")
}

/// `build_init_prompt`: generate an AGENTS.md for the project at `cwd`, or update `existing`
/// (merge discipline); `extra` is the user's notes after `/init`.
pub fn init_message(cwd: &str, existing: Option<&str>, extra: &str) -> String {
    let extra = extra.trim();
    let update = existing.is_some();
    let target = format!("{}/AGENTS.md", cwd.trim_end_matches(['/', '\\']));
    let mut parts = vec![
        format!(
            "[/init] The user wants you to {} AGENTS.md project-instructions file for the project at: {cwd}\n",
            if update { "UPDATE the existing" } else { "generate an" }
        ),
        "AGENTS.md is the instruction file coding agents (Hermes included) load as project \
context every session. It should teach an agent how to work in THIS repo: what the project is, how \
to set up, the exact build/test/lint commands, the conventions the code actually follows, and the \
pitfalls that waste time.\n"
            .to_string(),
        format!(
            "Do this:\n\
1. Inspect the project with your read-only tools (`read_file`, `search_files`) — start with \
manifests and toolchain files (package.json, pyproject.toml, Cargo.toml, go.mod, Makefile, CI \
workflow configs, lockfiles), then the directory layout, existing README/docs, and test/lint \
configuration. Learn the real commands, don't guess them.\n\
2. Write the file to {target} with `write_file`{}\n\
3. Confirm to the user the exact path you wrote and summarize in one or two lines what the file \
covers.\n",
            if update {
                " — but this is an UPDATE, so follow the merge discipline below."
            } else {
                "."
            }
        ),
    ];
    if let Some(existing) = existing {
        parts.push(format!(
            "MERGE DISCIPLINE — an AGENTS.md already exists (its current content is below). Do NOT \
overwrite or regenerate it from scratch. Preserve the user's existing content — their wording, \
their sections, their rules — and merge in only what is missing or verifiably stale (e.g. a command \
that no longer exists in the repo). When existing content conflicts with what you observed, prefer \
minimal surgical edits over rewrites, and keep the user's intent. The result must still meet the \
quality bar.\n\n\
CURRENT AGENTS.md CONTENT:\n<<<EXISTING_AGENTS_MD\n{existing}\nEXISTING_AGENTS_MD\n"
        ));
    }
    parts.push(INIT_QUALITY_BAR.to_string());
    if !extra.is_empty() {
        parts.push(format!(
            "\nUSER NOTES — honor these while authoring (they override the defaults above where \
they conflict):\n{extra}"
        ));
    }
    parts.join("\n")
}

/// The task a stored `/plan` message was sent with (`Some("")` for none): the chat shows the
/// command, not the prompt.
pub fn plan_request(message: &str) -> Option<String> {
    let rest = message.strip_prefix("[/plan — plan mode]")?;
    let Some((_, task)) = rest.split_once("Task to plan:\n") else {
        return Some(String::new());
    };
    let first_craft_line = PLAN_CRAFT.lines().next().unwrap_or_default();
    Some(task.split(first_craft_line).next().unwrap_or_default().trim().to_string())
}

/// The notes a stored `/init` message was sent with (`Some("")` for none).
pub fn init_request(message: &str) -> Option<String> {
    if !message.starts_with("[/init] ") {
        return None;
    }
    Some(
        message
            .split_once("they conflict):\n")
            .map_or(String::new(), |(_, notes)| notes.trim().to_string()),
    )
}

/// `build_review_task`: the reviewer's short goal (its live label) and its full briefing, from
/// the conversation's recent `(is_user, text)` messages, oldest first, and the skills the
/// primary agent loaded (its review must hold the work to them).
pub fn review_task(recent: &[(bool, String)], instructions: &str, skills: &[String]) -> (String, String) {
    let focus = instructions.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut goal = if focus.is_empty() {
        "Review recent work".to_string()
    } else {
        format!("Review: {focus}")
    };
    if goal.chars().count() > 80 {
        goal = format!("{}…", goal.chars().take(79).collect::<String>().trim_end());
    }
    let mut lines = vec![
        REVIEW_GOAL.to_string(),
        String::new(),
        "You were spawned by the /review command. The following is an excerpt of the most recent \
conversation between the user and their primary agent. It is your starting evidence — the work to \
review is referenced in it."
            .to_string(),
        String::new(),
        "--- Recent conversation (oldest first) ---".to_string(),
    ];
    for (is_user, text) in recent {
        let text = if text.chars().count() > REVIEW_MESSAGE_CHARS {
            format!("{}\n[... truncated ...]", text.chars().take(REVIEW_MESSAGE_CHARS).collect::<String>())
        } else {
            text.clone()
        };
        lines.push(format!("[{}]", if *is_user { "USER" } else { "PRIMARY AGENT" }));
        lines.push(text);
        lines.push(String::new());
    }
    lines.push("--- End of conversation excerpt ---".to_string());
    if !skills.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "The primary agent was operating under these loaded skills: {}. Before reviewing, load \
each with skill_view(name=...) and treat their conventions, invariants, and review standards as \
binding for your assessment — the work was produced under them and must be judged against them.",
            skills.join(", ")
        ));
    }
    if !instructions.trim().is_empty() {
        lines.push(String::new());
        lines.push("Additional review instructions from the user:".to_string());
        lines.push(instructions.trim().to_string());
    }
    lines.push(String::new());
    lines.push(
        "Your review is delivered back into that conversation, addressed to the primary agent and \
its user. Be direct and specific; do not soften findings."
            .to_string(),
    );
    (goal, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_names_the_task_or_infers_it() {
        let named = plan_message("add a cache");
        assert!(named.starts_with("[/plan — plan mode]"));
        assert!(named.contains("Task to plan:\nadd a cache\n"));
        assert!(named.contains(".hermes/plans/"));
        assert!(plan_message("  ").contains("infer the task from the current conversation"));
    }

    #[test]
    fn init_generates_or_merges() {
        let fresh = init_message("C:/work/repo", None, "");
        assert!(fresh.contains("generate an AGENTS.md"));
        assert!(fresh.contains("Write the file to C:/work/repo/AGENTS.md with `write_file`."));
        assert!(!fresh.contains("MERGE DISCIPLINE"));
        let update = init_message("C:/work/repo/", Some("# Old"), "mention the perf scripts");
        assert!(update.contains("UPDATE the existing AGENTS.md"));
        assert!(update.contains("<<<EXISTING_AGENTS_MD\n# Old\nEXISTING_AGENTS_MD"));
        assert!(update.contains("USER NOTES"));
        assert!(update.contains("C:/work/repo/AGENTS.md"), "no doubled slash");
    }

    #[test]
    fn the_reviewer_gets_the_excerpt_and_a_short_label() {
        let recent = vec![(true, "fix the parser".to_string()), (false, "Done: patched lexer.rs".to_string())];
        let (goal, context) = review_task(&recent, "focus on   edge cases", &["rust-style".to_string()]);
        assert_eq!(goal, "Review: focus on edge cases");
        assert!(context.contains("[USER]\nfix the parser"));
        assert!(context.contains("[PRIMARY AGENT]\nDone: patched lexer.rs"));
        assert!(context.contains("Additional review instructions from the user:\nfocus on   edge cases"));
        assert!(context.contains("these loaded skills: rust-style."));
        let (long, _) = review_task(&recent, &"x".repeat(200), &[]);
        assert_eq!(long.chars().count(), 80);
        assert_eq!(review_task(&[], "", &[]).0, "Review recent work");
    }

    #[test]
    fn stored_messages_read_back_as_their_commands() {
        assert_eq!(plan_request(&plan_message("add a cache\nwith TTLs")).as_deref(), Some("add a cache\nwith TTLs"));
        assert_eq!(plan_request(&plan_message("")).as_deref(), Some(""));
        assert_eq!(plan_request("plan this"), None);
        assert_eq!(init_request(&init_message("C:/r", None, "keep it short")).as_deref(), Some("keep it short"));
        assert_eq!(init_request(&init_message("C:/r", Some("# x"), "")).as_deref(), Some(""));
        assert_eq!(init_request("[/initial] x"), None);
    }
}
