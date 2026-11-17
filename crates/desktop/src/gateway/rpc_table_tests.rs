//! Every method the page may call, answered over a spread of params that need no session: the
//! table in `tests/fixtures/rpc_table.txt` is what `dispatch` answered before CP13f (typed
//! `Request`, `RpcError`) and must keep answering: the code and message of every error, the
//! shape of every success (a small result in full). One line per method and params.
//!
//! A method added or changed on purpose: run
//! `HERMES_RPC_TABLE_BLESS=1 cargo test -p hermes-desktop --lib rpc_table` and read the diff of
//! the fixture. Unknown names must stay `-32601` (ground rule 5).

use std::sync::Arc;

use serde_json::{Value, json};

use super::rpc::SERVED;
use super::*;

const FIXTURE: &str = include_str!("../../tests/fixtures/rpc_table.txt");

/// Fields the methods read; the "every field" spreads set all of them to one value.
const FIELDS: &[&str] = &[
    "session_id",
    "parent_session_id",
    "id",
    "name",
    "title",
    "text",
    "path",
    "paths",
    "cwd",
    "key",
    "value",
    "provider",
    "model",
    "operation",
    "payload",
    "format",
    "hash",
    "word",
    "command",
    "arg",
    "source",
    "offset",
    "days",
    "paused",
    "admission_id",
    "submission_id",
    "input_id",
    "prompt_id",
    "request_id",
    "choice",
    "answer",
    "subagent_id",
    "process_id",
    "folders",
    "primary_path",
    "use",
    "preview_limit",
    "project_id",
    "is_primary",
    "target_message_id",
    "through_message_id",
    "count",
    "idempotency_key",
    "replay_only",
    "all",
    "file_path",
    "redact",
    "focus_topic",
    "confirm_truncate",
    "truncate_before_row_id",
    "truncate_before_user_ordinal",
    "attachments",
    "messages",
    "effort",
    "shown",
    "data_url",
    "filename",
    "content_base64",
    "staged_upload",
];

fn spread() -> Vec<Value> {
    let all = |v: Value| Value::Object(FIELDS.iter().map(|f| (f.to_string(), v.clone())).collect());
    vec![
        Value::Null,
        json!({}),
        json!("x"),
        json!({"session_id": 5}),
        json!({"session_id": "nope-id", "extra": {"a": 1}, "text": "hi"}),
        all(json!(5)),
        all(json!("5")),
        all(json!(null)),
    ]
}

/// Payloads like the page's, for the methods where a plain spread says little.
fn shaped(method: &str) -> Vec<Value> {
    let s = "nope-session";
    match method {
        "projects.tree" | "projects.project_sessions" => vec![
            json!({"preview_limit": 1}),
            json!({"project_id": "nope-project"}),
            json!({"project_id": "nope-project", "preview_limit": "2"}),
        ],
        "projects.create" => vec![
            json!({"name": "  My  ", "folders": ["a", 5, "b"], "primary_path": "b", "use": true}),
            json!({"name": "", "folders": "x", "primary_path": ""}),
            json!({"use": 1}),
        ],
        "projects.update" | "projects.add_folder" => vec![
            json!({"id": "nope-project", "name": " Renamed ", "path": "c", "is_primary": true}),
            json!({"id": "nope-project", "name": "  ", "path": ""}),
            json!({"id": 5}),
        ],
        "projects.delete" | "projects.set_active" => {
            vec![json!({"id": "nope-project"}), json!({"id": null})]
        }
        "model.options" => vec![json!({"session_id": s})],
        "requests.status" => vec![
            json!({"provider": "mock"}),
            json!({"provider": "nope"}),
            json!({"provider": 3}),
        ],
        "staging.apply" | "staging.reject" => vec![json!({"id": "abcdef012345"}), json!({"id": 5})],
        "config.get" => vec![
            json!({"key": "model"}),
            json!({"key": "budget.profile"}),
            json!({"key": 5}),
        ],
        "config.set" => vec![
            json!({"key": "display.x", "value": 1}),
            json!({"key": "display.x", "value": {"a": [1, null]}}),
            json!({"key": "model", "value": "m2 --provider mock --session", "session_id": s}),
            json!({"key": "model", "value": ""}),
            json!({"key": "model", "value": 5}),
            json!({"key": "model", "value": "m --provider nope"}),
            json!({"key": "reasoning", "value": "High", "session_id": s}),
            json!({"key": "reasoning", "value": null, "session_id": s}),
            json!({"key": "reasoning", "value": "low"}),
            json!({"key": "reasoning", "value": 5, "session_id": 7}),
            json!({"key": "budget.profile", "value": "tokens"}),
            json!({"key": "budget.profile", "value": "fast"}),
            json!({"key": "budget.max_iterations", "value": 0}),
            json!({"key": "budget.nonsense", "value": 1}),
            json!({"key": "", "value": 1}),
            json!({"key": 5, "value": 1}),
            json!({"value": 1}),
        ],
        "complete.slash" => vec![json!({"text": "/pl"}), json!({"text": 5})],
        "slash.exec" => vec![
            json!({"command": "/goal set it"}),
            json!({"command": " /goal"}),
            json!({"command": "/plan do it", "session_id": s}),
            json!({"command": "/status"}),
            json!({"command": "/status", "session_id": s}),
            json!({"command": "/usage", "session_id": s}),
            json!({"command": "/review"}),
            json!({"command": "/init"}),
            json!({"command": "/nonexistent a b"}),
            json!({"command": ""}),
            json!({"command": 5}),
        ],
        "command.dispatch" => vec![
            json!({"name": "plan", "arg": "x"}),
            json!({"name": "/review", "session_id": s}),
            json!({"name": "nonexistent", "arg": "a"}),
            json!({"name": 5, "arg": 6}),
        ],
        "session.title" => vec![
            json!({"session_id": s, "title": " A title "}),
            json!({"session_id": s, "title": 5}),
        ],
        "session.mutate" => vec![
            json!({"session_id": s, "operation": "rename", "payload": {"title": " T "}}),
            json!({"session_id": s, "operation": "model", "payload": {"model": "m", "provider": "mock"}}),
            json!({"session_id": s, "operation": "reasoning", "payload": {"effort": "HIGH"}}),
            json!({"session_id": s, "operation": "rewind", "payload": {"target_message_id": 0}}),
            json!({"session_id": s, "operation": "rewind", "payload": {"target_message_id": "0"}}),
            json!({"session_id": s, "operation": "workspace", "payload": {"cwd": "C:/nonexistent-folder"}}),
            json!({"session_id": s, "operation": "workspace", "payload": {}}),
            json!({"session_id": s, "operation": "archive", "payload": {"archived": true}}),
            json!({"session_id": s, "operation": "nope", "payload": {}}),
            json!({"session_id": s, "operation": 5}),
        ],
        "session.rewind" => vec![
            json!({"session_id": s, "replay_only": true}),
            json!({"session_id": s, "target_message_id": 0}),
            json!({"session_id": s, "target_message_id": "1"}),
        ],
        "rollback.list" | "rollback.diff" | "rollback.restore" => vec![
            json!({"session_id": s}),
            json!({"session_id": s, "hash": " ABC ", "all": true, "file_path": "a.txt"}),
        ],
        "session.export" => vec![
            json!({"session_id": s, "format": "md"}),
            json!({"session_id": s, "format": "docx"}),
            json!({"session_id": s, "format": 5}),
        ],
        "session.foreign.list" => vec![
            json!({"source": "nosuch"}),
            json!({"source": "nosuch", "offset": 3}),
            json!({"source": "nosuch", "offset": "3"}),
            json!({"source": "nosuch", "offset": -1}),
        ],
        "session.foreign.preview" | "session.foreign.import" => {
            vec![json!({"id": "nosuch-session"}), json!({"id": 5})]
        }
        "session.branch" | "session.branch_whole" | "session.branch_stored" => vec![
            json!({"session_id": s, "idempotency_key": "k1", "name": " B "}),
            json!({"parent_session_id": s, "through_message_id": 0}),
            json!({"parent_session_id": "", "session_id": s}),
        ],
        "prompt.btw" | "prompt.background" => vec![
            json!({"session_id": s, "text": "  hi "}),
            json!({"session_id": s, "text": "   "}),
            json!({"session_id": s, "text": 5}),
        ],
        "file.attach" => vec![
            json!({"session_id": s, "path": "a.txt"}),
            json!({"session_id": s, "name": "n.txt", "content_base64": "aGk="}),
        ],
        "image.attach" => vec![
            json!({"session_id": s, "path": "C:/nonexistent/none.png"}),
            json!({"session_id": s}),
        ],
        "image.attach_bytes" => vec![
            json!({"session_id": s, "content_base64": "!!", "filename": "a.png"}),
            json!({"session_id": s}),
        ],
        "image.detach" => vec![
            json!({"session_id": s, "path": " x "}),
            json!({"session_id": s, "path": 5}),
        ],
        "insights.get" => vec![
            json!({"days": 7}),
            json!({"days": 0}),
            json!({"days": -3}),
            json!({"days": "7"}),
            json!({"days": 1.5}),
        ],
        "session.compress" => vec![json!({"session_id": s, "focus_topic": "  x "})],
        "session.create" => vec![
            json!({"cwd": "."}),
            json!({"cwd": ".", "title": "Titled", "model": "m9"}),
            json!({"provider": "nope"}),
            json!({"provider": "mock", "model": "x", "cwd": ""}),
            json!({"cwd": ".", "messages": [{"role": "user", "content": "hi"}], "parent_session_id": s}),
            json!({"cwd": ".", "messages": "x", "parent_session_id": "nope"}),
        ],
        "prompt.submit" => vec![
            json!({"session_id": s, "text": "hi"}),
            json!({"session_id": s}),
            json!({"session_id": s, "text": "hi", "confirm_truncate": true, "truncate_before_row_id": 99}),
        ],
        "approval.respond" | "clarify.respond" => vec![
            json!({"prompt_id": "p1", "choice": "once", "answer": "a"}),
            json!({"request_id": "p1", "choice": "once"}),
            json!({"prompt_id": 5}),
        ],
        "process.kill" => vec![
            json!({"session_id": s, "process_id": "p"}),
            json!({"session_id": s, "process_id": 5}),
        ],
        "subagent.tail" | "subagent.interrupt" => {
            vec![json!({"session_id": s, "subagent_id": "sa1"})]
        }
        "subagent.steer" => vec![
            json!({"session_id": s, "subagent_id": "sa1", "text": " go "}),
            json!({"session_id": s, "subagent_id": "sa1", "text": ""}),
            json!({"session_id": s, "subagent_id": 1, "text": 5}),
        ],
        "delegation.pause" => vec![
            json!({"paused": false}),
            json!({"paused": true}),
            json!({"paused": "false"}),
            json!({"paused": null}),
        ],
        "complete.path" => vec![
            json!({"word": "@fi", "cwd": "."}),
            json!({"word": "./", "session_id": s}),
            json!({"word": 5, "cwd": 5}),
        ],
        "session.steer" | "session.redirect" => vec![
            json!({"session_id": s, "text": "now"}),
            json!({"session_id": s, "text": 5}),
        ],
        "prompt.cancel" | "prompt.steer_queued" | "prompt.replace_queued" => vec![
            json!({"session_id": s, "admission_id": "a1", "text": "t", "submission_id": "n1"}),
            json!({"session_id": s}),
        ],
        _ => vec![],
    }
}

/// Names that are not served: `-32601`, whatever the params.
const UNSERVED: &[&str] = &[
    "nope.nothing",
    "Gateway.Ping",
    "gateway.ping ",
    " gateway.ping",
    "",
    "session.save",
    "terminal.resize",
    "voice.toggle",
    "config.show",
    "session.list",
    "gateway.PING",
    "event",
];

/// Ids, long numbers and temp names made stable.
fn clean(text: &str, dir: &str) -> String {
    let text = text
        .replace(dir, "<DIR>")
        .replace(&dir.replace('\\', "\\\\"), "<DIR>")
        .replace(&dir.replace('\\', "/"), "<DIR>");
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    while i < chars.len() {
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_hexdigit() || chars[j] == '-') {
            j += 1;
        }
        let run: String = chars[i..j].iter().collect();
        let digits = !run.is_empty() && run.chars().all(|c| c.is_ascii_digit());
        if run.len() >= 10 && digits {
            out.push_str("<N>");
            i = j;
        } else if run.len() >= 12 && run.chars().any(|c| c.is_ascii_alphabetic() || c == '-') {
            out.push_str("<ID>");
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// A success as the table keeps it: a small result in full, else its top-level keys.
fn shape(value: &Value, in_full: bool) -> String {
    let full = value.to_string();
    if in_full && full.len() <= 80 {
        return format!("ok {full}");
    }
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            format!("ok {{{}}}", keys.join(","))
        }
        Value::Array(items) => format!("ok [{} items]", items.len()),
        other => format!(
            "ok {}",
            match other {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                _ => "string",
            }
        ),
    }
}

async fn record() -> String {
    let data = hermes_testdir::tempdir().unwrap();
    let dir = data.path().display().to_string();
    std::fs::write(
        data.path().join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","base_url":"http://127.0.0.1:1/v1"}],"provider":"mock","model":"mock","background_review":false,"checkpoints":false}"#,
    )
    .unwrap();
    let config = hermes_runtime::RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..hermes_runtime::RuntimeConfig::for_current_user().unwrap()
    };
    let hub = Arc::new(Hub::default());
    let runtime = Arc::new(
        Runtime::new(
            config,
            Arc::new(crate::hub::DesktopBridge { hub: hub.clone() }),
        )
        .unwrap(),
    );
    let gateway = Arc::new(Gateway::new_for_acp(
        runtime,
        hub,
        data.path().to_path_buf(),
    ));
    let mut out = String::new();
    // Last: these two end what the others still read.
    let mut methods: Vec<&str> = SERVED.to_vec();
    methods.sort_by_key(|m| matches!(*m, "session.delete" | "projects.delete"));
    for method in methods.iter().copied().chain(UNSERVED.iter().copied()) {
        let mut cases = spread();
        cases.extend(shaped(method));
        for case in cases {
            let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
            let answer = gateway.clone().call(method, case.clone(), &outbox).await;
            let line = match answer {
                Ok(value) => shape(
                    &value,
                    !method.starts_with("session.foreign") && method != "session.create",
                ),
                Err((code, message)) => format!("err {code} {message}"),
            };
            let shown: String = case.to_string().chars().take(70).collect();
            out.push_str(&clean(&format!("{method} {shown} -> {line}\n"), &dir));
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_method_answers_as_the_table_says() {
    let record = record().await;
    if std::env::var_os("HERMES_RPC_TABLE_BLESS").is_some() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rpc_table.txt");
        std::fs::write(path, &record).unwrap();
        return;
    }
    let (want, got): (Vec<&str>, Vec<&str>) = (FIXTURE.lines().collect(), record.lines().collect());
    let differing: Vec<String> = want
        .iter()
        .zip(&got)
        .filter(|(w, g)| w != g)
        .take(8)
        .map(|(w, g)| format!("table: {w}\n  now: {g}"))
        .collect();
    assert!(
        differing.is_empty() && want.len() == got.len(),
        "{} lines in the table, {} now; first differences:\n{}",
        want.len(),
        got.len(),
        differing.join("\n")
    );
}

#[tokio::test]
async fn names_that_are_not_served_are_method_not_found() {
    // Through the whole `call`: the code and the text of the old `_` arm.
    let data = hermes_testdir::tempdir().unwrap();
    let config = hermes_runtime::RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..hermes_runtime::RuntimeConfig::for_current_user().unwrap()
    };
    let hub = Arc::new(Hub::default());
    let runtime = Arc::new(
        Runtime::new(
            config,
            Arc::new(crate::hub::DesktopBridge { hub: hub.clone() }),
        )
        .unwrap(),
    );
    let gateway = Arc::new(Gateway::new_for_acp(
        runtime,
        hub,
        data.path().to_path_buf(),
    ));
    for name in UNSERVED {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        for params in [Value::Null, json!({"session_id": "s"}), json!([1])] {
            let error = gateway
                .clone()
                .call(name, params, &outbox)
                .await
                .unwrap_err();
            assert_eq!(
                error,
                (-32601, format!("Method not found: {name}")),
                "{name:?}"
            );
        }
    }
}
