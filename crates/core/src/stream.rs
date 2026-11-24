//! A running turn's progress as [`CoreEvent`]s: the reply and reasoning as they stream, each tool
//! call's start, progress and result, and the status lines (compression, the quota notice, the
//! runtime's own notices such as a fallback or a taint warning).
//!
//! Hermes: `tui_gateway/prompt_turn.py` (the stream events), `tui_gateway/server.py`
//! (`_status_update`), `agent/conversation_compression.py` (`COMPACTION_STATUS`,
//! `COMPACTION_DONE_STATUS`).

use std::collections::HashMap;

use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::message::MessageContent;
use rmcp::model::Role;
use serde_json::Value;

use crate::events::CoreEvent;
use crate::transcript::{tool_result_is_error, tool_result_text, without_think_tags};

/// Hermes's compaction status lines (`COMPACTION_STATUS`, `COMPACTION_DONE_STATUS`).
pub const COMPACTING_TEXT: &str =
    "🗜️ Compacting context — summarizing earlier conversation so I can continue...";
pub const COMPACTED_TEXT: &str = "✓ Context compaction complete — continuing turn...";

/// One turn's stream: what it has said so far and which tool each call id is.
pub struct TurnStream {
    session_id: String,
    text: String,
    /// Tool call id → tool name, for the completion events.
    tools: HashMap<String, String>,
}

impl TurnStream {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            text: String::new(),
            tools: HashMap::new(),
        }
    }

    /// The reply's text so far (the end of the turn carries all of it).
    pub fn text(&self) -> &str {
        &self.text
    }

    fn status(&self, kind: &str, text: String) -> CoreEvent {
        CoreEvent::Status {
            session_id: self.session_id.clone(),
            kind: kind.to_string(),
            text,
        }
    }

    /// The runtime's notice that the provider's daily request quota is nearly used up.
    pub fn notice(&self, text: String) -> CoreEvent {
        self.status("quota", text)
    }

    /// An agent event as the stream's events. Usage, subagent activity and the history being
    /// replaced are not stream events: empty for those.
    pub fn agent_event(&mut self, event: AgentEvent) -> Vec<CoreEvent> {
        match event {
            AgentEvent::Message(message) => self.message(&message),
            // A status line for the conversation (a fallback, a taint warning).
            AgentEvent::Status { kind, text } => vec![self.status(kind, text)],
            AgentEvent::ToolProgress {
                tool_call_id,
                name,
                progress,
                message,
            } => vec![CoreEvent::ToolProgress {
                session_id: self.session_id.clone(),
                tool_id: tool_call_id,
                name,
                progress,
                message,
            }],
            // Compression before a model call can take minutes on a local model: without a line
            // the turn looks hung.
            AgentEvent::Compacting => vec![self.status("compacting", COMPACTING_TEXT.into())],
            AgentEvent::Compacted { .. } => vec![self.status("compacted", COMPACTED_TEXT.into())],
            AgentEvent::CompactionFailed(error) => {
                let text = match error {
                    Some(error) => format!(
                        "Context compression failed ({error}); continuing with the full conversation."
                    ),
                    None => "Context compression couldn't shorten the conversation; continuing."
                        .to_string(),
                };
                vec![self.status("compacted", text)]
            }
            _ => Vec::new(),
        }
    }

    fn message(&mut self, message: &hermes_llm::conversation::message::Message) -> Vec<CoreEvent> {
        let id = self.session_id.clone();
        let session_id = || id.clone();
        let mut events = Vec::new();
        for content in &message.content {
            match (&message.role, content) {
                (Role::Assistant, MessageContent::Text(t)) if !t.text.is_empty() => {
                    let text = without_think_tags(&t.text);
                    if !text.is_empty() {
                        self.text.push_str(&text);
                        events.push(CoreEvent::TextDelta {
                            session_id: session_id(),
                            text: text.into_owned(),
                        });
                    }
                }
                (Role::Assistant, MessageContent::Thinking(t)) if !t.thinking.is_empty() => {
                    events.push(CoreEvent::ReasoningDelta {
                        session_id: session_id(),
                        text: t.thinking.clone(),
                    });
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
                    events.push(CoreEvent::ToolStarted {
                        session_id: session_id(),
                        tool_id: request.id.clone(),
                        name,
                        args,
                    });
                }
                (_, MessageContent::ToolResponse(response)) => {
                    events.push(CoreEvent::ToolCompleted {
                        session_id: session_id(),
                        tool_id: response.id.clone(),
                        name: self.tools.get(&response.id).cloned().unwrap_or_default(),
                        text: tool_result_text(content),
                        is_error: tool_result_is_error(content),
                    });
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::conversation::message::Message;

    #[test]
    fn a_reply_streams_its_text_and_tool_calls_name_their_results() {
        let mut stream = TurnStream::new("s1");
        let events = stream.agent_event(AgentEvent::Message(
            Message::assistant()
                .with_text("<think>")
                .with_text("Hello")
                .with_tool_request(
                    "c1",
                    Ok(rmcp::model::CallToolRequestParams::new("terminal")),
                ),
        ));
        assert!(matches!(
            events.as_slice(),
            [
                CoreEvent::TextDelta { text, .. },
                CoreEvent::ToolStarted { name, .. }
            ] if text == "Hello" && name == "terminal"
        ));
        let done = stream.agent_event(AgentEvent::Message(Message::user().with_tool_response(
            "c1",
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::ContentBlock::text("ok"),
            ])),
        )));
        assert!(matches!(
            done.as_slice(),
            [CoreEvent::ToolCompleted { name, text, is_error: false, .. }]
                if name == "terminal" && text == "ok"
        ));
        assert_eq!(stream.text(), "Hello");
        // A result the tool flagged, and a call that failed in the harness, are errors.
        stream.agent_event(AgentEvent::Message(
            Message::assistant()
                .with_tool_request("c2", Ok(rmcp::model::CallToolRequestParams::new("read_file")))
                .with_tool_request("c3", Ok(rmcp::model::CallToolRequestParams::new("nope"))),
        ));
        let failed = stream.agent_event(AgentEvent::Message(
            Message::user()
                .with_tool_response(
                    "c2",
                    Ok(rmcp::model::CallToolResult::error(vec![
                        rmcp::model::ContentBlock::text(r#"{"success":false,"error":"x"}"#),
                    ])),
                )
                .with_tool_response(
                    "c3",
                    Err(rmcp::model::ErrorData::invalid_params("no such tool", None)),
                ),
        ));
        assert!(matches!(
            failed.as_slice(),
            [
                CoreEvent::ToolCompleted { is_error: true, .. },
                CoreEvent::ToolCompleted { is_error: true, text, .. },
            ] if text == "no such tool"
        ));
    }

    #[test]
    fn compression_shows_as_status_lines() {
        let mut stream = TurnStream::new("s1");
        let lines: Vec<CoreEvent> = [
            AgentEvent::Compacting,
            AgentEvent::Compacted { tokens: 1 },
            AgentEvent::CompactionFailed(None),
        ]
        .into_iter()
        .flat_map(|e| stream.agent_event(e))
        .collect();
        let kinds: Vec<&str> = lines
            .iter()
            .map(|e| match e {
                CoreEvent::Status { kind, .. } => kind.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(kinds, ["compacting", "compacted", "compacted"]);
    }
}
