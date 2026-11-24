//! The thread view's data with no GPUI in it: what the backend hands over for a page of history
//! ([`HistoryItem`], [`HistoryPage`]), how the core's stored transcript becomes those items
//! ([`items_from_entries`]), and the small rules the view follows: which link is a file
//! ([`file_target`]), how a duration reads ([`duration_label`]), what a generic tool card says
//! ([`tool_summary`]).
//!
//! A stored assistant message carries its reasoning; here they are two items (the reasoning first,
//! collapsed), as t3code shows a reasoning work entry above the reply. A stored message does not
//! keep how long the model thought, so a reasoning item from history has no duration (the live
//! turn measures its own).
//!
//! t3code: `apps/web/src/components/chat/MessagesTimeline.logic.ts` (rows from the thread's items),
//! `chat/MarkdownFileLink.tsx` and `markdown-links.ts` (which hrefs are files, `#L12` and `:12`
//! line suffixes), `TimelineWorkEntryRow.tsx` (reasoning: "Thought"), `MessagesTimeline.tsx`
//! (`Worked for 2.8s`: the duration's spelling).

use std::sync::Arc;

use hermes_core::transcript::{Entry, Speaker};
use serde_json::Value;

use super::tools::{ChangedFile, ToolCard, ToolKind, changed_files};

/// How many items a page of history holds (the first page, and each older one).
pub const PAGE: usize = 60;

/// An image attached to a user message: its bytes (decoded), for a thumbnail.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// What the thumbnail's tooltip says (`image 1`, or the file's name when known).
    pub name: String,
    /// `image/png`, `image/jpeg`, ...
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
}

/// One entry of a thread's history as the timeline shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryItem {
    /// What the user typed, with the images attached to it.
    User {
        text: String,
        images: Vec<Attachment>,
        at_ms: i64,
    },
    /// A reply (markdown).
    Assistant { text: String, at_ms: i64 },
    /// What the model thought before a reply; `duration_ms` when it was measured.
    Reasoning {
        text: String,
        duration_ms: Option<u64>,
    },
    /// A tool call and, once it has one, its result text (a call with none was stopped). `at_ms` is
    /// when its message was stored (second resolution).
    Tool {
        id: String,
        name: String,
        args: Value,
        result: Option<String>,
        /// The stored error flag (`transcript::ToolEntry::is_error`).
        is_error: bool,
        at_ms: i64,
    },
    /// The end of a turn: the files it changed and the stored rows its Retry and Branch actions
    /// name (the turn's typed prompt and its last reply).
    TurnFooter {
        files: Vec<ChangedFile>,
        user_row: Option<usize>,
        last_row: Option<usize>,
    },
    /// A failed turn's boundary ("Your request was not processed...").
    Failed { text: String },
}

/// A slice of a thread's history, oldest first: items `start..start + items.len()` of `total`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    /// Where `items` start in the whole history.
    pub start: usize,
    /// How many items the whole history has.
    pub total: usize,
}

impl HistoryPage {
    /// Whether there is history before this page.
    pub fn has_older(&self) -> bool {
        self.start > 0
    }
}

/// The page of `all` that ends before item `before` (the end when `None`), at most `limit` long.
pub fn page(all: &[HistoryItem], before: Option<usize>, limit: usize) -> HistoryPage {
    let end = before.unwrap_or(all.len()).min(all.len());
    let start = end.saturating_sub(limit);
    HistoryPage {
        items: all[start..end].to_vec(),
        start,
        total: all.len(),
    }
}

/// The timeline's items from the core's transcript entries. `images(row_id, item)` gives the
/// images attached to the stored message `row_id`, which becomes item number `item` (the
/// transcript's entries do not carry them; a caller that wants one page decodes only its own).
pub fn items_from_entries(
    entries: &[Entry],
    images: impl Fn(usize, usize) -> Vec<Attachment>,
) -> Vec<HistoryItem> {
    /// What the turn being read has so far, for its footer.
    #[derive(Default)]
    struct Turn {
        user_row: Option<usize>,
        last_row: Option<usize>,
        edits: Vec<ToolCard>,
    }
    fn close(turn: Turn, items: &mut Vec<HistoryItem>) {
        let files = changed_files(&turn.edits);
        // A turn that answered or changed files has a footer, as every live turn does (its
        // Branch; Retry is the newest turn's only, `ThreadModel::retry`).
        if turn.user_row.is_some() && (!files.is_empty() || turn.last_row.is_some()) {
            items.push(HistoryItem::TurnFooter {
                files,
                user_row: turn.user_row,
                last_row: turn.last_row,
            });
        }
    }
    let mut items = Vec::with_capacity(entries.len());
    let mut turn = Turn::default();
    for entry in entries {
        match entry {
            Entry::Tool(tool) => {
                if ToolKind::of(&tool.name) == ToolKind::Edit {
                    turn.edits.push(ToolCard::from_stored(
                        &tool.name,
                        &tool.args,
                        tool.result.as_deref(),
                        tool.is_error,
                    ));
                }
                items.push(HistoryItem::Tool {
                    id: tool.call_id.clone(),
                    name: tool.name.clone(),
                    args: tool.args.clone(),
                    result: tool.result.clone(),
                    is_error: tool.is_error,
                    at_ms: (tool.timestamp * 1000.) as i64,
                });
            }
            Entry::Message(message) => {
                let at_ms = (message.timestamp * 1000.) as i64;
                if message.failed_turn.is_some() {
                    items.push(HistoryItem::Failed {
                        text: message.text.clone(),
                    });
                    continue;
                }
                match message.speaker {
                    Speaker::User => {
                        close(std::mem::take(&mut turn), &mut items);
                        turn.user_row = Some(message.row_id);
                        items.push(HistoryItem::User {
                            text: message.text.clone(),
                            images: images(message.row_id, items.len()),
                            at_ms,
                        });
                    }
                    Speaker::Assistant => {
                        if let Some(reasoning) =
                            message.reasoning.as_ref().filter(|r| !r.is_empty())
                        {
                            items.push(HistoryItem::Reasoning {
                                text: reasoning.clone(),
                                duration_ms: None,
                            });
                        }
                        if !message.text.trim().is_empty() {
                            turn.last_row = Some(message.row_id);
                            items.push(HistoryItem::Assistant {
                                text: message.text.clone(),
                                at_ms,
                            });
                        }
                    }
                }
            }
        }
    }
    close(turn, &mut items);
    items
}

/// The one-line summary of a tool the cards do not know: its most telling argument (a path, a
/// command, a query, a URL), else nothing.
pub fn tool_summary(_name: &str, args: &Value) -> String {
    const KEYS: [&str; 8] = [
        "path",
        "file_path",
        "command",
        "query",
        "pattern",
        "url",
        "goal",
        "name",
    ];
    let Some(object) = args.as_object() else {
        return String::new();
    };
    KEYS.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(|text| text.lines().next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// How long something took, as t3code spells it: `0.4s`, `2.8s`, `12s`, `1m 12s`, `1h 3m`.
pub fn duration_label(ms: u64) -> String {
    let seconds = ms as f64 / 1000.;
    if seconds < 10. {
        format!("{seconds:.1}s")
    } else if seconds < 60. {
        format!("{}s", seconds.floor() as u64)
    } else if seconds < 3600. {
        let whole = seconds.floor() as u64;
        format!("{}m {}s", whole / 60, whole % 60)
    } else {
        let whole = seconds.floor() as u64;
        format!("{}h {}m", whole / 3600, (whole % 3600) / 60)
    }
}

/// The reasoning row's label: "Thinking" while it streams, "Thought for 2.8s" once measured,
/// "Thought" for one from history.
pub fn reasoning_label(live: bool, duration_ms: Option<u64>) -> String {
    match (live, duration_ms) {
        (true, _) => "Thinking".to_string(),
        (false, Some(ms)) => format!("Thought for {}", duration_label(ms)),
        (false, None) => "Thought".to_string(),
    }
}

/// A file a link in a message points at: what the right panel opens (Phase 6), with the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTarget {
    pub path: String,
    pub line: Option<u32>,
}

impl FileTarget {
    /// `path:line`, or the path alone.
    pub fn label(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

/// The file a link's `url` names, or `None` for a web (or other scheme's) link. Files are a
/// `file://` URI, a Windows path (`C:\x`, `C:/x`), an absolute or relative path (`src/main.rs`),
/// with an optional line: `#L12`, `#L12-L20` or `:12` (`:12:3` keeps the line).
pub fn file_target(url: &str) -> Option<FileTarget> {
    let url = url.trim();
    if url.is_empty() || url.starts_with('#') || url.starts_with("www.") {
        return None;
    }
    let path = if let Some(rest) = url.strip_prefix("file://") {
        let decoded = hermes_core::files::percent_decode(rest);
        // `file:///C:/x` keeps a slash before the drive letter.
        match decoded.strip_prefix('/') {
            Some(after) if is_drive_path(after) => after.to_string(),
            _ => decoded,
        }
    } else if has_scheme(url) {
        return None;
    } else {
        url.to_string()
    };
    let (path, line) = split_line(&path);
    (!path.is_empty()).then(|| FileTarget {
        path: path.to_string(),
        line,
    })
}

/// `C:\` or `C:/` at the start.
fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// A URL scheme (`https:`, `mailto:`): two or more letters before the colon, so a drive letter
/// is not one.
fn has_scheme(url: &str) -> bool {
    if is_drive_path(url) {
        return false;
    }
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The path and the line of `path#L12`, `path#L12-L20`, `path:12` or `path:12:3`.
fn split_line(path: &str) -> (&str, Option<u32>) {
    if let Some((before, anchor)) = path.rsplit_once("#L") {
        let digits: String = anchor.chars().take_while(char::is_ascii_digit).collect();
        return (before, digits.parse().ok());
    }
    let path = path.split('#').next().unwrap_or(path);
    // `:12` and `:12:3` at the end; a drive letter's colon is not a line.
    let mut rest = path;
    let mut numbers = Vec::new();
    while let Some((before, last)) = rest.rsplit_once(':') {
        if last.is_empty() || !last.chars().all(|c| c.is_ascii_digit()) || before.len() < 2 {
            break;
        }
        numbers.push(last);
        rest = before;
        if numbers.len() == 2 {
            break;
        }
    }
    match numbers.last() {
        Some(line) => (rest, line.parse().ok()),
        None => (path, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::transcript::{MessageEntry, ToolEntry};
    use serde_json::json;

    fn message(speaker: Speaker, text: &str, row_id: usize) -> Entry {
        Entry::Message(MessageEntry {
            speaker,
            text: text.into(),
            reasoning: None,
            timestamp: 1_700_000_000.,
            row_id,
            failed_turn: None,
        })
    }

    #[test]
    fn entries_become_items_with_reasoning_first_and_images_from_their_row() {
        let mut reply = message(Speaker::Assistant, "The answer", 2);
        if let Entry::Message(m) = &mut reply {
            m.reasoning = Some("Let me think".into());
        }
        let entries = vec![
            message(Speaker::User, "question", 1),
            reply,
            Entry::Tool(ToolEntry {
                name: "read_file".into(),
                args: json!({"path": "src/main.rs"}),
                call_id: "c1".into(),
                result: Some("fn main() {}".into()),
                is_error: false,
                timestamp: 0.,
            }),
        ];
        let image = Attachment {
            name: "image 1".into(),
            mime: "image/png".into(),
            bytes: Arc::new(vec![1, 2, 3]),
        };
        let items = items_from_entries(&entries, |row, _| {
            if row == 1 {
                vec![image.clone()]
            } else {
                Vec::new()
            }
        });
        assert_eq!(
            items,
            vec![
                HistoryItem::User {
                    text: "question".into(),
                    images: vec![image],
                    at_ms: 1_700_000_000_000
                },
                HistoryItem::Reasoning {
                    text: "Let me think".into(),
                    duration_ms: None
                },
                HistoryItem::Assistant {
                    text: "The answer".into(),
                    at_ms: 1_700_000_000_000
                },
                HistoryItem::Tool {
                    id: "c1".into(),
                    name: "read_file".into(),
                    args: json!({"path": "src/main.rs"}),
                    result: Some("fn main() {}".into()),
                    is_error: false,
                    at_ms: 0,
                },
                // The turn answered, so it has a footer (Branch; Retry on the newest).
                HistoryItem::TurnFooter {
                    files: vec![],
                    user_row: Some(1),
                    last_row: Some(2),
                },
            ]
        );
    }

    #[test]
    fn an_answered_turn_has_a_footer_with_its_files_and_an_unanswered_one_has_none() {
        let patch = |path: &str| {
            Entry::Tool(ToolEntry {
                name: "patch".into(),
                args: json!({"path": path, "old_string": "a", "new_string": "b"}),
                call_id: format!("p-{path}"),
                result: Some(
                    json!({"success": true, "diff": format!("--- a/{path}
+++ b/{path}
@@ -1,1 +1,1 @@
-a
+b
"), "files_modified": [path]})
                    .to_string(),
                ),
                is_error: false,
                timestamp: 0.,
            })
        };
        let entries = vec![
            message(Speaker::User, "fix it", 1),
            patch("src/a.rs"),
            message(Speaker::Assistant, "done", 4),
            message(Speaker::User, "thanks", 5),
            message(Speaker::Assistant, "welcome", 6),
            message(Speaker::User, "one more", 7),
        ];
        let items = items_from_entries(&entries, |_, _| Vec::new());
        let footers: Vec<(usize, &HistoryItem)> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i, HistoryItem::TurnFooter { .. }))
            .collect();
        assert_eq!(
            footers.len(),
            2,
            "the two answered turns: the newest has no answer yet"
        );
        assert!(matches!(
            footers[1].1,
            HistoryItem::TurnFooter { files, user_row: Some(5), last_row: Some(6) } if files.is_empty()
        ));
        assert!(matches!(
            footers[0].1,
            HistoryItem::TurnFooter { files, user_row: Some(1), last_row: Some(4) }
                if files.len() == 1 && files[0].path == "src/a.rs" && files[0].additions == 1
        ));
        assert_eq!(
            footers[0].0, 3,
            "after the turn's last item, before the next prompt"
        );
    }

    #[test]
    fn pages_end_where_asked_and_say_whether_there_is_more() {
        let all: Vec<HistoryItem> = (0..150)
            .map(|i| HistoryItem::Failed {
                text: i.to_string(),
            })
            .collect();
        let last = page(&all, None, PAGE);
        assert_eq!((last.start, last.items.len(), last.total), (90, 60, 150));
        assert!(last.has_older());
        let older = page(&all, Some(last.start), PAGE);
        assert_eq!((older.start, older.items.len()), (30, 60));
        let first = page(&all, Some(older.start), PAGE);
        assert_eq!((first.start, first.items.len()), (0, 30));
        assert!(!first.has_older());
        assert_eq!(page(&[], None, PAGE), HistoryPage::default());
    }

    #[test]
    fn links_to_files_are_told_from_web_links() {
        let file = |path: &str, line: Option<u32>| {
            Some(FileTarget {
                path: path.into(),
                line,
            })
        };
        assert_eq!(file_target("src/main.rs"), file("src/main.rs", None));
        assert_eq!(
            file_target("src/main.rs#L12"),
            file("src/main.rs", Some(12))
        );
        assert_eq!(
            file_target("src/main.rs#L12-L20"),
            file("src/main.rs", Some(12))
        );
        assert_eq!(file_target("src/main.rs:42"), file("src/main.rs", Some(42)));
        assert_eq!(
            file_target("src/main.rs:42:7"),
            file("src/main.rs", Some(42))
        );
        assert_eq!(
            file_target(r"C:\work\engine\lib.rs"),
            file(r"C:\work\engine\lib.rs", None)
        );
        assert_eq!(
            file_target("C:/work/engine/lib.rs:3"),
            file("C:/work/engine/lib.rs", Some(3))
        );
        assert_eq!(
            file_target("file:///C:/work/my%20file.rs"),
            file("C:/work/my file.rs", None)
        );
        for web in [
            "https://example.com/a.rs",
            "http://x",
            "mailto:me@example.com",
            "#section",
            "www.example.com",
            "",
        ] {
            assert_eq!(file_target(web), None, "{web}");
        }
        assert_eq!(
            FileTarget {
                path: "a.rs".into(),
                line: Some(3)
            }
            .label(),
            "a.rs:3"
        );
    }

    #[test]
    fn durations_read_like_t3codes() {
        assert_eq!(duration_label(400), "0.4s");
        assert_eq!(duration_label(2_800), "2.8s");
        assert_eq!(duration_label(12_400), "12s");
        assert_eq!(duration_label(72_000), "1m 12s");
        assert_eq!(duration_label(3_780_000), "1h 3m");
        assert_eq!(reasoning_label(true, Some(5)), "Thinking");
        assert_eq!(reasoning_label(false, Some(2_800)), "Thought for 2.8s");
        assert_eq!(reasoning_label(false, None), "Thought");
    }

    #[test]
    fn a_tool_line_names_its_most_telling_argument() {
        assert_eq!(
            tool_summary("terminal", &json!({"command": "cargo test\nmore"})),
            "cargo test"
        );
        assert_eq!(tool_summary("x", &json!({"other": 1})), "");
        assert_eq!(tool_summary("x", &json!("not an object")), "");
    }
}
