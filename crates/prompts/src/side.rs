//! Hermes's `/btw` side question (MIT, `agent/side_question.py`): a question about the
//! conversation answered from a snapshot of it, in one request with no tools, leaving the
//! conversation untouched. This is Hermes's one-shot path (a rendered transcript); its
//! cache-parity fork of the live agent is not ported.

/// The instructions the side question is asked under (`_ONESHOT_INSTRUCTIONS`).
pub const INSTRUCTIONS: &str = include_str!("../../../prompts/hermes/agent.side_question/ONESHOT_INSTRUCTIONS.txt");

/// Each message's cap in the transcript (`_PER_MESSAGE_CHAR_CAP`) and the transcript's
/// (`_TRANSCRIPT_CHAR_BUDGET`).
const PER_MESSAGE_CHARS: usize = 2000;
const TRANSCRIPT_CHARS: usize = 24_000;

/// One line of the transcript: who (`USER`, `ASSISTANT`, `TOOL RESULT`) and what.
pub enum Line {
    Said { label: &'static str, text: String },
    CalledTools(Vec<String>),
}

/// `render_history_for_side_question`: the newest lines that fit the budget, oldest first.
pub fn transcript(lines: &[Line]) -> String {
    let rendered: Vec<String> = lines
        .iter()
        .filter_map(|line| match line {
            Line::Said { label, text } if !text.trim().is_empty() => {
                Some(format!("{label}: {}", text.chars().take(PER_MESSAGE_CHARS).collect::<String>()))
            }
            Line::Said { .. } => None,
            Line::CalledTools(names) => Some(format!("ASSISTANT [called tools: {}]", names.join(", "))),
        })
        .collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for line in rendered.iter().rev() {
        let cost = line.chars().count() + 1;
        if used + cost > TRANSCRIPT_CHARS && !kept.is_empty() {
            break;
        }
        kept.push(line);
        used += cost;
    }
    if kept.is_empty() {
        return "(no prior conversation)".to_string();
    }
    let prefix = if kept.len() < rendered.len() { "[...older conversation omitted...]\n" } else { "" };
    kept.reverse();
    format!("{prefix}{}", kept.join("\n"))
}

/// The request's user message (`_answer_via_oneshot`).
pub fn user_input(transcript: &str, question: &str) -> String {
    format!(
        "Conversation transcript (snapshot):\n-----\n{transcript}\n-----\n\nSide question: {}",
        question.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_keeps_the_newest_lines_within_budget() {
        let said = |label, text: &str| Line::Said { label, text: text.to_string() };
        let small = transcript(&[
            said("USER", "fix the build"),
            Line::CalledTools(vec!["terminal".into(), "read_file".into()]),
            said("TOOL RESULT", ""),
            said("ASSISTANT", "Fixed: a missing import."),
        ]);
        assert_eq!(
            small,
            "USER: fix the build\nASSISTANT [called tools: terminal, read_file]\nASSISTANT: Fixed: a missing import."
        );
        assert_eq!(transcript(&[]), "(no prior conversation)");
        let many: Vec<Line> = (0..30).map(|i| said("USER", &format!("{i} {}", "x".repeat(1500)))).collect();
        let long = transcript(&many);
        assert!(long.starts_with("[...older conversation omitted...]\n"));
        assert!(long.ends_with(&"x".repeat(100)));
        assert!(long.contains("USER: 29 "));
        assert!(!long.contains("USER: 0 "));
        assert!(INSTRUCTIONS.contains("SIDE question"));
        assert!(user_input("T", " why? ").ends_with("Side question: why?"));
    }
}
