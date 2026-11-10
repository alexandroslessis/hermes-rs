//! The subagents running now (`delegate_task` children), so the page can list, follow, steer
//! and stop them: Hermes's live subagent registry (`tools/delegate_tool_registry.py`, served as
//! `subagent.list`, `subagent.tail`, `subagent.steer`, `subagent.interrupt` and
//! `delegation.status` / `delegation.pause` in `tui_gateway`).
//!
//! A child registers when it starts and leaves when it ends; its activity also goes out as
//! `AgentEvent::Subagent` events (`subagent.start`, `.tool`, `.thinking`, `.progress`,
//! `.complete`), which the page's live rows follow. The registry is what a reloaded page
//! reconciles against.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::ops::SharedTurnState;

/// How much of a child's output `subagent.tail` keeps.
const TAIL_CHARS: usize = 16_000;

struct Live {
    /// The parent session the child works for.
    session_id: String,
    goal: String,
    delegation_id: String,
    model: String,
    started_at: f64,
    tool_count: u64,
    last_tool: Option<String>,
    cancel: CancellationToken,
    /// The child's turn state: a steer waits there for its next tool results.
    state: SharedTurnState,
    tail: String,
    tail_truncated: bool,
}

#[derive(Default)]
pub struct Subagents {
    live: Mutex<HashMap<String, Live>>,
    /// `delegation.pause`: new delegations are refused while set.
    paused: AtomicBool,
}

/// What `register` needs about a child.
pub(crate) struct NewChild {
    pub subagent_id: String,
    pub session_id: String,
    pub goal: String,
    pub delegation_id: String,
    pub model: String,
    pub cancel: CancellationToken,
    pub state: SharedTurnState,
}

impl Subagents {
    pub(crate) fn register(&self, child: NewChild) {
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        self.live.lock().insert(
            child.subagent_id,
            Live {
                session_id: child.session_id,
                goal: child.goal,
                delegation_id: child.delegation_id,
                model: child.model,
                started_at,
                tool_count: 0,
                last_tool: None,
                cancel: child.cancel,
                state: child.state,
                tail: String::new(),
                tail_truncated: false,
            },
        );
    }

    /// A tool call the child started.
    pub(crate) fn tool_started(&self, subagent_id: &str, tool: &str) -> u64 {
        let mut live = self.live.lock();
        let Some(child) = live.get_mut(subagent_id) else {
            return 0;
        };
        child.tool_count += 1;
        child.last_tool = Some(tool.to_string());
        child.tool_count
    }

    /// Output for `subagent.tail` (the newest `TAIL_CHARS`).
    pub(crate) fn append_tail(&self, subagent_id: &str, text: &str) {
        let mut live = self.live.lock();
        let Some(child) = live.get_mut(subagent_id) else {
            return;
        };
        child.tail.push_str(text);
        let over = child.tail.len().saturating_sub(TAIL_CHARS);
        if over > 0 {
            let mut cut = over;
            while !child.tail.is_char_boundary(cut) {
                cut += 1;
            }
            child.tail.drain(..cut);
            child.tail_truncated = true;
        }
    }

    pub(crate) fn finish(&self, subagent_id: &str) {
        self.live.lock().remove(subagent_id);
    }

    fn snapshot(id: &str, child: &Live) -> Value {
        json!({
            "subagent_id": id,
            "parent_id": null,
            "depth": 1,
            "goal": child.goal,
            "delegation_id": child.delegation_id,
            "model": child.model,
            "started_at": child.started_at,
            "status": if child.cancel.is_cancelled() { "interrupted" } else { "running" },
            "tool_count": child.tool_count,
            "last_tool": child.last_tool,
            "accepting_steer": !child.cancel.is_cancelled(),
        })
    }

    /// `subagent.list`: the session's running children (`SubagentSnapshot`s).
    pub fn list(&self, session_id: &str) -> Vec<Value> {
        let live = self.live.lock();
        let mut rows: Vec<(f64, Value)> = live
            .iter()
            .filter(|(_, child)| child.session_id == session_id)
            .map(|(id, child)| (child.started_at, Self::snapshot(id, child)))
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.into_iter().map(|(_, row)| row).collect()
    }

    /// `delegation.status`'s `active`: every running child, with its session.
    pub fn active(&self) -> Vec<Value> {
        self.live
            .lock()
            .iter()
            .map(|(id, child)| {
                let mut row = Self::snapshot(id, child);
                row["owner_agent_session_id"] = json!(child.session_id);
                row
            })
            .collect()
    }

    /// `subagent.tail`: the child's recent output.
    pub fn tail(&self, session_id: &str, subagent_id: &str) -> Value {
        let live = self.live.lock();
        match live.get(subagent_id).filter(|c| c.session_id == session_id) {
            Some(child) => json!({"subagent_id": subagent_id, "available": true,
                                  "text": child.tail, "truncated": child.tail_truncated}),
            None => {
                json!({"subagent_id": subagent_id, "available": false, "text": "", "truncated": false})
            }
        }
    }

    /// `subagent.steer`: a message for the child, delivered after its next tool results.
    /// `false` when it isn't running.
    pub fn steer(&self, session_id: &str, subagent_id: &str, text: &str) -> bool {
        let live = self.live.lock();
        let Some(child) = live
            .get(subagent_id)
            .filter(|c| c.session_id == session_id && !c.cancel.is_cancelled())
        else {
            return false;
        };
        let mut state = child.state.lock();
        state.pending_steer = Some(match state.pending_steer.take() {
            Some(earlier) => format!("{earlier}\n\n{text}"),
            None => text.to_string(),
        });
        true
    }

    /// `subagent.interrupt`: stops the child (the parent's turn goes on with its result).
    pub fn interrupt(&self, session_id: &str, subagent_id: &str) -> bool {
        let live = self.live.lock();
        match live.get(subagent_id).filter(|c| c.session_id == session_id) {
            Some(child) => {
                child.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(id: &str, session: &str) -> NewChild {
        NewChild {
            subagent_id: id.into(),
            session_id: session.into(),
            goal: "look around".into(),
            delegation_id: "dg-1".into(),
            model: "m".into(),
            cancel: CancellationToken::new(),
            state: SharedTurnState::default(),
        }
    }

    #[test]
    fn children_are_listed_steered_tailed_and_stopped_per_session() {
        let registry = Subagents::default();
        registry.register(child("sa-0", "s1"));
        registry.register(child("sa-1", "s2"));
        assert_eq!(registry.list("s1").len(), 1);
        assert_eq!(registry.tool_started("sa-0", "read_file"), 1);
        assert_eq!(registry.list("s1")[0]["last_tool"], "read_file");

        registry.append_tail("sa-0", "hello");
        assert_eq!(registry.tail("s1", "sa-0")["text"], "hello");
        assert_eq!(
            registry.tail("s2", "sa-0")["available"],
            false,
            "another session's child"
        );

        assert!(registry.steer("s1", "sa-0", "focus on tests"));
        assert!(registry.steer("s1", "sa-0", "and docs"));
        let state = registry.live.lock().get("sa-0").unwrap().state.clone();
        assert_eq!(
            state.lock().pending_steer.as_deref(),
            Some("focus on tests\n\nand docs")
        );

        assert!(registry.interrupt("s1", "sa-0"));
        assert_eq!(registry.list("s1")[0]["status"], "interrupted");
        assert!(!registry.steer("s1", "sa-0", "too late"));
        registry.finish("sa-0");
        assert!(registry.list("s1").is_empty());
        assert!(!registry.interrupt("s1", "sa-0"));
        assert_eq!(registry.active().len(), 1);
    }

    #[test]
    fn the_tail_keeps_the_newest_output() {
        let registry = Subagents::default();
        registry.register(child("sa-0", "s1"));
        registry.append_tail("sa-0", &"é".repeat(TAIL_CHARS));
        registry.append_tail("sa-0", "end");
        let tail = registry.tail("s1", "sa-0");
        assert_eq!(tail["truncated"], true);
        assert!(tail["text"].as_str().unwrap().ends_with("end"));
        assert!(tail["text"].as_str().unwrap().len() <= TAIL_CHARS);
    }
}
