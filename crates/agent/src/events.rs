use hermes_llm::conversation::{
    message::{Message, MessageUsage},
    token_usage::ProviderUsage,
    Conversation,
};
use rmcp::model::ServerNotification;

#[derive(Clone, Debug)]
pub enum AgentEvent {
    Message(Message),
    Usage(ProviderUsage),
    MessageUsage {
        message_id: Option<String>,
        usage: MessageUsage,
    },
    McpNotification((String, ServerNotification)),
    HistoryReplaced(Conversation),
    /// Context compression started (the summary request is running).
    Compacting,
    /// Context compression finished; the request is now about `tokens` tokens.
    Compacted {
        tokens: usize,
    },
    /// Context compression was skipped (`None`: nothing to summarize) or failed.
    CompactionFailed(Option<String>),
    /// A `delegate_task` child (`task`) started a tool call (`text`), under the parent's call.
    SubagentProgress {
        tool_call_id: String,
        task: usize,
        text: String,
    },
    /// A `delegate_task` child's activity for the desktop page (Hermes's `subagent.*` events):
    /// `kind` is `start`, `tool`, `thinking`, `progress` or `complete`; `payload` is Hermes's
    /// `SubagentEventPayload`.
    Subagent {
        kind: &'static str,
        payload: serde_json::Value,
    },
    /// Live progress from inside a running tool call (the terminal's output tail; Hermes's
    /// `agent/tool_progress_relay.py` fires the same `tool.progress` wire event for MCP tools).
    /// Emitted from the tool's own thread, throttled there.
    ToolProgress {
        tool_call_id: String,
        name: String,
        progress: f64,
        message: String,
    },
}
