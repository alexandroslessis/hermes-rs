//! Our conversation in Hermes's terms: the stored conversation as `TranscriptMessage`s (what
//! `session.resume` returns), a failed turn's error card (`error_surface`), and a finished turn's
//! `persisted_turn` receipt. The reading itself (which entries, which rows, which error) is
//! `hermes_core::transcript`; this is the page's JSON for it. A running turn's events are
//! `hermes_core::stream`, as frames in `hub.rs`.
//!
//! Hermes: `tui_gateway/methods_session.py` (`session.resume`), `error_surface.py`.

use hermes_core::transcript::{Entry, MessageEntry, Speaker, ToolEntry};
use hermes_llm::conversation::message::{Message, MessageErrorKind};
use serde_json::{Value, json};

pub use hermes_core::transcript::{
    kept_before_row, message_error, row_id, turn_error_text, typed_prompts, user_turn_index,
};

/// The error card's structured descriptor (Hermes's `error_surface.py`). Advisory — the page
/// falls back to a generic headline when it is absent.
pub fn error_surface(kind: MessageErrorKind, text: &str, provider: &str, model: &str) -> Value {
    let surface = hermes_core::transcript::error_surface(kind, text);
    let mut surface = json!({
        "layer": surface.layer,
        "code": surface.code,
        "retryable": surface.retryable,
    });
    if !provider.is_empty() {
        surface["provider"] = json!(provider);
    }
    if !model.is_empty() {
        surface["model"] = json!(model);
    }
    surface
}

/// Stored messages as the transcript Hermes's frontend renders: user and assistant text, and
/// each tool call with its result. `provider`/`model` name the failed turn's boundary card
/// (Hermes's failed-turn fingerprint); pass empty when unknown.
pub fn messages(messages: &[Message], provider: &str, model: &str) -> Vec<Value> {
    hermes_core::transcript::entries(messages)
        .into_iter()
        .map(|entry| match entry {
            Entry::Tool(tool) => tool_row(tool),
            Entry::Message(message) => message_row(message, provider, model),
        })
        .collect()
}

fn tool_row(tool: ToolEntry) -> Value {
    json!({
        "role": "tool",
        "name": tool.name,
        "args": tool.args,
        "tool_call_id": tool.call_id,
        "text": tool.result,
        "timestamp": tool.timestamp,
    })
}

fn message_row(message: MessageEntry, provider: &str, model: &str) -> Value {
    let role = match message.speaker {
        Speaker::User => "user",
        Speaker::Assistant => "assistant",
    };
    let mut row = json!({
        "role": role,
        "text": message.text,
        "reasoning": message.reasoning,
        "timestamp": message.timestamp,
        "row_id": message.row_id,
    });
    // A failed turn is a Hermes boundary, not the model's reply: the page paints the styled card
    // it already knows from the error and the descriptor, not a text row (the row keeps Hermes's
    // short failed-turn notice as its text: the page won't drop a content-less failed_turn row
    // before it paints the card from `display_metadata`).
    if let Some(failed) = message.failed_turn {
        row["display_kind"] = json!("failed_turn");
        row["display_metadata"] = json!({
            "error": failed.error,
            "error_surface": error_surface(failed.kind, &failed.error, provider, model),
        });
    }
    row
}

/// Hermes's `persisted_turn` receipt for a finished turn: the stored rows it wrote (from index
/// `start` on), its typed rows (the prompt and steers) and its final reply, so the page can
/// address them (restore, edit) without reloading the transcript.
pub fn persisted_turn(messages: &[Message], start: usize, submission_id: &str) -> Value {
    persisted_turn_json(
        &hermes_core::transcript::persisted_turn(messages, start),
        submission_id,
    )
}

/// [`persisted_turn`] from the core's reading of it (a finished turn's `TurnEnd`).
pub fn persisted_turn_json(
    turn: &hermes_core::transcript::PersistedTurn,
    submission_id: &str,
) -> Value {
    json!({
        "row_ids": turn.row_ids,
        "complete": true,
        "user_row_id": turn.user_row_ids.first(),
        "user_row_ids": turn.user_row_ids,
        "final_assistant_row_id": turn.final_assistant_row_id,
        "submission_id": submission_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::errors::ProviderError;
    use std::time::Duration;

    /// The skills and memory the index attached to a user message are the model's: the page's
    /// transcript never shows them.
    #[test]
    fn the_block_the_index_attached_never_reaches_the_page() {
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
        let rows = messages(&stored, "", "");
        assert_eq!(rows[0]["role"], "user");
        assert_eq!(rows[0]["text"], "ship it");
        assert_eq!(rows[1]["text"], "done");
        // A message without the note is shown as it is, even if it looks like a block.
        let plain = vec![Message::user().with_text("<context>x</context> hi")];
        assert_eq!(
            messages(&plain, "", "")[0]["text"],
            "<context>x</context> hi"
        );
    }

    #[test]
    fn the_surface_names_the_failed_provider_only_when_known() {
        let surface = error_surface(
            MessageErrorKind::RateLimit,
            "user daily request limit reached (800 of 800).",
            "openrouter",
            "mock",
        );
        assert_eq!(surface["layer"], "provider");
        assert_eq!(surface["code"], "rate_limit");
        assert_eq!(surface["retryable"], false);
        assert_eq!(surface["provider"], "openrouter");
        assert_eq!(surface["model"], "mock");
        let anonymous = error_surface(MessageErrorKind::Other, "x", "", "");
        assert!(anonymous.get("provider").is_none() && anonymous.get("model").is_none());
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
        // The tool row: no result yet, no row id of its own.
        assert_eq!(rows[1]["role"], "tool");
        assert_eq!(rows[1]["text"], Value::Null);
        assert!(rows[1].get("row_id").is_none());
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
    fn row_ids_are_stored_positions_and_the_receipt_names_them() {
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
        let turn = persisted_turn(&stored, 2, "s1");
        assert_eq!(turn["row_ids"], json!([3, 4]));
        assert_eq!(turn["user_row_id"], 3);
        assert_eq!(turn["user_row_ids"], json!([3]));
        assert_eq!(turn["final_assistant_row_id"], 4);
        assert_eq!(turn["submission_id"], "s1");
    }
}
