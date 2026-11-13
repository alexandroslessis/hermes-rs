//! The `memory` tool and the pending-write store behind `memory.write_approval`, ported from
//! Hermes's `tools/memory_tool.py` and `tools/write_approval.py` (MIT).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::{MemoryStore, Operation, Target, TurnFailures};
use crate::tiers::{Extras, Tier};

const REPLACE_REMOVED: &str = "'replace' is not available; use 'patch'. old_text is the exact span to change and content is its new text; the rest of the entry is kept. To rewrite a whole entry, set old_text to the whole entry. Nothing was written.";

/// A write staged for the user's approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingWrite {
    pub id: String,
    pub target: Target,
    pub summary: String,
    pub operations: Vec<Operation>,
    /// The tier, tags, links and notes of `operations[i]` (empty: all `core`, as before CP9.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<Extras>,
    pub created_at: i64,
}

/// A `memory` call read into what it does.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub target: Target,
    pub ops: Vec<Operation>,
    /// Parallel to `ops` (all default for an old-shape call).
    pub extras: Vec<Extras>,
    /// `Some` for a `note_read` call: a read, never staged, never counted as a write.
    pub read: Option<ReadCall>,
}

/// A `note_read` call.
#[derive(Clone, Debug)]
pub struct ReadCall {
    pub tier: Tier,
    /// The session's project scope key (from the caller, never from the model).
    pub scope: Option<String>,
    pub old_text: String,
}

fn error(message: impl Into<String>) -> Value {
    json!({"success": false, "error": message.into()})
}

/// One line per op, as the approver sees it (a non-core tier is named).
fn op_line(op: &Operation, extra: &Extras) -> String {
    let content = op.content.as_deref().unwrap_or_default();
    let old = op.old_text.as_deref().unwrap_or_default();
    let tier = match extra.tier.as_deref() {
        Some(tier) if tier != "core" => format!(" [{tier}]"),
        _ => String::new(),
    };
    match op.action.as_str() {
        "remove" => format!("- remove{tier}: {old}"),
        "patch" => {
            format!("- patch{tier} span '{old}' -> '{content}' (rest of the entry is kept)")
        }
        action => format!("- {action}{tier}: {content}"),
    }
}

/// Reads the tier, tags, links and note of one operation (a batch item, or the top level of a
/// legacy single-op call). `n` names the operation in a refusal; `default_tier` is a call-level
/// `tier`. The project scope is the session's, never the model's.
fn take_extras(
    item: &Value,
    n: usize,
    default_tier: Option<&str>,
    session_scope: Option<&str>,
) -> Result<Extras, Value> {
    let bad = |what: &str| error(format!("Operation {n}: {what} Nothing was written or staged."));
    let tier = match item.get("tier") {
        None | Some(Value::Null) => default_tier.map(str::to_string),
        Some(Value::String(tier)) if tier.trim().is_empty() => default_tier.map(str::to_string),
        Some(Value::String(tier)) => Some(tier.trim().to_ascii_lowercase()),
        Some(_) => return Err(bad("tier must be 'core', 'project' or 'archive'.")),
    };
    let list = |key: &str| -> Result<Option<Vec<String>>, Value> {
        match item.get(key) {
            None | Some(Value::Null) => Ok(None),
            // Smaller models often send one label as a string, or a comma-separated string:
            // read it as that list rather than failing a write the old schema would have taken
            // (an unknown field was ignored before CP9.2). An empty string is no list.
            Some(Value::String(text)) => {
                let items: Vec<String> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                Ok((!items.is_empty()).then_some(items))
            }
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(Some)
                .ok_or_else(|| bad(&format!("{key} must be a list of strings."))),
            Some(_) => Err(bad(&format!("{key} must be a list of strings."))),
        }
    };
    let note = match item.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(bad("note must be a string (the long-form body).")),
    };
    let scope = (tier.as_deref() == Some("project"))
        .then(|| session_scope.map(str::to_string))
        .flatten();
    Ok(Extras {
        tier,
        scope,
        tags: list("tags")?,
        links: list("links")?,
        note,
    })
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

    fn stage(&self, target: Target, operations: Vec<Operation>, extras: Vec<Extras>) -> Value {
        // Pin each patch/remove to the full entry it selects now, so approval applies to exactly
        // what the approver reviewed.
        let resolved = self.resolve_ext(target, &operations, &extras);
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
        let empty = Extras::default();
        let lines: Vec<String> = operations
            .iter()
            .enumerate()
            .map(|(i, op)| op_line(op, extras.get(i).unwrap_or(&empty)))
            .collect();
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
            extras: if extras.iter().all(Extras::is_empty) {
                Vec::new()
            } else {
                extras
            },
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
        let result = self.apply_ext(
            record.target,
            &record.operations,
            &record.extras,
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
        self.tool_as(args, failures, false, None)
    }

    /// [`Self::tool`] for a session in the repository whose scope key is `project` (it may use
    /// tier `project`, for that repository only).
    pub fn tool_in(
        &self,
        args: &Value,
        failures: &mut TurnFailures,
        project: Option<&str>,
    ) -> Value {
        self.tool_as(args, failures, false, project)
    }

    /// The `memory` tool for an unattended background review (Hermes's `_background_delete_gate`):
    /// adding stays available, but patches and removals are staged for the user's approval
    /// instead of applied, since nobody is watching.
    pub fn unattended_tool(&self, args: &Value, failures: &mut TurnFailures) -> Value {
        self.tool_as(args, failures, true, None)
    }

    /// [`Self::unattended_tool`] for a session in the repository `project` (see [`Self::tool_in`]).
    pub fn unattended_tool_in(
        &self,
        args: &Value,
        failures: &mut TurnFailures,
        project: Option<&str>,
    ) -> Value {
        self.tool_as(args, failures, true, project)
    }

    fn tool_as(
        &self,
        args: &Value,
        failures: &mut TurnFailures,
        unattended: bool,
        project: Option<&str>,
    ) -> Value {
        let parsed = match self.parse_call_in(args, project) {
            Ok(parsed) => parsed,
            Err(refusal) => return refusal,
        };
        if let Some(read) = &parsed.read {
            return self.run_read(parsed.target, read);
        }
        let Parsed {
            target,
            ops,
            extras,
            ..
        } = parsed;
        if unattended && ops.iter().any(|op| op.action != "add") {
            let mut staged = self.stage(target, ops, extras);
            if staged["success"] == json!(true) {
                staged["proposal_staged"] = json!(true);
                staged["message"] = json!(
                    "Background review may not change or delete memory entries unattended. The proposed change was staged for the user's approval in the Memory screen."
                );
            }
            return staged;
        }
        if self.config().write_approval {
            return self.stage(target, ops, extras);
        }
        self.apply_ext(target, &ops, &extras, failures)
    }

    /// A `note_read` call: the body of the note an entry links to (a read: nothing is staged).
    pub fn run_read(&self, target: Target, read: &ReadCall) -> Value {
        self.read_note(target, read.tier, read.scope.as_deref(), &read.old_text)
    }

    /// Reads a `memory` call into its store and operations, with every check the tool makes
    /// before it writes (shape, target, the store being enabled). `Err` is the tool's own refusal.
    /// The taint-driven staging area (`hermes_tools::staging`) reads a call through this too, so a
    /// staged write is the same write the tool would have made.
    ///
    /// `project` is the session's project scope key (the repository it works in), or `None`
    /// outside one; it is what tier `project` writes to, and only the caller supplies it.
    pub fn parse_call_in(&self, args: &Value, project: Option<&str>) -> Result<Parsed, Value> {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let Some(target_name) = args.get("target").filter(|t| !t.is_null()) else {
            return Err(error(
                "Missing required target. Retry with target='memory' or target='user' at the top level. For a batch, remove target from every operation; use separate calls for different stores. Nothing was written or staged.",
            ));
        };
        let Some(target) = target_name.as_str().and_then(Target::parse) else {
            return Err(error(format!(
                "Invalid memory target '{}'. Use 'memory' or 'user'.",
                target_name.as_str().unwrap_or(&target_name.to_string())
            )));
        };
        let operations = args.get("operations").filter(|ops| !ops.is_null());
        if let Some(Value::Array(ops)) = operations {
            for (index, op) in ops.iter().enumerate() {
                if let Some(nested) = op.get("target").and_then(Value::as_str) {
                    if nested != target.name() {
                        return Err(error(format!(
                            "Operation {} names a different target than the one at the top level. One call writes one store: retry with separate calls for different stores. Nothing was written or staged.",
                            index + 1
                        )));
                    }
                }
            }
        }
        if !self.enabled(target) {
            return Err(
                json!({"success": false, "error": format!("Built-in {} writes are disabled in memory config.", target.file_name()), "target": target.name()}),
            );
        }

        // A call-level `tier` is the default of every operation that names none.
        let default_tier = text("tier");
        let mut extras: Vec<Extras> = Vec::new();
        let ops: Vec<Operation> = match operations {
            Some(Value::Array(items)) if !items.is_empty() => {
                let mut ops = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    let Ok(mut op) = serde_json::from_value::<Operation>(item.clone()) else {
                        return Err(error(
                            "operations must be a list of {action, content?, old_text?} objects.",
                        ));
                    };
                    if op.content.as_deref().is_none_or(str::is_empty) {
                        op.content = item
                            .get("new_text")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    op.matched_entry = None;
                    extras.push(take_extras(item, index + 1, default_tier.as_deref(), project)?);
                    ops.push(op);
                }
                if ops.iter().any(|op| op.action == "replace") {
                    return Err(error(REPLACE_REMOVED));
                }
                ops
            }
            Some(other) if !other.as_array().is_some_and(Vec::is_empty) => {
                return Err(error(
                    "operations must be a list of {action, content?, old_text?} objects.",
                ));
            }
            _ => {
                let Some(action) = text("action") else {
                    return Err(error(
                        "Missing required parameter: 'operations', a list of {action: add/patch/remove, content?, old_text?} changes; a single change is a one-item list.",
                    ));
                };
                if action == "replace" {
                    return Err(error(REPLACE_REMOVED));
                }
                if !matches!(action.as_str(), "add" | "patch" | "remove" | "note_read") {
                    return Err(error(format!(
                        "Unknown action '{action}'. Use: add, patch, remove"
                    )));
                }
                let op = Operation {
                    action,
                    content: text("content").or_else(|| text("new_text")),
                    old_text: text("old_text"),
                    matched_entry: None,
                };
                if let Some(invalid) = self.validate_single(target, &op) {
                    return Err(invalid);
                }
                extras.push(take_extras(args, 1, default_tier.as_deref(), project)?);
                vec![op]
            }
        };
        if ops.iter().any(|op| op.action == "note_read") {
            if ops.len() != 1 {
                return Err(error(
                    "note_read must be the only operation in its call (it reads; it writes nothing). Nothing was written or staged.",
                ));
            }
            let tier = match extras[0].tier.as_deref() {
                None => Tier::Archive,
                Some(name) => Tier::parse(name).ok_or_else(|| {
                    error(format!(
                        "Invalid tier '{name}'. Use 'project' or 'archive' for note_read."
                    ))
                })?,
            };
            let Some(old_text) = ops[0].old_text.clone() else {
                return Err(error(
                    "note_read needs old_text -- a short unique substring of the entry whose note to read.",
                ));
            };
            return Ok(Parsed {
                target,
                ops: Vec::new(),
                extras: Vec::new(),
                read: Some(ReadCall {
                    tier,
                    scope: project.map(str::to_string),
                    old_text,
                }),
            });
        }
        Ok(Parsed {
            target,
            ops,
            extras,
            read: None,
        })
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
    fn an_unattended_review_adds_but_stages_changes_and_removals() {
        let (_dir, store) = store(false);
        let mut f = TurnFailures::default();
        let added = store.unattended_tool(
            &json!({"target": "memory", "operations": [{"action": "add", "content": "Repo uses cargo"}]}),
            &mut f,
        );
        assert_eq!(added["success"], true, "{added}");
        assert!(added.get("staged").is_none());
        let removal = store.unattended_tool(
            &json!({"target": "memory", "operations": [{"action": "remove", "old_text": "cargo"}]}),
            &mut f,
        );
        assert_eq!(removal["proposal_staged"], true, "{removal}");
        assert_eq!(store.entries(Target::Memory), vec!["Repo uses cargo"]);
        assert_eq!(store.pending().len(), 1);
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
