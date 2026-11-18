//! The page's Import screen over sessions from the original Hermes: `session.foreign.list`,
//! `session.foreign.preview` and `session.foreign.import` (Hermes's RPCs, built for Claude Code and
//! Codex sessions; here they list Hermes's own). The reading of Hermes's `state.db` and the import
//! itself are `hermes_core::hermes_import`; this file is the page's JSON.
//!
//! Hermes: `tui_gateway/methods_session_foreign.py` (the RPCs).

use std::path::Path;

use hermes_core::hermes_import::{self, ForeignPage};
use serde_json::{Value, json};

pub use hermes_core::hermes_import::{Target, import_all, sources};

/// `ForeignPage`.
fn page(page: &ForeignPage) -> Value {
    json!({
        "sessions": page.sessions.iter().map(|row| json!({
            "id": row.id, "source": "hermes", "label": row.label, "title": row.title,
            "cwd": row.cwd, "mtime": row.mtime, "turn_count": row.turn_count,
            "excerpt": row.excerpt, "already_imported": row.already_imported,
        })).collect::<Vec<_>>(),
        "next_offset": page.next_offset,
        "host": page.host,
        "unreadable": page.unreadable,
    })
}

/// `session.foreign.list` `{source, offset}` (`ForeignPage`): the Hermes sessions here, newest
/// first.
pub fn list(data_dir: &Path, source: Option<&str>, offset: usize) -> Value {
    page(&hermes_import::list(data_dir, source, offset))
}

/// `session.foreign.preview` `{id}` (`ForeignPreview`): the conversation's text.
pub fn preview(data_dir: &Path, id: &str) -> Result<Value, String> {
    let preview = hermes_import::preview(data_dir, id).map_err(|e| e.0)?;
    Ok(json!({
        "truncated": preview.truncated,
        "total": preview.total,
        "messages": preview.messages.iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
        "already_imported": preview.already_imported,
        "cwd": preview.cwd,
    }))
}

/// `session.foreign.import` `{id}`: the session, made one of ours (or the one made before).
pub fn import(target: &Target, id: &str) -> Result<Value, String> {
    let done = hermes_import::import(target, id).map_err(|e| e.0)?;
    Ok(json!({"session_id": done.session_id, "already_imported": done.already_imported}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_or_codex_list_is_empty_in_the_page_s_shape() {
        let dir = hermes_testdir::tempdir().unwrap();
        let listed = list(dir.path(), Some("claude"), 0);
        assert_eq!(listed["sessions"], json!([]));
        assert_eq!(listed["next_offset"], Value::Null);
        assert_eq!(listed["unreadable"], 0);
        assert!(listed["host"].is_string());
    }

    #[test]
    fn a_session_key_that_is_not_hermes_s_is_refused_with_its_text() {
        let dir = hermes_testdir::tempdir().unwrap();
        assert_eq!(
            preview(dir.path(), "codex:x").unwrap_err(),
            "Not a Hermes session: codex:x"
        );
    }
}
