//! `/save` (`session.export`): a conversation as a file. JSON is Hermes's own snapshot (the
//! session's fields and its `TranscriptMessage` rows, built here); Markdown and HTML are
//! `hermes_core::export`'s.
//!
//! Hermes: `session.export` (`tui_gateway/methods_session.py`).

use hermes_core::export::{Format, clean, document, file_name, title};
use hermes_llm::conversation::message::Message;
use hermes_store::SessionRecord;
use serde_json::{Value, json};

/// `(content, suggested file name)` for a session's stored messages.
pub fn export(
    record: &SessionRecord,
    messages: &[Message],
    format: Format,
    masked: bool,
) -> (String, String) {
    let content = document(
        record,
        &hermes_core::transcript::entries(messages),
        format,
        masked,
    )
    .unwrap_or_else(|| {
        let rows = crate::transcript::messages(messages, &record.provider, &record.model);
        snapshot(record, &rows, masked)
    });
    (content, file_name(record, format))
}

/// The JSON snapshot: the session's fields and its transcript rows, masked when asked.
fn snapshot(record: &SessionRecord, rows: &[Value], masked: bool) -> String {
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            for key in ["text", "reasoning"] {
                if let Some(text) = row[key].as_str() {
                    row[key] = json!(clean(text, masked));
                }
            }
            if masked && !row["args"].is_null() {
                row["args"] = json!(clean(&row["args"].to_string(), masked));
            }
            row
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "session_id": record.id,
        "title": title(record),
        "model": record.model,
        "provider": record.provider,
        "cwd": record.cwd,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "messages": rows,
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SessionRecord {
        SessionRecord {
            id: "s1".into(),
            title: Some("Fix the <build>".into()),
            cwd: "C:/work".into(),
            provider: "mock".into(),
            model: "m".into(),
            system_prompt: String::new(),
            created_at: 1,
            updated_at: 2,
            pinned: false,
        }
    }

    fn messages() -> Vec<Message> {
        vec![
            Message::user().with_text("use key sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
            Message::assistant().with_text("Done <ok>."),
        ]
    }

    #[test]
    fn the_snapshot_is_hermes_json_and_masks_secrets() {
        let (json, name) = export(&record(), &messages(), Format::Json, false);
        assert_eq!(name, "hermes-fix-the-build.json");
        let snapshot: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot["session_id"], "s1");
        assert_eq!(snapshot["messages"].as_array().unwrap().len(), 2);
        assert_eq!(snapshot["messages"][1]["role"], "assistant");
        assert!(json.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"));
        let (masked, _) = export(&record(), &messages(), Format::Json, true);
        assert!(!masked.contains("abcdefghijklmnopqrstuvwxyz123456"));
        // The documents come from the core.
        let (md, name) = export(&record(), &messages(), Format::Markdown, false);
        assert_eq!(name, "hermes-fix-the-build.md");
        assert!(md.contains("## Hermes\n\nDone <ok>."));
    }
}
