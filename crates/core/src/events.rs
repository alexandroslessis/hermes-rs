//! What the backend tells the front ends while it works: one typed event stream for every session
//! (the streamed reply, tool calls, status lines, titles, the end of a turn), the questions the
//! agent asks the user (approvals and clarify questions, then their settling), background
//! processes and the background review. Built from the runtime's `RuntimeEvent`/`AgentEvent` and
//! the questions in `questions.rs`.
//!
//! Two ways to listen:
//! - [`EventBus::subscribe`]: a channel of every event (the native UI drains it into its models).
//! - [`EventBus::listen`]: a [`Listener`] called in the publisher's thread, in order, that says
//!   whether it showed the event. The Tauri gateway's hub is one: it turns each event into the
//!   page's JSON frame on the socket following that session, in the same order as the frames the
//!   gateway sends itself.
//!
//! A question counts as asked only when some listener showed it (`publish` returns `true`); one
//! nobody shows is answered "no answer" at once (an approval is denied), as Hermes does.
//!
//! Hermes: `tui_gateway/server.py` (`_emit`, the event names this stream maps to on the page).

use std::sync::{Arc, Mutex};

use hermes_runtime::{ProcessNotice, TurnStatus};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::turns::TurnEnd;

/// The two kinds of question the agent asks the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    /// A flagged command waits for the user's approval.
    Approval,
    /// The agent's `clarify` tool asks a question.
    Clarify,
}

impl QuestionKind {
    /// The page's name for it (`approval.request`, `clarify.settled`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionKind::Approval => "approval",
            QuestionKind::Clarify => "clarify",
        }
    }
}

/// One thing that happened, for whichever front end shows it. Every event names its session.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    /// More of the assistant's reply.
    TextDelta { session_id: String, text: String },
    /// More of the model's reasoning.
    ReasoningDelta { session_id: String, text: String },
    /// A tool call started; `args` is the call's arguments as the model sent them.
    ToolStarted {
        session_id: String,
        tool_id: String,
        name: String,
        args: Value,
    },
    /// Live progress from inside a running tool call (the terminal's output tail).
    ToolProgress {
        session_id: String,
        tool_id: String,
        name: String,
        progress: f64,
        message: String,
    },
    /// A tool call finished; `text` is its result as text.
    ToolCompleted {
        session_id: String,
        tool_id: String,
        name: String,
        text: String,
    },
    /// A line for the conversation's status row: `kind` names it (`compacting`, `compacted`,
    /// `fallback`, `quota`, the taint notices), `text` is what it says.
    Status {
        session_id: String,
        kind: String,
        text: String,
    },
    /// A model call's token counts.
    Usage {
        session_id: String,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
    /// A turn started (`admission_id`: the front end's id for the prompt that started it).
    TurnStarted {
        session_id: String,
        admission_id: String,
    },
    /// The turn ended (completed, stopped, or failed with the runtime's error): what it said, the
    /// error, the rows it stored (`turns::TurnEnd`).
    TurnEnded {
        session_id: String,
        status: TurnStatus,
        end: Box<TurnEnd>,
    },
    /// The prompts queued behind the session's turn changed (`Core::queue` lists them).
    QueueChanged { session_id: String },
    /// A queued prompt was taken off the queue to run, but its turn couldn't start (`error`
    /// says why): no `TurnStarted` / `TurnEnded` follows for it.
    QueuedFailed {
        session_id: String,
        admission_id: String,
        error: String,
    },
    /// The session's turn and every prompt queued behind it are done.
    Idle { session_id: String },
    /// A `delegate_task` child's activity: `kind` is `start`, `tool`, `thinking`, `progress` or
    /// `complete`; `payload` is Hermes's `SubagentEventPayload` (the runtime builds it).
    Subagent {
        session_id: String,
        kind: &'static str,
        payload: Value,
    },
    /// The session has a new title.
    TitleChanged { session_id: String, title: String },
    /// A flagged command waits for the user: answer with `Core::answer_approval`. `choices` are
    /// the answers offered, in order (`once`, `session`, `always`, `deny`; Manual and Plan offer
    /// `once` and `deny` only).
    ApprovalRequested {
        session_id: String,
        prompt_id: String,
        command: String,
        description: String,
        choices: Vec<&'static str>,
    },
    /// The agent asks a question: answer with `Core::answer_clarify`.
    ClarifyRequested {
        session_id: String,
        prompt_id: String,
        question: String,
        choices: Vec<String>,
        multi_select: bool,
    },
    /// A question was answered or dropped (its turn was stopped): its card closes.
    Settled {
        session_id: String,
        prompt_id: String,
        kind: QuestionKind,
    },
    /// A background process the agent started has exited.
    ProcessFinished {
        session_id: String,
        notice: ProcessNotice,
    },
    /// The background review of a session finished: what it changed. `requested` is true for
    /// `/refine`, which reports even when nothing changed.
    ReviewFinished {
        session_id: String,
        actions: Vec<String>,
        requested: bool,
    },
}

impl CoreEvent {
    /// The session the event belongs to.
    pub fn session_id(&self) -> &str {
        match self {
            CoreEvent::TextDelta { session_id, .. }
            | CoreEvent::ReasoningDelta { session_id, .. }
            | CoreEvent::ToolStarted { session_id, .. }
            | CoreEvent::ToolProgress { session_id, .. }
            | CoreEvent::ToolCompleted { session_id, .. }
            | CoreEvent::Status { session_id, .. }
            | CoreEvent::Usage { session_id, .. }
            | CoreEvent::TurnStarted { session_id, .. }
            | CoreEvent::TurnEnded { session_id, .. }
            | CoreEvent::QueueChanged { session_id }
            | CoreEvent::QueuedFailed { session_id, .. }
            | CoreEvent::Idle { session_id }
            | CoreEvent::Subagent { session_id, .. }
            | CoreEvent::TitleChanged { session_id, .. }
            | CoreEvent::ApprovalRequested { session_id, .. }
            | CoreEvent::ClarifyRequested { session_id, .. }
            | CoreEvent::Settled { session_id, .. }
            | CoreEvent::ProcessFinished { session_id, .. }
            | CoreEvent::ReviewFinished { session_id, .. } => session_id,
        }
    }
}

/// A front end that shows events as they are published, in the publisher's thread. It must not
/// block (it runs on the turn's task) and must not publish.
pub trait Listener: Send + Sync {
    /// Shows the event; `true` when a user can see it (a page follows its session).
    fn deliver(&self, event: &CoreEvent) -> bool;
    /// `false` once the listener is gone for good: it is then dropped from the bus.
    fn alive(&self) -> bool {
        true
    }
}

/// A [`EventBus::subscribe`] channel: every event, shown while its receiver lives.
struct Channel(mpsc::UnboundedSender<CoreEvent>);

impl Listener for Channel {
    fn deliver(&self, event: &CoreEvent) -> bool {
        self.0.send(event.clone()).is_ok()
    }

    fn alive(&self) -> bool {
        !self.0.is_closed()
    }
}

/// The event stream's listeners. Publishing takes the list's lock only to copy it, so a listener
/// may subscribe or publish from anywhere (never from inside `deliver`).
#[derive(Default)]
pub struct EventBus {
    listeners: Mutex<Vec<Arc<dyn Listener>>>,
}

impl EventBus {
    fn listeners(&self) -> std::sync::MutexGuard<'_, Vec<Arc<dyn Listener>>> {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds a listener called in order with every event.
    pub fn listen(&self, listener: Arc<dyn Listener>) {
        self.listeners().push(listener);
    }

    /// A channel with every event from now on.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<CoreEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.listen(Arc::new(Channel(sender)));
        receiver
    }

    /// Hands the event to every listener; `true` when one of them showed it.
    pub fn publish(&self, event: CoreEvent) -> bool {
        let listeners: Vec<Arc<dyn Listener>> = self.listeners().clone();
        let mut shown = false;
        let mut gone = false;
        for listener in &listeners {
            shown |= listener.deliver(&event);
            gone |= !listener.alive();
        }
        if gone {
            self.listeners().retain(|listener| listener.alive());
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(id: &str) -> CoreEvent {
        CoreEvent::TitleChanged {
            session_id: id.into(),
            title: "t".into(),
        }
    }

    /// Shows only the sessions it follows, like the page's hub.
    struct Follows(&'static str, Mutex<Vec<String>>);

    impl Listener for Follows {
        fn deliver(&self, event: &CoreEvent) -> bool {
            self.1.lock().unwrap().push(event.session_id().to_string());
            event.session_id() == self.0
        }
    }

    #[test]
    fn every_listener_sees_every_event_in_order_and_shown_means_someone_showed_it() {
        let bus = EventBus::default();
        assert!(!bus.publish(title("s1")), "nobody listens");
        let follows = Arc::new(Follows("s1", Mutex::default()));
        bus.listen(follows.clone());
        assert!(bus.publish(title("s1")));
        assert!(!bus.publish(title("s2")));
        assert_eq!(*follows.1.lock().unwrap(), ["s1", "s2"]);
        // A subscriber shows everything while it lives.
        let mut events = bus.subscribe();
        assert!(bus.publish(title("s3")));
        assert_eq!(events.try_recv().unwrap().session_id(), "s3");
        drop(events);
        assert!(
            !bus.publish(title("s4")),
            "a dropped subscriber shows nothing"
        );
        assert_eq!(bus.listeners().len(), 1, "and leaves the bus");
    }
}
