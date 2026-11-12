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
    /// Requests that offered no tools (session titles).
    titles: Arc<std::sync::atomic::AtomicUsize>,
}

impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        // A title request offers no tools; a turn always does.
        let offers_tools = body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty());
        if !offers_tools {
            self.titles
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    titles: Arc<std::sync::atomic::AtomicUsize>,
}

impl Model {
    async fn start(replies: Vec<String>, delay: Duration) -> Self {
        let server = MockServer::start().await;
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let script = Arc::new(std::sync::Mutex::new(VecDeque::from(replies)));
        let titles = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Script {
                replies: script.clone(),
                requests: requests.clone(),
                delay,
                titles: titles.clone(),
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
            titles,
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
    home: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Agent {
    async fn start(replies: Vec<String>) -> Self {
        Self::start_with(replies, Duration::ZERO).await
    }

    async fn start_with(replies: Vec<String>, delay: Duration) -> Self {
        // The suite's counts are `balanced`'s (the default profile makes no title request).
        Self::start_profile(replies, delay, "balanced").await
    }

    async fn start_profile(replies: Vec<String>, delay: Duration, profile: &str) -> Self {
        let model = Model::start(replies, delay).await;
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = json!({
            "providers": [{"id": "mock", "name": "Mock", "base_url": format!("{}/v1", model.server.uri())}],
            "provider": "mock", "model": "mock",
            "background_review": false, "checkpoints": false,
        });
        std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
        // The budget profile lives in the desktop config (`budget.profile`).
        std::fs::write(
            home.path().join("desktop-config.json"),
            json!({"budget": {"profile": profile}}).to_string(),
        )
        .unwrap();
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
        let data = home.path().to_path_buf();
        Self::spawn_at(model, home, work, &data)
    }

    /// A second agent process on the data folder of another one (the desktop app and an editor).
    async fn sharing(first: &Agent) -> Self {
        let model = Model::start(Vec::new(), Duration::ZERO).await;
        let data = first.home.path().to_path_buf();
        Self::spawn_at(
            model,
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            &data,
        )
    }

    fn spawn_at(
        model: Model,
        home: tempfile::TempDir,
        work: tempfile::TempDir,
        data: &Path,
    ) -> Self {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hermes-acp"))
            .arg("--acp")
            .env("HERMES_RS_HOME", data)
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
            home,
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

/// Under `requests` with no title model set, the thread is named from its first message and no
/// title request is made (Phase 4).
#[tokio::test]
async fn under_requests_a_thread_gets_a_heuristic_title_and_no_title_request() {
    let mut agent = Agent::start_profile(vec![text_reply("Hi")], Duration::ZERO, "requests").await;
    agent.initialize(json!({})).await;
    let session = agent.new_session().await;
    agent.prompt(&session, "Please introduce yourself").await;
    // Give a (wrong) title request time to show up.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let sid = session.clone();
    let titles: Vec<String> = agent
        .wait_for(move |seen, _| {
            let titles: Vec<String> = seen
                .iter()
                .filter(|m| m["params"]["sessionId"] == sid.as_str())
                .filter(|m| m["params"]["update"]["sessionUpdate"] == "session_info_update")
                .filter_map(|m| m["params"]["update"]["title"].as_str().map(str::to_string))
                .collect();
            (!titles.is_empty()).then_some(titles)
        })
        .await;
    assert_eq!(titles, vec!["Introduce Yourself"]);
    assert_eq!(agent.model.requests().len(), 1, "only the turn's request");
    assert_eq!(
        agent.model.titles.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no title request"
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

// ---- Phase 4: model, effort and mode ----

impl Agent {
    /// `session/new` in the working folder, the whole answer.
    async fn new_session_answer(&mut self) -> Value {
        let cwd = self.work.path().display().to_string();
        self.request("session/new", json!({"cwd": cwd, "mcpServers": []}))
            .await
    }
}

fn option<'a>(options: &'a Value, id: &str) -> &'a Value {
    options
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id)
        .unwrap_or_else(|| panic!("no option {id} in {options}"))
}

fn model_values(options: &Value) -> Vec<String> {
    option(options, "model")["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_new_session_answers_at_once_and_the_full_model_picker_follows() {
    let mut agent = Agent::start(Vec::new()).await;
    agent.initialize(json!({})).await;
    // The answer doesn't wait for the provider's model list: the thread's own model, then an update.
    let answer = agent.new_session_answer().await;
    let result = &answer["result"];
    let model = option(&result["configOptions"], "model");
    assert_eq!(model["category"], "model");
    assert_eq!(model["currentValue"], "mock/mock");
    let session = result["sessionId"].as_str().unwrap().to_string();
    let effort = option(&result["configOptions"], "effort");
    assert_eq!(effort["category"], "thought_level");
    assert_eq!(effort["currentValue"], "medium");
    assert_eq!(result["modes"]["currentModeId"], "accept_edits");
    // The full picker arrives as a config_option_update.
    let sid = session.clone();
    let full = agent
        .wait_for(move |seen, _| {
            seen.iter()
                .find(|m| {
                    m["params"]["sessionId"] == sid.as_str()
                        && m["params"]["update"]["sessionUpdate"] == "config_option_update"
                })
                .map(|m| m["params"]["update"]["configOptions"].clone())
        })
        .await;
    let values = model_values(&full);
    assert!(
        values.contains(&"mock/mock".to_string()) && values.contains(&"mock/other".to_string()),
        "{values:?}"
    );
    // Once fetched, the list is kept: the next thread opens with it.
    let second = agent.new_session_answer().await;
    assert!(model_values(&second["result"]["configOptions"]).contains(&"mock/other".to_string()));
}

#[tokio::test]
async fn picking_a_model_routes_the_next_turn_to_it() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("From the other model")]).await;
    let answer = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "model", "value": "mock/other"}),
        )
        .await;
    let model = option(&answer["result"]["configOptions"], "model");
    assert_eq!(model["currentValue"], "mock/other", "{answer}");
    agent.prompt(&session, "who are you?").await;
    assert_eq!(agent.model.requests()[0]["model"], "other");
}

#[tokio::test]
async fn picking_an_effort_is_kept() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let answer = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "effort", "value": "high"}),
        )
        .await;
    assert_eq!(
        option(&answer["result"]["configOptions"], "effort")["currentValue"],
        "high"
    );
    let back = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "effort", "value": "off"}),
        )
        .await;
    assert_eq!(
        option(&back["result"]["configOptions"], "effort")["currentValue"],
        "off"
    );
}

#[tokio::test]
async fn an_option_or_value_we_never_offered_is_refused() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    for (id, value) in [
        ("effort", "extreme"),
        ("model", "no-slash"),
        ("colour", "red"),
    ] {
        let answer = agent
            .request(
                "session/set_config_option",
                json!({"sessionId": session, "configId": id, "value": value}),
            )
            .await;
        assert_eq!(answer["error"]["code"], -32602, "{id}: {answer}");
    }
}

#[tokio::test]
async fn yolo_mode_runs_a_flagged_command_without_asking() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let victim = work_file(&agent, "victim");
    std::fs::create_dir(&victim).unwrap();
    agent.model.set_replies(vec![
        tool_reply(&[(
            "c1",
            "terminal",
            json!({"command": flagged_command(&victim)}),
        )]),
        text_reply("Gone."),
    ]);
    let set = agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "yolo"}),
        )
        .await;
    assert!(set["result"].is_object(), "{set}");
    let answer = agent.prompt(&session, "clean up").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(!victim.exists(), "YOLO runs the flagged command");
    assert!(
        !agent
            .seen
            .iter()
            .any(|m| m["method"] == "session/request_permission"),
        "no card in YOLO mode"
    );
}

#[tokio::test]
async fn an_unknown_mode_is_refused() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let answer = agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "chaos"}),
        )
        .await;
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
}

// ---- Zed's first real session: what it showed (and didn't) ----

#[tokio::test]
async fn the_mode_is_a_config_option_that_switches_yolo() {
    // Zed shows config options and ignores `modes` when it gets them.
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let answer = agent.new_session_answer().await;
    let mode = option(&answer["result"]["configOptions"], "mode");
    assert_eq!(mode["category"], "mode");
    assert_eq!(mode["currentValue"], "accept_edits");
    let ids: Vec<&str> = mode["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["auto", "manual", "accept_edits", "plan", "yolo"]);
    let set = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "mode", "value": "yolo"}),
        )
        .await;
    assert_eq!(
        option(&set["result"]["configOptions"], "mode")["currentValue"],
        "yolo",
        "{set}"
    );
    let bad = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "mode", "value": "chaos"}),
        )
        .await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
}

#[tokio::test]
async fn the_efforts_are_hermes_levels_and_off_is_sent_as_none() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("ok")]).await;
    let answer = agent.new_session_answer().await;
    let effort = option(&answer["result"]["configOptions"], "effort");
    let values: Vec<&str> = effort["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    for wanted in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert!(values.contains(&wanted), "{wanted} in {values:?}");
    }
    assert!(!values.contains(&"default"), "{values:?}");
    let set = agent
        .request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "effort", "value": "off"}),
        )
        .await;
    assert_eq!(
        option(&set["result"]["configOptions"], "effort")["currentValue"],
        "off",
        "{set}"
    );
    agent.prompt(&session, "think?").await;
    assert_eq!(
        agent.model.requests()[0]["reasoning_effort"],
        "none",
        "{}",
        agent.model.requests()[0]
    );
}

#[tokio::test]
async fn the_gateways_own_commands_are_offered_and_usage_runs_without_the_model() {
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
    let names: Vec<&str> = commands
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for wanted in ["usage", "status", "context", "tools"] {
        assert!(names.contains(&wanted), "{wanted} in {names:?}");
    }
    let answer = agent.prompt(&session, "/usage").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(agent.model.requests().is_empty());
    assert!(!agent.reply_text(&session).is_empty());
}

#[tokio::test]
async fn an_editor_with_terminal_cards_gets_the_command_output_in_one() {
    let mut agent = Agent::start(vec![
        tool_reply(&[("t1", "terminal", json!({"command": "echo card-output"}))]),
        text_reply("Done."),
    ])
    .await;
    agent
        .initialize(json!({"_meta": {"terminal_output": true}}))
        .await;
    let session = agent.new_session_answer().await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    agent.prompt(&session, "run it").await;
    let started = &agent.updates_of(&session, "tool_call")[0];
    assert_eq!(started["_meta"]["terminal_info"]["terminal_id"], "t1");
    assert_eq!(started["content"][0]["type"], "terminal");
    assert_eq!(started["content"][0]["terminalId"], "t1");
    let finished = agent.updates_of(&session, "tool_call_update");
    let card: Vec<&Value> = finished
        .iter()
        .filter(|u| u["toolCallId"] == "t1")
        .collect();
    // The output, live while it runs and whatever is left at the end: all of it, once.
    let streamed: String = card
        .iter()
        .filter_map(|u| u["_meta"]["terminal_output"]["data"].as_str())
        .collect();
    assert_eq!(streamed.matches("card-output").count(), 1, "{card:?}");
    let last = card.last().unwrap();
    assert_eq!(last["status"], "completed", "{last}");
    assert_eq!(last["_meta"]["terminal_exit"]["exit_code"], 0);
}

// ---- Modes: Auto, Manual, Accept edits, Plan, YOLO ----

fn write_reply(path: &Path, text: &str) -> String {
    tool_reply(&[(
        "w1",
        "write_file",
        json!({"path": path.display().to_string(), "content": text}),
    )])
}

#[tokio::test]
async fn plan_mode_refuses_edits_and_tells_the_model_to_plan() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "planned.txt");
    agent.model.set_replies(vec![
        write_reply(&target, "x"),
        text_reply("Here is a plan."),
    ]);
    let set = agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "plan"}),
        )
        .await;
    assert!(set["result"].is_object(), "{set}");
    let answer = agent.prompt(&session, "make the file").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(!target.exists(), "Plan mode must not write");
    let failed = agent.updates_of(&session, "tool_call_update");
    let last = failed.last().unwrap();
    assert_eq!(last["status"], "failed", "{last}");
    assert!(last.to_string().contains("Plan mode"), "{last}");
}

#[tokio::test]
async fn manual_mode_asks_before_an_edit_and_obeys_the_answer() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "asked.txt");
    agent
        .model
        .set_replies(vec![write_reply(&target, "hello"), text_reply("Written.")]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "manual"}),
        )
        .await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "write it"}]}),
        )
        .await;
    let (id, params) = agent.agent_request("session/request_permission").await;
    assert_eq!(params["toolCall"]["kind"], "edit", "{params}");
    assert!(!target.exists(), "nothing is written before the answer");
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}),
        )
        .await;
    agent.response(prompt).await;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
}

#[tokio::test]
async fn manual_mode_denying_an_edit_leaves_the_file_alone() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "denied.txt");
    agent
        .model
        .set_replies(vec![write_reply(&target, "no"), text_reply("Understood.")]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "manual"}),
        )
        .await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "write it"}]}),
        )
        .await;
    let (id, _) = agent.agent_request("session/request_permission").await;
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "deny"}}),
        )
        .await;
    agent.response(prompt).await;
    assert!(!target.exists());
}

#[tokio::test]
async fn accept_edits_writes_without_asking() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "free.txt");
    agent
        .model
        .set_replies(vec![write_reply(&target, "free"), text_reply("Done.")]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "accept_edits"}),
        )
        .await;
    agent.prompt(&session, "write it").await;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "free");
    assert!(
        !agent
            .seen
            .iter()
            .any(|m| m["method"] == "session/request_permission")
    );
}

// ---- /undo and /retry ----

fn user_texts(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].to_string())
        .collect()
}

#[tokio::test]
async fn undo_drops_the_last_turn_and_the_next_prompt_continues_from_there() {
    let (mut agent, session) =
        Agent::with_session(vec![text_reply("A1"), text_reply("A2"), text_reply("A3")]).await;
    agent.prompt(&session, "first question").await;
    agent.prompt(&session, "second question").await;
    let answer = agent.prompt(&session, "/undo").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let said = agent.reply_text(&session);
    assert!(said.contains("Undid 1 turn"), "{said}");
    assert!(said.contains("second question"), "{said}");
    assert_eq!(
        agent.model.requests().len(),
        2,
        "/undo must not call the model"
    );
    agent.prompt(&session, "third question").await;
    let requests = agent.model.requests();
    let last = user_texts(requests.last().unwrap()).join(" ");
    assert!(last.contains("first question"), "{last}");
    assert!(last.contains("third question"), "{last}");
    assert!(!last.contains("second question"), "{last}");
}

#[tokio::test]
async fn undo_n_drops_n_turns() {
    let (mut agent, session) = Agent::with_session(vec![
        text_reply("A1"),
        text_reply("A2"),
        text_reply("A3"),
        text_reply("A4"),
    ])
    .await;
    for prompt in ["one", "two", "three"] {
        agent.prompt(&session, prompt).await;
    }
    agent.prompt(&session, "/undo 2").await;
    assert!(agent.reply_text(&session).contains("Undid 2 turns"));
    agent.prompt(&session, "four").await;
    let requests = agent.model.requests();
    let last = user_texts(requests.last().unwrap()).join(" ");
    assert!(last.contains("one") && last.contains("four"), "{last}");
    assert!(!last.contains("two") && !last.contains("three"), "{last}");
}

#[tokio::test]
async fn retry_sends_the_last_prompt_again_without_the_old_answer() {
    let (mut agent, session) =
        Agent::with_session(vec![text_reply("old answer"), text_reply("new answer")]).await;
    agent.prompt(&session, "alpha").await;
    let answer = agent.prompt(&session, "/retry").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let requests = agent.model.requests();
    assert_eq!(requests.len(), 2);
    let again = &requests[1];
    assert_eq!(user_texts(again).len(), 1, "{again}");
    assert!(user_texts(again)[0].contains("alpha"));
    assert!(!again.to_string().contains("old answer"), "{again}");
    assert!(agent.reply_text(&session).contains("new answer"));
}

#[tokio::test]
async fn undo_with_nothing_to_undo_or_a_bad_count_says_so() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("A1")]).await;
    agent.prompt(&session, "/undo").await;
    assert!(agent.reply_text(&session).contains("Nothing to undo"));
    agent.prompt(&session, "only one").await;
    agent.prompt(&session, "/undo 5").await;
    assert!(agent.reply_text(&session).contains("Only 1 turn"));
    agent.prompt(&session, "/undo x").await;
    assert!(agent.reply_text(&session).contains("Usage: /undo [N]"));
}

// ---- Two windows on one thread ----

#[cfg(windows)]
#[tokio::test]
async fn a_thread_running_in_the_desktop_app_refuses_the_editors_prompt() {
    use std::os::windows::fs::OpenOptionsExt as _;
    let (mut agent, session) = Agent::with_session(vec![text_reply("never")]).await;
    // What the desktop app holds while a turn runs on this thread.
    let dir = agent.home.path().join("running");
    std::fs::create_dir_all(&dir).unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .share_mode(0)
        .open(dir.join(format!("{session}.lock")))
        .unwrap();
    let answer = agent.prompt(&session, "hello").await;
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("another Hermes window")),
        "{answer}"
    );
    assert!(agent.model.requests().is_empty());
    drop(held);
    let answer = agent.prompt(&session, "hello again").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
}

#[tokio::test]
async fn a_delegation_shows_the_subagents_work_on_its_card() {
    let (mut agent, session) = Agent::with_session(vec![
        tool_reply(&[("d1", "delegate_task", json!({"goal": "say hi to the user"}))]),
        text_reply("child says hi"),
        text_reply("All done."),
    ])
    .await;
    let answer = agent.prompt(&session, "delegate it").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let updates = agent.updates_of(&session, "tool_call_update");
    let card: Vec<&Value> = updates.iter().filter(|u| u["toolCallId"] == "d1").collect();
    let logged = card
        .iter()
        .any(|u| u.to_string().contains("say hi to the user"));
    assert!(logged, "no subagent line on the card: {card:?}");
    let last = card.last().unwrap();
    assert_eq!(last["status"], "completed", "{last}");
}

// ---- Crash safety ----

#[tokio::test]
async fn a_handler_that_panics_answers_an_error_and_the_agent_carries_on() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("still here")]).await;
    let answer = agent.request("_test/panic", json!({})).await;
    assert_eq!(answer["error"]["code"], -32603, "{answer}");
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("internal error")),
        "{answer}"
    );
    // The agent and the thread are fine.
    let next = agent.prompt(&session, "hello").await;
    assert_eq!(next["result"]["stopReason"], "end_turn", "{next}");
    assert_eq!(agent.reply_text(&session), "still here");
}

#[tokio::test]
async fn two_agent_processes_share_threads_and_continue_each_others() {
    // The desktop app and an editor are two processes on one data folder.
    let (mut first, session) =
        Agent::with_session(vec![text_reply("A1"), text_reply("B2"), text_reply("A3")]).await;
    first.prompt(&session, "from window one").await;
    let cwd = first.work.path().display().to_string();
    let mut second = Agent::sharing(&first).await;
    second.initialize(json!({})).await;
    let listed = second.request("session/list", json!({"cwd": cwd})).await;
    let ids: Vec<&str> = listed["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["sessionId"].as_str())
        .collect();
    assert!(ids.contains(&session.as_str()), "{listed}");
    let loaded = second
        .request(
            "session/load",
            json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert!(loaded["result"].is_object(), "{loaded}");
    second.prompt(&session, "from window two").await;
    // The first window's next turn starts from what the second stored.
    first.prompt(&session, "back in window one").await;
    let last = first.model.requests().last().unwrap().to_string();
    for wanted in ["from window one", "from window two", "back in window one"] {
        assert!(last.contains(wanted), "{wanted} missing from {last}");
    }
}

// ---- Review fixes: modes, effort, close, resume ----

#[tokio::test]
async fn plan_mode_allows_a_plan_file_but_not_an_escape_from_the_plans_folder() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let escape = work_file(&agent, "escaped.txt");
    let plan = agent.work.path().join(".hermes").join("plans").join("p.md");
    agent.model.set_replies(vec![
        tool_reply(&[(
            "w1",
            "write_file",
            json!({"path": ".hermes/plans/../../escaped.txt", "content": "x"}),
        )]),
        tool_reply(&[(
            "w2",
            "write_file",
            json!({"path": ".hermes/plans/p.md", "content": "# plan"}),
        )]),
        text_reply("Planned."),
    ]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "plan"}),
        )
        .await;
    agent.prompt(&session, "plan it").await;
    assert!(!escape.exists(), "the escape was written");
    assert_eq!(std::fs::read_to_string(&plan).unwrap(), "# plan");
}

#[tokio::test]
async fn a_subagent_keeps_the_threads_plan_mode() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "by-the-child.txt");
    agent.model.set_replies(vec![
        tool_reply(&[(
            "d1",
            "delegate_task",
            json!({"tasks": [{"goal": "write the file"}]}),
        )]),
        tool_reply(&[(
            "c1",
            "write_file",
            json!({"path": target.display().to_string(), "content": "x"}),
        )]),
        text_reply("I could not write it."),
        text_reply("Done."),
    ]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "plan"}),
        )
        .await;
    agent.prompt(&session, "delegate the write").await;
    assert!(!target.exists(), "a subagent wrote in Plan mode");
}

#[tokio::test]
async fn manual_mode_cards_offer_once_or_deny_only() {
    let (mut agent, session) = Agent::with_session(Vec::new()).await;
    let target = work_file(&agent, "once.txt");
    agent.model.set_replies(vec![
        tool_reply(&[(
            "w1",
            "write_file",
            json!({"path": target.display().to_string(), "content": "x"}),
        )]),
        text_reply("ok"),
    ]);
    agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "manual"}),
        )
        .await;
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "write"}]}),
        )
        .await;
    let (id, params) = agent.agent_request("session/request_permission").await;
    let ids: Vec<&str> = params["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["optionId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["allow_once", "deny"], "{params}");
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "deny"}}),
        )
        .await;
    agent.response(prompt).await;
}

#[tokio::test]
async fn the_effort_shown_is_the_effort_sent() {
    let (mut agent, session) = Agent::with_session(vec![text_reply("ok")]).await;
    agent.prompt(&session, "hi").await;
    // The picker said Medium (nothing picked, nothing in Settings): the request says it too.
    assert_eq!(agent.model.requests()[0]["reasoning_effort"], "medium");
}

#[tokio::test]
async fn closing_a_session_ends_its_turn_and_it_can_be_loaded_again() {
    let (mut agent, session) =
        Agent::with_session(vec![text_reply("first"), text_reply("again")]).await;
    let caps = agent
        .request("initialize", json!({"protocolVersion": 1}))
        .await;
    let session_caps = &caps["result"]["agentCapabilities"]["sessionCapabilities"];
    assert!(
        session_caps["close"].is_object() && session_caps["resume"].is_object(),
        "{caps}"
    );
    agent.prompt(&session, "hello").await;
    let closed = agent
        .request("session/close", json!({"sessionId": session}))
        .await;
    assert!(closed["result"].is_object(), "{closed}");
    let cwd = agent.work.path().display().to_string();
    let loaded = agent
        .request(
            "session/load",
            json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert!(loaded["result"].is_object(), "{loaded}");
    let answer = agent.prompt(&session, "still there?").await;
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let last = agent.model.requests().last().unwrap().to_string();
    assert!(last.contains("hello"), "{last}");
}

#[tokio::test]
async fn resume_reopens_a_thread_without_replaying_it() {
    let (mut first, session) = Agent::with_session(vec![text_reply("A1"), text_reply("B2")]).await;
    first.prompt(&session, "remember the word kiwi").await;
    let cwd = first.work.path().display().to_string();
    let mut second = Agent::sharing(&first).await;
    second.initialize(json!({})).await;
    let resumed = second
        .request(
            "session/resume",
            json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert!(resumed["result"]["configOptions"].is_array(), "{resumed}");
    assert!(
        second.updates_of(&session, "user_message_chunk").is_empty(),
        "resume must not replay the history"
    );
    second.prompt(&session, "which word?").await;
    let last = first.model.requests().last().unwrap().to_string();
    assert!(last.contains("kiwi"), "{last}");
}

// ---- t3code: a fresh agent per thread, a test session per check ----

#[tokio::test]
async fn a_fresh_agent_answers_with_the_model_list_an_earlier_one_fetched() {
    let mut first = Agent::start(Vec::new()).await;
    first.initialize(json!({})).await;
    let session = first.new_session_answer().await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let sid = session.clone();
    first
        .wait_for(move |seen, _| {
            seen.iter()
                .find(|m| {
                    m["params"]["sessionId"] == sid.as_str()
                        && m["params"]["update"]["sessionUpdate"] == "config_option_update"
                })
                .map(|_| ())
        })
        .await;
    // Another process on the same data folder, its first thread: the full picker at once.
    let mut second = Agent::sharing(&first).await;
    second.initialize(json!({})).await;
    let answer = second.new_session_answer().await;
    let values = model_values(&answer["result"]["configOptions"]);
    assert!(values.contains(&"mock/other".to_string()), "{values:?}");
}

#[tokio::test]
async fn an_empty_thread_this_agent_made_is_deleted_when_closed() {
    let (mut agent, empty) = Agent::with_session(vec![text_reply("kept")]).await;
    let used = agent.new_session().await;
    agent.prompt(&used, "keep me").await;
    for id in [&empty, &used] {
        let closed = agent
            .request("session/close", json!({"sessionId": id}))
            .await;
        assert!(closed["result"].is_object(), "{closed}");
    }
    let cwd = agent.work.path().display().to_string();
    let gone = agent
        .request(
            "session/load",
            json!({"sessionId": empty, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert!(
        gone["error"].is_object(),
        "the empty thread is still there: {gone}"
    );
    let kept = agent
        .request(
            "session/load",
            json!({"sessionId": used, "cwd": cwd, "mcpServers": []}),
        )
        .await;
    assert!(kept["result"].is_object(), "{kept}");
}

#[tokio::test]
async fn t3code_threads_ask_about_everything_so_its_policy_decides() {
    let mut agent = Agent::start(Vec::new()).await;
    agent
        .request(
            "initialize",
            json!({"protocolVersion": 2, "clientCapabilities": {},
                   "clientInfo": {"name": "t3-code", "version": "0.0.0"}}),
        )
        .await;
    let target = work_file(&agent, "t3.txt");
    agent.model.set_replies(vec![
        tool_reply(&[(
            "w1",
            "write_file",
            json!({"path": "t3.txt", "content": "x"}),
        )]),
        text_reply("ok"),
    ]);
    let answer = agent.new_session_answer().await;
    assert_eq!(
        answer["result"]["modes"]["currentModeId"], "manual",
        "{answer}"
    );
    let session = answer["result"]["sessionId"].as_str().unwrap().to_string();
    let prompt = agent
        .send(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": "write"}]}),
        )
        .await;
    // Even a plain file edit asks: t3code's policy answers it by kind and location.
    let (id, params) = agent.agent_request("session/request_permission").await;
    assert_eq!(params["toolCall"]["kind"], "edit");
    let location = params["toolCall"]["locations"][0]["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(location.ends_with("t3.txt"), "{location}");
    agent
        .reply(
            id,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}),
        )
        .await;
    agent.response(prompt).await;
    assert!(target.exists());
}
