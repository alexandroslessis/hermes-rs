//! The Star map (`/memory-graph`, `/learning`): what the agent has learned, as a graph. Learned
//! skills (made by the agent, or used) and every MEMORY.md / USER.md entry are nodes; skills link
//! through their `related_skills`, memories to the skills they mention. A node opens its content,
//! which the user can edit or delete (a skill is archived, restorable; a memory entry is removed
//! under the memory store's lock).
//!
//! Hermes: `agent/learning_graph.py` (`build_learning_graph`), `agent/learning_mutations.py`
//! (`node_detail`, `edit_node`, `delete_node`), served as `/api/learning/*` by
//! `hermes_cli/web_routers/status.py`.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use hermes_memory::{MemoryStore, Target};
use hermes_tools::SkillStore;
use serde_json::{Value, json};
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

/// `GET /api/learning/graph` (`StarmapGraph`).
pub fn graph(skills: &SkillStore, memory: &MemoryStore) -> Value {
    let learned = learned_skills(skills);
    let cards = memory_cards(memory);
    let links = skill_edges(&learned);
    let mentions = memory_edges(&cards, &learned);

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
    clusters.sort_by(|a, b| b.1.cmp(&a.1));

    let mut nodes: Vec<Value> = learned
        .iter()
        .map(|s| {
            json!({"id": s.name, "label": s.name, "kind": "skill", "timestamp": s.timestamp,
                   "category": s.category, "useCount": s.use_count, "state": s.state,
                   "createdBy": s.created_by, "pinned": s.pinned})
        })
        .collect();
    nodes.extend(cards.iter().enumerate().map(|(i, card)| {
        json!({"id": card_id(card, i), "label": card.title, "kind": "memory",
               "memorySource": card.source, "timestamp": card.timestamp, "category": "memory",
               "useCount": 0, "state": "active", "createdBy": "memory", "pinned": false})
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
    categories.sort_by(|a, b| b.1.cmp(&a.1));
    let round = |value: f64, places: i32| (value * 10f64.powi(places)).round() / 10f64.powi(places);
    json!({
        "nodes": nodes,
        "edges": links.iter().chain(&mentions).map(|(a, b)| json!({"source": a, "target": b})).collect::<Vec<_>>(),
        "clusters": clusters.iter().map(|(c, n)| json!({"category": c, "count": n})).collect::<Vec<_>>(),
        "memory": cards.iter().map(|card| json!({"source": card.source, "timestamp": card.timestamp,
            "title": card.title, "body": card.body, "fingerprint": card.fingerprint})).collect::<Vec<_>>(),
        "stats": {
            "nodes": learned.len(),
            "related_edges": links.len(),
            "edges_per_node": round(links.len() as f64 / total, 3),
            "linked_nodes": linked.len(),
            "isolated_pct": round(100.0 * (total - linked.len() as f64) / total, 1),
            "categories": category_count,
            "agent_created": learned.iter().filter(|s| s.created_by.as_deref() == Some("agent")).count(),
            "used": learned.iter().filter(|s| s.use_count > 0).count(),
            "top_categories": categories.iter().take(8).map(|(c, n)| json!([c, n])).collect::<Vec<_>>(),
            "memory_nodes": cards.len(),
            "memory_skill_edges": mentions.len(),
            "learned_skills": learned.len(),
        },
    })
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

type Answer = (u16, Value);

fn failure(status: u16, message: impl Into<String>) -> Answer {
    let message = message.into();
    (
        status,
        json!({"ok": false, "message": message, "detail": message}),
    )
}

/// `GET /api/learning/node?id=`: a node's content, for the edit form (`node_detail`).
pub fn detail(skills: &SkillStore, memory: &MemoryStore, id: &str) -> Answer {
    if id.starts_with("memory:") {
        return match locate_memory(memory, id) {
            Ok((_, entry)) => (
                200,
                json!({"ok": true, "kind": "memory", "id": id,
                       "label": entry.lines().next().unwrap_or_default().chars().take(80).collect::<String>(),
                       "content": entry.trim()}),
            ),
            Err(message) => failure(404, message),
        };
    }
    let Some(skill) = skills.find_any(id) else {
        return failure(404, format!("skill '{id}' not found"));
    };
    match std::fs::read_to_string(skill.skill_md()) {
        Ok(content) => (
            200,
            json!({"ok": true, "kind": "skill", "id": id, "label": id,
                   "content": content.trim_start_matches('\u{feff}')}),
        ),
        Err(_) => failure(404, format!("SKILL.md missing for '{id}'")),
    }
}

fn memory_outcome(result: Value) -> Answer {
    if result["success"] == true {
        (200, json!({"ok": true, "message": result["message"]}))
    } else {
        failure(
            400,
            result["error"].as_str().unwrap_or("memory write failed"),
        )
    }
}

/// `DELETE /api/learning/node` `{id}`: a skill is archived (restorable), a memory entry removed.
pub fn delete(skills: &SkillStore, memory: &MemoryStore, id: &str) -> Answer {
    if id.starts_with("memory:") {
        return match locate_memory(memory, id) {
            Ok((target, entry)) => memory_outcome(memory.edit_entry(target, &entry, None)),
            Err(message) => failure(400, message),
        };
    }
    if skills.record(id).pinned {
        return failure(400, format!("'{id}' is pinned — unpin it first"));
    }
    match skills.archive_by_user(id) {
        Ok(()) => (
            200,
            json!({"ok": true, "message": format!("archived '{id}' — it can be restored from the skills archive")}),
        ),
        Err(message) => failure(400, message),
    }
}

/// `PUT /api/learning/node` `{id, content}`: a skill's SKILL.md or a memory entry rewritten.
pub fn edit(skills: &SkillStore, memory: &MemoryStore, id: &str, content: &str) -> Answer {
    if id.starts_with("memory:") {
        let located = locate_memory(memory, id);
        if content.trim().is_empty() {
            return failure(400, "empty memory — use delete to remove it");
        }
        return match located {
            Ok((target, entry)) => memory_outcome(memory.edit_entry(target, &entry, Some(content))),
            Err(message) => failure(400, message),
        };
    }
    let result = skills.user_write(hermes_tools::SkillOp {
        action: "edit".into(),
        name: id.to_string(),
        content: Some(content.to_string()),
        ..Default::default()
    });
    if result["success"] == true {
        (
            200,
            json!({"ok": true, "message": format!("updated '{id}'")}),
        )
    } else {
        failure(400, result["error"].as_str().unwrap_or("edit failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (tempfile::TempDir, SkillStore, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
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
        let nodes = graph["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0]["label"], "Build");
        assert_eq!(nodes[2]["memorySource"], "profile");
        assert_eq!(
            graph["clusters"],
            json!([{"category": "memory", "count": 3}])
        );
        let second = nodes[1]["id"].as_str().unwrap().to_string();
        assert!(second.starts_with("memory:memory:1:"));

        let (status, shown) = detail(&skills, &memory, &second);
        assert_eq!(status, 200, "{shown}");
        assert_eq!(shown["content"], "Ports: 47731 for the app.");

        // An entry removed before this one shifts it; its id still names it.
        remember(&memory, Target::Memory, &["Ports: 47731 for the app."]);
        let (status, edited) = edit(
            &skills,
            &memory,
            &second,
            "Ports: 47731 (app), 47810 (perf).",
        );
        assert_eq!(status, 200, "{edited}");
        assert_eq!(
            memory.entries(Target::Memory),
            ["Ports: 47731 (app), 47810 (perf)."]
        );
        // The old text is gone: its id is stale now.
        assert_eq!(edit(&skills, &memory, &second, "x").0, 400);
        assert_eq!(edit(&skills, &memory, &second, "  ").0, 400);

        let user = nodes[2]["id"].as_str().unwrap();
        assert_eq!(delete(&skills, &memory, user).0, 200);
        assert!(memory.entries(Target::User).is_empty());
        assert_eq!(detail(&skills, &memory, "memory:nope:0").0, 404);
    }

    #[test]
    fn a_replacement_may_not_overflow_the_file() {
        let (_dir, skills, memory) = stores();
        remember(&memory, Target::User, &["short"]);
        let id = format!("memory:profile:0:{}", fingerprint("short"));
        let (status, answer) = edit(&skills, &memory, &id, &"x".repeat(5000));
        assert_eq!(status, 400);
        assert!(answer["message"].as_str().unwrap().contains("Shorten"));
    }

    #[test]
    fn only_learned_skills_are_nodes_linked_to_memories() {
        let (_dir, skills, memory) = stores();
        for (name, related) in [
            ("release-steps", "[deploy-checks]"),
            ("deploy-checks", "[]"),
            ("unused", "[]"),
        ] {
            let dir = skills.root().join("devops").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Does {name}.\nrelated_skills: {related}\n---\n# {name}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            skills.root().join(".usage.json"),
            r#"{"release-steps": {"uses": 2}, "deploy-checks": {"uses": 1}}"#,
        )
        .unwrap();
        remember(
            &memory,
            Target::Memory,
            &["Always run the release-steps before tagging."],
        );
        let graph = graph(&skills, &memory);
        let ids: Vec<&str> = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"release-steps") && ids.contains(&"deploy-checks"));
        assert!(
            !ids.contains(&"unused"),
            "never used, not made by the agent"
        );
        let edges = graph["edges"].as_array().unwrap();
        assert!(edges.contains(&json!({"source": "deploy-checks", "target": "release-steps"})));
        assert!(
            edges
                .iter()
                .any(|e| e["source"].as_str().unwrap().starts_with("memory:")
                    && e["target"] == "release-steps")
        );
        assert_eq!(graph["stats"]["learned_skills"], 2);

        // Edit and archive by the user.
        let (status, _) = edit(
            &skills,
            &memory,
            "deploy-checks",
            "---\nname: deploy-checks\ndescription: Checks a deploy.\n---\n# Deploy checks\n",
        );
        assert_eq!(status, 200);
        assert!(
            detail(&skills, &memory, "deploy-checks").1["content"]
                .as_str()
                .unwrap()
                .contains("Checks a deploy.")
        );
        assert_eq!(delete(&skills, &memory, "deploy-checks").0, 200);
        assert!(skills.find_any("deploy-checks").is_none());
    }
}
