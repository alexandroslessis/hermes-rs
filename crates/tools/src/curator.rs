//! Skill lifecycle and the curator's bookkeeping, after Hermes's `tools/skill_usage.py` and
//! `agent/curator.py` (MIT).
//!
//! Every skill has a record in `.usage.json`: how often it was viewed, loaded and patched, when it
//! was last active, who created it, whether it's pinned, and its state (active, stale, archived).
//! The curator only ever manages skills a background review created (`created_by: agent`) or the
//! user handed over, never pinned, bundled or external ones; it marks them stale after 14 idle
//! days and archives them after 30. Archiving moves the folder into `.archive/`, so it can be
//! restored; nothing is deleted.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::skill_manage::{SkillOp, WriteOrigin};
use crate::skills::SkillStore;

pub(crate) const USAGE: &str = ".usage.json";
const ARCHIVE: &str = ".archive";
const STATE: &str = ".curator_state";
const MANIFEST: &str = ".bundled_manifest.json";

/// Hermes's defaults: `curator.stale_after_days`, `archive_after_days`, `interval_hours`,
/// `min_idle_hours`.
pub const STALE_AFTER_DAYS: i64 = 14;
pub const ARCHIVE_AFTER_DAYS: i64 = 30;
pub const INTERVAL_HOURS: i64 = 24 * 7;
pub const MIN_IDLE_HOURS: i64 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillState {
    #[default]
    Active,
    Stale,
    Archived,
}

/// One skill's record in `.usage.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillRecord {
    pub views: u64,
    pub uses: u64,
    pub patches: u64,
    /// Last time it was loaded whole.
    pub last_used: Option<String>,
    pub last_viewed: Option<String>,
    pub last_patched: Option<String>,
    /// `agent` (a background review made it, so the curator manages it) or `learn` (the agent
    /// made it in a session the user was in).
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    /// When the curator first saw it, anchoring the inactivity clock of skills with no activity.
    pub first_seen: Option<String>,
    pub state: SkillState,
    pub pinned: bool,
    pub archived_at: Option<String>,
    /// Where it lived before archiving, relative to the skills folder.
    pub archived_from: Option<String>,
}

impl SkillRecord {
    /// Whether the curator may manage the skill (Hermes's `_is_curator_managed_record`).
    pub fn curator_managed(&self) -> bool {
        self.created_by.as_deref() == Some("agent")
    }

    /// The latest real activity: a load, a view or a patch.
    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        [&self.last_used, &self.last_viewed, &self.last_patched]
            .into_iter()
            .filter_map(|t| parse_time(t.as_deref()))
            .max()
    }
}

/// A curator-managed skill, for the Skills screen and the curator's candidate list.
#[derive(Clone, Debug, PartialEq)]
pub struct CuratedSkill {
    pub name: String,
    pub record: SkillRecord,
}

/// An archived skill.
#[derive(Clone, Debug, PartialEq)]
pub struct ArchivedSkill {
    pub name: String,
    pub archived_at: Option<String>,
    pub dir: PathBuf,
}

/// What a deterministic pass changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransitionCounts {
    pub checked: usize,
    pub marked_stale: usize,
    pub archived: usize,
    pub reactivated: usize,
    pub seeded: usize,
}

impl TransitionCounts {
    /// Hermes's `auto_summary`: "2 marked stale, 1 archived", or "no changes".
    pub fn summary(&self) -> String {
        let parts: Vec<String> = [
            (self.marked_stale, "marked stale"),
            (self.archived, "archived"),
            (self.reactivated, "reactivated"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();
        if parts.is_empty() {
            "no changes".into()
        } else {
            parts.join(", ")
        }
    }
}

/// The curator's scheduler state (`.curator_state`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CuratorState {
    pub last_run_at: Option<String>,
    pub last_run_summary: Option<String>,
    pub last_report_path: Option<String>,
    pub paused: bool,
    pub run_count: u64,
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn parse_time(text: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl SkillStore {
    pub(crate) fn load_records(&self) -> HashMap<String, SkillRecord> {
        std::fs::read(self.root().join(USAGE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_records(&self, records: &HashMap<String, SkillRecord>) {
        let sorted: BTreeMap<_, _> = records.iter().collect();
        let path = self.root().join(USAGE);
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::write(&tmp, serde_json::to_vec_pretty(&sorted).unwrap_or_default())
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(error) = written {
            tracing::warn!("saving skill usage failed: {error}");
        }
    }

    /// Changes one skill's record.
    pub(crate) fn update_record(&self, name: &str, change: impl FnOnce(&mut SkillRecord)) {
        let _guard = self.usage_lock.lock();
        let mut records = self.load_records();
        change(records.entry(name.to_string()).or_default());
        self.save_records(&records);
    }

    pub fn record(&self, name: &str) -> SkillRecord {
        self.load_records().remove(name).unwrap_or_default()
    }

    /// What a successful `skill_manage` op means for the skill's record (Hermes's
    /// `record_created` / `bump_patch`).
    pub(crate) fn record_write(&self, op: &SkillOp, origin: WriteOrigin) {
        let now = now_string();
        match op.action.as_str() {
            "create" => self.update_record(&op.name, |record| {
                *record = SkillRecord {
                    created_by: Some(
                        if origin == WriteOrigin::Foreground {
                            "learn"
                        } else {
                            "agent"
                        }
                        .into(),
                    ),
                    created_at: Some(now),
                    ..Default::default()
                };
            }),
            "patch" | "edit" | "write_file" | "remove_file" => {
                self.update_record(&op.name, |record| {
                    record.patches += 1;
                    record.last_patched = Some(now);
                    if record.state == SkillState::Stale {
                        record.state = SkillState::Active;
                    }
                })
            }
            _ => {}
        }
    }

    /// Pinned skills are never touched by the curator (deletion stays possible by hand).
    pub fn set_pinned(&self, name: &str, pinned: bool) {
        self.update_record(name, |record| record.pinned = pinned);
    }

    /// Hermes's `curator adopt`: hands a skill the user or agent wrote over to the curator.
    pub fn adopt(&self, name: &str) -> Result<(), String> {
        let skill = self
            .find(name)
            .ok_or_else(|| format!("Skill '{name}' not found."))?;
        if skill.external {
            return Err(format!(
                "'{name}' lives in an external folder and is read-only to the curator."
            ));
        }
        if self.is_bundled(&skill.dir) {
            return Err(format!(
                "'{name}' is bundled with the app; the curator never manages it."
            ));
        }
        self.update_record(&skill.name, |record| {
            record.created_by = Some("agent".into())
        });
        Ok(())
    }

    /// Takes a skill back from the curator.
    pub fn release(&self, name: &str) {
        self.update_record(name, |record| {
            if record.curator_managed() {
                record.created_by = Some("learn".into());
            }
        });
    }

    /// Whether the skill came with the app (the seeding manifest lists its files).
    pub fn is_bundled(&self, dir: &Path) -> bool {
        let Ok(rel) = dir.strip_prefix(self.root()) else {
            return false;
        };
        let prefix = format!("{}/", rel.to_string_lossy().replace('\\', "/"));
        std::fs::read(self.root().join(MANIFEST))
            .ok()
            .and_then(|b| serde_json::from_slice::<HashMap<String, String>>(&b).ok())
            .is_some_and(|manifest| manifest.keys().any(|file| file.starts_with(&prefix)))
    }

    /// Curator-managed skills that exist now, plus pinned ones so their pins stay visible.
    pub fn curated(&self) -> Vec<CuratedSkill> {
        let records = self.load_records();
        let mut skills: Vec<CuratedSkill> = self
            .catalog()
            .into_iter()
            .filter(|skill| !skill.external)
            .filter_map(|skill| {
                let record = records.get(&skill.name)?;
                (record.curator_managed() || record.pinned).then(|| CuratedSkill {
                    name: skill.name.clone(),
                    record: record.clone(),
                })
            })
            .collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    /// Moves a skill's folder into `.archive/` (Hermes's `archive_skill`). Only curator-managed,
    /// unpinned skills of this folder.
    pub fn archive(&self, name: &str) -> Result<(), String> {
        self.archive_skill(name, true)
    }

    /// [`Self::archive`] for the user's own delete (the Star map): any skill of this folder that
    /// isn't pinned, curator-managed or not (Hermes's `skill_usage.archive_skill`). Restorable.
    pub fn archive_by_user(&self, name: &str) -> Result<(), String> {
        self.archive_skill(name, false)
    }

    fn archive_skill(&self, name: &str, curator: bool) -> Result<(), String> {
        let skill = self
            .find(name)
            .ok_or_else(|| format!("Skill '{name}' not found."))?;
        if skill.external {
            return Err(format!(
                "'{name}' lives in an external folder and is read-only here."
            ));
        }
        let record = self.record(&skill.name);
        if record.pinned {
            return Err(format!("'{name}' is pinned."));
        }
        if curator && !record.curator_managed() {
            return Err(format!(
                "'{name}' isn't curator-managed, so the curator won't archive it."
            ));
        }
        let archive = self.root().join(ARCHIVE);
        std::fs::create_dir_all(&archive).map_err(|e| e.to_string())?;
        let mut dest = archive.join(&skill.name);
        if dest.exists() {
            dest = archive.join(format!(
                "{}-{}",
                skill.name,
                Utc::now().format("%Y%m%d%H%M%S")
            ));
        }
        let from = skill
            .dir
            .strip_prefix(self.root())
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| skill.name.clone());
        std::fs::rename(&skill.dir, &dest).map_err(|e| e.to_string())?;
        if let Some(parent) = skill.dir.parent().filter(|p| *p != self.root()) {
            let _ = std::fs::remove_dir(parent);
        }
        self.update_record(&skill.name, |record| {
            record.state = SkillState::Archived;
            record.archived_at = Some(now_string());
            record.archived_from = Some(from);
        });
        let who = if curator { "curator" } else { "user" };
        self.commit(&format!("{who}: archive {}", skill.name));
        Ok(())
    }

    /// Archived skills, newest first.
    pub fn archived(&self) -> Vec<ArchivedSkill> {
        let records = self.load_records();
        let Ok(entries) = std::fs::read_dir(self.root().join(ARCHIVE)) else {
            return Vec::new();
        };
        let mut skills: Vec<ArchivedSkill> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("SKILL.md").is_file())
            .map(|dir| {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let archived_at = records.get(&name).and_then(|r| r.archived_at.clone());
                ArchivedSkill {
                    name,
                    archived_at,
                    dir,
                }
            })
            .collect();
        skills.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        skills
    }

    /// Moves an archived skill back (Hermes's `restore_skill`): to where it was when that's
    /// free, else to the top of the skills folder.
    pub fn restore_archived(&self, name: &str) -> Result<(), String> {
        let archived = self
            .archived()
            .into_iter()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("'{name}' is not in the archive."))?;
        if self.find_any(name).is_some() {
            return Err(format!(
                "A skill named '{name}' exists again; rename or delete it first."
            ));
        }
        let record = self.record(name);
        let dest = record
            .archived_from
            .as_deref()
            .map(|rel| self.root().join(rel))
            .filter(|dest| !dest.exists())
            .unwrap_or_else(|| self.root().join(name));
        if dest.exists() {
            return Err(format!("{} already exists.", dest.display()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&archived.dir, &dest).map_err(|e| e.to_string())?;
        // Restoring counts as activity, so the next pass doesn't archive it again at once.
        self.update_record(name, |record| {
            record.state = SkillState::Active;
            record.archived_at = None;
            record.archived_from = None;
            record.first_seen = Some(now_string());
        });
        self.commit(&format!("curator: restore {name}"));
        Ok(())
    }

    /// A curator `delete`: consolidation may only archive a skill it absorbed into an existing
    /// umbrella (Hermes's `_curator_consolidation_delete_guard`).
    pub(crate) fn archive_for_curator(&self, op: &SkillOp) -> Value {
        let absorbed = op
            .absorbed_into
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty());
        let Some(absorbed) = absorbed else {
            return json!({"success": false, "error": format!(
                "Refusing background curator delete of skill '{}': the consolidation pass may only archive a skill it has absorbed into an umbrella. Pass absorbed_into=<umbrella> (the umbrella must already exist) to record a verified consolidation. Pruning a skill with no forwarding target is not permitted here — the deterministic inactivity prune handles staleness archival separately. Keeping '{}' active.",
                op.name, op.name
            )});
        };
        if absorbed == op.name {
            return json!({"success": false, "error": format!("absorbed_into='{absorbed}' cannot equal the skill being deleted.")});
        }
        if self.find_any(absorbed).is_none() {
            return json!({"success": false, "error": format!("absorbed_into='{absorbed}' does not exist. Create or patch the umbrella skill first, then retry the delete.")});
        }
        match self.archive(&op.name) {
            Ok(()) => json!({
                "success": true,
                "message": format!("Skill '{}' archived. Content absorbed into '{absorbed}'.", op.name),
                "_archived": true,
            }),
            Err(error) => json!({"success": false, "error": error}),
        }
    }

    /// Hermes's `apply_automatic_transitions`: moves each curator-managed, unpinned skill
    /// between active, stale and archived by its last activity. A skill never used anchors on
    /// when it was created or first seen, and is never archived younger than the stale window.
    pub fn apply_automatic_transitions(&self, now: DateTime<Utc>) -> TransitionCounts {
        let stale_cutoff = now - Duration::days(STALE_AFTER_DAYS);
        let archive_cutoff = now - Duration::days(ARCHIVE_AFTER_DAYS);
        let mut counts = TransitionCounts::default();
        for skill in self.curated() {
            counts.checked += 1;
            let record = skill.record;
            if record.pinned {
                continue;
            }
            let anchor = record
                .last_activity()
                .or_else(|| parse_time(record.created_at.as_deref()))
                .or_else(|| parse_time(record.first_seen.as_deref()));
            let Some(anchor) = anchor else {
                // First sight with no clock: start it now.
                self.update_record(&skill.name, |r| r.first_seen = Some(now.to_rfc3339()));
                counts.seeded += 1;
                continue;
            };
            let set = |state: SkillState| self.update_record(&skill.name, |r| r.state = state);
            if record.uses == 0 && anchor > stale_cutoff {
                if record.state == SkillState::Stale {
                    set(SkillState::Active);
                    counts.reactivated += 1;
                }
                continue;
            }
            if anchor <= archive_cutoff {
                if self.archive(&skill.name).is_ok() {
                    counts.archived += 1;
                }
            } else if anchor <= stale_cutoff && record.state == SkillState::Active {
                set(SkillState::Stale);
                counts.marked_stale += 1;
            } else if anchor > stale_cutoff && record.state == SkillState::Stale {
                set(SkillState::Active);
                counts.reactivated += 1;
            }
        }
        counts
    }

    pub fn curator_state(&self) -> CuratorState {
        std::fs::read(self.root().join(STATE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_curator_state(&self, state: &CuratorState) {
        let _ = std::fs::create_dir_all(self.root());
        let _ = std::fs::write(
            self.root().join(STATE),
            serde_json::to_vec_pretty(state).unwrap_or_default(),
        );
    }

    /// Hermes's `should_run_now`: not paused, and the last run is older than the interval. The
    /// first check only records "now", so a fresh install waits one interval before the curator
    /// changes anything.
    pub fn curator_due(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.curator_state();
        if state.paused {
            return false;
        }
        match parse_time(state.last_run_at.as_deref()) {
            Some(last) => now - last >= Duration::hours(INTERVAL_HOURS),
            None => {
                state.last_run_at = Some(now.to_rfc3339());
                state.last_run_summary = Some(
                    "deferred first run — the curator starts after one interval; use Run Now to run it sooner".into(),
                );
                self.save_curator_state(&state);
                false
            }
        }
    }

    /// Hermes's `_render_candidate_list`: the agent-created skills the consolidation pass works on.
    pub fn curator_candidates(&self) -> String {
        let rows: Vec<CuratedSkill> = self
            .curated()
            .into_iter()
            .filter(|s| s.record.curator_managed())
            .collect();
        if rows.is_empty() {
            return "No agent-created skills to review.".into();
        }
        let mut lines = vec![format!("Agent-created skills ({}):\n", rows.len())];
        for skill in rows {
            let r = &skill.record;
            let state = match r.state {
                SkillState::Active => "active",
                SkillState::Stale => "stale",
                SkillState::Archived => "archived",
            };
            lines.push(format!(
                "- {}  provenance=agent  state={state}  pinned={}  cron=no  activity={}  use={}  view={}  patches={}  last_activity={}",
                skill.name,
                if r.pinned { "yes" } else { "no" },
                r.uses + r.views + r.patches,
                r.uses,
                r.views,
                r.patches,
                r.last_activity()
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "never".into()),
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillsConfig;

    fn store_with(names: &[&str], origin: WriteOrigin) -> (tempfile::TempDir, SkillStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        for name in names {
            let content = format!(
                "---\nname: {name}\ndescription: Skill {name}.\n---\n\n# {name}\n\nSteps.\n"
            );
            let created = store.manage_with(
                &json!({"operations": [{"action": "create", "name": name, "category": "work", "content": content}]}),
                origin,
            );
            assert_eq!(created["success"], true, "{created}");
        }
        (dir, store)
    }

    fn backdate(store: &SkillStore, name: &str, days: i64, used: bool) {
        let then = (Utc::now() - Duration::days(days)).to_rfc3339();
        store.update_record(name, |r| {
            r.created_at = Some(then.clone());
            if used {
                r.uses = 1;
                r.last_used = Some(then);
            }
        });
    }

    #[test]
    fn only_review_created_skills_are_curated() {
        let (_dir, review) = store_with(&["a"], WriteOrigin::Review);
        assert!(review.record("a").curator_managed());
        let (_dir, foreground) = store_with(&["b"], WriteOrigin::Foreground);
        assert!(!foreground.record("b").curator_managed());
        assert!(foreground.curated().is_empty());
        foreground.adopt("b").unwrap();
        assert_eq!(foreground.curated().len(), 1);
    }

    #[test]
    fn idle_skills_go_stale_then_to_the_archive_and_come_back() {
        let (_dir, store) = store_with(&["old", "idle", "fresh", "pinned"], WriteOrigin::Review);
        backdate(&store, "old", 40, true);
        backdate(&store, "idle", 20, true);
        backdate(&store, "pinned", 40, true);
        store.set_pinned("pinned", true);
        let counts = store.apply_automatic_transitions(Utc::now());
        assert_eq!((counts.archived, counts.marked_stale), (1, 1), "{counts:?}");
        assert_eq!(counts.summary(), "1 marked stale, 1 archived");
        assert!(store.find("old").is_none());
        assert_eq!(store.record("idle").state, SkillState::Stale);
        assert_eq!(store.record("fresh").state, SkillState::Active);
        assert!(store.find("pinned").is_some());
        assert_eq!(store.archived()[0].name, "old");

        store.restore_archived("old").unwrap();
        let restored = store.find("old").unwrap();
        assert!(restored.dir.ends_with("work/old"));
        assert_eq!(store.record("old").state, SkillState::Active);
        // A patch counts as activity and wakes a stale skill.
        let view = store.view_tool(&json!({"name": "idle"}));
        assert_eq!(view["success"], true);
        let patched = store.manage_tool(&json!({"operations": [{"action": "patch", "name": "idle", "old_string": "Steps.", "new_string": "Steps, carefully."}]}));
        assert_eq!(patched["success"], true, "{patched}");
        assert_eq!(store.record("idle").state, SkillState::Active);
    }

    #[test]
    fn never_used_skills_are_not_archived_while_young() {
        let (_dir, store) = store_with(&["new"], WriteOrigin::Review);
        backdate(&store, "new", 5, false);
        let counts = store.apply_automatic_transitions(Utc::now());
        assert_eq!(counts.summary(), "no changes");
        assert!(store.find("new").is_some());
    }

    #[test]
    fn a_curator_delete_archives_only_what_it_absorbed() {
        let (_dir, store) = store_with(&["umbrella", "narrow"], WriteOrigin::Review);
        let bare = store.manage_with(
            &json!({"operations": [{"action": "delete", "name": "narrow"}]}),
            WriteOrigin::Curator,
        );
        assert!(
            bare["error"]
                .as_str()
                .unwrap()
                .starts_with("Refusing background curator delete")
        );
        let absorbed = store.manage_with(
            &json!({"operations": [{"action": "delete", "name": "narrow", "absorbed_into": "umbrella"}]}),
            WriteOrigin::Curator,
        );
        assert_eq!(absorbed["success"], true, "{absorbed}");
        assert!(store.find("narrow").is_none());
        assert_eq!(store.archived().len(), 1);
    }

    #[test]
    fn the_first_check_defers_the_curator_one_interval() {
        let (_dir, store) = store_with(&[], WriteOrigin::Review);
        let now = Utc::now();
        assert!(!store.curator_due(now));
        assert!(!store.curator_due(now + Duration::hours(INTERVAL_HOURS - 1)));
        assert!(store.curator_due(now + Duration::hours(INTERVAL_HOURS)));
    }
}
