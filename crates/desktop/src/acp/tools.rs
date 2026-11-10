//! A tool call as ACP shows it: its kind (the editor's icon), a short title, the files it touches,
//! and the content of its result (a diff for an edit, the output of a command). Pure functions;
//! `translate.rs` calls them.
//!
//! Hermes: `acp_adapter/tools.py` (`TOOL_KIND_MAP`, `build_tool_title`, `extract_locations`,
//! `build_tool_complete`, `_parse_unified_diff_content`, `_tool_result_failed`), with *our* tool
//! names (`todo_list`, `process_manage`, …). Unlike Hermes, a file write's "before" text is read by
//! the server when the call starts (`server.rs`), since our `write_file` result carries no diff.

use std::path::Path;

use serde_json::{Value, json};

/// The longest result text shown in a card; the rest is cut (Hermes: 5000).
const RESULT_LIMIT: usize = 5000;

/// Our tool → ACP `ToolKind`; anything unlisted (MCP tools included) is `other`.
pub fn kind(name: &str) -> &'static str {
    match name {
        "read_file" | "skill_view" | "skills_list" | "vision_analyze" | "video_analyze" => "read",
        "write_file" | "patch" | "skill_manage" => "edit",
        "search_files" | "session_search" => "search",
        "terminal" | "process_manage" | "delegate_task" => "execute",
        "web_search" | "web_extract" => "fetch",
        _ => "other",
    }
}

fn text_arg<'a>(args: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or_default()
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// The card's header: short, one line.
pub fn title(name: &str, args: &Value) -> String {
    let path = text_arg(args, &["path", "file_path", "file"]);
    match name {
        "terminal" => {
            let command = text_arg(args, &["command"]);
            let first = command.lines().next().unwrap_or_default();
            if first.is_empty() {
                "Run a command".to_string()
            } else {
                clip(first, 80)
            }
        }
        "read_file" => {
            let from = args.get("offset").and_then(Value::as_u64);
            let count = args.get("limit").and_then(Value::as_u64);
            match (from, count) {
                (Some(from), Some(count)) if count > 0 => {
                    format!("Read {path} (lines {from}\u{2013}{})", from + count - 1)
                }
                _ => format!("Read {path}"),
            }
        }
        "write_file" => format!("Write {path}"),
        "patch" => format!("Edit {path}"),
        "search_files" => {
            let pattern = clip(text_arg(args, &["pattern", "query"]), 60);
            if path.is_empty() {
                format!("Search \"{pattern}\"")
            } else {
                format!("Search \"{pattern}\" in {path}")
            }
        }
        "web_search" => format!("Web search \"{}\"", clip(text_arg(args, &["query"]), 60)),
        "web_extract" => {
            let url = args
                .get("urls")
                .and_then(Value::as_array)
                .and_then(|urls| urls.first())
                .and_then(Value::as_str)
                .unwrap_or_else(|| text_arg(args, &["url"]));
            format!("Fetch {}", clip(url, 80))
        }
        "delegate_task" => format!("Delegate: {}", clip(text_arg(args, &["goal"]), 60)),
        other => other.replace('_', " "),
    }
}

/// An absolute Windows path for `path` (relative ones against the session's folder): what the
/// editor opens when the card's location is clicked.
pub fn absolute(path: &str, cwd: &str) -> String {
    let given = Path::new(path);
    let full = if given.is_absolute() {
        given.to_path_buf()
    } else {
        Path::new(cwd).join(given)
    };
    full.display().to_string().replace('/', "\\")
}

/// The file a call names, as `ToolCallLocation`s (with the line a read starts at).
pub fn locations(args: &Value, cwd: &str) -> Vec<Value> {
    let path = text_arg(args, &["path", "file_path", "file"]);
    if path.is_empty() {
        return Vec::new();
    }
    let mut location = json!({"path": absolute(path, cwd)});
    if let Some(line) = args
        .get("offset")
        .and_then(Value::as_u64)
        .filter(|l| *l >= 1)
    {
        location["line"] = json!(line);
    }
    vec![location]
}

/// A write whose "before" the server must read when the call starts: `write_file` only (a
/// `patch` result carries its own diff).
pub fn snapshot_path(name: &str, args: &Value, cwd: &str) -> Option<String> {
    if name != "write_file" {
        return None;
    }
    let path = text_arg(args, &["path"]);
    (!path.is_empty()).then(|| absolute(path, cwd))
}

/// Did the tool report a failure? Our tools answer `{"success": false, "error": …}`.
pub fn failed(result: &Value) -> bool {
    let Some(object) = result.as_object() else {
        return false;
    };
    object.get("success") == Some(&Value::Bool(false))
        || object.get("error").is_some_and(|error| match error {
            Value::Null => false,
            Value::String(text) => !text.is_empty(),
            _ => true,
        })
}

/// The result's text for a card, cut at [`RESULT_LIMIT`].
fn result_text(result: &Value, raw: &str) -> String {
    let text = match result {
        Value::Object(object) => match (object.get("output"), object.get("error")) {
            (Some(Value::String(output)), _) => {
                let mut text = output.clone();
                if let Some(code) = object
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .filter(|c| *c != 0)
                {
                    text.push_str(&format!("\n(exit code {code})"));
                }
                text
            }
            (_, Some(Value::String(error))) => error.clone(),
            _ => serde_json::to_string_pretty(result).unwrap_or_else(|_| raw.to_string()),
        },
        Value::String(text) => text.clone(),
        _ => raw.to_string(),
    };
    if text.chars().count() > RESULT_LIMIT {
        let mut cut: String = text.chars().take(RESULT_LIMIT).collect();
        cut.push_str("\n\u{2026} (truncated)");
        return cut;
    }
    text
}

fn text_content(text: &str) -> Value {
    json!({"type": "content", "content": {"type": "text", "text": text}})
}

/// A unified diff as one `diff` item per hunk: the lines of the old side, the lines of the new
/// side (Hermes `_parse_unified_diff_content`). The editor shows each hunk as its own change.
pub fn diff_items(path: &str, diff: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut old: Vec<&str> = Vec::new();
    let mut new: Vec<&str> = Vec::new();
    let mut in_hunk = false;
    let flush = |old: &mut Vec<&str>, new: &mut Vec<&str>, items: &mut Vec<Value>| {
        if !old.is_empty() || !new.is_empty() {
            items.push(json!({
                "type": "diff",
                "path": path,
                "oldText": (!old.is_empty()).then(|| old.join("\n")),
                "newText": new.join("\n"),
            }));
        }
        old.clear();
        new.clear();
    };
    for line in diff.lines() {
        if line.starts_with("@@") {
            flush(&mut old, &mut new, &mut items);
            in_hunk = true;
        } else if !in_hunk || line.starts_with("\\ No newline") {
            continue;
        } else if let Some(removed) = line.strip_prefix('-') {
            old.push(removed);
        } else if let Some(added) = line.strip_prefix('+') {
            new.push(added);
        } else {
            let context = line.strip_prefix(' ').unwrap_or(line);
            old.push(context);
            new.push(context);
        }
    }
    flush(&mut old, &mut new, &mut items);
    items
}

/// What a finished call shows. `before` is the file's text when a `write_file` started.
pub fn complete_content(
    name: &str,
    args: &Value,
    result: &Value,
    raw: &str,
    cwd: &str,
    before: Option<&str>,
) -> Vec<Value> {
    if failed(result) {
        return vec![text_content(&result_text(result, raw))];
    }
    match name {
        "patch" => {
            let diff = result
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let path = absolute(text_arg(args, &["path"]), cwd);
            let items = diff_items(&path, diff);
            if items.is_empty() {
                vec![text_content(&result_text(result, raw))]
            } else {
                items
            }
        }
        "write_file" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let path = absolute(text_arg(args, &["path"]), cwd);
            // A "before" equal to the new text is a write that landed before it was read: show
            // the file as written rather than a diff of nothing.
            let old = before.filter(|b| *b != content);
            vec![json!({"type": "diff", "path": path, "oldText": old, "newText": content})]
        }
        "todo_list" => {
            let todos = result
                .get("todos")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            vec![text_content(&format!("{todos} task(s) on the list"))]
        }
        "terminal" => {
            let text = result_text(result, raw);
            if text.trim().is_empty() {
                vec![text_content("(no output)")]
            } else {
                vec![text_content(&format!("```\n{text}\n```"))]
            }
        }
        _ => {
            let text = result_text(result, raw);
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![text_content(&text)]
            }
        }
    }
}

/// The todo list as an ACP plan: the whole list each time (Hermes `_build_plan_update_from_todo_result`).
/// A cancelled task is kept as a finished entry marked as cancelled, so a client that replaces its
/// plan with ours doesn't lose it.
pub fn plan_from_todo(result: &Value) -> Option<Value> {
    let todos = result.get("todos")?.as_array()?;
    let entries: Vec<Value> = todos
        .iter()
        .filter_map(|todo| {
            let content = todo
                .get("content")
                .and_then(Value::as_str)
                .or_else(|| todo.get("id").and_then(Value::as_str))?
                .trim();
            if content.is_empty() {
                return None;
            }
            let status = todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let (content, status) = match status {
                "in_progress" => (content.to_string(), "in_progress"),
                "completed" => (content.to_string(), "completed"),
                "cancelled" => (format!("[cancelled] {content}"), "completed"),
                _ => (content.to_string(), "pending"),
            };
            Some(json!({"content": content, "priority": "medium", "status": status}))
        })
        .collect();
    Some(json!({"sessionUpdate": "plan", "entries": entries}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_the_kind_its_icon_expects() {
        for (name, expected) in [
            ("read_file", "read"),
            ("skill_view", "read"),
            ("write_file", "edit"),
            ("patch", "edit"),
            ("search_files", "search"),
            ("terminal", "execute"),
            ("process_manage", "execute"),
            ("delegate_task", "execute"),
            ("web_search", "fetch"),
            ("web_extract", "fetch"),
            ("todo_list", "other"),
            ("memory", "other"),
            ("mcp_github_create_issue", "other"),
        ] {
            assert_eq!(kind(name), expected, "{name}");
        }
    }

    #[test]
    fn titles_say_what_the_call_does() {
        assert_eq!(
            title("terminal", &json!({"command": "cargo test -p foo\nls"})),
            "cargo test -p foo"
        );
        assert_eq!(
            title("read_file", &json!({"path": "src/a.rs"})),
            "Read src/a.rs"
        );
        assert_eq!(
            title(
                "read_file",
                &json!({"path": "a.rs", "offset": 10, "limit": 5})
            ),
            "Read a.rs (lines 10\u{2013}14)"
        );
        assert_eq!(
            title("write_file", &json!({"path": "a.txt"})),
            "Write a.txt"
        );
        assert_eq!(title("patch", &json!({"path": "a.txt"})), "Edit a.txt");
        assert_eq!(
            title(
                "search_files",
                &json!({"pattern": "fn main", "path": "src"})
            ),
            "Search \"fn main\" in src"
        );
        assert_eq!(
            title("web_search", &json!({"query": "rust acp"})),
            "Web search \"rust acp\""
        );
        assert_eq!(
            title("web_extract", &json!({"urls": ["https://a.example/x"]})),
            "Fetch https://a.example/x"
        );
        assert_eq!(
            title("delegate_task", &json!({"goal": "review it"})),
            "Delegate: review it"
        );
        assert_eq!(title("memory", &json!({})), "memory");
        assert_eq!(title("skill_manage", &json!({})), "skill manage");
    }

    #[test]
    fn a_long_command_is_cut_on_a_character_boundary() {
        let command = "\u{9519}".repeat(200);
        let shown = title("terminal", &json!({"command": command}));
        assert_eq!(shown.chars().count(), 80);
        assert!(shown.ends_with('\u{2026}'));
    }

    #[test]
    fn locations_are_absolute_with_backslashes() {
        let cwd = "C:/Users/a/proj";
        assert_eq!(
            locations(&json!({"path": "src/a.rs"}), cwd),
            vec![json!({"path": "C:\\Users\\a\\proj\\src\\a.rs"})]
        );
        assert_eq!(
            locations(&json!({"path": "C:/x/b.rs", "offset": 7}), cwd),
            vec![json!({"path": "C:\\x\\b.rs", "line": 7})]
        );
        assert!(locations(&json!({"command": "ls"}), cwd).is_empty());
    }

    #[test]
    fn failures_are_what_our_tools_report() {
        assert!(failed(&json!({"success": false, "error": "nope"})));
        assert!(failed(&json!({"error": "nope"})));
        assert!(!failed(&json!({"success": true})));
        assert!(!failed(&json!({"output": "fine", "exit_code": 1})));
        assert!(!failed(&json!("plain text")));
    }

    #[test]
    fn a_patch_result_becomes_one_diff_per_hunk() {
        let diff = "--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -10,2 +10,3 @@\n ten\n+eleven\n twelve\n";
        let items = diff_items("C:\\p\\a.rs", diff);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["oldText"], "one\ntwo\nthree");
        assert_eq!(items[0]["newText"], "one\nTWO\nthree");
        assert_eq!(items[1]["newText"], "ten\neleven\ntwelve");
        assert_eq!(items[0]["path"], "C:\\p\\a.rs");
    }

    #[test]
    fn a_new_file_is_a_diff_without_old_text() {
        let content = complete_content(
            "write_file",
            &json!({"path": "hello.txt", "content": "hi"}),
            &json!({"success": true}),
            "",
            "C:/w",
            None,
        );
        assert_eq!(content[0]["type"], "diff");
        assert_eq!(content[0]["oldText"], Value::Null);
        assert_eq!(content[0]["newText"], "hi");
        assert_eq!(content[0]["path"], "C:\\w\\hello.txt");
    }

    #[test]
    fn an_overwrite_shows_the_text_it_replaced() {
        let content = complete_content(
            "write_file",
            &json!({"path": "a.txt", "content": "new"}),
            &json!({"success": true}),
            "",
            "C:/w",
            Some("old"),
        );
        assert_eq!(content[0]["oldText"], "old");
        // A "before" read after the write already landed is no diff.
        let content = complete_content(
            "write_file",
            &json!({"path": "a.txt", "content": "new"}),
            &json!({"success": true}),
            "",
            "C:/w",
            Some("new"),
        );
        assert_eq!(content[0]["oldText"], Value::Null);
    }

    #[test]
    fn a_command_result_is_its_output_in_a_fence() {
        let content = complete_content(
            "terminal",
            &json!({"command": "x"}),
            &json!({"output": "hello", "exit_code": 2}),
            "",
            "C:/w",
            None,
        );
        assert_eq!(
            content[0]["content"]["text"],
            "```\nhello\n(exit code 2)\n```"
        );
    }

    #[test]
    fn a_long_result_is_cut() {
        let big = "x".repeat(6000);
        let content = complete_content("memory", &json!({}), &json!(big), "", "C:/w", None);
        let text = content[0]["content"]["text"].as_str().unwrap();
        assert!(text.ends_with("(truncated)"));
        assert!(text.chars().count() < 5100);
    }

    #[test]
    fn a_failed_call_shows_its_error() {
        let content = complete_content(
            "patch",
            &json!({"path": "a"}),
            &json!({"success": false, "error": "old_string was not found"}),
            "",
            "C:/w",
            None,
        );
        assert_eq!(content[0]["content"]["text"], "old_string was not found");
    }

    #[test]
    fn the_todo_list_is_the_plan() {
        let plan = plan_from_todo(&json!({"todos": [
            {"id": "1", "content": "write it", "status": "completed"},
            {"id": "2", "content": "test it", "status": "in_progress"},
            {"id": "3", "content": "ship it", "status": "pending"},
            {"id": "4", "content": "skip it", "status": "cancelled"},
            {"id": "5", "content": "", "status": "pending"},
        ]}))
        .unwrap();
        assert_eq!(plan["sessionUpdate"], "plan");
        let entries = plan["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[1]["status"], "in_progress");
        assert_eq!(entries[3]["content"], "[cancelled] skip it");
        assert_eq!(entries[3]["status"], "completed");
        assert!(plan_from_todo(&json!({"nothing": true})).is_none());
    }

    #[test]
    fn only_write_file_needs_a_snapshot() {
        assert_eq!(
            snapshot_path("write_file", &json!({"path": "a.txt"}), "C:/w"),
            Some("C:\\w\\a.txt".to_string())
        );
        assert_eq!(
            snapshot_path("patch", &json!({"path": "a.txt"}), "C:/w"),
            None
        );
    }
}
