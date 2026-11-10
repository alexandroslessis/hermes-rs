//! Our conversation in Hermes's terms: stored messages as `TranscriptMessage`s (what
//! `session.resume` returns) and a running turn's agent events as Hermes's stream events
//! (`message.delta`, `reasoning.delta`, `tool.start`, `tool.complete`).
//!
//! Hermes: `tui_gateway/methods_session.py` (`session.resume`), `tui_gateway/prompt_turn.py` (stream
//! events), and the failed-turn boundary a provider-error turn leaves (`agent/conversation_loop.py`
//! `_close_durable_failed_turn` + `agent/turn_failure_copy.py` `failed_turn_notice`).

use std::collections::HashMap;

use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::message::{Message, MessageContent, MessageErrorKind};
use rmcp::model::Role;
use serde_json::{Value, json};

/// The error a turn ended on, as `(kind, text)` from its last message (`ops::ExitOnError`
/// yields a provider error as one): the page's error card keys off the failure status, so the
/// turn is reported like Hermes's failed turn result (`failed=True`, `error=<the text>`).
pub fn message_error(messages: &[Message]) -> Option<(MessageErrorKind, String)> {
    let error = messages.last()?.content.iter().find_map(|c| c.as_error())?;
    Some((error.kind, error.message.clone()))
}

/// The error card's descriptor parts `(layer, code, retryable)` (Hermes's `error_surface.py`):
/// which layer of the stack failed and the classifier's code, so the card says what happened in
/// plain words and offers code-appropriate actions instead of toasting an opaque string.
fn error_surface_parts(kind: MessageErrorKind, text: &str) -> (&'static str, &'static str, bool) {
    use MessageErrorKind as K;
    match kind {
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
    }
}

/// The error card's structured descriptor (Hermes's `error_surface.py`). Advisory — the page
/// falls back to a generic headline when it is absent.
pub fn error_surface(kind: MessageErrorKind, text: &str, provider: &str, model: &str) -> Value {
    let (layer, code, retryable) = error_surface_parts(kind, text);
    let mut surface = json!({"layer": layer, "code": code, "retryable": retryable});
    if !provider.is_empty() {
        surface["provider"] = json!(provider);
    }
    if !model.is_empty() {
        surface["model"] = json!(model);
    }
    surface
}

/// The card copy's `(title, hint)` for a failure code, else its layer (Hermes's
/// `tui_gateway/user_messages.py` `_TURN_ERROR_CODE_COPY`, desktop words: the page strips the
/// bubble's text and renders the card, so this is what the transcript keeps for a reopened
/// session and what `message.complete`'s text carries).
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

/// The assistant-slot text for a turn that produced no reply (Hermes's `turn_error_text`):
/// title, what it means for the user, the raw provider detail on its own line, and the next
/// step. The page's completion path appends the turn's message only when the frame's text is
/// non-empty, so a provider error with nothing streamed carries this account.
pub fn turn_error_text(kind: MessageErrorKind, detail: &str, provider: &str) -> String {
    let (layer, code, retryable) = error_surface_parts(kind, detail);
    let (title, hint) = turn_error_copy(code)
        .or_else(|| turn_error_copy(layer))
        .unwrap_or(("The request failed", "Retry, or switch provider."));
    // A quota limit reproduces on retry: the next step is the reset or another provider.
    let hint = if !retryable && code == "rate_limit" {
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

/// Stored messages as the transcript Hermes's frontend renders: user and assistant text, and
/// each tool call with its result. `provider`/`model` name the failed turn's boundary card
/// (Hermes's failed-turn fingerprint); pass empty when unknown.
pub fn messages(messages: &[Message], provider: &str, model: &str) -> Vec<Value> {
    let mut out = Vec::new();
    // Tool results arrive in a later message; they're filled into their call's entry.
    let mut calls: HashMap<String, usize> = HashMap::new();
    // A message's durable row id is its 1-based place in the stored list (the page takes only
    // positive ids; `row_id(index)`).
    for (index, message) in messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.is_user_visible())
    {
        let row_id = row_id(index);
        let timestamp = message.created as f64;
        let mut text = String::new();
        let mut reasoning = String::new();
        // A turn that ended on a provider error: the boundary row's display metadata so the page
        // redraws the styled error card on reopen (Hermes's failed-turn boundary). Advisory — the
        // account lives on the live card; a reopened session shows the card, not a plain block.
        let mut failed_turn: Option<Value> = None;
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
                    out.push(json!({
                        "role": "tool",
                        "name": name,
                        "args": args,
                        "tool_call_id": request.id,
                        "text": null,
                        "timestamp": timestamp,
                    }));
                }
                MessageContent::ToolResponse(response) => {
                    let result = tool_result_text(content);
                    if let Some(&index) = calls.get(&response.id) {
                        out[index]["text"] = Value::String(result);
                    }
                }
                // A provider error the turn ended on: the boundary row carries the display
                // metadata (Hermes's failed-turn boundary), so the page redraws the styled error
                // card on reopen. The row gets Hermes's short failed-turn notice as its own text
                // (the page won't drop a content-less failed_turn row before it paints the card
                // from `display_metadata`); the account itself lives on the live frame.
                MessageContent::Error(error) if message.role == Role::Assistant => {
                    let notice = if turn_called_a_tool(&messages[..=index]) {
                        "This turn did not complete. Some actions may have already run; verify their effects before resending."
                    } else {
                        "Your request was not processed. Send it again if you still want me to carry it out."
                    };
                    text.push_str(notice);
                    failed_turn = Some(json!({
                        "error": error.message,
                        "error_surface": error_surface(error.kind, &error.message, provider, model),
                    }));
                }
                _ => {}
            }
        }
        if text.trim().is_empty() && reasoning.is_empty() && failed_turn.is_none() {
            continue;
        }
        let role = match message.role {
            Role::User => {
                // A loaded skill shows as the command that loaded it.
                if let Some(command) = crate::commands::typed_command(&text) {
                    text = command;
                }
                "user"
            }
            Role::Assistant => "assistant",
        };
        let mut row = json!({
            "role": role,
            "text": text,
            "reasoning": (!reasoning.is_empty()).then_some(reasoning),
            "timestamp": timestamp,
            "row_id": row_id,
        });
        // A failed turn is a Hermes boundary, not the model's reply: the page paints the
        // styled card it already knows from the error and the descriptor, not a text row.
        if let Some(metadata) = failed_turn {
            row["display_kind"] = json!("failed_turn");
            row["display_metadata"] = metadata;
        }
        out.push(row);
    }
    out
}

/// Whether the turn that ends `messages` called a tool: a tool may have run before the provider
/// error ended it, which the failed-turn notice says (Hermes's `failed_turn_notice`). Only the
/// turn counts: the messages after the last typed one, not the whole conversation.
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

fn tool_result_text(content: &MessageContent) -> String {
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

/// A running turn's progress as Hermes's events.
#[derive(Default)]
pub struct TurnStream {
    text: String,
    /// Tool call id → tool name, for the completion events.
    tools: HashMap<String, String>,
}

impl TurnStream {
    /// The reply's text so far (`message.complete` carries all of it).
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn agent_event(&mut self, event: AgentEvent) -> Vec<(&'static str, Value)> {
        let AgentEvent::Message(message) = event else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for content in &message.content {
            match (&message.role, content) {
                (Role::Assistant, MessageContent::Text(t)) if !t.text.is_empty() => {
                    let text = without_think_tags(&t.text);
                    if !text.is_empty() {
                        self.text.push_str(&text);
                        events.push(("message.delta", json!({"text": text})));
                    }
                }
                (Role::Assistant, MessageContent::Thinking(t)) if !t.thinking.is_empty() => {
                    events.push(("reasoning.delta", json!({"text": t.thinking})));
                }
                (Role::Assistant, MessageContent::ToolRequest(request)) => {
                    let (name, args) = match &request.tool_call {
                        Ok(call) => (
                            call.name.to_string(),
                            call.arguments
                                .clone()
                                .map(Value::Object)
                                .unwrap_or_default(),
                        ),
                        Err(_) => ("invalid tool call".to_string(), Value::Null),
                    };
                    self.tools.insert(request.id.clone(), name.clone());
                    events.push((
                        "tool.start",
                        json!({"tool_id": request.id, "name": name, "args": args}),
                    ));
                }
                (_, MessageContent::ToolResponse(response)) => {
                    let name = self.tools.get(&response.id).cloned().unwrap_or_default();
                    let text = tool_result_text(content);
                    // Hermes's tools return JSON; the card reads `result` as the value.
                    let result = serde_json::from_str::<Value>(&text)
                        .unwrap_or_else(|_| Value::String(text.clone()));
                    events.push((
                        "tool.complete",
                        json!({
                            "tool_id": response.id,
                            "name": name,
                            "result": result,
                            "result_text": text,
                        }),
                    ));
                }
                _ => {}
            }
        }
        events
    }
}

/// A stored message's durable row id: its 1-based place in the session's list.
pub fn row_id(index: usize) -> usize {
    index + 1
}

/// How many stored messages a cut before `row` keeps.
pub fn kept_before_row(row: usize) -> usize {
    row.saturating_sub(1)
}

/// A typed message: the user's text (tool results ride in user messages and don't count).
fn is_typed(message: &Message) -> bool {
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
            (row_id(index), message.as_concat_text(), image)
        })
        .collect()
}

/// Hermes's `persisted_turn` receipt for a finished turn: the stored rows it wrote (from index
/// `start` on), its typed rows (the prompt and steers) and its final reply, so the page can
/// address them (restore, edit) without reloading the transcript.
pub fn persisted_turn(messages: &[Message], start: usize, submission_id: &str) -> Value {
    let rows: Vec<(usize, &Message)> = messages.iter().enumerate().skip(start).collect();
    let user_rows: Vec<usize> = rows
        .iter()
        .filter(|(_, m)| is_typed(m))
        .map(|(i, _)| row_id(*i))
        .collect();
    let final_reply = rows
        .iter()
        .rev()
        .find(|(_, m)| m.role == Role::Assistant && !m.as_concat_text().trim().is_empty())
        .map(|(i, _)| row_id(*i));
    json!({
        "row_ids": rows.iter().map(|(i, _)| row_id(*i)).collect::<Vec<_>>(),
        "complete": true,
        "user_row_id": user_rows.first(),
        "user_row_ids": user_rows,
        "final_assistant_row_id": final_reply,
        "submission_id": submission_id,
    })
}

/// Where the `ordinal`-th message the user typed (0-based, as the page counts them) sits among
/// the stored messages; `None` past the end. Tool results ride in user messages and don't count.
pub fn user_turn_index(messages: &[Message], ordinal: usize) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| is_typed(m))
        .nth(ordinal)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::errors::ProviderError;
    use std::time::Duration;

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

        let surface = error_surface(kind, &text, "openrouter", "mock");
        assert_eq!(surface["layer"], "provider");
        assert_eq!(surface["code"], "rate_limit");
        // A quota limit reproduces on retry: the card's Retry action hides.
        assert_eq!(surface["retryable"], false);
        assert_eq!(surface["provider"], "openrouter");
        assert_eq!(surface["model"], "mock");
    }

    #[test]
    fn a_transient_rate_limit_and_other_kinds_surface_their_fix() {
        let transient = error_surface(
            MessageErrorKind::RateLimit,
            "please try again in 1s",
            "openrouter",
            "mock",
        );
        assert_eq!(transient["code"], "rate_limit");
        assert_eq!(transient["retryable"], true);

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
            let surface = error_surface(kind, "x", "", "");
            assert_eq!(surface["layer"], layer, "{code}");
            assert_eq!(surface["code"], code, "{code}");
            assert_eq!(surface["retryable"], retryable, "{code}");
            // Identity is only stamped for the session that actually failed.
            assert!(surface.get("provider").is_none());
            assert!(surface.get("model").is_none());
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

    fn tool_call_message() -> Message {
        Message::assistant().with_tool_request(
            "call_1",
            Ok(rmcp::model::CallToolRequestParams::new("terminal")),
        )
    }

    #[test]
    fn the_failed_turn_notice_counts_only_this_turns_tool_calls() {
        let failed = Message::from_provider_error(&ProviderError::ServerError("boom".into()));
        // An earlier turn ran a tool; the failed turn did not.
        let stored = vec![
            Message::user().with_text("list files"),
            tool_call_message(),
            Message::user().with_text("now say hi"),
            failed,
        ];
        let rows = messages(&stored, "", "");
        let notice = rows.last().unwrap()["text"].as_str().unwrap();
        assert!(
            notice.starts_with("Your request was not processed"),
            "{notice}"
        );
    }

    #[test]
    fn the_failed_turn_notice_warns_when_this_turn_ran_a_tool() {
        let failed = Message::from_provider_error(&ProviderError::ServerError("boom".into()));
        let stored = vec![
            Message::user().with_text("list files"),
            tool_call_message(),
            failed,
        ];
        let rows = messages(&stored, "", "");
        let notice = rows.last().unwrap()["text"].as_str().unwrap();
        assert!(notice.starts_with("This turn did not complete"), "{notice}");
    }

    #[test]
    fn the_transcript_redraws_the_error_card_for_a_reopened_session() {
        let stored = vec![
            Message::user().with_text("hello"),
            Message::assistant().with_text("partial answer"),
            Message::from_provider_error(&ProviderError::RateLimitExceeded {
                details: "user daily request limit reached (800 of 800).".into(),
                retry_delay: Some(Duration::from_secs(19919)),
            }),
        ];
        let rows = messages(&stored, "error-mock", "mock-error");
        let last = rows.last().expect("the error row is kept");
        assert_eq!(last["role"], "assistant");
        // Hermes's failed-turn boundary: the page redraws the styled card from the descriptor,
        // so the row is a boundary (display_kind/display_metadata), not a plain-text account.
        assert_eq!(last["display_kind"], "failed_turn");
        let metadata = &last["display_metadata"];
        assert_eq!(metadata["error_surface"]["layer"], "provider");
        assert_eq!(metadata["error_surface"]["code"], "rate_limit");
        assert_eq!(metadata["error_surface"]["retryable"], false);
        assert_eq!(metadata["error_surface"]["provider"], "error-mock");
        assert_eq!(metadata["error_surface"]["model"], "mock-error");
        assert!(
            metadata["error"]
                .as_str()
                .unwrap()
                .contains("daily request limit reached"),
            "{metadata}"
        );
    }

    #[test]
    fn row_ids_are_stored_positions_and_cut_where_restore_asks() {
        let stored = vec![
            Message::user().with_text("first"),
            Message::assistant().with_text("one"),
            Message::user().with_text("second"),
            Message::assistant().with_text("two"),
        ];
        let rows = messages(&stored, "", "");
        let users: Vec<_> = rows.iter().filter(|r| r["role"] == "user").collect();
        assert_eq!(users[1]["text"], "second");
        assert_eq!(users[1]["row_id"], 3);
        // The page's ordinal for the same message lands on the same row.
        assert_eq!(user_turn_index(&stored, 1).map(row_id), Some(3));
        // A turn's receipt names its rows: the prompt and the final reply.
        let turn = persisted_turn(&stored, 2, "s1");
        assert_eq!(turn["row_ids"], json!([3, 4]));
        assert_eq!(turn["user_row_ids"], json!([3]));
        assert_eq!(turn["final_assistant_row_id"], 4);
    }
}

/// A reply's text without the bare `<think>` / `</think>` tags a reasoning model sometimes leaves
/// in it (its reasoning arrives separately). Hermes strips them before showing the reply too.
fn without_think_tags(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("think>") {
        std::borrow::Cow::Owned(text.replace("<think>", "").replace("</think>", ""))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}
