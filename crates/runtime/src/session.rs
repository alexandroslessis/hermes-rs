//! Open sessions: the live conversation, its tools, and how the agent loop's effects
//! (`agent_loop.rs`) are applied to memory, the database and the event stream.
//!
//! Effects reach the database through the store's writer (Phase 13 CP13b): `apply_effects`
//! queues each write ([`Store::queue`]) and goes on; the writer commits them in batches. A
//! queued write that failed is returned by the session's next `queue`, which stops the turn as
//! a synchronous write did, and the turn's end flushes (`flush_turn` in `lib.rs`).

use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_agent::events::AgentEvent;
use hermes_agent::inference::InferenceEffect;
use hermes_agent::operation::{ConversationEffect, Emitter, MachineEffect, MachineSession};
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::conversation::token_usage::ProviderUsage;
use hermes_store::{SessionRecord, Store, StoreOp};
use hermes_tools::SessionTools;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ops::{SharedTurnState, TurnState};
use crate::session_actor::{Busy, Command, Gone, SessionHandle};

pub enum HermesEffect {
    Conversation(ConversationEffect),
    RecordUsage(ProviderUsage),
    /// Replaces a message by id (a repaired tool call, or a withdrawn one hidden from the model).
    UpdateMessage(Message),
    /// Context compression's result: the rebuilt conversation and the refreshed frozen prompt.
    Compacted {
        conversation: Conversation,
        system_prompt: Option<String>,
        notice: crate::compress::CompactionNotice,
    },
}

impl MachineEffect for HermesEffect {
    fn ensure_message_ids(&mut self) {
        match self {
            HermesEffect::Conversation(effect) => effect.ensure_message_ids(),
            HermesEffect::UpdateMessage(_)
            | HermesEffect::RecordUsage(_)
            | HermesEffect::Compacted { .. } => {}
        }
    }
}

impl From<Message> for HermesEffect {
    fn from(message: Message) -> Self {
        HermesEffect::Conversation(ConversationEffect::AppendMessage(message))
    }
}

impl From<ConversationEffect> for HermesEffect {
    fn from(effect: ConversationEffect) -> Self {
        HermesEffect::Conversation(effect)
    }
}

impl InferenceEffect for HermesEffect {
    fn record_usage(usage: ProviderUsage) -> Self {
        HermesEffect::RecordUsage(usage)
    }
}

/// What one pass of the agent loop sees: a snapshot of the conversation plus the frozen prompt.
pub struct TurnSession {
    pub id: String,
    pub system_prompt: String,
    pub conversation: Conversation,
    /// One-shot output cap for the next request (see `TurnState::max_tokens`).
    pub max_tokens: Option<i32>,
    /// The last request's tokens (input + output) and the conversation length then.
    pub context: Option<(usize, usize)>,
}

impl TurnSession {
    /// Tokens the next request would take: the last one's plus what was added since, or an
    /// estimate of everything (`fixed`: system prompt and tools) when there was none.
    pub fn context_tokens(&self, conversation: &Conversation, fixed: usize) -> usize {
        let messages = conversation.messages();
        match self.context {
            Some((tokens, len)) if len <= messages.len() => {
                tokens + crate::compress::estimate_messages(&messages[len..])
            }
            _ => fixed + crate::compress::estimate_messages(messages),
        }
    }
}

impl MachineSession for TurnSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }

    fn max_tokens_override(&self) -> Option<i32> {
        self.max_tokens
    }
}

/// A running turn's hold on its session (RAII, like a C++ `lock_guard`): the session actor's run
/// slot, its cancellation token, and the cross-process lock that keeps another window from
/// starting a turn on the same thread. Dropping it frees all three, on every path (the end, an
/// early error, a panicking turn whose task is dropped): nothing resets the slot by hand.
pub struct RunGuard {
    handle: SessionHandle,
    claim: u64,
    token: CancellationToken,
    lock: Option<crate::turnlock::TurnLock>,
}

impl RunGuard {
    /// Takes the session's run slot: `Ok(Err(Busy))` while another turn (or `/compress`) holds
    /// it. `turn_state` is where `/steer` text for this turn waits (none for `/compress`).
    pub fn claim(
        handle: &SessionHandle,
        token: CancellationToken,
        turn_state: Option<SharedTurnState>,
    ) -> Result<Result<Self, Busy>, Gone> {
        let cancel = token.clone();
        let claim = handle.ask(|reply| Command::ClaimRun {
            cancel,
            turn_state,
            reply,
        })?;
        Ok(claim.map(|claim| Self {
            handle: handle.clone(),
            claim,
            token,
            lock: None,
        }))
    }

    /// Keeps the cross-process lock with the claim (taken after it: file work stays out of the
    /// actor).
    pub fn hold(&mut self, lock: crate::turnlock::TurnLock) {
        self.lock = Some(lock);
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // The cross-process lock first: once the slot is free, this process's next turn must
        // find the file free too.
        drop(self.lock.take());
        // Waited for (in-memory work on the actor's thread), so whoever learns that the turn
        // ended (its `TurnFinished` is sent after this) sees the slot free.
        let claim = self.claim;
        if let Err(gone) = self.handle.ask(|reply| Command::Release { claim, reply }) {
            tracing::warn!("session {}: the run slot was not released: {gone}", self.handle.id());
        }
    }
}

impl std::ops::Deref for RunGuard {
    type Target = CancellationToken;

    fn deref(&self) -> &CancellationToken {
        &self.token
    }
}

/// A background review in flight. A turn starting while it runs leaves it alone (Phase 4):
/// before its first request ([`ReviewHandle::started`] is `false`) it waits for the turn to end
/// (a pause: it takes its snapshot of the conversation only when no turn is running), and after
/// that it is left to finish on its own copy of the conversation. Its slot is the session
/// actor's (`session_actor::Command::ClaimReview`).
#[derive(Clone)]
pub struct ReviewHandle {
    pub cancel: CancellationToken,
    /// Set by the session's actor (`ConfirmIdle`) when the review took its snapshot of the
    /// conversation with no turn running, i.e. when it is about to make its first request.
    pub started: Arc<std::sync::atomic::AtomicBool>,
    /// What the review covers, and whether the user asked for it (`/refine`): a review that
    /// replaces a paused one takes these over, so the paused one's job isn't lost.
    pub memory: bool,
    pub skills: bool,
    pub requested: bool,
}

impl ReviewHandle {
    pub fn new(memory: bool, skills: bool, requested: bool) -> Self {
        Self {
            cancel: CancellationToken::new(),
            started: Arc::default(),
            memory,
            skills,
            requested,
        }
    }

    pub fn has_started(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A session's data: what a turn reads and writes. Its control state (who runs it, its review,
/// what the page queued) is its actor's (`session_actor.rs`); a review's or a subagent's copy
/// has none.
pub struct OpenSession {
    pub record: Mutex<SessionRecord>,
    pub conversation: Mutex<Conversation>,
    pub tools: Arc<SessionTools>,
    /// The last request's tokens and the conversation length then (see [`TurnSession`]).
    pub context: Mutex<Option<(usize, usize)>>,
}

impl OpenSession {
    /// A session's data: its record, conversation and tools, nothing measured yet.
    pub fn new(record: SessionRecord, conversation: Conversation, tools: Arc<SessionTools>) -> Self {
        Self {
            record: Mutex::new(record),
            conversation: Mutex::new(conversation),
            tools,
            context: Mutex::new(None),
        }
    }

    /// Stores a compacted conversation and, when rebuilt, the new frozen prompt (compaction is
    /// the one place it may change, as in Hermes). The next request is measured afresh.
    pub fn apply_compaction(
        &self,
        store: &Store,
        conversation: Conversation,
        system_prompt: Option<String>,
    ) -> Result<()> {
        let session_id = self.record.lock().id.clone();
        if let Some(prompt) = system_prompt {
            store.queue(StoreOp::SetSystemPrompt {
                session_id: session_id.clone(),
                system_prompt: prompt.clone(),
            })?;
            self.record.lock().system_prompt = prompt;
        }
        store.queue(StoreOp::ReplaceMessages {
            session_id,
            messages: conversation.messages().to_vec(),
        })?;
        *self.conversation.lock() = conversation;
        *self.context.lock() = None;
        Ok(())
    }

    pub fn snapshot(&self) -> TurnSession {
        let record = self.record.lock();
        TurnSession {
            id: record.id.clone(),
            system_prompt: record.system_prompt.clone(),
            conversation: self.conversation.lock().clone(),
            max_tokens: None,
            context: *self.context.lock(),
        }
    }
}

/// Bridges one open session to the agent loop (`agent_loop.rs`): loads snapshots, applies effects.
pub struct TurnRuntime {
    pub session: Arc<OpenSession>,
    pub store: Arc<Store>,
    pub state: Arc<Mutex<TurnState>>,
    /// Whether messages are saved to the session's history. A background review's aren't: it
    /// runs on a copy of the conversation, and only its token usage is recorded.
    pub persist: bool,
    /// Whether token usage is recorded against the session (not for the curator, which has none).
    pub record_usage: bool,
}

impl TurnRuntime {
    /// Appends (or merges a streamed chunk into) the conversation, then queues the merged
    /// message for the store's writer (committed within ~5 ms, and at the latest when the turn
    /// ends: Phase 13 CP13b). Queued under the conversation's lock, so two writers to one
    /// session reach the store in the order they changed it; queueing never blocks.
    pub fn append(&self, message: Message) -> Result<()> {
        let session_id = self.session.record.lock().id.clone();
        let mut conversation = self.session.conversation.lock();
        let id = message.id.clone();
        conversation.push(message);
        let messages = conversation.messages();
        let index = id
            .and_then(|id| {
                messages
                    .iter()
                    .rposition(|m| m.id.as_deref() == Some(id.as_str()))
            })
            .unwrap_or(messages.len().saturating_sub(1));
        let Some(stored) = messages.get(index).cloned() else {
            return Ok(());
        };
        if !self.persist {
            return Ok(());
        }
        self.store.queue(StoreOp::UpsertMessage {
            session_id,
            seq: index,
            message: stored,
        })
    }

    /// Removes the error messages that ended the last turn; `None` when the conversation
    /// doesn't end with one.
    pub fn drop_trailing_errors(&self) -> Result<Option<Conversation>> {
        let session_id = self.session.record.lock().id.clone();
        let conversation = {
            let mut conversation = self.session.conversation.lock();
            let messages = conversation.messages_mut();
            let before = messages.len();
            while messages
                .last()
                .is_some_and(|message| message.error_kind().is_some())
            {
                messages.pop();
            }
            if messages.len() == before {
                return Ok(None);
            }
            // Queued under the lock, as in `append`.
            self.store.queue(StoreOp::ReplaceMessages {
                session_id,
                messages: conversation.messages().to_vec(),
            })?;
            conversation.clone()
        };
        Ok(Some(conversation))
    }

    fn update_message(&self, message_id: &str, change: impl FnOnce(&mut Message)) -> Result<()> {
        let session_id = self.session.record.lock().id.clone();
        // Queued under the lock, as in `append`.
        let mut conversation = self.session.conversation.lock();
        let messages = conversation.messages_mut();
        let index = messages
            .iter()
            .rposition(|m| m.id.as_deref() == Some(message_id))
            .with_context(|| format!("message {message_id} not found"))?;
        change(Arc::make_mut(&mut messages[index]));
        let stored = messages[index].clone();
        if !self.persist {
            return Ok(());
        }
        self.store.queue(StoreOp::UpsertMessage {
            session_id,
            seq: index,
            message: stored,
        })
    }
}

impl TurnRuntime {
    /// What the next pass of the agent loop sees: a snapshot of the session, with the one-shot
    /// output cap a retry asked for (taken: the pass after it no longer has it).
    pub fn load(&self) -> TurnSession {
        let mut session = self.session.snapshot();
        session.max_tokens = self.state.lock().max_tokens.take();
        session
    }

    /// Applies one pass's effects in order: memory, the store's queue, the event stream.
    pub fn apply_effects(&self, effects: &mut [HermesEffect], emit: &Emitter) -> Result<()> {
        for effect in effects.iter_mut() {
            effect.ensure_message_ids();
            match effect {
                HermesEffect::Conversation(ConversationEffect::AppendMessage(message)) => {
                    self.append(message.clone())?;
                }
                HermesEffect::Conversation(ConversationEffect::ReplaceConversation(
                    conversation,
                )) => {
                    let session_id = self.session.record.lock().id.clone();
                    if self.persist {
                        self.store.queue(StoreOp::ReplaceMessages {
                            session_id,
                            messages: conversation.messages().to_vec(),
                        })?;
                    }
                    *self.session.conversation.lock() = conversation.clone();
                    emit.emit(AgentEvent::HistoryReplaced(conversation.clone()));
                }
                HermesEffect::Conversation(ConversationEffect::PatchToolRequestMeta {
                    tool_call_id,
                    patch,
                }) => {
                    let message_id = self
                        .session
                        .conversation
                        .lock()
                        .messages()
                        .iter()
                        .find(|message| {
                            message
                                .content
                                .iter()
                                .filter_map(|content| content.as_tool_request())
                                .any(|request| &request.id == tool_call_id)
                        })
                        .and_then(|message| message.id.clone());
                    if let Some(message_id) = message_id {
                        let tool_call_id = tool_call_id.clone();
                        let patch = patch.clone();
                        self.update_message(&message_id, |message| {
                            for content in &mut message.content {
                                if let MessageContent::ToolRequest(request) = content {
                                    if request.id == tool_call_id {
                                        merge_json(
                                            request
                                                .tool_meta
                                                .get_or_insert_with(|| serde_json::json!({})),
                                            &patch,
                                        );
                                    }
                                }
                            }
                        })?;
                    }
                }
                HermesEffect::Conversation(ConversationEffect::SetMessageOperationNote {
                    message_id,
                    operation,
                    key,
                    value,
                }) => {
                    let (operation, key, value) = (operation.clone(), key.clone(), value.clone());
                    self.update_message(message_id, |message| {
                        message.metadata.set_operation_note(&operation, &key, value);
                    })?;
                }
                HermesEffect::Conversation(ConversationEffect::SetMessageVisibility {
                    message_id,
                    user_visible,
                    agent_visible,
                }) => {
                    let (user_visible, agent_visible) = (*user_visible, *agent_visible);
                    self.update_message(message_id, |message| {
                        message.metadata.user_visible = user_visible;
                        message.metadata.agent_visible = agent_visible;
                    })?;
                }
                HermesEffect::Compacted {
                    conversation,
                    system_prompt,
                    notice,
                } => {
                    self.session.apply_compaction(
                        &self.store,
                        conversation.clone(),
                        system_prompt.take(),
                    )?;
                    emit.emit(AgentEvent::HistoryReplaced(conversation.clone()));
                    emit.emit(AgentEvent::Compacted {
                        tokens: notice.tokens_after,
                    });
                }
                HermesEffect::UpdateMessage(message) => {
                    let id = message.id.clone().context("updated message has no id")?;
                    let message = message.clone();
                    self.update_message(&id, |stored| *stored = message)?;
                    let conversation = self.session.conversation.lock().clone();
                    emit.emit(AgentEvent::HistoryReplaced(conversation));
                }
                HermesEffect::RecordUsage(usage) => {
                    let tokens = usage.usage.input_tokens.unwrap_or(0).max(0) as usize
                        + usage.usage.output_tokens.unwrap_or(0).max(0) as usize;
                    if self.persist && tokens > 0 {
                        let len = self.session.conversation.lock().len();
                        *self.session.context.lock() = Some((tokens, len));
                    }
                    if self.record_usage {
                        let session_id = self.session.record.lock().id.clone();
                        self.store.queue(StoreOp::RecordUsage {
                            session_id,
                            usage: usage.clone(),
                        })?;
                    }
                    emit.emit(AgentEvent::Usage(usage.clone()));
                }
            }
        }
        Ok(())
    }
}

fn merge_json(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                merge_json(
                    target.entry(key.clone()).or_insert(serde_json::Value::Null),
                    value,
                );
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

/// Test helpers shared with other modules' tests (`session_actor`).
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use async_trait::async_trait;
    use hermes_store::NewSession;
    use hermes_tools::{
        ClarifyOutcome, ClarifyRequest, ProcessNotice, SessionToolsConfig, UserBridge,
    };

    struct NoBridge;

    #[async_trait]
    impl UserBridge for NoBridge {
        async fn clarify(&self, _session_id: &str, _request: ClarifyRequest) -> ClarifyOutcome {
            ClarifyOutcome::Undelivered
        }
        fn process_finished(&self, _session_id: &str, _notice: ProcessNotice) {}
    }

    /// An open session over an in-memory store, with a user message and a short reply stored.
    pub(crate) fn open_session(dir: &std::path::Path) -> (TurnRuntime, Arc<OpenSession>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let record = store
            .create_session(NewSession {
                cwd: dir.display().to_string(),
                provider: "p".into(),
                model: "m".into(),
                system_prompt: "system".into(),
            })
            .unwrap();
        let tools = Arc::new(SessionTools::new(SessionToolsConfig {
            session_id: record.id.clone(),
            cwd: dir.to_path_buf(),
            home: dir.to_path_buf(),
            data_dir: dir.join("data"),
            scratch_dir: dir.join("scratch"),
            shell: hermes_tools::detect_shell(),
            bridge: Arc::new(NoBridge),
            todos: Vec::new(),
            memory: None,
            history: None,
            skills: None,
            approvals: None,
            checkpoints: None,
            web: None,
        }));
        let conversation = Conversation::new_unvalidated([
            Message::user()
                .with_text("question")
                .with_generated_id_if_missing(),
            Message::assistant()
                .with_text("answer")
                .with_generated_id_if_missing(),
        ]);
        let session = Arc::new(OpenSession::new(record, conversation, tools));
        let runtime = TurnRuntime {
            session: session.clone(),
            store,
            state: Arc::default(),
            persist: false,
            record_usage: false,
        };
        (runtime, session)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::open_session;
    use super::*;

    #[test]
    fn a_snapshot_shares_the_messages_and_does_not_see_later_changes() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, session) = open_session(dir.path());
        let snapshot = session.snapshot();
        for (live, seen) in session
            .conversation
            .lock()
            .iter()
            .zip(snapshot.conversation.iter())
        {
            assert!(Arc::ptr_eq(live, seen), "the snapshot copied a message");
        }

        // A message added, and a message edited, after the snapshot was taken.
        runtime
            .append(
                Message::user()
                    .with_text("later")
                    .with_generated_id_if_missing(),
            )
            .unwrap();
        let id = snapshot.conversation.messages()[1].id.clone().unwrap();
        runtime
            .update_message(&id, |message| message.metadata.agent_visible = false)
            .unwrap();

        assert_eq!(snapshot.conversation.len(), 2);
        assert!(snapshot.conversation.messages()[1].metadata.agent_visible);
        let live = session.conversation.lock();
        assert_eq!(live.len(), 3);
        assert!(!live.messages()[1].metadata.agent_visible);
        // What the edit did not touch is still shared.
        assert!(Arc::ptr_eq(
            &live.messages()[0],
            &snapshot.conversation.messages()[0]
        ));
        assert!(!Arc::ptr_eq(
            &live.messages()[1],
            &snapshot.conversation.messages()[1]
        ));
    }

    #[test]
    fn dropping_trailing_errors_leaves_a_snapshot_whole() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, session) = open_session(dir.path());
        let error = hermes_llm::errors::ProviderError::RequestFailed("boom".into());
        session
            .conversation
            .lock()
            .push(Message::from_provider_error(&error).with_generated_id_if_missing());
        let snapshot = session.snapshot();
        assert_eq!(snapshot.conversation.len(), 3);
        let after = runtime
            .drop_trailing_errors()
            .unwrap()
            .expect("an error was last");
        assert_eq!(after.len(), 2);
        assert_eq!(session.conversation.lock().len(), 2);
        assert_eq!(
            snapshot.conversation.len(),
            3,
            "the snapshot still has what it saw"
        );
        assert!(snapshot.conversation.last().unwrap().error_kind().is_some());
    }
}
