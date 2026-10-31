//! The operations a Hermes turn runs, in pipeline order. Adapted from Goose's
//! `goose/src/agents/state_machine/ops_*.rs` (Apache-2.0) for our session and effect types.

use std::collections::HashSet;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use hermes_agent::inference::is_empty_response_marker;
use hermes_agent::operation::{
    ConversationEffect, Emitter, Operation, OperationResult, applied, assistant_turn_count,
    ends_turn, messages_since_kickoff, not_applicable, yielded, yielded_with,
};
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use rmcp::model::{CallToolResult, ContentBlock};

use crate::session::{HermesEffect, TurnSession};

/// Contributes the session's frozen system prompt. It never changes mid-session, so the
/// provider's prompt cache stays valid (Hermes's central invariant).
pub struct FrozenPrompt;

#[async_trait]
impl Operation<TurnSession, HermesEffect> for FrozenPrompt {
    fn name(&self) -> &'static str {
        "prompt"
    }

    async fn prompt_parts(
        &self,
        session: &TurnSession,
        _conversation: &Conversation,
    ) -> Result<Vec<(String, String)>> {
        Ok(vec![("system".into(), session.system_prompt.clone())])
    }
}

/// Answers tool calls nothing else answered: a tool name the model invented. Without an answer
/// the next request would be rejected for an unpaired tool call.
pub struct UnhandledToolCalls {
    pub tool_names: Vec<String>,
}

#[async_trait]
impl Operation<TurnSession, HermesEffect> for UnhandledToolCalls {
    fn name(&self) -> &'static str {
        "unhandled_tool_calls"
    }

    async fn run(
        &self,
        _session: &TurnSession,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<HermesEffect>> {
        let turn = messages_since_kickoff(conversation)?;
        let answered: HashSet<&str> = turn
            .iter()
            .flat_map(Message::get_tool_response_ids)
            .collect();
        let unanswered: Vec<_> = turn
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(MessageContent::as_tool_request)
            .filter(|request| {
                !request.was_executed_externally() && !answered.contains(request.id.as_str())
            })
            .collect();
        if unanswered.is_empty() {
            return not_applicable();
        }
        let mut response = Message::user();
        for request in unanswered {
            let name = request
                .tool_call
                .as_ref()
                .map(|call| call.name.to_string())
                .unwrap_or_default();
            let text = format!(
                "Tool '{name}' does not exist. Available tools: {}.",
                self.tool_names.join(", ")
            );
            response.add_tool_response_with_metadata(
                request.id.clone(),
                Ok(CallToolResult::error(vec![ContentBlock::text(text)])),
                request.metadata.as_ref(),
            );
        }
        applied([emit.message(response).into()])
    }
}

/// Reveals the inference step's hidden "empty response" fallback when retries ran out.
pub struct EmptyResponse;

#[async_trait]
impl Operation<TurnSession, HermesEffect> for EmptyResponse {
    fn name(&self) -> &'static str {
        "empty_response"
    }

    async fn run(
        &self,
        _session: &TurnSession,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<HermesEffect>> {
        let Some(fallback) = messages_since_kickoff(conversation)?
            .last()
            .filter(|message| is_empty_response_marker(message) && !message.is_user_visible())
        else {
            return not_applicable();
        };
        let message_id = fallback
            .id
            .clone()
            .ok_or_else(|| anyhow!("empty-response fallback has no id"))?;
        emit.message(fallback.clone().with_visibility(true, true));
        applied([HermesEffect::Conversation(
            ConversationEffect::SetMessageVisibility {
                message_id,
                user_visible: true,
                agent_visible: true,
            },
        )])
    }
}

/// Ends the turn when a provider error is the last message.
pub struct ExitOnError;

#[async_trait]
impl Operation<TurnSession, HermesEffect> for ExitOnError {
    fn name(&self) -> &'static str {
        "exit_on_error"
    }

    async fn run(
        &self,
        _session: &TurnSession,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<HermesEffect>> {
        if conversation.last().and_then(Message::error_kind).is_some() {
            return yielded();
        }
        not_applicable()
    }
}

pub const MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can take without your input. Would you like me to continue?";

/// Ends the turn after `max_turns` model calls since the user's message (Hermes's iteration budget).
pub struct MaxTurns {
    pub max_turns: u32,
}

#[async_trait]
impl Operation<TurnSession, HermesEffect> for MaxTurns {
    fn name(&self) -> &'static str {
        "max_turns"
    }

    async fn run(
        &self,
        _session: &TurnSession,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<HermesEffect>> {
        let turn = messages_since_kickoff(conversation)?;
        if ends_turn(turn) || assistant_turn_count(turn) < self.max_turns {
            return not_applicable();
        }
        let message = emit.message(Message::assistant().with_text(MAX_TURNS_MESSAGE));
        yielded_with([message.into()])
    }
}
