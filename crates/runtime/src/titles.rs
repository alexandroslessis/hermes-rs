//! Session titles without a request (Phase 4 of docs/backend/PHASES.md; not in Hermes, which always
//! asks a model: `agent/title_generator.py`, see `generate_title` in `lib.rs`).
//!
//! Under the `requests` budget profile, with no auxiliary title model set, a session is named from
//! its opening message by [`heuristic_title`]. Once an auxiliary title model is configured, new
//! sessions get the model's title again (the desktop's `title_session`); sessions already named
//! by the heuristic keep their name (re-titling them would need a marker the frozen store schema
//! has no column for).

/// Most words a heuristic title keeps.
const MAX_WORDS: usize = 6;
/// Most characters (the first-line title it replaces had the same cap).
const MAX_CHARS: usize = 60;

/// Words that open a request without saying what it is about ("please can you ...").
const LEADING_FILLER: &[&str] = &[
    "please", "pls", "hey", "hi", "hello", "ok", "okay", "so", "can", "could", "would", "will",
    "you", "i", "i'd", "i'm", "id", "want", "need", "like", "to", "help", "me", "us", "just",
    "kindly", "let's", "lets", "we", "should", "now", "also", "go", "ahead", "and", "then",
];

/// Articles and similar words dropped anywhere (the model prompt drops them too).
const DROPPED: &[&str] = &["the", "this", "that", "my", "a", "an", "our", "your"];

/// Words kept lower case inside a title.
const SMALL: &[&str] = &[
    "of", "in", "on", "at", "to", "for", "and", "or", "but", "with", "by", "from", "as", "is",
];

/// Removes fenced and inline code, and `@file:` / `@url:` style references.
fn strip_noise(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    // Fenced blocks first (an unclosed fence drops the rest).
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        out.push(' ');
        match rest[start + 3..].find("```") {
            Some(end) => rest = &rest[start + 3 + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    // Inline code.
    let mut cleaned = String::new();
    let mut in_code = false;
    for c in out.chars() {
        if c == '`' {
            in_code = !in_code;
            cleaned.push(' ');
        } else if !in_code {
            cleaned.push(c);
        }
    }
    cleaned
}

/// The first sentence: up to the first `.`/`!`/`?` followed by whitespace or the end, or the
/// first line break, whichever comes first.
fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    let mut end = text.len();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, (pos, c)) in chars.iter().enumerate() {
        // CJK full stops end a sentence with no space after them.
        if matches!(c, '\n' | '。' | '！' | '？') {
            end = *pos;
            break;
        }
        if matches!(c, '.' | '!' | '?')
            && chars.get(i + 1).is_none_or(|(_, next)| next.is_whitespace())
        {
            end = *pos;
            break;
        }
    }
    &text[..end]
}

fn title_case(word: &str, first: bool) -> String {
    let lower = word.to_lowercase();
    // Keep identifiers, acronyms and file names as typed.
    let keep = word.chars().any(|c| c.is_uppercase() && word.chars().count() > 1)
        || word.contains(['.', '/', '\\', '_', '-', ':'])
        || word.chars().any(|c| c.is_ascii_digit());
    if keep {
        return word.to_string();
    }
    if !first && SMALL.contains(&lower.as_str()) {
        return lower;
    }
    let mut chars = lower.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A title for a session from its opening message: the first sentence, with code blocks,
/// `@references`, leading filler ("can you please ...") and articles removed, at most six words,
/// in title case. Empty when nothing is left (the caller keeps the message's first line).
pub fn heuristic_title(opening: &str) -> String {
    let cleaned = strip_noise(opening);
    // The first sentence that has words left after the noise is gone.
    let sentence = cleaned
        .lines()
        .map(strip_markers)
        .filter(|l| !l.is_empty())
        .map(first_sentence)
        .find(|s| {
            s.split_whitespace()
                .any(|w| !w.starts_with('@') && w.chars().any(char::is_alphanumeric))
        })
        .unwrap_or("");
    let mut words: Vec<&str> = sentence
        .split_whitespace()
        .filter(|w| !w.starts_with('@') && !w.contains("://"))
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '"' | '\'' | '(' | ')')))
        .filter(|w| !w.is_empty())
        .collect();
    let leading = words
        .iter()
        .take_while(|w| LEADING_FILLER.contains(&w.to_lowercase().as_str()))
        .count();
    // Keep at least one word even if everything looked like filler ("help me").
    if leading < words.len() {
        words.drain(..leading);
    }
    let words: Vec<&str> = words
        .into_iter()
        .filter(|w| !DROPPED.contains(&w.to_lowercase().as_str()))
        .take(MAX_WORDS)
        .collect();
    words
        .iter()
        .enumerate()
        .map(|(i, w)| title_case(w, i == 0))
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// A line without its Markdown list, heading or quote marker ("1. ", "- ", "## ", "> ").
fn strip_markers(line: &str) -> &str {
    let line = line.trim();
    let bullets = line.trim_start_matches(['#', '-', '*', '+', '>']);
    if bullets.len() != line.len() && (bullets.is_empty() || bullets.starts_with(char::is_whitespace)) {
        return bullets.trim_start();
    }
    let digits = line.trim_start_matches(|c: char| c.is_ascii_digit());
    if digits.len() != line.len()
        && let Some(rest) = digits.strip_prefix(['.', ')'])
        && rest.starts_with(char::is_whitespace)
    {
        return rest.trim_start();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_first_sentence_and_drops_filler() {
        assert_eq!(
            heuristic_title("Can you please fix the login button on mobile? It is broken."),
            "Fix Login Button on Mobile"
        );
    }

    #[test]
    fn caps_at_six_words() {
        assert_eq!(
            heuristic_title("Investigate why the nightly build keeps failing on windows runners"),
            "Investigate Why Nightly Build Keeps Failing"
        );
    }

    #[test]
    fn strips_code_blocks_and_references() {
        assert_eq!(
            heuristic_title("@file:src/main.rs refactor the parser\n```rust\nfn x() {}\n```"),
            "Refactor Parser"
        );
        assert_eq!(
            heuristic_title("```\nstack trace\n```\nWhy does `cargo build` hang"),
            "Why Does Hang"
        );
    }

    #[test]
    fn keeps_identifiers_and_file_names() {
        assert_eq!(
            heuristic_title("rename foo_bar in config.toml"),
            "Rename foo_bar in config.toml"
        );
    }

    #[test]
    fn empty_input_gives_an_empty_title() {
        assert_eq!(heuristic_title(""), "");
        assert_eq!(heuristic_title("```\ncode only\n```"), "");
        assert_eq!(heuristic_title("@file:a.rs"), "");
    }

    #[test]
    fn list_markers_urls_and_odd_input() {
        assert_eq!(heuristic_title("1. Fix the flaky test\n2. Ship"), "Fix Flaky Test");
        assert_eq!(heuristic_title("## Deploy notes"), "Deploy Notes");
        assert_eq!(heuristic_title("- refactor cache"), "Refactor Cache");
        assert_eq!(heuristic_title("--verbose is ignored"), "--verbose is Ignored");
        assert_eq!(
            heuristic_title("summarize https://example.com/a/very/long/path please"),
            "Summarize Please"
        );
        assert_eq!(heuristic_title("?!\n...\nwhy"), "Why");
        assert_eq!(heuristic_title("   \n\t"), "");
        assert_eq!(heuristic_title("`unclosed code"), "");
        // Non-English text passes through, title-cased; nothing panics on multi-byte input.
        assert_eq!(heuristic_title("Почини сборку пожалуйста"), "Почини Сборку Пожалуйста");
        assert_eq!(heuristic_title("ビルドを直して。次に"), "ビルドを直して");
        let long = "x".repeat(10_000);
        assert_eq!(heuristic_title(&long).chars().count(), 60);
    }

    #[test]
    fn all_filler_still_gives_something() {
        assert_eq!(heuristic_title("help me"), "Help Me");
    }
}
