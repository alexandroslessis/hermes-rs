//! Deferred tool schemas (Phase 11, CP11.2): the text and the names behind the `tokens` profile's
//! `tool_search`. Not in Hermes (it sends every schema every time): `docs/backend/PHASES.md`,
//! Phase 11. The mechanism lives in `crates/runtime/src/deferred.rs`; this module holds what the
//! model reads.
//!
//! In a deferred session the request's tool list is the core tools plus `tool_search`; the tools
//! in [`BUILTIN`] (and the MCP tools past the first few) are named in a frozen block of the system
//! prompt ([`block`]) and join the list when `tool_search` loads them.

use anyhow::{Context as _, Result};

use crate::tools::ToolSchema;

/// The loader's name. It is not in [`crate::tools::tool_names`]: only a deferred session has it.
pub const TOOL_SEARCH: &str = "tool_search";

/// Most names one `tool_search` call loads (more is refused, nothing loaded).
pub const MAX_NAMES: usize = 8;
/// Most matches a `query` lists.
pub const MAX_MATCHES: usize = 8;

/// The tool the main agent does not have at all in a deferred session: the background review
/// (which has its own tool set) writes skills.
pub const REVIEW_ONLY: &str = "skill_manage";

/// The built-in tools that are not in a deferred session's tool list until loaded, with the one
/// line the prompt gives each. Chosen by size and by how rarely a coding session needs them
/// (`docs/backend/progress/phase-11.md` has the sizes); the rest are core.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "session_search",
        "search or read past conversations (what did we do about X)",
    ),
    (
        "delegate_task",
        "run subagents in parallel on independent tasks",
    ),
    ("web_search", "search the web"),
    ("web_extract", "read web pages as text"),
    ("vision_analyze", "look at an image file or URL"),
    (
        "process_manage",
        "list, poll, write to or kill background processes",
    ),
    ("clarify", "ask the user a question with choices"),
];

/// Starts the block in the system prompt; a session is deferred exactly when its frozen prompt
/// holds a *sealed* block (so a profile change mid-session changes nothing, and a resumed session
/// knows). See [`seal`] for why a copy of the text elsewhere in the prompt does not count.
pub const MARKER: &str = "# Deferred tools";
/// The block's last line as [`block`] writes it, before [`seal`].
const END: &str = "<!-- End deferred tools -->";
/// The sealed last line: `<!-- End deferred tools <16 hex digits> -->`.
const END_SEALED_OPEN: &str = "<!-- End deferred tools ";
const COMMENT_CLOSE: &str = " -->";
/// The block records which MCP tools the session keeps in its list (the rest are deferred), so a
/// server that connects later, or a changed `deferred_mcp_keep`, never moves a tool out of it.
const CORE_MCP_OPEN: &str = "<!-- core MCP tools: ";

const MCP_LINES_MAX: usize = 40;

/// The `tool_search` schema (`prompts/tools/tool_search.json`).
pub fn tool_search_schema() -> Result<ToolSchema> {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("../../../prompts/tools/tool_search.json"))?;
    let serde_json::Value::Object(parameters) = value["parameters"].clone() else {
        anyhow::bail!("tool_search parameters must be an object");
    };
    Ok(ToolSchema {
        name: TOOL_SEARCH.to_string(),
        description: value["description"]
            .as_str()
            .context("tool_search description")?
            .to_string(),
        parameters,
    })
}

/// The first sentence of a description, one line, at most `max` characters: an MCP tool's line.
pub fn short_line(description: &str, max: usize) -> String {
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = flat
        .split(". ")
        .next()
        .unwrap_or(&flat)
        .trim_end_matches('.');
    if first.chars().count() <= max {
        return first.to_string();
    }
    let cut: String = first.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// The block that names the deferred tools and says how to load them, for the end of the stable
/// tier. `lines` are `(name, one line)`; the built-in ones come first. `core_mcp`: the MCP tools
/// that stay in the list, recorded in the block ([`core_mcp`]). Unsealed: the prompt builder seals
/// it ([`seal`]) once the stable tier before it is known.
pub fn block(lines: &[(String, String)], core_mcp: &[String]) -> String {
    let mut listed: Vec<String> = Vec::new();
    for (index, (name, line)) in lines.iter().enumerate() {
        if index == MCP_LINES_MAX + BUILTIN.len() {
            listed.push(format!(
                "- … and {} more (tool_search with `query` finds them)",
                lines.len() - index
            ));
            break;
        }
        listed.push(if line.is_empty() {
            format!("- {name}")
        } else {
            format!("- {name}: {line}")
        });
    }
    format!(
        "{MARKER}\n\n\
         Your tool list holds the core tools only, to keep every request small. These tools exist \
         too, but their schemas are not loaded:\n{}\n\n\
         To use one, first call tool_search with its exact names (`{{\"names\": [\"web_search\", \
         \"web_extract\"]}}`, everything you need in one call): the result holds the schemas and the \
         tools are in your tool list from your next step. A deferred tool called before it is \
         loaded is not run; you only get its schema back. `tool_search` with `query` finds tools by \
         keyword.\n\
         You have no skill_manage tool: a background review of the conversation saves skills. When \
         a task teaches you a procedure, a pitfall or the user's correction, say it plainly in your \
         answer so the review can pick it up.\n{core}{END}",
        listed.join("\n"),
        core = if core_mcp.is_empty() {
            String::new()
        } else {
            format!("{CORE_MCP_OPEN}{}{COMMENT_CLOSE}\n", core_mcp.join(", "))
        }
    )
}

/// FNV-1a, 64 bits: the seal's checksum. **Never change it**: a stored session's prompt carries
/// its value, and a different function would turn every deferred session into a plain one.
fn fnv64(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Seals the block at the end of a stable tier: its last line gets a checksum of everything before
/// the block. The stable tier is the start of the prompt and holds only the identity and fixed
/// guidance, so the real block is preceded by text the runtime wrote; a copy of the block in a
/// project's context file, a skill or a memory entry (a repository's docs quoting it, say) follows
/// other text and fails the check, so it can neither make a plain session deferred nor be stripped
/// from one. `None` when `stable` does not end with `block`.
pub fn seal(stable: &str, block: &str) -> Option<String> {
    let start = stable.len().checked_sub(block.len())?;
    if !stable.ends_with(block) || !block.ends_with(END) {
        return None;
    }
    Some(format!(
        "{}{END_SEALED_OPEN}{:016x}{COMMENT_CLOSE}",
        &stable[..stable.len() - END.len()],
        fnv64(&stable[..start])
    ))
}

/// The sealed block of a prompt as byte offsets `(start, end)`: the first block whose seal matches
/// the text before it.
fn find_block(prompt: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(offset) = prompt[from..].find(MARKER) {
        let start = from + offset;
        from = start + MARKER.len();
        if start != 0 && !prompt[..start].ends_with('\n') {
            continue;
        }
        let open = start + prompt[start..].find(END_SEALED_OPEN)?;
        let digits = open + END_SEALED_OPEN.len();
        let Some(hex) = prompt.get(digits..digits + 16) else {
            continue;
        };
        if !prompt[digits + 16..].starts_with(COMMENT_CLOSE) {
            continue;
        }
        if u64::from_str_radix(hex, 16).ok() == Some(fnv64(&prompt[..start])) {
            return Some((start, digits + 16 + COMMENT_CLOSE.len()));
        }
    }
    None
}

/// Whether a frozen system prompt belongs to a deferred session (it holds a sealed block).
pub fn is_deferred_prompt(prompt: &str) -> bool {
    find_block(prompt).is_some()
}

/// The MCP tools a deferred session keeps in its list, as its frozen block recorded them (empty
/// for a session that is not deferred or kept none).
pub fn core_mcp(prompt: &str) -> Vec<String> {
    let Some((start, end)) = find_block(prompt) else {
        return Vec::new();
    };
    prompt[start..end]
        .lines()
        .find_map(|line| {
            line.strip_prefix(CORE_MCP_OPEN)?
                .strip_suffix(COMMENT_CLOSE)
        })
        .map(|names| names.split(", ").map(str::to_string).collect())
        .unwrap_or_default()
}

/// `prompt` without the deferred block, for a context that has the whole tool list (the review).
pub fn strip_block(prompt: &str) -> String {
    let Some((start, end)) = find_block(prompt) else {
        return prompt.to_string();
    };
    let before = prompt[..start].trim_end();
    let after = prompt[end..].trim_start();
    if after.is_empty() {
        before.to_string()
    } else {
        format!("{before}\n\n{after}")
    }
}

/// The skills block of a prompt for a session without `skill_manage`: the sentences that tell the
/// model to call it say that the background review does it instead.
pub fn without_skill_manage(skills_index: &str) -> String {
    skills_index
        .replace(
            "If a skill has issues, fix it with skill_manage(action='patch').\n",
            "If a skill has issues, say so in your answer: the background review fixes skills.\n",
        )
        .replace(
            "After difficult/iterative tasks, offer to save as a skill. If a skill you loaded was missing steps, had wrong commands, or needed pitfalls you discovered, update it before finishing.\n",
            "If a skill you loaded was missing steps or had wrong commands, say so in your answer: the background review updates skills.\n",
        )
}

/// `memory`'s description for a session without `skill_manage`.
pub fn memory_description_without_skill_manage(description: &str) -> String {
    description.replace(
        "belongs in that task's skill via skill_manage",
        "belongs in that task's skill, which the background review saves",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{tool_names, tool_schema};

    #[test]
    fn tool_search_is_not_a_regular_tool() {
        assert!(!tool_names().any(|name| name == TOOL_SEARCH));
        let schema = tool_search_schema().unwrap();
        assert_eq!(schema.name, TOOL_SEARCH);
        assert_eq!(schema.parameters["type"], "object");
        let size = serde_json::json!({"type": "function", "function": {
            "name": schema.name, "description": schema.description, "parameters": schema.parameters}})
        .to_string()
        .len();
        assert!(size <= 900, "tool_search is {size} bytes");
    }

    #[test]
    fn the_deferred_builtins_exist_and_skill_manage_is_not_among_them() {
        for (name, line) in BUILTIN {
            assert!(tool_schema(name).is_ok(), "{name}");
            assert!(!line.is_empty() && line.len() <= 70, "{name}: {line}");
            assert_ne!(*name, REVIEW_ONLY);
        }
        assert!(tool_schema(REVIEW_ONLY).is_ok());
    }

    #[test]
    fn the_block_is_found_and_stripped() {
        let lines: Vec<(String, String)> = BUILTIN
            .iter()
            .map(|(n, l)| (n.to_string(), l.to_string()))
            .collect();
        let block = block(&lines, &[]);
        // What every deferred session's frozen prompt carries (about 300 tokens), once.
        println!("deferred block: {} bytes", block.len());
        assert!(block.len() <= 1_700, "the block is {} bytes", block.len());
        let stable = seal(&format!("identity\n\nguidance\n\n{block}"), &block).unwrap();
        let prompt = format!("{stable}\n\nmore context");
        assert!(is_deferred_prompt(&prompt));
        assert!(!is_deferred_prompt("identity\n\nmore"));
        let stripped = strip_block(&prompt);
        assert_eq!(stripped, "identity\n\nguidance\n\nmore context");
        assert!(!is_deferred_prompt(&stripped));
        // At the very end too.
        assert_eq!(
            strip_block(&seal(&format!("a\n\n{block}"), &block).unwrap()),
            "a"
        );
        for (name, _) in BUILTIN {
            assert!(block.contains(&format!("- {name}: ")), "{name}");
        }
        assert!(block.contains("tool_search"));
        // The text names no tool it would not offer.
        assert!(block.contains("no skill_manage tool"));
        assert!(core_mcp(&prompt).is_empty());
    }

    /// A copy of the block anywhere but where the builder sealed it (a project's docs quoting it in
    /// a context file, a memory entry, an edited prompt) makes no session deferred and is never
    /// stripped; the real block is found after a copy in the identity (SOUL.md) too.
    #[test]
    fn a_copied_block_does_not_count() {
        let block = block(&[("web_search".into(), "search".into())], &[]);
        let real = seal(&format!("identity\n\n{block}"), &block).unwrap();
        let sealed_block = &real["identity\n\n".len()..];
        // Plain sessions whose context quotes the block, unsealed or sealed for another prefix.
        for copy in [block.as_str(), sealed_block] {
            let plain = format!("identity\n\nguidance\n\n# Project Context\n\n{copy}\n\ntail");
            assert!(!is_deferred_prompt(&plain), "{copy}");
            assert_eq!(strip_block(&plain), plain);
            assert!(core_mcp(&plain).is_empty());
        }
        // A copy before the real one (the user's SOUL.md) and one after it.
        let stable = seal(&format!("{sealed_block}\n\nguidance\n\n{block}"), &block).unwrap();
        let prompt = format!("{stable}\n\n{sealed_block}");
        assert!(is_deferred_prompt(&prompt));
        assert_eq!(
            strip_block(&prompt),
            format!("{sealed_block}\n\nguidance\n\n{sealed_block}")
        );
        // A changed byte before the block breaks the seal.
        assert!(!is_deferred_prompt(
            &real.replacen("identity", "identitY", 1)
        ));
    }

    #[test]
    fn the_block_records_the_core_mcp_tools() {
        let core = vec!["mcp__a__x".to_string(), "mcp__b__y".to_string()];
        let block = block(&[("mcp__c__z".into(), "zed".into())], &core);
        let prompt = format!(
            "{}\n\nrest",
            seal(&format!("id\n\n{block}"), &block).unwrap()
        );
        assert_eq!(core_mcp(&prompt), core);
        assert!(is_deferred_prompt(&prompt));
        assert_eq!(strip_block(&prompt), "id\n\nrest");
    }

    #[test]
    fn a_long_list_is_cut() {
        let lines: Vec<(String, String)> = (0..80)
            .map(|i| (format!("mcp__s__t{i}"), "does a thing".to_string()))
            .collect();
        let block = block(&lines, &[]);
        assert!(block.contains("and 33 more"), "{block}");
        assert!(block.lines().count() < 70);
    }

    #[test]
    fn lines_are_short() {
        assert_eq!(short_line("Add two numbers", 60), "Add two numbers");
        assert_eq!(
            short_line("Add two numbers. Then more words follow.", 60),
            "Add two numbers"
        );
        let long = short_line(&"word ".repeat(40), 30);
        assert!(long.chars().count() <= 30 && long.ends_with('…'), "{long}");
    }

    #[test]
    fn the_skill_manage_sentences_are_rewritten() {
        let names =
            "## Skills\nIf a skill has issues, fix it with skill_manage(action='patch').\n\n<x>";
        assert!(!without_skill_manage(names).contains("skill_manage"));
        let full = "a\nIf a skill has issues, fix it with skill_manage(action='patch').\nAfter difficult/iterative tasks, offer to save as a skill. If a skill you loaded was missing steps, had wrong commands, or needed pitfalls you discovered, update it before finishing.\nb";
        assert!(!without_skill_manage(full).contains("skill_manage"));
        let memory = tool_schema("memory").unwrap().description;
        assert!(memory.contains("skill_manage"));
        assert!(!memory_description_without_skill_manage(&memory).contains("skill_manage"));
    }
}
