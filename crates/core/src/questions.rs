//! The questions waiting for the user: a flagged command's approval and the agent's clarify
//! questions. The runtime asks through [`CoreBridge`] (its `UserBridge`); each question goes out as
//! a [`CoreEvent`] (`ApprovalRequested` / `ClarifyRequested`) and waits for the front end's answer
//! (`Core::answer_approval` / `Core::answer_clarify`), then `Settled` closes its card. A question no
//! front end shows is answered "no answer" at once: an approval is denied, as Hermes does when
//! nobody answers.
//!
//! Hermes: `tui_gateway/server.py` (`_request_approval`, `_request_clarify`, the canonical prompt
//! events `approval.request` / `clarify.request` and their `*.settled`), `tools/approval.py` (the
//! four choices).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_runtime::{ClarifyOutcome, ClarifyRequest, ProcessNotice};
use hermes_tools::UserBridge;
use hermes_tools::approval::{ApprovalChoice, CommandApproval};
use hermes_tools::{ClarifyAnswer, ClarifyResponse};
use tokio::sync::oneshot;

use crate::events::{CoreEvent, EventBus, QuestionKind};

/// Hermes's approval choices, in its order.
pub const APPROVAL_CHOICES: [&str; 4] = ["once", "session", "always", "deny"];

/// What a question asks.
#[derive(Debug, Clone)]
pub enum Ask {
    /// Run this flagged command? `choices` are the answers offered.
    Approval {
        command: String,
        description: String,
        choices: Vec<&'static str>,
    },
    /// The agent's question, with its choices (free text when there are none).
    Clarify {
        question: String,
        choices: Vec<String>,
        multi_select: bool,
    },
}

impl Ask {
    pub fn kind(&self) -> QuestionKind {
        match self {
            Ask::Approval { .. } => QuestionKind::Approval,
            Ask::Clarify { .. } => QuestionKind::Clarify,
        }
    }

    fn event(&self, session_id: &str, prompt_id: &str) -> CoreEvent {
        let (session_id, prompt_id) = (session_id.to_string(), prompt_id.to_string());
        match self.clone() {
            Ask::Approval {
                command,
                description,
                choices,
            } => CoreEvent::ApprovalRequested {
                session_id,
                prompt_id,
                command,
                description,
                choices,
            },
            Ask::Clarify {
                question,
                choices,
                multi_select,
            } => CoreEvent::ClarifyRequested {
                session_id,
                prompt_id,
                question,
                choices,
                multi_select,
            },
        }
    }
}

/// A question still waiting, for a front end that (re)opens its session.
#[derive(Debug, Clone)]
pub struct OpenQuestion {
    pub prompt_id: String,
    pub session_id: String,
    pub ask: Ask,
}

struct Pending {
    session_id: String,
    ask: Ask,
    reply: oneshot::Sender<String>,
}

/// The open questions by prompt id, and the bus they are shown on.
pub struct Questions {
    events: Arc<EventBus>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl Questions {
    pub fn new(events: Arc<EventBus>) -> Self {
        Self {
            events,
            pending: Mutex::default(),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Shows a question and waits for the answer; `None` when no front end shows the session or
    /// the question was dropped.
    async fn ask(&self, session_id: &str, ask: Ask) -> Option<String> {
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let event = ask.event(session_id, &prompt_id);
        let (reply, answer) = oneshot::channel();
        self.pending().insert(
            prompt_id.clone(),
            Pending {
                session_id: session_id.to_string(),
                ask,
                reply,
            },
        );
        if !self.events.publish(event) {
            self.pending().remove(&prompt_id);
            return None;
        }
        // The turn may stop waiting without `Core::stop` (its cancel token drops the tool call, an
        // editor closes the thread): the card closes then, instead of staying open for good.
        let _asking = Asking {
            questions: self,
            prompt_id,
        };
        answer.await.ok()
    }

    /// Drops a question nobody waits for any more, if it is still open, and closes its card.
    fn abandon(&self, prompt_id: &str) {
        let pending = self.pending().remove(prompt_id);
        if let Some(pending) = pending {
            self.events.publish(CoreEvent::Settled {
                session_id: pending.session_id,
                prompt_id: prompt_id.to_string(),
                kind: pending.ask.kind(),
            });
        }
    }

    /// The user's answer to a question, as text: an approval's choice (`once`, `session`,
    /// `always`, anything else denies) or a clarify answer (choices joined with commas for a
    /// multi-select; empty skips). `false` when it is no longer open.
    pub fn answer(&self, prompt_id: &str, answer: String) -> bool {
        let Some(pending) = self.pending().remove(prompt_id) else {
            return false;
        };
        self.events.publish(CoreEvent::Settled {
            session_id: pending.session_id,
            prompt_id: prompt_id.to_string(),
            kind: pending.ask.kind(),
        });
        let _ = pending.reply.send(answer);
        true
    }

    /// Drops a session's open questions (its turn was stopped): their cards close and the
    /// runtime hears "no answer" (an approval is denied).
    pub fn cancel_session(&self, session_id: &str) {
        let dropped: Vec<(String, QuestionKind)> = {
            let mut pending = self.pending();
            let ids: Vec<String> = pending
                .iter()
                .filter(|(_, p)| p.session_id == session_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|p| (id, p.ask.kind())))
                .collect()
        };
        for (prompt_id, kind) in dropped {
            self.events.publish(CoreEvent::Settled {
                session_id: session_id.to_string(),
                prompt_id,
                kind,
            });
        }
    }

    /// A session's open questions (in no particular order).
    pub fn open(&self, session_id: &str) -> Vec<OpenQuestion> {
        self.pending()
            .iter()
            .filter(|(_, p)| p.session_id == session_id)
            .map(|(prompt_id, p)| OpenQuestion {
                prompt_id: prompt_id.clone(),
                session_id: p.session_id.clone(),
                ask: p.ask.clone(),
            })
            .collect()
    }

    /// Whether the session waits for the user (an open question).
    pub fn is_waiting(&self, session_id: &str) -> bool {
        self.pending().values().any(|p| p.session_id == session_id)
    }
}

/// A question being waited for; dropping it (answered, or the waiting future dropped) leaves no
/// open card behind.
struct Asking<'a> {
    questions: &'a Questions,
    prompt_id: String,
}

impl Drop for Asking<'_> {
    fn drop(&mut self) {
        self.questions.abandon(&self.prompt_id);
    }
}

/// The runtime's way to the user: questions through [`Questions`], background news as events.
pub struct CoreBridge {
    pub(crate) questions: Arc<Questions>,
    pub(crate) events: Arc<EventBus>,
}

#[async_trait]
impl UserBridge for CoreBridge {
    async fn clarify(&self, session_id: &str, request: ClarifyRequest) -> ClarifyOutcome {
        // One question at a time, as the page answers them.
        let mut responses = Vec::new();
        for question in request.questions {
            let ask = Ask::Clarify {
                question: question.question,
                choices: question.choices,
                multi_select: question.multi_select,
            };
            let Some(answer) = self.questions.ask(session_id, ask).await else {
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
        self.events.publish(CoreEvent::ProcessFinished {
            session_id: session_id.to_string(),
            notice,
        });
    }

    async fn approve_command(&self, session_id: &str, request: CommandApproval) -> ApprovalChoice {
        let choices: Vec<&'static str> = if request.once_only {
            vec!["once", "deny"]
        } else {
            APPROVAL_CHOICES.to_vec()
        };
        let ask = Ask::Approval {
            command: request.command,
            description: request.description,
            choices,
        };
        // No front end to ask (or the card was dropped): deny, as Hermes does when nobody answers.
        choice(self.questions.ask(session_id, ask).await.as_deref())
    }

    fn review_finished(&self, session_id: &str, actions: Vec<String>, requested: bool) {
        if !actions.is_empty() || requested {
            tracing::info!("background review of {session_id}: {actions:?}");
        }
        self.events.publish(CoreEvent::ReviewFinished {
            session_id: session_id.to_string(),
            actions,
            requested,
        });
    }
}

/// An approval answer's text as the runtime's choice; anything else is a denial.
pub fn choice(answer: Option<&str>) -> ApprovalChoice {
    match answer {
        Some("once") => ApprovalChoice::Once,
        Some("session") => ApprovalChoice::Session,
        Some("always") => ApprovalChoice::Always,
        _ => ApprovalChoice::Deny,
    }
}

/// The text of an approval choice ([`choice`]'s inverse).
pub fn choice_text(choice: ApprovalChoice) -> &'static str {
    match choice {
        ApprovalChoice::Once => "once",
        ApprovalChoice::Session => "session",
        ApprovalChoice::Always => "always",
        ApprovalChoice::Deny => "deny",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Listener;

    /// A front end following some sessions, recording what it was shown.
    struct Page {
        follows: Vec<&'static str>,
        shown: Mutex<Vec<CoreEvent>>,
    }

    impl Listener for Page {
        fn deliver(&self, event: &CoreEvent) -> bool {
            let follows = self.follows.contains(&event.session_id());
            if follows {
                self.shown.lock().unwrap().push(event.clone());
            }
            follows
        }
    }

    fn bridge(follows: Vec<&'static str>) -> (Arc<CoreBridge>, Arc<Page>) {
        let events = Arc::new(EventBus::default());
        let page = Arc::new(Page {
            follows,
            shown: Mutex::default(),
        });
        events.listen(page.clone());
        let questions = Arc::new(Questions::new(events.clone()));
        (Arc::new(CoreBridge { questions, events }), page)
    }

    fn shown(page: &Page) -> Vec<CoreEvent> {
        std::mem::take(&mut *page.shown.lock().unwrap())
    }

    #[tokio::test]
    async fn an_approval_is_asked_and_answered() {
        let (bridge, page) = bridge(vec!["s1"]);
        let asking = {
            let bridge = bridge.clone();
            tokio::spawn(async move {
                bridge
                    .approve_command(
                        "s1",
                        CommandApproval {
                            command: "rm -rf build".into(),
                            description: "recursive delete".into(),
                            once_only: false,
                        },
                    )
                    .await
            })
        };
        // The card goes out, and stays open for a front end that reopens the session.
        let prompt_id = loop {
            if let Some(CoreEvent::ApprovalRequested {
                prompt_id,
                command,
                choices,
                ..
            }) = shown(&page).into_iter().next()
            {
                assert_eq!(command, "rm -rf build");
                assert_eq!(choices, APPROVAL_CHOICES);
                break prompt_id;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(bridge.questions.open("s1").len(), 1);
        assert!(bridge.questions.is_waiting("s1"));
        assert!(bridge.questions.answer(&prompt_id, "session".into()));
        assert!(matches!(asking.await.unwrap(), ApprovalChoice::Session));
        assert!(matches!(
            shown(&page).as_slice(),
            [CoreEvent::Settled {
                kind: QuestionKind::Approval,
                ..
            }]
        ));
        assert!(bridge.questions.open("s1").is_empty());
        // A second answer to the same card finds nothing.
        assert!(!bridge.questions.answer(&prompt_id, "once".into()));
    }

    #[tokio::test]
    async fn manual_modes_offer_once_and_deny_only() {
        let (bridge, page) = bridge(vec!["s1"]);
        let asking = {
            let bridge = bridge.clone();
            tokio::spawn(async move {
                bridge
                    .approve_command(
                        "s1",
                        CommandApproval {
                            command: "x".into(),
                            description: "y".into(),
                            once_only: true,
                        },
                    )
                    .await
            })
        };
        let open = loop {
            let open = bridge.questions.open("s1");
            if !open.is_empty() {
                break open;
            }
            tokio::task::yield_now().await;
        };
        let Ask::Approval { choices, .. } = &open[0].ask else {
            panic!("an approval")
        };
        assert_eq!(choices, &["once", "deny"]);
        // Stopping the turn drops the card: denied, and settled for the page.
        bridge.questions.cancel_session("s1");
        assert!(matches!(asking.await.unwrap(), ApprovalChoice::Deny));
        assert!(
            shown(&page)
                .iter()
                .any(|e| matches!(e, CoreEvent::Settled { .. }))
        );
    }

    #[tokio::test]
    async fn a_question_its_turn_stopped_waiting_for_closes() {
        let (bridge, page) = bridge(vec!["s1"]);
        let asking = {
            let bridge = bridge.clone();
            tokio::spawn(async move {
                bridge
                    .approve_command(
                        "s1",
                        CommandApproval {
                            command: "x".into(),
                            description: "y".into(),
                            once_only: false,
                        },
                    )
                    .await
            })
        };
        while !bridge.questions.is_waiting("s1") {
            tokio::task::yield_now().await;
        }
        // The turn's tool call is dropped (cancelled without `Core::stop`).
        asking.abort();
        assert!(asking.await.unwrap_err().is_cancelled());
        assert!(!bridge.questions.is_waiting("s1"), "no card left open");
        assert!(matches!(
            shown(&page).last(),
            Some(CoreEvent::Settled {
                kind: QuestionKind::Approval,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_multi_select_answer_is_split_into_choices() {
        let (bridge, _page) = bridge(vec!["s1"]);
        let asking = {
            let bridge = bridge.clone();
            tokio::spawn(async move {
                bridge
                    .clarify(
                        "s1",
                        ClarifyRequest {
                            questions: vec![hermes_tools::ClarifyQuestion {
                                question: "Which?".into(),
                                choices: vec!["a".into(), "b".into()],
                                multi_select: true,
                            }],
                        },
                    )
                    .await
            })
        };
        let prompt_id = loop {
            if let Some(open) = bridge.questions.open("s1").pop() {
                break open.prompt_id;
            }
            tokio::task::yield_now().await;
        };
        assert!(bridge.questions.answer(&prompt_id, "a, b\n".into()));
        match asking.await.unwrap() {
            ClarifyOutcome::Submitted(responses) => assert!(matches!(
                responses.as_slice(),
                [ClarifyResponse::Answered(ClarifyAnswer::Choices(c))] if c == &["a", "b"]
            )),
            _ => panic!("submitted"),
        }
    }

    #[tokio::test]
    async fn nobody_to_ask_denies_and_skips() {
        let (bridge, _page) = bridge(vec![]);
        let request = CommandApproval {
            command: "x".into(),
            description: "y".into(),
            once_only: false,
        };
        assert!(matches!(
            bridge.approve_command("none", request).await,
            ApprovalChoice::Deny
        ));
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
        assert!(bridge.questions.open("none").is_empty());
    }

    #[test]
    fn choices_round_trip() {
        for c in [
            ApprovalChoice::Once,
            ApprovalChoice::Session,
            ApprovalChoice::Always,
            ApprovalChoice::Deny,
        ] {
            assert_eq!(choice_text(choice(Some(choice_text(c)))), choice_text(c));
        }
        assert!(matches!(choice(Some("bogus")), ApprovalChoice::Deny));
    }
}
