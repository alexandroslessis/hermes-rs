//! Applying a validated review reply, one operation at a time, through the same doors as any
//! other write: memory through `MemoryStore::apply` (its limits and threat scan), skills through
//! `skill_manage`'s write path (`SkillStore::apply_staged`: validation, the threat scan of the
//! whole skill, the git commit, the index hook). Nothing here writes a file itself.
//!
//! - **One at a time**: each operation is its own write, so a refused one never takes the others
//!   down (the stores' batches are all-or-nothing; a review's ops are independent lessons).
//! - **Held for the user** when the session is tainted (it read something the user didn't write:
//!   the evidence may carry an injection), when `review.require_approval` is on, or when the
//!   store's own `write_approval` is on: the operation is checked the same way and goes to the
//!   staging area (`hermes_tools::staging`) with the reason and the sources, one item per op.
//!   A memory **replace or remove** is held in every session (nobody is watching a background
//!   review; Hermes's `_background_delete_gate`): only adds and skill writes of an untainted
//!   session are written directly.
//! - **Skills are patched only as seen**: a patch must name a skill the packet showed, the file
//!   must still have the fingerprint it had when read (else the user or another writer changed
//!   it meanwhile), a diff must apply exactly (`udiff.rs`), and a whole-body replacement is taken
//!   only for a skill shown whole. A create may not take an existing name. Skills are never
//!   deleted, and supporting files are out of reach.
//!
//! Blocking file work: run it through `spawn_blocking`.

use std::collections::BTreeMap;
use std::sync::Arc;

use hermes_memory::{MemoryStore, Operation, Target, TurnFailures};
use hermes_tools::skill_manage::{FileBase, SkillOp, StagedSkillError, WriteOrigin, fingerprint};
use hermes_tools::skills::SkillStore;
use hermes_tools::staging::{Meta, StagingStore};
use serde_json::{Value, json};

use super::contract::{MemoryAction, MemoryChange, Parsed, SkillAction, SkillChange};
use super::packet::ShownSkill;
use super::udiff;

/// Why every write of this review waits for the user.
#[derive(Clone, Debug, PartialEq)]
pub struct Hold {
    pub reason: String,
    pub sources: Vec<String>,
}

/// What applying needs.
pub struct ApplyContext {
    pub memory: Arc<MemoryStore>,
    pub skills: Arc<SkillStore>,
    pub staging: StagingStore,
    pub session_id: String,
    /// `Some`: everything is staged instead of written.
    pub hold: Option<Hold>,
    /// The skills the packet showed (filled in from the packet).
    pub shown: BTreeMap<String, ShownSkill>,
}

/// What became of one operation.
#[derive(Clone, Debug, PartialEq)]
pub enum OpResult {
    /// Written.
    Applied(String),
    /// Waiting in the staging area for the user.
    Staged(String),
    /// Refused (by validation, a guard, or the store), with why.
    Rejected(String),
}

/// One operation of the reply and its result.
#[derive(Clone, Debug, PartialEq)]
pub struct OpReport {
    /// `memory`, `skills` or `reply`.
    pub section: &'static str,
    /// Its place in the reply's list.
    pub index: usize,
    /// "memory add (user)", "skill patch deploy"; empty for one refused before it was read.
    pub what: String,
    pub result: OpResult,
}

impl OpReport {
    /// The line the app shows for a change (Hermes's review summary wording), if it is one.
    pub fn action_line(&self) -> Option<String> {
        match (&self.result, self.section) {
            (OpResult::Staged(_), "memory") => {
                Some("Memory change waiting for your approval".into())
            }
            (OpResult::Staged(_), _) => Some("Skill change waiting for your approval".into()),
            (OpResult::Applied(_), "memory") if self.what.ends_with("(user)") => {
                Some("User profile updated".into())
            }
            (OpResult::Applied(_), "memory") => Some("Memory updated".into()),
            (OpResult::Applied(message), _) => Some(message.clone()),
            _ => None,
        }
    }
}

/// Applies (or stages) every valid operation of `parsed`; the refused ones are reported too.
pub fn apply_all(ctx: &ApplyContext, parsed: &Parsed) -> Vec<OpReport> {
    let mut reports: Vec<OpReport> = parsed
        .rejected
        .iter()
        .map(|r| OpReport {
            section: r.section,
            index: r.index,
            what: String::new(),
            result: OpResult::Rejected(r.reason.clone()),
        })
        .collect();
    for change in &parsed.memory {
        reports.push(OpReport {
            section: "memory",
            index: change.index,
            what: format!("memory {} ({})", change.action.name(), change.target.name()),
            result: apply_memory(ctx, change),
        });
    }
    for change in &parsed.skills {
        reports.push(OpReport {
            section: "skills",
            index: change.index,
            what: format!("skill {} {}", change.action.name(), change.name),
            result: apply_skill(ctx, change),
        });
    }
    reports.sort_by_key(|r| (section_order(r.section), r.index));
    reports
}

fn section_order(section: &str) -> u8 {
    match section {
        "reply" => 0,
        "memory" => 1,
        _ => 2,
    }
}

fn meta(ctx: &ApplyContext, hold: &Hold) -> Meta {
    Meta {
        session_id: ctx.session_id.clone(),
        sources: hold.sources.clone(),
        reason: hold.reason.clone(),
    }
}

/// The hold for a store whose own `write_approval` is on (the review has none of its own).
fn write_approval_hold() -> Hold {
    Hold {
        reason: "write approval is on".into(),
        sources: Vec::new(),
    }
}

/// The hold for a memory replace or remove in a session that holds nothing else: the review runs
/// with nobody watching, and a change or deletion of what the user already has waits for them
/// (Hermes's `_background_delete_gate`; `MemoryStore::unattended_tool` for the agentic review).
/// The evidence quotes tool output, which a session needn't be tainted to carry.
fn unattended_hold() -> Hold {
    Hold {
        reason: "a background review may not change or delete memory entries unattended".into(),
        sources: Vec::new(),
    }
}

/// A store reply (`{"success": ..., "error"|"message": ...}`) as a result.
fn store_result(value: &Value, staged: bool, done: &str) -> OpResult {
    if value["success"] != json!(true) {
        let error = value["error"].as_str().unwrap_or("the store refused it");
        return OpResult::Rejected(error.chars().take(500).collect());
    }
    if staged || value["staged"] == json!(true) {
        return OpResult::Staged(hermes_tools::staging::SAVED_FOR_REVIEW.into());
    }
    OpResult::Applied(done.to_string())
}

fn apply_memory(ctx: &ApplyContext, change: &MemoryChange) -> OpResult {
    let memory = &ctx.memory;
    if !memory.enabled(change.target) {
        return OpResult::Rejected(format!(
            "{} writes are off in Settings",
            change.target.file_name()
        ));
    }
    let op = Operation {
        action: change.action.name().to_string(),
        content: change.text.clone(),
        old_text: change.old.clone(),
        matched_entry: None,
    };
    let hold = ctx
        .hold
        .clone()
        .or_else(|| memory.config().write_approval.then(write_approval_hold))
        .or_else(|| (change.action != MemoryAction::Add).then(unattended_hold));
    let done = match change.target {
        Target::User => "User profile updated",
        Target::Memory => "Memory updated",
    };
    match hold {
        Some(hold) => {
            let value =
                ctx.staging
                    .stage_memory_ops(memory, change.target, vec![op], &meta(ctx, &hold));
            store_result(&value, true, done)
        }
        None => {
            let value = memory.apply(change.target, &[op], &mut TurnFailures::default());
            store_result(&value, false, done)
        }
    }
}

/// The write a skill change makes, and the file state it must find: `Err` is why it is refused.
fn plan_skill(ctx: &ApplyContext, change: &SkillChange) -> Result<(SkillOp, FileBase), String> {
    let name = &change.name;
    match change.action {
        SkillAction::Create => {
            if let Some(existing) = ctx.skills.find_any(name) {
                return Err(format!(
                    "a skill named '{name}' already exists ({}); patch it instead",
                    existing.dir.display()
                ));
            }
            let body = if udiff::is_unified_diff(&change.patch) {
                udiff::apply("", &change.patch)
                    .map_err(|e| format!("the diff for a new skill does not apply: {e}"))?
            } else {
                change.patch.clone()
            };
            let op = SkillOp {
                action: "create".into(),
                name: name.clone(),
                content: Some(body),
                ..Default::default()
            };
            let base = FileBase {
                skill: name.clone(),
                file: "SKILL.md".into(),
                fingerprint: None,
            };
            Ok((op, base))
        }
        SkillAction::Patch => {
            let Some(shown) = ctx.shown.get(name) else {
                return Err(format!(
                    "skill '{name}' was not in the evidence packet; only a skill shown there may be patched"
                ));
            };
            let Some(entry) = ctx.skills.find_any(name) else {
                return Err(format!("skill '{name}' no longer exists"));
            };
            let bytes = std::fs::read(entry.skill_md())
                .map_err(|e| format!("could not read {name}/SKILL.md: {e}"))?;
            let now = fingerprint(&bytes);
            if now != shown.fingerprint {
                return Err(format!(
                    "{name}/SKILL.md changed after the review read it; nothing was written"
                ));
            }
            let current = String::from_utf8(bytes)
                .map_err(|_| format!("{name}/SKILL.md is not UTF-8 text"))?;
            let updated = if udiff::is_unified_diff(&change.patch) {
                udiff::apply(&current, &change.patch)
                    .map_err(|e| format!("the diff does not apply cleanly: {e}"))?
            } else if shown.whole {
                change.patch.clone()
            } else {
                return Err(format!(
                    "skill '{name}' was not shown whole (cut or masked), so it may only be patched with a unified diff"
                ));
            };
            if updated == current {
                return Err("the patch changes nothing".into());
            }
            let op = SkillOp {
                action: "edit".into(),
                name: name.clone(),
                content: Some(updated),
                ..Default::default()
            };
            let base = FileBase {
                skill: name.clone(),
                file: "SKILL.md".into(),
                fingerprint: Some(now),
            };
            Ok((op, base))
        }
    }
}

fn apply_skill(ctx: &ApplyContext, change: &SkillChange) -> OpResult {
    let (op, base) = match plan_skill(ctx, change) {
        Ok(planned) => planned,
        Err(reason) => return OpResult::Rejected(reason),
    };
    let skills = &ctx.skills;
    let done = match change.action {
        SkillAction::Create => format!("Skill '{}' created", change.name),
        SkillAction::Patch => format!("Skill '{}' patched", change.name),
    };
    let hold = ctx
        .hold
        .clone()
        .or_else(|| skills.config().write_approval.then(write_approval_hold));
    match hold {
        Some(hold) => {
            let value = ctx.staging.stage_skill_ops(
                skills,
                vec![op],
                WriteOrigin::Review,
                &meta(ctx, &hold),
            );
            store_result(&value, true, &done)
        }
        // Under the skill store's write lock, the file must still be what was read.
        None => match skills.apply_staged(&[op], WriteOrigin::Review, &[base]) {
            Ok(value) => store_result(&value, false, &done),
            Err(StagedSkillError::Stale(message)) => OpResult::Rejected(message),
        },
    }
}
