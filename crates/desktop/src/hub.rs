//! Which socket each session's events go to, and the page's JSON for the core's events. Every
//! event the page sees goes out here: the gateway's own (`emit`) and the core's (`CoreEvent`s,
//! translated by the [`Listener`] impl into Hermes's frames: `message.delta`, `tool.start`, the
//! canonical prompt events `approval.request` / `clarify.request` and their `*.settled`, ...). The
//! questions themselves (waiting for an answer, `approval.respond` / `clarify.respond`) are the
//! core's (`hermes_core::questions`).
//!
//! Hermes: `tui_gateway/server.py` (`_emit`), `apps/desktop/src/api/canonical-protocol.ts` (what the
//! page reads from the prompt events).

use std::collections::HashMap;
use std::sync::Mutex;

use hermes_core::{Ask, CoreEvent, Listener, OpenQuestion, QuestionKind};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Where a connection's outgoing frames go.
pub type Outbox = mpsc::UnboundedSender<String>;

/// Hermes's approval choices, in its order.
pub use hermes_core::APPROVAL_CHOICES;

#[derive(Default)]
pub struct Hub {
    /// Session id → the socket that last created, resumed or prompted it.
    subscribers: Mutex<HashMap<String, Outbox>>,
}

impl Hub {
    pub fn subscribe(&self, session_id: &str, outbox: &Outbox) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.insert(session_id.to_string(), outbox.clone());
        }
    }

    /// The sessions a page is following.
    pub fn followed(&self) -> Vec<String> {
        self.subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .iter()
                    .filter(|(_, outbox)| !outbox.is_closed())
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn outbox(&self, session_id: &str) -> Option<Outbox> {
        let subscribers = self.subscribers.lock().ok()?;
        subscribers
            .get(session_id)
            .filter(|outbox| !outbox.is_closed())
            .cloned()
    }

    /// Sends a session event to the socket following that session. `false` when none is.
    pub fn emit(&self, session_id: &str, event: &str, payload: Value) -> bool {
        // The wire log (`RUST_LOG=hermes_desktop::wire=debug`): every event but the streamed
        // text, with the turn state the page acts on.
        if !matches!(event, "message.delta" | "reasoning.delta") {
            tracing::debug!(
                target: "hermes_desktop::wire",
                "→ {event} {session_id} running={} status={}",
                payload["running"],
                payload["status"],
            );
        }
        let Some(outbox) = self.outbox(session_id) else {
            return false;
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "event",
            // The profile the page treats as this backend's (what an unnamed one means anyway);
            // some updates, like the approval mode, apply only from an event that names it.
            "params": {"type": event, "session_id": session_id, "profile": "default", "payload": payload},
        });
        outbox.send(frame.to_string()).is_ok()
    }
}

/// A question as the page's card payload (`approval.request` / `clarify.request`, and
/// `session.resume`'s `prompts`).
pub fn question_payload(prompt_id: &str, ask: &Ask) -> Value {
    let mut payload = match ask {
        Ask::Approval {
            command,
            description,
            choices,
        } => json!({
            "command": command,
            "description": description,
            "choices": choices,
        }),
        Ask::Clarify {
            question,
            choices,
            multi_select,
        } => json!({
            "question": question,
            "choices": choices,
            "multi_select": multi_select,
        }),
    };
    payload["prompt_id"] = Value::String(prompt_id.to_string());
    payload["kind"] = Value::String(ask.kind().as_str().to_string());
    // The page answers a card only while its generation is the session's.
    payload["execution_generation"] = json!(crate::gateway::EXECUTION_GENERATION);
    payload
}

/// A session's open questions as the page's card payloads.
pub fn open_payloads(open: &[OpenQuestion]) -> Vec<Value> {
    open.iter()
        .map(|q| question_payload(&q.prompt_id, &q.ask))
        .collect()
}

impl Listener for Hub {
    /// The core's events as Hermes's frames. Usage and the end of a turn are the gateway's own
    /// frames (`session.usage`, `message.complete`), built from more than the event; background
    /// processes and the review's result have no page event (they are logged).
    fn deliver(&self, event: &CoreEvent) -> bool {
        let id = event.session_id();
        match event {
            CoreEvent::TextDelta { text, .. } => {
                self.emit(id, "message.delta", json!({"text": text}))
            }
            CoreEvent::ReasoningDelta { text, .. } => {
                self.emit(id, "reasoning.delta", json!({"text": text}))
            }
            CoreEvent::ToolStarted {
                tool_id,
                name,
                args,
                ..
            } => self.emit(
                id,
                "tool.start",
                json!({"tool_id": tool_id, "name": name, "args": args}),
            ),
            // The running row shows the message beside its timer, and the clicked-open row
            // renders it as its output while the call works (patches/0017). The payload is the
            // contract's ToolProgressPayload.
            CoreEvent::ToolProgress {
                tool_id,
                name,
                progress,
                message,
                ..
            } => self.emit(
                id,
                "tool.progress",
                json!({
                    "tool_id": tool_id,
                    "name": name,
                    "progress": progress,
                    "total": Value::Null,
                    "percent": Value::Null,
                    "message": message,
                }),
            ),
            CoreEvent::ToolCompleted {
                tool_id,
                name,
                text,
                ..
            } => {
                // Hermes's tools return JSON; the card reads `result` as the value.
                let result = serde_json::from_str::<Value>(text)
                    .unwrap_or_else(|_| Value::String(text.clone()));
                self.emit(
                    id,
                    "tool.complete",
                    json!({
                        "tool_id": tool_id,
                        "name": name,
                        "result": result,
                        "result_text": text,
                    }),
                )
            }
            // The page keeps `fallback` ones as a system line; `compacting` / `compacted` drive
            // its "summarizing" indicator.
            CoreEvent::Status { kind, text, .. } => {
                self.emit(id, "status.update", json!({"kind": kind, "text": text}))
            }
            CoreEvent::TitleChanged { title, .. } => self.emit(
                id,
                "session.title",
                json!({"session_id": id, "title": title}),
            ),
            CoreEvent::ApprovalRequested {
                prompt_id,
                command,
                description,
                choices,
                ..
            } => {
                let ask = Ask::Approval {
                    command: command.clone(),
                    description: description.clone(),
                    choices: choices.clone(),
                };
                self.emit(id, "approval.request", question_payload(prompt_id, &ask))
            }
            CoreEvent::ClarifyRequested {
                prompt_id,
                question,
                choices,
                multi_select,
                ..
            } => {
                let ask = Ask::Clarify {
                    question: question.clone(),
                    choices: choices.clone(),
                    multi_select: *multi_select,
                };
                self.emit(id, "clarify.request", question_payload(prompt_id, &ask))
            }
            CoreEvent::Settled {
                prompt_id, kind, ..
            } => self.emit(
                id,
                &format!("{}.settled", kind.as_str()),
                json!({"prompt_id": prompt_id, "kind": kind.as_str()}),
            ),
            // The page turns a queued prompt's card into the user's bubble on the admission it
            // names (patches/0001), before `session.info` retires the card.
            CoreEvent::TurnStarted { admission_id, .. } => self.emit(
                id,
                "message.start",
                json!({"turn_id": admission_id, "admission_id": admission_id}),
            ),
            // A subagent's activity: the page's live subagent rows (the runtime builds Hermes's
            // payload).
            CoreEvent::Subagent { kind, payload, .. } => self.emit(id, kind, payload.clone()),
            // The gateway's own frames (`session.usage`, `message.complete`, `session.info`),
            // built from more than the event by its turn hooks (`gateway/turn.rs`).
            CoreEvent::Usage { .. }
            | CoreEvent::TurnEnded { .. }
            | CoreEvent::QueueChanged { .. }
            | CoreEvent::QueuedFailed { .. }
            | CoreEvent::Idle { .. }
            | CoreEvent::ProcessFinished { .. }
            | CoreEvent::ReviewFinished { .. }
            | CoreEvent::CuratorStarted
            | CoreEvent::CuratorFinished { .. } => false,
        }
    }
}

/// `true` for an approval card.
pub fn is_approval(question: &OpenQuestion) -> bool {
    question.ask.kind() == QuestionKind::Approval
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            out.push(serde_json::from_str(&frame).unwrap());
        }
        out
    }

    /// The card goes out as Hermes's canonical event, in the payload's field order, and only to
    /// the socket following its session.
    #[test]
    fn an_approval_is_the_canonical_card_on_the_following_socket() {
        let hub = Hub::default();
        let (outbox, mut page) = mpsc::unbounded_channel();
        hub.subscribe("s1", &outbox);
        let request = CoreEvent::ApprovalRequested {
            session_id: "s1".into(),
            prompt_id: "p1".into(),
            command: "rm -rf build".into(),
            description: "recursive delete".into(),
            choices: APPROVAL_CHOICES.to_vec(),
        };
        assert!(hub.deliver(&request));
        let frame = &frames(&mut page)[0];
        assert_eq!(frame["params"]["type"], "approval.request");
        assert_eq!(frame["params"]["session_id"], "s1");
        let payload = &frame["params"]["payload"];
        let keys: Vec<&str> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "command",
                "description",
                "choices",
                "prompt_id",
                "kind",
                "execution_generation"
            ]
        );
        assert_eq!(payload["kind"], "approval");
        assert!(hub.deliver(&CoreEvent::Settled {
            session_id: "s1".into(),
            prompt_id: "p1".into(),
            kind: QuestionKind::Approval,
        }));
        assert_eq!(frames(&mut page)[0]["params"]["type"], "approval.settled");
        // Nobody follows s2: not shown, so the core denies it.
        let elsewhere = CoreEvent::ClarifyRequested {
            session_id: "s2".into(),
            prompt_id: "p2".into(),
            question: "Which?".into(),
            choices: vec![],
            multi_select: false,
        };
        assert!(!hub.deliver(&elsewhere));
    }

    #[test]
    fn a_tool_result_in_json_is_read_as_its_value() {
        let hub = Hub::default();
        let (outbox, mut page) = mpsc::unbounded_channel();
        hub.subscribe("s1", &outbox);
        hub.deliver(&CoreEvent::ToolCompleted {
            session_id: "s1".into(),
            tool_id: "c1".into(),
            name: "terminal".into(),
            text: r#"{"exit_code": 0}"#.into(),
        });
        let payload = &frames(&mut page)[0]["params"]["payload"];
        assert_eq!(payload["result"]["exit_code"], 0);
        assert_eq!(payload["result_text"], r#"{"exit_code": 0}"#);
    }
}
