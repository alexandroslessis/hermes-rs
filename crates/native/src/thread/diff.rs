//! The mini diff of a file edit card, with no GPUI in it: lines marked added, removed, context or
//! hunk header, and the counts. Three sources, all from what the tools really send:
//! - `patch`'s result carries a unified diff (`similar`, three lines of context): [`parse_unified`];
//! - the same edit while it is still running, or stored without its result: [`from_strings`] over
//!   the call's `old_string` / `new_string` (no surrounding context: the file is not read here);
//! - `write_file` carries the whole content and the tool does not say what was there before: every
//!   line is an addition ([`all_added`]).
//!
//! t3code: `apps/web/src/components/chat/DiffStatLabel.tsx` (the `+N -M` counts and their
//! spelling), `ChangedFilesTree.tsx` (where the counts are shown).

use similar::{ChangeTag, TextDiff};

/// How a diff line is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Add,
    Del,
    Context,
    /// `@@ -a,b +c,d @@`
    Hunk,
}

/// One line of a diff, with the line numbers it has (an addition has only the new one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    pub text: String,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
}

/// A diff and its counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub additions: u32,
    pub deletions: u32,
}

impl Diff {
    fn push(&mut self, kind: Kind, text: &str, old_no: Option<u32>, new_no: Option<u32>) {
        match kind {
            Kind::Add => self.additions += 1,
            Kind::Del => self.deletions += 1,
            _ => {}
        }
        self.lines.push(Line {
            kind,
            text: text.to_string(),
            old_no,
            new_no,
        });
    }
}

/// The numbers of a hunk header `@@ -12,3 +14,4 @@ ...`: the old and the new start.
fn hunk_starts(header: &str) -> (u32, u32) {
    let number = |sign: char| {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(sign))
            .and_then(|part| part.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    };
    (number('-'), number('+'))
}

/// A unified diff's lines: its `---` / `+++` file headers are dropped, hunk headers kept.
pub fn parse_unified(text: &str) -> Diff {
    let mut diff = Diff::default();
    let (mut old_no, mut new_no) = (1, 1);
    let mut in_hunk = false;
    for line in text.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            (old_no, new_no) = hunk_starts(line);
            diff.push(Kind::Hunk, line, None, None);
            continue;
        }
        // Before the first hunk, `--- a/x` and `+++ b/x` are headers (inside one, a removed line
        // that starts with `--` is data).
        if !in_hunk {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            diff.push(Kind::Add, rest, None, Some(new_no));
            new_no += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            diff.push(Kind::Del, rest, Some(old_no), None);
            old_no += 1;
        } else if let Some(rest) = line.strip_prefix(' ') {
            diff.push(Kind::Context, rest, Some(old_no), Some(new_no));
            old_no += 1;
            new_no += 1;
        } else if line.is_empty() {
            // An empty context line whose leading space was trimmed.
            diff.push(Kind::Context, "", Some(old_no), Some(new_no));
            old_no += 1;
            new_no += 1;
        }
        // `\ No newline at end of file` and anything else: not a line of the file.
    }
    diff
}

/// A line diff of `old` against `new`, with no context lines around the changes.
pub fn from_strings(old: &str, new: &str) -> Diff {
    let mut diff = Diff::default();
    let (mut old_no, mut new_no) = (1u32, 1u32);
    for change in TextDiff::from_lines(old, new).iter_all_changes() {
        let text = change.value().trim_end_matches(['\n', '\r']);
        match change.tag() {
            ChangeTag::Equal => {
                old_no += 1;
                new_no += 1;
            }
            ChangeTag::Delete => {
                diff.push(Kind::Del, text, Some(old_no), None);
                old_no += 1;
            }
            ChangeTag::Insert => {
                diff.push(Kind::Add, text, None, Some(new_no));
                new_no += 1;
            }
        }
    }
    diff
}

/// Every line of `content` as an addition.
pub fn all_added(content: &str) -> Diff {
    let mut diff = Diff::default();
    for (i, line) in content.lines().enumerate() {
        diff.push(Kind::Add, line, None, Some(i as u32 + 1));
    }
    diff
}

/// A count as t3code spells it: `812`, `1.2k`, `13k`, `1.5m`.
pub fn compact_count(value: u32) -> String {
    let value = u64::from(value);
    if value < 1000 {
        return value.to_string();
    }
    let scaled = |d: f64, unit: &str| {
        let v = value as f64 / d;
        if v < 10. {
            format!("{}{unit}", format!("{v:.1}").trim_end_matches(".0"))
        } else {
            format!("{}{unit}", v.round() as u64)
        }
    };
    if value < 999_500 {
        scaled(1000., "k")
    } else {
        scaled(1_000_000., "m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIFIED: &str = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,5 +3,5 @@\n fn a() {}\n-fn b() {}\n+fn b() { todo() }\n+// new\n \n fn c() {}\n--- a deleted line that looks like a header\n\\ No newline at end of file\n";

    #[test]
    fn a_unified_diff_keeps_hunks_and_numbers_its_lines() {
        let diff = parse_unified(UNIFIED);
        assert_eq!((diff.additions, diff.deletions), (2, 2));
        let kinds: Vec<Kind> = diff.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Hunk,
                Kind::Context,
                Kind::Del,
                Kind::Add,
                Kind::Add,
                Kind::Context,
                Kind::Context,
                Kind::Del
            ]
        );
        let del = &diff.lines[2];
        assert_eq!(
            (del.old_no, del.new_no, del.text.as_str()),
            (Some(4), None, "fn b() {}")
        );
        let add = &diff.lines[4];
        assert_eq!((add.old_no, add.new_no), (None, Some(5)));
        // The header-looking removed line is data once a hunk has begun.
        assert_eq!(
            diff.lines[7].text,
            "-- a deleted line that looks like a header"
        );
        assert_eq!(diff.lines[7].old_no, Some(7));
    }

    #[test]
    fn text_before_the_first_hunk_and_an_empty_diff_are_nothing() {
        assert_eq!(parse_unified("--- a/x\n+++ b/x\n"), Diff::default());
        assert_eq!(parse_unified(""), Diff::default());
    }

    #[test]
    fn a_diff_of_two_strings_has_the_changed_lines_numbered() {
        let diff = from_strings("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!((diff.additions, diff.deletions), (2, 1));
        let shown: Vec<(Kind, &str, Option<u32>, Option<u32>)> = diff
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str(), l.old_no, l.new_no))
            .collect();
        assert_eq!(
            shown,
            [
                (Kind::Del, "b", Some(2), None),
                (Kind::Add, "B", None, Some(2)),
                (Kind::Add, "d", None, Some(4)),
            ]
        );
        assert_eq!(from_strings("same\n", "same\n"), Diff::default());
    }

    #[test]
    fn new_content_is_all_additions() {
        let diff = all_added("one\ntwo\n");
        assert_eq!(
            (diff.additions, diff.deletions, diff.lines.len()),
            (2, 0, 2)
        );
        assert_eq!(diff.lines[1].new_no, Some(2));
        assert_eq!(all_added(""), Diff::default());
    }

    #[test]
    fn counts_read_like_t3codes() {
        let spelled: Vec<String> = [
            0, 7, 999, 1000, 1234, 9999, 10_000, 13_400, 999_499, 1_500_000,
        ]
        .into_iter()
        .map(compact_count)
        .collect();
        assert_eq!(
            spelled,
            [
                "0", "7", "999", "1k", "1.2k", "10k", "10k", "13k", "999k", "1.5m"
            ]
        );
    }
}
