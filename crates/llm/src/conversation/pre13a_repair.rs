//! Test only: the repair passes as they were before Phase 13a (Goose's code at e16fbe2, over
//! plain `Message`s, every message copied). `conversation.rs`'s equivalence test runs these and
//! the copy-on-write passes over many generated conversations and compares the results and the
//! issue lists, so the "check read-only, `make_mut` only then" rewrite cannot drift.
#![allow(dead_code)]

use crate::conversation::message::{Message, MessageContentBlock};
use crate::conversation::{effective_role, is_signed_thinking};
use crate::mcp_utils::extract_text_from_resource;
use rmcp::model::{ContentBlock, Role};
use std::collections::HashSet;

/// HEAD's `fix_conversation`, over a plain list.
pub fn fix_conversation(all_messages: &[Message]) -> (Vec<Message>, Vec<String>) {
    // Create a shadow map: track each message as either Visible or NonVisible with its index
    enum MessageSlot {
        Visible(usize),      // Index into agent_visible_messages
        NonVisible(Message), // Non-visible messages pass through unchanged
    }

    let mut agent_visible_messages = Vec::new();
    let shadow_map: Vec<MessageSlot> = all_messages
        .iter()
        .map(|msg| {
            if msg.metadata.agent_visible {
                let idx = agent_visible_messages.len();
                agent_visible_messages.push(msg.clone());
                MessageSlot::Visible(idx)
            } else {
                MessageSlot::NonVisible(msg.clone())
            }
        })
        .collect();

    // Fix only the agent-visible messages
    let (fixed_visible, issues) = fix_messages(agent_visible_messages);

    // Reconstruct using shadow map: replace Visible slots with fixed messages
    let final_messages: Vec<Message> = shadow_map
        .into_iter()
        .filter_map(|slot| match slot {
            MessageSlot::Visible(idx) => fixed_visible.get(idx).cloned(),
            MessageSlot::NonVisible(msg) => Some(msg),
        })
        .collect();

    (final_messages, issues)
}

fn fix_messages(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    [
        merge_text_content_items,
        trim_assistant_text_whitespace,
        remove_empty_messages,
        fix_empty_tool_results,
        fix_tool_calling,
        merge_consecutive_messages,
        dedupe_signed_thinking,
        fix_lead_trail,
        populate_if_empty,
    ]
    .into_iter()
    .fold(
        (messages, Vec::new()),
        |(msgs, mut all_issues), processor| {
            let (new_msgs, issues) = processor(msgs);
            all_issues.extend(issues);
            (new_msgs, all_issues)
        },
    )
}

fn merge_text_content_in_message(mut msg: Message) -> Message {
    if msg.role != Role::Assistant {
        return msg;
    }
    msg.content = msg
        .content
        .into_iter()
        .fold(Vec::new(), |mut content, item| {
            match item {
                MessageContentBlock::Text(text) => match content.last_mut() {
                    Some(MessageContentBlock::Text(last))
                        if last.annotations.as_ref().and_then(|a| a.audience.as_ref())
                            == text.annotations.as_ref().and_then(|a| a.audience.as_ref()) =>
                    {
                        last.text.push_str(&text.text);
                    }
                    _ => content.push(MessageContentBlock::Text(text)),
                },
                other => content.push(other),
            }
            content
        });
    msg
}

fn merge_text_content_items(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    messages.into_iter().fold(
        (Vec::new(), Vec::new()),
        |(mut messages, mut issues), message| {
            let content_len = message.content.len();
            let message = merge_text_content_in_message(message);
            if content_len != message.content.len() {
                issues.push(String::from("Merged text content"))
            }
            messages.push(message);
            (messages, issues)
        },
    )
}

fn trim_assistant_text_whitespace(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();

    let fixed_messages = messages
        .into_iter()
        .map(|mut message| {
            if message.role == Role::Assistant {
                for content in &mut message.content {
                    if let MessageContentBlock::Text(text) = content {
                        let trimmed = text.text.trim_end();
                        if trimmed.len() != text.text.len() {
                            issues.push(
                                "Trimmed trailing whitespace from assistant message".to_string(),
                            );
                            text.text = trimmed.to_string();
                        }
                    }
                }
            }
            message
        })
        .collect();

    (fixed_messages, issues)
}

fn remove_empty_messages(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();
    let filtered_messages = messages
        .into_iter()
        .filter(|msg| {
            if msg
                .content
                .iter()
                .all(|c| c.as_text().is_some_and(str::is_empty))
            {
                issues.push("Removed empty message".to_string());
                false
            } else {
                true
            }
        })
        .collect();
    (filtered_messages, issues)
}

/// Checks whether tool result content has any meaningful payload.
/// Text and resources must contain non-empty strings; images are always meaningful.
fn has_tool_result_content(content: &[ContentBlock]) -> bool {
    content.iter().any(|c| {
        if let Some(t) = c.as_text() {
            return !t.text.is_empty();
        }
        if let Some(r) = c.as_resource() {
            return !extract_text_from_resource(&r.resource).is_empty();
        }
        c.as_image().is_some()
    })
}

/// Fix tool results that would be empty when formatted for LLM APIs.
/// Some APIs (like Anthropic) reject tool_result blocks with empty content.
/// This adds a placeholder message for tool results that have no extractable text.
fn fix_empty_tool_results(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();

    let fixed_messages = messages
        .into_iter()
        .map(|mut message| {
            for content in &mut message.content {
                if let MessageContentBlock::ToolResponse(ref mut tool_response) = content {
                    if let Ok(ref mut result) = tool_response.tool_result {
                        if !has_tool_result_content(&result.content) {
                            // Add a placeholder text content so the tool result isn't empty
                            result.content.push(ContentBlock::text("(empty result)"));
                            issues.push(format!(
                                "Added placeholder to empty tool result '{}'",
                                tool_response.id
                            ));
                        }
                    }
                }
            }
            message
        })
        .collect();

    (fixed_messages, issues)
}

fn fix_tool_calling(mut messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();
    let mut pending_tool_requests: HashSet<String> = HashSet::new();

    for message in &mut messages {
        let mut content_to_remove = Vec::new();

        match message.role {
            Role::User => {
                for (idx, content) in message.content.iter().enumerate() {
                    match content {
                        MessageContentBlock::ToolRequest(req) => {
                            content_to_remove.push(idx);
                            issues.push(format!(
                                "Removed tool request '{}' from user message",
                                req.id
                            ));
                        }
                        MessageContentBlock::ToolConfirmationRequest(req) => {
                            content_to_remove.push(idx);
                            issues.push(format!(
                                "Removed tool confirmation request '{}' from user message",
                                req.id
                            ));
                        }
                        MessageContentBlock::Thinking(_)
                        | MessageContentBlock::RedactedThinking(_) => {
                            content_to_remove.push(idx);
                            issues.push("Removed thinking content from user message".to_string());
                        }
                        MessageContentBlock::ToolResponse(resp) => {
                            if pending_tool_requests.contains(&resp.id) {
                                pending_tool_requests.remove(&resp.id);
                            } else {
                                content_to_remove.push(idx);
                                issues
                                    .push(format!("Removed orphaned tool response '{}'", resp.id));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Role::Assistant => {
                for (idx, content) in message.content.iter().enumerate() {
                    match content {
                        MessageContentBlock::ToolResponse(resp) => {
                            content_to_remove.push(idx);
                            issues.push(format!(
                                "Removed tool response '{}' from assistant message",
                                resp.id
                            ));
                        }
                        MessageContentBlock::ToolRequest(req) => {
                            pending_tool_requests.insert(req.id.clone());
                        }
                        _ => {}
                    }
                }
            }
        }

        for &idx in content_to_remove.iter().rev() {
            message.content.remove(idx);
        }
    }

    for message in &mut messages {
        if message.role == Role::Assistant {
            let mut content_to_remove = Vec::new();
            for (idx, content) in message.content.iter().enumerate() {
                if let MessageContentBlock::ToolRequest(req) = content {
                    if pending_tool_requests.contains(&req.id) {
                        content_to_remove.push(idx);
                        issues.push(format!("Removed orphaned tool request '{}'", req.id));
                    }
                }
            }
            for &idx in content_to_remove.iter().rev() {
                message.content.remove(idx);
            }
        }
    }
    let (messages, empty_removed) = remove_empty_messages(messages);
    issues.extend(empty_removed);
    (messages, issues)
}

/// Never merges across visibility or turn-context boundaries, so the result
/// is safe to persist.
pub fn merge_consecutive_messages(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    merge_consecutive(messages, false)
}

/// Merges regardless of visibility, for providers that require strict role
/// alternation. Never persist the result.
pub fn merge_consecutive_messages_for_request(messages: Vec<Message>) -> Vec<Message> {
    merge_consecutive(messages, true).0
}

pub fn merge_consecutive(
    messages: Vec<Message>,
    across_visibility: bool,
) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();
    let mut merged_messages: Vec<Message> = Vec::new();

    for message in messages {
        if let Some(last) = merged_messages.last_mut() {
            let effective = effective_role(&message);
            if effective_role(last) == effective
                && (across_visibility
                    || (last.metadata.user_visible == message.metadata.user_visible
                        && last.metadata.turn_context == message.metadata.turn_context))
            {
                last.content.extend(message.content);
                issues.push(format!("Merged consecutive {} messages", effective));
                continue;
            }
        }
        merged_messages.push(message);
    }

    (merged_messages, issues)
}

/// Drops duplicate signed thinking blocks, keeping the first occurrence. Some
/// signed-replay APIs (like Anthropic) reject a request that repeats the same
/// signed block more than once.
///
/// Duplicates arise two ways, both handled here:
///   - Within one assistant message, when a standalone thinking message is
///     merged with a tool-call message that re-embedded the same thinking.
///   - Across assistant messages, when the agent splits one provider turn into
///     several tool-call messages (interleaved with tool results) that each
///     carry a copy of the turn's signed thinking.
///
/// The `seen` set spans the whole conversation. A signed block carries a
/// cryptographic signature unique to its generation, so an exact (text +
/// signature) match can only be the same turn's thinking copied onto split
/// messages — never two genuinely distinct thoughts. Unsigned reasoning
/// summaries are left untouched, since providers like Kimi/DeepSeek require
/// them echoed on every tool-call message.
///
/// This runs before any provider formatter, so it covers every Claude transport
/// (direct Anthropic, Bedrock, Databricks, Vertex) in one place.
fn dedupe_signed_thinking(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();
    let mut seen: Vec<MessageContentBlock> = Vec::new();

    let fixed_messages = messages
        .into_iter()
        .map(|mut message| {
            if message.role != Role::Assistant {
                return message;
            }

            let original_len = message.content.len();
            let mut deduped: Vec<MessageContentBlock> = Vec::with_capacity(original_len);
            for content in &message.content {
                let is_signed = is_signed_thinking(content);
                if is_signed && seen.contains(content) {
                    continue;
                }
                if is_signed {
                    seen.push(content.clone());
                }
                deduped.push(content.clone());
            }

            if deduped.len() != original_len {
                issues.push("Removed duplicate signed thinking block".to_string());
                message.content = deduped;
            }
            message
        })
        .collect();

    (fixed_messages, issues)
}

fn fix_lead_trail(mut messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();

    if let Some(first) = messages.first() {
        if first.role == Role::Assistant {
            messages.remove(0);
            issues.push("Removed leading assistant message".to_string());
        }
    }

    if let Some(last) = messages.last() {
        if last.role == Role::Assistant {
            messages.pop();
            issues.push("Removed trailing assistant message".to_string());
        }
    }

    (messages, issues)
}

const PLACEHOLDER_USER_MESSAGE: &str = "Hello";

fn populate_if_empty(mut messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
    let mut issues = Vec::new();

    if messages.is_empty() {
        issues.push("Added placeholder user message to empty conversation".to_string());
        messages.push(Message::user().with_text(PLACEHOLDER_USER_MESSAGE));
    }
    (messages, issues)
}
