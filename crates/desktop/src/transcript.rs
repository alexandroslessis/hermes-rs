//! Our conversation in Hermes's terms: stored messages as `TranscriptMessage`s (what
//! `session.resume` returns) and a running turn's agent events as Hermes's stream events
//! (`message.delta`, `reasoning.delta`, `tool.start`, `tool.complete`).

use std::collections::HashMap;

use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::message::{Message, MessageContent};
use rmcp::model::Role;
use serde_json::{Value, json};

/// Stored messages as the transcript Hermes's frontend renders: user and assistant text, and
/// each tool call with its result.
pub fn messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    // Tool results arrive in a later message; they're filled into their call's entry.
    let mut calls: HashMap<String, usize> = HashMap::new();
    for message in messages.iter().filter(|m| m.is_user_visible()) {
        let timestamp = message.created as f64;
        let mut text = String::new();
        let mut reasoning = String::new();
        for content in &message.content {
            match content {
                MessageContent::Text(t) => text.push_str(&t.text),
                MessageContent::Thinking(t) => reasoning.push_str(&t.thinking),
                MessageContent::ToolRequest(request) => {
                    let (name, args) = match &request.tool_call {
                        Ok(call) => (
                            call.name.to_string(),
                            call.arguments.clone().map(Value::Object).unwrap_or_default(),
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
                _ => {}
            }
        }
        if text.trim().is_empty() && reasoning.is_empty() {
            continue;
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        out.push(json!({
            "role": role,
            "text": text,
            "reasoning": (!reasoning.is_empty()).then_some(reasoning),
            "timestamp": timestamp,
        }));
    }
    out
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
                    self.text.push_str(&t.text);
                    events.push(("message.delta", json!({"text": t.text})));
                }
                (Role::Assistant, MessageContent::Thinking(t)) if !t.thinking.is_empty() => {
                    events.push(("reasoning.delta", json!({"text": t.thinking})));
                }
                (Role::Assistant, MessageContent::ToolRequest(request)) => {
                    let (name, args) = match &request.tool_call {
                        Ok(call) => (
                            call.name.to_string(),
                            call.arguments.clone().map(Value::Object).unwrap_or_default(),
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
