//! ACP's wire: JSON-RPC 2.0, one message per line, over any async reader and writer (the
//! process's stdin and stdout in `--acp` mode, `tokio::io::duplex` in tests).
//!
//! Both sides send requests, so [`Sender::request`] waits for the peer's answer, which the reader
//! hands back through [`Sender::resolve`]. Everything outgoing goes through one channel and one
//! writer task, a line per message, flushed at once.
//!
//! Hermes: `acp` (the Python SDK's `run_agent` connection), `acp_adapter/entry.py` (stdio only).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(RESOURCE_NOT_FOUND, message)
    }

    fn to_value(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }

    fn from_value(value: &Value) -> Self {
        Self {
            code: value["code"].as_i64().unwrap_or(INTERNAL_ERROR),
            message: value["message"]
                .as_str()
                .unwrap_or("the client reported an error")
                .to_string(),
            data: value.get("data").cloned(),
        }
    }
}

/// One line from the peer.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// The peer's answer to a request of ours.
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
}

/// Reads one line. A line that isn't JSON is -32700, JSON that isn't a JSON-RPC message -32600.
pub fn parse_line(line: &str) -> Result<Incoming, RpcError> {
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|e| RpcError::new(PARSE_ERROR, format!("not JSON: {e}")))?;
    let Some(object) = value.as_object() else {
        return Err(RpcError::new(
            INVALID_REQUEST,
            "a JSON-RPC message is an object",
        ));
    };
    let id = object.get("id").filter(|id| !id.is_null()).cloned();
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match id {
            Some(id) => Incoming::Request {
                id,
                method: method.to_string(),
                params,
            },
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        });
    }
    match (id, object.get("result"), object.get("error")) {
        (Some(id), Some(result), _) => Ok(Incoming::Response {
            id,
            result: Ok(result.clone()),
        }),
        (Some(id), None, Some(error)) => Ok(Incoming::Response {
            id,
            result: Err(RpcError::from_value(error)),
        }),
        _ => Err(RpcError::new(
            INVALID_REQUEST,
            "neither a request, a notification nor a response",
        )),
    }
}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;

/// Everything outgoing. Cheap to clone; every clone feeds the same writer.
#[derive(Clone)]
pub struct Sender {
    tx: mpsc::UnboundedSender<String>,
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<Pending>>,
}

impl Sender {
    /// A sender whose lines come out of the returned receiver (tests read it directly).
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                next_id: Arc::new(AtomicU64::new(1)),
                pending: Arc::default(),
            },
            rx,
        )
    }

    /// A sender writing to `output`, one flushed line per message.
    pub fn writing_to<W: AsyncWrite + Unpin + Send + 'static>(
        mut output: W,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (sender, mut lines) = Self::channel();
        let writer = tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                let written = async {
                    output.write_all(line.as_bytes()).await?;
                    output.write_all(b"\n").await?;
                    output.flush().await
                }
                .await;
                if written.is_err() {
                    break;
                }
            }
        });
        (sender, writer)
    }

    fn send(&self, message: Value) {
        // `to_string` escapes newlines inside strings: the message is one line.
        let _ = self.tx.send(message.to_string());
    }

    pub fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        self.send(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error.to_value()}),
        });
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Asks the peer and waits for its answer. An error if the peer errors, or goes away.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, reply);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        answer
            .await
            .unwrap_or_else(|_| Err(RpcError::internal("the client went away")))
    }

    /// The reader's hand-off of a response to one of our requests.
    pub fn resolve(&self, id: &Value, result: Result<Value, RpcError>) {
        let reply = id.as_u64().and_then(|id| {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
        });
        match reply {
            Some(reply) => {
                let _ = reply.send(result);
            }
            None => tracing::debug!("a response to no request of ours: {id}"),
        }
    }

    /// The peer is gone: every request still waiting fails.
    pub fn fail_all(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// Reads lines until the input ends (the client closed it), calling `each` with every message.
/// Blank lines are skipped; a line that doesn't parse is reported to `each` as an error.
pub async fn read_lines<R: AsyncRead + Unpin>(
    input: R,
    mut each: impl FnMut(Result<Incoming, RpcError>),
) {
    let mut lines = BufReader::new(input).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => {}
            Ok(Some(line)) => each(parse_line(&line)),
            Ok(None) => return,
            Err(error) => {
                tracing::warn!("reading the client's input: {error}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_has_an_id_and_a_notification_does_not() {
        let request =
            parse_line(r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"a":1}}"#);
        assert_eq!(
            request,
            Ok(Incoming::Request {
                id: json!(3),
                method: "initialize".into(),
                params: json!({"a": 1})
            })
        );
        let note = parse_line(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{}}"#);
        assert_eq!(
            note,
            Ok(Incoming::Notification {
                method: "session/cancel".into(),
                params: json!({})
            })
        );
    }

    #[test]
    fn ids_may_be_strings_and_params_may_be_missing() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":"abc","method":"x"}"#),
            Ok(Incoming::Request {
                id: json!("abc"),
                method: "x".into(),
                params: Value::Null
            })
        );
    }

    #[test]
    fn a_response_carries_a_result_or_an_error() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#),
            Ok(Incoming::Response {
                id: json!(1),
                result: Ok(json!({"ok": true}))
            })
        );
        let Ok(Incoming::Response { result: Err(e), .. }) =
            parse_line(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"no"}}"#)
        else {
            panic!("an error response");
        };
        assert_eq!((e.code, e.message.as_str()), (-32000, "no"));
    }

    #[test]
    fn bad_lines_are_refused_with_the_right_codes() {
        assert_eq!(parse_line("not json").unwrap_err().code, PARSE_ERROR);
        assert_eq!(parse_line("[1,2]").unwrap_err().code, INVALID_REQUEST);
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0"}"#).unwrap_err().code,
            INVALID_REQUEST
        );
    }

    #[test]
    fn a_windows_line_ending_is_trimmed() {
        assert!(parse_line("{\"jsonrpc\":\"2.0\",\"method\":\"x\"}\r").is_ok());
    }

    #[tokio::test]
    async fn a_response_reaches_the_request_it_answers() {
        let (sender, mut lines) = Sender::channel();
        let asker = sender.clone();
        let waiting = tokio::spawn(async move { asker.request("ask", json!({"q": 1})).await });
        let sent: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
        assert_eq!(sent["method"], "ask");
        let id = sent["id"].clone();
        // Someone else's answer first: it matches nothing and is ignored.
        sender.resolve(&json!(999), Ok(json!("stray")));
        sender.resolve(&id, Ok(json!("yes")));
        assert_eq!(waiting.await.unwrap(), Ok(json!("yes")));
    }

    #[tokio::test]
    async fn a_waiting_request_fails_when_the_client_goes_away() {
        let (sender, mut lines) = Sender::channel();
        let asker = sender.clone();
        let waiting = tokio::spawn(async move { asker.request("ask", json!({})).await });
        lines.recv().await.unwrap();
        sender.fail_all();
        assert!(waiting.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn messages_are_written_one_per_line_and_read_back() {
        let (client, server) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(server);
        let (sender, _writer) = Sender::writing_to(write);
        sender.notify("session/update", json!({"text": "two\nlines"}));
        sender.respond(json!(1), Ok(json!({})));
        drop(sender);
        let mut seen = Vec::new();
        let mut lines = BufReader::new(client).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            seen.push(line);
            if seen.len() == 2 {
                break;
            }
        }
        // The newline inside the text is escaped: still one line.
        assert!(seen[0].contains(r"two\nlines"));
        assert!(seen[1].contains("\"result\""));
        drop(read);
    }

    #[tokio::test]
    async fn the_reader_skips_blank_lines_and_stops_at_the_end() {
        let input = "\n{\"jsonrpc\":\"2.0\",\"method\":\"a\"}\n\nnot json\n";
        let mut got = Vec::new();
        read_lines(input.as_bytes(), |message| got.push(message)).await;
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], Ok(Incoming::Notification { .. })));
        assert_eq!(got[1].as_ref().unwrap_err().code, PARSE_ERROR);
    }
}
