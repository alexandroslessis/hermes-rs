//! The Star map (`/memory-graph`, `/learning`): what the agent has learned, as a graph. Learned
//! skills (made by the agent, or used) and every MEMORY.md / USER.md entry are nodes; skills link
//! through their `related_skills`, memories to the skills they mention. A node opens its content,
//! which the user can edit or delete (a skill is archived, restorable; a memory entry is removed
//! under the memory store's lock).
//!
//! Typed [`Graph`] and node calls with [`LearningError`]; the page's JSON (`StarmapGraph`, the
//! `{ok, message, detail}` answers) is `crates/desktop/src/learning.rs`.
//!
//! Hermes: `agent/learning_graph.py` (`build_learning_graph`), `agent/learning_mutations.py`
//! (`node_detail`, `edit_node`, `delete_node`), served as `/api/learning/*` by
//! `hermes_cli/web_routers/status.py`.
//!
//! **Links (Backend v2 Phase 9, CP9.3).** A memory entry may link to skills, files, sessions and
//! other memories (`memory.db`'s `links`). The page's graph knows nodes and `{source, target}`
//! edges between nodes (edges to a missing node are ignored by the page), so:
//! - a MEMORY.md / USER.md card's `skill:<name>` link to a learned skill, and its `memory:<id>`
//!   link to another card, are **real edges** (added to `edges`, next to the old name-overlap
//!   ones, without repeating a pair);
//! - everything else is **additive and ignored by the page**: top-level `archive` (the newest
//!   archive entries; the page has no node kind for them, and cannot edit them) and
//!   `memory_links` (every link of a core card or an archive entry, with whether its end still
//!   exists: `resolved` true/false, `null` for a file or a session, which are not looked up).
//!   A link whose end is gone is listed as unresolved and drawn nowhere: it never breaks the
//!   answer.
//! - **Project memory is not in the map.** The map has no working folder, and a repository's
//!   notes must not show up in another's view: only the user's own entries (core) and the global
//!   archive appear.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use hermes_memory::{CoreRow, MemoryStore, Target};
use hermes_tools::SkillStore;
use sha2::{Digest, Sha256};

/// `(source, file target)`: MEMORY.md cards first, then USER.md (Hermes's order).
const MEMORY_SOURCES: [(&str, Target); 2] = [("memory", Target::Memory), ("profile", Target::User)];

/// `memory_fingerprint`: the first 12 hex digits of the entry's SHA-256, so a node names its
/// entry by what it says, not where it sat.
pub fn fingerprint(entry: &str) -> String {
    let digest = Sha256::digest(entry.trim().as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Seconds since the epoch of an RFC 3339 time (the usage records' format).
fn epoch(time: Option<&str>) -> Option<i64> {
    time.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp())
}

fn modified(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since.as_secs() as i64)
}

/// One memory card (`_memory_cards`).
struct Card {
    source: &'static str,
    timestamp: Option<i64>,
    title: String,
    body: String,
    entry: String,
    fingerprint: String,
}

fn memory_cards(memory: &MemoryStore) -> Vec<Card> {
    let mut cards = Vec::new();
    for (source, target) in MEMORY_SOURCES {
        let Some(file_time) = modified(&memory.path(target)) else {
            continue;
        };
        for (index, entry) in memory.entries(target).into_iter().enumerate() {
            let first = entry.lines().next().unwrap_or_default().trim();
            let first = first.trim_start_matches(['#', ' ']).trim();
            let title = if first.chars().count() > 80 {
                format!("{}…", first.chars().take(80).collect::<String>())
            } else {
                first.to_string()
            };
            cards.push(Card {
                source,
                timestamp: Some(file_time + index as i64),
                title,
                body: entry.chars().take(1200).collect(),
                fingerprint: fingerprint(&entry),
                entry,
            });
        }
    }
    cards
}

fn card_id(card: &Card, index: usize) -> String {
    format!("memory:{}:{index}:{}", card.source, card.fingerprint)
}

/// A learned skill (`SkillNode`).
struct Skill {
    name: String,
    category: String,
    timestamp: Option<i64>,
    use_count: u64,
    state: String,
    created_by: Option<String>,
    pinned: bool,
    related: Vec<String>,
}

/// The skills with a learning signal: made by the agent (`agent`, `learn`) or used; never one
/// from an external folder (configured, not learned).
fn learned_skills(skills: &SkillStore) -> Vec<Skill> {
    skills
        .catalog_all()
        .into_iter()
        .filter(|entry| !entry.external)
        .filter_map(|entry| {
            let record = skills.record(&entry.name);
            let learned =
                matches!(record.created_by.as_deref(), Some("agent" | "learn")) || record.uses > 0;
            if !learned {
                return None;
            }
            let text = std::fs::read_to_string(entry.skill_md()).unwrap_or_default();
            let timestamp = [
                &record.last_used,
                &record.last_viewed,
                &record.last_patched,
                &record.created_at,
            ]
            .into_iter()
            .find_map(|t| epoch(t.as_deref()))
            .or_else(|| modified(&entry.skill_md()));
            Some(Skill {
                category: entry.category.clone().unwrap_or_else(|| "general".into()),
                timestamp,
                use_count: record.uses,
                state: serde_json::to_value(&record.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "active".into()),
                created_by: record.created_by.clone(),
                pinned: record.pinned,
                related: hermes_tools::skills::skill_md_list(&text, "related_skills"),
                name: entry.name,
            })
        })
        .collect()
}

/// `related_skills` links whose both ends are learned skills, undirected, first seen first.
fn skill_edges(skills: &[Skill]) -> Vec<(String, String)> {
    let names: HashSet<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for skill in skills {
        for target in &skill.related {
            if target != &skill.name && names.contains(target.as_str()) {
                let pair = if skill.name <= *target {
                    (skill.name.clone(), target.clone())
                } else {
                    (target.clone(), skill.name.clone())
                };
                if seen.insert(pair.clone()) {
                    edges.push(pair);
                }
            }
        }
    }
    edges
}

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(str::to_string)
        .collect()
}

/// Each memory card to its 4 most overlapping skills (a name mentioned weighs 6).
fn memory_edges(cards: &[Card], skills: &[Skill]) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let text = format!("{}\n{}", card.title, card.body).to_lowercase();
        let text_tokens = tokens(&text);
        let mut scored: Vec<(usize, &str)> = skills
            .iter()
            .filter_map(|skill| {
                let name = skill.name.to_lowercase();
                let score = if text.contains(&name) { 6 } else { 0 }
                    + tokens(&skill.name).intersection(&text_tokens).count();
                (score > 0).then_some((score, skill.name.as_str()))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        edges.extend(
            scored
                .into_iter()
                .take(4)
                .map(|(_, name)| (card_id(card, index), name.to_string())),
        );
    }
    edges
}

/// The `memory.db` row behind each card (by file and text), if the database knows it.
fn card_rows(memory: &MemoryStore, cards: &[Card]) -> Vec<Option<CoreRow>> {
    let mut by_source: Vec<(&str, Vec<CoreRow>)> = Vec::new();
    for (source, target) in MEMORY_SOURCES {
        by_source.push((source, memory.core_rows(target)));
    }
    cards
        .iter()
        .map(|card| {
            by_source
                .iter()
                .find(|(source, _)| *source == card.source)
                .and_then(|(_, rows)| rows.iter().find(|r| r.text.trim() == card.entry.trim()))
                .cloned()
        })
        .collect()
}

/// The explicit link edges of the cards (see the module doc): to learned skills and to other
/// cards. Pairs already in `have` (either direction for card-card) are not repeated.
fn link_edges(
    cards: &[Card],
    rows: &[Option<CoreRow>],
    skills: &[Skill],
    have: &[(String, String)],
) -> Vec<(String, String)> {
    let names: HashSet<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let by_row: Vec<(i64, String)> = cards
        .iter()
        .enumerate()
        .filter_map(|(i, card)| rows[i].as_ref().map(|r| (r.id, card_id(card, i))))
        .collect();
    let mut seen: HashSet<(String, String)> = have.iter().cloned().collect();
    let mut out = Vec::new();
    for (i, card) in cards.iter().enumerate() {
        let Some(row) = &rows[i] else { continue };
        let this = card_id(card, i);
        for link in &row.links {
            let target = match link.split_once(':') {
                Some(("skill", name)) if names.contains(name) => name.to_string(),
                Some(("memory", id)) => match id
                    .parse::<i64>()
                    .ok()
                    .and_then(|id| by_row.iter().find(|(row_id, _)| *row_id == id))
                {
                    Some((_, other)) if *other != this => other.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            let pair = (this.clone(), target.clone());
            let reverse = (target, this.clone());
            if !seen.contains(&pair) && !seen.contains(&reverse) {
                seen.insert(pair.clone());
                out.push(pair);
            }
        }
    }
    out
}

/// How many links the additive `memory_links` list holds at most.
const MAX_LINK_ROWS: usize = 500;
/// How many archive entries the additive `archive` list holds at most.
const MAX_ARCHIVE_ROWS: usize = 100;

/// A link of a card or an archive entry, and whether its end still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLink {
    pub source: String,
    pub kind: String,
    pub target: String,
    /// `None` for a file or a session, which are not looked up.
    pub resolved: Option<bool>,
}

/// Every link of the cards and the newest archive entries, with whether its end still exists.
fn link_rows(
    skills: &SkillStore,
    memory: &MemoryStore,
    cards: &[Card],
    rows: &[Option<CoreRow>],
    archive: &[hermes_memory::ArchiveRow],
) -> Vec<MemoryLink> {
    let core_ids: HashSet<i64> = rows.iter().flatten().map(|r| r.id).collect();
    let mut out = Vec::new();
    let mut push = |source: String, links: &[String]| {
        for link in links {
            let (kind, rest) = link.split_once(':').unwrap_or(("", link));
            let resolved = match kind {
                "skill" => Some(skills.find_any(rest).is_some()),
                "memory" => Some(
                    rest.parse::<i64>()
                        .is_ok_and(|id| core_ids.contains(&id) || memory.live_tier_row(id)),
                ),
                // Not looked up: a path or a session may be anywhere.
                _ => None,
            };
            out.push(MemoryLink {
                source: source.clone(),
                kind: kind.to_string(),
                target: link.clone(),
                resolved,
            });
        }
    };
    for (i, card) in cards.iter().enumerate() {
        if let Some(row) = &rows[i] {
            push(card_id(card, i), &row.links);
        }
    }
    for row in archive {
        push(format!("archive:{}", row.id), &row.links);
    }
    out.truncate(MAX_LINK_ROWS);
    out
}

/// A node of the map: a learned skill or a memory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The skill's name, or `memory:<source>:<index>:<fingerprint>`.
    pub id: String,
    pub label: String,
    /// `"skill"` or `"memory"`.
    pub kind: &'static str,
    /// `"memory"` or `"profile"` for a memory node.
    pub memory_source: Option<&'static str>,
    pub timestamp: Option<i64>,
    pub category: String,
    pub use_count: u64,
    pub state: String,
    pub created_by: Option<String>,
    pub pinned: bool,
}

/// A memory card, as the page lists it beside the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCard {
    pub source: &'static str,
    pub timestamp: Option<i64>,
    pub title: String,
    pub body: String,
    pub fingerprint: String,
}

/// The graph's numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub memory_link_edges: usize,
    pub archive_entries: usize,
    pub nodes: usize,
    pub related_edges: usize,
    pub edges_per_node: f64,
    pub linked_nodes: usize,
    pub isolated_pct: f64,
    pub categories: usize,
    pub agent_created: usize,
    pub used: usize,
    pub top_categories: Vec<(String, usize)>,
    pub memory_nodes: usize,
    pub memory_skill_edges: usize,
    pub learned_skills: usize,
}

/// What the agent has learned, as a graph (`StarmapGraph`).
#[derive(Debug, Clone)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// `(source, target)`: `related_skills` links, then memory-to-skill and memory-to-memory ones.
    pub edges: Vec<(String, String)>,
    pub clusters: Vec<(String, usize)>,
    pub memory: Vec<MemoryCard>,
    /// The newest entries of the global archive (the page has no node kind for them).
    pub archive: Vec<hermes_memory::ArchiveRow>,
    pub memory_links: Vec<MemoryLink>,
    pub stats: Stats,
}

/// The learning graph (`build_learning_graph`).
pub fn graph(skills: &SkillStore, memory: &MemoryStore) -> Graph {
    let learned = learned_skills(skills);
    let cards = memory_cards(memory);
    let links = skill_edges(&learned);
    let mut mentions = memory_edges(&cards, &learned);
    let rows = card_rows(memory, &cards);
    let explicit = link_edges(&cards, &rows, &learned, &mentions);
    mentions.extend(explicit.iter().cloned());
    let archive = memory.archive_rows(MAX_ARCHIVE_ROWS);

    let mut clusters: Vec<(String, usize)> = Vec::new();
    for skill in &learned {
        match clusters.iter_mut().find(|(c, _)| *c == skill.category) {
            Some(cluster) => cluster.1 += 1,
            None => clusters.push((skill.category.clone(), 1)),
        }
    }
    if !cards.is_empty() {
        clusters.push(("memory".into(), cards.len()));
    }
    clusters.sort_by_key(|c| std::cmp::Reverse(c.1));

    let mut nodes: Vec<Node> = learned
        .iter()
        .map(|s| Node {
            id: s.name.clone(),
            label: s.name.clone(),
            kind: "skill",
            memory_source: None,
            timestamp: s.timestamp,
            category: s.category.clone(),
            use_count: s.use_count,
            state: s.state.clone(),
            created_by: s.created_by.clone(),
            pinned: s.pinned,
        })
        .collect();
    nodes.extend(cards.iter().enumerate().map(|(i, card)| Node {
        id: card_id(card, i),
        label: card.title.clone(),
        kind: "memory",
        memory_source: Some(card.source),
        timestamp: card.timestamp,
        category: "memory".into(),
        use_count: 0,
        state: "active".into(),
        created_by: Some("memory".into()),
        pinned: false,
    }));

    // `density_stats`.
    let linked: BTreeSet<&str> = links
        .iter()
        .flat_map(|(a, b)| [a.as_str(), b.as_str()])
        .collect();
    let total = learned.len().max(1) as f64;
    let mut categories: Vec<(String, usize)> = Vec::new();
    for skill in &learned {
        match categories.iter_mut().find(|(c, _)| *c == skill.category) {
            Some(entry) => entry.1 += 1,
            None => categories.push((skill.category.clone(), 1)),
        }
    }
    let category_count = categories.len();
    categories.sort_by_key(|c| std::cmp::Reverse(c.1));
    let round = |value: f64, places: i32| (value * 10f64.powi(places)).round() / 10f64.powi(places);
    let stats = Stats {
        memory_link_edges: explicit.len(),
        archive_entries: archive.len(),
        nodes: learned.len(),
        related_edges: links.len(),
        edges_per_node: round(links.len() as f64 / total, 3),
        linked_nodes: linked.len(),
        isolated_pct: round(100.0 * (total - linked.len() as f64) / total, 1),
        categories: category_count,
        agent_created: learned
            .iter()
            .filter(|s| s.created_by.as_deref() == Some("agent"))
            .count(),
        used: learned.iter().filter(|s| s.use_count > 0).count(),
        top_categories: categories.iter().take(8).cloned().collect(),
        memory_nodes: cards.len(),
        memory_skill_edges: mentions.len(),
        learned_skills: learned.len(),
    };
    let memory_links = link_rows(skills, memory, &cards, &rows, &archive);
    Graph {
        nodes,
        edges: links.iter().chain(&mentions).cloned().collect(),
        clusters,
        memory: cards
            .iter()
            .map(|card| MemoryCard {
                source: card.source,
                timestamp: card.timestamp,
                title: card.title.clone(),
                body: card.body.clone(),
                fingerprint: card.fingerprint.clone(),
            })
            .collect(),
        archive,
        memory_links,
        stats,
    }
}

/// A memory node id → its file and the entry's text (`_locate_memory`): by fingerprint when the
/// id has one (so a list that shifted still names the clicked entry), else by position.
fn locate_memory(memory: &MemoryStore, id: &str) -> Result<(Target, String), String> {
    let parts: Vec<&str> = id.split(':').collect();
    let bad = || format!("bad memory node id: {id:?}");
    if !(parts.len() == 3 || parts.len() == 4) || parts[0] != "memory" {
        return Err(bad());
    }
    let (source, target) = MEMORY_SOURCES
        .into_iter()
        .find(|(source, _)| *source == parts[1])
        .ok_or_else(bad)?;
    let index: usize = parts[2].parse().map_err(|_| bad())?;
    let entries = memory.entries(target);
    if let Some(print) = parts.get(3) {
        return entries
            .into_iter()
            .find(|entry| fingerprint(entry) == *print)
            .map(|entry| (target, entry))
            .ok_or_else(|| "memory node id is stale — refresh the graph".to_string());
    }
    // An id from before fingerprints: its place in the combined list.
    let cards = memory_cards(memory);
    let card = cards
        .get(index)
        .ok_or_else(|| format!("memory index {index} out of range"))?;
    if card.source != source {
        return Err("memory node id is stale — refresh the graph".into());
    }
    Ok((target, card.entry.clone()))
}

/// Why a node call failed. The gateway answers 404 or 400 with `{ok: false, message, detail}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LearningError {
    /// No such node (a read of an unknown or stale id).
    NotFound(String),
    /// The change is refused (a stale id for a write, a pinned skill, a write the store refused).
    Rejected(String),
}

impl LearningError {
    pub fn message(&self) -> &str {
        match self {
            LearningError::NotFound(m) | LearningError::Rejected(m) => m,
        }
    }
}

impl std::fmt::Display for LearningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for LearningError {}

/// A node's content, for the edit form (`node_detail`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDetail {
    /// `"memory"` or `"skill"`.
    pub kind: &'static str,
    pub id: String,
    pub label: String,
    pub content: String,
}

/// A node's content, for the edit form (`node_detail`).
pub fn detail(
    skills: &SkillStore,
    memory: &MemoryStore,
    id: &str,
) -> Result<NodeDetail, LearningError> {
    if id.starts_with("memory:") {
        let (_, entry) = locate_memory(memory, id).map_err(LearningError::NotFound)?;
        return Ok(NodeDetail {
            kind: "memory",
            id: id.to_string(),
            label: entry
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect::<String>(),
            content: entry.trim().to_string(),
        });
    }
    let Some(skill) = skills.find_any(id) else {
        return Err(LearningError::NotFound(format!("skill '{id}' not found")));
    };
    match std::fs::read_to_string(skill.skill_md()) {
        Ok(content) => Ok(NodeDetail {
            kind: "skill",
            id: id.to_string(),
            label: id.to_string(),
            content: content.trim_start_matches('\u{feff}').to_string(),
        }),
        Err(_) => Err(LearningError::NotFound(format!(
            "SKILL.md missing for '{id}'"
        ))),
    }
}

/// The message of a memory write's result, or why it failed.
fn memory_outcome(result: serde_json::Value) -> Result<String, LearningError> {
    if result["success"] == true {
        Ok(result["message"].as_str().unwrap_or_default().to_string())
    } else {
        Err(LearningError::Rejected(
            result["error"]
                .as_str()
                .unwrap_or("memory write failed")
                .to_string(),
        ))
    }
}

/// A skill is archived (restorable), a memory entry removed. Returns the message to show.
pub fn delete(
    skills: &SkillStore,
    memory: &MemoryStore,
    id: &str,
) -> Result<String, LearningError> {
    if id.starts_with("memory:") {
        let (target, entry) = locate_memory(memory, id).map_err(LearningError::Rejected)?;
        return memory_outcome(memory.edit_entry(target, &entry, None));
    }
    if skills.record(id).pinned {
        return Err(LearningError::Rejected(format!(
            "'{id}' is pinned — unpin it first"
        )));
    }
    match skills.archive_by_user(id) {
        Ok(()) => Ok(format!(
            "archived '{id}' — it can be restored from the skills archive"
        )),
        Err(message) => Err(LearningError::Rejected(message)),
    }
}

/// A skill's SKILL.md or a memory entry rewritten. Returns the message to show.
pub fn edit(
    skills: &SkillStore,
    memory: &MemoryStore,
    id: &str,
    content: &str,
) -> Result<String, LearningError> {
    if id.starts_with("memory:") {
        let located = locate_memory(memory, id);
        if content.trim().is_empty() {
            return Err(LearningError::Rejected(
                "empty memory — use delete to remove it".into(),
            ));
        }
        let (target, entry) = located.map_err(LearningError::Rejected)?;
        return memory_outcome(memory.edit_entry(target, &entry, Some(content)));
    }
    let result = skills.user_write(hermes_tools::SkillOp {
        action: "edit".into(),
        name: id.to_string(),
        content: Some(content.to_string()),
        ..Default::default()
    });
    if result["success"] == true {
        Ok(format!("updated '{id}'"))
    } else {
        Err(LearningError::Rejected(
            result["error"]
                .as_str()
                .unwrap_or("edit failed")
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (hermes_testdir::TestDir, SkillStore, MemoryStore) {
        let dir = hermes_testdir::tempdir().unwrap();
        let skills = SkillStore::new(dir.path().join("skills"), Default::default());
        let memory = MemoryStore::new(dir.path().join("memories"), Default::default());
        (dir, skills, memory)
    }

    fn remember(memory: &MemoryStore, target: Target, entries: &[&str]) {
        let path = memory.path(target);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, entries.join(hermes_memory::ENTRY_DELIMITER)).unwrap();
    }

    #[test]
    fn fingerprints_match_hermes() {
        // Python: hashlib.sha256("The user likes Rust.".strip().encode()).hexdigest()[:12]
        assert_eq!(fingerprint("The user likes Rust."), "19a1881b8db2");
        assert_eq!(fingerprint("  The user likes Rust.\n"), "19a1881b8db2");
    }

    #[test]
    fn memories_are_cards_and_nodes_and_edit_by_their_text() {
        let (_dir, skills, memory) = stores();
        remember(
            &memory,
            Target::Memory,
            &[
                "# Build\nRun cargo build in the repo root.",
                "Ports: 47731 for the app.",
            ],
        );
        remember(&memory, Target::User, &["The user prefers short answers."]);
        let graph = graph(&skills, &memory);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[0].label, "Build");
        assert_eq!(graph.nodes[2].memory_source, Some("profile"));
        assert_eq!(graph.clusters, [("memory".to_string(), 3)]);
        let second = graph.nodes[1].id.clone();
        assert!(second.starts_with("memory:memory:1:"));

        let shown = detail(&skills, &memory, &second).unwrap();
        assert_eq!(shown.content, "Ports: 47731 for the app.");

        // An entry removed before this one shifts it; its id still names it.
        remember(&memory, Target::Memory, &["Ports: 47731 for the app."]);
        edit(
            &skills,
            &memory,
            &second,
            "Ports: 47731 (app), 47810 (perf).",
        )
        .unwrap();
        assert_eq!(
            memory.entries(Target::Memory),
            ["Ports: 47731 (app), 47810 (perf)."]
        );
        // The old text is gone: its id is stale now.
        assert!(matches!(
            edit(&skills, &memory, &second, "x"),
            Err(LearningError::Rejected(_))
        ));
        assert!(matches!(
            edit(&skills, &memory, &second, "  "),
            Err(LearningError::Rejected(_))
        ));

        let user = graph.nodes[2].id.clone();
        delete(&skills, &memory, &user).unwrap();
        assert!(memory.entries(Target::User).is_empty());
        assert!(matches!(
            detail(&skills, &memory, "memory:nope:0"),
            Err(LearningError::NotFound(_))
        ));
    }

    #[test]
    fn a_replacement_may_not_overflow_the_file() {
        let (_dir, skills, memory) = stores();
        remember(&memory, Target::User, &["short"]);
        let id = format!("memory:profile:0:{}", fingerprint("short"));
        let error = edit(&skills, &memory, &id, &"x".repeat(5000)).unwrap_err();
        assert!(error.message().contains("Shorten"), "{error}");
    }

    #[test]
    fn unknown_skills_are_refused() {
        let (_dir, skills, memory) = stores();
        assert!(matches!(
            detail(&skills, &memory, "nope"),
            Err(LearningError::NotFound(_))
        ));
        assert!(matches!(
            delete(&skills, &memory, "nope"),
            Err(LearningError::Rejected(_))
        ));
    }
}
