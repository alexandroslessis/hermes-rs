//! Provider inference operation for the unrolled agent loop.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use hermes_llm::base::{MessageStream, Provider};
use hermes_llm::conversation::message::{InferenceMetadata, Message, MessageContent};
use hermes_llm::conversation::token_usage::ProviderUsage;
use hermes_llm::conversation::{
    agent_visible_arc, effective_role, fix_conversation, merge_consecutive_messages_for_request,
    Conversation, EffectiveRole,
};
use hermes_llm::errors::ProviderError;
use hermes_llm::model::ModelConfig;
use tracing_futures::Instrument;

use crate::events::AgentEvent;
use crate::fallback::{switch_notice, Bound, ChainRun, FallbackChain, FallbackTarget, Pending};
use crate::operation::{
    applied, messages_since_kickoff, not_applicable, trailing_error, Emitter, Inference,
    InferenceInput, MachineSession, Operation, OperationResult, CLIENT_LOG,
};

pub struct PreparedInferenceRequest {
    pub system_prompt: String,
    pub tools: Vec<rmcp::model::Tool>,
    pub additional_messages: Vec<Message>,
}

#[async_trait]
pub trait InferenceRequestPreparer<S>: Send + Sync {
    async fn prepare_session(&self, _session: &S) -> Result<Option<S>> {
        Ok(None)
    }

    async fn prepare(
        &self,
        session: &S,
        conversation: &Conversation,
        input: InferenceInput,
    ) -> Result<PreparedInferenceRequest>;
}

pub struct IdentityInferenceRequestPreparer;

#[async_trait]
impl<S: Sync> InferenceRequestPreparer<S> for IdentityInferenceRequestPreparer {
    async fn prepare(
        &self,
        _session: &S,
        _conversation: &Conversation,
        input: InferenceInput,
    ) -> Result<PreparedInferenceRequest> {
        Ok(PreparedInferenceRequest {
            system_prompt: input
                .prompt_parts
                .into_iter()
                .map(|(_, part)| part)
                .collect::<Vec<_>>()
                .join("\n\n"),
            tools: input.tools,
            additional_messages: Vec::new(),
        })
    }
}

pub trait InferenceEffect: From<Message> + Send + 'static {
    fn record_usage(usage: ProviderUsage) -> Self;
}

pub const EMPTY_RESPONSE_MESSAGE: &str =
    "The model returned an empty response. Please resend your message to continue.";
/// Retries of an empty reply when the caller sets none (the `balanced` budget profile's value).
pub const DEFAULT_EMPTY_RESPONSE_RETRIES: usize = 3;
const EMPTY_RESPONSE_NOTE_SCOPE: &str = "inference";
const EMPTY_RESPONSE_NOTE: &str = "empty_response";

/// The model's response stayed empty after retries. The fallback message is stored
/// hidden so that operations owning the end of a turn (recipe retries, final output)
/// can take over; when none does, it is revealed to the user.
pub fn is_empty_response_marker(message: &Message) -> bool {
    message
        .metadata
        .operation_note(EMPTY_RESPONSE_NOTE_SCOPE, EMPTY_RESPONSE_NOTE)
        .is_some()
}

fn is_thinking(content: &MessageContent) -> bool {
    matches!(
        content,
        MessageContent::Thinking(_) | MessageContent::RedactedThinking(_)
    )
}

fn drop_repeated_tool_call_thinking(accumulator: &Conversation, chunk: &mut Message) {
    if !chunk
        .content
        .iter()
        .any(|content| matches!(content, MessageContent::ToolRequest(_)))
    {
        return;
    }
    let prior: Vec<&MessageContent> = accumulator
        .iter()
        .filter(|message| message.role == chunk.role)
        .flat_map(|message| message.content.iter())
        .filter(|content| is_thinking(content))
        .collect();
    chunk
        .content
        .retain(|content| !(is_thinking(content) && prior.contains(&content)));
}

fn pending_operation_logs(messages: &[Arc<Message>]) -> Vec<String> {
    let mut seen = messages
        .iter()
        .flat_map(|message| message.metadata.operation_logs.iter().cloned())
        .collect::<HashSet<_>>();
    let mut logs = Vec::new();

    for (operation, line) in messages
        .iter()
        .filter_map(|message| message.metadata.operations.as_deref())
        .flat_map(|operations| operations.iter())
        .filter_map(|(operation, notes)| {
            notes
                .get(CLIENT_LOG)
                .and_then(serde_json::Value::as_str)
                .map(|line| (operation, line))
        })
    {
        let log = format!("ops_{operation}: {line}");
        if seen.insert(log.clone()) {
            logs.push(log);
        }
    }

    logs
}

fn attach_operation_logs(message: &mut Message, logs: &mut Vec<String>) {
    if message.role == rmcp::model::Role::Assistant && !logs.is_empty() {
        message.metadata.operation_logs = std::mem::take(logs);
    }
}

fn is_empty_response(message: &Message) -> bool {
    message.content.iter().all(|content| match content {
        MessageContent::Text(text) => text.text.trim().is_empty(),
        MessageContent::Thinking(thinking) => {
            thinking.thinking.trim().is_empty() && thinking.signature.is_empty()
        }
        _ => false,
    })
}

pub fn ends_with_successful_tool_response(messages: &[Arc<Message>]) -> bool {
    let Some(message) = messages.last() else {
        return false;
    };
    let mut responses = message
        .content
        .iter()
        .filter_map(MessageContent::as_tool_response)
        .peekable();
    responses.peek().is_some()
        && responses.all(|response| {
            response
                .tool_result
                .as_ref()
                .is_ok_and(|result| !result.is_error.unwrap_or(false))
        })
}

fn record_request_params(span: &tracing::Span, model_config: &ModelConfig) {
    if let Some(temperature) = model_config.temperature {
        span.record("gen_ai.request.temperature", temperature as f64);
    }
    if let Some(max_tokens) = model_config.max_tokens {
        span.record("gen_ai.request.max_tokens", max_tokens as i64);
    }
}

pub fn record_chat_usage(span: &tracing::Span, usage: &ProviderUsage) {
    span.record("gen_ai.response.model", usage.model.as_str());
    if let Some(tokens) = usage.usage.input_tokens {
        span.record("gen_ai.usage.input_tokens", tokens);
    }
    if let Some(tokens) = usage.usage.output_tokens {
        span.record("gen_ai.usage.output_tokens", tokens);
    }
    if let Some(tokens) = usage.usage.cache_read_input_tokens {
        span.record("gen_ai.usage.cache_read.input_tokens", tokens);
    }
    if let Some(tokens) = usage.usage.cache_write_input_tokens {
        span.record("gen_ai.usage.cache_creation.input_tokens", tokens);
    }
    if let Some(reasons) = &usage.finish_reasons {
        let reasons_json = serde_json::to_string(reasons).unwrap_or_default();
        span.record("gen_ai.response.finish_reasons", reasons_json.as_str());
    }
    if let Some(id) = &usage.response_id {
        span.record("gen_ai.response.id", id.as_str());
    }
    if let Some(id) = &usage.request_id {
        span.record("gen_ai.request.id", id.as_str());
    }
}

#[derive(Default)]
struct InferenceOutput {
    accumulator: Conversation,
    additional_messages: Vec<Message>,
    usage: Vec<ProviderUsage>,
}

pub struct InferenceRunner<'a, S, E> {
    provider: Arc<dyn Provider>,
    model_config: ModelConfig,
    request_preparer: Arc<dyn InferenceRequestPreparer<S> + 'a>,
    output: Mutex<InferenceOutput>,
    empty_response_retries: usize,
    /// The providers after this one that a failing request may move to (Phase 12).
    chain: Option<FallbackChain>,
    run: Mutex<ChainRun>,
    effect: std::marker::PhantomData<fn() -> E>,
}

/// What serves a request: the provider and the model asked for.
struct Serving {
    provider: Arc<dyn Provider>,
    model: String,
}

/// The result of opening a request's stream.
enum Opened {
    Stream(MessageStream, Serving),
    /// Every provider that could serve it failed (or none was allowed): the error to show.
    Failed(ProviderError),
    /// The conversation does not fit the next provider's window: compress, then ask again.
    Compress,
}

/// The error shown when no provider of the chain could serve the request: the first one's, with
/// what happened at each position.
fn exhausted(first_error: Option<ProviderError>, trail: &[String]) -> ProviderError {
    with_trail(
        first_error.unwrap_or_else(|| {
            ProviderError::RequestFailed(
                "no provider of the fallback chain could serve the request".into(),
            )
        }),
        trail,
    )
}

/// Why a request left a provider, for the notice.
struct Cause {
    from: usize,
    /// A sentence naming the provider ("a has used up its request quota").
    reason: String,
    /// The provider is out of requests until its reset.
    quota: bool,
}

/// Where the next request of a failing one goes.
enum Pick {
    Use(usize),
    Compress(usize),
    Exhausted,
}

/// Appends what happened at each position of the chain to the error the user will read.
fn with_trail(mut error: ProviderError, trail: &[String]) -> ProviderError {
    if !trail.is_empty() {
        if let Some(details) = error.details_mut() {
            details.push_str(&format!("\n\nFallback chain: {}", trail.join("; ")));
        }
    }
    error
}

/// An error in one line, for the chain's trail.
fn brief(error: &ProviderError) -> String {
    let text = error.to_string().replace('\n', " ");
    match text.char_indices().nth(160) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text,
    }
}

/// The agent-visible conversation as the provider sees it: tool requests left
/// unanswered by an earlier turn are dropped, since nothing will answer them now.
///
/// hermes-rs (Phase 13a): the list holds the conversation's own messages (`Arc` clones) and
/// copies one only when something has to be taken out of it: a part meant for the user only,
/// or a tool request nothing answered.
fn messages_for_provider(
    conversation: &Conversation,
    turn: &[Arc<Message>],
    keep_empty_messages: bool,
) -> Vec<Arc<Message>> {
    let answered: HashSet<&str> = conversation
        .messages()
        .iter()
        .flat_map(|message| message.get_tool_response_ids())
        .collect();
    let start = conversation.len() - turn.len();
    conversation
        .messages()
        .iter()
        .enumerate()
        .filter(|(_, message)| message.is_agent_visible())
        .map(|(index, message)| {
            let mut message = agent_visible_arc(message);
            let unanswered = |content: &MessageContent| match content {
                MessageContent::ToolRequest(request) => !answered.contains(request.id.as_str()),
                _ => false,
            };
            if index < start && message.content.iter().any(unanswered) {
                Arc::make_mut(&mut message)
                    .content
                    .retain(|content| !unanswered(content));
            }
            message
        })
        .filter(|message| keep_empty_messages || !message.content.is_empty())
        .collect()
}

/// The messages as one provider takes them: signed reasoning of another provider dropped, the
/// conversation projected, repaired and merged. Messages that need none of that stay shared
/// with the conversation.
fn finalize_request(mut messages: Vec<Arc<Message>>, provider: &str) -> Conversation {
    drop_foreign_signed_thinking(&mut messages, provider);
    let projected = Conversation::from_arcs(messages).agent_visible_messages();
    let (fixed, _) = fix_conversation(Conversation::from_arcs(projected));
    Conversation::from_arcs(merge_consecutive_messages_for_request(
        fixed.messages().to_vec(),
    ))
}

/// Signed reasoning only verifies with the provider account that produced it.
fn drop_foreign_signed_thinking(messages: &mut [Arc<Message>], provider: &str) {
    let dropped = |content: &MessageContent| match content {
        MessageContent::Thinking(thinking) => !thinking.signature.is_empty(),
        MessageContent::RedactedThinking(_) => true,
        _ => false,
    };
    for message in messages {
        let foreign = message
            .metadata
            .inference
            .as_ref()
            .is_some_and(|inference| inference.provider != provider);
        if foreign && message.content.iter().any(dropped) {
            Arc::make_mut(message)
                .content
                .retain(|content| !dropped(content));
        }
    }
}

fn latest_provider_session_id<'a>(
    conversation: &'a Conversation,
    provider: &str,
) -> Option<&'a str> {
    conversation
        .messages()
        .iter()
        .rev()
        .find_map(|message| message.metadata.inference.as_ref())
        .filter(|inference| inference.provider == provider)
        .and_then(|inference| inference.provider_session_id.as_deref())
}

fn ends_with_provider_turn(messages: &[Arc<Message>]) -> bool {
    messages.last().is_some_and(|message| {
        matches!(
            effective_role(message),
            EffectiveRole::User | EffectiveRole::Tool
        )
    })
}

/// The messages the next request starts from, or `None` when no request should be made. The
/// projection is built once (with empty messages kept) and the version without them is taken
/// from it, so a request costs one pass over the conversation.
fn request_base(conversation: &Conversation, turn: &[Arc<Message>]) -> Option<Vec<Arc<Message>>> {
    let mut projected = messages_for_provider(conversation, turn, true);
    if projected
        .last()
        .is_some_and(|message| message.content.is_empty())
    {
        return None;
    }
    projected.retain(|message| !message.content.is_empty());
    ends_with_provider_turn(&projected).then_some(projected)
}

fn inference_span(provider: &dyn Provider, model_config: &ModelConfig) -> tracing::Span {
    let span = tracing::info_span!(
        target: "goose::state_machine",
        "chat",
        "gen_ai.operation.name" = "chat",
        "gen_ai.provider.name" = %provider.get_name(),
        "gen_ai.request.model" = %model_config.model_name,
        "gen_ai.request.temperature" = tracing::field::Empty,
        "gen_ai.request.max_tokens" = tracing::field::Empty,
        "gen_ai.response.model" = tracing::field::Empty,
        "gen_ai.response.finish_reasons" = tracing::field::Empty,
        "gen_ai.response.id" = tracing::field::Empty,
        "gen_ai.request.id" = tracing::field::Empty,
        "gen_ai.usage.input_tokens" = tracing::field::Empty,
        "gen_ai.usage.output_tokens" = tracing::field::Empty,
        "error.type" = tracing::field::Empty,
    );
    record_request_params(&span, model_config);
    span
}

impl<'a, S: Sync, E: InferenceEffect> InferenceRunner<'a, S, E> {
    pub fn new(provider: Arc<dyn Provider>, model_config: ModelConfig) -> Self {
        Self {
            provider,
            model_config,
            request_preparer: Arc::new(IdentityInferenceRequestPreparer),
            output: Mutex::new(InferenceOutput::default()),
            empty_response_retries: DEFAULT_EMPTY_RESPONSE_RETRIES,
            chain: None,
            run: Mutex::new(ChainRun::default()),
            effect: std::marker::PhantomData,
        }
    }

    /// The providers a request moves to when this one cannot serve it (see [`crate::fallback`]).
    /// An empty chain changes nothing.
    pub fn with_fallback(mut self, chain: FallbackChain) -> Self {
        self.chain = (!chain.targets.is_empty()).then_some(chain);
        self
    }

    /// How often an empty reply is requested again before the empty-response marker is given.
    pub fn with_empty_response_retries(mut self, retries: usize) -> Self {
        self.empty_response_retries = retries;
        self
    }

    /// Opens the stream of one request. Without a chain this is `provider.stream`. With one it
    /// also moves the request to the next provider when the failure is the provider's (see
    /// [`hermes_llm::fallback::classify`]), the governor refuses a provider, or the request
    /// does not fit a provider's window; the position is kept for the rest of the turn.
    #[allow(clippy::too_many_arguments)]
    async fn open_stream(
        &self,
        primary: &ModelConfig,
        max_tokens: Option<i32>,
        system_prompt: &str,
        request: &Conversation,
        base: Option<&[Arc<Message>]>,
        tools: &[rmcp::model::Tool],
        emit: &Emitter,
    ) -> Opened {
        let (Some(chain), Some(base)) = (&self.chain, base) else {
            return match self
                .provider
                .stream(primary, system_prompt, request.messages(), tools)
                .await
            {
                Ok(stream) => Opened::Stream(
                    stream,
                    Serving {
                        provider: self.provider.clone(),
                        model: primary.model_name.clone(),
                    },
                ),
                Err(error) => Opened::Failed(error),
            };
        };
        let primary_name = self.provider.get_name().to_string();
        let label = |index: usize| -> String {
            match index {
                0 => primary_name.clone(),
                n => chain.targets[n - 1].label.clone(),
            }
        };
        let tokens = chain
            .gate
            .estimate(system_prompt, request.messages(), tools);

        // Where this request starts: after a compression asked for a target, at that target;
        // else where the turn is now, unless the gate refuses that provider.
        let mut trail: Vec<String> = Vec::new();
        let mut first_error: Option<ProviderError> = None;
        let mut cause: Option<Cause> = None;
        let pending = self.run.lock().unwrap().pending.take();
        let start = match pending {
            Some(pending) => {
                trail = pending.trail;
                first_error = pending.first_error;
                cause = Some(Cause {
                    from: pending.from,
                    reason: pending.reason,
                    quota: pending.quota,
                });
                self.pick(chain, pending.index, tokens, &label, &mut trail)
            }
            None => {
                let current = self.run.lock().unwrap().current;
                match chain.gate.allow(current) {
                    Ok(()) => Pick::Use(current),
                    Err(why) => {
                        // Our own decision (the provider's configured daily limit is used up):
                        // no request is spent finding out.
                        let name = label(current);
                        trail.push(format!("{name}: {why}"));
                        first_error = Some(ProviderError::RateLimitExceeded {
                            details: why,
                            retry_delay: None,
                        });
                        cause = Some(Cause {
                            from: current,
                            reason: format!(
                                "{name} {}",
                                hermes_llm::fallback::Trigger::GovernorExhausted.describe()
                            ),
                            quota: true,
                        });
                        self.pick(chain, current + 1, tokens, &label, &mut trail)
                    }
                }
            }
        };
        let mut index = match start {
            Pick::Use(index) => index,
            Pick::Compress(next) => {
                return self.ask_compression(chain, next, cause, first_error, trail);
            }
            Pick::Exhausted => return Opened::Failed(exhausted(first_error, &trail)),
        };

        loop {
            self.settle(chain, index, &mut cause, emit, &label);
            let (provider, config, name) = if index == 0 {
                (self.provider.clone(), primary.clone(), label(0))
            } else {
                let target: &FallbackTarget = &chain.targets[index - 1];
                let config = match max_tokens {
                    Some(tokens) => target.model_config.clone().with_max_tokens(Some(tokens)),
                    None => target.model_config.clone(),
                };
                (target.provider.clone(), config, target.label.clone())
            };
            let opened = if index == 0 {
                provider
                    .stream(&config, system_prompt, request.messages(), tools)
                    .await
            } else {
                let request = finalize_request(base.to_vec(), &name);
                provider
                    .stream(&config, system_prompt, request.messages(), tools)
                    .await
            };
            let error = match opened {
                Ok(mut stream) => {
                    // The first item decides: an error before anything arrived is still a failure
                    // of the request, not of a response that was half delivered.
                    match stream.next().await {
                        Some(Err(error)) => error,
                        first => {
                            // An ended stream is not polled again (not every stream is fused).
                            let stream: MessageStream = match first {
                                Some(item) => {
                                    Box::pin(futures::stream::once(async { item }).chain(stream))
                                }
                                None => Box::pin(futures::stream::empty()),
                            };
                            return Opened::Stream(
                                stream,
                                Serving {
                                    provider,
                                    model: config.model_name.clone(),
                                },
                            );
                        }
                    }
                }
                Err(error) => error,
            };
            let Some(trigger) = hermes_llm::fallback::classify(&error) else {
                // Not the provider's failure (auth, a bad request, context length...): the same
                // request fails anywhere, and a wrong key is for the user to see.
                trail.push(format!("{name}: {}", brief(&error)));
                let shown = if first_error.is_none() {
                    error
                } else {
                    with_trail(error, &trail)
                };
                return Opened::Failed(shown);
            };
            chain.gate.failed(index, trigger, &error);
            tracing::warn!(provider = %name, trigger = trigger.as_str(), "request failed, trying the next provider: {error}");
            trail.push(format!("{name}: {}", brief(&error)));
            cause = Some(Cause {
                from: index,
                reason: format!("{name} {}", trigger.describe()),
                quota: trigger.is_quota(),
            });
            first_error.get_or_insert(error);
            match self.pick(chain, index + 1, tokens, &label, &mut trail) {
                Pick::Use(next) => index = next,
                Pick::Compress(next) => {
                    return self.ask_compression(chain, next, cause, first_error, trail);
                }
                Pick::Exhausted => return Opened::Failed(exhausted(first_error, &trail)),
            }
        }
    }

    /// The first position from `start` on that the gate allows and the request fits.
    fn pick(
        &self,
        chain: &FallbackChain,
        start: usize,
        tokens: usize,
        label: &dyn Fn(usize) -> String,
        trail: &mut Vec<String>,
    ) -> Pick {
        for index in start.max(1)..=chain.targets.len() {
            let target = &chain.targets[index - 1];
            if let Err(why) = chain.gate.allow(index) {
                trail.push(format!("{}: skipped, {why}", label(index)));
                continue;
            }
            if let Some(limit) = target.fit_limit.filter(|&limit| tokens > limit) {
                // Compression first (once per target and turn); never an over-limit request.
                if self.run.lock().unwrap().compress_asked.insert(index) {
                    return Pick::Compress(index);
                }
                trail.push(format!(
                    "{}: skipped, the conversation (about {tokens} tokens) does not fit its window ({limit} tokens)",
                    label(index)
                ));
                continue;
            }
            return Pick::Use(index);
        }
        Pick::Exhausted
    }

    /// Records that `index` should be tried once the conversation is compressed for its window,
    /// and tells the turn's compress step which window that is.
    fn ask_compression(
        &self,
        chain: &FallbackChain,
        index: usize,
        cause: Option<Cause>,
        first_error: Option<ProviderError>,
        trail: Vec<String>,
    ) -> Opened {
        let target = &chain.targets[index - 1];
        chain.shared.bind(Some(Bound {
            label: target.label.clone(),
            provider: target.provider.clone(),
            model: target.model_config.model_name.clone(),
            window: target.window,
        }));
        let cause = cause.unwrap_or(Cause {
            from: 0,
            reason: String::new(),
            quota: false,
        });
        self.run.lock().unwrap().pending = Some(Pending {
            index,
            from: cause.from,
            reason: cause.reason,
            quota: cause.quota,
            first_error,
            trail,
        });
        // The conversation is rebuilt after the compression, so what this call prepared is dropped.
        self.output().additional_messages.clear();
        Opened::Compress
    }

    /// The request goes to `index`: remember it for the rest of the turn, bind the compress step
    /// to its window, and tell the user which provider now has the conversation.
    fn settle(
        &self,
        chain: &FallbackChain,
        index: usize,
        cause: &mut Option<Cause>,
        emit: &Emitter,
        label: &dyn Fn(usize) -> String,
    ) {
        let previous = {
            let mut run = self.run.lock().unwrap();
            std::mem::replace(&mut run.current, index)
        };
        if previous != index || cause.is_some() {
            chain.shared.bind((index > 0).then(|| {
                let target = &chain.targets[index - 1];
                Bound {
                    label: target.label.clone(),
                    provider: target.provider.clone(),
                    model: target.model_config.model_name.clone(),
                    window: target.window,
                }
            }));
        }
        let Some(cause) = cause.take() else {
            return;
        };
        let model = if index == 0 {
            self.model_config.model_name.clone()
        } else {
            chain.targets[index - 1].model_config.model_name.clone()
        };
        emit.emit(AgentEvent::Status {
            kind: "fallback",
            text: switch_notice(
                &cause.reason,
                &label(index),
                &model,
                &label(0),
                cause.quota && cause.from == 0,
            ),
        });
    }

    fn output(&self) -> MutexGuard<'_, InferenceOutput> {
        self.output.lock().unwrap()
    }

    fn take_output(&self) -> Vec<E> {
        let output = std::mem::take(&mut *self.output());
        let mut effects: Vec<E> = output
            .additional_messages
            .into_iter()
            .map(E::from)
            .collect();
        effects.extend(output.usage.into_iter().map(E::record_usage));
        effects.extend(output.accumulator.into_iter().map(E::from));
        effects
    }

    fn emit_message(&self, message: Message, emit: &Emitter) {
        let message = message.with_generated_id_if_missing();
        self.output().accumulator.push(message.clone());
        emit.message(message);
    }

    fn error_outcome(
        &self,
        err: &ProviderError,
        operation_logs: &mut Vec<String>,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        tracing::Span::current().record("error.type", err.telemetry_type());
        tracing::error!("LLM provider error: {err}");
        let mut message = Message::from_provider_error(err);
        attach_operation_logs(&mut message, operation_logs);
        self.emit_message(message, emit);
        applied(self.take_output())
    }
}

#[async_trait]
impl<S: Sync, E: InferenceEffect> Operation<S, E> for InferenceRunner<'_, S, E> {
    fn name(&self) -> &'static str {
        "llm"
    }

    async fn finalize_cancellation(
        &self,
        _session: &S,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Vec<E> {
        self.take_output()
    }
}

#[async_trait]
impl<S: MachineSession, E: InferenceEffect> Inference<S, E> for InferenceRunner<'_, S, E> {
    fn applies(&self, conversation: &Conversation) -> bool {
        let Ok(turn) = messages_since_kickoff(conversation) else {
            return false;
        };
        trailing_error(conversation).is_none() && request_base(conversation, turn).is_some()
    }

    async fn prepare_session(&self, session: &S) -> Result<Option<S>> {
        self.request_preparer.prepare_session(session).await
    }

    async fn infer(
        &self,
        session: &S,
        conversation: &Conversation,
        input: InferenceInput,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let messages = messages_since_kickoff(conversation)?;
        if trailing_error(conversation).is_some() {
            return not_applicable();
        }

        let Some(mut messages_for_provider) = request_base(conversation, messages) else {
            return not_applicable();
        };
        let mut operation_logs = pending_operation_logs(messages);

        let model_config = session
            .thinking_effort()
            .map(|effort| self.model_config.clone().with_thinking_effort(effort))
            .unwrap_or_else(|| self.model_config.clone());
        let model_config = match session.max_tokens_override() {
            Some(tokens) => model_config.with_max_tokens(Some(tokens)),
            None => model_config,
        };
        let span = inference_span(self.provider.as_ref(), &model_config);

        async {
            let PreparedInferenceRequest {
                system_prompt,
                tools,
                additional_messages,
            } = self
                .request_preparer
                .prepare(session, conversation, input)
                .await?;

            for message in &additional_messages {
                messages_for_provider.push(Arc::new(message.clone()));
            }
            self.output().additional_messages = additional_messages;

            let provider_name = self.provider.get_name();
            // The fallback chain takes the same messages, finished for its own provider.
            let fallback_base = self.chain.is_some().then(|| messages_for_provider.clone());
            if let Some(session_id) = latest_provider_session_id(conversation, provider_name) {
                if let Err(error) = self.provider.resume(session_id).await {
                    tracing::warn!(
                        provider = provider_name,
                        %error,
                        "Could not resume provider session; continuing with a handoff"
                    );
                }
            }

            let conversation_for_provider = finalize_request(messages_for_provider, provider_name);
            let successful_tool_response =
                ends_with_successful_tool_response(conversation.messages());
            let mut empty_responses = 0;
            let empty_output = loop {
                let attempt_start = self.output().usage.len();
                let opened = self
                    .open_stream(
                        &model_config,
                        session.max_tokens_override(),
                        &system_prompt,
                        &conversation_for_provider,
                        fallback_base.as_deref(),
                        &tools,
                        emit,
                    )
                    .await;
                let (mut stream, serving) = match opened {
                    Opened::Stream(stream, serving) => (stream, serving),
                    Opened::Failed(err) => {
                        return self.error_outcome(&err, &mut operation_logs, emit)
                    }
                    // The turn's compress step makes the conversation fit the next provider; this
                    // runs again after it.
                    Opened::Compress => {
                        self.output().additional_messages.clear();
                        return applied(Vec::<E>::new());
                    }
                };

                let requested_model = serving.model.clone();
                let resolved_model = serving
                    .provider
                    .fetch_model_info(&requested_model)
                    .await
                    .ok()
                    .and_then(|model_info| model_info.resolved_model);
                let provider_session_id = serving.provider.provider_session_id();
                let inference = Some(InferenceMetadata {
                    provider: serving.provider.get_name().to_string(),
                    requested_model,
                    resolved_model,
                    provider_session_id,
                });

                let mut tool_request_ids = HashSet::new();
                while let Some(result) = stream.next().await {
                    let (msg_opt, usage_opt) = match result {
                        Ok(chunk) => chunk,
                        Err(err) => return self.error_outcome(&err, &mut operation_logs, emit),
                    };
                    if let Some(usage) = usage_opt {
                        record_chat_usage(&tracing::Span::current(), &usage);
                        let mut output = self.output();
                        output.usage.truncate(attempt_start);
                        output.usage.push(usage);
                    }
                    if let Some(mut chunk) = msg_opt {
                        if let Some(inference) = &inference {
                            chunk = chunk.with_inference_if_assistant(inference.clone());
                        }
                        chunk.content.retain(|content| match content {
                            MessageContent::ToolRequest(request) => {
                                tool_request_ids.insert(request.id.clone())
                            }
                            _ => true,
                        });
                        drop_repeated_tool_call_thinking(&self.output().accumulator, &mut chunk);
                        if chunk.content.is_empty() && !chunk.metadata.output_token_limit_reached {
                            self.output().accumulator.push(chunk);
                        } else {
                            attach_operation_logs(&mut chunk, &mut operation_logs);
                            self.emit_message(chunk, emit);
                        }
                    }
                }

                let empty_output = {
                    let output = self.output();
                    !output
                        .accumulator
                        .iter()
                        .any(|message| message.metadata.output_token_limit_reached)
                        && output
                            .accumulator
                            .iter()
                            .all(|message| is_empty_response(message))
                };
                if !empty_output || successful_tool_response || emit.cancel_token().is_cancelled() {
                    break empty_output;
                }
                self.output().accumulator.clear();
                if empty_responses < self.empty_response_retries {
                    empty_responses += 1;
                    tracing::warn!(
                        "Provider returned an empty response; retrying ({empty_responses}/{})",
                        self.empty_response_retries
                    );
                    continue;
                }
                let mut marker = Message::assistant()
                    .with_text(EMPTY_RESPONSE_MESSAGE)
                    .with_visibility(false, false);
                attach_operation_logs(&mut marker, &mut operation_logs);
                marker.metadata.set_operation_note(
                    EMPTY_RESPONSE_NOTE_SCOPE,
                    EMPTY_RESPONSE_NOTE,
                    serde_json::Value::Bool(true),
                );
                self.output().accumulator.push(marker);
                return applied(self.take_output());
            };

            if empty_output && successful_tool_response {
                let message = {
                    let mut output = self.output();
                    let mut message = output
                        .accumulator
                        .last()
                        .cloned()
                        .unwrap_or_else(Message::assistant);
                    message.content.clear();
                    message.metadata.user_visible = false;
                    message.metadata.agent_visible = true;
                    attach_operation_logs(&mut message, &mut operation_logs);
                    output.accumulator.clear();
                    message
                };
                self.emit_message(message, emit);
            }
            applied(self.take_output())
        }
        .instrument(span)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_session_id_comes_only_from_latest_inference() {
        let conversation = Conversation::new_unvalidated([
            Message::assistant().with_inference(InferenceMetadata {
                provider: "provider-a".to_string(),
                requested_model: "model".to_string(),
                resolved_model: None,
                provider_session_id: Some("session-a".to_string()),
            }),
            Message::assistant().with_inference(InferenceMetadata {
                provider: "provider-b".to_string(),
                requested_model: "model".to_string(),
                resolved_model: None,
                provider_session_id: Some("session-b".to_string()),
            }),
        ]);

        assert_eq!(
            latest_provider_session_id(&conversation, "provider-b"),
            Some("session-b")
        );
        assert_eq!(
            latest_provider_session_id(&conversation, "provider-a"),
            None
        );
    }

    #[test]
    fn signed_thinking_without_text_is_not_an_empty_response() {
        assert!(is_empty_response(
            &Message::assistant().with_content(MessageContent::thinking("", ""))
        ));
        assert!(!is_empty_response(
            &Message::assistant().with_content(MessageContent::thinking("", "sig-omitted"))
        ));
    }

    #[test]
    fn whitespace_only_text_is_an_empty_response() {
        assert!(is_empty_response(&Message::assistant().with_text(" \n\t ")));
    }

    /// The projection as it was before Phase 13a: every agent-visible message copied.
    fn copying_messages_for_provider(
        conversation: &Conversation,
        turn: &[Arc<Message>],
        keep_empty_messages: bool,
    ) -> Vec<Message> {
        let answered: HashSet<&str> = conversation
            .messages()
            .iter()
            .flat_map(|message| message.get_tool_response_ids())
            .collect();
        let start = conversation.len() - turn.len();
        conversation
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, message)| message.is_agent_visible())
            .map(|(index, message)| {
                let mut message = message.agent_visible_content();
                if index < start {
                    message.content.retain(|content| match content {
                        MessageContent::ToolRequest(request) => {
                            answered.contains(request.id.as_str())
                        }
                        _ => true,
                    });
                }
                message
            })
            .filter(|message| keep_empty_messages || !message.content.is_empty())
            .collect()
    }

    fn user_only_text(text: &str) -> MessageContent {
        use rmcp::model::{Annotations, Role, TextContent};
        MessageContent::Text(
            TextContent::new(text)
                .with_annotations(Annotations::default().with_audience(vec![Role::User])),
        )
    }

    fn history() -> Conversation {
        use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
        let call = |id: &str| {
            Ok(CallToolRequestParams::new("terminal")
                .with_arguments(rmcp::object!({"command": id})))
        };
        Conversation::new_unvalidated([
            Message::user().with_text("first question"),
            // An earlier turn's request nothing answered: dropped from what the model sees.
            Message::assistant()
                .with_text("trying")
                .with_tool_request("lost", call("lost")),
            Message::user().with_content(user_only_text("shown to the user only")),
            Message::user().with_text("second question"),
            Message::assistant()
                .with_text("running")
                .with_tool_request("c1", call("c1")),
            Message::user().with_tool_response(
                "c1",
                Ok(CallToolResult::success(vec![ContentBlock::text("done")])),
            ),
            Message::assistant().with_text("answer").agent_only(),
            Message::assistant().user_only(),
            Message::user().with_text("third question"),
        ])
    }

    #[test]
    fn the_request_projection_matches_the_copying_one_and_shares_what_it_can() {
        let conversation = history();
        let turn = messages_since_kickoff(&conversation).unwrap();
        for keep_empty in [true, false] {
            let shared = messages_for_provider(&conversation, turn, keep_empty);
            let copied = copying_messages_for_provider(&conversation, turn, keep_empty);
            assert_eq!(shared.len(), copied.len());
            for (shared, copied) in shared.iter().zip(&copied) {
                assert_eq!(&**shared, copied);
            }
        }
        // Messages that needed no change are the conversation's own, not copies.
        let shared = messages_for_provider(&conversation, turn, false);
        let own = |text: &str| {
            conversation
                .iter()
                .find(|message| message.as_concat_text() == text)
                .unwrap()
        };
        let projected = |text: &str| {
            shared
                .iter()
                .find(|message| message.as_concat_text() == text)
                .unwrap()
        };
        for text in [
            "first question",
            "second question",
            "third question",
            "running",
        ] {
            assert!(Arc::ptr_eq(own(text), projected(text)), "{text} was copied");
        }
        // The one whose unanswered request was taken out is a copy; the original is untouched.
        let trying = projected("trying");
        assert!(!Arc::ptr_eq(own("trying"), trying));
        assert!(trying.content.iter().all(|c| c.as_tool_request().is_none()));
        assert!(own("trying")
            .content
            .iter()
            .any(|c| c.as_tool_request().is_some()));
    }

    #[test]
    fn a_request_is_made_only_when_the_model_is_next() {
        let conversation = history();
        let turn = messages_since_kickoff(&conversation).unwrap();
        assert!(request_base(&conversation, turn).is_some());
        let mut answered = conversation.clone();
        answered.push(Message::assistant().with_text("done"));
        let turn = messages_since_kickoff(&answered).unwrap();
        assert!(request_base(&answered, turn).is_none());
    }

    #[test]
    fn reasoning_signed_elsewhere_is_dropped_from_a_copy_only() {
        let signed = |provider: &str| {
            Message::assistant()
                .with_thinking("thought", "sig")
                .with_text("reply")
                .with_inference(InferenceMetadata {
                    provider: provider.to_string(),
                    requested_model: "m".to_string(),
                    resolved_model: None,
                    provider_session_id: None,
                })
        };
        let own = Arc::new(signed("a"));
        let foreign = Arc::new(signed("b"));
        let mut messages = vec![own.clone(), foreign.clone()];
        drop_foreign_signed_thinking(&mut messages, "a");
        assert!(Arc::ptr_eq(&messages[0], &own));
        assert!(!Arc::ptr_eq(&messages[1], &foreign));
        assert_eq!(messages[1].content.len(), 1);
        assert_eq!(
            foreign.content.len(),
            2,
            "the conversation's message is untouched"
        );
    }
}
