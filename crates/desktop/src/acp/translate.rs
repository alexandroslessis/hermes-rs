//! The gateway's page events for one prompt turn, as ACP `session/update`s. Pure: no IO, no
//! channels. [`Turn::event`] takes one event and returns what to do about it; `server.rs` does it
//! (sending the updates, asking the editor, answering the prompt).
//!
//! The events are the ones `gateway.rs` `submit` emits for the page: `message.delta`,
//! `reasoning.delta`, `tool.start`, `tool.complete`, `approval.request`, `clarify.request`,
//! `status.update`, `message.complete`.
//!
//! Hermes: `acp_adapter/gateway_server.py` (`_project`), `gateway_reply.py` (`_project_tool`,
//! `_finish_reply`, `_chunk`, `_project_side_update`), `events.py` (`flush_open_tool_calls`,
//! `AssistantMessageIdAllocator`).

use std::collections::HashMap;

use serde_json::{Value, json};

use super::rpc::RpcError;
use super::tools;

/// What the server does about an event.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// A `session/update`'s `update` object.
    Update(Value),
    /// The agent wants a dangerous command approved.
    Approval {
        prompt_id: String,
        command: String,
        description: String,
        choices: Vec<String>,
    },
    /// The agent asks the user a question.
    Clarify {
        prompt_id: String,
        question: String,
        choices: Vec<String>,
        multi_select: bool,
    },
    /// A file write is starting: read its text now (the "before" of the diff) and hand it back
    /// through [`Turn::set_before`].
    Snapshot { tool_id: String, path: String },
    /// The turn is over: the prompt's answer.
    Finish(Result<Value, RpcError>),
}

/// One prompt turn's translation state.
pub struct Turn {
    cwd: String,
    /// The reply bubble being written; a new one opens after a tool call.
    message_id: Option<String>,
    thought_id: Option<String>,
    /// Calls started and not completed: id → (name, args).
    open: Vec<(String, String, Value)>,
    /// A `write_file`'s text before it ran.
    before: HashMap<String, Option<String>>,
    /// Whether any reply text was streamed (else the completion's text is sent).
    streamed: bool,
}

fn chunk(kind: &str, id: &str, text: &str) -> Action {
    Action::Update(json!({
        "sessionUpdate": kind,
        "content": {"type": "text", "text": text},
        "messageId": id,
    }))
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl Turn {
    pub fn new(cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
            message_id: None,
            thought_id: None,
            open: Vec::new(),
            before: HashMap::new(),
            streamed: false,
        }
    }

    /// The text a `write_file` call found on disk when it started (`None`: no such file).
    pub fn set_before(&mut self, tool_id: &str, text: Option<String>) {
        self.before.insert(tool_id.to_string(), text);
    }

    /// The card of a call that never finished (the turn ended first, or a guard stopped it).
    fn abandoned(id: &str) -> Action {
        Action::Update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": "failed",
        }))
    }

    pub fn event(&mut self, kind: &str, payload: &Value) -> Vec<Action> {
        match kind {
            "message.start" => {
                self.message_id = None;
                self.thought_id = None;
                Vec::new()
            }
            "message.delta" => {
                let text = payload["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return Vec::new();
                }
                self.streamed = true;
                let id = self.message_id.get_or_insert_with(new_id).clone();
                vec![chunk("agent_message_chunk", &id, text)]
            }
            "reasoning.delta" => {
                let text = payload["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return Vec::new();
                }
                let id = self.thought_id.get_or_insert_with(new_id).clone();
                vec![chunk("agent_thought_chunk", &id, text)]
            }
            // Compression progress: Zed has no status line, so it reads as a thought (Hermes does
            // the same).
            "status.update" => {
                let text = payload["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return Vec::new();
                }
                self.thought_id = None;
                vec![chunk(
                    "agent_thought_chunk",
                    &new_id(),
                    &format!("{text}\n"),
                )]
            }
            "tool.start" => self.tool_start(payload),
            "tool.complete" => self.tool_complete(payload),
            "approval.request" => vec![Action::Approval {
                prompt_id: payload["prompt_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                command: payload["command"].as_str().unwrap_or_default().to_string(),
                description: payload["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                choices: string_list(&payload["choices"]),
            }],
            "clarify.request" => vec![Action::Clarify {
                prompt_id: payload["prompt_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                question: payload["question"].as_str().unwrap_or_default().to_string(),
                choices: string_list(&payload["choices"]),
                multi_select: payload["multi_select"] == true,
            }],
            "message.complete" => self.complete(payload),
            _ => Vec::new(),
        }
    }

    fn tool_start(&mut self, payload: &Value) -> Vec<Action> {
        let id = payload["tool_id"].as_str().unwrap_or_default().to_string();
        let name = payload["name"].as_str().unwrap_or_default().to_string();
        let args = payload.get("args").cloned().unwrap_or(Value::Null);
        // The next text is a new message, after the card.
        self.message_id = None;
        self.thought_id = None;
        let mut update = json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": tools::title(&name, &args),
            "kind": tools::kind(&name),
            "status": "in_progress",
            "rawInput": args,
        });
        let locations = tools::locations(&args, &self.cwd);
        if !locations.is_empty() {
            update["locations"] = json!(locations);
        }
        let mut actions = vec![Action::Update(update)];
        if let Some(path) = tools::snapshot_path(&name, &args, &self.cwd) {
            actions.push(Action::Snapshot {
                tool_id: id.clone(),
                path,
            });
        }
        self.open.push((id, name, args));
        actions
    }

    fn tool_complete(&mut self, payload: &Value) -> Vec<Action> {
        let id = payload["tool_id"].as_str().unwrap_or_default().to_string();
        let position = self.open.iter().position(|(open, _, _)| *open == id);
        let (name, args) = match position {
            Some(at) => {
                let (_, name, args) = self.open.remove(at);
                (name, args)
            }
            // A completion for a call we never saw start: show it as a finished card.
            None => (
                payload["name"].as_str().unwrap_or_default().to_string(),
                Value::Null,
            ),
        };
        let result = payload.get("result").cloned().unwrap_or(Value::Null);
        let raw = payload["result_text"].as_str().unwrap_or_default();
        let failed = tools::failed(&result);
        let before = self.before.remove(&id).flatten();
        let content =
            tools::complete_content(&name, &args, &result, raw, &self.cwd, before.as_deref());
        let raw_output = match &result {
            Value::Object(_) => result.clone(),
            other => json!({"output": other.as_str().unwrap_or(raw)}),
        };
        let mut update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if failed { "failed" } else { "completed" },
            "rawOutput": raw_output,
        });
        if !content.is_empty() {
            update["content"] = json!(content);
        }
        let mut actions = vec![Action::Update(update)];
        if name == "todo_list" && !failed {
            if let Some(plan) = tools::plan_from_todo(&result) {
                actions.push(Action::Update(plan));
            }
        }
        actions
    }

    fn complete(&mut self, payload: &Value) -> Vec<Action> {
        let mut actions: Vec<Action> = self
            .open
            .drain(..)
            .map(|(id, _, _)| Self::abandoned(&id))
            .collect();
        let status = payload["status"].as_str().unwrap_or("complete");
        let text = payload["text"].as_str().unwrap_or_default();
        // A reply that never streamed (a command's output, a failed turn's account) is shown now.
        if status != "error" && !self.streamed && !text.is_empty() {
            let id = self.message_id.get_or_insert_with(new_id).clone();
            actions.push(chunk("agent_message_chunk", &id, text));
        }
        // The editor's context meter: tokens in the last request against the model's window.
        let usage = &payload["usage"];
        if let (Some(used), Some(size)) = (
            usage["context_used"].as_u64(),
            usage["context_max"].as_u64(),
        ) {
            actions.push(Action::Update(
                json!({"sessionUpdate": "usage_update", "used": used, "size": size}),
            ));
        }
        actions.push(Action::Finish(match status {
            "interrupted" => Ok(json!({"stopReason": "cancelled"})),
            "error" => {
                let error = payload["error"].as_str().filter(|e| !e.is_empty());
                let message = error.or(Some(text).filter(|t| !t.is_empty()));
                Err(RpcError::internal(message.unwrap_or("The turn failed.")))
            }
            _ => Ok(json!({"stopReason": "end_turn"})),
        }));
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn updates(actions: &[Action]) -> Vec<&Value> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Update(update) => Some(update),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_reply_streams_as_chunks_of_one_message() {
        let mut turn = Turn::new("C:/w");
        let a = turn.event("message.delta", &json!({"text": "Hel"}));
        let b = turn.event("message.delta", &json!({"text": "lo"}));
        let (a, b) = (updates(&a)[0].clone(), updates(&b)[0].clone());
        assert_eq!(a["sessionUpdate"], "agent_message_chunk");
        assert_eq!(a["content"]["text"], "Hel");
        assert_eq!(a["messageId"], b["messageId"]);
    }

    #[test]
    fn empty_deltas_send_nothing() {
        let mut turn = Turn::new("C:/w");
        assert!(turn.event("message.delta", &json!({"text": ""})).is_empty());
        assert!(turn.event("reasoning.delta", &json!({})).is_empty());
    }

    #[test]
    fn thinking_is_a_thought_chunk() {
        let mut turn = Turn::new("C:/w");
        let actions = turn.event("reasoning.delta", &json!({"text": "hmm"}));
        assert_eq!(updates(&actions)[0]["sessionUpdate"], "agent_thought_chunk");
    }

    #[test]
    fn text_after_a_tool_call_is_a_new_message() {
        let mut turn = Turn::new("C:/w");
        let first = turn.event("message.delta", &json!({"text": "before"}));
        turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "terminal", "args": {"command": "ls"}}),
        );
        let second = turn.event("message.delta", &json!({"text": "after"}));
        assert_ne!(
            updates(&first)[0]["messageId"],
            updates(&second)[0]["messageId"]
        );
    }

    #[test]
    fn a_tool_call_starts_and_completes_by_id() {
        let mut turn = Turn::new("C:/w");
        let start = turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "read_file", "args": {"path": "a.rs"}}),
        );
        let call = updates(&start)[0];
        assert_eq!(call["sessionUpdate"], "tool_call");
        assert_eq!(call["toolCallId"], "t1");
        assert_eq!(call["kind"], "read");
        assert_eq!(call["status"], "in_progress");
        assert_eq!(call["title"], "Read a.rs");
        assert_eq!(call["locations"][0]["path"], "C:\\w\\a.rs");

        let done = turn.event(
            "tool.complete",
            &json!({"tool_id": "t1", "name": "read_file",
                    "result": {"content": "x"}, "result_text": "{\"content\":\"x\"}"}),
        );
        let update = updates(&done)[0];
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["status"], "completed");
        assert_eq!(update["rawOutput"]["content"], "x");
    }

    #[test]
    fn a_failed_tool_is_marked_failed() {
        let mut turn = Turn::new("C:/w");
        turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "patch", "args": {"path": "a"}}),
        );
        let done = turn.event(
            "tool.complete",
            &json!({"tool_id": "t1", "name": "patch",
                    "result": {"success": false, "error": "no match"}, "result_text": ""}),
        );
        assert_eq!(updates(&done)[0]["status"], "failed");
    }

    #[test]
    fn a_file_write_asks_for_its_before_and_shows_a_diff() {
        let mut turn = Turn::new("C:/w");
        let start = turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "write_file",
                    "args": {"path": "a.txt", "content": "new"}}),
        );
        assert!(start.contains(&Action::Snapshot {
            tool_id: "t1".into(),
            path: "C:\\w\\a.txt".into()
        }));
        turn.set_before("t1", Some("old".into()));
        let done = turn.event(
            "tool.complete",
            &json!({"tool_id": "t1", "name": "write_file", "result": {"success": true}, "result_text": ""}),
        );
        let diff = &updates(&done)[0]["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["oldText"], "old");
        assert_eq!(diff["newText"], "new");
    }

    #[test]
    fn the_todo_list_also_sends_the_plan() {
        let mut turn = Turn::new("C:/w");
        turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "todo_list", "args": {}}),
        );
        let done = turn.event(
            "tool.complete",
            &json!({"tool_id": "t1", "name": "todo_list",
                    "result": {"todos": [{"id": "1", "content": "a", "status": "pending"}]}, "result_text": ""}),
        );
        let all = updates(&done);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1]["sessionUpdate"], "plan");
    }

    #[test]
    fn approvals_and_questions_become_asks() {
        let mut turn = Turn::new("C:/w");
        let approval = turn.event(
            "approval.request",
            &json!({"prompt_id": "p1", "command": "rm -rf x", "description": "recursive delete",
                    "choices": ["once", "session", "always", "deny"]}),
        );
        assert_eq!(
            approval,
            vec![Action::Approval {
                prompt_id: "p1".into(),
                command: "rm -rf x".into(),
                description: "recursive delete".into(),
                choices: vec![
                    "once".into(),
                    "session".into(),
                    "always".into(),
                    "deny".into()
                ],
            }]
        );
        let question = turn.event(
            "clarify.request",
            &json!({"prompt_id": "p2", "question": "Which?", "choices": ["a", "b"], "multi_select": true}),
        );
        assert_eq!(
            question,
            vec![Action::Clarify {
                prompt_id: "p2".into(),
                question: "Which?".into(),
                choices: vec!["a".into(), "b".into()],
                multi_select: true,
            }]
        );
    }

    #[test]
    fn the_end_of_a_turn_answers_the_prompt() {
        let mut turn = Turn::new("C:/w");
        turn.event("message.delta", &json!({"text": "hi"}));
        let done = turn.event(
            "message.complete",
            &json!({"text": "hi", "status": "complete"}),
        );
        assert_eq!(
            done,
            vec![Action::Finish(Ok(json!({"stopReason": "end_turn"})))]
        );

        let mut turn = Turn::new("C:/w");
        let cancelled = turn.event(
            "message.complete",
            &json!({"text": "", "status": "interrupted"}),
        );
        assert_eq!(
            cancelled,
            vec![Action::Finish(Ok(json!({"stopReason": "cancelled"})))]
        );

        let mut turn = Turn::new("C:/w");
        let failed = turn.event(
            "message.complete",
            &json!({"text": "account", "status": "error", "error": "provider down"}),
        );
        assert_eq!(
            failed,
            vec![Action::Finish(Err(RpcError::internal("provider down")))]
        );
    }

    #[test]
    fn calls_left_open_are_failed_before_the_answer() {
        let mut turn = Turn::new("C:/w");
        turn.event(
            "tool.start",
            &json!({"tool_id": "t1", "name": "terminal", "args": {"command": "x"}}),
        );
        let done = turn.event(
            "message.complete",
            &json!({"text": "", "status": "interrupted"}),
        );
        assert_eq!(updates(&done)[0]["toolCallId"], "t1");
        assert_eq!(updates(&done)[0]["status"], "failed");
        assert!(matches!(done.last(), Some(Action::Finish(_))));
    }

    #[test]
    fn a_reply_that_never_streamed_is_sent_at_the_end() {
        let mut turn = Turn::new("C:/w");
        let done = turn.event(
            "message.complete",
            &json!({"text": "Status: ok", "status": "complete"}),
        );
        let all = updates(&done);
        assert_eq!(all[0]["content"]["text"], "Status: ok");
        assert!(matches!(done.last(), Some(Action::Finish(Ok(_)))));
    }

    #[test]
    fn the_context_meter_is_updated_before_the_answer() {
        let mut turn = Turn::new("C:/w");
        let done = turn.event(
            "message.complete",
            &json!({"text": "", "status": "complete",
                    "usage": {"context_used": 1200, "context_max": 32768}}),
        );
        let meter = updates(&done)[0];
        assert_eq!(meter["sessionUpdate"], "usage_update");
        assert_eq!(meter["used"], 1200);
        assert_eq!(meter["size"], 32768);
        assert!(matches!(done.last(), Some(Action::Finish(_))));
    }

    #[test]
    fn compression_progress_reads_as_a_thought() {
        let mut turn = Turn::new("C:/w");
        let actions = turn.event(
            "status.update",
            &json!({"kind": "compacting", "text": "Compacting"}),
        );
        let update = updates(&actions)[0];
        assert_eq!(update["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(update["content"]["text"], "Compacting\n");
    }

    #[test]
    fn other_events_are_ignored() {
        let mut turn = Turn::new("C:/w");
        for kind in [
            "session.info",
            "session.usage",
            "tool.progress",
            "subagent.start",
        ] {
            assert!(turn.event(kind, &json!({})).is_empty(), "{kind}");
        }
    }
}
