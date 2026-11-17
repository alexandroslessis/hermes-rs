use async_trait::async_trait;
use futures::Stream;
use rmcp::model::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crate::{
    canonical::{map_to_canonical_model, CanonicalModelRegistry},
    conversation::{
        message::{Message, MessageContentBlock},
        token_usage::{ProviderUsage, Usage},
    },
    errors::ProviderError,
    model::ModelConfig,
    retry::RetryConfig,
};

/// How a model's thinking is replayed back to the provider on subsequent turns.
///
/// Cerebras rejects requests that replay `messages[].reasoning_content`, so such models
/// declare an inline `content` form instead.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingPreservationFormat {
    /// Prepend the thinking to the message content as plain text.
    ContentPrepend,
    /// Prepend the thinking to the message content wrapped in `<think>` tags.
    ContentXml,
    /// Replay in the separate `reasoning_content` field, the OpenAI-compatible default.
    ReasoningContent,
}

/// Information about a model's capabilities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    /// The name of the model
    pub name: String,
    /// The underlying model resolved from provider metadata, when the configured model is an alias or endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    /// The maximum context length this model supports
    pub context_limit: Option<usize>,
    /// Cost per token for input in USD (optional)
    pub input_token_cost: Option<f64>,
    /// Cost per token for output in USD (optional)
    pub output_token_cost: Option<f64>,
    /// Cost per cached input token read in USD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_token_cost: Option<f64>,
    /// Cost per input token written to cache in USD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_token_cost: Option<f64>,
    /// Currency for the costs (default: "$")
    pub currency: Option<String>,
    /// Whether this model supports cache control
    pub supports_cache_control: Option<bool>,
    /// Whether this model supports reasoning/thinking controls
    #[serde(default)]
    pub reasoning: bool,
    /// Whether this model accepts image input; when set, overrides canonical detection
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_preservation_format: Option<ThinkingPreservationFormat>,
    /// Static params merged into the request body for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_params: Option<HashMap<String, Value>>,
}

impl ModelInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            resolved_model: None,
            context_limit: None,
            input_token_cost: None,
            output_token_cost: None,
            cache_read_token_cost: None,
            cache_write_token_cost: None,
            currency: None,
            supports_cache_control: None,
            reasoning: false,
            supports_vision: None,
            thinking_preservation_format: None,
            request_params: None,
        }
    }

    pub fn with_context_limit(mut self, context_limit: usize) -> Self {
        self.context_limit = Some(context_limit);
        self
    }

    pub fn with_vision_support(mut self, supports_vision: bool) -> Self {
        self.supports_vision = Some(supports_vision);
        self
    }

    /// Create a new ModelInfo with cost information (per token)
    pub fn with_cost(
        name: impl Into<String>,
        context_limit: usize,
        input_cost: f64,
        output_cost: f64,
    ) -> Self {
        Self {
            name: name.into(),
            resolved_model: None,
            context_limit: Some(context_limit),
            input_token_cost: Some(input_cost),
            output_token_cost: Some(output_cost),
            cache_read_token_cost: None,
            cache_write_token_cost: None,
            currency: Some("$".to_string()),
            supports_cache_control: None,
            reasoning: false,
            supports_vision: None,
            thinking_preservation_format: None,
            request_params: None,
        }
    }
}

/// A message stream yields partial text content but complete tool calls, all within the Message object
/// So a message with text will contain potentially just a word of a longer response, but tool calls
/// messages will only be yielded once concatenated.
pub type MessageStream = Pin<
    Box<dyn Stream<Item = Result<(Option<Message>, Option<ProviderUsage>), ProviderError>> + Send>,
>;

pub fn model_info_for_provider_model(provider_name: &str, model_name: &str) -> ModelInfo {
    let registry = CanonicalModelRegistry::bundled().ok();
    let canonical = registry.as_ref().and_then(|registry| {
        let canonical_id = map_to_canonical_model(provider_name, model_name, registry)?;
        let (provider, model) = canonical_id.split_once('/')?;
        registry.get(provider, model)
    });

    let reasoning = canonical
        .as_ref()
        .and_then(|model| model.reasoning)
        .unwrap_or_else(|| ModelConfig::new(model_name).is_reasoning_model());

    ModelInfo {
        name: model_name.to_string(),
        resolved_model: None,
        context_limit: canonical.as_ref().map(|model| model.limit.context),
        input_token_cost: None,
        output_token_cost: None,
        cache_read_token_cost: None,
        cache_write_token_cost: None,
        currency: None,
        supports_cache_control: None,
        reasoning,
        supports_vision: None,
        thinking_preservation_format: None,
        request_params: None,
    }
}

/// Build `ModelInfo` for discovered model names, preferring metadata declared in
/// provider configuration over the canonical registry.
///
/// Configured entries are authoritative: a statically declared model carries its
/// own `context_limit`/`reasoning` values, which the canonical registry does not
/// know about. Names absent from `configured` fall back to registry metadata.
pub fn merge_configured_model_info(
    provider_name: &str,
    model_names: &[String],
    configured: &[ModelInfo],
) -> Vec<ModelInfo> {
    model_names
        .iter()
        .map(|model_name| {
            configured
                .iter()
                .find(|declared| declared.name == *model_name)
                .cloned()
                .unwrap_or_else(|| model_info_for_provider_model(provider_name, model_name))
        })
        .collect()
}

/// Collect all chunks from a MessageStream into a single Message and ProviderUsage
pub async fn collect_stream(
    mut stream: MessageStream,
) -> Result<(Message, ProviderUsage), ProviderError> {
    use futures::StreamExt;

    let mut final_message: Option<Message> = None;
    let mut final_usage: Option<ProviderUsage> = None;

    while let Some(result) = stream.next().await {
        let (msg_opt, usage_opt) = result?;

        if let Some(msg) = msg_opt {
            final_message = Some(match final_message {
                Some(mut prev) => {
                    // A multi-block message is a complete unit from the
                    // provider (e.g. a closing Thinking block bundled with the
                    // start of subsequent content), not a raw incremental
                    // delta — mirror Conversation::push's `content.len() == 1`
                    // gate so thinking coalescing only fires for single-block
                    // deltas, never absorbing the first block of a multi-block
                    // chunk into the prior message's last block.
                    let is_single_block_delta = msg.content.len() == 1;
                    for new_content in msg.content {
                        match (&mut prev.content.last_mut(), &new_content) {
                            // Coalesce consecutive text blocks
                            (
                                Some(MessageContentBlock::Text(last_text)),
                                MessageContentBlock::Text(new_text),
                            ) if last_text
                                .annotations
                                .as_ref()
                                .and_then(|a| a.audience.as_ref())
                                == new_text
                                    .annotations
                                    .as_ref()
                                    .and_then(|a| a.audience.as_ref()) =>
                            {
                                last_text.text.push_str(&new_text.text);
                            }
                            // Coalesce consecutive thinking blocks, mirroring
                            // Conversation::push's signature rules: append while
                            // the previous block is unsigned or the incoming
                            // delta shares its signature; a signed block never
                            // absorbs a differently-signed delta.
                            (
                                Some(MessageContentBlock::Thinking(last_thinking)),
                                MessageContentBlock::Thinking(new_thinking),
                            ) if is_single_block_delta
                                && (last_thinking.signature.is_empty()
                                    || new_thinking.signature == last_thinking.signature) =>
                            {
                                last_thinking.thinking.push_str(&new_thinking.thinking);
                                if !new_thinking.signature.is_empty() {
                                    last_thinking.signature = new_thinking.signature.clone();
                                }
                            }
                            _ => {
                                prev.content.push(new_content);
                            }
                        }
                    }
                    prev
                }
                None => msg,
            });
        }

        if let Some(usage) = usage_opt {
            final_usage = Some(usage);
        }
    }

    match (final_message, final_usage) {
        (Some(msg), usage) => {
            let usage = usage
                .unwrap_or_else(|| ProviderUsage::new("unknown".to_string(), Usage::default()));
            Ok((msg, usage))
        }
        (None, Some(usage)) => Ok((
            Message::new(
                rmcp::model::Role::Assistant,
                chrono::Utc::now().timestamp(),
                Vec::new(),
            ),
            usage,
        )),
        (None, None) => Err(ProviderError::ExecutionError(
            "Stream yielded no message".to_string(),
        )),
    }
}

pub fn stream_from_single_message(message: Message, usage: ProviderUsage) -> MessageStream {
    let stream = futures::stream::once(async move { Ok((Some(message), Some(usage))) });
    Box::pin(stream)
}

/// Base trait for AI providers (OpenAI, Anthropic, etc)
#[async_trait]
pub trait Provider: Send + Sync {
    /// Get the name of this provider instance
    fn get_name(&self) -> &str;

    fn provider_session_id(&self) -> Option<String> {
        None
    }

    async fn resume(&self, _session_id: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Primary streaming method that all providers must implement.
    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Arc<Message>],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError>;

    async fn complete(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Arc<Message>],
        tools: &[Tool],
    ) -> Result<(Message, ProviderUsage), ProviderError> {
        let stream = self.stream(model_config, system, messages, tools).await?;
        collect_stream(stream).await
    }

    /// Resolve the effective context limit for a model.
    ///
    /// `override_limit` is consumer policy and takes precedence over provider
    /// configuration and discovery. The method is infallible because providers
    /// fall through to canonical metadata and the global default.
    async fn get_context_limit(&self, model: &str, override_limit: Option<usize>) -> usize {
        crate::context_limit::ContextLimitResolver::new(self.get_name())
            .resolve(model, override_limit, || async { Ok(None) })
            .await
    }

    fn retry_config(&self) -> RetryConfig {
        RetryConfig::default()
    }

    async fn fetch_supported_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }

    async fn fetch_supported_model_info(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(self
            .fetch_supported_models()
            .await?
            .iter()
            .map(|model_name| model_info_for_provider_model(self.get_name(), model_name))
            .collect())
    }

    async fn fetch_model_info(&self, model_name: &str) -> Result<ModelInfo, ProviderError> {
        Ok(model_info_for_provider_model(self.get_name(), model_name))
    }

    /// Whether the provider manages its own conversation context (e.g. CLI
    /// wrappers like Claude Code or Gemini CLI). When true, goose-side
    /// context management such as tool-pair summarization is skipped because
    /// the provider's internal state is the source of truth.
    fn manages_own_context(&self) -> bool {
        false
    }

    fn uses_local_session_naming(&self) -> bool {
        self.manages_own_context()
    }

    fn supports_builtin_tools(&self) -> bool {
        !self.manages_own_context()
    }

    async fn refresh_credentials(&self) -> Result<(), ProviderError> {
        Err(ProviderError::NotImplemented(
            "credential refresh not supported by this provider".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn merge_configured_model_info_falls_back_to_registry_for_undeclared_models() {
        let declared = ModelInfo {
            reasoning: true,
            ..ModelInfo::new("declared-model").with_context_limit(4096)
        };

        let merged = merge_configured_model_info(
            "openai",
            &["declared-model".to_string(), "gpt-4o".to_string()],
            std::slice::from_ref(&declared),
        );

        assert_eq!(merged[0].context_limit, Some(4096));
        assert!(merged[0].reasoning);

        assert_eq!(merged[1].name, "gpt-4o");
        assert_eq!(
            merged[1].context_limit,
            model_info_for_provider_model("openai", "gpt-4o").context_limit
        );
    }

    fn content_from_str(s: String) -> MessageContentBlock {
        if let Some(img_data) = s.strip_prefix("*img:") {
            MessageContentBlock::image(format!("http://example.com/{}", img_data), "image/png")
        } else if let Some(tool_name) = s.strip_prefix("*tool:") {
            let tool_call = Ok(
                rmcp::model::CallToolRequestParams::new(tool_name.to_string())
                    .with_arguments(serde_json::Map::new()),
            );
            MessageContentBlock::tool_request(format!("tool_{}", tool_name), tool_call)
        } else {
            MessageContentBlock::text(s)
        }
    }

    fn create_test_stream(
        items: Vec<String>,
    ) -> impl Stream<Item = Result<(Option<Message>, Option<ProviderUsage>), ProviderError>> {
        use futures::stream;
        stream::iter(items.into_iter().map(|item| {
            let content = content_from_str(item);
            let message = Message::new(
                rmcp::model::Role::Assistant,
                chrono::Utc::now().timestamp(),
                vec![content],
            );
            Ok((Some(message), None))
        }))
    }

    fn content_to_strings(msg: &Message) -> Vec<String> {
        msg.content
            .iter()
            .map(|c| match c {
                MessageContentBlock::Text(t) => t.text.clone(),
                MessageContentBlock::Image(_) => "*img".to_string(),
                MessageContentBlock::ToolRequest(tr) => {
                    if let Ok(call) = &tr.tool_call {
                        format!("*tool:{}", call.name)
                    } else {
                        "*tool:error".to_string()
                    }
                }
                _ => "*other".to_string(),
            })
            .collect()
    }

    #[test_case(
        vec!["Hello", " ", "world"],
        vec!["Hello world"]
        ; "consecutive text coalesces"
    )]
    #[test_case(
        vec!["Hello", "*img:pic1", "world"],
        vec!["Hello", "*img", "world"]
        ; "non-text breaks coalescing"
    )]
    #[test_case(
        vec!["A", "B", "*img:pic1", "C", "D", "*tool:read", "E", "F"],
        vec!["AB", "*img", "CD", "*tool:read", "EF"]
        ; "multiple text groups"
    )]
    #[test_case(
        vec!["Text1", "*img:pic", "Text2"],
        vec!["Text1", "*img", "Text2"]
        ; "mixed content in chunk"
    )]
    #[tokio::test]
    async fn test_collect_stream_coalescing(input_items: Vec<&str>, expected: Vec<&str>) {
        let items: Vec<String> = input_items.into_iter().map(|s| s.to_string()).collect();
        let stream = create_test_stream(items);
        let (msg, _) = collect_stream(Box::pin(stream)).await.unwrap();
        assert_eq!(content_to_strings(&msg), expected);
    }

    #[tokio::test]
    async fn test_collect_stream_defaults_usage() {
        let stream = create_test_stream(vec!["Hello".to_string()]);
        let (msg, usage) = collect_stream(Box::pin(stream)).await.unwrap();
        assert_eq!(content_to_strings(&msg), vec!["Hello"]);
        assert_eq!(usage.model, "unknown");
    }

    #[tokio::test]
    async fn test_collect_stream_usage_only_yields_empty_message() {
        let usage = ProviderUsage::new("claude-sonnet-4".to_string(), Usage::default());
        let stream = futures::stream::once(async move { Ok((None, Some(usage))) });
        let (msg, usage) = collect_stream(Box::pin(stream)).await.unwrap();
        assert!(msg.content.is_empty());
        assert_eq!(usage.model, "claude-sonnet-4");
    }

    #[tokio::test]
    async fn test_collect_stream_no_message_no_usage_errors() {
        let stream = futures::stream::empty();
        let result = collect_stream(Box::pin(stream)).await;
        assert!(matches!(result, Err(ProviderError::ExecutionError(_))));
    }

    #[tokio::test]
    async fn test_collect_stream_preserves_text_audience_boundaries() {
        use futures::stream;
        use rmcp::model::{Annotations, Role, TextContent};

        let message = |text: &str, audience| {
            Message::assistant().with_content(MessageContentBlock::Text(
                TextContent::new(text)
                    .with_annotations(Annotations::default().with_audience(vec![audience])),
            ))
        };
        let stream = stream::iter([
            Ok((Some(message("public", Role::User)), None)),
            Ok((Some(message("private", Role::Assistant)), None)),
        ]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(message.content.len(), 2);
        assert_eq!(message.user_visible_content().as_concat_text(), "public");
        assert_eq!(message.agent_visible_content().as_concat_text(), "private");
    }

    #[tokio::test]
    async fn test_collect_stream_coalesces_thinking_deltas() {
        use futures::stream;

        let delta = |t: &str| Ok((Some(Message::assistant().with_thinking(t, "")), None));
        let stream = stream::iter([delta("Thinking"), delta(" Process"), delta(":")]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(
            message.content.len(),
            1,
            "thinking deltas must coalesce into one block, got {:?}",
            message.content
        );
        match &message.content[0] {
            MessageContentBlock::Thinking(t) => assert_eq!(t.thinking, "Thinking Process:"),
            other => panic!("expected Thinking, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_collect_stream_never_merges_distinctly_signed_thinking_blocks() {
        use futures::stream;

        let delta =
            |t: &str, sig: &str| Ok((Some(Message::assistant().with_thinking(t, sig)), None));
        // Two complete, independently-signed thinking blocks streamed back to
        // back must stay separate, not merge into one.
        let stream = stream::iter([delta("first", "sig-a"), delta("second", "sig-b")]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(message.content.len(), 2);
        match (&message.content[0], &message.content[1]) {
            (MessageContentBlock::Thinking(a), MessageContentBlock::Thinking(b)) => {
                assert_eq!(a.thinking, "first");
                assert_eq!(a.signature, "sig-a");
                assert_eq!(b.thinking, "second");
                assert_eq!(b.signature, "sig-b");
            }
            other => panic!("expected two Thinking blocks, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_collect_stream_unsigned_body_adopts_closing_signature() {
        use futures::stream;

        let delta =
            |t: &str, sig: &str| Ok((Some(Message::assistant().with_thinking(t, sig)), None));
        // An unsigned body streamed as several deltas, closed by a delta that
        // finally carries the signature — the whole block adopts it.
        let stream = stream::iter([
            delta("Thinking", ""),
            delta(" more", ""),
            delta("", "sig-final"),
        ]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(message.content.len(), 1);
        match &message.content[0] {
            MessageContentBlock::Thinking(t) => {
                assert_eq!(t.thinking, "Thinking more");
                assert_eq!(t.signature, "sig-final");
            }
            other => panic!("expected Thinking, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_collect_stream_unsigned_thinking_after_signed_starts_a_new_block() {
        use futures::stream;

        let delta =
            |t: &str, sig: &str| Ok((Some(Message::assistant().with_thinking(t, sig)), None));
        // An unsigned delta arriving after a signed (closed) block belongs to
        // the next block, not the closed one — signature-at-end streams emit
        // the first text of block N+1 before its own signature.
        let stream = stream::iter([delta("first", "sig-a"), delta("second", "")]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(message.content.len(), 2);
        match (&message.content[0], &message.content[1]) {
            (MessageContentBlock::Thinking(a), MessageContentBlock::Thinking(b)) => {
                assert_eq!(a.thinking, "first");
                assert_eq!(a.signature, "sig-a");
                assert_eq!(b.thinking, "second");
                assert_eq!(b.signature, "");
            }
            other => panic!("expected two Thinking blocks, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_collect_stream_multi_block_chunk_does_not_merge_into_prior_thinking() {
        use futures::stream;

        let unsigned_delta = |t: &str| Ok((Some(Message::assistant().with_thinking(t, "")), None));
        // A single chunk that already bundles a complete (signed) Thinking
        // block with subsequent content is a structured unit from the
        // provider, not a raw delta — its first block must not merge into
        // whatever unsigned Thinking preceded it (mirrors Conversation::push's
        // `content.len() == 1` gate). Regression for a maintainer-caught bug:
        // this used to sign the concatenation of "prior" + "next", erasing
        // the block boundary.
        let multi_block = Ok((
            Some(
                Message::assistant()
                    .with_thinking("next", "sig")
                    .with_text("after"),
            ),
            None,
        ));
        let stream = stream::iter([unsigned_delta("prior"), multi_block]);

        let (message, _) = collect_stream(Box::pin(stream)).await.unwrap();

        assert_eq!(message.content.len(), 3, "got {:?}", message.content);
        match (
            &message.content[0],
            &message.content[1],
            &message.content[2],
        ) {
            (
                MessageContentBlock::Thinking(a),
                MessageContentBlock::Thinking(b),
                MessageContentBlock::Text(c),
            ) => {
                assert_eq!(a.thinking, "prior");
                assert_eq!(
                    a.signature, "",
                    "prior block must stay unsigned and unmerged"
                );
                assert_eq!(b.thinking, "next");
                assert_eq!(b.signature, "sig");
                assert_eq!(c.text, "after");
            }
            other => panic!("expected Thinking, Thinking, Text, got {:?}", other),
        }
    }

    #[test]
    fn test_model_info_creation() {
        // Test direct ModelInfo creation
        let info = ModelInfo {
            name: "test-model".to_string(),
            resolved_model: None,
            context_limit: Some(1000),
            input_token_cost: None,
            output_token_cost: None,
            cache_read_token_cost: None,
            cache_write_token_cost: None,
            currency: None,
            supports_cache_control: None,
            reasoning: false,
            supports_vision: None,
            thinking_preservation_format: None,
            request_params: None,
        };
        assert_eq!(info.context_limit, Some(1000));

        // Test equality
        let info2 = ModelInfo {
            name: "test-model".to_string(),
            resolved_model: None,
            context_limit: Some(1000),
            input_token_cost: None,
            output_token_cost: None,
            cache_read_token_cost: None,
            cache_write_token_cost: None,
            currency: None,
            supports_cache_control: None,
            reasoning: false,
            supports_vision: None,
            thinking_preservation_format: None,
            request_params: None,
        };
        assert_eq!(info, info2);

        // Test inequality
        let info3 = ModelInfo {
            name: "test-model".to_string(),
            resolved_model: None,
            context_limit: Some(2000),
            input_token_cost: None,
            output_token_cost: None,
            cache_read_token_cost: None,
            cache_write_token_cost: None,
            currency: None,
            supports_cache_control: None,
            reasoning: false,
            supports_vision: None,
            thinking_preservation_format: None,
            request_params: None,
        };
        assert_ne!(info, info3);
    }

    #[test]
    fn test_model_info_deserializes_thinking_preservation_and_request_params() {
        let info: ModelInfo = serde_json::from_str(
            r#"{
                "name": "zai-glm-4.7",
                "context_limit": 131072,
                "thinking_preservation_format": "content_xml",
                "request_params": {"reasoning_format": "parsed"}
            }"#,
        )
        .unwrap();

        assert_eq!(
            info.thinking_preservation_format,
            Some(ThinkingPreservationFormat::ContentXml)
        );
        assert_eq!(
            info.request_params.unwrap().get("reasoning_format"),
            Some(&serde_json::json!("parsed"))
        );

        let bare: ModelInfo =
            serde_json::from_str(r#"{"name": "gpt-4o", "context_limit": 128000}"#).unwrap();
        assert_eq!(bare.thinking_preservation_format, None);
        assert_eq!(bare.request_params, None);
    }

    #[test]
    fn test_model_info_with_cost() {
        let info = ModelInfo::with_cost("gpt-4o", 128000, 0.0000025, 0.00001);
        assert_eq!(info.name, "gpt-4o");
        assert_eq!(info.context_limit, Some(128000));
        assert_eq!(info.input_token_cost, Some(0.0000025));
        assert_eq!(info.output_token_cost, Some(0.00001));
        assert_eq!(info.currency, Some("$".to_string()));
    }
}
