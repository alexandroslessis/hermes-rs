//! The work log's data: consecutive tool calls fold into one [`WorkGroup`] row ("Worked for 1m 12s
//! 8 steps"), each call a [`ToolEntry`] holding its card and what is open; a turn ends in a
//! [`Footer`] (the files it changed, its usage, Retry and Branch). No drawing here: the cards are
//! `cards.rs`, the model puts them in rows.
//!
//! The rules, as t3code folds its work entries (`MessagesTimeline.logic.ts`: one `work-toggle` row
//! per run of tool entries, `work-live` while one runs, a lone tool call shown as itself):
//! - a run is the tool calls with nothing between them (a reply, a reasoning row or a notice ends
//!   it); one call is not folded, it is the card;
//! - while any call of a group runs, its label is "Working for 12s", counting from the first
//!   call's start; when all are done it is "Worked for 1m 12s", from the first start to the last
//!   end; a group read from the store has only the messages' times (second resolution): the span
//!   from its first call's message to its last, and the last command's own duration when it has
//!   one;
//! - the steps are the calls: "8 steps", "1 step".
//!
//! t3code: `apps/web/src/components/chat/TimelineWorkGroupRows.tsx` (`WorkGroupToggleTimelineRow`,
//! `WorkingTimelineRow`), `MessagesTimeline.logic.ts` (`Worked for ${duration}`), `ChangedFilesTree.tsx`.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::UniformListScrollHandle;

use super::data::duration_label;
use super::tools::{ChangedFile, Node, Status, ToolCard, build_tree, dir_paths};

/// A tool call in a group.
pub struct ToolEntry {
    /// Stable for the entry's life (element ids).
    pub key: u64,
    pub call_id: String,
    pub card: ToolCard,
    pub open: bool,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    /// The scroll position of the card's output list (it follows the tail while the call runs).
    pub scroll: UniformListScrollHandle,
}

impl ToolEntry {
    pub fn new(key: u64, call_id: &str, card: ToolCard) -> Self {
        Self {
            key,
            call_id: call_id.to_string(),
            card,
            open: false,
            started_ms: None,
            ended_ms: None,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

/// A run of tool calls.
#[derive(Default)]
pub struct WorkGroup {
    pub entries: Vec<ToolEntry>,
    pub open: bool,
    /// The span of a group read from the store (see the module docs).
    pub stored_span_ms: Option<u64>,
}

/// What a group's header says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// "Worked for 1m 12s", "Working for 12s", "Worked".
    pub label: String,
    /// "8 steps".
    pub steps: String,
    pub running: bool,
    /// The latest call failed (t3code marks a group by its latest tool call).
    pub failed: bool,
}

impl WorkGroup {
    pub fn is_single(&self) -> bool {
        self.entries.len() == 1
    }

    pub fn running(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.card.status == Status::Running)
    }

    /// The header at time `now_ms`.
    pub fn summary(&self, now_ms: i64) -> Summary {
        let running = self.running();
        let started = self.entries.iter().filter_map(|e| e.started_ms).min();
        let ended = self.entries.iter().filter_map(|e| e.ended_ms).max();
        let span = match (running, started, ended) {
            (true, Some(start), _) => Some(now_ms.saturating_sub(start).max(0) as u64),
            (false, Some(start), Some(end)) => Some(end.saturating_sub(start).max(0) as u64),
            _ => self.stored_span_ms,
        };
        let label = match (running, span) {
            (true, Some(ms)) => format!("Working for {}", duration_label(ms)),
            (true, None) => "Working".to_string(),
            (false, Some(ms)) if ms > 0 => format!("Worked for {}", duration_label(ms)),
            (false, _) => "Worked".to_string(),
        };
        let n = self.entries.len();
        Summary {
            label,
            steps: format!("{n} {}", if n == 1 { "step" } else { "steps" }),
            running,
            failed: self
                .entries
                .last()
                .is_some_and(|e| e.card.status == Status::Failed),
        }
    }

    /// The span of calls read from the store: the first call's message to the last one's, plus
    /// the last command's own duration.
    pub fn stored_span(first_ms: i64, last_ms: i64, last: &ToolCard) -> Option<u64> {
        let span = last_ms.saturating_sub(first_ms).max(0) as u64 + last.duration_ms.unwrap_or(0);
        (span > 0).then_some(span)
    }
}

/// What a turn used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
}

impl Usage {
    /// "12.3k in, 480 out".
    pub fn label(&self) -> String {
        let spell = |n: i64| super::diff::compact_count(n.clamp(0, i64::from(u32::MAX)) as u32);
        format!("{} in, {} out", spell(self.input), spell(self.output))
    }
}

/// The end of a turn.
pub struct Footer {
    pub files: Arc<Vec<ChangedFile>>,
    pub tree: Arc<Vec<Node>>,
    /// Folders the user closed.
    pub closed: HashSet<String>,
    /// A live turn's usage and duration (the store keeps neither).
    pub usage: Option<Usage>,
    pub elapsed_ms: Option<u64>,
    /// The stored rows Retry (the typed prompt) and Branch (the last reply) name; `None` when
    /// the turn is not stored (a replay).
    pub user_row: Option<usize>,
    pub last_row: Option<usize>,
    /// A Retry or Branch is being done: the buttons wait.
    pub busy: bool,
}

impl Footer {
    pub fn new(
        files: Vec<ChangedFile>,
        usage: Option<Usage>,
        elapsed_ms: Option<u64>,
        user_row: Option<usize>,
        last_row: Option<usize>,
    ) -> Self {
        let tree = build_tree(&files);
        Self {
            files: Arc::new(files),
            tree: Arc::new(tree),
            closed: HashSet::new(),
            usage,
            elapsed_ms,
            user_row,
            last_row,
            busy: false,
        }
    }

    /// Replaces the files (git's account of a turn instead of the edit cards'): the tree is
    /// rebuilt, the folders the user closed stay closed.
    pub fn replace_files(&mut self, files: Vec<ChangedFile>) {
        self.tree = Arc::new(build_tree(&files));
        self.files = Arc::new(files);
    }

    /// Whether every folder is closed.
    pub fn all_closed(&self) -> bool {
        let dirs = dir_paths(&self.tree);
        !dirs.is_empty() && dirs.iter().all(|d| self.closed.contains(d))
    }

    /// Opens every folder, or closes them all when all are open.
    pub fn toggle_all(&mut self) {
        if self.all_closed() {
            self.closed.clear();
        } else {
            self.closed = dir_paths(&self.tree).into_iter().collect();
        }
    }

    pub fn has_dirs(&self) -> bool {
        self.tree.iter().any(|n| matches!(n, Node::Dir { .. }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(key: u64, done: bool, start: i64, end: Option<i64>) -> ToolEntry {
        let mut card = ToolCard::started("read_file", &json!({"path": "a"}));
        if done {
            card.finish(&json!({"content": "x", "total_lines": 1}).to_string(), false);
        }
        let mut e = ToolEntry::new(key, &format!("c{key}"), card);
        e.started_ms = Some(start);
        e.ended_ms = end;
        e
    }

    #[test]
    fn a_running_group_counts_from_its_first_start_and_an_ended_one_to_its_last_end() {
        let mut group = WorkGroup::default();
        group.entries.push(entry(1, true, 1_000, Some(3_000)));
        group.entries.push(entry(2, false, 3_500, None));
        let live = group.summary(13_000);
        assert_eq!(
            (live.label.as_str(), live.steps.as_str(), live.running),
            ("Working for 12s", "2 steps", true)
        );
        group.entries[1]
            .card
            .finish(&json!({"content": "y"}).to_string(), false);
        group.entries[1].ended_ms = Some(73_000);
        let ended = group.summary(999_999);
        assert_eq!(
            (ended.label.as_str(), ended.steps.as_str(), ended.running),
            ("Worked for 1m 12s", "2 steps", false)
        );
    }

    #[test]
    fn steps_are_singular_for_one_and_the_latest_failure_marks_the_group() {
        let mut group = WorkGroup::default();
        group.entries.push(entry(1, true, 0, Some(400)));
        assert_eq!(group.summary(0).steps, "1 step");
        assert!(group.is_single());
        let mut bad = entry(2, false, 500, None);
        bad.card
            .finish(&json!({"success": false, "error": "no"}).to_string(), true);
        bad.ended_ms = Some(600);
        group.entries.push(bad);
        assert!(group.summary(0).failed);
        assert!(!group.is_single());
        assert_eq!(group.summary(0).label, "Worked for 0.6s");
    }

    #[test]
    fn a_stored_group_uses_the_messages_times_and_says_worked_when_it_has_none() {
        let mut group = WorkGroup::default();
        let mut e = ToolEntry::new(
            1,
            "c1",
            ToolCard::from_stored("read_file", &json!({"path": "a"}), Some("{}"), false),
        );
        e.started_ms = None;
        group.entries.push(e);
        assert_eq!(group.summary(0).label, "Worked");
        group.stored_span_ms = WorkGroup::stored_span(10_000, 14_000, &group.entries[0].card);
        assert_eq!(group.summary(0).label, "Worked for 4.0s");
        let command = ToolCard::from_stored(
            "terminal",
            &json!({"command": "x"}),
            Some(&json!({"output": "", "exit_code": 0, "duration_ms": 2_000}).to_string()),
            false,
        );
        assert_eq!(WorkGroup::stored_span(5, 5, &command), Some(2_000));
        assert_eq!(WorkGroup::stored_span(5, 5, &group.entries[0].card), None);
    }

    #[test]
    fn a_footer_opens_and_closes_all_its_folders() {
        let files = vec![
            ChangedFile {
                path: "a/b.rs".into(),
                additions: 1,
                deletions: 0,
            },
            ChangedFile {
                path: "c.rs".into(),
                additions: 2,
                deletions: 1,
            },
        ];
        let mut footer = Footer::new(files, None, None, Some(1), Some(2));
        assert!(footer.has_dirs());
        assert!(!footer.all_closed());
        footer.toggle_all();
        assert!(footer.all_closed());
        footer.toggle_all();
        assert!(footer.closed.is_empty());
        let flat = Footer::new(
            vec![ChangedFile {
                path: "c.rs".into(),
                additions: 1,
                deletions: 0,
            }],
            None,
            None,
            None,
            None,
        );
        assert!(!flat.has_dirs());
        assert_eq!(
            Usage {
                input: 12_345,
                output: 480
            }
            .label(),
            "12k in, 480 out"
        );
    }
}
