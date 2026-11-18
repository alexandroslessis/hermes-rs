//! A session's background processes (the `process` tool's list), its subagents (`delegate_task`
//! children: the page's rows follow their `subagent.*` events and reconcile against these calls)
//! and the delegation switch, in Hermes's JSON; the logic is `hermes_core::delegation`.
//! Hermes: `tui_gateway/server.py` (`process.*`, `subagent.*`, `delegation.*`).

use std::sync::Arc;

use hermes_core::delegation::KillOutcome;
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
            .core
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
        Ok(match self.core.kill_process(&id, process_id).await {
            KillOutcome::NotFound => json!({"status": "not_found", "session_id": process_id}),
            KillOutcome::AlreadyExited { exit_code } => {
                json!({"status": "already_exited", "session_id": process_id, "exit_code": exit_code})
            }
            KillOutcome::Killed { command, exit_code } => json!({
                "status": "killed",
                "session_id": process_id,
                "command": command,
                "exit_code": exit_code,
                "termination_source": "user",
            }),
        })
    }

    pub(super) async fn process_stop(self: &Arc<Self>, p: SessionRef) -> Reply {
        let id = p.id()?;
        Ok(json!({"killed": self.core.stop_processes(&id).await}))
    }

    /// The session's running subagents (`delegate_task` children).
    pub(super) fn subagent_list(&self, p: SessionRef) -> Reply {
        let id = p.id()?;
        Ok(json!({"subagents": self.core.subagents(&id), "delegations": []}))
    }

    pub(super) fn subagent_tail(&self, p: SubagentRef) -> Reply {
        let id = p.session.id()?;
        Ok(self.core.subagent_tail(&id, &p.subagent_id))
    }

    pub(super) fn subagent_interrupt(&self, p: SubagentRef) -> Reply {
        let id = p.session.id()?;
        let child = p.subagent_id;
        let found = self.core.interrupt_subagent(&id, &child);
        Ok(json!({"found": found, "subagent_id": child}))
    }

    pub(super) fn subagent_steer(&self, p: SubagentSteer) -> Reply {
        let id = p.session.id()?;
        let (child, text) = (p.subagent_id, p.text);
        // (Hermes's `SteerStatus`: queued for the child's next tool results.)
        let status = match self.core.steer_subagent(&id, &child, &text) {
            None => return Err(RpcError::invalid("text is required")),
            Some(true) => "queued",
            Some(false) => "rejected",
        };
        Ok(json!({"status": status, "subagent_id": child, "text": text}))
    }

    pub(super) fn delegation_status(&self) -> Reply {
        let delegation = self.core.delegation();
        Ok(json!({
            "active": delegation.active,
            "paused": delegation.paused,
            "max_spawn_depth": 1,
            "max_concurrent_children": delegation.max_concurrent_children,
        }))
    }

    pub(super) fn delegation_pause(&self, p: DelegationPause) -> Reply {
        self.core.pause_delegation(p.paused);
        Ok(json!({"paused": p.paused}))
    }
}
