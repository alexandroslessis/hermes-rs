//! The `memory` tool and the pending-write store behind `memory.write_approval`, ported from
//! Hermes's `tools/memory_tool.py` and `tools/write_approval.py` (MIT).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::{MemoryStore, Operation, Target, TurnFailures};

const REPLACE_REMOVED: &str = "'replace' is not available; use 'patch'. old_text is the exact span to change and content is its new text; the rest of the entry is kept. To rewrite a whole entry, set old_text to the whole entry. Nothing was written.";

/// A write staged for the user's approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingWrite {
    pub id: String,
    pub target: Target,
    pub summary: String,
    pub operations: Vec<Operation>,
    pub created_at: i64,
}

fn error(message: impl Into<String>) -> Value {
    json!({"success": false, "error": message.into()})
}

/// One line per op, as the approver sees it.
fn op_line(op: &Operation) -> String {
    let content = op.content.as_deref().unwrap_or_default();
    let old = op.old_text.as_deref().unwrap_or_default();
    match op.action.as_str() {
        "remove" => format!("- remove: {old}"),
        "patch" => format!("- patch span '{old}' -> '{content}' (rest of the entry is kept)"),
        action => format!("- {action}: {content}"),
    }
}

impl MemoryStore {
    fn pending_dir(&self) -> PathBuf {
        self.dir()
            .parent()
            .unwrap_or(self.dir())
            .join("pending")
            .join("memory")
    }

    /// Staged writes, oldest first.
    pub fn pending(&self) -> Vec<PendingWrite> {
        let Ok(dir) = std::fs::read_dir(self.pending_dir()) else {
            return Vec::new();
        };
        let mut records: Vec<PendingWrite> = dir
            .filter_map(|entry| std::fs::read(entry.ok()?.path()).ok())
            .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
            .collect();
        records.sort_by_key(|record| record.created_at);
        records
    }

    fn stage(&self, target: Target, operations: Vec<Operation>) -> Value {
        // Pin each patch/remove to the full entry it selects now, so approval applies to exactly
        // what the approver reviewed.
        let resolved = self.resolve(target, &operations);
        if resolved["success"] != json!(true) {
            return resolved;
        }
        let operations: Vec<Operation> = operations
            .into_iter()
            .zip(
                resolved["matched_entries"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            )
            .map(|(op, matched)| Operation {
                matched_entry: matched.as_str().map(str::to_string),
                ..op
            })
            .collect();
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let lines: Vec<String> = operations.iter().map(op_line).collect();
        let record = PendingWrite {
            id: id.clone(),
            target,
            summary: format!(
                "apply {} op(s) to {}:\n{}",
                operations.len(),
                target.label(),
                lines.join("\n")
            ),
            operations,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        let dir = self.pending_dir();
        let saved = std::fs::create_dir_all(&dir).and_then(|()| {
            std::fs::write(
                dir.join(format!("{id}.json")),
                serde_json::to_vec_pretty(&record).unwrap_or_default(),
            )
        });
        if let Err(error) = saved {
            tracing::error!("staging a memory write failed: {error}");
        }
        json!({
            "success": true,
            "staged": true,
            "pending_id": id,
            "message": "Staged for approval (memory.write_approval is on). Not yet saved — the user reviews it in the Memory screen.",
        })
    }

    /// Applies a staged write (the user approved it) and drops the record when it succeeds.
    pub fn approve(&self, id: &str) -> Value {
        let Some(record) = self.pending().into_iter().find(|r| r.id == id) else {
            return error(format!("No pending memory write {id}."));
        };
        let result = self.apply(
            record.target,
            &record.operations,
            &mut TurnFailures::default(),
        );
        if result["success"] == json!(true) {
            self.discard(id);
        }
        result
    }

    pub fn discard(&self, id: &str) -> bool {
        std::fs::remove_file(self.pending_dir().join(format!("{id}.json"))).is_ok()
    }

    /// The `memory` tool. Accepts Hermes's batch shape (`target` + `operations`) and the legacy
    /// single-op shape (`action`, `content`/`new_text`, `old_text`).
    pub fn tool(&self, args: &Value, failures: &mut TurnFailures) -> Value {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let Some(target_name) = args.get("target").filter(|t| !t.is_null()) else {
            return error(
                "Missing required target. Retry with target='memory' or target='user' at the top level. For a batch, remove target from every operation; use separate calls for different stores. Nothing was written or staged.",
            );
        };
        let Some(target) = target_name.as_str().and_then(Target::parse) else {
            return error(format!(
                "Invalid memory target '{}'. Use 'memory' or 'user'.",
                target_name.as_str().unwrap_or(&target_name.to_string())
            ));
        };
        let operations = args.get("operations").filter(|ops| !ops.is_null());
        if let Some(Value::Array(ops)) = operations {
            for (index, op) in ops.iter().enumerate() {
                if let Some(nested) = op.get("target").and_then(Value::as_str) {
                    if nested != target.name() {
                        return error(format!(
                            "Operation {} names a different target than the one at the top level. One call writes one store: retry with separate calls for different stores. Nothing was written or staged.",
                            index + 1
                        ));
                    }
                }
            }
        }
        if !self.enabled(target) {
            return json!({"success": false, "error": format!("Built-in {} writes are disabled in memory config.", target.file_name()), "target": target.name()});
        }

        let ops: Vec<Operation> = match operations {
            Some(Value::Array(items)) if !items.is_empty() => {
                let mut ops = Vec::new();
                for item in items {
                    let Ok(mut op) = serde_json::from_value::<Operation>(item.clone()) else {
                        return error(
                            "operations must be a list of {action, content?, old_text?} objects.",
                        );
                    };
                    if op.content.as_deref().is_none_or(str::is_empty) {
                        op.content = item
                            .get("new_text")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    op.matched_entry = None;
                    ops.push(op);
                }
                if ops.iter().any(|op| op.action == "replace") {
                    return error(REPLACE_REMOVED);
                }
                ops
            }
            Some(other) if !other.as_array().is_some_and(Vec::is_empty) => {
                return error(
                    "operations must be a list of {action, content?, old_text?} objects.",
                );
            }
            _ => {
                let Some(action) = text("action") else {
                    return error(
                        "Missing required parameter: 'operations', a list of {action: add/patch/remove, content?, old_text?} changes; a single change is a one-item list.",
                    );
                };
                if action == "replace" {
                    return error(REPLACE_REMOVED);
                }
                if !matches!(action.as_str(), "add" | "patch" | "remove") {
                    return error(format!(
                        "Unknown action '{action}'. Use: add, patch, remove"
                    ));
                }
                let op = Operation {
                    action,
                    content: text("content").or_else(|| text("new_text")),
                    old_text: text("old_text"),
                    matched_entry: None,
                };
                if let Some(invalid) = self.validate_single(target, &op) {
                    return invalid;
                }
                vec![op]
            }
        };
        if self.config().write_approval {
            return self.stage(target, ops);
        }
        self.apply(target, &ops, failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryConfig;

    fn store(write_approval: bool) -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            write_approval,
            ..Default::default()
        };
        let store = MemoryStore::new(dir.path().join("memories"), config);
        (dir, store)
    }

    #[test]
    fn the_tool_accepts_batches_and_the_legacy_shape() {
        let (_dir, store) = store(false);
        let mut f = TurnFailures::default();
        let batch = store.tool(&json!({"target": "memory", "operations": [{"action": "add", "content": "Repo uses cargo"}]}), &mut f);
        assert_eq!(batch["success"], true, "{batch}");
        let legacy = store.tool(&json!({"target": "memory", "action": "patch", "old_text": "cargo", "new_text": "cargo + uv"}), &mut f);
        assert_eq!(legacy["success"], true, "{legacy}");
        assert_eq!(store.entries(Target::Memory), vec!["Repo uses cargo + uv"]);
    }

    #[test]
    fn the_tool_explains_bad_calls() {
        let (_dir, store) = store(false);
        let mut f = TurnFailures::default();
        let call = |args: Value, f: &mut TurnFailures| {
            store.tool(&args, f)["error"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert!(call(json!({"operations": []}), &mut f).starts_with("Missing required target"));
        assert!(
            call(json!({"target": "notes", "operations": []}), &mut f)
                .starts_with("Invalid memory target 'notes'")
        );
        assert!(
            call(json!({"target": "memory"}), &mut f)
                .starts_with("Missing required parameter: 'operations'")
        );
        assert!(call(json!({"target": "memory", "operations": [{"action": "replace", "old_text": "a", "content": "b"}]}), &mut f).starts_with("'replace' is not available"));
        assert!(call(json!({"target": "user", "operations": [{"action": "add", "content": "x", "target": "memory"}]}), &mut f).starts_with("Operation 1 names a different target"));
        let missing = store.tool(&json!({"target": "memory", "action": "remove"}), &mut f);
        assert!(
            missing["error"]
                .as_str()
                .unwrap()
                .starts_with("'remove' needs old_text")
        );
        assert!(missing["current_entries"].is_array());
    }

    #[test]
    fn approval_stages_pins_and_applies_on_approve() {
        let (_dir, store) = store(false);
        let mut f = TurnFailures::default();
        store.tool(&json!({"target": "user", "operations": [{"action": "add", "content": "Name is Alex"}]}), &mut f);
        store.set_config(MemoryConfig {
            write_approval: true,
            ..Default::default()
        });
        let staged = store.tool(&json!({"target": "user", "operations": [{"action": "patch", "old_text": "Alex", "content": "Alexandros"}]}), &mut f);
        assert_eq!(staged["staged"], true, "{staged}");
        assert_eq!(store.entries(Target::User), vec!["Name is Alex"]);
        let pending = store.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].operations[0].matched_entry.as_deref(),
            Some("Name is Alex")
        );
        assert!(
            pending[0]
                .summary
                .contains("- patch span 'Alex' -> 'Alexandros'")
        );
        let applied = store.approve(&pending[0].id);
        assert_eq!(applied["success"], true, "{applied}");
        assert_eq!(store.entries(Target::User), vec!["Name is Alexandros"]);
        assert!(store.pending().is_empty());
    }

    #[test]
    fn a_stale_approval_is_refused_and_kept() {
        let (_dir, store) = store(false);
        let mut f = TurnFailures::default();
        store.tool(
            &json!({"target": "memory", "operations": [{"action": "add", "content": "Old fact"}]}),
            &mut f,
        );
        store.set_config(MemoryConfig {
            write_approval: true,
            ..Default::default()
        });
        store.tool(&json!({"target": "memory", "operations": [{"action": "remove", "old_text": "Old fact"}]}), &mut f);
        // The entry changes before the user approves.
        std::fs::write(store.path(Target::Memory), "Old fact, revised").unwrap();
        let id = store.pending()[0].id.clone();
        let refused = store.approve(&id);
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("Entry changed since it was staged")
        );
        assert_eq!(store.pending().len(), 1);
    }
}
