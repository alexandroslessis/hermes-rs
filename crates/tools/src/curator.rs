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
    /// Turns the local index attached it to the user's message (Phase 8), and the last time.
    /// Kept apart from `uses`/`last_used` (a whole-skill load), which keep their meaning: the
    /// Star map counts a skill with `uses > 0` as learned, and a bundled skill the index only
    /// attached is not. The curator counts both as activity.
    #[serde(skip_serializing_if = "is_zero")]
    pub attached: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attached: Option<String>,
    /// How the turns it was loaded or attached in went (Phase 8): the newest
    /// [`MAX_OUTCOMES`], oldest first. Old files have none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<UseEntry>,
    /// Share of `outcomes` that were `clean` (a summary kept in the file for other readers, set
    /// whenever the history changes; `None` until a use has an outcome).
    pub clean_rate: Option<f64>,
    /// Fields this version doesn't know (a newer one wrote them): kept as they are.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Uses a skill keeps outcomes for (the oldest go first).
pub const MAX_OUTCOMES: usize = 50;

/// How the turn a skill was used in went, as far as the learning signals tell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Nothing went wrong that the signals can see (so far: a later turn may still correct it).
    Clean,
    /// The user corrected the agent in the next message, or denied a command in the turn.
    Corrected,
    /// The turn was rewound, retried, edited away or rolled back.
    Undone,
    /// The turn failed, or a tool failed twice before working with other arguments.
    Errored,
    /// A value a newer version wrote: kept, never changed.
    #[serde(untagged)]
    Other(String),
}

impl Outcome {
    /// How bad: a later signal may make an outcome worse, never better.
    fn severity(&self) -> Option<u8> {
        match self {
            Outcome::Clean => Some(0),
            Outcome::Errored => Some(1),
            Outcome::Corrected => Some(2),
            Outcome::Undone => Some(3),
            Outcome::Other(_) => None,
        }
    }
}

/// One use of a skill and how it went. `turn` is the user turn's number in the session
/// (1 = the first message), as the learning signals number them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UseEntry {
    pub session: String,
    pub turn: u64,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl SkillRecord {
    /// Whether the curator may manage the skill (Hermes's `_is_curator_managed_record`).
    pub fn curator_managed(&self) -> bool {
        self.created_by.as_deref() == Some("agent")
    }

    /// Recomputes `clean_rate` from the history.
    fn refresh_summary(&mut self) {
        self.clean_rate = (!self.outcomes.is_empty()).then(|| {
            let clean = self
                .outcomes
                .iter()
                .filter(|e| e.outcome == Outcome::Clean)
                .count();
            (clean as f64 / self.outcomes.len() as f64 * 1000.0).round() / 1000.0
        });
    }

    /// The latest real activity: a load, an attachment by the local index, a view or a patch.
    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        [
            &self.last_used,
            &self.last_attached,
            &self.last_viewed,
            &self.last_patched,
        ]
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

/// What `.usage.json` holds: the skills' records, and any entry that isn't a record (kept as
/// it is, so a file a newer version wrote loses nothing here).
#[derive(Default)]
struct UsageFile {
    records: HashMap<String, SkillRecord>,
    foreign: BTreeMap<String, Value>,
}

impl UsageFile {
    /// `name`'s record, made when there is none. An entry of that name that wasn't a record
    /// gives the new one its fields (an object's are kept in `extra`).
    fn record_mut(&mut self, name: &str) -> &mut SkillRecord {
        if !self.records.contains_key(name) {
            let mut record = SkillRecord::default();
            if let Some(Value::Object(fields)) = self.foreign.remove(name) {
                record.extra = fields.into_iter().collect();
            }
            self.records.insert(name.to_string(), record);
        }
        self.records.get_mut(name).expect("inserted above")
    }
}

/// The fields of a [`SkillRecord`] (an object with none of them is not a record).
const RECORD_KEYS: &[&str] = &[
    "views",
    "uses",
    "patches",
    "last_used",
    "last_viewed",
    "last_patched",
    "created_by",
    "created_at",
    "first_seen",
    "state",
    "pinned",
    "archived_at",
    "archived_from",
    "attached",
    "last_attached",
    "outcomes",
    "clean_rate",
];

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A `.usage.json` entry as a record. `None`: not a record (not an object, or an object with
/// none of the record's fields, like a newer version's `_meta`), kept as it is.
///
/// A hand-edited or older file may hold a field with the wrong type (`"uses": "3"`, `null`,
/// `"state": "Stale"`): such a field is coerced where the meaning is plain, else dropped, so
/// one bad field never costs the rest of the record (which a save would then overwrite).
fn record_from(value: &Value) -> Option<SkillRecord> {
    let Value::Object(map) = value else {
        return None;
    };
    if !map.keys().any(|k| RECORD_KEYS.contains(&k.as_str())) {
        return None;
    }
    if let Ok(record) = serde_json::from_value::<SkillRecord>(value.clone()) {
        return Some(record);
    }
    let mut map = map.clone();
    for key in RECORD_KEYS {
        let Some(field) = map.remove(*key) else {
            continue;
        };
        let fixed = match *key {
            "views" | "uses" | "patches" | "attached" => as_count(&field).map(Value::from),
            "pinned" => match &field {
                Value::Bool(b) => Some(Value::Bool(*b)),
                Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "yes" | "1" => Some(Value::Bool(true)),
                    "false" | "no" | "0" | "" => Some(Value::Bool(false)),
                    _ => None,
                },
                Value::Number(n) => n.as_f64().map(|n| Value::Bool(n != 0.0)),
                _ => None,
            },
            "state" => field
                .as_str()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| matches!(s.as_str(), "active" | "stale" | "archived"))
                .map(Value::String),
            "clean_rate" => field.as_f64().map(Value::from),
            "outcomes" => field.as_array().map(|entries| {
                Value::Array(
                    entries
                        .iter()
                        .filter_map(|entry| {
                            let mut entry = entry.clone();
                            if let Some(turn) = entry.get("turn").and_then(as_count) {
                                entry["turn"] = Value::from(turn);
                            }
                            serde_json::from_value::<UseEntry>(entry.clone())
                                .ok()
                                .map(|_| entry)
                        })
                        .collect(),
                )
            }),
            // The text fields: a string, a number written as one; anything else is no value.
            _ => match &field {
                Value::String(_) => Some(field.clone()),
                Value::Number(n) => Some(Value::String(n.to_string())),
                _ => None,
            },
        };
        match fixed {
            Some(fixed) => {
                map.insert(key.to_string(), fixed);
            }
            None if !field.is_null() => {
                tracing::warn!("skill usage: dropped an unreadable `{key}`: {field}");
            }
            None => {}
        }
    }
    serde_json::from_value::<SkillRecord>(Value::Object(map)).ok()
}

/// A count as Hermes's `_int_or_zero` reads one: a whole number, a float, or a numeric string.
fn as_count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64),
        _ => None,
    }
}

impl SkillStore {
    /// `.usage.json` as it is. `None` when it exists but couldn't be read (locked by another
    /// process, an antivirus scan...): a writer must then leave it alone, or it would replace
    /// every record with the one it changed.
    fn load_file(&self) -> Option<UsageFile> {
        let mut file = UsageFile::default();
        let path = self.root().join(USAGE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(file),
            Err(error) => {
                tracing::warn!("reading skill usage failed: {error}");
                return None;
            }
        };
        let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        if text.iter().all(u8::is_ascii_whitespace) {
            return Some(file);
        }
        let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(text) else {
            // Not what we wrote: keep a copy before the next save replaces it. One copy only
            // (the first corruption is the one with the user's data in it), so a file that
            // keeps breaking can't fill the disk.
            let copy = path.with_extension("json.corrupt");
            if !copy.exists() {
                let _ = std::fs::write(copy, &bytes);
            }
            return Some(file);
        };
        for (name, value) in map {
            match record_from(&value) {
                Some(record) => {
                    file.records.insert(name, record);
                }
                None => {
                    file.foreign.insert(name, value);
                }
            }
        }
        Some(file)
    }

    pub(crate) fn load_records(&self) -> HashMap<String, SkillRecord> {
        self.load_file().unwrap_or_default().records
    }

    fn save_file(&self, file: &UsageFile) {
        let mut sorted: BTreeMap<&String, Value> = BTreeMap::new();
        for (name, value) in &file.foreign {
            sorted.insert(name, value.clone());
        }
        for (name, record) in &file.records {
            sorted.insert(name, serde_json::to_value(record).unwrap_or(Value::Null));
        }
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
        let Some(mut file) = self.load_file() else {
            return;
        };
        change(file.record_mut(name));
        self.save_file(&file);
    }

    pub fn record(&self, name: &str) -> SkillRecord {
        self.load_records().remove(name).unwrap_or_default()
    }

    /// Records that `name` was used in turn `turn` of `session` and how the turn went (Phase 8).
    ///
    /// `attached`: the local index attached it (counted in `attached`/`last_attached`, not in
    /// `uses`); a `skill_view` load is counted by the view itself. One entry per skill and turn:
    /// a second call for the same turn is ignored, except that a failed turn that was retried
    /// (its entry is `errored`) is replaced by the retry's. A skill that doesn't exist (deleted
    /// since) is ignored. Returns whether an entry was added.
    pub fn record_skill_use(
        &self,
        name: &str,
        session: &str,
        turn: u64,
        outcome: Outcome,
        attached: bool,
    ) -> bool {
        if self.find_any(name).is_none() {
            return false;
        }
        let now = now_string();
        let _guard = self.usage_lock.lock();
        let Some(mut file) = self.load_file() else {
            return false;
        };
        let record = file.record_mut(name);
        let same = |e: &UseEntry| e.session == session && e.turn == turn;
        match record.outcomes.iter().position(same) {
            Some(i) if record.outcomes[i].outcome == Outcome::Errored => {
                record.outcomes.remove(i);
            }
            // An older, rewound turn of that number is `undone`: this is a new one.
            Some(_)
                if record
                    .outcomes
                    .iter()
                    .any(|e| same(e) && e.outcome != Outcome::Undone) =>
            {
                return false;
            }
            _ => {}
        }
        record.outcomes.push(UseEntry {
            session: session.to_string(),
            turn,
            outcome,
            at: Some(now.clone()),
            extra: BTreeMap::new(),
        });
        if record.outcomes.len() > MAX_OUTCOMES {
            let excess = record.outcomes.len() - MAX_OUTCOMES;
            record.outcomes.drain(..excess);
        }
        if attached {
            record.attached += 1;
            record.last_attached = Some(now);
            if record.state == SkillState::Stale {
                record.state = SkillState::Active;
            }
        }
        record.refresh_summary();
        self.save_file(&file);
        true
    }

    /// Makes the outcome of every use in `session`'s turns `turns` at least as bad as `to`
    /// (a correction in the next message, a rewind): an outcome only ever gets worse, and one a
    /// newer version wrote is left alone. Returns how many entries changed.
    pub fn worsen_outcomes(
        &self,
        session: &str,
        turns: std::ops::RangeInclusive<u64>,
        to: &Outcome,
    ) -> usize {
        let Some(target) = to.severity() else {
            return 0;
        };
        let _guard = self.usage_lock.lock();
        let Some(mut file) = self.load_file() else {
            return 0;
        };
        let mut changed = 0;
        for record in file.records.values_mut() {
            let mut touched = false;
            for entry in &mut record.outcomes {
                if entry.session == session
                    && turns.contains(&entry.turn)
                    && entry.outcome.severity().is_some_and(|now| now < target)
                {
                    entry.outcome = to.clone();
                    touched = true;
                    changed += 1;
                }
            }
            if touched {
                record.refresh_summary();
            }
        }
        if changed > 0 {
            self.save_file(&file);
        }
        changed
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
            if record.uses == 0 && record.attached == 0 && anchor > stale_cutoff {
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
    fn outcomes(store: &SkillStore, name: &str) -> Vec<(String, u64, Outcome)> {
        store
            .record(name)
            .outcomes
            .into_iter()
            .map(|e| (e.session, e.turn, e.outcome))
            .collect()
    }

    #[test]
    fn an_old_usage_file_loads_and_gets_outcomes_added() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        // The file as it was before outcomes: no history, no clean_rate.
        std::fs::write(
            store.root().join(USAGE),
            r#"{"a": {"views": 3, "uses": 2, "patches": 1, "last_used": "2026-11-01T00:00:00+00:00",
                "created_by": "agent", "state": "stale", "pinned": true}}"#,
        )
        .unwrap();
        let old = store.record("a");
        assert_eq!((old.views, old.uses, old.patches), (3, 2, 1));
        assert!(old.outcomes.is_empty() && old.clean_rate.is_none());
        assert!(old.pinned && old.curator_managed() && old.state == SkillState::Stale);
        // A first outcome on top of it.
        assert!(store.record_skill_use("a", "s1", 1, Outcome::Clean, false));
        let now = store.record("a");
        assert_eq!(
            (now.uses, now.views),
            (2, 3),
            "history only: no extra use counted"
        );
        assert_eq!(now.clean_rate, Some(1.0));
        assert!(now.pinned);
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        std::fs::write(
            store.root().join(USAGE),
            r#"{"_meta": {"v": 9}, "a": {"uses": 1, "future": {"x": [1, 2]}, "created_by": "agent",
                "outcomes": [{"session": "s0", "turn": 4, "outcome": "reverted", "weight": 7}]},
               "ghost": "not a record"}"#,
        )
        .unwrap();
        // Any change rewrites the file.
        store.set_pinned("a", true);
        assert!(store.record_skill_use("a", "s1", 2, Outcome::Corrected, true));
        let text = std::fs::read_to_string(store.root().join(USAGE)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["_meta"]["v"], json!(9));
        assert_eq!(value["ghost"], json!("not a record"));
        assert_eq!(value["a"]["future"], json!({"x": [1, 2]}));
        let entries = value["a"]["outcomes"].as_array().unwrap();
        assert_eq!(entries[0]["outcome"], "reverted");
        assert_eq!(entries[0]["weight"], 7);
        assert_eq!(entries[1]["outcome"], "corrected");
        // A value this version doesn't know is never made worse or better.
        assert_eq!(store.worsen_outcomes("s0", 4..=4, &Outcome::Undone), 0);
        // The unknown outcome isn't a clean one in the rate.
        assert_eq!(store.record("a").clean_rate, Some(0.0));
    }

    #[test]
    fn a_use_has_one_entry_per_turn_and_a_retry_replaces_an_error() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        assert!(store.record_skill_use("a", "s1", 1, Outcome::Errored, true));
        assert!(
            store.record_skill_use("a", "s1", 1, Outcome::Clean, true),
            "the retry"
        );
        assert!(
            !store.record_skill_use("a", "s1", 1, Outcome::Clean, true),
            "same turn again"
        );
        assert_eq!(
            outcomes(&store, "a"),
            [("s1".to_string(), 1, Outcome::Clean)]
        );
        // A rewound turn gives its number to a new one.
        store.worsen_outcomes("s1", 1..=1, &Outcome::Undone);
        assert!(store.record_skill_use("a", "s1", 1, Outcome::Clean, false));
        assert_eq!(
            outcomes(&store, "a"),
            [
                ("s1".to_string(), 1, Outcome::Undone),
                ("s1".to_string(), 1, Outcome::Clean)
            ]
        );
        // A skill that is gone gets no record.
        assert!(!store.record_skill_use("deleted", "s1", 1, Outcome::Clean, true));
        assert!(!store.load_records().contains_key("deleted"));
    }

    #[test]
    fn outcomes_only_get_worse_and_the_rate_follows() {
        let (_dir, store) = store_with(&["a", "b"], WriteOrigin::Review);
        for (turn, outcome) in [
            (1, Outcome::Clean),
            (2, Outcome::Clean),
            (3, Outcome::Clean),
            (4, Outcome::Errored),
        ] {
            store.record_skill_use("a", "s1", turn, outcome, true);
        }
        store.record_skill_use("b", "s1", 2, Outcome::Clean, true);
        store.record_skill_use("b", "other", 2, Outcome::Clean, true);
        assert_eq!(store.record("a").clean_rate, Some(0.75));
        // The user corrected turn 2 of s1 (not of the other session): every skill used in it.
        assert_eq!(store.worsen_outcomes("s1", 2..=2, &Outcome::Corrected), 2);
        assert_eq!(store.record("a").clean_rate, Some(0.5));
        assert_eq!(store.record("b").clean_rate, Some(0.5));
        // Turn 2 and later undone: a's turns 2 (corrected), 3 (clean) and 4 (errored) get worse.
        assert_eq!(store.worsen_outcomes("s1", 2..=4, &Outcome::Undone), 4);
        // Corrected is milder than undone: nothing changes.
        assert_eq!(store.worsen_outcomes("s1", 1..=4, &Outcome::Corrected), 1);
        let kinds: Vec<Outcome> = outcomes(&store, "a").into_iter().map(|e| e.2).collect();
        assert_eq!(
            kinds,
            [
                Outcome::Corrected,
                Outcome::Undone,
                Outcome::Undone,
                Outcome::Undone
            ]
        );
    }

    #[test]
    fn the_history_is_capped_and_summaries_come_from_what_is_kept() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        for turn in 1..=(MAX_OUTCOMES as u64 + 10) {
            let outcome = if turn <= 10 {
                Outcome::Corrected
            } else {
                Outcome::Clean
            };
            store.record_skill_use("a", "s1", turn, outcome, true);
        }
        let record = store.record("a");
        assert_eq!(record.outcomes.len(), MAX_OUTCOMES);
        assert_eq!(record.outcomes[0].turn, 11, "the oldest went");
        assert_eq!(
            record.attached,
            MAX_OUTCOMES as u64 + 10,
            "attached counts every attachment"
        );
        assert_eq!(record.clean_rate, Some(1.0));
        assert!(record.last_attached.is_some());
        // `uses` keeps its meaning (a whole-skill load): the Star map's "learned" filter
        // (`uses > 0`) doesn't pick up a bundled skill the index only attached.
        assert_eq!((record.uses, record.last_used.as_deref()), (0, None));
    }

    /// Writes `.usage.json` as `bytes`, then makes one change through the store and returns the
    /// file as JSON.
    fn after_a_change(store: &SkillStore, bytes: &[u8]) -> Value {
        std::fs::write(store.root().join(USAGE), bytes).unwrap();
        assert!(store.record_skill_use("a", "s1", 1, Outcome::Clean, true));
        serde_json::from_slice(&std::fs::read(store.root().join(USAGE)).unwrap()).unwrap()
    }

    #[test]
    fn every_older_usage_file_shape_loads_without_losing_records() {
        // M4 part 1 (`skills.rs`'s `Usage`: views, uses, last_used only).
        let (_dir, store) = store_with(&["a", "b"], WriteOrigin::Review);
        let v = after_a_change(
            &store,
            br#"{"a": {"views": 2, "uses": 1, "last_used": null}, "b": {"views": 5, "uses": 4, "last_used": "2026-11-01T00:00:00+00:00"}}"#,
        );
        assert_eq!((v["a"]["views"].clone(), v["a"]["uses"].clone()), (json!(2), json!(1)));
        assert_eq!(v["b"]["uses"], json!(4));
        assert_eq!(v["a"]["attached"], json!(1));

        // A byte-order mark (Notepad) and hand-edited values of the wrong type: numbers as
        // strings or floats, null, a capitalised state, a string bool; every other record kept.
        let (_dir, store) = store_with(&["a", "b"], WriteOrigin::Review);
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(
            br#"{"a": {"views": "3", "uses": 2.0, "patches": null, "pinned": "true", "state": "Stale",
                 "created_by": "agent", "last_used": 5, "outcomes": [{"session": "s0", "turn": "4", "outcome": "clean"}, {"bad": 1}]},
                "b": {"views": 1, "uses": 1, "pinned": true, "created_by": "agent"}}"#,
        );
        let v = after_a_change(&store, &bytes);
        let a = store.record("a");
        assert_eq!((a.views, a.uses, a.patches), (3, 2, 0));
        assert!(a.pinned && a.curator_managed());
        // Using it made it active again; the old entry is kept, the unreadable one dropped.
        assert_eq!(a.state, SkillState::Active);
        assert_eq!(a.outcomes.len(), 2);
        assert_eq!(a.outcomes[0].turn, 4);
        assert_eq!(v["b"]["pinned"], json!(true));
        assert!(!store.root().join(".usage.json.corrupt").exists());

        // A non-record entry stays exactly as it was; an empty file is no corruption.
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        let v = after_a_change(&store, br#"{"_meta": {"v": 9}, "list": [1, 2], "n": null}"#);
        assert_eq!(v["_meta"], json!({"v": 9}));
        assert_eq!((v["list"].clone(), v["n"].clone()), (json!([1, 2]), Value::Null));
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        after_a_change(&store, b"  \n");
        assert!(!store.root().join(".usage.json.corrupt").exists());

        // A skill's entry that isn't a record yet keeps its fields when it becomes one.
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        let v = after_a_change(&store, br#"{"a": {"future_only": 1}}"#);
        assert_eq!((v["a"]["future_only"].clone(), v["a"]["attached"].clone()), (json!(1), json!(1)));
    }

    #[test]
    fn a_big_usage_file_round_trips() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        let mut map = serde_json::Map::new();
        for i in 0..2000 {
            map.insert(format!("skill-{i}"), json!({"views": i, "uses": i, "pinned": i % 2 == 0}));
        }
        let v = after_a_change(&store, &serde_json::to_vec(&Value::Object(map)).unwrap());
        assert_eq!(v.as_object().unwrap().len(), 2001);
        assert_eq!(v["skill-1999"]["uses"], json!(1999));
    }

    #[test]
    #[cfg(windows)]
    fn a_usage_file_that_cannot_be_read_is_left_alone() {
        // A directory where the file should be stands in for a locked file: reading fails with
        // something other than "not found", and no writer may replace it.
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        let _ = std::fs::remove_file(store.root().join(USAGE));
        std::fs::create_dir(store.root().join(USAGE)).unwrap();
        assert!(!store.record_skill_use("a", "s1", 1, Outcome::Clean, true));
        store.set_pinned("a", true);
        assert!(store.root().join(USAGE).is_dir());
    }

    #[test]
    fn an_attached_skill_counts_as_activity_for_the_curator() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        backdate(&store, "a", 20, true);
        assert_eq!(
            store.apply_automatic_transitions(Utc::now()).marked_stale,
            1
        );
        store.record_skill_use("a", "s1", 1, Outcome::Clean, true);
        assert_eq!(store.record("a").state, SkillState::Active);
        assert_eq!(
            store.apply_automatic_transitions(Utc::now()).summary(),
            "no changes"
        );
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_before_it_is_replaced() {
        let (_dir, store) = store_with(&["a"], WriteOrigin::Review);
        std::fs::write(store.root().join(USAGE), "{not json").unwrap();
        store.set_pinned("a", true);
        assert_eq!(
            std::fs::read_to_string(store.root().join(".usage.json.corrupt")).unwrap(),
            "{not json"
        );
        assert!(store.record("a").pinned);
    }
}
