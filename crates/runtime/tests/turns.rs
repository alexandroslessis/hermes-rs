//! Full agent turns against a scripted OpenAI-compatible mock server.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hermes_agent::events::AgentEvent;
use hermes_runtime::{
    ClarifyOutcome, ClarifyRequest, ProcessNotice, ProviderConfig, Runtime, RuntimeConfig,
    RuntimeEvent, TurnOptions, TurnStatus,
};
use hermes_tools::{UserBridge, detect_shell};
use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct Bridge;

#[async_trait]
impl UserBridge for Bridge {
    async fn clarify(&self, _session_id: &str, _request: ClarifyRequest) -> ClarifyOutcome {
        ClarifyOutcome::Undelivered
    }
    fn process_finished(&self, _session_id: &str, _notice: ProcessNotice) {}
}

/// Answers each chat-completions request with the next scripted SSE body, and records requests.
struct Script {
    replies: Mutex<VecDeque<String>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.requests.lock().push(request.body_json().unwrap());
        match self.replies.lock().pop_front() {
            Some(body) => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
            None => ResponseTemplate::new(500)
                .set_body_json(json!({"error": {"message": "script exhausted"}})),
        }
    }
}

fn sse(chunks: Vec<Value>) -> String {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn delta(delta: Value, finish: Option<&str>) -> Value {
    json!({"id": "c", "object": "chat.completion.chunk", "model": "test-model",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn usage() -> Value {
    json!({"id": "c", "object": "chat.completion.chunk", "model": "test-model", "choices": [],
           "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}})
}

fn text_reply(text: &str) -> String {
    sse(vec![
        delta(json!({"role": "assistant", "content": text}), Some("stop")),
        usage(),
    ])
}

fn tool_reply(calls: &[(&str, &str, Value)]) -> String {
    let calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(index, (id, name, args))| {
            json!({"index": index, "id": id, "type": "function",
                   "function": {"name": name, "arguments": args.to_string()}})
        })
        .collect();
    sse(vec![
        delta(
            json!({"role": "assistant", "tool_calls": calls}),
            Some("tool_calls"),
        ),
        usage(),
    ])
}

struct Harness {
    _server: MockServer,
    data: tempfile::TempDir,
    work: tempfile::TempDir,
    runtime: Runtime,
    provider: ProviderConfig,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn harness(replies: Vec<String>) -> Harness {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            replies: Mutex::new(replies.into()),
            requests: requests.clone(),
        })
        .mount(&server)
        .await;
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let runtime = runtime_in(data.path());
    let provider = ProviderConfig {
        id: "mock".into(),
        base_url: format!("{}/v1", server.uri()),
        api_key: Some("key".into()),
    };
    Harness {
        _server: server,
        data,
        work,
        runtime,
        provider,
        requests,
    }
}

fn runtime_in(data_dir: &Path) -> Runtime {
    Runtime::new(
        RuntimeConfig {
            data_dir: data_dir.to_path_buf(),
            shell: detect_shell(),
            max_turns: 20,
        },
        Arc::new(Bridge),
    )
    .unwrap()
}

/// Runs a turn to completion and returns its events.
async fn run_turn(h: &Harness, session_id: &str, text: &str) -> Vec<RuntimeEvent> {
    let mut turn = h
        .runtime
        .start_turn(session_id, &h.provider, TurnOptions::default(), text)
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(60), turn.events.recv())
        .await
        .unwrap()
    {
        let finished = matches!(event, RuntimeEvent::TurnFinished(_));
        events.push(event);
        if finished {
            break;
        }
    }
    events
}

fn finished_status(events: &[RuntimeEvent]) -> TurnStatus {
    match events.last() {
        Some(RuntimeEvent::TurnFinished(status)) => status.clone(),
        other => panic!("turn did not finish: {other:?}"),
    }
}

#[tokio::test]
async fn tool_loop_runs_and_keeps_the_system_prompt_byte_stable() {
    let h = harness(vec![
        tool_reply(&[(
            "call_1",
            "write_file",
            json!({"path": "hello.txt", "content": "hi there"}),
        )]),
        tool_reply(&[
            ("call_2", "read_file", json!({"path": "hello.txt"})),
            (
                "call_3",
                "search_files",
                json!({"pattern": "hi", "target": "content"}),
            ),
        ]),
        text_reply("Done: hello.txt says hi there."),
    ])
    .await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();

    let events = run_turn(&h, &session.id, "Create hello.txt").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(h.work.path().join("hello.txt")).unwrap(),
        "hi there"
    );

    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 3);
    let system: Vec<&Value> = requests
        .iter()
        .map(|r| &r["messages"][0]["content"])
        .collect();
    assert!(
        system.iter().all(|s| *s == system[0]),
        "system prompt changed between calls"
    );
    assert!(
        system[0]
            .as_str()
            .unwrap()
            .contains("# Hermes runtime environment")
    );

    // The third request carries both parallel tool results, in call order.
    let tool_messages: Vec<&Value> = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect();
    let ids: Vec<&str> = tool_messages
        .iter()
        .map(|m| m["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["call_1", "call_2", "call_3"]);
    assert!(
        tool_messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("1|hi there")
    );

    let messages = h.runtime.messages(&session.id).unwrap();
    assert_eq!(
        messages.last().unwrap().as_concat_text(),
        "Done: hello.txt says hi there."
    );
    let totals = h.runtime.store().usage_totals(&session.id).unwrap();
    assert_eq!(totals.calls, 3);
    assert_eq!(totals.input_tokens, 300);
}

#[tokio::test]
async fn streamed_text_arrives_as_events() {
    let h = harness(vec![sse(vec![
        delta(json!({"role": "assistant", "content": "Hel"}), None),
        delta(json!({"content": "lo"}), Some("stop")),
        usage(),
    ])])
    .await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    let events = run_turn(&h, &session.id, "hi").await;
    let texts: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Agent(AgentEvent::Message(m))
                if m.role == rmcp::model::Role::Assistant =>
            {
                Some(m.as_concat_text())
            }
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Hel", "lo"]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Agent(AgentEvent::Usage(_))))
    );
    let messages = h.runtime.messages(&session.id).unwrap();
    assert_eq!(messages.len(), 2, "chunks merge into one stored message");
    assert_eq!(messages[1].as_concat_text(), "Hello");
}

#[tokio::test]
async fn invented_tools_get_an_error_and_the_turn_continues() {
    let h = harness(vec![
        tool_reply(&[(
            "call_1",
            "browser_open",
            json!({"url": "https://example.com"}),
        )]),
        text_reply("I don't have a browser tool."),
    ])
    .await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    let events = run_turn(&h, &session.id, "open example.com").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let second = &h.requests.lock()[1];
    let tool = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(tool["content"].as_str().unwrap().contains("does not exist"));
}

#[tokio::test]
async fn provider_errors_end_the_turn_with_an_error_message() {
    let h = harness(vec![]).await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    let events = run_turn(&h, &session.id, "hi").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let messages = h.runtime.messages(&session.id).unwrap();
    assert!(messages.last().unwrap().error_kind().is_some());
    assert!(!h.runtime.is_running(&session.id));
}

#[tokio::test]
async fn retry_drops_the_error_and_answers_the_same_message() {
    // The first request fails (empty script → 500); the retry gets the scripted reply.
    let h = harness(vec![]).await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    run_turn(&h, &session.id, "hello").await;
    assert!(
        h.runtime
            .messages(&session.id)
            .unwrap()
            .last()
            .unwrap()
            .error_kind()
            .is_some()
    );
    let first_turn_requests = h.requests.lock().len();
    let mut turn = h
        .runtime
        .retry_turn(&session.id, &h.provider, TurnOptions::default())
        .unwrap();
    let mut first = None;
    while let Some(event) = turn.events.recv().await {
        first.get_or_insert_with(|| {
            matches!(event, RuntimeEvent::Agent(AgentEvent::HistoryReplaced(_)))
        });
        if matches!(event, RuntimeEvent::TurnFinished(_)) {
            break;
        }
    }
    assert_eq!(first, Some(true), "the UI is told the error was removed");
    // The retry fails again (the mock has no replies left): still exactly one error, after "hello".
    let messages = h.runtime.messages(&session.id).unwrap();
    assert_eq!(
        messages.iter().filter(|m| m.error_kind().is_some()).count(),
        1
    );
    assert_eq!(messages.len(), 2);
    // The provider layer retries failed HTTP calls, so a failing turn makes several requests.
    assert_eq!(h.requests.lock().len(), 2 * first_turn_requests);
}

#[tokio::test]
async fn retry_without_a_failed_turn_is_refused() {
    let h = harness(vec![text_reply("hi")]).await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    run_turn(&h, &session.id, "hello").await;
    let refused = h
        .runtime
        .retry_turn(&session.id, &h.provider, TurnOptions::default());
    assert!(refused.is_err());
    assert!(!h.runtime.is_running(&session.id));
}

#[tokio::test]
async fn cancelling_stops_a_running_command_and_answers_its_call() {
    if !matches!(detect_shell(), hermes_tools::ShellKind::GitBash(_)) {
        return;
    }
    let h = harness(vec![tool_reply(&[(
        "call_1",
        "terminal",
        json!({"command": "sleep 60"}),
    )])])
    .await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    let mut turn = h
        .runtime
        .start_turn(
            &session.id,
            &h.provider,
            TurnOptions::default(),
            "wait a minute",
        )
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    h.runtime.cancel(&session.id);
    let mut status = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(20), turn.events.recv())
        .await
        .unwrap()
    {
        if let RuntimeEvent::TurnFinished(finished) = event {
            status = Some(finished);
            break;
        }
    }
    assert_eq!(status, Some(TurnStatus::Cancelled));
    let messages = h.runtime.messages(&session.id).unwrap();
    let last = messages.last().unwrap();
    assert!(last.is_tool_response());
    assert!(format!("{last:?}").contains("interrupted"));
    h.runtime.close(&session.id).await;
}

#[tokio::test]
async fn sessions_reopen_from_disk_with_their_todos() {
    let h = harness(vec![
        tool_reply(&[(
            "call_1",
            "todo_list",
            json!({"todos": [
                {"id": "1", "content": "write plan", "status": "in_progress"},
            ]}),
        )]),
        text_reply("Planned."),
    ])
    .await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    run_turn(&h, &session.id, "plan it").await;
    let before = h.runtime.messages(&session.id).unwrap();

    let reopened = runtime_in(h.data.path());
    let after = reopened.messages(&session.id).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap(),
        "stored conversation round-trips exactly"
    );
    let record = reopened.store().session(&session.id).unwrap().unwrap();
    assert_eq!(
        record.system_prompt,
        h.runtime
            .store()
            .session(&session.id)
            .unwrap()
            .unwrap()
            .system_prompt
    );
}

#[tokio::test]
async fn switching_models_rebuilds_the_frozen_prompt() {
    let h = harness(vec![]).await;
    let session = h
        .runtime
        .create_session(&h.provider, "anthropic/claude-x", None, h.work.path())
        .unwrap();
    assert!(!session.system_prompt.contains("# Tool-use enforcement"));

    h.runtime
        .set_session_model(&session.id, "openai/gpt-5", None)
        .unwrap();
    let record = h.runtime.store().session(&session.id).unwrap().unwrap();
    assert_eq!(record.model, "openai/gpt-5");
    assert!(record.system_prompt.contains("Model: openai/gpt-5"));
    assert!(record.system_prompt.contains("# Tool-use enforcement"));
}

#[tokio::test]
async fn effort_is_sent_only_when_set() {
    let h = harness(vec![text_reply("a"), text_reply("b")]).await;
    let session = h
        .runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap();
    run_turn(&h, &session.id, "first").await;
    let options = TurnOptions {
        context_length: None,
        effort: Some("high".into()),
    };
    let mut turn = h
        .runtime
        .start_turn(&session.id, &h.provider, options, "second")
        .unwrap();
    while let Some(event) = turn.events.recv().await {
        if matches!(event, RuntimeEvent::TurnFinished(_)) {
            break;
        }
    }
    let requests = h.requests.lock().clone();
    assert!(requests[0].get("reasoning_effort").is_none());
    assert_eq!(requests[1]["reasoning_effort"], "high");
}

#[test]
fn openrouter_gets_its_unified_reasoning_field() {
    let params = hermes_runtime::effort_request_params("https://openrouter.ai/api/v1", "low");
    assert_eq!(params["reasoning"], json!({"effort": "low"}));
    let params = hermes_runtime::effort_request_params("http://localhost:1234/v1", "low");
    assert_eq!(params["reasoning_effort"], "low");
}
