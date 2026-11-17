//! Fake conversation data for the GPUI Kit spike (CP0.1 of `docs/native-ui/PHASES.md`): a
//! deterministic mix of message kinds (short and long prose, lists, tables, links, headings and
//! fenced code in four languages) so the timeline sees rows of very different heights.

/// Who wrote a fake message; the spike shows it above the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The person typing in the composer.
    User,
    /// The agent's answer, rendered as markdown.
    Assistant,
}

/// One message of the fake conversation.
#[derive(Clone, Debug)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// Markdown source.
    pub text: String,
}

/// The four fenced-code languages the spike shows highlighted.
pub const CODE_LANGUAGES: [&str; 4] = ["rust", "python", "typescript", "bash"];

/// Words the streaming message is built from, appended a few at a time.
pub const STREAM_WORDS: &[&str] = &[
    "The",
    "agent",
    "reads",
    "the",
    "file,",
    "finds",
    "the",
    "failing",
    "test",
    "and",
    "writes",
    "a",
    "patch.",
    "Then",
    "it",
    "runs",
    "`cargo",
    "test`",
    "again",
    "to",
    "check",
    "the",
    "fix",
    "holds",
    "under",
    "load.",
    "Streaming",
    "text",
    "must",
    "keep",
    "the",
    "list",
    "pinned",
    "to",
    "the",
    "bottom",
    "while",
    "it",
    "grows.",
];

/// A tiny linear congruential generator, so the fake conversation is the same on every run
/// (the snapshot harness of CP0.2 will need that) without a `rand` dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }
}

/// `count` messages alternating user and assistant, with assistant messages of seven kinds.
pub fn conversation(count: usize) -> Vec<Message> {
    let mut rng = Lcg(0x5eed);
    (0..count)
        .map(|ix| {
            if ix % 2 == 0 {
                Message {
                    role: Role::User,
                    text: user_text(ix, &mut rng),
                }
            } else {
                Message {
                    role: Role::Assistant,
                    text: assistant_text(ix, &mut rng),
                }
            }
        })
        .collect()
}

fn user_text(ix: usize, rng: &mut Lcg) -> String {
    match rng.below(3) {
        0 => format!("Message {ix}: can you fix the failing test?"),
        1 => format!(
            "Message {ix}: the build is slow on Windows. Look at `Cargo.toml` and tell me which \
             profile settings matter, and whether incremental builds are on."
        ),
        _ => format!("Message {ix}: run it again with logging on, then summarise."),
    }
}

fn assistant_text(ix: usize, rng: &mut Lcg) -> String {
    let lang = CODE_LANGUAGES[rng.below(4)];
    match rng.below(7) {
        0 => format!("Done ({ix}). The test passes now."),
        1 => format!(
            "## Result {ix}\n\nThe failure came from a race between the writer and the reader. \
             I moved the flush before the notification, so a reader that wakes up always sees \
             the row. See [the pitfalls](https://example.com/pitfalls) for the background.\n\n\
             A second paragraph makes this message taller than the others, so the virtual list \
             has to measure rows of different heights instead of assuming one."
        ),
        2 => format!(
            "Steps taken for {ix}:\n\n1. Read `src/lib.rs`\n2. Ran the tests\n   - 3 failed\n   \
             - 1 was flaky\n3. Fixed the order of writes\n\n- **bold** item\n- *italic* item\n\
             - `inline code` item"
        ),
        3 => format!(
            "### Timings {ix}\n\n| Step | Before | After |\n|---|---:|---:|\n| build | 41 s | 12 s \
             |\n| test | 9 s | 9 s |\n| link | 6 s | 2 s |\n\nAll numbers are from a warm cache."
        ),
        4 | 5 => format!("Here is the change ({ix}):\n\n{}", code_block(lang)),
        _ => format!(
            "# Summary {ix}\n\n> Quoted from the log: the run finished.\n\nNext, compare with \
             [the plan](https://example.com/plan) and check:\n\n- [x] tests\n- [ ] docs\n\n{}",
            code_block(CODE_LANGUAGES[(ix / 2) % 4])
        ),
    }
}

/// A fenced code block in `lang` (one of [`CODE_LANGUAGES`]).
pub fn code_block(lang: &str) -> String {
    let body = match lang {
        "rust" => {
            "fn main() {\n    let items = vec![1, 2, 3];\n    for item in &items {\n        \
             println!(\"item {item}\");\n    }\n}"
        }
        "python" => {
            "def total(items: list[int]) -> int:\n    \"\"\"Sum the items.\"\"\"\n    \
             return sum(i for i in items if i > 0)\n\nprint(total([1, -2, 3]))"
        }
        "typescript" => {
            "interface Item { id: number; name: string }\n\nexport function names(items: \
             Item[]): string[] {\n  return items.map((item) => item.name)\n}"
        }
        _ => {
            "#!/usr/bin/env bash\nset -euo pipefail\nfor f in *.rs; do\n  echo \"checking $f\"\ndone"
        }
    };
    format!("```{lang}\n{body}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversation_is_deterministic_and_mixed() {
        let a = conversation(2_000);
        let b = conversation(2_000);
        assert_eq!(a.len(), 2_000);
        assert!(a.iter().zip(&b).all(|(x, y)| x.text == y.text));
        assert_eq!(a[0].role, Role::User);
        assert_eq!(a[1].role, Role::Assistant);
        let texts: Vec<&str> = a.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("| Step |")), "a table");
        assert!(texts.iter().any(|t| t.contains("](https://")), "a link");
        assert!(texts.iter().any(|t| t.starts_with('#')), "a heading");
        for lang in CODE_LANGUAGES {
            assert!(
                texts.iter().any(|t| t.contains(&format!("```{lang}\n"))),
                "fenced {lang}"
            );
        }
    }
}
