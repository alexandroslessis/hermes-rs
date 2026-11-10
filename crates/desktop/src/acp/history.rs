//! A stored conversation as `session/update`s, for `session/load`: the editor shows the thread as
//! it was. The rows are the page's transcript rows (`transcript::messages`): user and assistant
//! text, reasoning, and each tool call with its result.
//!
//! Hermes: `acp_adapter/server.py` (`_history_replay_updates`, `_history_message_chunk`).

use serde_json::{Value, json};

use super::tools;

fn chunk(kind: &str, text: &str) -> Value {
    json!({
        "sessionUpdate": kind,
        "content": {"type": "text", "text": text},
        "messageId": uuid::Uuid::new_v4().to_string(),
    })
}

/// A finished tool call: its card, then its result.
fn tool_updates(row: &Value, cwd: &str) -> Vec<Value> {
    let name = row["name"].as_str().unwrap_or_default();
    let args = row.get("args").cloned().unwrap_or(Value::Null);
    let id = row["tool_call_id"].as_str().unwrap_or_default();
    let raw = row["text"].as_str().unwrap_or_default();
    let result = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.into()));
    let failed = tools::failed(&result);
    let mut start = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": id,
        "title": tools::title(name, &args),
        "kind": tools::kind(name),
        "status": "in_progress",
        "rawInput": args,
    });
    let locations = tools::locations(&args, cwd);
    if !locations.is_empty() {
        start["locations"] = json!(locations);
    }
    // Without the file's old text, a replayed write is shown as text, not as a diff of a new file.
    let content = if name == "write_file" && !failed {
        vec![json!({"type": "content", "content": {"type": "text", "text": "File written."}})]
    } else {
        tools::complete_content(name, &args, &result, raw, cwd, None)
    };
    let mut done = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": id,
        "status": if row["text"].is_null() || failed { "failed" } else { "completed" },
    });
    if !content.is_empty() {
        done["content"] = json!(content);
    }
    let mut updates = vec![start, done];
    if name == "todo_list" && !failed {
        updates.extend(tools::plan_from_todo(&result));
    }
    updates
}

/// The updates that redraw `rows`, in order.
pub fn replay(rows: &[Value], cwd: &str) -> Vec<Value> {
    let mut updates = Vec::new();
    for row in rows {
        let text = row["text"].as_str().unwrap_or_default();
        match row["role"].as_str().unwrap_or_default() {
            "user" if !text.trim().is_empty() => {
                updates.push(chunk("user_message_chunk", text));
            }
            "assistant" => {
                if let Some(thought) = row["reasoning"].as_str().filter(|t| !t.trim().is_empty()) {
                    updates.push(chunk("agent_thought_chunk", thought));
                }
                if !text.trim().is_empty() {
                    updates.push(chunk("agent_message_chunk", text));
                }
            }
            "tool" => updates.extend(tool_updates(row, cwd)),
            _ => {}
        }
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(updates: &[Value]) -> Vec<&str> {
        updates
            .iter()
            .map(|u| u["sessionUpdate"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn a_conversation_replays_in_order() {
        let rows = vec![
            json!({"role": "user", "text": "list files"}),
            json!({"role": "assistant", "text": "", "reasoning": "I should run ls"}),
            json!({"role": "tool", "name": "terminal", "args": {"command": "ls"},
                   "tool_call_id": "t1", "text": "{\"output\":\"a.txt\",\"exit_code\":0}"}),
            json!({"role": "assistant", "text": "There is a.txt.", "reasoning": null}),
        ];
        let updates = replay(&rows, "C:/w");
        assert_eq!(
            kinds(&updates),
            [
                "user_message_chunk",
                "agent_thought_chunk",
                "tool_call",
                "tool_call_update",
                "agent_message_chunk"
            ]
        );
        assert_eq!(updates[2]["title"], "ls");
        assert_eq!(updates[3]["status"], "completed");
        assert_eq!(
            updates[3]["content"][0]["content"]["text"],
            "```\na.txt\n```"
        );
    }

    #[test]
    fn empty_rows_send_nothing() {
        let rows = vec![
            json!({"role": "user", "text": "  "}),
            json!({"role": "assistant", "text": "", "reasoning": null}),
        ];
        assert!(replay(&rows, "C:/w").is_empty());
    }

    #[test]
    fn a_call_with_no_result_is_failed() {
        let rows = vec![
            json!({"role": "tool", "name": "terminal", "args": {"command": "x"},
                               "tool_call_id": "t1", "text": null}),
        ];
        assert_eq!(replay(&rows, "C:/w")[1]["status"], "failed");
    }

    #[test]
    fn a_replayed_write_is_not_a_diff_of_a_new_file() {
        let rows = vec![json!({"role": "tool", "name": "write_file",
                               "args": {"path": "a.txt", "content": "x"},
                               "tool_call_id": "t1", "text": "{\"success\":true}"})];
        let updates = replay(&rows, "C:/w");
        assert_eq!(updates[1]["content"][0]["content"]["text"], "File written.");
    }

    #[test]
    fn a_replayed_todo_list_restores_the_plan() {
        let rows = vec![json!({"role": "tool", "name": "todo_list", "args": {},
                               "tool_call_id": "t1",
                               "text": "{\"todos\":[{\"id\":\"1\",\"content\":\"a\",\"status\":\"pending\"}]}"})];
        let updates = replay(&rows, "C:/w");
        assert_eq!(updates.last().unwrap()["sessionUpdate"], "plan");
    }
}
