use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use hermes_llm::conversation::{
    message::{Message, MessageContent, ToolRequest},
    Conversation,
};
use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorData, Tool};

use crate::operation::{
    applied, messages_since_kickoff, not_applicable, Emitter, Operation, OperationResult,
};

fn pending_requests(requests: Vec<ToolRequest>, tool_names: &HashSet<&str>) -> Vec<ToolRequest> {
    requests
        .into_iter()
        .filter(|request| {
            request
                .tool_call
                .as_ref()
                .map_or(true, |call| tool_names.contains(call.name.as_ref()))
        })
        .collect()
}

/// Supplies tools whose definitions and implementations may vary by session.
///
/// A provider's tool names and their handlers must remain stable from an inference
/// advertisement until every tool call produced by that inference has been handled.
#[async_trait]
pub trait ToolProvider<S>: Send + Sync {
    async fn tools(&self, session: &S) -> Result<Vec<Tool>>;

    async fn call(
        &self,
        session: &S,
        request_id: &str,
        call: CallToolRequestParams,
        emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData>;
}

/// An agent operation that advertises and dispatches tools.
///
/// Tools are supplied at runtime by [`ToolProvider`]s whose definitions may vary by session.
pub struct ToolOperation<S> {
    providers: Vec<Arc<dyn ToolProvider<S>>>,
    response: Mutex<Option<Message>>,
}

impl<S> ToolOperation<S>
where
    S: Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            response: Mutex::new(None),
        }
    }

    pub fn with_provider(mut self, provider: Arc<dyn ToolProvider<S>>) -> Self {
        self.providers.push(provider);
        self
    }

    async fn available_tools(&self, session: &S) -> Result<Vec<(Tool, &dyn ToolProvider<S>)>> {
        let mut available: Vec<(Tool, &dyn ToolProvider<S>)> = Vec::new();
        for provider in &self.providers {
            for tool in provider.tools(session).await? {
                if available
                    .iter()
                    .any(|(available, _)| available.name == tool.name)
                {
                    anyhow::bail!("multiple tool providers registered '{}'", tool.name);
                }
                available.push((tool, provider.as_ref()));
            }
        }
        Ok(available)
    }
}

impl<S> Default for ToolOperation<S>
where
    S: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<S, E> Operation<S, E> for ToolOperation<S>
where
    S: Send + Sync + 'static,
    E: From<Message> + Send + 'static,
{
    fn name(&self) -> &'static str {
        "tools"
    }

    async fn finalize_cancellation(
        &self,
        _session: &S,
        _conversation: &Conversation,
        emit: &Emitter,
    ) -> Vec<E> {
        let response = self.response.lock().unwrap().take();
        match response {
            Some(response) => vec![E::from(emit.message(response))],
            None => Vec::new(),
        }
    }

    async fn inference_tools(&self, session: &S) -> Result<Vec<Tool>> {
        Ok(self
            .available_tools(session)
            .await?
            .into_iter()
            .map(|(tool, _)| tool)
            .collect())
    }

    async fn run(
        &self,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let turn = messages_since_kickoff(conversation)?;
        let answered: HashSet<&str> = turn
            .iter()
            .flat_map(|message| message.get_tool_response_ids())
            .collect();
        // hermes-rs: a message hidden from the model was withdrawn (a retried malformed or
        // truncated tool call); its calls must never run.
        let pending = turn
            .iter()
            .filter(|message| message.is_agent_visible())
            .flat_map(|message| message.content.iter())
            .filter_map(MessageContent::as_tool_request)
            .filter(|request| {
                !request.was_executed_externally() && !answered.contains(request.id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return not_applicable();
        }

        let available = self.available_tools(session).await?;
        let available_names = available
            .iter()
            .map(|(tool, _)| tool.name.as_ref())
            .collect();
        let pending = pending_requests(pending, &available_names);
        if pending.is_empty() {
            return not_applicable();
        }

        // hermes-rs: independent tool calls from one response run concurrently (the prompt tells
        // the model they do); tools that share state, like the terminal's shell, serialize
        // internally. Responses keep the order of the calls.
        let calls = pending.iter().map(|request| {
            let call = request.tool_call.as_ref().map(|call| {
                let provider = available
                    .iter()
                    .find(|(tool, _)| tool.name == call.name)
                    .map(|(_, provider)| *provider)
                    .expect("pending requests were filtered by available tools");
                (provider, call)
            });
            async move {
                match call {
                    Ok((provider, call)) => {
                        provider
                            .call(session, &request.id, call.clone(), emit)
                            .await
                    }
                    Err(error) => Err(error.clone()),
                }
            }
        });
        let results = futures::future::join_all(calls).await;
        for (request, tool_result) in pending.into_iter().zip(results) {
            let mut output = self.response.lock().unwrap();
            let message =
                output.get_or_insert_with(|| Message::user().with_generated_id_if_missing());
            message.add_tool_response_with_metadata(
                request.id,
                tool_result,
                request.metadata.as_ref(),
            );
        }
        let message = self
            .response
            .lock()
            .unwrap()
            .take()
            .expect("tool responses retained");
        let message = emit.message(message);
        applied([E::from(message)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lists(Vec<&'static str>);

    #[async_trait]
    impl ToolProvider<()> for Lists {
        async fn tools(&self, _session: &()) -> Result<Vec<Tool>> {
            Ok(self
                .0
                .iter()
                .map(|name| Tool::new(*name, "test", Arc::new(serde_json::Map::new())))
                .collect())
        }

        async fn call(
            &self,
            _session: &(),
            _request_id: &str,
            _call: CallToolRequestParams,
            _emit: &Emitter,
        ) -> Result<CallToolResult, ErrorData> {
            unreachable!()
        }
    }

    fn tools(providers: Vec<Vec<&'static str>>) -> ToolOperation<()> {
        providers
            .into_iter()
            .fold(ToolOperation::new(), |operation, names| {
                operation.with_provider(Arc::new(Lists(names)))
            })
    }

    async fn listed(operation: &ToolOperation<()>) -> Result<Vec<String>> {
        Ok(Operation::<(), Message>::inference_tools(operation, &())
            .await?
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect())
    }

    /// hermes-rs: the request's tool list never names a tool twice. (Goose's state machine also
    /// checked this across its steps; since Phase 13e there is one tool step, and this is the
    /// check: a duplicate fails the request instead of reaching the provider, which would reject it.)
    #[tokio::test]
    async fn a_tool_name_offered_twice_is_refused() {
        assert_eq!(
            listed(&tools(vec![vec!["a", "b"], vec!["c"]]))
                .await
                .unwrap(),
            ["a", "b", "c"]
        );
        for providers in [vec![vec!["a"], vec!["a"]], vec![vec!["a", "b", "a"]]] {
            let error = listed(&tools(providers)).await.unwrap_err();
            assert_eq!(error.to_string(), "multiple tool providers registered 'a'");
        }
    }
}
