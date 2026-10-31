//! End-to-end check of the OpenAI-compatible provider: request building, HTTP, SSE parsing and
//! tool-call reassembly, against a mock server that streams like an OpenAI-compatible endpoint.

use hermes_llm::api_client::{ApiClient, AuthMethod};
use hermes_llm::base::Provider;
use hermes_llm::conversation::message::Message;
use hermes_llm::model::ModelConfig;
use hermes_llm::openai_compatible::OpenAiCompatibleProvider;
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunks: &[Value]) -> String {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "model": "test-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    })
}

#[tokio::test]
async fn streams_text_and_reassembles_a_tool_call() {
    let server = MockServer::start().await;
    let body = sse(&[
        chunk(json!({"role": "assistant", "content": "Let me "}), None),
        chunk(json!({"content": "look."}), None),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                   "function": {"name": "read_file", "arguments": "{\"path\": "}}]}),
            None,
        ),
        chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"README.md\"}"}}]}),
            Some("tool_calls"),
        ),
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "test-model",
               "choices": [], "usage": {"prompt_tokens": 42, "completion_tokens": 7, "total_tokens": 49}}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let client = ApiClient::new_with_tls(
        format!("{}/v1", server.uri()),
        AuthMethod::BearerToken("test-key".into()),
        None,
    )
    .unwrap();
    let provider = OpenAiCompatibleProvider::new("test".into(), client, String::new());
    let tool: rmcp::model::Tool = serde_json::from_value(json!({
        "name": "read_file",
        "description": "Read a text file.",
        "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
    }))
    .unwrap();

    let (message, usage) = provider
        .complete(
            &ModelConfig::new("test-model"),
            "You are a test agent.",
            &[Message::user().with_text("What's in the README?")],
            &[tool],
        )
        .await
        .unwrap();

    assert_eq!(message.as_concat_text(), "Let me look.");
    let request = message
        .content
        .iter()
        .find_map(|content| content.as_tool_request())
        .expect("a tool call");
    assert_eq!(request.id, "call_1");
    let call = request.tool_call.as_ref().unwrap();
    assert_eq!(call.name, "read_file");
    assert_eq!(call.arguments.as_ref().unwrap()["path"], "README.md");
    assert_eq!(usage.usage.input_tokens, Some(42));
    assert_eq!(usage.usage.output_tokens, Some(7));

    let sent: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(sent["model"], "test-model");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["messages"][0]["content"], "You are a test agent.");
    assert_eq!(sent["messages"][1]["role"], "user");
    assert_eq!(sent["tools"][0]["function"]["name"], "read_file");
}
