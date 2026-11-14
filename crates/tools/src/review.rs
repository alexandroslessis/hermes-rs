//! The tools a background review may use, after Hermes's `background_review.py`
//! (`_review_tool_whitelist`) and `skill_manager_guards.py` (MIT).
//!
//! The review advertises the session's full tool list (so its requests share the session's
//! prompt cache) but only dispatches the skill tools, the read-only file tools and, when memory
//! is under review, `memory`. On top of `skill_manage`'s own checks it requires the review to
//! have loaded a file before changing it, and never deletes a skill; an unattended review
//! stages memory changes and removals for the user's approval.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::operation::Emitter;
use hermes_agent::tool::ToolProvider;
use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorData, JsonObject, Tool};
use serde_json::{Value, json};

use crate::skill_manage::{SkillOp, WriteOrigin};
use crate::{SessionTools, ToolOutput};

pub struct ReviewTools {
    tools: Arc<SessionTools>,
    /// Memory is under review (Hermes gives a skills-only review no `memory` tool).
    allow_memory: bool,
    /// The user asked for this review (`/refine`): memory keeps its full set of operations.
    attended: bool,
    /// Files the review loaded, normalized: the read-before-write guard checks them.
    read: parking_lot::Mutex<HashSet<String>>,
    /// The curator's consolidation pass rather than a review: only the skill tools exist, and a
    /// delete archives a skill absorbed into an umbrella.
    curator: bool,
}

impl ReviewTools {
    pub fn new(tools: Arc<SessionTools>, allow_memory: bool, attended: bool) -> Self {
        Self {
            tools,
            allow_memory,
            attended,
            read: Default::default(),
            curator: false,
        }
    }

    /// The tools of the curator's consolidation pass (Hermes gives it only the `skills` toolset).
    pub fn curator(tools: Arc<SessionTools>) -> Self {
        Self {
            tools,
            allow_memory: false,
            attended: false,
            read: Default::default(),
            curator: true,
        }
    }

    fn origin(&self) -> WriteOrigin {
        if self.curator {
            WriteOrigin::Curator
        } else {
            WriteOrigin::Review
        }
    }

    fn allowed(&self, name: &str) -> bool {
        if self.curator {
            return matches!(name, "skills_list" | "skill_view" | "skill_manage");
        }
        matches!(
            name,
            "skills_list" | "skill_view" | "skill_manage" | "read_file" | "search_files"
        ) || (self.allow_memory && name == "memory")
    }

    /// Hermes's denial, naming what the review may use instead.
    fn denial(&self, name: &str) -> ToolOutput {
        if self.curator {
            return ToolOutput::error(format!(
                "Curator denied non-whitelisted tool: {name}. Allowed here: skills_list/skill_view \
                 to read, skill_manage to change skills. Do not retry {name}."
            ));
        }
        let memory = if self.allow_memory {
            " and memory for notes (add only)"
        } else {
            ""
        };
        ToolOutput::error(format!(
            "Background review denied non-whitelisted tool: {name}. Allowed here: \
             skill_view/skills_list/read_file/search_files to read, \
             skill_manage(action='patch'|...) to change skills{memory}. Do not retry {name}."
        ))
    }

    fn mark_read(&self, path: &Path) {
        self.read.lock().insert(read_key(path));
    }

    fn has_read(&self, path: &Path) -> bool {
        self.read.lock().contains(&read_key(path))
    }

    /// The review guards on a `skill_manage` call: `Some(refusal)` stops it.
    fn guard_skill_manage(&self, args: &Value) -> Option<ToolOutput> {
        let skills = self.tools.skills.as_ref()?;
        for op in requested_ops(args) {
            let Some(skill) = skills.find(&op.name) else {
                continue;
            };
            if op.action == "delete" && !self.curator {
                return Some(ToolOutput::error(format!(
                    "Refusing background review delete of skill '{}': a review never removes \
                     skills. If two skills overlap, say so in your reply; consolidation is the \
                     curator's job.",
                    op.name
                )));
            }
            let file = op
                .file_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty());
            let (target, label) = match (op.action.as_str(), file) {
                ("patch" | "edit", None) => (skill.skill_md(), "SKILL.md".to_string()),
                ("patch" | "write_file" | "remove_file", Some(file)) => {
                    (skill.dir.join(file), format!("supporting file '{file}'"))
                }
                _ => continue,
            };
            // A new supporting file needs no prior read.
            if !target.exists() || self.has_read(&target) {
                continue;
            }
            return Some(ToolOutput::error_with(
                format!(
                    "Refusing background curator {} for skill '{}': the current {label} content \
                     has not been loaded in this review turn. Call skill_view(name) for \
                     SKILL.md, or skill_view(name, file_path=...) for a supporting file, then \
                     retry the write using the content just returned.",
                    op.action, op.name
                ),
                json!({"_read_before_write_required": true}),
            ));
        }
        None
    }

    async fn dispatch(&self, name: &str, args: Value) -> ToolOutput {
        if !self.allowed(name) {
            return self.denial(name);
        }
        match name {
            "skill_manage" => {
                if let Some(refusal) = self.guard_skill_manage(&args) {
                    return refusal;
                }
                let Some(skills) = &self.tools.skills else {
                    return self.tools.dispatch(name, args, None).await;
                };
                // The review reads the whole conversation, so a tainted session's review writes
                // nothing either: its changes wait for the user (Phase 7 reworks the review).
                if self.tools.taint().is_tainted() {
                    return self.tools.stage_skills(&args, self.origin()).await;
                }
                let source = (!self.curator).then(|| self.tools.skill_source(WriteOrigin::Review));
                let result = skills.manage_with_source(&args, self.origin(), source.as_ref());
                let failed = result.get("success") == Some(&Value::Bool(false));
                ToolOutput {
                    value: result,
                    is_error: failed,
                }
            }
            "skill_view" => {
                let output = self.tools.dispatch(name, args.clone(), None).await;
                if !output.is_error {
                    if let Some(path) = self.viewed_path(&args) {
                        self.mark_read(&path);
                    }
                }
                output
            }
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|p| self.tools.resolve(p));
                // The review's own read: recorded in `self.read` for its guard, not in the
                // session's file state the turn's write guard checks.
                let output = crate::files::SIDE_READ
                    .scope((), self.tools.dispatch(name, args, None))
                    .await;
                if let (false, Some(path), None) =
                    (output.is_error, &path, output.value.get("files"))
                {
                    self.mark_read(path);
                }
                // `paths` (Phase 10): each file that was read, by the path its entry names.
                if let Some(Value::Array(files)) = output.value.get("files") {
                    for entry in files {
                        if entry.get("success") == Some(&Value::Bool(false)) {
                            continue;
                        }
                        if let Some(read) = entry.get("path").and_then(Value::as_str) {
                            self.mark_read(Path::new(read));
                        }
                    }
                }
                output
            }
            "memory" if !self.attended => {
                if self.tools.memory.is_none() {
                    return self.tools.dispatch(name, args, None).await;
                }
                if self.tools.taint().is_tainted() {
                    return self.tools.stage_memory(&args).await;
                }
                // Off the async workers, like the foreground tool.
                self.tools.run_memory_tool(&args, true).await
            }
            _ => self.tools.dispatch(name, args, None).await,
        }
    }

    /// The file a successful `skill_view` call loaded.
    fn viewed_path(&self, args: &Value) -> Option<std::path::PathBuf> {
        let name = args.get("name").and_then(Value::as_str)?.trim();
        let skill = self.tools.skills.as_ref()?.find(name)?;
        match args
            .get("file_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(file) => Some(skill.dir.join(file)),
            None => Some(skill.skill_md()),
        }
    }
}

/// The operations of a `skill_manage` call, leniently (`skill_manage` reports malformed ones).
fn requested_ops(args: &Value) -> Vec<SkillOp> {
    match args.get("operations") {
        Some(Value::Array(items)) => {
            let default_name = args.get("name").and_then(Value::as_str).unwrap_or_default();
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<SkillOp>(item.clone()).ok())
                .map(|mut op| {
                    if op.name.is_empty() {
                        op.name = default_name.to_string();
                    }
                    op
                })
                .collect()
        }
        _ => serde_json::from_value::<SkillOp>(args.clone())
            .ok()
            .into_iter()
            .collect(),
    }
}

fn read_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
        .replace('\\', "/")
        .to_lowercase()
}

#[async_trait]
impl<S: Send + Sync + 'static> ToolProvider<S> for ReviewTools {
    async fn tools(&self, session: &S) -> anyhow::Result<Vec<Tool>> {
        let tools = <SessionTools as ToolProvider<S>>::tools(&self.tools, session).await?;
        if !self.curator {
            return Ok(tools);
        }
        Ok(tools
            .into_iter()
            .filter(|tool| self.allowed(tool.name.as_ref()))
            .collect())
    }

    async fn call(
        &self,
        _session: &S,
        _request_id: &str,
        call: CallToolRequestParams,
        _emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        let args = Value::Object(call.arguments.unwrap_or_else(JsonObject::new));
        Ok(self.dispatch(&call.name, args).await.into_result())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionToolsConfig;
    use crate::skills::{SkillStore, SkillsConfig};

    fn review_tools(attended: bool) -> (tempfile::TempDir, ReviewTools, Arc<SkillStore>) {
        let dir = tempfile::tempdir().unwrap();
        let skills = Arc::new(SkillStore::new(
            dir.path().join("skills"),
            SkillsConfig::default(),
        ));
        let memory = Arc::new(hermes_memory::MemoryStore::new(
            dir.path().join("memories"),
            Default::default(),
        ));
        let tools = SessionTools::new(SessionToolsConfig {
            session_id: "ses_test".into(),
            cwd: dir.path().to_path_buf(),
            home: dir.path().to_path_buf(),
            data_dir: dir.path().join(".data"),
            scratch_dir: dir.path().join(".scratch"),
            shell: crate::detect_shell(),
            bridge: Arc::new(crate::tests::TestBridge::default()),
            todos: Vec::new(),
            memory: Some(memory),
            history: None,
            skills: Some(skills.clone()),
            approvals: None,
            checkpoints: None,
            web: None,
        });
        (
            dir,
            ReviewTools::new(Arc::new(tools), true, attended),
            skills,
        )
    }

    const SKILL: &str = "---\nname: deploy\ndescription: Deploy the app to staging.\n---\n\n# Deploy\n\nRun the script.\n";

    #[tokio::test]
    async fn only_the_review_tools_run() {
        let (_dir, review, _) = review_tools(false);
        let output = review
            .dispatch("terminal", json!({"command": "echo hi"}))
            .await;
        let error = output.value["error"].as_str().unwrap();
        assert!(error.starts_with("Background review denied non-whitelisted tool: terminal."));
    }

    #[tokio::test]
    async fn changing_a_skill_needs_a_fresh_view_and_deleting_is_refused() {
        let (_dir, review, skills) = review_tools(false);
        let created = skills.manage_tool(&json!({"operations": [
            {"action": "create", "name": "deploy", "content": SKILL}
        ]}));
        assert_eq!(created["success"], true, "{created}");
        let patch = json!({"operations": [
            {"action": "patch", "name": "deploy", "old_string": "Run the script.", "new_string": "Run the script twice."}
        ]});
        let refused = review.dispatch("skill_manage", patch.clone()).await;
        assert_eq!(
            refused.value["_read_before_write_required"], true,
            "{}",
            refused.value
        );
        let viewed = review
            .dispatch("skill_view", json!({"name": "deploy"}))
            .await;
        assert!(!viewed.is_error);
        let patched = review.dispatch("skill_manage", patch).await;
        assert_eq!(patched.value["success"], true, "{}", patched.value);
        let delete = review
            .dispatch(
                "skill_manage",
                json!({"operations": [{"action": "delete", "name": "deploy"}]}),
            )
            .await;
        assert!(
            delete.value["error"]
                .as_str()
                .unwrap()
                .starts_with("Refusing background review delete")
        );
        assert!(skills.find("deploy").is_some());
    }

    /// A multi-path `read_file` (Phase 10) counts for each file it read, as single reads would;
    /// a file that failed counts for nothing.
    #[tokio::test]
    async fn a_multi_path_read_counts_for_each_file_it_read() {
        let (_dir, review, skills) = review_tools(false);
        let created = skills.manage_tool(&json!({"operations": [
            {"action": "create", "name": "deploy", "content": SKILL}
        ]}));
        assert_eq!(created["success"], true, "{created}");
        let skill_md = skills.find("deploy").unwrap().skill_md();
        let patch = json!({"operations": [
            {"action": "patch", "name": "deploy", "old_string": "Run the script.", "new_string": "Run the script twice."}
        ]});
        let missing = review
            .dispatch("read_file", json!({"path": "no-such.md", "paths": ["also-missing.md"]}))
            .await;
        assert!(missing.is_error);
        let refused = review.dispatch("skill_manage", patch.clone()).await;
        assert_eq!(refused.value["_read_before_write_required"], true, "{}", refused.value);
        let read = review
            .dispatch(
                "read_file",
                json!({"paths": ["no-such.md", skill_md.display().to_string()]}),
            )
            .await;
        assert_eq!(read.value["files_read"], 1, "{}", read.value);
        let patched = review.dispatch("skill_manage", patch).await;
        assert_eq!(patched.value["success"], true, "{}", patched.value);
    }

    /// A review's `read_file` is its own: it doesn't satisfy the session's read-before-write
    /// guard (a review may run beside a turn, Phase 4).
    #[tokio::test]
    async fn a_review_read_does_not_count_as_the_sessions() {
        let (dir, review, _) = review_tools(false);
        std::fs::write(dir.path().join("notes.md"), "old\n").unwrap();
        let read = review
            .dispatch("read_file", json!({"path": "notes.md"}))
            .await;
        assert!(!read.is_error, "{}", read.value);
        let write = review
            .tools
            .dispatch(
                "write_file",
                json!({"path": "notes.md", "content": "new\n"}),
                None,
            )
            .await;
        assert!(write.is_error, "{}", write.value);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "old\n"
        );
        // The session's own read still counts.
        let read = review
            .tools
            .dispatch("read_file", json!({"path": "notes.md"}), None)
            .await;
        assert!(!read.is_error);
        let write = review
            .tools
            .dispatch(
                "write_file",
                json!({"path": "notes.md", "content": "new\n"}),
                None,
            )
            .await;
        assert!(!write.is_error, "{}", write.value);
    }
}
