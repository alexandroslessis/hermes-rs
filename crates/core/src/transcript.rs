//! Reading a stored conversation the way the user sees it: the visible entries (what was typed,
//! the replies with their reasoning, each tool call with its result, the boundary a provider error
//! left), the rows a cut or a rewind names, the turn a finished prompt wrote, and the error a turn
//! ended on with the words that explain it. Typed; the Tauri page's `TranscriptMessage` JSON is
//! built from these in `crates/desktop/src/transcript.rs`.
//!
//! Hermes: `tui_gateway/methods_session.py` (`session.resume`), the failed-turn boundary a
//! provider-error turn leaves (`agent/conversation_loop.py` `_close_durable_failed_turn` +
//! `agent/turn_failure_copy.py` `failed_turn_notice`), `error_surface.py` and
//! `tui_gateway/user_messages.py` (`_TURN_ERROR_CODE_COPY`).

use std::collections::HashMap;

use hermes_llm::conversation::message::{Message, MessageContent, MessageErrorKind};
use rmcp::model::Role;
use serde_json::{Value, json};

/// The error a turn ended on, as `(kind, text)` from its last message (`ops::ExitOnError`
/// yields a provider error as one): the turn is reported like Hermes's failed turn result.
pub fn message_error(messages: &[Message]) -> Option<(MessageErrorKind, String)> {
    let error = messages.last()?.content.iter().find_map(|c| c.as_error())?;
    Some((error.kind, error.message.clone()))
}

/// Which layer of the stack failed and the classifier's code (Hermes's `error_surface.py`), so
/// the error says what happened in plain words and offers the fitting next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorSurface {
    pub layer: &'static str,
    pub code: &'static str,
    pub retryable: bool,
}

/// The [`ErrorSurface`] of a provider error.
pub fn error_surface(kind: MessageErrorKind, text: &str) -> ErrorSurface {
    use MessageErrorKind as K;
    let (layer, code, retryable) = match kind {
        K::Authentication => ("auth", "auth", false),
        // A quota limit (usage/daily/monthly reached) reproduces on retry; a transient rate
        // limit clears on its own — the same classification `should_retry` uses.
        K::RateLimit => (
            "provider",
            "rate_limit",
            !hermes_llm::retry::is_usage_limit(text),
        ),
        K::ServerError => ("provider", "server_error", true),
        K::Network => ("endpoint", "timeout", true),
        K::ContextLengthExceeded => ("provider", "context_overflow", false),
        K::CreditsExhausted => ("billing", "billing", false),
        K::Other => ("provider", "provider_error", true),
    };
    ErrorSurface {
        layer,
        code,
        retryable,
    }
}

/// The copy's `(title, hint)` for a failure code, else its layer (Hermes's
/// `_TURN_ERROR_CODE_COPY`, desktop words).
fn turn_error_copy(code: &str) -> Option<(&'static str, &'static str)> {
    match code {
        "auth" => Some((
            "The model provider rejected the API key",
            "Fix the key in Settings ▸ Keys, then retry.",
        )),
        "billing" => Some((
            "The model provider reports no credit left",
            "Top up the account or switch provider, then retry.",
        )),
        "rate_limit" => Some((
            "The model provider is rate-limiting requests",
            "Wait a moment, then retry.",
        )),
        "server_error" => Some((
            "The model provider had an internal error",
            "Wait a moment, then retry.",
        )),
        "timeout" => Some((
            "The model provider did not answer in time",
            "Try again; if it keeps happening, switch provider.",
        )),
        "context_overflow" => Some((
            "The conversation is too long for this model",
            "Run /compress, then retry.",
        )),
        _ => None,
    }
}

/// The account of a turn that produced no reply (Hermes's `turn_error_text`): title, what it
/// means for the user, the raw provider detail on its own line, and the next step.
pub fn turn_error_text(kind: MessageErrorKind, detail: &str, provider: &str) -> String {
    let surface = error_surface(kind, detail);
    let (title, hint) = turn_error_copy(surface.code)
        .or_else(|| turn_error_copy(surface.layer))
        .unwrap_or(("The request failed", "Retry, or switch provider."));
    // A quota limit reproduces on retry: the next step is the reset or another provider.
    let hint = if !surface.retryable && surface.code == "rate_limit" {
        "The limit resets on its own; switch provider to keep going."
    } else {
        hint
    };
    let identity = if provider.is_empty() {
        String::new()
    } else {
        format!(" ({provider})")
    };
    let mut detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    // By characters: a byte cut can land inside a multi-byte one (a provider error in Chinese,
    // say) and `String::truncate` panics there.
    if detail.chars().count() > 400 {
        detail = detail.chars().take(399).collect();
        detail.push('\u{2026}');
    }
    [format!("{title}{identity}. Your message was not answered.")]
        .into_iter()
        .chain((!detail.is_empty()).then(|| format!("Details: {detail}")))
        .chain(std::iter::once(hint.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Who wrote a visible message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
}

/// The provider error a turn ended on, kept on its boundary entry so a reopened session shows the
/// error again.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedTurn {
    pub kind: MessageErrorKind,
    pub error: String,
}

/// One tool call and its result (`None` until the result is stored).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolEntry {
    pub name: String,
    /// The call's arguments; `{"error": ...}` for a call the model sent malformed.
    pub args: Value,
    pub call_id: String,
    pub result: Option<String>,
    /// Seconds since the epoch.
    pub timestamp: f64,
}

/// A visible message: what the user typed (without the context the index attached, a loaded
/// skill shown as its command), or a reply with its reasoning, or a failed turn's boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageEntry {
    pub speaker: Speaker,
    pub text: String,
    pub reasoning: Option<String>,
    pub timestamp: f64,
    /// The stored row ([`row_id`]): what a restore, an edit or a branch names.
    pub row_id: usize,
    pub failed_turn: Option<FailedTurn>,
}

/// One entry of the conversation as the user sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Tool(ToolEntry),
    Message(MessageEntry),
}

impl Entry {
    /// The stored row of a message entry (a tool call has none of its own).
    pub fn row_id(&self) -> Option<usize> {
        match self {
            Entry::Message(message) => Some(message.row_id),
            Entry::Tool(_) => None,
        }
    }
}

/// The failed-turn notice a boundary shows (Hermes's `failed_turn_notice`).
const NOT_COMPLETED: &str = "This turn did not complete. Some actions may have already run; verify their effects before resending.";
const NOT_PROCESSED: &str =
    "Your request was not processed. Send it again if you still want me to carry it out.";

/// The stored messages as the user sees them: user and assistant text, and each tool call with
/// its result filled in from the later message that carries it.
pub fn entries(messages: &[Message]) -> Vec<Entry> {
    let mut out = Vec::new();
    // Tool results arrive in a later message; they're filled into their call's entry.
    let mut calls: HashMap<String, usize> = HashMap::new();
    for (index, message) in messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.is_user_visible())
    {
        let timestamp = message.created as f64;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut failed_turn = None;
        for content in &message.content {
            match content {
                MessageContent::Text(t) => text.push_str(&without_think_tags(&t.text)),
                MessageContent::Thinking(t) => reasoning.push_str(&t.thinking),
                MessageContent::ToolRequest(request) => {
                    let (name, args) = match &request.tool_call {
                        Ok(call) => (
                            call.name.to_string(),
                            call.arguments
                                .clone()
                                .map(Value::Object)
                                .unwrap_or_default(),
                        ),
                        Err(error) => ("invalid tool call".into(), json!({"error": error.message})),
                    };
                    calls.insert(request.id.clone(), out.len());
                    out.push(Entry::Tool(ToolEntry {
                        name,
                        args,
                        call_id: request.id.clone(),
                        result: None,
                        timestamp,
                    }));
                }
                MessageContent::ToolResponse(response) => {
                    let result = tool_result_text(content);
                    if let Some(Entry::Tool(tool)) =
                        calls.get(&response.id).and_then(|&at| out.get_mut(at))
                    {
                        tool.result = Some(result);
                    }
                }
                // A provider error the turn ended on: the boundary entry. It gets Hermes's short
                // failed-turn notice as its text; the account itself lives on the live turn.
                MessageContent::Error(error) if message.role == Role::Assistant => {
                    text.push_str(if turn_called_a_tool(&messages[..=index]) {
                        NOT_COMPLETED
                    } else {
                        NOT_PROCESSED
                    });
                    failed_turn = Some(FailedTurn {
                        kind: error.kind,
                        error: error.message.clone(),
                    });
                }
                _ => {}
            }
        }
        if text.trim().is_empty() && reasoning.is_empty() && failed_turn.is_none() {
            continue;
        }
        let speaker = match message.role {
            Role::User => {
                // The skills and memory the index attached are the model's, not the user's.
                text = hermes_runtime::preinject::strip(message, text);
                // A loaded skill shows as the command that loaded it.
                if let Some(command) = crate::commands::typed_command(&text) {
                    text = command;
                }
                Speaker::User
            }
            Role::Assistant => Speaker::Assistant,
        };
        out.push(Entry::Message(MessageEntry {
            speaker,
            text,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            timestamp,
            row_id: row_id(index),
            failed_turn,
        }));
    }
    out
}

/// Whether the turn that ends `messages` called a tool: a tool may have run before the provider
/// error ended it, which the failed-turn notice says. Only the turn counts: the messages after the
/// last typed one, not the whole conversation.
fn turn_called_a_tool(messages: &[Message]) -> bool {
    messages
        .iter()
        .rev()
        .take_while(|message| !is_typed(message))
        .any(|message| {
            message
                .content
                .iter()
                .any(|c| matches!(c, MessageContent::ToolRequest(_)))
        })
}

/// A tool result as text (its text parts joined by lines; an error's message).
pub fn tool_result_text(content: &MessageContent) -> String {
    match content {
        MessageContent::ToolResponse(response) => match &response.tool_result {
            Ok(result) => result
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.to_string()))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(error) => error.message.to_string(),
        },
        _ => String::new(),
    }
}

/// A stored message's durable row id: its 1-based place in the session's list (the page takes
/// only positive ids).
pub fn row_id(index: usize) -> usize {
    index + 1
}

/// How many stored messages a cut before `row` keeps.
pub fn kept_before_row(row: usize) -> usize {
    row.saturating_sub(1)
}

/// A typed message: the user's text (tool results ride in user messages and don't count).
pub fn is_typed(message: &Message) -> bool {
    message.role == Role::User
        && message.is_user_visible()
        && message
            .content
            .iter()
            .any(|c| matches!(c, MessageContent::Text(t) if !t.text.trim().is_empty()))
}

/// The user's typed prompts, oldest first, as `(row id, text, has an image)`: the turns an editor's
/// `/undo` and `/retry` count (tool results ride in user messages and don't).
pub fn typed_prompts(messages: &[Message]) -> Vec<(usize, String, bool)> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, message)| is_typed(message))
        .map(|(index, message)| {
            let image = message
                .content
                .iter()
                .any(|c| matches!(c, MessageContent::Image(_)));
            (
                row_id(index),
                hermes_runtime::preinject::user_text(message),
                image,
            )
        })
        .collect()
}

/// The rows a finished turn wrote (from index `start` on): all of them, its typed rows (the prompt
/// and steers) and its final reply, so a front end can address them (restore, edit) without
/// reloading the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedTurn {
    pub row_ids: Vec<usize>,
    pub user_row_ids: Vec<usize>,
    pub final_assistant_row_id: Option<usize>,
}

/// The [`PersistedTurn`] of the turn whose rows start at `start`.
pub fn persisted_turn(messages: &[Message], start: usize) -> PersistedTurn {
    let rows: Vec<(usize, &Message)> = messages.iter().enumerate().skip(start).collect();
    PersistedTurn {
        row_ids: rows.iter().map(|(i, _)| row_id(*i)).collect(),
        user_row_ids: rows
            .iter()
            .filter(|(_, m)| is_typed(m))
            .map(|(i, _)| row_id(*i))
            .collect(),
        final_assistant_row_id: rows
            .iter()
            .rev()
            .find(|(_, m)| m.role == Role::Assistant && !m.as_concat_text().trim().is_empty())
            .map(|(i, _)| row_id(*i)),
    }
}

/// Where the `ordinal`-th message the user typed (0-based) sits among the stored messages; `None`
/// past the end. Tool results ride in user messages and don't count.
pub fn user_turn_index(messages: &[Message], ordinal: usize) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| is_typed(m))
        .nth(ordinal)
        .map(|(index, _)| index)
}

/// A reply's text without the bare `<think>` / `</think>` tags a reasoning model sometimes leaves
/// in it (its reasoning arrives separately). Hermes strips them before showing the reply too.
pub fn without_think_tags(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("think>") {
        std::borrow::Cow::Owned(text.replace("<think>", "").replace("</think>", ""))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::errors::ProviderError;
    use std::time::Duration;

    fn texts(entries: &[Entry]) -> Vec<(Speaker, String)> {
        entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message(m) => Some((m.speaker, m.text.clone())),
                Entry::Tool(_) => None,
            })
            .collect()
    }

    /// The skills and memory the index attached to a user message are the model's: the
    /// conversation, the editor's typed prompts and the rewind box all show only what was typed.
    #[test]
    fn the_block_the_index_attached_is_not_shown() {
        let block = hermes_runtime::preinject::Block {
            text: "<context>\n<relevant-skills>\nsecret steps\n</relevant-skills>\n</context>"
                .into(),
            skills: vec!["deploy".into()],
            memory: vec![],
            project_skills: vec![],
        };
        let mut user = Message::user().with_text(format!("{}\n\nship it", block.text));
        block.note(&mut user);
        let stored = vec![user.clone(), Message::assistant().with_text("done")];
        assert_eq!(
            texts(&entries(&stored)),
            [
                (Speaker::User, "ship it".to_string()),
                (Speaker::Assistant, "done".to_string())
            ]
        );
        assert_eq!(typed_prompts(&stored)[0].1, "ship it");
    }

    #[test]
    fn a_tool_call_gets_its_result_and_a_bad_call_its_error() {
        let call = Message::assistant().with_tool_request(
            "c1",
            Ok(rmcp::model::CallToolRequestParams::new("read_file")),
        );
        let result = Message::user().with_tool_response(
            "c1",
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::ContentBlock::text("the file"),
            ])),
        );
        let stored = vec![Message::user().with_text("read it"), call, result];
        let all = entries(&stored);
        let Entry::Tool(tool) = &all[1] else {
            panic!("a tool entry: {all:?}")
        };
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.result.as_deref(), Some("the file"));
        assert_eq!(all[1].row_id(), None);
        assert_eq!(all[0].row_id(), Some(1));
    }

    #[test]
    fn the_error_a_turn_ended_on_is_reported_with_its_surface() {
        let stored = vec![
            Message::user().with_text("hello"),
            Message::assistant().with_text("partial answer"),
            Message::from_provider_error(&ProviderError::RateLimitExceeded {
                details:
                    "user daily request limit reached (800 of 800). Retry after 19919 seconds."
                        .into(),
                retry_delay: Some(Duration::from_secs(19919)),
            }),
        ];
        let (kind, text) = message_error(&stored).expect("the turn ended on an error");
        assert_eq!(kind, MessageErrorKind::RateLimit);
        assert!(text.contains("daily request limit reached"));
        let surface = error_surface(kind, &text);
        assert_eq!((surface.layer, surface.code), ("provider", "rate_limit"));
        // A quota limit reproduces on retry.
        assert!(!surface.retryable);
        // The boundary entry keeps the error for a reopened session.
        let Some(Entry::Message(last)) = entries(&stored).pop() else {
            panic!("a boundary")
        };
        assert_eq!(last.failed_turn.unwrap().kind, MessageErrorKind::RateLimit);
        assert!(last.text.starts_with("Your request was not processed"));
    }

    #[test]
    fn every_kind_names_its_layer_and_code() {
        assert!(error_surface(MessageErrorKind::RateLimit, "please try again in 1s").retryable);
        for (kind, layer, code, retryable) in [
            (MessageErrorKind::Authentication, "auth", "auth", false),
            (
                MessageErrorKind::ContextLengthExceeded,
                "provider",
                "context_overflow",
                false,
            ),
            (
                MessageErrorKind::CreditsExhausted,
                "billing",
                "billing",
                false,
            ),
            (
                MessageErrorKind::ServerError,
                "provider",
                "server_error",
                true,
            ),
            (MessageErrorKind::Network, "endpoint", "timeout", true),
            (MessageErrorKind::Other, "provider", "provider_error", true),
        ] {
            assert_eq!(
                error_surface(kind, "x"),
                ErrorSurface {
                    layer,
                    code,
                    retryable
                }
            );
        }
    }

    #[test]
    fn a_turn_that_completed_normally_has_no_error_to_report() {
        let stored = vec![
            Message::user().with_text("hello"),
            Message::assistant().with_text("the reply"),
        ];
        assert_eq!(message_error(&stored), None);
    }

    #[test]
    fn the_account_names_the_fix_and_carries_the_detail() {
        let account = turn_error_text(
            MessageErrorKind::RateLimit,
            "user daily request limit reached (800 of 800).",
            "error-mock",
        );
        assert!(account.starts_with("The model provider is rate-limiting requests (error-mock). Your message was not answered."), "{account}");
        assert!(account.contains("Details: user daily request limit reached"));
        // A quota limit reproduces on retry: the next step says so.
        assert!(account.contains("The limit resets on its own"), "{account}");

        // A transient rate limit's next step is the retry.
        let account = turn_error_text(MessageErrorKind::RateLimit, "please try again in 1s", "");
        assert!(account.contains("Wait a moment, then retry."), "{account}");
        assert!(
            !account.contains('('),
            "no identity without a provider: {account}"
        );
    }

    #[test]
    fn a_long_non_ascii_detail_is_cut_on_a_character_boundary() {
        // 450 three-byte characters: a byte cut at 399 would split one and panic.
        let detail = "\u{9519}".repeat(450);
        let account = turn_error_text(MessageErrorKind::Other, &detail, "p");
        let shown = account
            .lines()
            .find_map(|line| line.strip_prefix("Details: "))
            .expect("the detail is shown");
        assert_eq!(shown.chars().count(), 400);
        assert!(shown.ends_with('\u{2026}'));
    }

    #[test]
    fn row_ids_are_stored_positions_and_cut_where_restore_asks() {
        let stored = vec![
            Message::user().with_text("first"),
            Message::assistant().with_text("one"),
            Message::user().with_text("second"),
            Message::assistant().with_text("two"),
        ];
        // The page's ordinal for the second typed message lands on its row.
        assert_eq!(user_turn_index(&stored, 1).map(row_id), Some(3));
        assert_eq!(kept_before_row(3), 2);
        assert_eq!(user_turn_index(&stored, 2), None);
        // A turn's receipt names its rows: the prompt and the final reply.
        assert_eq!(
            persisted_turn(&stored, 2),
            PersistedTurn {
                row_ids: vec![3, 4],
                user_row_ids: vec![3],
                final_assistant_row_id: Some(4),
            }
        );
    }

    #[test]
    fn think_tags_are_stripped() {
        assert_eq!(without_think_tags("<think>a</think>b"), "ab");
        assert_eq!(without_think_tags("plain"), "plain");
    }
}
