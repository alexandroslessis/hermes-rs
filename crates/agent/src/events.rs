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
}
