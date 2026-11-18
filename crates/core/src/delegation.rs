//! A session's background processes (the `process` tool's list: list, kill, stop all), its
//! subagents (`delegate_task` children: list, tail, interrupt, steer) and the delegation switch.
//! The rows of both lists are the runtime's own (the process tool's and Hermes's
//! `SubagentEventPayload`), passed through.
//!
//! Hermes: `tui_gateway/server.py` (`process.*`, `subagent.*`, `delegation.*`).

use serde_json::Value;

use crate::Core;

/// What stopping one background process did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// No such process in the session.
    NotFound,
    /// It had already ended.
    AlreadyExited { exit_code: Option<i64> },
    /// Stopped now.
    Killed {
        command: Option<String>,
        exit_code: Option<i64>,
    },
}

/// The delegation switch and what runs under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Delegation {
    /// The running subagents, every session's.
    pub active: Vec<Value>,
    pub paused: bool,
    /// What a turn started now would allow (the budget profile's, with overrides).
    pub max_concurrent_children: usize,
}

impl Core {
    /// The session's background processes (the `process` tool's rows).
    pub fn processes(&self, session_id: &str) -> Vec<Value> {
        self.runtime.processes(session_id)
    }

    /// Stops one background process of the session.
    pub async fn kill_process(&self, session_id: &str, process_id: &str) -> KillOutcome {
        let before = self
            .runtime
            .processes(session_id)
            .into_iter()
            .find(|p| p["session_id"] == process_id);
        let Some(before) = before else {
            return KillOutcome::NotFound;
        };
        if before["status"] != "running" {
            return KillOutcome::AlreadyExited {
                exit_code: before["exit_code"].as_i64(),
            };
        }
        let after = self
            .runtime
            .kill_process(session_id, process_id)
            .await
            .unwrap_or_default();
        KillOutcome::Killed {
            command: before["command"].as_str().map(str::to_string),
            exit_code: after["exit_code"].as_i64(),
        }
    }

    /// Stops every background process of the session; how many.
    pub async fn stop_processes(&self, session_id: &str) -> usize {
        self.runtime.stop_processes(session_id).await
    }

    /// The session's running subagents (`delegate_task` children).
    pub fn subagents(&self, session_id: &str) -> Vec<Value> {
        self.runtime.subagents().list(session_id)
    }

    /// A subagent's recent activity.
    pub fn subagent_tail(&self, session_id: &str, subagent_id: &str) -> Value {
        self.runtime.subagents().tail(session_id, subagent_id)
    }

    /// Stops a subagent; whether it was found.
    pub fn interrupt_subagent(&self, session_id: &str, subagent_id: &str) -> bool {
        self.runtime.subagents().interrupt(session_id, subagent_id)
    }

    /// A message for a subagent's next tool results (Hermes's `SteerStatus`): `None` for an empty
    /// text, else whether it was queued.
    pub fn steer_subagent(&self, session_id: &str, subagent_id: &str, text: &str) -> Option<bool> {
        if text.is_empty() {
            return None;
        }
        Some(
            self.runtime
                .subagents()
                .steer(session_id, subagent_id, text),
        )
    }

    /// The delegation switch and the running subagents.
    pub fn delegation(&self) -> Delegation {
        let subagents = self.runtime.subagents();
        Delegation {
            active: subagents.active(),
            paused: subagents.paused(),
            max_concurrent_children: self.runtime.budget().delegate_max_children,
        }
    }

    /// Pauses or resumes new delegations.
    pub fn pause_delegation(&self, paused: bool) {
        self.runtime.subagents().set_paused(paused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_process_or_subagent_is_not_found() {
        let data = hermes_testdir::tempdir().unwrap();
        let config = hermes_runtime::RuntimeConfig {
            data_dir: data.path().to_path_buf(),
            ..hermes_runtime::RuntimeConfig::for_current_user().unwrap()
        };
        let core = Core::new(config, tokio::runtime::Handle::current()).unwrap();
        assert_eq!(core.kill_process("s", "p").await, KillOutcome::NotFound);
        assert_eq!(core.stop_processes("s").await, 0);
        assert!(core.processes("s").is_empty() && core.subagents("s").is_empty());
        assert!(!core.interrupt_subagent("s", "c"));
        assert_eq!(core.steer_subagent("s", "c", ""), None);
        assert_eq!(core.steer_subagent("s", "c", "go"), Some(false));
        core.pause_delegation(true);
        assert!(core.delegation().paused);
        core.pause_delegation(false);
        assert!(!core.delegation().paused);
        drop(core);
    }
}
