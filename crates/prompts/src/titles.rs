//! Hermes's session titles (`agent/title_generator.py`, MIT): a short model-written name from the
//! opening message.

/// `MAX_TITLE_INPUT_CHARS`.
pub const MAX_TITLE_INPUT_CHARS: usize = 1000;
/// `TITLE_MAX_TOKENS`: room for a fenced reply or a model that thinks anyway.
pub const TITLE_MAX_TOKENS: i32 = 512;

/// `_TITLE_PROMPT_TEMPLATE` with the match-the-user language rule.
pub const TITLE_PROMPT: &str = "You name chat sessions. Given the user's opening message, write a title that lets them find this conversation again in a list.\n\nRules:\n- 3 to 7 words, sentence case (capitalize only the first word and proper nouns).\n- Name what the user wants DONE, not that they asked a question.\n- Keep technical terms, filenames, numbers, and error codes exact.\n- Drop filler words: the, this, my, a, an.\n- No trailing punctuation, no quotes, no tool names, no 'Title:' prefix.\n- Never answer the message. Name it.\n- Always produce something, even for a bare greeting.\n- Write the title in the same language as the user's message.\nGood: {\"title\": \"Fix login button on mobile\"}\nGood: {\"title\": \"Postgres connection pool exhaustion\"}\nGood: {\"title\": \"Friendly greeting\"}\nToo vague: {\"title\": \"Code changes\"}\nToo long: {\"title\": \"Investigate and fix the issue where the login button does not respond on mobile devices\"}\n\nReply with JSON only: {\"title\": \"...\"}";

/// `_EXAMPLE_ECHO_REJECT`: a model parroting a prompt example says nothing about the session.
const ECHOES: &[&str] = &[
    "fix login button on mobile",
    "postgres connection pool exhaustion",
    "code changes",
];

fn strip_prefix(title: &str) -> &str {
    let lower = title.to_lowercase();
    if lower.starts_with("title:") {
        title[6..].trim()
    } else {
        title
    }
}

/// `_extract_title_text` + `_clean_title`: the title from strict or loose JSON, else the first
/// line of prose; `None` when nothing usable (or an echoed example) remains.
pub fn parse_title(content: &str) -> Option<String> {
    let mut raw = content.trim();
    if let Some(inner) = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
    {
        raw = inner.trim();
    }
    let from_json = raw.find('{').and_then(|start| {
        let end = raw.rfind('}')?;
        let value: serde_json::Value = serde_json::from_str(&raw[start..=end]).ok()?;
        value.get("title")?.as_str().map(str::to_string)
    });
    let text = match from_json {
        Some(title) => title,
        // A cut-off JSON reply is dropped, not used as prose.
        None if raw.starts_with('{') => return None,
        None => raw.lines().find(|l| !l.trim().is_empty())?.to_string(),
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = strip_prefix(collapsed.trim_matches(['"', '\'']).trim())
        .trim_end_matches(['.', '!', ',', ';', ':'])
        .to_string();
    let title = if title.chars().count() > 80 {
        format!(
            "{}...",
            title.chars().take(77).collect::<String>().trim_end()
        )
    } else {
        title
    };
    (!title.is_empty() && !ECHOES.contains(&title.to_lowercase().as_str())).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_parse_from_json_fences_and_prose() {
        assert_eq!(
            parse_title(r#"{"title": "Fix parser crash"}"#).as_deref(),
            Some("Fix parser crash")
        );
        assert_eq!(
            parse_title("```json\n{\"title\": \"Speed up CI\"}\n```").as_deref(),
            Some("Speed up CI")
        );
        assert_eq!(
            parse_title("Title: Rust lifetimes question.").as_deref(),
            Some("Rust lifetimes question")
        );
        assert_eq!(parse_title(r#"{"title": "Code changes"}"#), None);
        assert_eq!(parse_title(r#"{"title": "unfinished"#), None);
    }
}
