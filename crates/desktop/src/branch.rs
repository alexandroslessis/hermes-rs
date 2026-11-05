//! `/branch`: a new session that starts with this one's conversation (up to a message, or all
//! of it), listed under it in the sidebar. The store's schema (shared with the GPUI app) has no
//! parent column, so the links live in the data folder's `branches.json`.
//!
//! Hermes: `tui_gateway/methods_session_branch.py` (`session.branch`, `session.branch_whole`,
//! `session.branch_stored`); Hermes copies the visible user/assistant text only, this copies the
//! messages as stored (tool calls and results too), so the branch keeps the working context.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hermes_llm::conversation::message::{Message, MessageContent};
use std::sync::Mutex;
use rmcp::model::Role;

const FILE: &str = "branches.json";

/// Child session → parent session, kept in `branches.json`.
pub struct Links {
    path: PathBuf,
    parents: Mutex<HashMap<String, String>>,
}

impl Links {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let parents = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, parents: Mutex::new(parents) }
    }

    pub fn parent(&self, child: &str) -> Option<String> {
        self.parents.lock().unwrap_or_else(|e| e.into_inner()).get(child).cloned()
    }

    pub fn link(&self, child: &str, parent: &str) {
        let mut parents = self.parents.lock().unwrap_or_else(|e| e.into_inner());
        parents.insert(child.to_string(), parent.to_string());
        if let Ok(bytes) = serde_json::to_vec_pretty(&*parents) {
            let _ = std::fs::write(&self.path, bytes);
        }
    }
}

/// The messages a branch starts with: the first `keep`, without a trailing tool call that has no
/// result (a cut mid-step would leave the history invalid for the next request).
pub fn cut(messages: &[Message], keep: usize) -> Vec<Message> {
    let mut kept: Vec<Message> = messages[..keep.min(messages.len())].to_vec();
    while kept.last().is_some_and(|last| {
        last.role == Role::Assistant
            && last.content.iter().any(|c| matches!(c, MessageContent::ToolRequest(_)))
    }) {
        kept.pop();
    }
    kept
}

/// The title of a new branch of `parent`: `<parent> (branch)`, numbered when that's taken.
pub fn title(parent: Option<&str>, taken: &[String]) -> String {
    let base = format!("{} (branch)", parent.filter(|t| !t.trim().is_empty()).unwrap_or("Chat"));
    if !taken.contains(&base) {
        return base;
    }
    let stem = base.trim_end_matches(')').to_string();
    (2..)
        .map(|n| format!("{stem} {n})"))
        .find(|candidate| !taken.contains(candidate))
        .expect("a free title")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;

    #[test]
    fn a_cut_never_ends_on_an_unanswered_tool_call() {
        let call = Message::assistant().with_tool_request("c1", Ok(CallToolRequestParams::new("read_file")));
        let messages = vec![
            Message::user().with_text("hi"),
            Message::assistant().with_text("hello"),
            Message::user().with_text("read it"),
            call,
        ];
        assert_eq!(cut(&messages, 2).len(), 2);
        assert_eq!(cut(&messages, 4).len(), 3, "the unanswered call goes");
        assert_eq!(cut(&messages, 99).len(), 3);
        assert!(cut(&messages, 0).is_empty());
    }

    #[test]
    fn titles_follow_the_parent() {
        assert_eq!(title(Some("Fix the parser"), &[]), "Fix the parser (branch)");
        let taken = vec!["Fix the parser (branch)".to_string(), "Fix the parser (branch 2)".to_string()];
        assert_eq!(title(Some("Fix the parser"), &taken), "Fix the parser (branch 3)");
        assert_eq!(title(None, &[]), "Chat (branch)");
    }

    #[test]
    fn links_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        Links::load(dir.path()).link("child", "parent");
        assert_eq!(Links::load(dir.path()).parent("child").as_deref(), Some("parent"));
        assert_eq!(Links::load(dir.path()).parent("other"), None);
    }
}
