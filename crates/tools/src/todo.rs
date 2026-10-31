//! todo_list: the agent's task list, ported from Hermes's `tools/todo_tool.py`.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{SessionTools, ToolOutput, parse_args};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

pub struct TodoList {
    items: Mutex<Vec<TodoItem>>,
    revision: Mutex<u64>,
}

impl TodoList {
    pub fn new(items: Vec<TodoItem>) -> Self {
        Self {
            items: Mutex::new(items),
            revision: Mutex::new(0),
        }
    }

    pub fn items(&self) -> Vec<TodoItem> {
        self.items.lock().clone()
    }

    fn write(&self, todos: Vec<TodoItem>, merge: bool) -> Result<Vec<TodoItem>, String> {
        for item in &todos {
            if item.id.trim().is_empty() {
                return Err("Every todo item needs a non-empty id.".into());
            }
            if item.content.trim().is_empty() && !merge {
                return Err(format!("Todo '{}' has empty content.", item.id));
            }
        }
        // A repeated id within one call: the last occurrence wins, in its original position.
        let mut deduped: Vec<TodoItem> = Vec::new();
        for item in todos {
            deduped.retain(|existing| existing.id != item.id);
            deduped.push(item);
        }

        let mut items = self.items.lock();
        let updated = if merge {
            let mut merged = items.clone();
            for item in deduped {
                match merged.iter_mut().find(|existing| existing.id == item.id) {
                    Some(existing) => {
                        if !item.content.trim().is_empty() {
                            existing.content = item.content;
                        }
                        existing.status = item.status;
                        if item.parent.is_some() {
                            existing.parent = item.parent;
                        }
                    }
                    None => merged.push(item),
                }
            }
            merged
        } else {
            deduped
        };
        let updated = lift_in_progress(updated);
        *items = updated.clone();
        *self.revision.lock() += 1;
        Ok(updated)
    }
}

/// Moves the in-progress step ahead of earlier pending placeholders (flat lists only; nested
/// lists keep authored order so subtasks stay with their siblings).
fn lift_in_progress(mut items: Vec<TodoItem>) -> Vec<TodoItem> {
    if items.iter().any(|item| item.parent.is_some()) {
        return items;
    }
    let Some(active) = items
        .iter()
        .position(|item| item.status == TodoStatus::InProgress)
    else {
        return items;
    };
    if let Some(first_pending) = items[..active]
        .iter()
        .position(|item| item.status == TodoStatus::Pending)
    {
        let item = items.remove(active);
        items.insert(first_pending, item);
    }
    items
}

#[derive(Deserialize)]
struct TodoArgs {
    #[serde(default)]
    todos: Option<Vec<TodoItem>>,
    #[serde(default)]
    merge: bool,
}

pub fn todo_list(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: TodoArgs = match parse_args("todo_list", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let items = match args.todos {
        None => tools.todos.items(),
        Some(todos) => match tools.todos.write(todos, args.merge) {
            Ok(items) => items,
            Err(error) => return ToolOutput::error(error),
        },
    };
    let count = |status| items.iter().filter(|item| item.status == status).count();
    ToolOutput::ok(json!({
        "todos": items,
        "revision": *tools.todos.revision.lock(),
        "summary": {
            "total": items.len(),
            "pending": count(TodoStatus::Pending),
            "in_progress": count(TodoStatus::InProgress),
            "completed": count(TodoStatus::Completed),
            "cancelled": count(TodoStatus::Cancelled),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::tools_in;

    #[test]
    fn replace_merge_and_read() {
        let (_dir, tools) = tools_in();
        let out = todo_list(
            &tools,
            json!({"todos": [
                {"id": "1", "content": "plan", "status": "completed"},
                {"id": "2", "content": "build", "status": "pending"},
                {"id": "3", "content": "test", "status": "in_progress"},
            ]}),
        );
        let ids: Vec<_> = out.value["todos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].clone())
            .collect();
        assert_eq!(
            ids,
            vec!["1", "3", "2"],
            "in_progress lifts ahead of pending"
        );

        let out = todo_list(
            &tools,
            json!({"merge": true, "todos": [{"id": "2", "content": "", "status": "completed"}]}),
        );
        assert_eq!(out.value["summary"]["completed"], 2);
        assert_eq!(
            out.value["todos"][2]["content"], "build",
            "merge keeps content when omitted"
        );

        let read = todo_list(&tools, json!({}));
        assert_eq!(read.value["summary"]["total"], 3);
    }

    #[test]
    fn rejects_items_without_ids() {
        let (_dir, tools) = tools_in();
        let out = todo_list(
            &tools,
            json!({"todos": [{"id": " ", "content": "x", "status": "pending"}]}),
        );
        assert!(out.is_error);
    }
}
