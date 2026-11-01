//! Open sessions: the live conversation, its tools, and how state-machine effects are applied
//! to memory, the database and the event stream.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use hermes_agent::events::AgentEvent;
use hermes_agent::inference::InferenceEffect;
use hermes_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use hermes_agent::operation::{ConversationEffect, Emitter, MachineEffect};
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::conversation::token_usage::ProviderUsage;
use hermes_store::{SessionRecord, Store};
use hermes_tools::SessionTools;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ops::TurnState;

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

/// What one state-machine pass sees: a snapshot of the conversation plus the frozen prompt.
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

pub struct OpenSession {
    pub record: Mutex<SessionRecord>,
    pub conversation: Mutex<Conversation>,
    pub tools: Arc<SessionTools>,
    pub running: Mutex<Option<CancellationToken>>,
    /// The running turn's loop state (where `/steer` text waits for delivery).
    pub turn_state: Mutex<Option<Arc<Mutex<TurnState>>>>,
    /// The background review running after the last turn; a new turn cancels it.
    pub review: Mutex<Option<CancellationToken>>,
    /// Hermes's nudge counters, which decide when a turn is followed by a review.
    pub nudges: Mutex<crate::review::Nudges>,
    /// The last request's tokens and the conversation length then (see [`TurnSession`]).
    pub context: Mutex<Option<(usize, usize)>>,
}

impl OpenSession {
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
            store.set_system_prompt(&session_id, &prompt)?;
            self.record.lock().system_prompt = prompt;
        }
        store.replace_messages(&session_id, conversation.messages())?;
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

/// Bridges one open session to the state machine: loads snapshots, applies effects.
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
    /// Appends (or merges a streamed chunk into) the conversation, then stores the merged message.
    pub fn append(&self, message: Message) -> Result<()> {
        let session_id = self.session.record.lock().id.clone();
        let (index, stored) = {
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
            match messages.get(index) {
                Some(message) => (index, message.clone()),
                None => return Ok(()),
            }
        };
        if !self.persist {
            return Ok(());
        }
        self.store.upsert_message(&session_id, index, &stored)
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
            conversation.clone()
        };
        self.store
            .replace_messages(&session_id, conversation.messages())?;
        Ok(Some(conversation))
    }

    fn update_message(&self, message_id: &str, change: impl FnOnce(&mut Message)) -> Result<()> {
        let session_id = self.session.record.lock().id.clone();
        let (index, stored) = {
            let mut conversation = self.session.conversation.lock();
            let messages = conversation.messages_mut();
            let index = messages
                .iter()
                .rposition(|m| m.id.as_deref() == Some(message_id))
                .with_context(|| format!("message {message_id} not found"))?;
            change(&mut messages[index]);
            (index, messages[index].clone())
        };
        if !self.persist {
            return Ok(());
        }
        self.store.upsert_message(&session_id, index, &stored)
    }
}

#[async_trait]
impl SessionLoader<TurnSession> for TurnRuntime {
    async fn load(&self, _session_id: &str) -> Result<TurnSession> {
        let mut session = self.session.snapshot();
        session.max_tokens = self.state.lock().max_tokens.take();
        Ok(session)
    }
}

#[async_trait]
impl EffectHandler<TurnSession, HermesEffect> for TurnRuntime {
    async fn apply_effects(
        &self,
        _session: &TurnSession,
        effects: &mut [HermesEffect],
        emit: &Emitter,
    ) -> Result<()> {
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
                        self.store
                            .replace_messages(&session_id, conversation.messages())?;
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
                        self.store.record_usage(&session_id, usage)?;
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
