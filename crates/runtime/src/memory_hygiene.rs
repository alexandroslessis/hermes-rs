//! The memory part of the curator: a deterministic pass, zero model requests (Backend v2 Phase 9,
//! CP9.3). Not from Hermes: see `PORTING.md`, `diverged`.
//!
//! A `core` entry (MEMORY.md / USER.md: in every system prompt, so every one costs tokens on every
//! turn) that nobody has confirmed or hit for [`hermes_memory::STALE_DAYS`] days is proposed for
//! **demotion to the archive**, where it is retrieved only when a message is about it. The pass
//! runs only under a profile that retrieves memory (`budget.memory_preload` > 0: `requests`); see
//! [`Runtime::memory_hygiene`]. The
//! proposal is **staged** (Pending in the memory screen), never applied here: applying it
//! (`MemoryStore::demote_core`) goes through the same limits, threat scan and staleness checks as
//! any write. Entries tagged `pinned`, entries touched within the window (a new entry counts from
//! the day it was made), and rows proposed in the last 30 days (a rejected proposal is not made
//! again at the next run) are left alone; at most [`MAX_PROPOSALS`] are made per run, oldest
//! first; a proposal that is already pending is not made twice (the staging area keeps one item per
//! change).

use hermes_memory::{MemoryStore, StaleEntry};
use hermes_tools::staging::{Meta, StagingStore};

use crate::Runtime;

/// Demotions one run proposes.
pub const MAX_PROPOSALS: usize = 5;

/// The reason the Pending list shows.
const REASON: &str = "memory hygiene: not used or confirmed for 60 days";

/// Stages demotions for the stale `core` entries of `memory` (at most [`MAX_PROPOSALS`]); the
/// entries proposed (new or already pending). Blocking.
pub fn propose_demotions(
    memory: &MemoryStore,
    staging: &StagingStore,
    now: i64,
) -> Vec<StaleEntry> {
    let meta = Meta {
        session_id: "curator".into(),
        sources: Vec::new(),
        reason: REASON.into(),
    };
    let mut proposed = Vec::new();
    for entry in memory.hygiene_candidates(now, MAX_PROPOSALS) {
        match staging.stage_demotion(entry.target, &entry.text, &meta) {
            Ok(_) => proposed.push(entry),
            Err(why) => {
                // The area is full or unusable: stop, the next run tries again.
                tracing::warn!("memory hygiene: could not stage a demotion: {why}");
                break;
            }
        }
    }
    let ids: Vec<i64> = proposed.iter().map(|e| e.id).collect();
    memory.mark_proposed(&ids, now);
    proposed
}

impl Runtime {
    /// The curator's memory pass: how many demotions were proposed (see the module doc). Only
    /// when the profile retrieves memory (`budget.memory_preload` > 0, i.e. `requests`): under
    /// `balanced` / `tokens` nothing brings an archive entry back into the model's view, so a
    /// demotion would quietly make the agent forget, and no retrieval records the hits that keep
    /// a used entry out of the pass (every entry would look unused after 60 days).
    pub(crate) async fn memory_hygiene(&self) -> usize {
        if self.budget().memory_preload == 0 {
            return 0;
        }
        let (memory, staging) = (self.memory.clone(), self.staging());
        let now = chrono::Utc::now().timestamp();
        tokio::task::spawn_blocking(move || propose_demotions(&memory, &staging, now).len())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("memory hygiene failed: {e}");
                0
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_memory::{MemoryConfig, Target, TurnFailures};
    use serde_json::json;

    const DAY: i64 = 86_400;

    fn world() -> (tempfile::TempDir, MemoryStore, StagingStore) {
        let dir = tempfile::tempdir().unwrap();
        let memory = MemoryStore::new(dir.path().join("memories"), MemoryConfig::default());
        let staging = StagingStore::new(dir.path());
        (dir, memory, staging)
    }

    fn add(memory: &MemoryStore, text: &str, tags: &[&str]) {
        let done = memory.tool(
            &json!({"target": "memory", "operations": [{"action": "add", "content": text, "tags": tags}]}),
            &mut TurnFailures::default(),
        );
        assert_eq!(done["success"], true, "{done}");
    }

    #[test]
    fn nothing_is_proposed_for_entries_that_are_new() {
        let (_dir, memory, staging) = world();
        add(&memory, "Brand new entry", &[]);
        let now = chrono::Utc::now().timestamp();
        assert!(propose_demotions(&memory, &staging, now + 30 * DAY).is_empty());
        assert!(staging.list().is_empty());
    }

    #[test]
    fn stale_entries_are_staged_boundedly_once_each_and_pinned_ones_never() {
        let (_dir, memory, staging) = world();
        for n in 0..7 {
            add(&memory, &format!("Stale entry number {n} about topic{n}"), &[]);
        }
        add(&memory, "Pinned entry that stays in the prompt", &["pinned"]);
        let later = chrono::Utc::now().timestamp() + 90 * DAY;
        // A run proposes at most MAX_PROPOSALS.
        let first = propose_demotions(&memory, &staging, later);
        assert_eq!(first.len(), MAX_PROPOSALS);
        assert_eq!(staging.list().len(), MAX_PROPOSALS);
        // The next run takes the rest, never one already proposed (pending or rejected).
        let second = propose_demotions(&memory, &staging, later);
        assert_eq!(second.len(), 7 - MAX_PROPOSALS);
        assert_eq!(staging.list().len(), 7);
        assert!(propose_demotions(&memory, &staging, later).is_empty());
        let items = staging.list();
        let diffs: std::collections::HashSet<&str> = items.iter().map(|i| i.diff.as_str()).collect();
        assert_eq!(diffs.len(), 7, "no proposal twice");
        assert!(items.iter().all(|i| !i.diff.contains("Pinned entry")));
        assert!(items.iter().all(|i| i.reason.contains("60 days") && i.session_id == "curator"));
        // Nothing was applied: the prompt still holds every entry.
        assert_eq!(memory.entries(Target::Memory).len(), 8);
    }

    #[test]
    fn a_rejected_proposal_is_not_made_again_soon() {
        let (_dir, memory, staging) = world();
        add(&memory, "Entry the user wants to keep", &[]);
        let later = chrono::Utc::now().timestamp() + 90 * DAY;
        assert_eq!(propose_demotions(&memory, &staging, later).len(), 1);
        let id = staging.list()[0].id.clone();
        staging.reject(&id).unwrap();
        assert!(propose_demotions(&memory, &staging, later + 5 * DAY).is_empty());
        // After the quiet period it may be proposed again.
        assert_eq!(propose_demotions(&memory, &staging, later + 40 * DAY).len(), 1);
    }

    #[test]
    fn a_hand_edit_of_memory_md_between_proposal_and_apply_is_kept_never_clobbered() {
        let (dir, memory, staging) = world();
        let skills = hermes_tools::skills::SkillStore::new(dir.path().join("skills"), Default::default());
        let file = dir.path().join("memories").join("MEMORY.md");
        add(&memory, "Old fact to archive", &[]);
        add(&memory, "Other fact the user edits", &[]);
        let later = chrono::Utc::now().timestamp() + 90 * DAY;
        let proposed = propose_demotions(&memory, &staging, later);
        assert_eq!(proposed.len(), 2);
        let item_for = |text: &str| {
            staging.list().into_iter().find(|i| i.diff.contains(text)).unwrap().id
        };
        let (old_item, other_item) = (item_for("Old fact to archive"), item_for("Other fact the user edits"));
        // The user edits the OTHER entry by hand: the demotion still applies, the edit survives.
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, text.replace("Other fact the user edits", "Other fact, edited by hand")).unwrap();
        let applied = staging.apply(&old_item, &memory, &skills);
        assert!(matches!(applied, hermes_tools::staging::Applied::Done(_)), "{applied:?}");
        assert_eq!(memory.entries(Target::Memory), ["Other fact, edited by hand"]);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(on_disk.contains("Other fact, edited by hand") && !on_disk.contains("Old fact to archive"), "{on_disk}");
        assert_eq!(memory.tier_index_rows().unwrap().1[0].text, "Old fact to archive");
        // The proposed entry itself was changed by hand: stale, nothing copied, the file as edited.
        let applied = staging.apply(&other_item, &memory, &skills);
        assert!(matches!(applied, hermes_tools::staging::Applied::Failed { stale: true, .. }), "{applied:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), on_disk);
        assert_eq!(memory.tier_index_rows().unwrap().1.len(), 1);
    }

    #[test]
    fn approving_a_proposal_moves_the_entry_and_rechecks_it() {
        let (_dir, memory, staging) = world();
        let skills = hermes_tools::skills::SkillStore::new(_dir.path().join("skills"), Default::default());
        add(&memory, "Entry that moves to the archive", &[]);
        add(&memory, "Entry that is rewritten first", &[]);
        let later = chrono::Utc::now().timestamp() + 90 * DAY;
        propose_demotions(&memory, &staging, later);
        let items = staging.list();
        assert_eq!(items.len(), 2);
        memory.tool(
            &json!({"target": "memory", "operations": [{"action": "patch", "old_text": "rewritten first", "content": "rewritten already"}]}),
            &mut TurnFailures::default(),
        );
        let mut moved = 0;
        let mut stale = 0;
        for item in &items {
            match staging.apply(&item.id, &memory, &skills) {
                hermes_tools::staging::Applied::Done(_) => moved += 1,
                hermes_tools::staging::Applied::Failed { stale: true, .. } => stale += 1,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!((moved, stale), (1, 1));
        assert_eq!(memory.entries(Target::Memory), ["Entry that is rewritten already"]);
        assert_eq!(memory.tier_index_rows().unwrap().1.len(), 1);
    }
}
