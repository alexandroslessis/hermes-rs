//! A session's background processes (the `process` tool's list), its subagents (`delegate_task`
//! children: the page's rows follow their `subagent.*` events and reconcile against these calls)
//! and the delegation switch. Hermes: `tui_gateway/server.py` (`process.*`, `subagent.*`,
//! `delegation.*`).

use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{
    DelegationPause, ProcessKill, Reply, RpcError, SessionRef, SubagentRef, SubagentSteer,
};

impl Gateway {
    /// The session's background processes (the `process` tool's list).
    pub(super) fn process_list(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        let processes: Vec<Value> = self
            .runtime
            .processes(&id)
            .into_iter()
            .map(|mut process| {
                process["uptime_seconds"] = process["running_seconds"].clone();
                process
            })
            .collect();
        Ok(json!({"processes": processes}))
    }

    pub(super) async fn process_kill(self: &Arc<Self>, p: ProcessKill) -> Reply {
        let id = p.session.id()?;
        let process_id = p.process_id.as_str();
        let before = self
            .runtime
            .processes(&id)
            .into_iter()
            .find(|p| p["session_id"] == process_id);
        let Some(before) = before else {
            return Ok(json!({"status": "not_found", "session_id": process_id}));
        };
        if before["status"] != "running" {
            return Ok(
                json!({"status": "already_exited", "session_id": process_id, "exit_code": before["exit_code"]}),
            );
        }
        let after = self
            .runtime
            .kill_process(&id, process_id)
            .await
            .unwrap_or_default();
        Ok(json!({
            "status": "killed",
            "session_id": process_id,
            "command": before["command"],
            "exit_code": after["exit_code"],
            "termination_source": "user",
        }))
    }

    pub(super) async fn process_stop(self: &Arc<Self>, p: SessionRef) -> Reply {
        let id = p.id()?;
        Ok(json!({"killed": self.runtime.stop_processes(&id).await}))
    }

    /// The session's running subagents (`delegate_task` children).
    pub(super) fn subagent_list(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        Ok(json!({"subagents": self.runtime.subagents().list(&id), "delegations": []}))
    }

    pub(super) fn subagent_tail(&self, p: SubagentRef) -> Reply {
        let id = p.session.id()?;
        Ok(self.runtime.subagents().tail(&id, &p.subagent_id))
    }

    pub(super) fn subagent_interrupt(&self, p: SubagentRef) -> Reply {
        let id = p.session.id()?;
        let child = p.subagent_id;
        let subagents = self.runtime.subagents();
        Ok(json!({"found": subagents.interrupt(&id, &child), "subagent_id": child}))
    }

    pub(super) fn subagent_steer(&self, p: SubagentSteer) -> Reply {
        let id = p.session.id()?;
        let child = p.subagent_id;
        let subagents = self.runtime.subagents();
        let text = p.text;
        if text.is_empty() {
            return Err(RpcError::invalid("text is required"));
        }
        // (Hermes's `SteerStatus`: queued for the child's next tool results.)
        let status = if subagents.steer(&id, &child, &text) {
            "queued"
        } else {
            "rejected"
        };
        Ok(json!({"status": status, "subagent_id": child, "text": text}))
    }

    pub(super) fn delegation_status(&self) -> Reply {
        Ok(json!({
            "active": self.runtime.subagents().active(),
            "paused": self.runtime.subagents().paused(),
            "max_spawn_depth": 1,
            // What a turn started now would allow (the budget profile's, with overrides).
            "max_concurrent_children": self.runtime.budget().delegate_max_children,
        }))
    }

    pub(super) fn delegation_pause(&self, p: DelegationPause) -> Reply {
        self.runtime.subagents().set_paused(p.paused);
        Ok(json!({"paused": p.paused}))
    }
}
