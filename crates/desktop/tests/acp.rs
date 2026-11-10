//! `hermes-desktop.exe --acp` as an editor sees it: the real exe on pipes, a scripted
//! OpenAI-compatible model behind it, and the JSON-RPC lines both ways. The data folder and the
//! working folder are temporary; nothing here touches the user's own.
//!
//! The harness is `Agent`; the tests read like a conversation with an editor.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const WAIT: Duration = Duration::from_secs(40);

// ---- the scripted model ----

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
    json!({"id": "c", "object": "chat.completion.chunk", "model": "mock",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn usage() -> Value {
    json!({"id": "c", "object": "chat.completion.chunk", "model": "mock", "choices": [],
           "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}})
}

fn text_reply(text: &str) -> String {
    sse(vec![
        delta(json!({"role": "assistant", "content": text}), Some("stop")),
        usage(),
    ])
}

/// A reply streamed in two parts, with reasoning first.
fn thinking_reply(thought: &str, text: &str) -> String {
    sse(vec![
        delta(
            json!({"role": "assistant", "reasoning_content": thought}),
            None,
        ),
        delta(json!({"content": text}), Some("stop")),
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

/// Answers each streamed chat request with the next scripted body (after an optional delay) and
/// records the requests; a non-streamed one (a session title) gets a fixed answer and doesn't
/// use up a scripted reply.
struct Script {
    replies: Arc<std::sync::Mutex<VecDeque<String>>>,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    delay: Duration,
}

impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        // A title request offers no tools; a turn always does.
        let offers_tools = body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty());
        if !offers_tools {
            if body["stream"] == true {
                return ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(text_reply("A test title"));
            }
            return ResponseTemplate::new(200).set_body_json(json!({
                "id": "t", "object": "chat.completion", "model": "mock",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "A test title"}}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
            }));
        }
        self.requests.lock().unwrap().push(body);
        match self.replies.lock().unwrap().pop_front() {
            Some(body) => ResponseTemplate::new(200)
                .set_delay(self.delay)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
            None => ResponseTemplate::new(500)
                .set_body_json(json!({"error": {"message": "script exhausted"}})),
        }
    }
}

struct Model {
    server: MockServer,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    script: Arc<std::sync::Mutex<VecDeque<String>>>,
}

impl Model {
    async fn start(replies: Vec<String>, delay: Duration) -> Self {
        let server = MockServer::start().await;
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let script = Arc::new(std::sync::Mutex::new(VecDeque::from(replies)));
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Script {
                replies: script.clone(),
                requests: requests.clone(),
                delay,
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"object": "list", "data": [{"id": "mock"}, {"id": "other"}]}),
            ))
            .mount(&server)
            .await;
        Self {
            server,
            requests,
            script,
        }
    }

    /// The bodies of the streamed chat requests so far.
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

// ---- the editor ----

struct Agent {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::UnboundedReceiver<Value>,
    seen: Vec<Value>,
    answered: HashSet<String>,
    next_id: u64,
    model: Model,
    _home: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Agent {
    async fn start(replies: Vec<String>) -> Self {
        Self::start_with(replies, Duration::ZERO).await
    }

    async fn start_with(replies: Vec<String>, delay: Duration) -> Self {
        let model = Model::start(replies, delay).await;
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = json!({
            "providers": [{"id": "mock", "name": "Mock", "base_url": format!("{}/v1", model.server.uri())}],
            "provider": "mock", "model": "mock",
            "background_review": false, "checkpoints": false,
        });
        std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
        Self::spawn(model, home, work)
    }

    /// A data folder with no provider in it.
    async fn start_without_a_model() -> Self {
        let model = Model::start(Vec::new(), Duration::ZERO).await;
        Self::spawn(
            model,
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        )
    }

    fn spawn(model: Model, home: tempfile::TempDir, work: tempfile::TempDir) -> Self {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hermes-desktop"))
            .arg("--acp")
            .env("HERMES_RS_HOME", home.path())
            .env("RUST_LOG", "info,hermes_desktop::acp=debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the exe starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                // A line that isn't JSON is a failure of the stdout rule (see `clean_stdout`).
                let value = serde_json::from_str::<Value>(&line)
                    .unwrap_or_else(|_| json!({"__not_json": line}));
                let _ = tx.send(value);
            }
        });
        // The agent's stderr is drained (a full pipe would stall it) and shown on failure.
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("[agent] {line}");
            }
        });
        Self {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            answered: HashSet::new(),
            next_id: 0,
            model,
            _home: home,
            work,
        }
    }

    async fn pump_one(&mut self) {
        match tokio::time::timeout(WAIT, self.lines.recv()).await {
            Ok(Some(message)) => self.seen.push(message),
            Ok(None) => panic!("the agent closed stdout; seen so far: {:#?}", self.seen),
            Err(_) => panic!("timed out waiting; seen so far: {:#?}", self.seen),
        }
    }

    async fn wait_for<T>(
        &mut self,
        mut find: impl FnMut(&[Value], &HashSet<String>) -> Option<T>,
    ) -> T {
        loop {
            if let Some(found) = find(&self.seen, &self.answered) {
                return found;
            }
            self.pump_one().await;
        }
    }

    async fn write(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    /// Sends a request without waiting; returns its id.
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    /// The response to request `id`: the whole message, `result` or `error`.
    async fn response(&mut self, id: u64) -> Value {
        self.wait_for(|seen, _| {
            seen.iter()
                .find(|m| m["id"] == json!(id) && m.get("method").is_none())
                .cloned()
        })
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.response(id).await
    }

    /// A request the agent made of the editor, and not yet answered: `(id, params)`.
    async fn agent_request(&mut self, method: &str) -> (Value, Value) {
        let method = method.to_string();
        self.wait_for(move |seen, answered| {
            seen.iter()
                .find(|m| {
                    m["method"] == method.as_str()
                        && m.get("id").is_some()
                        && !answered.contains(&m["id"].to_string())
                })
                .map(|m| (m["id"].clone(), m["params"].clone()))
        })
        .await
    }

    async fn reply(&mut self, id: Value, result: Value) {
        self.answered.insert(id.to_string());
        self.write(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    async fn reply_error(&mut self, id: Value, code: i64, message: &str) {
        self.answered.insert(id.to_string());
        self.write(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        )
        .await;
    }

    /// `initialize`, with the capabilities of the editor under test.
    async fn initialize(&mut self, capabilities: Value) -> Value {
        self.request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": capabilities,
                   "clientInfo": {"name": "test-editor", "version": "0"}}),
        )
        .await
    }

    /// Starts the agent, initializes (no forms) and makes a session in the working folder.
    async fn with_session(replies: Vec<String>) -> (Self, String) {
        let mut agent = Self::start(replies).await;
        agent.initialize(json!({})).await;
        let session = agent.new_session().await;
        (agent, session)
    }

    async fn new_session(&mut self) -> String {
        let cwd = self.work.path().display().to_string();
        let answer = self
            .request("session/new", json!({"cwd": cwd, "mcpServers": []}))
            .await;
        answer["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("session/new: {answer}"))
            .to_string()
    }

    async fn prompt(&mut self, session: &str, text: &str) -> Value {
        self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
        )
        .await
    }

    /// The `update` objects the agent sent for a session, in order.
    fn updates(&self, session: &str) -> Vec<Value> {
        self.seen
            .iter()
            .filter(|m| m["method"] == "session/update" && m["params"]["sessionId"] == session)
            .map(|m| m["params"]["update"].clone())
            .collect()
    }

    fn updates_of(&self, session: &str, kind: &str) -> Vec<Value> {
        self.updates(session)
            .into_iter()
            .filter(|u| u["sessionUpdate"] == kind)
            .collect()
    }

    fn reply_text(&self, session: &str) -> String {
        self.updates_of(session, "agent_message_chunk")
            .iter()
            .filter_map(|u| u["content"]["text"].as_str())
            .collect()
    }

    /// Everything on stdout so far was a JSON-RPC message.
    fn clean_stdout(&self) {
        for message in &self.seen {
            assert!(
                message.get("__not_json").is_none(),
                "a line on stdout that isn't JSON: {message}"
            );
            assert_eq!(
                message["jsonrpc"], "2.0",
                "not a JSON-RPC message: {message}"
            );
        }
    }
}

fn work_file(agent: &Agent, name: &str) -> std::path::PathBuf {
    agent.work.path().join(name)
}

fn windows_path(path: &Path) -> String {
    path.display().to_string()
}

// ---- Phase 0: the protocol's front door ----

#[tokio::test]
async fn initialize_answers_version_1_and_names_the_agent() {
    let mut agent = Agent::start(Vec::new()).await;
    let answer = agent.initialize(json!({})).await;
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentInfo"]["name"], "hermes-rs");
    assert_eq!(result["authMethods"], json!([]));
    assert!(result["agentCapabilities"]["promptCapabilities"].is_object());
    agent.clean_stdout();
}

#[tokio::test]
async fn an_unknown_method_is_method_not_found() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.initialize(json!({})).await;
    let answer = agent.request("session/teleport", json!({})).await;
    assert_eq!(answer["error"]["code"], -32601);
}

#[tokio::test]
async fn a_line_that_is_not_json_is_a_parse_error_and_the_agent_keeps_going() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.write_raw("this is not json").await;
    agent
        .wait_for(|seen, _| seen.iter().find(|m| m["error"]["code"] == -32700).cloned())
        .await;
    let answer = agent.initialize(json!({})).await;
    assert_eq!(answer["result"]["protocolVersion"], 1);
}

#[tokio::test]
async fn closing_stdin_ends_the_process() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.initialize(json!({})).await;
    agent.stdin = None;
    let status = tokio::time::timeout(Duration::from_secs(15), agent.child.wait())
        .await
        .expect("the agent exits when its input closes")
        .unwrap();
    assert!(status.success(), "{status:?}");
}

// ---- Phase 1: chat ----

#[tokio::test]
async fn a_prompt_streams_the_reply_and_ends_the_turn() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Hello there")]).await;
    let answer = agent.prompt(&session, "hi").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert_eq!(agent.reply_text(&session), "Hello there");
    // What the model was sent: the user's words.
    assert!(agent.model.requests()[0].to_string().contains("\"hi\""));
    agent.clean_stdout();
}

#[tokio::test]
async fn thinking_streams_as_thought_chunks() {
    let (mut agent, session) = Agent::with_session(vec![thinking_reply("pondering", "Done")]).await;
    let answer = agent.prompt(&session, "think").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let thoughts = agent.updates_of(&session, "agent_thought_chunk");
    assert_eq!(thoughts[0]["content"]["text"], "pondering");
    assert_eq!(agent.reply_text(&session), "Done");
}

#[tokio::test]
async fn every_chunk_of_a_reply_shares_one_message_id() {
    let reply = sse(vec![
        delta(json!({"role": "assistant", "content": "one "}), None),
        delta(json!({"content": "two"}), Some("stop")),
        usage(),
    ]);
    let (mut agent, session) = Agent::with_session(vec![reply]).await;
    agent.prompt(&session, "go").await;
    let ids: HashSet<String> = agent
        .updates_of(&session, "agent_message_chunk")
        .iter()
        .map(|u| u["messageId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
}

#[tokio::test]
async fn cancel_ends_the_turn_with_cancelled() {
    let mut agent =
        Agent::start_with(vec![text_reply("slow answer")], Duration::from_secs(20)).await;
    agent.initialize(json!({})).await;
    let session = agent.new_session().await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    agent
        .notify("session/cancel", json!({"sessionId": session}))
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
}

#[tokio::test]
async fn a_provider_error_answers_an_error() {
    // No scripted reply: the mock answers 500.
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let answer = agent.prompt(&session, "hi").await;
    assert!(answer["error"]["message"].is_string(), "{answer}");
}

#[tokio::test]
async fn no_provider_gives_a_readable_error() {
    let mut agent = Agent::start_without_a_model().await;
    agent.initialize(json!({})).await;
    let cwd = windows_path(agent.work.path());
    let answer = agent
        .request("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await;
    let message = answer["error"]["message"].as_str().unwrap();
    assert!(message.contains("Settings"), "{message}");
}

#[tokio::test]
async fn two_sessions_stream_independently() {
    let (mut agent, first) = Agent::with_session(vec![
        text_reply("first answer"),
        text_reply("second answer"),
    ])
    .await;
    let second = agent.new_session().await;
    assert_ne!(first, second);
    agent.prompt(&first, "a").await;
    agent.prompt(&second, "b").await;
    assert_eq!(agent.reply_text(&first), "first answer");
    assert_eq!(agent.reply_text(&second), "second answer");
}

#[tokio::test]
async fn a_prompt_for_an_unknown_session_is_invalid_params() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.initialize(json!({})).await;
    let answer = agent.prompt("no-such-session", "hi").await;
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
}

// ---- Phase 2: tools, approvals, questions ----

#[tokio::test]
async fn a_tool_call_shows_its_start_and_its_result() {
    let file = "listing.txt";
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[(
            "call_1",
            "terminal",
            json!({"command": "echo hello-from-tool"}),
        )]),
        text_reply("Done."),
    ])
    .await;
    let _ = file;
    let answer = agent.prompt(&session, "run it").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let started = agent.updates_of(&session, "tool_call");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["toolCallId"], "call_1");
    assert_eq!(started[0]["kind"], "execute");
    assert_eq!(started[0]["status"], "in_progress");
    assert_eq!(started[0]["title"], "echo hello-from-tool");
    let finished = agent.updates_of(&session, "tool_call_update");
    let last = finished.last().unwrap();
    assert_eq!(last["toolCallId"], "call_1");
    assert_eq!(last["status"], "completed");
    assert!(
        last["content"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("hello-from-tool")
    );
    assert_eq!(agent.reply_text(&session), "Done.");
}

#[tokio::test]
async fn write_file_sends_a_diff_and_creates_the_file() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[(
            "call_1",
            "write_file",
            json!({"path": "hello.txt", "content": "hi there"}),
        )]),
        text_reply("Written."),
    ])
    .await;
    let answer = agent.prompt(&session, "write it").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert_eq!(
        std::fs::read_to_string(work_file(&agent, "hello.txt")).unwrap(),
        "hi there"
    );
    let started = &agent.updates_of(&session, "tool_call")[0];
    assert_eq!(started["kind"], "edit");
    assert_eq!(
        started["locations"][0]["path"],
        windows_path(&work_file(&agent, "hello.txt")).replace('/', "\\")
    );
    let done = agent.updates_of(&session, "tool_call_update");
    let diff = &done.last().unwrap()["content"][0];
    assert_eq!(diff["type"], "diff");
    assert_eq!(diff["newText"], "hi there");
    assert_eq!(diff["oldText"], Value::Null);
}

#[tokio::test]
async fn a_patch_sends_the_changed_hunk() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        tool_reply(&[(
            "c2",
            "patch",
            json!({"path": "a.txt", "old_string": "beta", "new_string": "BETA"}),
        )]),
        text_reply("Patched."),
    ])
    .await;
    std::fs::write(work_file(&agent, "a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let answer = agent.prompt(&session, "patch it").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert_eq!(
        std::fs::read_to_string(work_file(&agent, "a.txt")).unwrap(),
        "alpha\nBETA\ngamma\n"
    );
    let done = agent.updates_of(&session, "tool_call_update");
    let patch = done.iter().find(|u| u["toolCallId"] == "c2").unwrap();
    let diff = &patch["content"][0];
    assert_eq!(diff["type"], "diff");
    assert!(diff["oldText"].as_str().unwrap().contains("beta"));
    assert!(diff["newText"].as_str().unwrap().contains("BETA"));
}

#[tokio::test]
async fn a_failed_tool_is_marked_failed() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[("c1", "read_file", json!({"path": "missing.txt"}))]),
        text_reply("It's not there."),
    ])
    .await;
    agent.prompt(&session, "read it").await;
    let done = agent.updates_of(&session, "tool_call_update");
    assert_eq!(done[0]["status"], "failed", "{done:?}");
}

#[tokio::test]
async fn the_todo_list_is_sent_as_a_plan() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[(
            "c1",
            "todo_list",
            json!({"todos": [
                {"id": "1", "content": "write it", "status": "in_progress"},
                {"id": "2", "content": "test it", "status": "pending"},
            ]}),
        )]),
        text_reply("Planned."),
    ])
    .await;
    agent.prompt(&session, "plan").await;
    let plans = agent.updates_of(&session, "plan");
    assert_eq!(plans.len(), 1, "{:?}", agent.updates(&session));
    assert_eq!(plans[0]["entries"][0]["status"], "in_progress");
    assert_eq!(plans[0]["entries"][1]["content"], "test it");
}

/// A command the approval policy flags (a recursive delete), against a folder the test made.
/// The terminal tool runs PowerShell unless desktop-config.json says bash.
fn flagged_command(target: &Path) -> String {
    format!("Remove-Item -Recurse -Force '{}'", target.display())
}

#[tokio::test]
async fn a_flagged_command_asks_permission_and_runs_when_allowed() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let victim = work_file(&agent, "victim");
    std::fs::create_dir(&victim).unwrap();
    agent.model.set_replies(vec![
        tool_reply(&[(
            "c1",
            "terminal",
            json!({"command": flagged_command(&victim)}),
        )]),
        text_reply("Deleted."),
    ]);
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "clean up"}]}),
        )
        .await;
    let (id, params) = agent.agent_request("session/request_permission").await;
    let ids: Vec<&str> = params["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["optionId"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"allow_once") && ids.contains(&"deny"),
        "{ids:?}"
    );
    assert_eq!(params["sessionId"], session);
    assert_eq!(params["toolCall"]["kind"], "execute");
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}),
        )
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(!victim.exists(), "the command should have run");
    // The permission card was closed.
    let closing = agent
        .updates_of(&session, "tool_call_update")
        .into_iter()
        .find(|u| {
            u["toolCallId"]
                .as_str()
                .is_some_and(|id| id.starts_with("perm-"))
        })
        .unwrap();
    assert_eq!(closing["status"], "completed");
}

#[tokio::test]
async fn a_denied_command_does_not_run() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let victim = work_file(&agent, "victim");
    std::fs::create_dir(&victim).unwrap();
    agent.model.set_replies(vec![
        tool_reply(&[(
            "c1",
            "terminal",
            json!({"command": flagged_command(&victim)}),
        )]),
        text_reply("Understood."),
    ]);
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "clean up"}]}),
        )
        .await;
    let (id, _) = agent.agent_request("session/request_permission").await;
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "deny"}}),
        )
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(victim.exists(), "a denied command must not run");
}

#[tokio::test]
async fn a_cancelled_permission_card_denies() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let victim = work_file(&agent, "victim");
    std::fs::create_dir(&victim).unwrap();
    agent.model.set_replies(vec![
        tool_reply(&[(
            "c1",
            "terminal",
            json!({"command": flagged_command(&victim)}),
        )]),
        text_reply("Okay."),
    ]);
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "clean up"}]}),
        )
        .await;
    let (id, _) = agent.agent_request("session/request_permission").await;
    // The editor's Stop: it cancels the turn, and answers the open card as cancelled.
    agent
        .notify("session/cancel", json!({"sessionId": session}))
        .await;
    agent
        .reply(id, json!({"outcome": {"outcome": "cancelled"}}))
        .await;
    let answer = agent.response(prompt).await;
    assert!(
        answer["result"]["stopReason"] == "cancelled"
            || answer["result"]["stopReason"] == "end_turn",
        "{answer}"
    );
    assert!(victim.exists(), "a cancelled card must not run the command");
}

#[tokio::test]
async fn a_question_is_asked_as_a_form_when_the_editor_has_forms() {
    let mut agent = Agent::start(vec![
        tool_reply(&[(
            "c1",
            "clarify",
            json!({"questions": [{"question": "Which language?", "choices": ["Rust", "Python"]}]}),
        )]),
        text_reply("Going with it."),
    ])
    .await;
    agent.initialize(json!({"elicitation": {"form": {}}})).await;
    let session = agent.new_session().await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "ask me"}]}),
        )
        .await;
    let (id, params) = agent.agent_request("elicitation/create").await;
    assert_eq!(params["message"], "Which language?");
    assert_eq!(
        params["requestedSchema"]["properties"]["answer"]["enum"],
        json!(["Rust", "Python"])
    );
    agent
        .reply(
            id,
            json!({"action": "accept", "content": {"answer": "Rust"}}),
        )
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    // The model's next request carries the answer.
    let requests = agent.model.requests();
    assert!(requests[1].to_string().contains("Rust"), "{}", requests[1]);
}

#[tokio::test]
async fn a_question_is_a_card_when_the_editor_has_no_forms() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[(
            "c1",
            "clarify",
            json!({"questions": [{"question": "Which language?", "choices": ["Rust", "Python"]}]}),
        )]),
        text_reply("Going with it."),
    ])
    .await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "ask me"}]}),
        )
        .await;
    let (id, params) = agent.agent_request("session/request_permission").await;
    let names: Vec<&str> = params["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Rust", "Python", "Skip"]);
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "choice-1"}}),
        )
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(agent.model.requests()[1].to_string().contains("Python"));
}

#[tokio::test]
async fn the_stdout_rule_holds_through_a_whole_tool_turn() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[("c1", "terminal", json!({"command": "echo out"}))]),
        text_reply("ok"),
    ])
    .await;
    agent.prompt(&session, "go").await;
    agent.stdin = None;
    let _ = tokio::time::timeout(Duration::from_secs(15), agent.child.wait()).await;
    // Whatever is left on stdout after the close is read too.
    while let Ok(Some(message)) =
        tokio::time::timeout(Duration::from_millis(300), agent.lines.recv()).await
    {
        agent.seen.push(message);
    }
    agent.clean_stdout();
}

impl Agent {
    async fn write_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }
}

impl Model {
    /// Loads the scripted replies after the session exists (a test that needs a path first).
    fn set_replies(&self, replies: Vec<String>) {
        *self.script.lock().unwrap() = VecDeque::from(replies);
    }
}

// ---- Phase 3: sessions, titles, commands, context ----

impl Agent {
    async fn prompt_blocks(&mut self, session: &str, blocks: Value) -> Value {
        self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": blocks}),
        )
        .await
    }
}

#[tokio::test]
async fn the_agent_advertises_what_phase_3_serves() {
    let mut agent = Agent::start(Vec::new()).await;
    let answer = agent.initialize(json!({})).await;
    let caps = &answer["result"]["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["promptCapabilities"]["image"], true);
    assert_eq!(caps["promptCapabilities"]["embeddedContext"], true);
    assert!(caps["sessionCapabilities"]["list"].is_object());
}

#[tokio::test]
async fn load_replays_the_history_before_it_answers() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("A reply to remember")]).await;
    agent.prompt(&session, "remember this").await;
    let before = agent.seen.len();
    let cwd = windows_path(agent.work.path());
    let id = agent
        .send(
            "session/load",
            json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    let answer = agent.response(id).await;
    assert!(answer["result"].is_object(), "{answer}");
    let answered_at = agent
        .seen
        .iter()
        .position(|m| m["id"] == json!(id))
        .unwrap();
    let replayed: Vec<&Value> = agent.seen[before..answered_at]
        .iter()
        .filter(|m| m["method"] == "session/update")
        .map(|m| &m["params"]["update"])
        .collect();
    let kinds: Vec<&str> = replayed
        .iter()
        .map(|u| u["sessionUpdate"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"user_message_chunk"), "{kinds:?}");
    assert!(kinds.contains(&"agent_message_chunk"), "{kinds:?}");
    let user = replayed
        .iter()
        .find(|u| u["sessionUpdate"] == "user_message_chunk")
        .unwrap();
    assert_eq!(user["content"]["text"], "remember this");
}

#[tokio::test]
async fn loading_an_unknown_session_is_not_found() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.initialize(json!({})).await;
    let cwd = windows_path(agent.work.path());
    let answer = agent
        .request(
            "session/load",
            json!({"sessionId": "nope", "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert_eq!(answer["error"]["code"], -32002, "{answer}");
}

#[tokio::test]
async fn the_list_has_this_folders_threads_that_were_written_in() {
    let (mut agent, first) = Agent::with_session(vec![text_reply("one"), text_reply("two")]).await;
    let second = agent.new_session().await;
    let _unused = agent.new_session().await;
    agent.prompt(&first, "first thread").await;
    agent.prompt(&second, "second thread").await;
    let cwd = windows_path(agent.work.path());
    let listed = agent.request("session/list", json!({"cwd": cwd})).await;
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s["sessionId"].as_str().unwrap())
        .collect();
    // The thread nobody wrote in isn't listed.
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&first.as_str()) && ids.contains(&second.as_str()));
    assert!(sessions[0]["updatedAt"].is_string());
    assert!(listed["result"]["nextCursor"].is_null());
    // Another folder has none; no folder lists everything.
    let elsewhere = agent
        .request("session/list", json!({"cwd": "C:\\no\\such\\folder"}))
        .await;
    assert_eq!(elsewhere["result"]["sessions"], json!([]));
    let all = agent.request("session/list", json!({})).await;
    assert_eq!(all["result"]["sessions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_thread_gets_a_title_update() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Hi")]).await;
    agent.prompt(&session, "Please introduce yourself").await;
    // First the opening line, then the title model's.
    let sid = session.clone();
    let titles: Vec<String> = agent
        .wait_for(move |seen, _| {
            let titles: Vec<String> = seen
                .iter()
                .filter(|m| m["params"]["sessionId"] == sid.as_str())
                .filter(|m| m["params"]["update"]["sessionUpdate"] == "session_info_update")
                .filter_map(|m| m["params"]["update"]["title"].as_str().map(str::to_string))
                .collect();
            titles
                .contains(&"A test title".to_string())
                .then_some(titles)
        })
        .await;
    assert_eq!(
        titles.first().map(String::as_str),
        Some("Please introduce yourself")
    );
}

#[tokio::test]
async fn a_turn_reports_the_context_in_use() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Hi")]).await;
    agent.prompt(&session, "hello").await;
    let meter = agent.updates_of(&session, "usage_update");
    assert_eq!(meter.len(), 1, "{:?}", agent.updates(&session));
    assert!(meter[0]["used"].as_u64().unwrap() > 0);
    assert!(meter[0]["size"].as_u64().unwrap() > meter[0]["used"].as_u64().unwrap());
}

#[tokio::test]
async fn commands_are_offered_after_a_new_session() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let sid = session.clone();
    let commands = agent
        .wait_for(move |seen, _| {
            seen.iter()
                .find(|m| {
                    m["params"]["sessionId"] == sid.as_str()
                        && m["params"]["update"]["sessionUpdate"] == "available_commands_update"
                })
                .map(|m| m["params"]["update"]["availableCommands"].clone())
        })
        .await;
    let commands = commands.as_array().unwrap();
    assert!(!commands.is_empty());
    assert!(
        commands
            .iter()
            .all(|c| c["name"].is_string() && !c["name"].as_str().unwrap().starts_with('/'))
    );
}

#[tokio::test]
async fn a_slash_command_answers_without_the_model() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let answer = agent.prompt(&session, "/status").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(
        agent.reply_text(&session).contains("Session:"),
        "{:?}",
        agent.updates(&session)
    );
    assert!(
        agent.model.requests().is_empty(),
        "a command must not call the model"
    );
}

#[tokio::test]
async fn text_that_only_looks_like_a_command_goes_to_the_model() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Fine")]).await;
    agent.prompt(&session, "/not-a-command at all").await;
    assert!(
        agent.model.requests()[0]
            .to_string()
            .contains("/not-a-command at all")
    );
}

#[tokio::test]
async fn a_linked_file_reaches_the_model_as_its_content() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Read it")]).await;
    let file = work_file(&agent, "notes.txt");
    std::fs::write(&file, "the-secret-word-is-pineapple\n").unwrap();
    let uri = url_for(&file);
    agent
        .prompt_blocks(
            &session,
            json!([
                {"type": "text", "text": "what does this say?"},
                {"type": "resource_link", "uri": uri, "name": "notes.txt"},
            ]),
        )
        .await;
    assert!(
        agent.model.requests()[0].to_string().contains("pineapple"),
        "{}",
        agent.model.requests()[0]
    );
}

#[tokio::test]
async fn an_unsaved_buffer_is_sent_as_the_editor_has_it() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("Seen")]).await;
    agent
        .prompt_blocks(
            &session,
            json!([
                {"type": "text", "text": "review this"},
                {"type": "resource", "resource": {"uri": "file:///C:/proj/unsaved.rs", "text": "fn unsaved_edit_marker() {}"}},
            ]),
        )
        .await;
    assert!(
        agent.model.requests()[0]
            .to_string()
            .contains("unsaved_edit_marker")
    );
}

#[tokio::test]
async fn an_image_is_sent_with_the_prompt() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("A pixel")]).await;
    // A 1x1 PNG.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let answer = agent
        .prompt_blocks(
            &session,
            json!([
                {"type": "text", "text": "what is this?"},
                {"type": "image", "data": png, "mimeType": "image/png"},
            ]),
        )
        .await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let sent = agent.model.requests()[0].to_string();
    assert!(
        sent.contains("image_url") || sent.contains("pasted-image"),
        "{sent}"
    );
}

#[tokio::test]
async fn an_editor_that_refuses_forms_gets_the_question_as_a_card() {
    let mut agent = Agent::start(vec![
        tool_reply(&[(
            "c1",
            "clarify",
            json!({"questions": [{"question": "Which?", "choices": ["a", "b"]}]}),
        )]),
        text_reply("Done."),
    ])
    .await;
    // It says it takes forms, then answers the form request with "method not found".
    agent.initialize(json!({"elicitation": {"form": {}}})).await;
    let session = agent.new_session().await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "ask"}]}),
        )
        .await;
    let (form, _) = agent.agent_request("elicitation/create").await;
    agent.reply_error(form, -32601, "Method not found").await;
    let (card, params) = agent.agent_request("session/request_permission").await;
    assert_eq!(params["options"].as_array().unwrap().len(), 3);
    agent
        .reply(
            card,
            json!({"outcome": {"outcome": "selected", "optionId": "choice-0"}}),
        )
        .await;
    let answer = agent.response(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    // The turn went on to a second model request, after the answer.
    assert_eq!(agent.model.requests().len(), 2);
}

fn url_for(path: &Path) -> String {
    let text = path
        .display()
        .to_string()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file:///{text}")
}
