//! Memory tiers and scopes (Backend v2 Phase 9, CP9.2). Not from Hermes (it has the two files
//! only): see `PORTING.md`, `diverged`.
//!
//! - `core`: the entries of `MEMORY.md` / `USER.md`, in every session's system prompt. Unchanged:
//!   `store.rs`, `sync.rs` (the file is the view of `core`, and wins).
//! - `project`: in `memory.db` only, scope = the git root of the session's working folder
//!   (`hermes_tools::project_skills::find_root`, a linked worktree folded into its main checkout:
//!   `memory_scope_key`, passed in by the caller as a scope key; this crate knows no paths). Enters the system prompt of sessions in that repository, when the session
//!   starts (frozen with it), and of no other: [`MemoryStore::snapshot_for`] reads one scope's rows.
//!   Limited in characters per project (`MemoryConfig::project_char_limit`, default 1500).
//! - `archive`: in `memory.db` only, global, not in the system prompt (CP9.3 retrieves it per
//!   turn). No character limit, but an entry is at most [`ARCHIVE_ENTRY_CHARS`] and a target holds
//!   at most [`ARCHIVE_ROWS_MAX`] live entries.
//!
//! The agent's changes in `project` and `archive` go through the same batch planner as `core`
//! (`store.rs`): same matching, same errors, same all-or-nothing rule, same threat scan. Only the
//! storage differs ([`MemoryStore::mutate_tier`]): one transaction under the database's write
//! lock, rows marked removed (never deleted) when the agent removes or rewrites them, a rewritten
//! entry's row pointing at its replacement. The user's removals erase ([`MemoryStore::delete_tier_entry`]):
//! the rows, their older versions and the note files they link to (not a file a surviving row
//! still links). Settings ▸ Memory ▸ reset ([`MemoryStore::reset`]) erases `core` only: its
//! confirmation names MEMORY.md / USER.md.
//!
//! Notes: `note` on an `add` or `patch` in these two tiers stores a long body as
//! `<memories>/notes/<slug>.md` (`notes.rs`) and links it from the entry; the `note_read` action
//! returns it. Core entries have no notes (they are always in the prompt: keep them short).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db;
use crate::notes;
use crate::store::{Leaving, MemoryStore, Mutation, Operation, RowMeta, Target};
use crate::threats::{Scope, first_threat_message, scan_for_threats};
use crate::ENTRY_DELIMITER;

/// Live entries of one target in one project / in the archive.
pub(crate) const PROJECT_ROWS_MAX: usize = 100;
pub(crate) const ARCHIVE_ROWS_MAX: usize = 5000;
/// The longest archive entry (a summary; long text goes in a note).
pub(crate) const ARCHIVE_ENTRY_CHARS: usize = 1000;
const TAGS_MAX: usize = 8;
const TAG_CHARS: usize = 32;
const LINKS_MAX: usize = 8;
const LINK_CHARS: usize = 300;
/// What `memory.project_char_limit` is held to.
pub const PROJECT_LIMIT_MIN: usize = 200;
pub const PROJECT_LIMIT_MAX: usize = 20_000;
pub const PROJECT_LIMIT_DEFAULT: usize = 1500;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tier {
    Core,
    Project,
    Archive,
}

impl Tier {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "core" => Some(Tier::Core),
            "project" => Some(Tier::Project),
            "archive" => Some(Tier::Archive),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Core => "core",
            Tier::Project => "project",
            Tier::Archive => "archive",
        }
    }

    /// How the store is named in messages.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tier::Core => "memory",
            Tier::Project => "project memory",
            Tier::Archive => "archive memory",
        }
    }
}

/// What a memory operation carries beyond `Operation`'s four fields (kept apart so `Operation`
/// stays as it was, and an old-shape call is `Extras::default()` throughout). Parallel to the
/// operations: `extras[i]` belongs to `ops[i]`; missing ones are default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Extras {
    /// `core` (the default), `project` or `archive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The project's scope key. Set by the caller from the session's repository, never from the
    /// model's arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    /// The long-form body of a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Extras {
    pub fn is_empty(&self) -> bool {
        *self == Extras::default()
    }
}

/// Which store a batch writes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    pub tier: Tier,
    pub scope: Option<String>,
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

/// Tags, trimmed and de-duplicated; `Err` is the message.
pub(crate) fn clean_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().count() > TAG_CHARS || has_control(tag) {
            return Err(format!(
                "a tag must be 1-{TAG_CHARS} characters on one line (got '{tag}')."
            ));
        }
        if !tag
            .chars()
            .all(|c| c.is_alphanumeric() || " _.:/#+-".contains(c))
        {
            return Err(format!(
                "a tag may hold letters, digits, spaces and _ . : / # + - only (got '{tag}')."
            ));
        }
        if !out.iter().any(|t| t == tag) {
            out.push(tag.to_string());
        }
    }
    if out.len() > TAGS_MAX {
        return Err(format!("at most {TAGS_MAX} tags."));
    }
    Ok(out)
}

/// Links (`skill:`, `file:`, `session:`, `memory:`), trimmed and de-duplicated; `Err` is the
/// message. A `file:` link is kept as the text it is: nothing here opens it.
pub(crate) fn clean_links(links: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for link in links {
        let link = link.trim();
        let (kind, rest) = link.split_once(':').unwrap_or(("", link));
        let rest = rest.trim();
        let bad = |why: &str| {
            Err(format!(
                "link '{link}' {why}. Links are 'skill:<name>', 'file:<path>', 'session:<id>' or 'memory:<id>'."
            ))
        };
        if link.chars().count() > LINK_CHARS || has_control(link) {
            return bad(&format!("is longer than {LINK_CHARS} characters or has control characters"));
        }
        let ok = match kind {
            "skill" => {
                !rest.is_empty()
                    && rest.len() <= 64
                    && rest.starts_with(|c: char| c.is_ascii_alphanumeric())
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            }
            "session" => {
                !rest.is_empty()
                    && rest.len() <= 64
                    && rest.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            }
            "memory" => !rest.is_empty() && rest.len() <= 12 && rest.chars().all(|c| c.is_ascii_digit()),
            "file" => !rest.is_empty(),
            _ => return bad("has an unknown kind"),
        };
        if !ok {
            return bad("is not valid for its kind");
        }
        if notes::reserved_link(link) {
            return bad("points into the notes folder, which the 'note' field manages");
        }
        let normal = format!("{kind}:{rest}");
        if !out.contains(&normal) {
            out.push(normal);
        }
    }
    if out.len() > LINKS_MAX {
        return Err(format!("at most {LINKS_MAX} links."));
    }
    Ok(out)
}

/// Checks the new fields of a batch and says which store it writes. Old-shape operations (no
/// extras) are always `core` and never fail here. `Err` is the model-facing message.
pub(crate) fn check_extras(ops: &[Operation], extras: &[Extras]) -> Result<Place, String> {
    let empty = Extras::default();
    let mut place: Option<Place> = None;
    for (i, op) in ops.iter().enumerate() {
        let n = i + 1;
        let extra = extras.get(i).unwrap_or(&empty);
        let tier = match extra.tier.as_deref() {
            None => Tier::Core,
            Some(name) => Tier::parse(name).ok_or_else(|| {
                format!(
                    "Operation {n}: invalid tier '{name}'. Use 'core', 'project' or 'archive'."
                )
            })?,
        };
        let here = Place {
            tier,
            scope: (tier == Tier::Project).then(|| extra.scope.clone()).flatten(),
        };
        if tier == Tier::Project && here.scope.is_none() {
            return Err(format!(
                "Operation {n}: tier 'project' needs a session inside a git repository, and this one is not in one. Use tier 'archive' (all projects) or 'core'. Nothing was written or staged."
            ));
        }
        match &place {
            None => place = Some(here),
            Some(first) if *first != here => {
                return Err(format!(
                    "Operation {n} names a different tier than operation 1. One call writes one tier: retry with separate calls for different tiers. Nothing was written or staged."
                ));
            }
            Some(_) => {}
        }
        let writes = matches!(op.action.as_str(), "add" | "patch");
        if let Some(tags) = extra.tags.as_ref().filter(|t| !t.is_empty()) {
            if !writes {
                return Err(format!("Operation {n}: tags go with 'add' or 'patch'."));
            }
            for tag in clean_tags(tags).map_err(|e| format!("Operation {n}: {e}"))? {
                if let Some(threat) = first_threat_message(&tag, Scope::Strict) {
                    return Err(format!("Operation {n}: {threat}"));
                }
            }
        }
        if let Some(links) = extra.links.as_ref().filter(|l| !l.is_empty()) {
            if !writes {
                return Err(format!("Operation {n}: links go with 'add' or 'patch'."));
            }
            for link in clean_links(links).map_err(|e| format!("Operation {n}: {e}"))? {
                if let Some(threat) = first_threat_message(&link, Scope::Strict) {
                    return Err(format!("Operation {n}: {threat}"));
                }
            }
        }
        if let Some(note) = &extra.note {
            if !writes {
                return Err(format!("Operation {n}: a note goes with 'add' or 'patch'."));
            }
            if tier == Tier::Core {
                return Err(format!(
                    "Operation {n}: core entries have no notes (they are in every prompt; keep them short). Use tier 'project' or 'archive' for a note."
                ));
            }
            let body = note.trim();
            if body.is_empty() {
                return Err(format!("Operation {n}: the note is empty."));
            }
            let size = body.chars().count();
            if size > notes::NOTE_MAX_CHARS {
                return Err(format!(
                    "Operation {n}: the note is {size} characters; a note holds at most {}. Split it into two notes (two entries).",
                    notes::NOTE_MAX_CHARS
                ));
            }
            if let Some(threat) = first_threat_message(body, Scope::Strict) {
                return Err(format!("Operation {n}: {threat}"));
            }
        }
    }
    Ok(place.unwrap_or(Place {
        tier: Tier::Core,
        scope: None,
    }))
}

/// A JSON array column from a list.
fn json_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

fn parse_list(column: &str) -> Vec<String> {
    serde_json::from_str(column).unwrap_or_default()
}

/// A row's links after a write: the new note's link (if any), the explicit list (or the row's own
/// non-note links), and the note links the row already had.
fn compose_links(base: &str, explicit: Option<&Vec<String>>, new_note: Option<&str>) -> Vec<String> {
    let base = parse_list(base);
    let (notes_of, others): (Vec<String>, Vec<String>) = base
        .into_iter()
        .partition(|link| notes::slug_of_link(link).is_some());
    let mut out: Vec<String> = match explicit {
        Some(list) => list.clone(),
        None => others,
    };
    // The newest note first (`note_read` reads the first); older ones stay linked, so removing
    // the entry for good deletes every file it ever had.
    if let Some(link) = new_note {
        out.insert(0, link.to_string());
    }
    out.extend(notes_of);
    let mut seen = Vec::new();
    out.retain(|l| {
        let fresh = !seen.contains(l);
        seen.push(l.clone());
        fresh
    });
    out
}

/// What a project entry's block shows for an entry a threat pattern matched.
fn blocked(kind: &str, findings: &[String]) -> String {
    let target = if kind == "user" { "user" } else { "memory" };
    format!(
        "[BLOCKED: project memory entry contained threat pattern(s): {}. Removed from system prompt; use memory(action=remove, tier='project', target='{target}') to delete the original.]",
        findings.join(", ")
    )
}

impl MemoryStore {
    /// The project character limit, held to its bounds.
    pub fn project_limit(&self) -> usize {
        self.config()
            .project_char_limit
            .clamp(PROJECT_LIMIT_MIN, PROJECT_LIMIT_MAX)
    }

    /// The system prompt block of the project `scope`: its live entries (the kinds that are
    /// switched on), `None` when there are none or `memory.db` can't be used. Built when a
    /// session starts and frozen with its prompt.
    pub(crate) fn project_block(&self, scope: &str) -> Option<String> {
        let rows = {
            let mut inner = self.lock_inner();
            let db = inner.db(self.db_path(), false)?;
            db::tier_rows(db.conn(), "project", scope, None).ok()?
        };
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|row| self.enabled(if row.kind == "user" { Target::User } else { Target::Memory }))
            .collect();
        if rows.is_empty() {
            return None;
        }
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        let used = texts.join(ENTRY_DELIMITER).chars().count();
        let limit = self.project_limit();
        let shown: Vec<String> = rows
            .iter()
            .map(|row| {
                let findings = scan_for_threats(&row.text, Scope::Strict);
                if findings.is_empty() {
                    row.text.clone()
                } else {
                    tracing::warn!("project memory entry blocked at load time: {findings:?}");
                    blocked(&row.kind, &findings)
                }
            })
            .collect();
        let separator = "═".repeat(46);
        Some(format!(
            "{separator}\nPROJECT MEMORY (notes for this repository only) [{}% — {used}/{limit} chars]\n{separator}\n{}",
            if limit > 0 { (used * 100 / limit).min(100) } else { 0 },
            shown.join(ENTRY_DELIMITER)
        ))
    }

    /// The live entries of a non-`core` tier for `target` (`scope`: the project's key, ignored
    /// for `archive`). Empty when `memory.db` can't be used.
    pub fn tier_entries(&self, target: Target, tier: Tier, scope: Option<&str>) -> Vec<String> {
        let Some(scope) = Self::scope_of(tier, scope) else {
            return Vec::new();
        };
        let mut inner = self.lock_inner();
        let Some(db) = inner.db(self.db_path(), false) else {
            return Vec::new();
        };
        db::tier_rows(db.conn(), tier.name(), &scope, Some(target.kind()))
            .map(|rows| rows.into_iter().map(|r| r.text).collect())
            .unwrap_or_default()
    }

    /// The column value a tier's rows carry as their scope; `None` for a project without one.
    fn scope_of(tier: Tier, scope: Option<&str>) -> Option<String> {
        match tier {
            Tier::Core => Some(String::new()),
            Tier::Archive => Some(String::new()),
            Tier::Project => scope.map(str::to_string),
        }
    }

    fn unavailable(&self, tier: Tier) -> Value {
        let why = {
            let mut inner = self.lock_inner();
            let _ = inner.db(self.db_path(), false);
            inner.problem()
        };
        json!({"success": false, "error": format!(
            "The {} tier needs memory.db, which is not available right now{}. Nothing was changed; use tier 'core' or retry later.",
            tier.name(),
            why.map(|w| format!(" ({w})")).unwrap_or_default()
        )})
    }

    /// One write batch in `project` or `archive`: `plan` (the shared batch planner) decides from
    /// the live entries of `target` in this store; its result is recorded in one transaction
    /// (note files written first, removed again if the commit fails).
    pub(crate) fn mutate_tier(
        &self,
        target: Target,
        place: &Place,
        mut plan: impl FnMut(&[String], usize) -> Mutation,
    ) -> Value {
        let tier = place.tier;
        let Some(scope) = Self::scope_of(tier, place.scope.as_deref()) else {
            return self.unavailable(tier);
        };
        let kind = target.kind();
        let other_kind = if kind == "user" { "fact" } else { "user" };
        let project_limit = self.project_limit();
        let notes_dir = self.dir().join("notes");
        let mut written: Vec<String> = Vec::new();
        let outcome = {
            let mut inner = self.lock_inner();
            let Some(db) = inner.db(self.db_path(), false) else {
                drop(inner);
                return self.unavailable(tier);
            };
            let tx = match db.begin() {
                Ok(tx) => tx,
                Err(e) => {
                    return json!({"success": false, "error": format!(
                        "memory.db is busy or unavailable ({e}). Nothing was changed — retry in a moment."
                    )});
                }
            };
            let run = (|| -> rusqlite::Result<Result<Value, Value>> {
                let before = db::tier_rows(&tx, tier.name(), &scope, Some(kind))?;
                // The project's limit covers both kinds: what the other kind uses is not ours.
                let reserved = if tier == Tier::Project {
                    let other = db::tier_rows(&tx, "project", &scope, Some(other_kind))?;
                    let chars: usize = other.iter().map(|r| r.text.chars().count()).sum::<usize>()
                        + other.len().saturating_sub(1) * ENTRY_DELIMITER.chars().count();
                    if other.is_empty() { 0 } else { chars + ENTRY_DELIMITER.chars().count() }
                } else {
                    0
                };
                let limit = match tier {
                    Tier::Project => project_limit.saturating_sub(reserved),
                    _ => usize::MAX,
                };
                let texts: Vec<String> = before.iter().map(|r| r.text.clone()).collect();
                let (entries, message, extra, leaving, metas) = match plan(&texts, limit) {
                    Mutation::Reply(mut value) => {
                        if let Value::Object(map) = &mut value {
                            map.remove("over_budget");
                        }
                        return Ok(Err(value));
                    }
                    Mutation::Write(entries, message, extra, leaving, metas) => {
                        (entries, message, extra, leaving, metas)
                    }
                };
                let mut extra = extra;
                let message = crate::store::decorate(message, &mut extra, &|text| {
                    before.iter().find(|r| r.text == text).map(|r| r.id)
                });
                let supersedes: Vec<(String, String)> = match leaving {
                    Leaving::Mark(pairs) => pairs,
                    Leaving::Purge => Vec::new(),
                };
                let now = chrono::Utc::now().timestamp();
                let mut next: Vec<String> = Vec::new();
                for text in entries {
                    if !next.contains(&text) {
                        next.push(text);
                    }
                }
                let mut free: HashMap<&str, Vec<&db::TierRow>> = HashMap::new();
                for row in before.iter().rev() {
                    free.entry(row.text.as_str()).or_default().push(row);
                }
                let mut ids: Vec<i64> = Vec::new();
                let mut kept: Vec<i64> = Vec::new();
                let mut note_lines: Vec<Value> = Vec::new();
                for (position, text) in next.iter().enumerate() {
                    let existing = free.get_mut(text.as_str()).and_then(Vec::pop);
                    let predecessor = existing.is_none().then(|| {
                        supersedes
                            .iter()
                            .find(|(_, new)| new == text)
                            .and_then(|(old, _)| before.iter().find(|r| &r.text == old))
                    });
                    let base = existing.or(predecessor.flatten());
                    let meta: Option<&RowMeta> = metas.iter().find(|m| &m.text == text);
                    let tags = match meta.and_then(|m| m.tags.as_ref()) {
                        Some(list) => json_list(list),
                        None => base.map_or_else(|| "[]".to_string(), |r| r.tags.clone()),
                    };
                    let mut new_note = None;
                    if let Some(body) = meta.and_then(|m| m.note.as_deref()) {
                        let slug = notes::write_new(&notes_dir, &notes::slug_for(text), body)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        written.push(slug.clone());
                        note_lines.push(json!({"entry": text, "file": notes::link_for(&slug)}));
                        new_note = Some(notes::link_for(&slug));
                    }
                    let links = json_list(&compose_links(
                        base.map_or("[]", |r| r.links.as_str()),
                        meta.and_then(|m| m.links.as_ref()),
                        new_note.as_deref(),
                    ));
                    let id = match existing {
                        Some(row) => {
                            db::set_position(&tx, row.id, position)?;
                            if tags != row.tags || links != row.links {
                                db::set_row_meta(&tx, row.id, &tags, &links)?;
                            }
                            if meta.is_some_and(|m| m.confirm) {
                                db::confirm_row(&tx, row.id, now)?;
                            }
                            kept.push(row.id);
                            row.id
                        }
                        None => db::insert_tier_row(
                            &tx,
                            tier.name(),
                            &scope,
                            kind,
                            text,
                            &tags,
                            &links,
                            base.map_or(now, |r| r.created),
                            base.map_or(0, |r| r.hits),
                            now,
                            position,
                        )?,
                    };
                    ids.push(id);
                }
                for row in &before {
                    if kept.contains(&row.id) {
                        continue;
                    }
                    let successor = supersedes
                        .iter()
                        .find(|(old, _)| old == &row.text)
                        .and_then(|(_, new)| next.iter().position(|t| t == new))
                        .map(|index| ids[index]);
                    db::mark_removed(&tx, row.id, now, "store", successor)?;
                }
                db::bump_generation(&tx)?;
                let own: usize = next.join(ENTRY_DELIMITER).chars().count();
                let usage = match tier {
                    Tier::Project => {
                        let total = own + reserved;
                        format!(
                            "{}% — {total}/{project_limit} chars",
                            (total * 100 / project_limit).min(100)
                        )
                    }
                    _ => format!("{own} chars, {} entries (no limit)", next.len()),
                };
                let mut response = json!({
                    "success": true,
                    "done": true,
                    "target": target.name(),
                    "tier": tier.name(),
                    "usage": usage,
                    "entry_count": next.len(),
                    "message": message,
                });
                if let Value::Object(extra) = extra {
                    for (key, value) in extra {
                        response[key] = value;
                    }
                }
                if !note_lines.is_empty() {
                    response["notes"] = json!(note_lines);
                }
                response["note"] =
                    json!("Write saved. This update is complete — do not repeat it.");
                Ok(Ok(response))
            })();
            match run {
                Ok(Ok(response)) => match tx.commit() {
                    Ok(()) => Ok(response),
                    Err(e) => Err(format!("memory.db could not save the write ({e})")),
                },
                Ok(Err(reply)) => {
                    // A reply: nothing to record (the transaction rolls back on drop).
                    drop(tx);
                    return reply;
                }
                Err(e) => Err(format!("memory.db: {e}")),
            }
        };
        match outcome {
            Ok(response) => {
                self.changed();
                response
            }
            Err(problem) => {
                for slug in &written {
                    notes::delete(&notes_dir, slug);
                }
                json!({"success": false, "error": format!(
                    "Failed to write {}: {problem}. Nothing was changed — retry in a moment.",
                    tier.label()
                )})
            }
        }
    }

    /// `note_read`: the long-form body of the entry `old_text` selects in `tier` (`project` needs
    /// the session's `scope`).
    pub fn read_note(
        &self,
        target: Target,
        tier: Tier,
        scope: Option<&str>,
        old_text: &str,
    ) -> Value {
        let err = |message: String| json!({"success": false, "error": message});
        if tier == Tier::Core {
            return err("core entries have no notes; use tier 'project' or 'archive'.".into());
        }
        let Some(scope_key) = Self::scope_of(tier, scope) else {
            return err("tier 'project' needs a session inside a git repository, and this one is not in one.".into());
        };
        let old_text = old_text.trim();
        if old_text.is_empty() {
            return err("note_read needs old_text -- a short unique substring of the entry whose note to read.".into());
        }
        let rows = {
            let mut inner = self.lock_inner();
            let Some(db) = inner.db(self.db_path(), false) else {
                drop(inner);
                return self.unavailable(tier);
            };
            match db::tier_rows(db.conn(), tier.name(), &scope_key, Some(target.kind())) {
                Ok(rows) => rows,
                Err(e) => return err(format!("memory.db could not be read ({e}). Retry in a moment.")),
            }
        };
        let texts: Vec<String> = rows.iter().map(|r| r.text.clone()).collect();
        let index = match crate::store::find_unique_match(&texts, old_text) {
            (_, true) => {
                return err(format!("'{old_text}' matched multiple distinct entries -- be more specific."));
            }
            (None, false) => {
                return err(format!(
                    "No {} entry matched '{old_text}'. Retry with old_text copied verbatim from the intended entry.",
                    tier.name()
                ));
            }
            (Some(index), false) => index,
        };
        let row = &rows[index];
        let Some(slug) = db::note_slugs(&row.links).into_iter().next() else {
            return err("That entry has no note.".into());
        };
        match notes::read(&self.dir().join("notes"), &slug) {
            Ok(body) => json!({
                "success": true,
                "tier": tier.name(),
                "entry": row.text,
                "file": notes::link_for(&slug),
                "note": body,
            }),
            Err(why) => err(format!("The entry's note could not be read: {why}.")),
        }
    }

    /// The user removes one entry of a non-`core` tier (the memory screen; the Star map reads these
    /// from CP9.3): the row, its older versions and the note files they link to are erased.
    /// Whether an entry was found.
    pub fn delete_tier_entry(
        &self,
        target: Target,
        tier: Tier,
        scope: Option<&str>,
        entry: &str,
    ) -> Result<bool, String> {
        if tier == Tier::Core {
            return Err("core entries are removed with edit_entry".into());
        }
        let Some(scope_key) = Self::scope_of(tier, scope) else {
            return Err("a project entry needs its project".into());
        };
        let entry = entry.trim();
        let failed = |e: rusqlite::Error| format!("Could not remove the entry: memory.db: {e}");
        let slugs = {
            let mut inner = self.lock_inner();
            let Some(db) = inner.db(self.db_path(), false) else {
                return Err(format!("memory.db is not available, so the {} tier is not either", tier.name()));
            };
            let tx = db.begin().map_err(failed)?;
            let rows = db::tier_rows(&tx, tier.name(), &scope_key, Some(target.kind())).map_err(failed)?;
            let Some(row) = rows.iter().find(|r| r.text.trim() == entry) else {
                return Ok(false);
            };
            let slugs = db::purge_entry(&tx, target.kind(), row.id).map_err(failed)?;
            db::bump_generation(&tx).map_err(failed)?;
            tx.commit().map_err(failed)?;
            db.checkpoint();
            slugs
        };
        let dir = self.dir().join("notes");
        for slug in &slugs {
            notes::delete(&dir, slug);
        }
        self.changed();
        Ok(true)
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;

    fn tags(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tags_are_trimmed_unique_and_bounded() {
        assert_eq!(clean_tags(&tags(&[" a ", "a", "b c", "x/y:z"])).unwrap(), tags(&["a", "b c", "x/y:z"]));
        assert!(clean_tags(&tags(&[""])).is_err());
        assert!(clean_tags(&tags(&["bad\nline"])).is_err());
        assert!(clean_tags(&tags(&["semi;colon"])).is_err());
        assert!(clean_tags(&tags(&[&"x".repeat(33)])).is_err());
        let many: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        assert!(clean_tags(&many).is_err());
    }

    #[test]
    fn links_have_a_known_kind_and_a_sane_target() {
        let ok = clean_links(&tags(&[
            "skill:git-commit",
            "file:C:/work/notes.md",
            "session:abc_123",
            "memory:42",
            " skill:git-commit ",
        ]))
        .unwrap();
        assert_eq!(ok.len(), 4, "{ok:?}");
        for bad in [
            "http://example.com",
            "skill:",
            "skill:has space",
            "skill:../x",
            "memory:abc",
            "memory:1234567890123",
            "session:a/b",
            "file:",
            "nothing",
            "file:notes/other-projects-note.md",
            "file:Notes\\x.md",
            "skill:a\nb",
        ] {
            assert!(clean_links(&tags(&[bad])).is_err(), "{bad}");
        }
        assert!(clean_links(&tags(&[&format!("file:{}", "x".repeat(300))])).is_err());
        let many: Vec<String> = (0..9).map(|i| format!("memory:{i}")).collect();
        assert!(clean_links(&many).is_err());
    }
}
