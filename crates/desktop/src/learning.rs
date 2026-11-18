//! The Star map (`/memory-graph`, `/learning`) as Hermes's routes answer it
//! (`/api/learning/*`): `StarmapGraph` and the `{ok, message, detail}` node answers. The graph
//! itself (learned skills and memory entries as nodes, their links, editing and deleting a node)
//! is `hermes_core::learning`; its header says what is Hermes's and what is ours.
//!
//! Hermes: `agent/learning_graph.py` (`build_learning_graph`), `agent/learning_mutations.py`
//! (`node_detail`, `edit_node`, `delete_node`), served as `/api/learning/*` by
//! `hermes_cli/web_routers/status.py`.
//!
//! **Additive and ignored by the page:** top-level `archive` and `memory_links` (see the core
//! module's doc): the page's graph knows nodes and `{source, target}` edges between nodes.

use hermes_core::learning::{self, Graph, LearningError};
use hermes_memory::MemoryStore;
use hermes_tools::SkillStore;
use serde_json::{Value, json};

pub use hermes_core::learning::fingerprint;

type Answer = (u16, Value);

/// `GET /api/learning/graph` (`StarmapGraph`).
pub fn graph(skills: &SkillStore, memory: &MemoryStore) -> Value {
    graph_json(&learning::graph(skills, memory))
}

fn graph_json(graph: &Graph) -> Value {
    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|node| match node.memory_source {
            None => json!({"id": node.id, "label": node.label, "kind": node.kind,
                           "timestamp": node.timestamp, "category": node.category,
                           "useCount": node.use_count, "state": node.state,
                           "createdBy": node.created_by, "pinned": node.pinned}),
            Some(source) => json!({"id": node.id, "label": node.label, "kind": node.kind,
                                   "memorySource": source, "timestamp": node.timestamp,
                                   "category": node.category, "useCount": node.use_count,
                                   "state": node.state, "createdBy": node.created_by,
                                   "pinned": node.pinned}),
        })
        .collect();
    let stats = &graph.stats;
    json!({
        "nodes": nodes,
        "edges": graph.edges.iter().map(|(a, b)| json!({"source": a, "target": b})).collect::<Vec<_>>(),
        "clusters": graph.clusters.iter().map(|(c, n)| json!({"category": c, "count": n})).collect::<Vec<_>>(),
        "memory": graph.memory.iter().map(|card| json!({"source": card.source, "timestamp": card.timestamp,
            "title": card.title, "body": card.body, "fingerprint": card.fingerprint})).collect::<Vec<_>>(),
        // Additive, ignored by the page (see the module doc).
        "archive": graph.archive.iter().map(|row| json!({
            "id": format!("archive:{}", row.id), "kind": row.kind, "text": row.text,
            "tags": row.tags, "links": row.links, "created": row.created, "hits": row.hits,
        })).collect::<Vec<_>>(),
        "memory_links": graph.memory_links.iter().map(|link| json!({
            "source": link.source, "kind": link.kind, "target": link.target,
            "resolved": link.resolved,
        })).collect::<Vec<_>>(),
        "stats": {
            "memory_link_edges": stats.memory_link_edges,
            "archive_entries": stats.archive_entries,
            "nodes": stats.nodes,
            "related_edges": stats.related_edges,
            "edges_per_node": stats.edges_per_node,
            "linked_nodes": stats.linked_nodes,
            "isolated_pct": stats.isolated_pct,
            "categories": stats.categories,
            "agent_created": stats.agent_created,
            "used": stats.used,
            "top_categories": stats.top_categories.iter().map(|(c, n)| json!([c, n])).collect::<Vec<_>>(),
            "memory_nodes": stats.memory_nodes,
            "memory_skill_edges": stats.memory_skill_edges,
            "learned_skills": stats.learned_skills,
        },
    })
}

/// The page's failure answer: the status and the message under `message` and `detail`.
fn failure(status: u16, error: LearningError) -> Answer {
    let message = error.message().to_string();
    (
        status,
        json!({"ok": false, "message": message, "detail": message}),
    )
}

/// A node call that went through: `{ok: true, message}`.
fn done(message: String) -> Answer {
    (200, json!({"ok": true, "message": message}))
}

/// `GET /api/learning/node?id=`: a node's content, for the edit form (`node_detail`).
pub fn detail(skills: &SkillStore, memory: &MemoryStore, id: &str) -> Answer {
    match learning::detail(skills, memory, id) {
        Ok(node) => (
            200,
            json!({"ok": true, "kind": node.kind, "id": node.id,
                   "label": node.label, "content": node.content}),
        ),
        Err(error) => failure(404, error),
    }
}

/// `DELETE /api/learning/node` `{id}`: a skill is archived (restorable), a memory entry removed.
pub fn delete(skills: &SkillStore, memory: &MemoryStore, id: &str) -> Answer {
    match learning::delete(skills, memory, id) {
        Ok(message) => done(message),
        Err(error) => failure(400, error),
    }
}

/// `PUT /api/learning/node` `{id, content}`: a skill's SKILL.md or a memory entry rewritten.
pub fn edit(skills: &SkillStore, memory: &MemoryStore, id: &str, content: &str) -> Answer {
    match learning::edit(skills, memory, id, content) {
        Ok(message) => done(message),
        Err(error) => failure(400, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_memory::Target;
    use std::collections::HashSet;

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

    // ---- links (Phase 9, CP9.3) ---------------------------------------------------------------

    fn learned_skill(skills: &SkillStore, name: &str) {
        let dir = skills.root().join("devops").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---
name: {name}
description: Does {name}.
---
# {name}
"
            ),
        )
        .unwrap();
        let path = skills.root().join(".usage.json");
        let mut usage: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}));
        usage[name] = json!({"uses": 1});
        std::fs::write(path, usage.to_string()).unwrap();
    }

    fn add_with(memory: &MemoryStore, scope: Option<&str>, tier: &str, text: &str, links: Value) {
        let reply = memory.tool_in(
            &json!({"target": "memory", "operations": [{"action": "add", "tier": tier, "content": text, "links": links}]}),
            &mut hermes_memory::TurnFailures::default(),
            scope,
        );
        assert_eq!(reply["success"], true, "{reply}");
    }

    #[test]
    fn a_cards_links_are_real_edges_and_dangling_ones_break_nothing() {
        let (_dir, skills, memory) = stores();
        learned_skill(&skills, "release-steps");
        // An archive entry the first card links to (`memory:<id>`), and a second core card.
        add_with(
            &memory,
            None,
            "archive",
            "Archived release checklist for the old pipeline",
            json!([]),
        );
        let archive_id = memory.archive_rows(10)[0].id;
        add_with(
            &memory,
            None,
            "core",
            "Tag releases from the main branch",
            json!([
                "skill:release-steps",
                "skill:deleted-skill",
                format!("memory:{archive_id}"),
                "memory:99999",
                "file:C:/docs/x.md",
                "session:abc"
            ]),
        );
        add_with(
            &memory,
            None,
            "core",
            "A second card with no links",
            json!([]),
        );
        let graph = graph(&skills, &memory);
        let nodes = graph["nodes"].as_array().unwrap();
        let card = nodes
            .iter()
            .find(|n| n["label"] == "Tag releases from the main branch")
            .unwrap()["id"]
            .clone();
        let edges = graph["edges"].as_array().unwrap();
        // The explicit skill link is an edge between two nodes of the page's graph...
        assert!(
            edges.contains(&json!({"source": card, "target": "release-steps"})),
            "{edges:?}"
        );
        // ...and nothing points at something that is not a node (the page ignores those anyway).
        let ids: HashSet<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
        for edge in edges {
            assert!(
                ids.contains(edge["source"].as_str().unwrap())
                    && ids.contains(edge["target"].as_str().unwrap()),
                "{edge}"
            );
        }
        // Every link is listed, additively, with whether its end exists.
        let links = graph["memory_links"].as_array().unwrap();
        let find = |target: &str| {
            links
                .iter()
                .find(|l| l["target"] == target)
                .unwrap()
                .clone()
        };
        assert_eq!(find("skill:release-steps")["resolved"], true);
        assert_eq!(find("skill:deleted-skill")["resolved"], false);
        assert_eq!(find(&format!("memory:{archive_id}"))["resolved"], true);
        assert_eq!(find("memory:99999")["resolved"], false);
        assert_eq!(find("file:C:/docs/x.md")["resolved"], Value::Null);
        assert_eq!(find("session:abc")["kind"], "session");
        // The archive is listed (additive), newest first; the old fields are all still there.
        assert_eq!(graph["archive"][0]["id"], format!("archive:{archive_id}"));
        for key in ["nodes", "edges", "clusters", "memory", "stats"] {
            assert!(graph.get(key).is_some(), "{key}");
        }
        assert_eq!(graph["stats"]["memory_link_edges"], 1);
        // The linked skill and the archive entry go away: the endpoint still answers, the links
        // are unresolved.
        assert_eq!(delete(&skills, &memory, "release-steps").0, 200);
        assert_eq!(
            memory.delete_tier_entry(
                Target::Memory,
                hermes_memory::Tier::Archive,
                None,
                "Archived release checklist for the old pipeline"
            ),
            Ok(true)
        );
        let after = super::graph(&skills, &memory);
        let links = after["memory_links"].as_array().unwrap();
        assert!(
            links
                .iter()
                .filter(|l| l["kind"] == "skill" || l["kind"] == "memory")
                .all(|l| l["resolved"] == false),
            "{links:?}"
        );
        assert!(
            after["edges"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["target"] != "release-steps")
        );
        assert!(after["archive"].as_array().unwrap().is_empty());
    }

    #[test]
    fn project_memory_is_never_in_the_map() {
        let (_dir, skills, memory) = stores();
        add_with(
            &memory,
            Some("project:c:/work/repo-a"),
            "project",
            "Repo A builds with cargo xtask",
            json!([]),
        );
        add_with(
            &memory,
            Some("project:c:/work/repo-b"),
            "project",
            "Repo B deploys with make ship",
            json!([]),
        );
        add_with(
            &memory,
            None,
            "archive",
            "A global archive fact",
            json!(["skill:x"]),
        );
        let graph = graph(&skills, &memory);
        let text = graph.to_string();
        assert!(
            !text.contains("xtask") && !text.contains("make ship"),
            "{text}"
        );
        assert_eq!(graph["archive"].as_array().unwrap().len(), 1);
        assert_eq!(
            graph["nodes"].as_array().unwrap().len(),
            0,
            "archive entries are not nodes the page could edit"
        );
    }
}
