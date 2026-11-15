use crate::conversation::message::{Message, MessageContentBlock, MessageMetadata};
use crate::mcp_utils::extract_text_from_resource;
use rmcp::model::{ContentBlock, Role};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::HashSet;
use std::sync::Arc;
use thiserror::Error;

pub mod message;
#[cfg(test)]
mod pre13a_repair;
pub mod token_usage;
mod tool_request;
mod tool_result_serde;

/// The messages of a conversation, each behind an `Arc` (hermes-rs, Phase 13a). Cloning a
/// conversation, taking a snapshot of it for a turn step or a review, or projecting it for a
/// request copies pointers, not messages (like a shallow copy of a Python list). A message is
/// copied only when someone changes it while another holder still has it
/// (`Arc::make_mut`: copy-on-write).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conversation(Vec<Arc<Message>>);

#[derive(Error, Debug)]
#[error("invalid conversation: {reason}")]
pub struct InvalidConversation {
    reason: String,
    conversation: Conversation,
}

impl Conversation {
    pub fn new<I>(messages: I) -> Result<Self, InvalidConversation>
    where
        I: IntoIterator<Item = Message>,
    {
        Self::new_unvalidated(messages).validate()
    }

    pub fn new_unvalidated<I>(messages: I) -> Self
    where
        I: IntoIterator<Item = Message>,
    {
        Self(messages.into_iter().map(Arc::new).collect())
    }

    /// A conversation over messages that are already shared (no copy).
    pub fn from_arcs(messages: Vec<Arc<Message>>) -> Self {
        Self(messages)
    }

    pub fn empty() -> Self {
        Self::new_unvalidated([])
    }

    pub fn messages(&self) -> &[Arc<Message>] {
        &self.0
    }

    /// The list itself, for removing, reordering or truncating. Change a message in place
    /// only through `Arc::make_mut`, so a snapshot that shares it keeps what it saw.
    pub fn messages_mut(&mut self) -> &mut Vec<Arc<Message>> {
        &mut self.0
    }

    /// Plain copies of every message, for the places that hand the history to something that
    /// owns it (the page's transcript, the store's import). A deep copy: not for the request path.
    pub fn to_vec(&self) -> Vec<Message> {
        self.0.iter().map(|message| (**message).clone()).collect()
    }

    pub fn push(&mut self, message: Message) {
        let output_token_limit_reached = message.metadata.output_token_limit_reached;
        if message.content.is_empty()
            && (message.metadata.inference.is_some() || output_token_limit_reached)
        {
            if let Some(existing) = self.0.iter_mut().rev().find(|existing| {
                existing.role == message.role
                    && existing.is_user_visible()
                    && (!output_token_limit_reached
                        || (message.id.is_some()
                            && existing.id.as_deref() == message.id.as_deref()))
            }) {
                let existing = Arc::make_mut(existing);
                if let Some(inference) = message.metadata.inference.clone() {
                    existing.metadata.inference = Some(inference);
                }
                existing.metadata.output_token_limit_reached |= output_token_limit_reached;
                return;
            }

            if output_token_limit_reached {
                self.0.push(Arc::new(message.with_visibility(true, false)));
            }
            return;
        }

        if let Some(last) = self
            .0
            .last_mut()
            .filter(|m| m.id.is_some() && m.id == message.id)
        {
            let last = Arc::make_mut(last);
            if message.metadata.inference.is_some() {
                last.metadata.inference = message.metadata.inference.clone();
            }
            last.metadata.output_token_limit_reached |= message.metadata.output_token_limit_reached;
            match (last.content.last_mut(), message.content.last()) {
                (
                    Some(MessageContentBlock::Text(ref mut last)),
                    Some(MessageContentBlock::Text(new)),
                ) if message.content.len() == 1
                    && last.annotations.as_ref().and_then(|a| a.audience.as_ref())
                        == new.annotations.as_ref().and_then(|a| a.audience.as_ref()) =>
                {
                    last.text.push_str(&new.text);
                }
                (
                    Some(MessageContentBlock::Thinking(ref mut last)),
                    Some(MessageContentBlock::Thinking(new)),
                ) if message.content.len() == 1
                    && (last.signature.is_empty() || new.signature == last.signature) =>
                {
                    // Merge cases:
                    //   - `last` is still unsigned (block in progress) — append
                    //     and adopt `new.signature` if it's the closing delta.
                    //   - signatures match — same block continuing.
                    // An unsigned delta arriving after a signed block belongs
                    // to the next block (signature-at-end streams emit the
                    // first text of block N+1 before its signature), so the
                    // outer match arm falls through to push it separately.
                    last.thinking.push_str(&new.thinking);
                    if !new.signature.is_empty() {
                        last.signature = new.signature.clone();
                    }
                }
                (_, _) => {
                    last.content.extend(message.content);
                }
            }
        } else {
            self.0.push(Arc::new(message));
        }
    }

    pub fn last(&self) -> Option<&Message> {
        self.0.last().map(|message| &**message)
    }

    pub fn first(&self) -> Option<&Message> {
        self.0.first().map(|message| &**message)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = Message>,
    {
        for message in iter {
            self.push(message);
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Arc<Message>> {
        self.0.iter()
    }

    pub fn pop(&mut self) -> Option<Arc<Message>> {
        self.0.pop()
    }

    pub fn remove(&mut self, index: usize) -> Arc<Message> {
        self.0.remove(index)
    }

    pub fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn filtered_messages<F>(&self, filter: F) -> Vec<Arc<Message>>
    where
        F: Fn(&MessageMetadata) -> bool,
    {
        self.0
            .iter()
            .filter(|msg| filter(&msg.metadata))
            .cloned()
            .collect()
    }

    /// What the model may see of each agent-visible message. A message with nothing hidden
    /// from the model is shared, not copied.
    pub fn agent_visible_messages(&self) -> Vec<Arc<Message>> {
        self.0
            .iter()
            .filter(|message| message.metadata.agent_visible)
            .map(agent_visible_arc)
            .filter(|message| !message.content.is_empty())
            .collect()
    }

    pub fn user_visible_messages(&self) -> Vec<Message> {
        self.0
            .iter()
            .filter(|message| message.metadata.user_visible)
            .map(|message| message.user_visible_content())
            .filter(|message| !message.content.is_empty())
            .collect()
    }

    fn validate(self) -> Result<Self, InvalidConversation> {
        let (_messages, issues) = fix_messages(self.0.clone());
        if !issues.is_empty() {
            let reason = issues.join("\n");
            Err(InvalidConversation {
                reason,
                conversation: self,
            })
        } else {
            Ok(self)
        }
    }
}

/// Plain messages as shared ones, for the helpers that read a conversation's list (and for tests).
pub fn arcs<I: IntoIterator<Item = Message>>(messages: I) -> Vec<Arc<Message>> {
    messages.into_iter().map(Arc::new).collect()
}

/// `Message::agent_visible_content`, sharing the message when that would change nothing.
pub fn agent_visible_arc(message: &Arc<Message>) -> Arc<Message> {
    if message.has_content_hidden_from_agent() {
        Arc::new(message.agent_visible_content())
    } else {
        message.clone()
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::empty()
    }
}

/// Consumes the conversation into plain messages (a message nobody else holds moves out
/// without a copy; a shared one is copied).
impl IntoIterator for Conversation {
    type Item = Message;
    type IntoIter = std::iter::Map<std::vec::IntoIter<Arc<Message>>, fn(Arc<Message>) -> Message>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().map(Arc::unwrap_or_clone)
    }
}
impl<'a> IntoIterator for &'a Conversation {
    type Item = &'a Arc<Message>;
    type IntoIter = std::slice::Iter<'a, Arc<Message>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Fix a conversation that we're about to send to an LLM. So the first and last
/// messages should always be from the user.
pub fn fix_conversation(conversation: Conversation) -> (Conversation, Vec<String>) {
    let all_messages = conversation.messages();

    // Create a shadow map: track each message as either Visible or NonVisible with its index
    enum MessageSlot {
        Visible(usize),           // Index into agent_visible_messages
        NonVisible(Arc<Message>), // Non-visible messages pass through unchanged
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
    let final_messages: Vec<Arc<Message>> = shadow_map
        .into_iter()
        .filter_map(|slot| match slot {
            MessageSlot::Visible(idx) => fixed_visible.get(idx).cloned(),
            MessageSlot::NonVisible(msg) => Some(msg),
        })
        .collect();

    (Conversation::from_arcs(final_messages), issues)
}

/// The repair passes. Each one changes a message only when it has something to repair
/// (`Arc::make_mut`), so the messages of an already-valid conversation stay shared with the
/// conversation they came from.
fn fix_messages(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
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

fn same_audience(a: &rmcp::model::TextContent, b: &rmcp::model::TextContent) -> bool {
    a.annotations.as_ref().and_then(|a| a.audience.as_ref())
        == b.annotations.as_ref().and_then(|a| a.audience.as_ref())
}

/// Whether `merge_text_content_in_message` would join two text blocks of this message. Chains
/// merge into the first block's audience, so one equal adjacent pair is the test.
fn has_mergeable_text(msg: &Message) -> bool {
    msg.role == Role::Assistant
        && msg.content.windows(2).any(|pair| {
            matches!(
                (&pair[0], &pair[1]),
                (MessageContentBlock::Text(a), MessageContentBlock::Text(b)) if same_audience(a, b)
            )
        })
}

fn merge_text_content_in_message(mut msg: Arc<Message>) -> Arc<Message> {
    if !has_mergeable_text(&msg) {
        return msg;
    }
    let message = Arc::make_mut(&mut msg);
    message.content =
        std::mem::take(&mut message.content)
            .into_iter()
            .fold(Vec::new(), |mut content, item| {
                match item {
                    MessageContentBlock::Text(text) => match content.last_mut() {
                        Some(MessageContentBlock::Text(last)) if same_audience(last, &text) => {
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

fn merge_text_content_items(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
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

fn trim_assistant_text_whitespace(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
    let mut issues = Vec::new();

    let fixed_messages = messages
        .into_iter()
        .map(|mut message| {
            let needs_trim = message.role == Role::Assistant
                && message.content.iter().any(|content| {
                    matches!(content, MessageContentBlock::Text(text)
                        if text.text.trim_end().len() != text.text.len())
                });
            if needs_trim {
                for content in &mut Arc::make_mut(&mut message).content {
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

fn remove_empty_messages(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
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
fn fix_empty_tool_results(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
    let mut issues = Vec::new();

    let is_empty_result = |content: &MessageContentBlock| {
        matches!(content, MessageContentBlock::ToolResponse(response)
            if response.tool_result.as_ref().is_ok_and(|result| !has_tool_result_content(&result.content)))
    };

    let fixed_messages = messages
        .into_iter()
        .map(|mut message| {
            if !message.content.iter().any(is_empty_result) {
                return message;
            }
            for content in &mut Arc::make_mut(&mut message).content {
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

fn fix_tool_calling(mut messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
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

        if !content_to_remove.is_empty() {
            let message = Arc::make_mut(message);
            for &idx in content_to_remove.iter().rev() {
                message.content.remove(idx);
            }
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
            if !content_to_remove.is_empty() {
                let message = Arc::make_mut(message);
                for &idx in content_to_remove.iter().rev() {
                    message.content.remove(idx);
                }
            }
        }
    }
    let (messages, empty_removed) = remove_empty_messages(messages);
    issues.extend(empty_removed);
    (messages, issues)
}

/// Never merges across visibility or turn-context boundaries, so the result
/// is safe to persist.
pub fn merge_consecutive_messages(messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
    merge_consecutive(messages, false)
}

/// Merges regardless of visibility, for providers that require strict role
/// alternation. Never persist the result.
pub fn merge_consecutive_messages_for_request(messages: Vec<Arc<Message>>) -> Vec<Arc<Message>> {
    merge_consecutive(messages, true).0
}

fn merge_consecutive(
    messages: Vec<Arc<Message>>,
    across_visibility: bool,
) -> (Vec<Arc<Message>>, Vec<String>) {
    let mut issues = Vec::new();
    let mut merged_messages: Vec<Arc<Message>> = Vec::new();

    for message in messages {
        if let Some(last) = merged_messages.last_mut() {
            let effective = effective_role(&message);
            if effective_role(last) == effective
                && (across_visibility
                    || (last.metadata.user_visible == message.metadata.user_visible
                        && last.metadata.turn_context == message.metadata.turn_context))
            {
                // Only the first of a run of messages is copied (when shared); the rest append to it.
                let last = Arc::make_mut(last);
                match Arc::try_unwrap(message) {
                    Ok(message) => last.content.extend(message.content),
                    Err(message) => last.content.extend(message.content.iter().cloned()),
                }
                issues.push(format!("Merged consecutive {} messages", effective));
                continue;
            }
        }
        merged_messages.push(message);
    }

    (merged_messages, issues)
}

/// Signed thinking carries a signature; redacted thinking is always signed.
/// Signed blocks must be replayed exactly; unsigned reasoning summaries need not.
fn is_signed_thinking(content: &MessageContentBlock) -> bool {
    match content {
        MessageContentBlock::Thinking(t) => !t.signature.is_empty(),
        MessageContentBlock::RedactedThinking(_) => true,
        _ => false,
    }
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
///
/// hermes-rs: two passes, so blocks are compared by reference and only a message that
/// loses a block is copied (the Goose version cloned every signed block into `seen`).
fn dedupe_signed_thinking(mut messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
    let mut issues = Vec::new();
    // (message index, which blocks to keep) for each message that loses one.
    let mut plans: Vec<(usize, Vec<bool>)> = Vec::new();
    {
        let mut seen: Vec<&MessageContentBlock> = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            if message.role != Role::Assistant {
                continue;
            }
            let mut keep = vec![true; message.content.len()];
            let mut dropped = false;
            for (i, content) in message.content.iter().enumerate() {
                let is_signed = is_signed_thinking(content);
                if is_signed && seen.contains(&content) {
                    keep[i] = false;
                    dropped = true;
                    continue;
                }
                if is_signed {
                    seen.push(content);
                }
            }
            if dropped {
                plans.push((index, keep));
            }
        }
    }
    for (index, keep) in plans {
        let message = Arc::make_mut(&mut messages[index]);
        let mut flags = keep.into_iter();
        message.content.retain(|_| flags.next().unwrap_or(true));
        issues.push("Removed duplicate signed thinking block".to_string());
    }

    (messages, issues)
}

fn has_tool_response(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|content| matches!(content, MessageContentBlock::ToolResponse(_)))
}

pub const TURN_CONTEXT_TAG: &str = "turn-context";
pub const CURRENT_TIME_TAG: &str = "current-time";
pub const WORKING_DIRECTORY_TAG: &str = "working-directory";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveRole {
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for EffectiveRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

pub fn effective_role(message: &Message) -> EffectiveRole {
    if message.role == Role::User && has_tool_response(message) {
        EffectiveRole::Tool
    } else {
        match message.role {
            Role::User => EffectiveRole::User,
            Role::Assistant => EffectiveRole::Assistant,
        }
    }
}

fn fix_lead_trail(mut messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
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

fn populate_if_empty(mut messages: Vec<Arc<Message>>) -> (Vec<Arc<Message>>, Vec<String>) {
    let mut issues = Vec::new();

    if messages.is_empty() {
        issues.push("Added placeholder user message to empty conversation".to_string());
        messages.push(Arc::new(
            Message::user().with_text(PLACEHOLDER_USER_MESSAGE),
        ));
    }
    (messages, issues)
}

pub fn debug_conversation_fix<A: Borrow<Message>, B: Borrow<Message>>(
    messages: &[A],
    fixed: &[B],
    issues: &[String],
) -> String {
    let mut output = String::new();

    output.push_str("=== CONVERSATION FIX DEBUG ===\n\n");

    output.push_str("BEFORE:\n");
    for (i, msg) in messages.iter().enumerate() {
        output.push_str(&format!("  [{}] {}\n", i, msg.borrow().debug()));
    }

    output.push_str("\nISSUES FOUND:\n");
    if issues.is_empty() {
        output.push_str("  (none)\n");
    } else {
        for issue in issues {
            output.push_str(&format!("  - {}\n", issue));
        }
    }

    output.push_str("\nAFTER:\n");
    for (i, msg) in fixed.iter().enumerate() {
        output.push_str(&format!("  [{}] {}\n", i, msg.borrow().debug()));
    }

    output.push_str("\n==============================\n");
    output
}

#[cfg(test)]
mod tests {
    use crate::conversation::message::{
        InferenceMetadata, Message, MessageContentBlock, MessageMetadata,
    };
    use crate::conversation::{debug_conversation_fix, fix_conversation, Conversation};
    use rmcp::model::{CallToolRequestParams, Role};
    use rmcp::object;

    macro_rules! assert_has_issues_unordered {
        ($fixed:expr, $issues:expr, $($expected:expr),+ $(,)?) => {
            {
                let mut expected: Vec<&str> = vec![$($expected),+];
                let mut actual: Vec<&str> = $issues.iter().map(|s| s.as_str()).collect();
                expected.sort();
                actual.sort();

                if actual != expected {
                    panic!(
                        "assertion failed: issues don't match\nexpected: {:?}\n  actual: {:?}. Fixed conversation is:\n{:#?}",
                        expected, $issues, $fixed,
                    );
                }
            }
        };
    }

    fn run_verify(messages: Vec<Message>) -> (Vec<Message>, Vec<String>) {
        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages.clone()));

        // Uncomment the following line to print the debug report
        // let report = debug_conversation_fix(&messages, &fixed, &issues);
        // print!("\n{}", report);

        let (_fixed, issues_with_fixed) = fix_conversation(fixed.clone());
        assert_eq!(
            issues_with_fixed.len(),
            0,
            "Fixed conversation should have no issues, but found: {:?}\n\n{}",
            issues_with_fixed,
            debug_conversation_fix(&messages, fixed.messages(), &issues)
        );
        (fixed.to_vec(), issues)
    }

    #[test]
    fn test_valid_conversation() {
        use rmcp::model::ContentBlock;

        let all_messages = [
            Message::user().with_text("Can you help me search for something?"),
            Message::assistant()
                .with_text("I'll help you search.")
                .with_tool_request(
                    "search_1",
                    Ok(CallToolRequestParams::new("web_search")
                        .with_arguments(object!({"query": "rust programming"}))),
                ),
            Message::user().with_tool_response(
                "search_1",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("Search results here"),
                ])),
            ),
            Message::assistant().with_text("Based on the search results, here's what I found..."),
        ];

        for i in 1..=all_messages.len() {
            let messages = Conversation::new_unvalidated(all_messages[..i].to_vec());
            if messages.last().unwrap().role == Role::User {
                let (fixed, issues) = fix_conversation(messages.clone());
                assert_eq!(
                    fixed.len(),
                    messages.len(),
                    "Step {}: Length should match",
                    i
                );
                assert!(
                    issues.is_empty(),
                    "Step {}: Should have no issues, but found: {:?}",
                    i,
                    issues
                );
                assert_eq!(
                    fixed.messages(),
                    messages.messages(),
                    "Step {}: Messages should be unchanged",
                    i
                );
            }
        }
    }

    #[test]
    fn test_role_alternation_and_content_placement_issues() {
        use rmcp::model::ContentBlock;

        let messages = vec![
            Message::user().with_text("Hello"),
            Message::user().with_text("Another user message"),
            Message::assistant()
                .with_text("Response")
                .with_tool_response(
                    "orphan_1",
                    Ok(rmcp::model::CallToolResult::success(vec![
                        ContentBlock::text("result"),
                    ])),
                ), // Wrong role
            Message::assistant().with_thinking("Let me think", "sig"),
            Message::user()
                .with_tool_request(
                    "bad_req",
                    Ok(CallToolRequestParams::new("search").with_arguments(object!({}))),
                )
                .with_text("User with bad tool request"),
        ];

        let (fixed, issues) = run_verify(messages);

        assert_eq!(fixed.len(), 3);

        assert_has_issues_unordered!(
            fixed,
            issues,
            "Merged consecutive assistant messages",
            "Merged consecutive user messages",
            "Removed tool response 'orphan_1' from assistant message",
            "Removed tool request 'bad_req' from user message",
        );

        assert_eq!(fixed[0].role, Role::User);
        assert_eq!(fixed[1].role, Role::Assistant);
        assert_eq!(fixed[2].role, Role::User);

        assert_eq!(fixed[0].content.len(), 2);
    }

    #[test]
    fn merge_does_not_cross_user_visibility() {
        let visible = Message::user().with_text("what is in main.rs?");
        let hidden = Message::user()
            .with_text("<turn-context>frozen</turn-context>")
            .with_visibility(false, true);

        let (fixed, _) = fix_conversation(Conversation::new_unvalidated(vec![visible, hidden]));
        assert_eq!(
            fixed.messages().len(),
            2,
            "the persistable form keeps the agent-only event separate"
        );
        assert!(fixed.messages()[0].is_user_visible());
        assert!(!fixed.messages()[1].is_user_visible());

        let merged =
            crate::conversation::merge_consecutive_messages_for_request(fixed.messages().to_vec());
        assert_eq!(
            merged.len(),
            1,
            "the request form merges to satisfy role alternation"
        );
        assert_eq!(merged[0].content.len(), 2);
    }

    #[test]
    fn merge_does_not_erase_turn_context_marker() {
        let preserved_prompt = Message::user()
            .with_text("the preserved prompt")
            .with_metadata(MessageMetadata::agent_only());
        let carried_event = Message::user()
            .with_text("<turn-context>frozen</turn-context>")
            .with_metadata(MessageMetadata::agent_only().with_turn_context());

        let (fixed, _) = fix_conversation(Conversation::new_unvalidated(vec![
            preserved_prompt,
            carried_event,
        ]));
        assert_eq!(
            fixed.messages().len(),
            2,
            "the persistable form must not fold a turn-context event into the prompt"
        );
        assert!(!fixed.messages()[0].is_turn_context());
        assert!(fixed.messages()[1].is_turn_context());

        let merged =
            crate::conversation::merge_consecutive_messages_for_request(fixed.messages().to_vec());
        assert_eq!(
            merged.len(),
            1,
            "the request form still merges to satisfy role alternation"
        );
    }

    #[test]
    fn test_orphaned_tools_and_empty_messages() {
        use rmcp::model::ContentBlock;

        // This conversation completely collapses. the first user message is invalid
        // then we remove the empty user message and the wrong tool response
        // then we collapse the assistant messages
        // which we then remove because you can't end a conversation with an assistant message
        let messages = vec![
            Message::assistant()
                .with_text("I'll search for you")
                .with_tool_request(
                    "search_1",
                    Ok(CallToolRequestParams::new("search").with_arguments(object!({}))),
                ),
            Message::user(),
            Message::user().with_tool_response(
                "wrong_id",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("result"),
                ])),
            ),
            Message::assistant().with_tool_request(
                "search_2",
                Ok(CallToolRequestParams::new("search").with_arguments(object!({}))),
            ),
        ];

        let (fixed, issues) = run_verify(messages);

        assert_eq!(fixed.len(), 1);

        assert_has_issues_unordered!(
            fixed,
            issues,
            "Removed empty message",
            "Removed orphaned tool response 'wrong_id'",
            "Removed orphaned tool request 'search_1'",
            "Removed orphaned tool request 'search_2'",
            "Removed empty message",
            "Removed empty message",
            "Removed leading assistant message",
            "Added placeholder user message to empty conversation",
        );

        assert_eq!(fixed[0].role, Role::User);
        assert_eq!(fixed[0].as_concat_text(), "Hello");
    }

    #[test]
    fn test_real_world_consecutive_assistant_messages() {
        use rmcp::model::ContentBlock;

        let conversation = Conversation::new_unvalidated(vec![
            Message::user().with_text("run ls in the current directory and then run a word count on the smallest file"),

            Message::assistant()
                .with_text("I'll help you run `ls` in the current directory and then perform a word count on the smallest file. Let me start by listing the directory contents.")
                .with_tool_request("toolu_bdrk_018adWbP4X26CfoJU5hkhu3i", Ok(CallToolRequestParams::new("developer__shell").with_arguments(object!({"command": "ls -la"})))),

            Message::assistant()
                .with_text("Now I'll identify the smallest file by size. Looking at the output, I can see that both `slack.yaml` and `subrecipes.yaml` have a size of 0 bytes, making them the smallest files. I'll run a word count on one of them:")
                .with_tool_request("toolu_bdrk_01KgDYHs4fAodi22NqxRzmwx", Ok(CallToolRequestParams::new("developer__shell").with_arguments(object!({"command": "wc slack.yaml"})))),

            Message::user()
                .with_tool_response("toolu_bdrk_01KgDYHs4fAodi22NqxRzmwx", Ok(rmcp::model::CallToolResult::success(vec![ContentBlock::text("0 0 0 slack.yaml")]))),

            Message::assistant()
                .with_text("I ran `ls -la` in the current directory and found several files. Looking at the file sizes, I can see that both `slack.yaml` and `subrecipes.yaml` are 0 bytes (the smallest files). I ran a word count on `slack.yaml` which shows: **0 lines**, **0 words**, **0 characters**"),
            Message::user().with_text("thanks!"),
        ]);

        let (fixed, issues) = fix_conversation(conversation);

        assert_eq!(fixed.len(), 5);
        assert_has_issues_unordered!(
            fixed,
            issues,
            "Removed orphaned tool request 'toolu_bdrk_018adWbP4X26CfoJU5hkhu3i'",
            "Merged consecutive assistant messages"
        )
    }

    #[test]
    fn test_tool_response_effective_role() {
        use rmcp::model::ContentBlock;

        let messages = vec![
            Message::user().with_text("Search for something"),
            Message::assistant()
                .with_text("I'll search for you")
                .with_tool_request(
                    "search_1",
                    Ok(CallToolRequestParams::new("search").with_arguments(object!({}))),
                ),
            Message::user().with_tool_response(
                "search_1",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("search results"),
                ])),
            ),
            Message::user().with_text("Thanks!"),
        ];

        let (_fixed, issues) = run_verify(messages);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_merge_text_content_items() {
        use crate::conversation::message::MessageContentBlock;
        use rmcp::model::TextContent;

        let mut message = Message::assistant().with_text("Hello");

        message
            .content
            .push(MessageContentBlock::Text(TextContent::new(" world")));
        message
            .content
            .push(MessageContentBlock::Text(TextContent::new("!")));

        let messages = vec![
            Message::user().with_text("hello"),
            message,
            Message::user().with_text("thanks"),
        ];

        let (fixed, issues) = run_verify(messages);

        assert_eq!(fixed.len(), 3);
        assert_has_issues_unordered!(fixed, issues, "Merged text content");

        let fixed_msg = &fixed[1];
        assert_eq!(fixed_msg.content.len(), 1);

        if let MessageContentBlock::Text(text_content) = &fixed_msg.content[0] {
            assert_eq!(text_content.text, "Hello world!");
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_merge_text_content_items_with_mixed_content() {
        use crate::conversation::message::MessageContentBlock;
        use rmcp::model::TextContent;

        let mut image_message = Message::assistant().with_text("Look at");

        image_message
            .content
            .push(MessageContentBlock::Text(TextContent::new(" this image:")));

        image_message = image_message.with_image("", "");

        let messages = vec![
            Message::user().with_text("hello"),
            image_message,
            Message::user().with_text("thanks"),
        ];

        let (fixed, issues) = run_verify(messages);

        assert_eq!(fixed.len(), 3);
        assert_has_issues_unordered!(fixed, issues, "Merged text content");
        let fixed_msg = &fixed[1];

        assert_eq!(fixed_msg.content.len(), 2);
        if let MessageContentBlock::Text(text_content) = &fixed_msg.content[0] {
            assert_eq!(text_content.text, "Look at this image:");
        } else {
            panic!("Expected first item to be text content");
        }

        if let MessageContentBlock::Image(_) = &fixed_msg.content[1] {
            // Good
        } else {
            panic!("Expected second item to be an image");
        }
    }

    #[test]
    fn test_streamed_text_with_different_audiences_is_not_merged() {
        use rmcp::model::{Annotations, Role, TextContent};

        let text = |value: &str, audience| {
            MessageContentBlock::Text(
                TextContent::new(value)
                    .with_annotations(Annotations::default().with_audience(vec![audience])),
            )
        };

        for (first, second) in [(Role::User, Role::Assistant), (Role::Assistant, Role::User)] {
            let mut conversation = Conversation::empty();
            conversation.push(
                Message::assistant()
                    .with_id("stream-1")
                    .with_content(text("first", first.clone())),
            );
            conversation.push(
                Message::assistant()
                    .with_id("stream-1")
                    .with_content(text("second", second)),
            );

            let message = conversation.last().unwrap();
            assert_eq!(message.content.len(), 2);
            assert_eq!(
                message
                    .user_visible_content()
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        MessageContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                if first == Role::User {
                    vec!["first"]
                } else {
                    vec!["second"]
                }
            );
            assert_eq!(
                message
                    .agent_visible_content()
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        MessageContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                if first == Role::Assistant {
                    vec!["first"]
                } else {
                    vec!["second"]
                }
            );
        }
    }

    #[test]
    fn test_user_visible_messages_projects_content_and_drops_hidden_rows() {
        use rmcp::model::{Annotations, Role, TextContent};

        let assistant_only = |value: &str| {
            MessageContentBlock::Text(
                TextContent::new(value)
                    .with_annotations(Annotations::default().with_audience(vec![Role::Assistant])),
            )
        };
        let conversation = Conversation::new_unvalidated([
            Message::assistant()
                .with_content(assistant_only("content hidden by audience"))
                .agent_only(),
            Message::assistant()
                .with_content(assistant_only("private"))
                .with_text("public"),
        ]);

        let projected = conversation.user_visible_messages();

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].as_concat_text(), "public");
    }

    #[test]
    fn test_agent_visible_messages_projects_content_and_drops_hidden_rows() {
        use rmcp::model::{Annotations, Role, TextContent};

        let user_only = |value: &str| {
            MessageContentBlock::Text(
                TextContent::new(value)
                    .with_annotations(Annotations::default().with_audience(vec![Role::User])),
            )
        };
        let conversation = Conversation::new_unvalidated([
            Message::assistant()
                .with_content(user_only("content hidden from agent"))
                .user_only(),
            Message::assistant()
                .with_content(user_only("private from agent"))
                .with_text("shared with agent"),
        ]);

        let projected = conversation.agent_visible_messages();

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].as_concat_text(), "shared with agent");
    }

    #[test]
    fn test_agent_visible_non_visible_message_ordering_with_fixes() {
        // Test that non-visible messages maintain their position relative to visible messages
        // even when visible messages are fixed (merged, removed, etc.)

        // Create messages with mixed visibility where visible ones need fixing
        let mut msg1_user = Message::user().with_text("First user message");
        msg1_user.metadata.agent_visible = true;

        let mut msg2_non_visible = Message::user().with_text("Non-visible note 1");
        msg2_non_visible.metadata.agent_visible = false;

        // These two consecutive user messages should be merged (triggering a fix)
        let mut msg3_user = Message::user().with_text("Second user message");
        msg3_user.metadata.agent_visible = true;

        let mut msg4_user = Message::user().with_text("Third user message");
        msg4_user.metadata.agent_visible = true;

        let mut msg5_non_visible = Message::user().with_text("Non-visible note 2");
        msg5_non_visible.metadata.agent_visible = false;

        let mut msg6_assistant = Message::assistant().with_text("Assistant response");
        msg6_assistant.metadata.agent_visible = true;

        let mut msg7_non_visible = Message::user().with_text("Non-visible note 3");
        msg7_non_visible.metadata.agent_visible = false;

        let mut msg8_user = Message::user().with_text("Final user message");
        msg8_user.metadata.agent_visible = true;

        let messages = vec![
            msg1_user.clone(),
            msg2_non_visible.clone(),
            msg3_user.clone(),
            msg4_user.clone(),
            msg5_non_visible.clone(),
            msg6_assistant.clone(),
            msg7_non_visible.clone(),
            msg8_user.clone(),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages.clone()));

        // Should have merged consecutive user messages
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|i| i.contains("Merged consecutive")));

        let fixed_messages = fixed.messages();

        // Verify non-visible messages are still present
        let non_visible_texts: Vec<String> = fixed_messages
            .iter()
            .filter(|m| !m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(non_visible_texts.len(), 3);
        assert_eq!(non_visible_texts[0], "Non-visible note 1");
        assert_eq!(non_visible_texts[1], "Non-visible note 2");
        assert_eq!(non_visible_texts[2], "Non-visible note 3");

        // Verify visible messages were processed
        let visible_texts: Vec<String> = fixed_messages
            .iter()
            .filter(|m| m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        // Should have 3 visible messages: first user, merged user messages, assistant, final user
        // But after merging consecutive users and fixing lead/trail, we get fewer
        assert!(!visible_texts.is_empty());

        // The key assertion: non-visible messages should be preserved and not reordered
        // relative to each other
        let mut found_note1 = false;
        let mut found_note2 = false;

        for msg in fixed_messages {
            let text = msg.as_concat_text();
            if text == "Non-visible note 1" {
                assert!(!found_note2 && !found_note1);
                found_note1 = true;
            } else if text == "Non-visible note 2" {
                assert!(found_note1 && !found_note2);
                found_note2 = true;
            } else if text == "Non-visible note 3" {
                assert!(found_note1 && found_note2);
            }
        }
    }

    #[test]
    fn test_shadow_map_with_multiple_consecutive_merges() {
        // Test the shadow map handles multiple consecutive visible messages that all merge
        let mut msg1 = Message::user().with_text("User 1");
        msg1.metadata.agent_visible = true;

        let mut msg2_non_vis = Message::user().with_text("Non-visible A");
        msg2_non_vis.metadata.agent_visible = false;

        let mut msg3 = Message::user().with_text("User 2");
        msg3.metadata.agent_visible = true;

        let mut msg4 = Message::user().with_text("User 3");
        msg4.metadata.agent_visible = true;

        let mut msg5 = Message::user().with_text("User 4");
        msg5.metadata.agent_visible = true;

        let mut msg6_non_vis = Message::user().with_text("Non-visible B");
        msg6_non_vis.metadata.agent_visible = false;

        let messages = vec![
            msg1,
            msg2_non_vis.clone(),
            msg3,
            msg4,
            msg5,
            msg6_non_vis.clone(),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        // Should have merged the consecutive user messages
        assert!(issues.iter().any(|i| i.contains("Merged consecutive")));

        let fixed_messages = fixed.messages();

        // Non-visible messages should still be present and in order
        let non_visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| !m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(non_visible.len(), 2);
        assert_eq!(non_visible[0], "Non-visible A");
        assert_eq!(non_visible[1], "Non-visible B");

        // The merged message should contain all the user texts
        let visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(visible.len(), 1);
        assert!(visible[0].contains("User 1"));
        assert!(visible[0].contains("User 2"));
        assert!(visible[0].contains("User 3"));
        assert!(visible[0].contains("User 4"));
    }

    #[test]
    fn test_shadow_map_with_leading_trailing_removal() {
        // Test that shadow map handles removal of leading/trailing assistant messages
        let mut msg1_assistant = Message::assistant().with_text("Leading assistant");
        msg1_assistant.metadata.agent_visible = true;

        let mut msg2_non_vis = Message::user().with_text("Non-visible note");
        msg2_non_vis.metadata.agent_visible = false;

        let mut msg3_user = Message::user().with_text("User message");
        msg3_user.metadata.agent_visible = true;

        let mut msg4_assistant = Message::assistant().with_text("Assistant response");
        msg4_assistant.metadata.agent_visible = true;

        let mut msg5_assistant = Message::assistant().with_text("Trailing assistant");
        msg5_assistant.metadata.agent_visible = true;

        let messages = vec![
            msg1_assistant,
            msg2_non_vis.clone(),
            msg3_user,
            msg4_assistant,
            msg5_assistant,
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        // Should have merged consecutive assistants, removed leading, and removed trailing
        assert!(issues
            .iter()
            .any(|i| i.contains("Merged consecutive assistant")));
        assert!(issues
            .iter()
            .any(|i| i.contains("Removed leading assistant")));
        assert!(issues
            .iter()
            .any(|i| i.contains("Removed trailing assistant")));

        let fixed_messages = fixed.messages();

        // Non-visible message should still be present
        let non_visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| !m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(non_visible.len(), 1);
        assert_eq!(non_visible[0], "Non-visible note");

        // The two consecutive assistant messages get merged, then the merged message
        // is removed as trailing, leaving only the user message
        let visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0], "User message");
    }

    #[test]
    fn test_shadow_map_all_visible_messages_removed() {
        // Edge case: all visible messages are removed, only non-visible remain
        let mut msg1_assistant = Message::assistant().with_text("Only assistant");
        msg1_assistant.metadata.agent_visible = true;

        let mut msg2_non_vis = Message::user().with_text("Non-visible note 1");
        msg2_non_vis.metadata.agent_visible = false;

        let mut msg3_non_vis = Message::user().with_text("Non-visible note 2");
        msg3_non_vis.metadata.agent_visible = false;

        let messages = vec![msg1_assistant, msg2_non_vis, msg3_non_vis];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        // Should have removed the assistant and added placeholder
        assert!(issues
            .iter()
            .any(|i| i.contains("Removed leading assistant")));
        assert!(issues.iter().any(|i| i.contains("Added placeholder")));

        let fixed_messages = fixed.messages();

        // Non-visible messages should still be present
        let non_visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| !m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(non_visible.len(), 2);
        assert_eq!(non_visible[0], "Non-visible note 1");
        assert_eq!(non_visible[1], "Non-visible note 2");

        // Should have placeholder user message
        let visible: Vec<String> = fixed_messages
            .iter()
            .filter(|m| m.metadata.agent_visible)
            .map(|m| m.as_concat_text())
            .collect();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0], "Hello");
    }

    #[test]
    fn test_empty_tool_result_gets_placeholder() {
        // Test that tool results with empty content get a placeholder added
        let messages = vec![
            Message::user().with_text("Search for something"),
            Message::assistant()
                .with_text("I'll search for you")
                .with_tool_request(
                    "search_1",
                    Ok(CallToolRequestParams::new("search").with_arguments(object!({}))),
                ),
            Message::user().with_tool_response(
                "search_1",
                Ok(rmcp::model::CallToolResult::success(vec![])), // Empty content - this should get a placeholder
            ),
            Message::user().with_text("Thanks!"),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        // Should have added a placeholder
        assert!(issues
            .iter()
            .any(|i| i.contains("Added placeholder to empty tool result")));

        // Find the tool response and verify it has content now
        let tool_response_msg = fixed
            .messages()
            .iter()
            .find(|m| {
                m.content.iter().any(|c| {
                    matches!(
                        c,
                        crate::conversation::message::MessageContentBlock::ToolResponse(_)
                    )
                })
            })
            .expect("Should have a tool response message");

        if let crate::conversation::message::MessageContentBlock::ToolResponse(resp) =
            &tool_response_msg.content[0]
        {
            if let Ok(result) = &resp.tool_result {
                assert!(!result.content.is_empty(), "Content should not be empty");
                // Verify the placeholder text
                let text = result.content[0]
                    .as_text()
                    .expect("Should be text content")
                    .text
                    .clone();
                assert_eq!(text, "(empty result)");
            } else {
                panic!("Tool result should be Ok");
            }
        } else {
            panic!("First content should be ToolResponse");
        }
    }

    #[test]
    fn test_shadow_map_preserves_interleaving_pattern() {
        // Test that complex interleaving patterns are preserved
        let mut msg1_user = Message::user().with_text("User 1");
        msg1_user.metadata.agent_visible = true;

        let mut msg2_non_vis = Message::user().with_text("Non-vis A");
        msg2_non_vis.metadata.agent_visible = false;

        let mut msg3_assistant = Message::assistant().with_text("Assistant 1");
        msg3_assistant.metadata.agent_visible = true;

        let mut msg4_non_vis = Message::user().with_text("Non-vis B");
        msg4_non_vis.metadata.agent_visible = false;

        let mut msg5_user = Message::user().with_text("User 2");
        msg5_user.metadata.agent_visible = true;

        let mut msg6_non_vis = Message::user().with_text("Non-vis C");
        msg6_non_vis.metadata.agent_visible = false;

        let messages = vec![
            msg1_user,
            msg2_non_vis,
            msg3_assistant,
            msg4_non_vis,
            msg5_user,
            msg6_non_vis,
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        // Should have no issues for this valid conversation
        assert!(issues.is_empty());

        let fixed_messages = fixed.messages();

        // Verify the interleaving pattern is preserved
        assert_eq!(fixed_messages.len(), 6);

        assert_eq!(fixed_messages[0].as_concat_text(), "User 1");
        assert!(fixed_messages[0].metadata.agent_visible);

        assert_eq!(fixed_messages[1].as_concat_text(), "Non-vis A");
        assert!(!fixed_messages[1].metadata.agent_visible);

        assert_eq!(fixed_messages[2].as_concat_text(), "Assistant 1");
        assert!(fixed_messages[2].metadata.agent_visible);

        assert_eq!(fixed_messages[3].as_concat_text(), "Non-vis B");
        assert!(!fixed_messages[3].metadata.agent_visible);

        assert_eq!(fixed_messages[4].as_concat_text(), "User 2");
        assert!(fixed_messages[4].metadata.agent_visible);

        assert_eq!(fixed_messages[5].as_concat_text(), "Non-vis C");
        assert!(!fixed_messages[5].metadata.agent_visible);
    }

    #[test]
    fn test_dedupes_duplicate_signed_thinking_around_tool_call() {
        use crate::conversation::message::MessageContentBlock;
        use rmcp::model::ContentBlock;

        // Reproduces the Anthropic 400 scenario: a standalone signed thinking
        // message immediately followed by an assistant message that repeats the
        // same signed thinking plus a tool_use. After merging consecutive
        // assistant messages these become two adjacent identical thinking blocks.
        let messages = vec![
            Message::user().with_text("Do the thing"),
            Message::assistant().with_thinking("Let me think about this", "sig-1"),
            Message::assistant()
                .with_thinking("Let me think about this", "sig-1")
                .with_tool_request(
                    "tool_1",
                    Ok(CallToolRequestParams::new("do_thing").with_arguments(object!({"x": 1}))),
                ),
            Message::user().with_tool_response(
                "tool_1",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("done"),
                ])),
            ),
            Message::user().with_text("Now continue"),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        assert!(
            issues
                .iter()
                .any(|i| i == "Removed duplicate signed thinking block"),
            "expected dedupe issue, got: {:?}",
            issues
        );

        let fixed_messages = fixed.messages();
        let assistant = fixed_messages
            .iter()
            .find(|m| {
                m.role == Role::Assistant
                    && m.content
                        .iter()
                        .any(|c| matches!(c, MessageContentBlock::ToolRequest(_)))
            })
            .expect("assistant tool-call message should exist");

        let thinking_count = assistant
            .content
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    MessageContentBlock::Thinking(_) | MessageContentBlock::RedactedThinking(_)
                )
            })
            .count();
        assert_eq!(
            thinking_count, 1,
            "duplicate signed thinking should be collapsed to one block"
        );
    }

    #[test]
    fn test_keeps_distinct_signed_thinking_blocks_in_assistant_message() {
        use crate::conversation::message::MessageContentBlock;
        use rmcp::model::ContentBlock;

        // Distinct signed thinking blocks (different signatures) must be preserved.
        let messages = vec![
            Message::user().with_text("Do the thing"),
            Message::assistant()
                .with_thinking("First thought", "sig-A")
                .with_thinking("Second thought", "sig-B")
                .with_tool_request(
                    "tool_1",
                    Ok(CallToolRequestParams::new("do_thing").with_arguments(object!({"x": 1}))),
                ),
            Message::user().with_tool_response(
                "tool_1",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("done"),
                ])),
            ),
            Message::user().with_text("Now continue"),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        assert!(
            !issues
                .iter()
                .any(|i| i == "Removed duplicate signed thinking block"),
            "should not dedupe distinct thinking blocks, got: {:?}",
            issues
        );

        let fixed_messages = fixed.messages();
        let thinking_count = fixed_messages[1]
            .content
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    MessageContentBlock::Thinking(_) | MessageContentBlock::RedactedThinking(_)
                )
            })
            .count();
        assert_eq!(thinking_count, 2, "distinct thinking blocks must be kept");
    }

    #[test]
    fn test_keeps_duplicate_unsigned_thinking_blocks() {
        use crate::conversation::message::MessageContentBlock;

        // Unsigned thinking (reasoning summaries from non-Anthropic providers)
        // can legitimately repeat and must not be dropped, since only signed
        // blocks trigger the Anthropic exact-replay 400.
        let messages = vec![
            Message::user().with_text("Do the thing"),
            Message::assistant()
                .with_thinking("same reasoning", "")
                .with_thinking("same reasoning", ""),
            Message::user().with_text("Now continue"),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        assert!(
            !issues
                .iter()
                .any(|i| i == "Removed duplicate signed thinking block"),
            "unsigned thinking must not be deduped, got: {:?}",
            issues
        );

        let fixed_messages = fixed.messages();
        let thinking_count = fixed_messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter(|c| matches!(c, MessageContentBlock::Thinking(_)))
            .count();
        assert_eq!(
            thinking_count, 2,
            "duplicate unsigned thinking blocks must be kept"
        );
    }

    #[test]
    fn test_dedupes_signed_thinking_across_split_tool_messages() {
        use crate::conversation::message::MessageContentBlock;
        use rmcp::model::ContentBlock;

        // The agent splits one provider turn with multiple tool calls into one
        // assistant message per call (interleaved with tool results), each
        // carrying the same signed thinking. merge_consecutive_messages cannot
        // merge them because tool results sit between, so the dedupe must span
        // messages and keep the signed block only on the first.
        let messages = vec![
            Message::user().with_text("Use both tools"),
            Message::assistant()
                .with_thinking("multi-tool reasoning", "sig-1")
                .with_tool_request(
                    "call_1",
                    Ok(CallToolRequestParams::new("tool_a").with_arguments(object!({"p": 1}))),
                ),
            Message::user().with_tool_response(
                "call_1",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("ok"),
                ])),
            ),
            Message::assistant()
                .with_thinking("multi-tool reasoning", "sig-1")
                .with_tool_request(
                    "call_2",
                    Ok(CallToolRequestParams::new("tool_b").with_arguments(object!({"p": 2}))),
                ),
            Message::user().with_tool_response(
                "call_2",
                Ok(rmcp::model::CallToolResult::success(vec![
                    ContentBlock::text("ok"),
                ])),
            ),
            Message::user().with_text("Now continue"),
        ];

        let (fixed, issues) = fix_conversation(Conversation::new_unvalidated(messages));

        assert!(
            issues
                .iter()
                .any(|i| i == "Removed duplicate signed thinking block"),
            "expected cross-message dedupe issue, got: {:?}",
            issues
        );

        let fixed_messages = fixed.messages();
        let total_thinking = fixed_messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter(|c| {
                matches!(
                    c,
                    MessageContentBlock::Thinking(_) | MessageContentBlock::RedactedThinking(_)
                )
            })
            .count();
        assert_eq!(
            total_thinking, 1,
            "the repeated signed block must survive only once across the turn"
        );

        let total_tool_requests = fixed_messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter(|c| matches!(c, MessageContentBlock::ToolRequest(_)))
            .count();
        assert_eq!(total_tool_requests, 2, "both tool calls must be preserved");

        // The first split message keeps the thinking; the second loses it.
        assert!(
            fixed_messages[1]
                .content
                .iter()
                .any(|c| matches!(c, MessageContentBlock::Thinking(_))),
            "first split message keeps signed thinking"
        );
    }

    #[test]
    fn test_push_coalesces_thinking_deltas() {
        use crate::conversation::message::MessageContentBlock;

        let mut conv = Conversation::empty();
        for fragment in ["I ", "should ", "think ", "about ", "this."] {
            conv.push(
                Message::assistant()
                    .with_thinking(fragment, "")
                    .with_id("turn-1"),
            );
        }

        assert_eq!(conv.messages().len(), 1);
        let content = &conv.messages()[0].content;
        assert_eq!(content.len(), 1);
        match &content[0] {
            MessageContentBlock::Thinking(t) => {
                assert_eq!(t.thinking, "I should think about this.");
                assert_eq!(t.signature, "");
            }
            other => panic!("expected single Thinking block, got {:?}", other),
        }
    }

    #[test]
    fn test_push_merges_empty_output_token_limit_update_by_id() {
        let mut conv = Conversation::empty();
        conv.push(Message::assistant().with_text("first").with_id("turn-1"));

        let inference = InferenceMetadata {
            provider: "test-provider".to_string(),
            requested_model: "test-model".to_string(),
            resolved_model: None,
            provider_session_id: None,
        };
        let mut limited = Message::assistant()
            .with_id("turn-1")
            .with_inference(inference.clone());
        limited.metadata.output_token_limit_reached = true;
        conv.push(limited);

        assert_eq!(conv.messages().len(), 1);
        assert!(conv.messages()[0].metadata.output_token_limit_reached);
        assert_eq!(conv.messages()[0].metadata.inference, Some(inference));
    }

    #[test]
    fn test_push_retains_unmatched_output_token_limit_update_for_user_only() {
        let mut conv = Conversation::empty();
        let mut limited = Message::assistant().with_id("turn-1");
        limited.metadata.output_token_limit_reached = true;

        conv.push(limited);

        assert_eq!(conv.messages().len(), 1);
        let persisted = &conv.messages()[0];
        assert!(persisted.content.is_empty());
        assert!(persisted.metadata.user_visible);
        assert!(!persisted.metadata.agent_visible);
        assert!(persisted.metadata.output_token_limit_reached);
        assert!(conv.agent_visible_messages().is_empty());
    }

    #[test]
    fn test_push_thinking_adopts_signature_on_closing_delta() {
        use crate::conversation::message::MessageContentBlock;

        let mut conv = Conversation::empty();
        // Streamed shape for one signed block: text deltas accumulate while
        // unsigned; the closing delta carries the signature.
        conv.push(
            Message::assistant()
                .with_thinking("a", "")
                .with_id("turn-1"),
        );
        conv.push(
            Message::assistant()
                .with_thinking("b", "sig1")
                .with_id("turn-1"),
        );

        let content = &conv.messages()[0].content;
        assert_eq!(content.len(), 1);
        match &content[0] {
            MessageContentBlock::Thinking(t) => {
                assert_eq!(t.thinking, "ab");
                assert_eq!(t.signature, "sig1");
            }
            other => panic!("expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn test_push_unsigned_thinking_after_signed_starts_new_block() {
        use crate::conversation::message::MessageContentBlock;

        let mut conv = Conversation::empty();
        conv.push(
            Message::assistant()
                .with_thinking("first body", "sig1")
                .with_id("turn-1"),
        );
        // A second thinking block begins; in signature-at-end streams the
        // first text arrives before the block's signature, so the new
        // unsigned delta must NOT be appended to the already-signed block —
        // otherwise the closing signature later would replay text under
        // the wrong signature.
        conv.push(
            Message::assistant()
                .with_thinking("second body start", "")
                .with_id("turn-1"),
        );

        let content = &conv.messages()[0].content;
        assert_eq!(
            content.len(),
            2,
            "unsigned delta must not merge into a signed block: {:?}",
            content
        );
        match (&content[0], &content[1]) {
            (MessageContentBlock::Thinking(a), MessageContentBlock::Thinking(b)) => {
                assert_eq!(a.thinking, "first body");
                assert_eq!(a.signature, "sig1");
                assert_eq!(b.thinking, "second body start");
                assert_eq!(b.signature, "");
            }
            other => panic!("unexpected content shape: {:?}", other),
        }
    }

    #[test]
    fn test_push_keeps_distinct_signed_thinking_blocks_separate() {
        use crate::conversation::message::MessageContentBlock;

        let mut conv = Conversation::empty();
        conv.push(
            Message::assistant()
                .with_thinking("block A", "sig-A")
                .with_id("turn-1"),
        );
        conv.push(
            Message::assistant()
                .with_thinking("block B", "sig-B")
                .with_id("turn-1"),
        );

        let content = &conv.messages()[0].content;
        assert_eq!(
            content.len(),
            2,
            "two distinct signed blocks must not coalesce: {:?}",
            content
        );
        match (&content[0], &content[1]) {
            (MessageContentBlock::Thinking(a), MessageContentBlock::Thinking(b)) => {
                assert_eq!(a.thinking, "block A");
                assert_eq!(a.signature, "sig-A");
                assert_eq!(b.thinking, "block B");
                assert_eq!(b.signature, "sig-B");
            }
            other => panic!("unexpected content shape: {:?}", other),
        }
    }

    #[test]
    fn test_push_does_not_coalesce_multi_block_thinking_message() {
        use crate::conversation::message::MessageContentBlock;

        let mut conv = Conversation::empty();
        conv.push(
            Message::assistant()
                .with_thinking("first", "")
                .with_id("turn-1"),
        );

        // Multi-block message must NOT coalesce into the existing thinking
        // block — the merge arm requires `message.content.len() == 1`.
        let mut multi = Message::assistant().with_thinking("second", "");
        multi = multi.with_text("and now text").with_id("turn-1");
        conv.push(multi);

        let content = &conv.messages()[0].content;
        assert_eq!(content.len(), 3);
        match (&content[0], &content[1], &content[2]) {
            (
                MessageContentBlock::Thinking(a),
                MessageContentBlock::Thinking(b),
                MessageContentBlock::Text(c),
            ) => {
                assert_eq!(a.thinking, "first");
                assert_eq!(b.thinking, "second");
                assert_eq!(c.text, "and now text");
            }
            other => panic!("unexpected content shape: {:?}", other),
        }
    }

    // ---- Phase 13a: messages are shared, copied only when changed ----

    fn sample() -> Conversation {
        Conversation::new_unvalidated([
            Message::user().with_text("question"),
            Message::assistant().with_id("a1").with_text("par"),
            Message::user().with_text("follow-up"),
        ])
    }

    #[test]
    fn a_clone_shares_the_messages_and_keeps_what_it_saw() {
        let mut live = sample();
        let snapshot = live.clone();
        for (a, b) in live.iter().zip(snapshot.iter()) {
            assert!(std::sync::Arc::ptr_eq(a, b));
        }

        // A streamed chunk merges into the last message of the live conversation only.
        live.push(Message::assistant().with_id("a2").with_text("one"));
        let before = live.clone();
        live.push(Message::assistant().with_id("a2").with_text(" two"));
        assert_eq!(live.last().unwrap().as_concat_text(), "one two");
        assert_eq!(before.last().unwrap().as_concat_text(), "one");
        assert_eq!(snapshot.len(), 3);

        // An edit through make_mut copies that message; the others stay shared.
        let before = live.clone();
        std::sync::Arc::make_mut(&mut live.messages_mut()[1])
            .metadata
            .user_visible = false;
        assert!(before.messages()[1].metadata.user_visible);
        assert!(!live.messages()[1].metadata.user_visible);
        assert!(std::sync::Arc::ptr_eq(
            &before.messages()[0],
            &live.messages()[0]
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &before.messages()[1],
            &live.messages()[1]
        ));
    }

    #[test]
    fn a_message_nobody_else_holds_is_changed_in_place() {
        let mut live = Conversation::new_unvalidated([
            Message::user().with_text("question"),
            Message::assistant().with_id("a1").with_text("par"),
        ]);
        let address = std::sync::Arc::as_ptr(&live.messages()[1]);
        live.push(Message::assistant().with_id("a1").with_text("tial"));
        assert_eq!(live.messages()[1].as_concat_text(), "partial");
        assert_eq!(std::sync::Arc::as_ptr(&live.messages()[1]), address);
    }

    #[test]
    fn repairing_a_valid_conversation_copies_nothing() {
        let conversation = sample();
        let (fixed, issues) = fix_conversation(conversation.clone());
        assert!(issues.is_empty());
        for (a, b) in conversation.iter().zip(fixed.iter()) {
            assert!(std::sync::Arc::ptr_eq(a, b));
        }
    }

    #[test]
    fn repairing_copies_only_the_messages_it_changes() {
        let conversation = Conversation::new_unvalidated([
            Message::user().with_text("question"),
            Message::assistant().with_text("trailing   "),
            Message::user().with_text("follow-up"),
        ]);
        let (fixed, issues) = fix_conversation(conversation.clone());
        assert_has_issues_unordered!(
            fixed,
            issues,
            "Trimmed trailing whitespace from assistant message"
        );
        assert_eq!(fixed.messages()[1].as_concat_text(), "trailing");
        assert_eq!(conversation.messages()[1].as_concat_text(), "trailing   ");
        assert!(std::sync::Arc::ptr_eq(
            &conversation.messages()[0],
            &fixed.messages()[0]
        ));
        assert!(std::sync::Arc::ptr_eq(
            &conversation.messages()[2],
            &fixed.messages()[2]
        ));
    }

    #[test]
    fn merging_consecutive_messages_leaves_the_originals_alone() {
        let conversation = Conversation::new_unvalidated([
            Message::user().with_text("one"),
            Message::user().with_text("two"),
        ]);
        let merged = crate::conversation::merge_consecutive_messages_for_request(
            conversation.messages().to_vec(),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content.len(), 2);
        assert_eq!(conversation.messages()[0].content.len(), 1);
    }

    #[test]
    fn the_agent_view_shares_a_message_with_nothing_hidden() {
        use rmcp::model::{Annotations, TextContent};
        let user_only = MessageContentBlock::Text(
            TextContent::new("for the user")
                .with_annotations(Annotations::default().with_audience(vec![Role::User])),
        );
        let both = MessageContentBlock::Text(TextContent::new("for both").with_annotations(
            Annotations::default().with_audience(vec![Role::User, Role::Assistant]),
        ));
        let hidden = std::sync::Arc::new(Message::assistant().with_content(user_only));
        let shown = std::sync::Arc::new(
            Message::assistant()
                .with_content(both)
                .with_text("plain")
                .with_thinking("t", "sig"),
        );
        // The shortcut agrees with the projection it stands for.
        for message in [&hidden, &shown] {
            assert_eq!(
                message.has_content_hidden_from_agent(),
                message.agent_visible_content() != **message,
            );
        }
        assert!(hidden.has_content_hidden_from_agent());
        assert!(!shown.has_content_hidden_from_agent());
        assert!(std::sync::Arc::ptr_eq(
            &crate::conversation::agent_visible_arc(&shown),
            &shown
        ));
        let projected = crate::conversation::agent_visible_arc(&hidden);
        assert!(projected.content.is_empty());
        assert_eq!(hidden.content.len(), 1);
    }

    #[test]
    fn a_tool_result_part_for_the_user_only_counts_as_hidden() {
        use rmcp::model::{Annotations, CallToolResult, ContentBlock, TextContent};
        let private = ContentBlock::Text(
            TextContent::new("secret")
                .with_annotations(Annotations::default().with_audience(vec![Role::User])),
        );
        let message = Message::user().with_tool_response(
            "c",
            Ok(CallToolResult::success(vec![
                private,
                ContentBlock::text("ok"),
            ])),
        );
        assert!(message.has_content_hidden_from_agent());
        assert_ne!(message.agent_visible_content(), message);
        let plain = Message::user().with_tool_response(
            "c",
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")])),
        );
        assert!(!plain.has_content_hidden_from_agent());
        assert_eq!(plain.agent_visible_content(), plain);
    }

    /// Phase 13a review: the copy-on-write repair passes against HEAD's copying ones
    /// (`pre13a_repair`), over thousands of generated conversations mixing every shape the
    /// passes look at (audiences, empty and whitespace text, images, tool calls with answered,
    /// orphaned and misplaced results, empty and error results, signed / unsigned / redacted
    /// thinking, hidden messages, turn context). Same messages, same issues in the same order,
    /// and the input conversation is left as it was.
    #[test]
    fn copy_on_write_repair_matches_the_copying_passes() {
        use crate::conversation::{merge_consecutive, pre13a_repair};
        use rmcp::model::{
            Annotations, CallToolResult, ContentBlock, ErrorCode, ErrorData, TextContent,
        };

        // xorshift: deterministic, no dependency.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: usize| -> usize {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let audiences: [Option<Vec<Role>>; 5] = [
            None,
            Some(vec![]),
            Some(vec![Role::User]),
            Some(vec![Role::Assistant]),
            Some(vec![Role::User, Role::Assistant]),
        ];
        let annotations = |audience: &Option<Vec<Role>>| {
            audience
                .clone()
                .map(|roles| Annotations::default().with_audience(roles))
        };
        let texts = ["", " ", "a", "b ", "x\n", "same"];
        let ids = ["c1", "c2", "c3"];

        let mut generated = 0;
        let mut kinds = std::collections::BTreeSet::new();
        for _ in 0..4000 {
            let mut messages = Vec::new();
            for _ in 0..next(7) {
                let mut message = if next(2) == 0 {
                    Message::user()
                } else {
                    Message::assistant()
                };
                for _ in 0..next(5) {
                    let block = match next(9) {
                        0 | 1 => {
                            let mut text = TextContent::new(texts[next(texts.len())]);
                            text.annotations = annotations(&audiences[next(audiences.len())]);
                            MessageContentBlock::Text(text)
                        }
                        2 => {
                            let mut image = MessageContentBlock::image("aGk=", "image/png");
                            if let MessageContentBlock::Image(image) = &mut image {
                                image.annotations = annotations(&audiences[next(audiences.len())]);
                            }
                            image
                        }
                        3 => MessageContentBlock::tool_request(
                            ids[next(ids.len())],
                            Ok(CallToolRequestParams::new("terminal")),
                        ),
                        4 => {
                            let result = match next(6) {
                                0 => Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, "boom", None)),
                                1 => Ok(CallToolResult::success(vec![])),
                                2 => Ok(CallToolResult::success(vec![ContentBlock::text("")])),
                                3 => {
                                    let mut private = TextContent::new("secret");
                                    private.annotations =
                                        annotations(&audiences[next(audiences.len())]);
                                    Ok(CallToolResult::success(vec![
                                        ContentBlock::Text(private),
                                        ContentBlock::text("ok"),
                                    ]))
                                }
                                4 => Ok(CallToolResult::success(vec![ContentBlock::image(
                                    "aGk=",
                                    "image/png",
                                )])),
                                _ => Ok(CallToolResult::success(vec![ContentBlock::text("ok")])),
                            };
                            MessageContentBlock::tool_response(ids[next(ids.len())], result)
                        }
                        5 => {
                            let (text, signature) =
                                [("t", "sig"), ("t", "sig"), ("u", "sig2"), ("t", "")][next(4)];
                            MessageContentBlock::thinking(text, signature)
                        }
                        6 => MessageContentBlock::redacted_thinking(["r", "s"][next(2)]),
                        _ => MessageContentBlock::text(texts[next(texts.len())]),
                    };
                    message = message.with_content(block);
                }
                message = match next(8) {
                    0 => message.agent_only(),
                    1 => message.user_only(),
                    2 => {
                        message.metadata.turn_context = true;
                        message
                    }
                    _ => message,
                };
                messages.push(message);
            }
            generated += messages.len();

            // The shortcut agrees with the projection it stands for, message by message.
            for message in &messages {
                assert_eq!(
                    message.has_content_hidden_from_agent(),
                    message.agent_visible_content() != *message,
                    "{message:?}"
                );
            }

            let input = Conversation::new_unvalidated(messages.clone());
            let (fixed, issues) = fix_conversation(input.clone());
            let (old_fixed, old_issues) = pre13a_repair::fix_conversation(&messages);
            assert_eq!(issues, old_issues, "{messages:#?}");
            kinds.extend(
                issues
                    .iter()
                    .map(|issue| issue.split(" '").next().unwrap().to_string()),
            );
            assert_eq!(fixed.to_vec(), old_fixed, "{messages:#?}");
            assert_eq!(input.to_vec(), messages, "the repair changed its input");

            // The request-side helpers too.
            let visible = input.agent_visible_messages();
            let old_visible: Vec<Message> = messages
                .iter()
                .filter(|m| m.metadata.agent_visible)
                .map(Message::agent_visible_content)
                .filter(|m| !m.content.is_empty())
                .collect();
            assert_eq!(
                Conversation::from_arcs(visible.clone()).to_vec(),
                old_visible
            );
            for across in [false, true] {
                let merged = merge_consecutive(visible.clone(), across);
                let old = pre13a_repair::merge_consecutive(old_visible.clone(), across);
                assert_eq!(merged.1, old.1);
                assert_eq!(Conversation::from_arcs(merged.0).to_vec(), old.0);
            }
            assert_eq!(input.to_vec(), messages, "merging changed its input");
        }
        assert!(generated > 10_000);
        // Every pass had work to do somewhere in the sample.
        for kind in [
            "Merged text content",
            "Trimmed trailing whitespace from assistant message",
            "Removed empty message",
            "Added placeholder to empty tool result",
            "Removed tool request",
            "Removed thinking content from user message",
            "Removed orphaned tool response",
            "Removed tool response",
            "Removed orphaned tool request",
            "Merged consecutive user messages",
            "Merged consecutive assistant messages",
            "Removed duplicate signed thinking block",
            "Removed leading assistant message",
            "Removed trailing assistant message",
            "Added placeholder user message to empty conversation",
        ] {
            assert!(
                kinds.contains(kind),
                "no case exercised {kind:?}: {kinds:?}"
            );
        }
    }
}
