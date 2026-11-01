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

/// One streamed response. Like a real provider, every response gets its own id.
fn sse(chunks: Vec<Value>) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!(
        "resp_{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let mut body: String = chunks
        .into_iter()
        .map(|mut chunk| {
            chunk["id"] = json!(id);
            format!("data: {chunk}\n\n")
        })
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
            max_iterations: 20,
            memory: Default::default(),
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

// ---- Hermes turn-loop behaviors ---------------------------------------------------------------

fn raw_tool_reply(name: &str, arguments: &str, finish: &str) -> String {
    sse(vec![
        delta(
            json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_raw", "type": "function",
                   "function": {"name": name, "arguments": arguments}}]}),
            Some(finish),
        ),
        usage(),
    ])
}

fn finished_text_reply(text: &str, finish: &str) -> String {
    sse(vec![
        delta(json!({"role": "assistant", "content": text}), Some(finish)),
        usage(),
    ])
}

fn session_in(h: &Harness) -> String {
    h.runtime
        .create_session(&h.provider, "test-model", None, h.work.path())
        .unwrap()
        .id
}

fn last_text(h: &Harness, session: &str) -> String {
    h.runtime
        .messages(session)
        .unwrap()
        .last()
        .unwrap()
        .as_concat_text()
}

fn sent_messages(request: &Value) -> Vec<Value> {
    request["messages"].as_array().unwrap().clone()
}

#[tokio::test]
async fn misspelled_tool_names_are_repaired() {
    let h = harness(vec![
        tool_reply(&[(
            "call_1",
            "Write-File",
            json!({"path": "a.txt", "content": "x"}),
        )]),
        text_reply("ok"),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "write it").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(h.work.path().join("a.txt")).unwrap(),
        "x"
    );
    let second = sent_messages(&h.requests.lock()[1]);
    let call = second.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(call["tool_calls"][0]["function"]["name"], "write_file");
}

#[tokio::test]
async fn three_batches_of_invented_tools_end_the_turn() {
    let invented = || tool_reply(&[("call_x", "browser_navigate", json!({"url": "x"}))]);
    let h = harness(vec![
        invented(),
        invented(),
        invented(),
        text_reply("never sent"),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "browse").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    assert_eq!(h.requests.lock().len(), 3);
    assert_eq!(
        last_text(&h, &session),
        "Model generated invalid tool call: browser_navigate"
    );
    // The first two strikes were answered with the tool list.
    let third = sent_messages(&h.requests.lock()[2]);
    let answer = third.iter().rev().find(|m| m["role"] == "tool").unwrap();
    assert!(
        answer["content"]
            .as_str()
            .unwrap()
            .contains("does not exist. Available tools:")
    );
}

#[tokio::test]
async fn malformed_arguments_are_retried_without_a_trace() {
    let h = harness(vec![
        raw_tool_reply("read_file", r#"{"path" "a.txt"}"#, "tool_calls"),
        raw_tool_reply("read_file", r#"{"path" "a.txt"}"#, "tool_calls"),
        text_reply("fine"),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "read").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 3);
    // The withdrawn responses never reach the model.
    assert!(
        sent_messages(&requests[2])
            .iter()
            .all(|m| m["role"] != "assistant" && m["role"] != "tool")
    );
    assert_eq!(last_text(&h, &session), "fine");
}

#[tokio::test]
async fn the_third_malformed_response_gets_an_error_result() {
    let bad = || raw_tool_reply("read_file", r#"{"path" "a.txt"}"#, "tool_calls");
    let h = harness(vec![bad(), bad(), bad(), text_reply("recovered")]).await;
    let session = session_in(&h);
    run_turn(&h, &session, "read").await;
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 4);
    let answer = sent_messages(&requests[3])
        .into_iter()
        .rfind(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        answer["content"]
            .as_str()
            .unwrap()
            .starts_with("Error: Invalid JSON arguments.")
    );
}

#[tokio::test]
async fn truncated_text_is_continued() {
    let h = harness(vec![
        finished_text_reply("The first half of the answer", "length"),
        text_reply(" and the second half."),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "explain").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    let last_sent = sent_messages(&requests[1]).pop().unwrap();
    assert_eq!(last_sent["role"], "user");
    assert!(
        last_sent["content"]
            .as_str()
            .unwrap()
            .contains("truncated by the output length limit")
    );
    // The user sees both fragments and not the nudge.
    let visible: Vec<String> = h
        .runtime
        .messages(&session)
        .unwrap()
        .iter()
        .filter(|m| m.is_user_visible())
        .map(|m| m.as_concat_text())
        .collect();
    assert_eq!(
        visible,
        vec![
            "explain",
            "The first half of the answer",
            " and the second half."
        ]
    );
}

#[tokio::test]
async fn a_truncated_repetition_loop_is_not_continued() {
    let looped = "I will now repeat this sentence because I am stuck in a loop.\n".repeat(30);
    let h = harness(vec![
        finished_text_reply(&looped, "length"),
        text_reply("never"),
    ])
    .await;
    let session = session_in(&h);
    run_turn(&h, &session, "go").await;
    assert_eq!(h.requests.lock().len(), 1);
    assert!(last_text(&h, &session).contains("Repetition Detected"));
    let stored = h.runtime.messages(&session).unwrap();
    assert!(
        stored
            .iter()
            .filter(|m| m.is_user_visible())
            .all(|m| !m.as_concat_text().contains("stuck in a loop"))
    );
}

#[tokio::test]
async fn a_tool_call_cut_off_by_the_output_limit_is_retried_with_a_bigger_cap() {
    let h = harness(vec![
        raw_tool_reply(
            "write_file",
            r#"{"path": "big.txt", "content": "abc"#,
            "length",
        ),
        tool_reply(&[(
            "call_2",
            "write_file",
            json!({"path": "big.txt", "content": "abc"}),
        )]),
        text_reply("written"),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "write a big file").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 3);
    let cap = |r: &Value| {
        r.get("max_tokens")
            .or_else(|| r.get("max_completion_tokens"))
            .cloned()
    };
    assert_eq!(cap(&requests[0]), None);
    assert_eq!(cap(&requests[1]), Some(json!(8192)));
    assert_eq!(cap(&requests[2]), None);
    assert_eq!(
        std::fs::read_to_string(h.work.path().join("big.txt")).unwrap(),
        "abc"
    );
}

#[tokio::test]
async fn an_empty_reply_after_tools_gets_one_nudge() {
    let h = harness(vec![
        tool_reply(&[(
            "call_1",
            "write_file",
            json!({"path": "n.txt", "content": "1"}),
        )]),
        finished_text_reply("", "stop"),
        text_reply("All done."),
    ])
    .await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "do it").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 3);
    let sent = sent_messages(&requests[2]);
    assert_eq!(sent[sent.len() - 2]["content"], "(empty)");
    assert!(
        sent[sent.len() - 1]["content"]
            .as_str()
            .unwrap()
            .contains("returned an empty response")
    );
    assert_eq!(last_text(&h, &session), "All done.");
}

#[tokio::test]
async fn a_thinking_only_reply_is_asked_for_its_answer() {
    let h = harness(vec![
        sse(vec![
            delta(
                json!({"role": "assistant", "reasoning_content": "Let me think about it."}),
                Some("stop"),
            ),
            usage(),
        ]),
        text_reply("Here is the answer."),
    ])
    .await;
    let session = session_in(&h);
    run_turn(&h, &session, "question").await;
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    let nudge = sent_messages(&requests[1]).pop().unwrap();
    assert!(
        nudge["content"]
            .as_str()
            .unwrap()
            .contains("contained only internal reasoning")
    );
    assert_eq!(last_text(&h, &session), "Here is the answer.");
}

#[tokio::test]
async fn an_exhausted_budget_ends_with_a_toolless_summary() {
    // The harness allows 20 model calls per turn.
    let mut replies: Vec<String> = (0..20)
        .map(|i| {
            tool_reply(&[(
                &format!("call_{i}"),
                "write_file",
                json!({"path": format!("f{i}.txt"), "content": "x"}),
            )])
        })
        .collect();
    replies.push(text_reply("Summary: wrote twenty files."));
    let h = harness(replies).await;
    let session = session_in(&h);
    let events = run_turn(&h, &session, "write many files").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 21);
    assert!(
        requests[19]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty())
    );
    assert!(
        requests[20]
            .get("tools")
            .is_none_or(|t| t.as_array().is_none_or(Vec::is_empty))
    );
    let ask = sent_messages(&requests[20]).pop().unwrap();
    assert_eq!(ask["content"], hermes_runtime::SUMMARY_REQUEST);
    assert_eq!(last_text(&h, &session), "Summary: wrote twenty files.");
}

#[tokio::test]
async fn repeated_identical_calls_get_a_loop_notice() {
    let search = |id: &str| {
        tool_reply(&[(
            id,
            "search_files",
            json!({"pattern": "nothing-matches-this", "target": "content"}),
        )])
    };
    let h = harness(vec![
        search("c1"),
        search("c2"),
        search("c3"),
        text_reply("giving up"),
    ])
    .await;
    let session = session_in(&h);
    run_turn(&h, &session, "search").await;
    let requests = h.requests.lock().clone();
    let results: Vec<String> = sent_messages(&requests[3])
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert!(results[1].contains("idempotent_no_progress_warning"));
    assert!(results[2].contains("3rd consecutive identical call to search_files"));
}

async fn collect(mut turn: hermes_runtime::Turn) -> Vec<RuntimeEvent> {
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

#[tokio::test]
async fn a_steer_is_delivered_after_the_running_tool_batch() {
    let h = harness(vec![
        tool_reply(&[("call_1", "terminal", json!({"command": "sleep 2"}))]),
        text_reply("Switching to python."),
    ])
    .await;
    let session = session_in(&h);
    let turn = h
        .runtime
        .start_turn(&session, &h.provider, TurnOptions::default(), "count files")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(h.runtime.steer(&session, "use python instead"));
    let events = collect(turn).await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    let sent = sent_messages(&requests[1]);
    assert_eq!(sent[sent.len() - 2]["role"], "tool");
    let steer = sent.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(steer.starts_with("[OUT-OF-BAND USER MESSAGE"));
    assert!(steer.contains("\nuse python instead\n[/OUT-OF-BAND USER MESSAGE]"));
    // The chat shows the user's own words.
    let shown: Vec<String> = h
        .runtime
        .messages(&session)
        .unwrap()
        .iter()
        .filter(|m| m.is_user_visible() && m.role == rmcp::model::Role::User)
        .map(|m| m.as_concat_text())
        .filter(|t| !t.is_empty())
        .collect();
    assert_eq!(shown, vec!["count files", "use python instead"]);
}

#[tokio::test]
async fn a_steer_without_a_tool_batch_comes_back_for_the_next_message() {
    let h = harness(vec![text_reply("Hello!")]).await;
    let session = session_in(&h);
    let turn = h
        .runtime
        .start_turn(&session, &h.provider, TurnOptions::default(), "hi")
        .unwrap();
    assert!(h.runtime.steer(&session, "and in French"));
    let events = collect(turn).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::SteerUndelivered(t) if t == "and in French"))
    );
    assert!(!h.runtime.steer(&session, "too late"));
}

#[tokio::test]
async fn memory_saved_in_one_session_reaches_the_next_sessions_prompt_only() {
    let h = harness(vec![
        tool_reply(&[(
            "call_m",
            "memory",
            json!({"target": "user", "operations": [{"action": "add", "content": "User's name is Alex"}]}),
        )]),
        text_reply("Noted."),
        text_reply("Hi Alex."),
    ])
    .await;
    let first = session_in(&h);
    let events = run_turn(&h, &first, "My name is Alex, remember it").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let saved = sent_messages(&h.requests.lock()[1])
        .into_iter()
        .rfind(|m| m["role"] == "tool")
        .unwrap();
    assert!(saved["content"].as_str().unwrap().contains("Write saved"));
    assert_eq!(
        std::fs::read_to_string(h.data.path().join("memories").join("USER.md")).unwrap(),
        "User's name is Alex"
    );
    // The running session's prompt stays frozen; a new session sees the memory.
    let prompt = |id: &str| {
        h.runtime
            .store()
            .session(id)
            .unwrap()
            .unwrap()
            .system_prompt
    };
    assert!(!prompt(&first).contains("User's name is Alex"));
    let second = session_in(&h);
    assert!(prompt(&second).contains("USER PROFILE (who the user is) ["));
    assert!(prompt(&second).contains("User's name is Alex"));
    assert!(prompt(&second).contains("You have persistent memory"));
    run_turn(&h, &second, "hi").await;
    let system = h.requests.lock()[2]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(system.contains("User's name is Alex"));
}

#[tokio::test]
async fn session_search_recalls_an_earlier_session() {
    let h = harness(vec![
        text_reply("Stored in the team vault under deploy/staging."),
        tool_reply(&[("call_s", "session_search", json!({"query": "vault"}))]),
        text_reply("It's in the team vault."),
    ])
    .await;
    let earlier = session_in(&h);
    run_turn(
        &h,
        &earlier,
        "Where do the staging credentials live? The vault?",
    )
    .await;
    let now = session_in(&h);
    run_turn(&h, &now, "Where did we say the credentials are?").await;
    let result = sent_messages(&h.requests.lock()[2])
        .into_iter()
        .rfind(|m| m["role"] == "tool")
        .unwrap();
    let result: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["mode"], "discover");
    assert_eq!(result["count"], 1);
    assert_eq!(result["results"][0]["session_id"], earlier.as_str());
    let messages = result["results"][0]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["content"].as_str().unwrap().contains("team vault"))
    );
}

#[tokio::test]
async fn rewinding_drops_the_message_and_what_followed() {
    let h = harness(vec![
        tool_reply(&[(
            "call_t",
            "todo_list",
            json!({"todos": [{"id": "1", "content": "later step", "status": "pending"}]}),
        )]),
        text_reply("Planned."),
        text_reply("Second answer."),
        text_reply("Answer again."),
    ])
    .await;
    let session = session_in(&h);
    run_turn(&h, &session, "first").await;
    run_turn(&h, &session, "second").await;
    // Rewinding to the first message drops both turns.
    let kept = h.runtime.rewind(&session, 0).unwrap();
    assert!(kept.is_empty());
    assert!(h.runtime.messages(&session).unwrap().is_empty());
    // The store agrees after a reopen.
    assert!(
        h.runtime
            .store()
            .load_messages(&session)
            .unwrap()
            .is_empty()
    );
    run_turn(&h, &session, "first").await;
    let texts: Vec<String> = h
        .runtime
        .messages(&session)
        .unwrap()
        .iter()
        .map(|m| m.as_concat_text())
        .collect();
    assert_eq!(texts, vec!["first", "Answer again."]);
}

#[tokio::test]
async fn rewinding_is_refused_while_a_turn_runs() {
    let h = harness(vec![
        tool_reply(&[("call_1", "terminal", json!({"command": "sleep 2"}))]),
        text_reply("done"),
    ])
    .await;
    let session = session_in(&h);
    let turn = h
        .runtime
        .start_turn(&session, &h.provider, TurnOptions::default(), "wait")
        .unwrap();
    assert!(h.runtime.rewind(&session, 0).is_err());
    collect(turn).await;
    assert!(h.runtime.rewind(&session, 0).is_ok());
}
