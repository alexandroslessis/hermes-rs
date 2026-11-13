//! CP9.3 tests: the project and archive tiers in `index.db`: retrieval goldens, scope isolation
//! between repositories (in SQL, before the limit), mid-session visibility, removed rows never
//! retrieved, the generation seen across stores, skills boosted by links, hits.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_memory::{MemoryConfig, MemoryStore, TurnFailures};
use serde_json::{Value, json};

use super::*;
use crate::skills::SkillStore;

struct World {
    dir: tempfile::TempDir,
    skills: Arc<SkillStore>,
    memory: Arc<MemoryStore>,
    index: Arc<LocalIndex>,
    a: PathBuf,
    b: PathBuf,
}

fn repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    project_skills::canonical(&root).unwrap()
}

fn world() -> World {
    world_with(MemoryConfig::default())
}

fn world_with(config: MemoryConfig) -> World {
    let dir = tempfile::tempdir().unwrap();
    let skills = Arc::new(SkillStore::new(dir.path().join("skills"), Default::default()));
    let memory = Arc::new(MemoryStore::new(dir.path().join("memories"), config));
    let index = LocalIndex::open(&dir.path().join("index.db"), skills.clone(), memory.clone()).unwrap();
    index.attach();
    let (a, b) = (repo(dir.path(), "repo-a"), repo(dir.path(), "repo-b"));
    World { dir, skills, memory, index, a, b }
}

fn scope(root: &Path) -> String {
    project_skills::memory_scope_key(root)
}

fn write(w: &World, root: Option<&Path>, tier: &str, target: &str, text: &str) -> Value {
    let key = root.map(scope);
    let reply = w.memory.tool_in(
        &json!({"target": target, "operations": [{"action": "add", "tier": tier, "content": text}]}),
        &mut TurnFailures::default(),
        key.as_deref(),
    );
    assert_eq!(reply["success"], true, "{reply}");
    reply
}

fn ask(message: &str, root: Option<&Path>) -> IndexQuery {
    IndexQuery {
        message: message.into(),
        project_root: root.map(Path::to_path_buf),
        ..Default::default()
    }
}

fn limits() -> Limits {
    Limits { skills: 3, memory: 4, ..Limits::default() }
}

fn found(w: &World, message: &str, root: Option<&Path>) -> Vec<MemoryHit> {
    w.index.query(&ask(message, root), &limits()).unwrap().memory
}

fn texts(hits: &[MemoryHit]) -> Vec<&str> {
    hits.iter().map(|h| h.text.as_str()).collect()
}

/// The fixture the goldens run on: an archive and two repositories' project entries.
fn fixture() -> World {
    let w = world();
    for text in [
        "The staging database password is rotated on the first Monday of every month",
        "Invoices are filed under the accounting folder by month",
        "The dog is called Pixel and is afraid of thunder",
        "Conference travel is booked through the university portal",
        "The printer on the third floor jams with recycled paper",
        "Backups of the photo library run nightly to the NAS",
    ] {
        write(&w, None, "archive", "memory", text);
    }
    for text in [
        "Release builds of alpha use the cargo release profile with lto enabled",
        "Alpha integration tests need the docker compose stack running",
        "Alpha deploys go through the staging cluster before production",
    ] {
        write(&w, Some(&w.a), "project", "memory", text);
    }
    for text in [
        "Beta deploys use terraform and the production cluster directly",
        "Beta integration tests run against a local postgres container",
    ] {
        write(&w, Some(&w.b), "project", "memory", text);
    }
    w
}

// ---- goldens --------------------------------------------------------------------------------

/// (message, the repository, a text the top hit must contain; `None`: nothing is retrieved).
const GOLDENS: &[(&str, u8, Option<&str>)] = &[
    ("when is the staging database password rotated", 0, Some("rotated on the first Monday")),
    ("where do I file the invoices for accounting", 0, Some("Invoices are filed")),
    ("why does the printer jam on the third floor", 0, Some("printer on the third floor")),
    ("what is the dog afraid of when there is thunder", 0, Some("Pixel")),
    ("how do I book conference travel through the portal", 0, Some("Conference travel")),
    ("how do the photo backups run to the NAS", 0, Some("Backups of the photo library")),
    ("how are the alpha integration tests run with docker", 1, Some("docker compose stack")),
    ("how do alpha release builds use lto", 1, Some("cargo release profile")),
    ("how does beta deploy to the production cluster with terraform", 2, Some("terraform")),
    ("explain this error", 1, None),
    ("what is the weather like tomorrow in the mountains", 1, None),
    ("hello", 1, None),
    ("", 1, None),
];

#[test]
fn goldens() {
    let w = fixture();
    let mut failures = Vec::new();
    for (message, repo, expected) in GOLDENS {
        let root = match repo {
            1 => Some(w.a.as_path()),
            2 => Some(w.b.as_path()),
            _ => None,
        };
        let hits = found(&w, message, root);
        let got = hits.first().map(|h| h.text.as_str());
        let ok = match expected {
            Some(part) => got.is_some_and(|t| t.contains(part)),
            None => got.is_none(),
        };
        if !ok {
            failures.push(format!("{message:?}: got {got:?}, wanted {expected:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_hit_names_its_tier_and_row_id() {
    let w = fixture();
    let hit = &found(&w, "when is the staging database password rotated", None)[0];
    assert_eq!(hit.tier, "archive");
    assert!(hit.id.starts_with("archive:") && hit.id[8..].parse::<i64>().is_ok(), "{}", hit.id);
    let hit = &found(&w, "how do alpha release builds use lto", Some(&w.a))[0];
    assert_eq!(hit.tier, "project");
    assert!(hit.id.starts_with("project:"), "{}", hit.id);
}

// ---- scope ----------------------------------------------------------------------------------

#[test]
fn a_repositorys_project_memory_never_leaks_into_another_repositorys_query() {
    let w = fixture();
    // B's words, asked from A: A's own entries or nothing, never B's.
    for message in [
        "how does beta deploy to the production cluster with terraform",
        "beta integration tests local postgres container",
    ] {
        for root in [Some(w.a.as_path()), None] {
            let hits = found(&w, message, root);
            assert!(hits.iter().all(|h| !h.text.starts_with("Beta")), "{message}: {:?}", texts(&hits));
        }
    }
    // And the other way round.
    let from_b = found(&w, "alpha deploys staging cluster before production", Some(&w.b));
    assert!(from_b.iter().all(|h| !h.text.starts_with("Alpha")), "{:?}", texts(&from_b));
    // The archive is global: both see it.
    for root in [&w.a, &w.b] {
        assert!(!found(&w, "when is the staging database password rotated", Some(root)).is_empty());
    }
}

#[test]
fn the_scope_is_filtered_before_the_limit_so_another_repository_cannot_crowd_it_out() {
    let w = world_with(MemoryConfig { project_char_limit: 20_000, ..MemoryConfig::default() });
    // 40 rows of B that match the question better than A's one: with a filter after LIMIT they
    // would fill the candidate list and A would see nothing.
    for n in 0..40 {
        write(&w, Some(&w.b), "project", "memory", &format!("Gamma deployment checklist step {n} deploy gamma cluster"));
    }
    write(&w, Some(&w.a), "project", "memory", "Gamma deployment is done by the ops team");
    let hits = found(&w, "gamma deployment checklist deploy cluster", Some(&w.a));
    assert!(hits.iter().any(|h| h.text.contains("done by the ops team")), "{:?}", texts(&hits));
    assert!(hits.iter().all(|h| !h.text.contains("checklist step")), "{:?}", texts(&hits));
}

#[test]
fn a_worktree_of_a_repository_sees_its_project_memory() {
    let w = world();
    write(&w, Some(&w.a), "project", "memory", "Alpha deploys go through the staging cluster");
    // A linked worktree's scope is its main checkout's (`memory_scope_key`): the same rows.
    let worktree = w.dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let gitdir = w.a.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(worktree.join(".git"), format!("gitdir: {}", gitdir.display())).unwrap();
    std::fs::write(gitdir.join("gitdir"), worktree.join(".git").display().to_string()).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..").unwrap();
    let wt = project_skills::canonical(&worktree).unwrap();
    assert_eq!(scope(&wt), scope(&w.a), "the worktree shares its checkout's memory scope");
    let hits = found(&w, "alpha deploys go through the staging cluster", Some(&wt));
    assert!(!hits.is_empty());
}

// ---- freshness ------------------------------------------------------------------------------

#[test]
fn an_entry_written_now_is_retrievable_by_the_next_query() {
    let w = world();
    assert!(found(&w, "when does the deploy password rotation happen", None).is_empty());
    write(&w, None, "archive", "memory", "The deploy password rotation happens on the first Monday");
    // No refresh was asked for: the write marked the index dirty and the query looked first.
    let hits = found(&w, "when does the deploy password rotation happen", None);
    assert_eq!(hits.len(), 1, "{:?}", texts(&hits));
    assert!(hits[0].text.contains("first Monday"));
}

#[test]
fn another_stores_write_is_seen_without_a_hook() {
    let w = world();
    let other = MemoryStore::new(w.dir.path().join("memories"), MemoryConfig::default());
    assert!(found(&w, "quarterly budget review meeting schedule", None).is_empty());
    other.tool(
        &json!({"target": "memory", "operations": [{"action": "add", "tier": "archive",
            "content": "The quarterly budget review meeting is scheduled by finance"}]}),
        &mut TurnFailures::default(),
    );
    let hits = found(&w, "quarterly budget review meeting schedule", None);
    assert_eq!(hits.len(), 1, "{:?}", texts(&hits));
}

#[test]
fn removed_and_rewritten_rows_are_not_retrieved_and_deleted_rows_leave_the_index() {
    let w = world();
    write(&w, Some(&w.a), "project", "memory", "Old advice: alpha pipeline uses jenkins for builds");
    write(&w, None, "archive", "memory", "Archived zebra grooming schedule for the weekend");
    write(&w, None, "archive", "memory", "Archived giraffe feeding rota for the weekend");
    assert_eq!(found(&w, "alpha pipeline jenkins builds", Some(&w.a)).len(), 1);
    let key = scope(&w.a);
    // Rewritten: the old text is gone, the new one is there.
    w.memory.tool_in(
        &json!({"target": "memory", "operations": [{"action": "patch", "tier": "project",
            "old_text": "jenkins", "content": "github actions"}]}),
        &mut TurnFailures::default(),
        Some(&key),
    );
    assert!(found(&w, "alpha pipeline jenkins builds", Some(&w.a)).iter().all(|h| !h.text.contains("jenkins")));
    assert!(found(&w, "alpha pipeline github actions builds", Some(&w.a)).iter().any(|h| h.text.contains("github actions")));
    // Removed by the agent (soft): not retrieved.
    w.memory.tool(
        &json!({"target": "memory", "operations": [{"action": "remove", "tier": "archive", "old_text": "zebra grooming"}]}),
        &mut TurnFailures::default(),
    );
    assert!(found(&w, "zebra grooming schedule weekend", None).iter().all(|h| !h.text.contains("zebra")));
    // Deleted by the user (hard): not in the index at all.
    let before = w.index.memory_count();
    assert_eq!(
        w.memory.delete_tier_entry(hermes_memory::Target::Memory, hermes_memory::Tier::Archive, None, "Archived giraffe feeding rota for the weekend"),
        Ok(true)
    );
    assert!(found(&w, "giraffe feeding rota weekend", None).is_empty());
    assert!(w.index.memory_count() < before);
}

#[test]
fn a_disabled_store_is_not_retrieved() {
    let w = world();
    write(&w, None, "archive", "user", "The user keeps a vegetable garden behind the house");
    assert_eq!(found(&w, "tell me about the vegetable garden behind the house", None).len(), 1);
    w.memory.set_config(MemoryConfig { user_profile_enabled: false, ..MemoryConfig::default() });
    assert!(found(&w, "tell me about the vegetable garden behind the house", None).is_empty());
}

// ---- when nothing is retrieved --------------------------------------------------------------

#[test]
fn one_word_and_generic_messages_get_no_tier_entry() {
    let w = fixture();
    for message in ["staging", "hello", "thanks", "ok, continue", "explain this error", "what does this do"] {
        assert!(found(&w, message, Some(&w.a)).is_empty(), "{message}");
    }
    // The assistant's last message alone can't pull an entry in: the user's own words must.
    let mut query = ask("tell me more about that", Some(&w.a));
    query.last_assistant = Some("The staging database password is rotated every month".into());
    assert!(w.index.query(&query, &limits()).unwrap().memory.is_empty());
}

/// A realistic archive (240 entries over many topics, written the way the agent writes them):
/// small talk and vague requests get nothing; a specific question gets its entry.
#[test]
fn a_large_varied_archive_injects_nothing_for_small_talk_or_vague_requests() {
    let w = world_with(MemoryConfig { project_char_limit: 20_000, ..MemoryConfig::default() });
    let subjects = [
        "the user", "we", "the build", "the bug tracker", "the team", "this project", "the API",
        "the frontend", "the database", "the deploy script", "the CI runner", "the laptop",
    ];
    let facts = [
        "prefers short answers and no emojis when we discuss the code",
        "fixed the bug in the parser by escaping backslashes in paths",
        "can help with reviewing pull requests on Fridays",
        "uses the error log under logs/app.log to explain this kind of error",
        "needs the VPN to reach the internal package mirror",
        "runs the nightly job at 02:00 UTC and mails a summary",
        "thanks the reviewers in the changelog for each release",
        "keeps the secrets in the password manager, never in the repo",
        "had a flaky test in the auth module that was fixed by a retry",
        "is configured with tabs of width four",
        "should be restarted after changing the environment file",
        "works best with the release profile for benchmarks",
        "hello world example lives in examples/hello.rs",
        "makes the build fail when clippy warnings appear",
        "do not push directly to main; open a pull request instead",
        "can run the tests in parallel with nextest",
        "talks to the payments sandbox with test keys only",
        "what changed in version 2 is in MIGRATION.md",
        "upgraded to Rust 1.85 in October",
        "stores uploads in the S3 bucket named media-prod",
    ];
    let mut n = 0;
    for subject in subjects {
        for fact in facts {
            n += 1;
            if n > 240 {
                break;
            }
            write(&w, None, "archive", "memory", &format!("{subject} {fact} (note {n})"));
        }
    }
    let quiet = [
        "hello", "thanks", "thanks!", "ok", "ok, continue", "yes please", "go on", "fix the bug",
        "can you help me with this", "what do you think", "do it", "explain this error",
        "looks good, thanks", "try again", "what now", "hello, how are you",
    ];
    let mut noisy = Vec::new();
    for message in quiet {
        let hits = found(&w, message, None);
        if !hits.is_empty() {
            noisy.push(format!("{message:?} -> {:?}", texts(&hits)));
        }
    }
    assert!(noisy.is_empty(), "{}", noisy.join("\n"));
    // A specific question still finds its entry.
    let hits = found(&w, "which S3 bucket stores the uploads", None);
    assert!(hits.first().is_some_and(|h| h.text.contains("media-prod")), "{:?}", texts(&hits));
    let hits = found(&w, "how do I reach the internal package mirror over VPN", None);
    assert!(hits.first().is_some_and(|h| h.text.contains("package mirror")), "{:?}", texts(&hits));
}

// ---- skills linked from memory --------------------------------------------------------------

fn make_skill(w: &World, name: &str, description: &str) {
    let dir = w.skills.root().join("general").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\nSteps.\n"),
    )
    .unwrap();
}

#[test]
fn a_skill_linked_from_a_retrieved_memory_ranks_up_but_is_never_brought_in() {
    let w = world();
    make_skill(&w, "release-notes", "Write release notes for a changelog.");
    make_skill(&w, "unrelated-linked", "Handles quasar telemetry only.");
    w.memory.tool(
        &json!({"target": "memory", "operations": [{"action": "add", "tier": "archive",
            "content": "Changelog entries for releases follow the keep a changelog format",
            "links": ["skill:release-notes", "skill:unrelated-linked", "skill:deleted-skill"]}]}),
        &mut TurnFailures::default(),
    );
    let q = ask("write the changelog entries for this release", None);
    let loose = Limits { skills: 3, memory: 3, min_skill_score: 0.0, ..Limits::default() };
    let plain = w.index.query(&ask("write the changelog entries for this release", None), &Limits { memory: 0, ..loose }).unwrap();
    let boosted = w.index.query(&q, &loose).unwrap();
    let score = |r: &Retrieval, name: &str| r.skills.iter().find(|s| s.name == name).map(|s| s.score);
    let (before, after) = (score(&plain, "release-notes").unwrap(), score(&boosted, "release-notes").unwrap());
    assert!((after - before - SKILL_LINK_BOOST).abs() < 1e-9, "{before} -> {after}");
    // A linked skill that doesn't match the message is not added; a dangling link is harmless.
    assert!(score(&boosted, "unrelated-linked").is_none());
    // The threshold is what the boost can change: a skill just under it comes over it.
    let strict = Limits { min_skill_score: before + 0.5, ..loose };
    assert!(w.index.query(&q, &strict).unwrap().skills.iter().any(|s| s.name == "release-notes"));
    assert!(w.index.query(&q, &Limits { memory: 0, ..strict }).unwrap().skills.is_empty());
}

#[test]
fn the_boost_is_bounded() {
    let w = world();
    make_skill(&w, "release-notes", "Write release notes for a changelog.");
    for n in 0..4 {
        w.memory.tool(
            &json!({"target": "memory", "operations": [{"action": "add", "tier": "archive",
                "content": format!("Changelog convention {n}: keep release headings level {n} consistent"),
                "links": ["skill:release-notes"]}]}),
            &mut TurnFailures::default(),
        );
    }
    let q = ask("write changelog convention for release headings", None);
    let loose = Limits { skills: 3, memory: 4, min_skill_score: 0.0, ..Limits::default() };
    let plain = w.index.query(&q, &Limits { memory: 0, ..loose }).unwrap().skills[0].score;
    let boosted = w.index.query(&q, &loose).unwrap();
    assert!(boosted.memory.len() >= 3);
    assert!((boosted.skills[0].score - plain - MAX_SKILL_BOOST).abs() < 1e-9);
}

// ---- hits -----------------------------------------------------------------------------------

#[test]
fn retrieved_entries_are_counted_as_hits_in_a_batch() {
    let w = world();
    write(&w, None, "archive", "memory", "The backup of the photo library runs nightly to the NAS");
    w.memory.tool(
        &json!({"target": "memory", "operations": [{"action": "add", "content": "The NAS is reachable at nas.local on the home network"}]}),
        &mut TurnFailures::default(),
    );
    let hits = found(&w, "does the nightly photo library backup to the NAS run", None);
    assert!(!hits.is_empty());
    w.index.record_hits(&hits);
    assert!(w.memory.pending_hits() > 0, "counted, not yet written");
    w.memory.flush_hits();
    let stats = w.memory.row_stats(&[hits.iter().find(|h| h.tier == "archive").unwrap().id[8..].parse().unwrap()]);
    assert_eq!(stats[0].hits, 1);
    assert!(w.memory.core_rows(hermes_memory::Target::Memory).iter().any(|r| r.hits == 1)
        == hits.iter().any(|h| h.tier == "memory"));
}

#[test]
fn recent_and_popular_entries_rank_up_among_matches_but_do_not_replace_relevance() {
    let w = world();
    write(&w, None, "archive", "memory", "Sprint planning for the mobile app happens on Mondays");
    write(&w, None, "archive", "memory", "Sprint planning for the mobile team is held on Mondays too");
    let ids: Vec<i64> = w.memory.tier_index_rows().unwrap().1.iter().map(|r| r.id).collect();
    let message = "when is the sprint planning for the mobile app held on mondays";
    let first = found(&w, message, None)[0].id.clone();
    // Hits push the other one up when the BM25 gap is small.
    let other = ids.iter().map(|id| format!("archive:{id}")).find(|id| *id != first).unwrap();
    let id: i64 = other[8..].parse().unwrap();
    w.memory.record_hits(std::iter::repeat(hermes_memory::HitRef::Row(id)).take(40));
    w.memory.flush_hits();
    let after = found(&w, message, None);
    assert!(after.iter().any(|h| h.id == other));
    assert_eq!(after.len(), 2);
}

// ---- the file itself ------------------------------------------------------------------------

#[test]
fn an_index_file_of_the_previous_version_is_rebuilt() {
    let World { dir, skills, memory, index, a, b } = world();
    drop(index);
    let _ = (&a, &b);
    let path = dir.path().join("index.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE memory_doc;
             CREATE VIRTUAL TABLE memory_doc USING fts5(id UNINDEXED, tier UNINDEXED, scope UNINDEXED, text, tags);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
    }
    memory.tool(
        &json!({"target": "memory", "operations": [{"action": "add", "tier": "archive",
            "content": "The nightly job restarts the reporting service"}]}),
        &mut TurnFailures::default(),
    );
    let index = LocalIndex::open(&path, skills.clone(), memory.clone()).unwrap();
    let hits = index.query(&ask("which nightly job restarts the reporting service", None), &limits()).unwrap().memory;
    assert_eq!(hits.len(), 1, "{:?}", texts(&hits));
}
