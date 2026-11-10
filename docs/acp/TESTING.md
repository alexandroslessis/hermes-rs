# Testing ACP

Four layers, in order of preference. The first two run in `cargo test` and are the gate for every
commit; the probe is for debugging; Zed is the user's check.

| Layer | Where | Catches |
|---|---|---|
| 1. Unit tests | `#[cfg(test)]` in `acp/rpc.rs`, `translate.rs`, `tools.rs`, `prompt.rs`, `history.rs`, `permissions.rs` | Wrong shapes, wrong mappings, edge cases (Windows paths, empty text, unknown tools) |
| 2. Black-box protocol tests | `crates/desktop/tests/acp.rs` | The whole path: stdio framing → server → gateway → runtime → mock model → events → updates |
| 3. Probe script | `scripts/acp/acp-probe.mjs` | Seeing the raw traffic; trying a real model; checking the release exe |
| 4. Zed | [ZED.md](ZED.md) checklists | What only the real editor shows |

Build and run (from the repo root, PowerShell):
```powershell
$env:CARGO_TARGET_DIR = "target/claude"
cargo test -p hermes-desktop acp            # unit tests whose names contain "acp" + module tests
cargo test -p hermes-desktop --test acp     # the black-box tests (builds the exe first)
```

## 1. Unit tests

Pure functions, plain inputs, assert on `serde_json::Value`. Name tests as sentences
(docs/TESTING.md). What each module must cover:

- **rpc.rs:** request / notification / response (result and error) parse; id as number and as
  string; `\r\n`; blank line; invalid JSON → -32700; `Sender::request` gets *its* response even
  when another response arrives first; an unanswered request fails when the reader stops.
- **translate.rs:** feed recorded gateway frames (as `json!` literals copied from real wire logs;
  see "recording frames" below) and assert the `Action`s: deltas → chunks in order; one
  `messageId` per reply, a new one after a tool call; `tool.start`/`tool.complete` pairing by
  `tool_id`; open calls failed at the end; `complete`/`interrupted`/`error`.
- **tools.rs:** one test per table row (kind, title), locations for relative and absolute paths
  (`C:\x`, `C:/x`, `src\a.rs` against a cwd), failure detection (`{"error":…}`,
  `{"success":false}`, `Error: …`, a normal result), truncation at 5000 chars, plan mapping
  including cancelled items.
- **prompt.rs:** each block type; `file:///C:/Users/a%20b/x.rs` → `C:\Users\a b\x.rs`; resource
  without name; text joining.
- **history.rs:** a stored conversation with text, reasoning, a tool call + result, an empty
  assistant row → the expected update list.
- **permissions.rs:** options built from `choices`; every outcome → the right gateway answer,
  including `cancelled` and an unknown `optionId` → `deny`; clarify form schema for free / choices /
  multi-select; elicitation answers → clarify answers.

**Recording frames:** run the desktop perf instance with the wire log (`perf.ps1` sets
`RUST_LOG=info,hermes_desktop::wire=debug`) or, better, have the probe log what the ACP server
received from the gateway (add a debug log line in the pump under target `hermes_desktop::acp::wire`).
Paste a few real frames into the tests; don't invent payload fields.

## 2. Black-box protocol tests (`crates/desktop/tests/acp.rs`)

`hermes-desktop` is a binary crate, so integration tests can't call its functions — but Cargo
builds the exe for them and gives its path in `env!("CARGO_BIN_EXE_hermes-desktop")`. The test
spawns it with `--acp` and talks to it over pipes, exactly like Zed. (Python analogy: a pytest that
runs the CLI with `subprocess.Popen` and speaks to its stdin/stdout.)

Dev-dependencies to add to `crates/desktop/Cargo.toml` (both already in `Cargo.lock`):
`wiremock = "0.6"`, `tokio` (with `process`, `io-util`, `macros`, `rt-multi-thread`, `time`).

### The harness

Write it once, at the top of `tests/acp.rs`:

```rust
struct Agent {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
    seen: Vec<Value>,               // every line the agent sent, for asserts and for failure output
    home: tempfile::TempDir,        // HERMES_RS_HOME
    work: tempfile::TempDir,        // the session cwd
    model: MockModel,               // wiremock server + scripted replies + recorded requests
}

impl Agent {
    /// Starts the exe with --acp; HERMES_RS_HOME = a temp folder with config.json pointing at the
    /// mock model: {"providers":[{"id":"mock","name":"Mock","base_url":"<mock>/v1"}],
    ///              "provider":"mock","model":"mock","background_review":false,"checkpoints":false}
    async fn start(replies: Vec<String>) -> Self;
    async fn request(&mut self, method: &str, params: Value) -> Value;   // sends, collects notifications into `seen` until the response with our id; panics on timeout (30 s) with `seen` printed
    async fn notify(&mut self, method: &str, params: Value);
    async fn next_request_from_agent(&mut self, method: &str) -> (Value /*id*/, Value /*params*/); // e.g. session/request_permission
    async fn reply(&mut self, id: Value, result: Value);
    fn updates(&self, session: &str) -> Vec<Value>;                       // the `update` objects of session/update notifications
    async fn initialized_session(replies: Vec<String>) -> (Self, String); // start + initialize + session/new
}
```

- **The mock model:** copy `sse`, `text_reply`, `tool_reply`, `delta` and the `Script` responder
  from `crates/runtime/tests/turns.rs` (same crate graph, copy is fine: they're test helpers).
  Add: a **non-streaming** POST (titles, `"stream": false` in the body) answers a fixed title
  JSON, without consuming a scripted reply; `GET /v1/models` answers `{"data":[{"id":"mock"}]}`.
- **Timeouts everywhere:** every read is under `tokio::time::timeout`; a hang must fail the test
  with the transcript (`seen`), not block CI forever.
- **Never the user's data:** `HERMES_RS_HOME` always set; also set `HERMES_DESKTOP_PORT` to
  something unused, and `RUST_LOG=info,hermes_desktop::acp=debug` so failures have logs (stderr is
  captured; print it on failure).
- **Kill on drop:** `kill_on_drop(true)` on the `Command`.
- **Parallel tests:** each test has its own exe process, home and mock port; that's fine.

### T-stdout: the stdout guard

`stdout_carries_only_json_lines`: run a full prompt with a tool call, then close stdin, read all
remaining stdout and assert every line parses as JSON with `"jsonrpc":"2.0"`. Any log line on
stdout fails it. Keep this test in every phase.

### Tests by phase

The names are in [PHASES.md](PHASES.md) under each phase. Shape of a typical one:

```rust
#[tokio::test]
async fn a_prompt_streams_the_reply_and_ends_the_turn() {
    let (mut agent, session) = Agent::initialized_session(vec![text_reply("Hello there")]).await;
    let result = agent
        .request("session/prompt", json!({"sessionId": session, "prompt": [{"type": "text", "text": "hi"}]}))
        .await;
    assert_eq!(result["result"]["stopReason"], "end_turn");
    let text: String = agent.updates(&session).iter()
        .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
        .map(|u| u["content"]["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(text, "Hello there");
    // What the model was sent: the user's text.
    assert!(agent.model.requests()[0].to_string().contains("\"hi\""));
}
```

Cancel test pattern: script a reply the mock **delays** (wiremock `ResponseTemplate::set_delay`,
5 s), send `session/prompt` without awaiting (`write` the line yourself), wait for the first
update or 300 ms, send `session/cancel`, then read until the prompt's response:
`stopReason == "cancelled"` within 3 s.

Permission test pattern: script `tool_reply(terminal, {"command": "Remove-Item -Recurse C:\\x"})`
(a command the approval patterns flag — pick one from `crates/tools/src/approval.rs` tests) then
`text_reply("done")`; send the prompt without awaiting; `next_request_from_agent("session/request_permission")`;
assert the options; reply `{"outcome":{"outcome":"selected","optionId":"deny"}}`; await the
prompt; assert the second model request contains the denial text and nothing ran.

## 3. The probe (`scripts/acp/acp-probe.mjs`)

For seeing what actually goes over the wire, and for runs against a real model.

```powershell
# a conversation script: JSON array of messages to send; "$session" is replaced by the id session/new returned
tools/node/node.exe scripts/acp/acp-probe.mjs `
  --exe target/claude/debug/hermes-desktop.exe `
  --home target/acp-home `
  --script scripts/acp/scripts/hello.json `
  [--allow allow_once]            # how to answer permission requests (default: deny)
```

- Prints every line with direction (`→` to the agent, `←` from it) and milliseconds since start;
  stderr lines prefixed `!`.
- Waits for each request's response before sending the next message (notifications are sent at
  once); `{"sleep": 500}` entries pause.
- Exits non-zero if any stdout line isn't JSON.
- `target/acp-home` is yours: set it up once with a `config.json` (the mock as above, or a real
  OpenRouter model with the key from `hermes_rs_or_api_key` stored the way `providers.rs` reads
  keys — never write the key into a file, never print it).
- Keep a few scripts in `scripts/acp/scripts/` (`hello.json`, `tool.json`, `cancel.json`,
  `load.json`) and mention the probe in `scripts/perf/README.md`.

## 4. Zed

Zed is the real client; it's on this machine (`%LOCALAPPDATA%\Programs\Zed`, 1.26.2). Setup and
the per-phase checklists are in [ZED.md](ZED.md).

- **Don't edit the user's Zed settings** (`%APPDATA%\Zed\settings.json`) without asking; give them
  the snippet. If the user agrees to you running Zed yourself, use an isolated profile
  (`zed.exe --user-data-dir <repo>\target\zed-profile`), never their own.
- What the user reports back is best captured as **ACP logs**: Zed command palette →
  `dev: open acp logs` shows every message both ways; ask them to copy the relevant part. Our side:
  `<HERMES_RS_HOME>\logs\acp.log`.

## Before you commit (every ACP phase)

```powershell
$env:CARGO_TARGET_DIR = "target/claude"
cargo test -p hermes-desktop                    # includes tests/acp.rs
cargo test -p hermes_runtime --test turns       # if the runtime changed
cargo build --workspace
```
All green, the phase's ZED.md checklist written for the user, docs updated (PHASES.md "Finishing
each phase").
