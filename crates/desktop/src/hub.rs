//! What the runtime and the page share: which socket each session's events go to, and the
//! questions waiting for the user (a flagged command's approval, the agent's clarify questions).
//! The runtime asks through [`DesktopBridge`]; the page sees Hermes's canonical prompt events
//! (`approval.request` / `clarify.request`, then `*.settled`) and answers with
//! `approval.respond` / `clarify.respond` (`apps/desktop/src/api/canonical-protocol.ts`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_runtime::{ClarifyOutcome, ClarifyRequest, ProcessNotice};
use hermes_tools::UserBridge;
use hermes_tools::approval::{ApprovalChoice, CommandApproval};
use hermes_tools::{ClarifyAnswer, ClarifyResponse};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// Where a connection's outgoing frames go.
pub type Outbox = mpsc::UnboundedSender<String>;

/// Hermes's approval choices, in its order.
pub const APPROVAL_CHOICES: [&str; 4] = ["once", "session", "always", "deny"];

struct Pending {
    session_id: String,
    kind: &'static str,
    /// The event payload, kept so a resumed session shows the card again.
    payload: Value,
    reply: oneshot::Sender<String>,
}

#[derive(Default)]
pub struct Hub {
    /// Session id → the socket that last created, resumed or prompted it.
    subscribers: Mutex<HashMap<String, Outbox>>,
    /// Open questions by prompt id.
    prompts: Mutex<HashMap<String, Pending>>,
}

impl Hub {
    pub fn subscribe(&self, session_id: &str, outbox: &Outbox) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.insert(session_id.to_string(), outbox.clone());
        }
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
        let Some(outbox) = self.outbox(session_id) else {
            return false;
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {"type": event, "session_id": session_id, "payload": payload},
        });
        outbox.send(frame.to_string()).is_ok()
    }

    /// Shows a question and waits for the answer; `None` when no page follows the session or
    /// the question was dropped.
    async fn ask(&self, session_id: &str, kind: &'static str, mut payload: Value) -> Option<String> {
        let prompt_id = uuid::Uuid::new_v4().to_string();
        payload["prompt_id"] = Value::String(prompt_id.clone());
        payload["kind"] = Value::String(kind.to_string());
        let (reply, answer) = oneshot::channel();
        self.prompts.lock().ok()?.insert(
            prompt_id.clone(),
            Pending {
                session_id: session_id.to_string(),
                kind,
                payload: payload.clone(),
                reply,
            },
        );
        if !self.emit(session_id, &format!("{kind}.request"), payload) {
            self.prompts.lock().ok()?.remove(&prompt_id);
            return None;
        }
        answer.await.ok()
    }

    /// The page's answer to a question. `false` when it's no longer open.
    pub fn answer(&self, prompt_id: &str, answer: String) -> bool {
        let Some(pending) = self
            .prompts
            .lock()
            .ok()
            .and_then(|mut prompts| prompts.remove(prompt_id))
        else {
            return false;
        };
        self.emit(
            &pending.session_id,
            &format!("{}.settled", pending.kind),
            json!({"prompt_id": prompt_id, "kind": pending.kind}),
        );
        let _ = pending.reply.send(answer);
        true
    }

    /// A session's open questions, for `session.resume` (its `prompts`).
    pub fn open_prompts(&self, session_id: &str) -> Vec<Value> {
        self.prompts
            .lock()
            .map(|prompts| {
                prompts
                    .values()
                    .filter(|p| p.session_id == session_id)
                    .map(|p| p.payload.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The runtime's way to the user: through the page following the session.
pub struct DesktopBridge {
    pub hub: Arc<Hub>,
}

#[async_trait]
impl UserBridge for DesktopBridge {
    async fn clarify(&self, session_id: &str, request: ClarifyRequest) -> ClarifyOutcome {
        // The page answers one question at a time.
        let mut responses = Vec::new();
        for question in request.questions {
            let payload = json!({
                "question": question.question,
                "choices": question.choices,
                "multi_select": question.multi_select,
            });
            let Some(answer) = self.hub.ask(session_id, "clarify", payload).await else {
                return if responses.is_empty() {
                    ClarifyOutcome::Undelivered
                } else {
                    ClarifyOutcome::Cancelled
                };
            };
            let answer = answer.trim().to_string();
            responses.push(if answer.is_empty() {
                ClarifyResponse::Skipped
            } else if question.multi_select {
                ClarifyResponse::Answered(ClarifyAnswer::Choices(
                    answer
                        .split([',', '\n'])
                        .map(|choice| choice.trim().to_string())
                        .filter(|choice| !choice.is_empty())
                        .collect(),
                ))
            } else {
                ClarifyResponse::Answered(ClarifyAnswer::Text(answer))
            });
        }
        ClarifyOutcome::Submitted(responses)
    }

    fn process_finished(&self, session_id: &str, notice: ProcessNotice) {
        tracing::info!("background process finished in {session_id}: {notice:?}");
    }

    async fn approve_command(&self, session_id: &str, request: CommandApproval) -> ApprovalChoice {
        let payload = json!({
            "command": request.command,
            "description": request.description,
            "choices": APPROVAL_CHOICES,
        });
        // No page to ask (or the card was dropped): deny, as Hermes does when nobody answers.
        match self.hub.ask(session_id, "approval", payload).await.as_deref() {
            Some("once") => ApprovalChoice::Once,
            Some("session") => ApprovalChoice::Session,
            Some("always") => ApprovalChoice::Always,
            _ => ApprovalChoice::Deny,
        }
    }

    fn review_finished(&self, session_id: &str, actions: Vec<String>, requested: bool) {
        if !actions.is_empty() || requested {
            tracing::info!("background review of {session_id}: {actions:?}");
        }
    }
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

    #[tokio::test]
    async fn an_approval_is_asked_on_the_page_and_answered_from_it() {
        let hub = Arc::new(Hub::default());
        let (outbox, mut page) = mpsc::unbounded_channel();
        hub.subscribe("s1", &outbox);
        let bridge = DesktopBridge { hub: hub.clone() };
        let asking = tokio::spawn(async move {
            bridge
                .approve_command(
                    "s1",
                    CommandApproval {
                        command: "rm -rf build".into(),
                        description: "recursive delete".into(),
                    },
                )
                .await
        });
        // The card goes out as Hermes's canonical event, and stays open for a resume.
        let prompt_id = loop {
            if let Some(frame) = frames(&mut page).into_iter().next() {
                assert_eq!(frame["params"]["type"], "approval.request");
                assert_eq!(frame["params"]["payload"]["command"], "rm -rf build");
                break frame["params"]["payload"]["prompt_id"].as_str().unwrap().to_string();
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(hub.open_prompts("s1").len(), 1);
        assert!(hub.answer(&prompt_id, "session".into()));
        assert!(matches!(asking.await.unwrap(), ApprovalChoice::Session));
        assert_eq!(frames(&mut page)[0]["params"]["type"], "approval.settled");
        assert!(hub.open_prompts("s1").is_empty());
        // A second answer to the same card finds nothing.
        assert!(!hub.answer(&prompt_id, "once".into()));
    }

    #[tokio::test]
    async fn nobody_to_ask_denies_and_skips() {
        let bridge = DesktopBridge { hub: Arc::new(Hub::default()) };
        let request = CommandApproval {
            command: "x".into(),
            description: "y".into(),
        };
        assert!(matches!(bridge.approve_command("none", request).await, ApprovalChoice::Deny));
        let outcome = bridge
            .clarify(
                "none",
                ClarifyRequest {
                    questions: vec![hermes_tools::ClarifyQuestion {
                        question: "Which?".into(),
                        choices: vec![],
                        multi_select: false,
                    }],
                },
            )
            .await;
        assert!(matches!(outcome, ClarifyOutcome::Undelivered));
    }
}
