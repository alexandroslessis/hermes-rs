//! A recorded turn: the events a mock provider's turn sent through the real core, kept as a test
//! fixture (`crates/native/fixtures/thread-session.json`) so the gallery, the snapshots and the
//! tests replay a real turn without a provider: a prompt, reasoning, a reply, a call of every
//! tool kind and its result (a command with coloured output, a read, a search, a patch and a
//! written file, a blocked web fetch, a memory write, a skill write, a delegated task with its
//! subagent's events, a call that fails), the final markdown reply (a code block, a table, a GitHub
//! alert, a file link), usage, the turn's end and the session settling. Only the kinds of event the
//! thread view takes in are kept ([`Recorded`]); titles are the sidebar's and the header's.
//! Live command output (`ToolProgress`) is throttled by the clock, so it is not recorded: the
//! tests make their own.
//!
//! The recording is made by `the_recorded_session_is_what_the_real_core_sends` below: a real
//! `Core` on a `hermes_testdir` folder, a `wiremock` provider streaming fixed chunks, a prompt sent
//! through `Core::submit`. It fails when what the core sends no longer matches the file;
//! `UPDATE_FIXTURES=1` writes the file again.
//!
//! Hermes: none (the event kinds are `hermes_core::events`).

use std::time::Duration;

use hermes_core::{CoreEvent, Outcome, TurnEnd, transcript::PersistedTurn};
use hermes_runtime::TurnStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The time between two replayed events (the fixture app's replay, and the durations a replay
/// into a model measures).
pub const REPLAY_STEP: Duration = Duration::from_millis(120);

/// One event of a recorded turn, without its session (a replay names the session).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recorded {
    TurnStarted,
    Reasoning {
        text: String,
    },
    Text {
        text: String,
    },
    ToolStarted {
        tool: String,
        name: String,
        args: Value,
    },
    ToolProgress {
        tool: String,
        name: String,
        message: String,
    },
    ToolCompleted {
        tool: String,
        name: String,
        text: String,
        /// The core's error flag (`CoreEvent::ToolCompleted::is_error`).
        #[serde(default)]
        is_error: bool,
    },
    Status {
        status: String,
        text: String,
    },
    /// A `delegate_task` child's activity (`kind`: `subagent.start`, `subagent.tool`...).
    Subagent {
        phase: String,
        payload: Value,
    },
    /// A model call's token counts.
    Usage {
        input: Option<i64>,
        output: Option<i64>,
    },
    /// `outcome`: `complete`, `interrupted` or `error`. `user_row` / `last_row`: the stored rows
    /// of the turn's typed prompt and last reply (what Retry and Branch name).
    TurnEnded {
        outcome: String,
        error: Option<String>,
        text: String,
        #[serde(default)]
        user_row: Option<usize>,
        #[serde(default)]
        last_row: Option<usize>,
    },
    Idle,
}

/// A prompt and the events its turn sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub prompt: String,
    pub events: Vec<Recorded>,
}

/// The fixture file, compiled in.
const FIXTURE: &str = include_str!("../../fixtures/thread-session.json");

impl Recording {
    /// The recorded session of `crates/native/fixtures/thread-session.json`.
    pub fn fixture() -> Self {
        // The file is ours and checked by the recorder test; a broken one is a build mistake.
        serde_json::from_str(FIXTURE).expect("fixtures/thread-session.json is a recording")
    }

    /// The core events of the recording, as they would arrive for `session`.
    pub fn events_for(&self, session: &str) -> Vec<CoreEvent> {
        self.events.iter().map(|e| e.to_core(session)).collect()
    }

    /// A slice of the turn for a story of a card: only the calls named in `tools` (`t0`, `t1`...:
    /// their events, and a delegated task's subagent events when it is one of them), after the
    /// turn's start; with `with_end`, the usage and the end of the turn too. No reasoning or text.
    pub fn only_calls(&self, tools: &[&str], with_end: bool) -> Self {
        let delegated = self.events.iter().any(|e| {
            matches!(e, Recorded::ToolStarted { tool, name, .. }
                if name == "delegate_task" && tools.contains(&tool.as_str()))
        });
        let events = self
            .events
            .iter()
            .filter(|e| match e {
                Recorded::TurnStarted => true,
                Recorded::ToolStarted { tool, .. }
                | Recorded::ToolProgress { tool, .. }
                | Recorded::ToolCompleted { tool, .. } => tools.contains(&tool.as_str()),
                Recorded::Subagent { .. } => delegated,
                Recorded::Usage { .. } | Recorded::TurnEnded { .. } | Recorded::Idle => with_end,
                _ => false,
            })
            .cloned()
            .collect();
        Self {
            prompt: self.prompt.clone(),
            events,
        }
    }

    /// The first `n` events only (a turn caught mid-stream).
    pub fn truncated(&self, n: usize) -> Self {
        Self {
            prompt: self.prompt.clone(),
            events: self.events.iter().take(n).cloned().collect(),
        }
    }
}

impl Recorded {
    /// What the core published, for a session's recording; `None` for kinds the thread view does
    /// not take in. `tool_ids` names the core's tool call ids `t0`, `t1`... in order, so the file
    /// does not change from run to run.
    pub fn from_core(event: &CoreEvent, tool_ids: &mut Vec<String>) -> Option<Self> {
        let mut tool = |id: &str| match tool_ids.iter().position(|t| t == id) {
            Some(ix) => format!("t{ix}"),
            None => {
                tool_ids.push(id.to_string());
                format!("t{}", tool_ids.len() - 1)
            }
        };
        Some(match event {
            CoreEvent::TurnStarted { .. } => Recorded::TurnStarted,
            CoreEvent::ReasoningDelta { text, .. } => Recorded::Reasoning { text: text.clone() },
            CoreEvent::TextDelta { text, .. } => Recorded::Text { text: text.clone() },
            CoreEvent::ToolStarted {
                tool_id,
                name,
                args,
                ..
            } => Recorded::ToolStarted {
                tool: tool(tool_id),
                name: name.clone(),
                args: args.clone(),
            },
            CoreEvent::ToolProgress {
                tool_id,
                name,
                message,
                ..
            } => Recorded::ToolProgress {
                tool: tool(tool_id),
                name: name.clone(),
                message: message.clone(),
            },
            CoreEvent::ToolCompleted {
                tool_id,
                name,
                text,
                is_error,
                ..
            } => Recorded::ToolCompleted {
                tool: tool(tool_id),
                name: name.clone(),
                text: text.clone(),
                is_error: *is_error,
            },
            CoreEvent::Status { kind, text, .. } => Recorded::Status {
                status: kind.clone(),
                text: text.clone(),
            },
            CoreEvent::Subagent { kind, payload, .. } => Recorded::Subagent {
                phase: (*kind).to_string(),
                payload: payload.clone(),
            },
            CoreEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => Recorded::Usage {
                input: *input_tokens,
                output: *output_tokens,
            },
            CoreEvent::TurnEnded { end, .. } => Recorded::TurnEnded {
                outcome: match end.outcome {
                    Outcome::Complete => "complete",
                    Outcome::Interrupted => "interrupted",
                    Outcome::Error => "error",
                }
                .to_string(),
                error: end.error.clone(),
                text: end.text.clone(),
                user_row: end.persisted.user_row_ids.first().copied(),
                last_row: end.persisted.final_assistant_row_id,
            },
            CoreEvent::Idle { .. } => Recorded::Idle,
            _ => return None,
        })
    }

    /// The core event this stands for, in `session`.
    pub fn to_core(&self, session: &str) -> CoreEvent {
        let session_id = session.to_string();
        match self {
            Recorded::TurnStarted => CoreEvent::TurnStarted {
                session_id,
                admission_id: "replay".into(),
            },
            Recorded::Reasoning { text } => CoreEvent::ReasoningDelta {
                session_id,
                text: text.clone(),
            },
            Recorded::Text { text } => CoreEvent::TextDelta {
                session_id,
                text: text.clone(),
            },
            Recorded::ToolStarted { tool, name, args } => CoreEvent::ToolStarted {
                session_id,
                tool_id: tool.clone(),
                name: name.clone(),
                args: args.clone(),
            },
            Recorded::ToolProgress {
                tool,
                name,
                message,
            } => CoreEvent::ToolProgress {
                session_id,
                tool_id: tool.clone(),
                name: name.clone(),
                progress: 0.,
                message: message.clone(),
            },
            Recorded::ToolCompleted {
                tool,
                name,
                text,
                is_error,
            } => CoreEvent::ToolCompleted {
                session_id,
                tool_id: tool.clone(),
                name: name.clone(),
                text: text.clone(),
                is_error: *is_error,
            },
            Recorded::Status { status, text } => CoreEvent::Status {
                session_id,
                kind: status.clone(),
                text: text.clone(),
            },
            Recorded::Subagent { phase, payload } => CoreEvent::Subagent {
                session_id,
                kind: match phase.as_str() {
                    "subagent.start" => "subagent.start",
                    "subagent.tool" => "subagent.tool",
                    "subagent.thinking" => "subagent.thinking",
                    "subagent.complete" => "subagent.complete",
                    _ => "subagent.progress",
                },
                payload: payload.clone(),
            },
            Recorded::Usage { input, output } => CoreEvent::Usage {
                session_id,
                input_tokens: *input,
                output_tokens: *output,
            },
            Recorded::TurnEnded {
                outcome,
                error,
                text,
                user_row,
                last_row,
            } => {
                let (status, outcome) = match outcome.as_str() {
                    "interrupted" => (TurnStatus::Cancelled, Outcome::Interrupted),
                    "error" => (
                        TurnStatus::Failed(error.clone().unwrap_or_default()),
                        Outcome::Error,
                    ),
                    _ => (TurnStatus::Completed, Outcome::Complete),
                };
                CoreEvent::TurnEnded {
                    session_id: session_id.clone(),
                    status: status.clone(),
                    end: Box::new(TurnEnd {
                        session_id,
                        admission_id: "replay".into(),
                        status,
                        outcome,
                        error: error.clone(),
                        failure: None,
                        failed_provider: String::new(),
                        failed_model: String::new(),
                        text: text.clone(),
                        partial: false,
                        persisted: PersistedTurn {
                            row_ids: Vec::new(),
                            user_row_ids: user_row.iter().copied().collect(),
                            final_assistant_row_id: *last_row,
                        },
                    }),
                }
            }
            Recorded::Idle => CoreEvent::Idle { session_id },
        }
    }
}

#[cfg(test)]
mod tests {
    //! The recorder: a real core and a mock provider make the fixture's events.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hermes_core::{Core, NewSession, NoHooks, SubmitTurn};
    use hermes_runtime::{ProviderConfig, RuntimeConfig};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;
    use crate::thread::tools::ToolCard;

    /// The prompt of the recording.
    const PROMPT: &str = "Read notes.md and sketch the fix, with a table of the timings.";

    /// The final reply, in the chunks the mock streams it in: a paragraph with bold text, a
    /// GitHub alert, fenced Python, a table, a file link and a web link, a list.
    const FINAL: [&str; 9] = [
        "Here is what **notes.md** says, ",
        "with a sketch of the fix.\n\n",
        "> [!NOTE]\n> The parser keeps the last block open while text streams in.\n\n",
        "```python\ndef primes_up_to(n: int) -> list[int]:\n    sieve = bytearray([1]) * (n + 1)\n",
        "    sieve[0:2] = b\"\\x00\\x00\"\n    for i in range(2, int(n**0.5) + 1):\n        if sieve[i]:\n            sieve[i * i :: i] = b\"\\x00\" * len(sieve[i * i :: i])\n    return [i for i, p in enumerate(sieve) if p]\n```\n\n",
        "| Step | Before | After |\n|---|---|---|\n| parse | 41 ms | 3 ms |\n",
        "| layout | 12 ms | 11 ms |\n\n",
        "See [src/stream.rs](src/stream.rs#L42) for where deltas land, and [the plan](https://example.com/plan).\n\n",
        "- keep the tail open\n- reparse only the last block\n",
    ];

    const SKILL: &str = "---\nname: notes-fix\ndescription: Use when changing how notes are parsed.\n---\n# Notes fix\nKeep the tail open.\n";

    /// The calls the mock model makes, one per request, in order: a command with coloured output,
    /// a read, a search, a patch, a new file, a memory write, a skill write, a web fetch (blocked: it
    /// names a local address), a delegated task, and a read of a file that is not there.
    fn calls() -> Vec<(&'static str, Value)> {
        vec![
            (
                "terminal",
                json!({"command": "printf '\\033[32mpass\\033[0m 12 tests\n\\033[31mfail\\033[0m 1 test\n'"}),
            ),
            (
                "terminal",
                json!({"command": "echo 'error: expected 12 got 11' >&2; false"}),
            ),
            ("read_file", json!({"path": "notes.md"})),
            ("search_files", json!({"pattern": "Streaming", "path": "."})),
            (
                "patch",
                json!({"path": "notes.md",
                       "old_string": "Streaming re-parses the whole message on every delta.",
                       "new_string": "Streaming re-parses only the last block on every delta.\nThe earlier blocks keep their parse."}),
            ),
            (
                "write_file",
                json!({"path": "docs/plan.md", "content": "# Plan\n\n1. Keep the tail open.\n2. Re-parse the last block only.\n"}),
            ),
            (
                "memory",
                json!({"target": "memory", "operations": [{"action": "add", "content": "Notes about streaming live in notes.md."}]}),
            ),
            (
                "skill_manage",
                json!({"operations": [{"action": "create", "name": "notes-fix", "content": SKILL}]}),
            ),
            (
                "web_extract",
                json!({"urls": ["http://127.0.0.1:9/timings"]}),
            ),
            (
                "delegate_task",
                json!({"tasks": [{"goal": "Find where text deltas land in the thread view", "context": "Look in notes.md"}]}),
            ),
            ("read_file", json!({"path": "missing.md"})),
        ]
    }

    /// Request `n` of the parent: the first has reasoning and a few words before its call; the
    /// last one is the final reply; a child's requests are told apart by their prompt and answered
    /// on their own count (a read, then a summary); any later one (a title): one word.
    struct Turn {
        parent: AtomicUsize,
        child: AtomicUsize,
    }

    fn sse(chunks: &[Value]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("data: {chunk}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn delta(n: usize, delta: Value, finish: Option<&str>) -> Value {
        json!({"id": format!("r{n}"), "object": "chat.completion.chunk", "model": "mock",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    fn call_chunk(n: usize, name: &str, args: &Value) -> Value {
        delta(
            n,
            json!({"tool_calls": [{"index": 0, "id": format!("call_{n}"), "type": "function",
                "function": {"name": name, "arguments": args.to_string()}}]}),
            Some("tool_calls"),
        )
    }

    impl Respond for Turn {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = String::from_utf8_lossy(&request.body);
            let is_child = body.contains("You are a focused subagent");
            let n = if is_child {
                1000 + self.child.fetch_add(1, Ordering::SeqCst)
            } else {
                self.parent.fetch_add(1, Ordering::SeqCst)
            };
            let calls = calls();
            let mut chunks = vec![delta(n, json!({"role": "assistant", "content": ""}), None)];
            if n == 1000 {
                chunks.push(call_chunk(n, "read_file", &json!({"path": "notes.md"})));
            } else if n == 1001 {
                chunks.push(delta(
                    n,
                    json!({"content": "The deltas land in the streaming row's TextViewState."}),
                    Some("stop"),
                ));
            } else if n < calls.len() {
                if n == 0 {
                    for part in ["The user wants ", "the notes first, ", "then a fix."] {
                        chunks.push(delta(n, json!({"reasoning_content": part}), None));
                    }
                    for part in ["Let me look ", "around."] {
                        chunks.push(delta(n, json!({"content": part}), None));
                    }
                }
                chunks.push(call_chunk(n, calls[n].0, &calls[n].1));
            } else if n == calls.len() {
                for part in FINAL {
                    chunks.push(delta(n, json!({"content": part}), None));
                }
                chunks.push(delta(n, json!({}), Some("stop")));
            } else {
                chunks.push(delta(n, json!({"content": "Notes"}), Some("stop")));
            }
            chunks.push(
                json!({"id": format!("r{n}"), "object": "chat.completion.chunk",
                "model": "mock", "choices": [],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}}),
            );
            ResponseTemplate::new(200).set_body_raw(sse(&chunks), "text/event-stream")
        }
    }

    fn fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/thread-session.json")
    }

    /// What differs from run to run (times, ids, the temporary folders) set to fixed values.
    fn scrub(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    match key.as_str() {
                        "duration_ms" => *v = json!(150),
                        "duration_seconds" | "total_duration_seconds" => *v = json!(1.5),
                        "subagent_id" => *v = json!("sa-0-00000000"),
                        "delegation_id" | "request_id" => *v = json!("delegation"),
                        _ => scrub(v),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(scrub),
            _ => {}
        }
    }

    fn scrub_text(text: &str, folders: &[(String, &str)]) -> String {
        let mut text = text.to_string();
        for (folder, name) in folders {
            for spelling in [
                folder.clone(),
                folder.replace('\\', "/"),
                folder.replace('\\', "\\\\"),
            ] {
                text = text.replace(&spelling, name);
            }
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(mut value) => {
                scrub(&mut value);
                value.to_string()
            }
            Err(_) => text,
        }
    }

    /// The turn, run through a real core: the events the thread view takes in, and the tool cards
    /// as the live events build them next to the ones the stored transcript builds.
    struct Run {
        recording: Recording,
        live: Vec<ToolCard>,
        stored: Vec<ToolCard>,
    }

    fn run_scenario() -> Run {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let data = hermes_testdir::tempdir().unwrap();
        let work = hermes_testdir::tempdir().unwrap();
        std::fs::write(
            work.path().join("notes.md"),
            "# Notes\n\nStreaming re-parses the whole message on every delta.\n",
        )
        .unwrap();
        let folders = vec![
            (work.path().display().to_string(), "<work>"),
            (data.path().display().to_string(), "<data>"),
        ];
        let run = runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(Turn {
                    parent: AtomicUsize::new(0),
                    child: AtomicUsize::new(0),
                })
                .mount(&server)
                .await;
            let config = RuntimeConfig {
                data_dir: data.path().to_path_buf(),
                ..RuntimeConfig::for_current_user().unwrap()
            };
            let core = Core::new(config, tokio::runtime::Handle::current()).unwrap();
            let provider = ProviderConfig {
                id: "mock".into(),
                base_url: format!("{}/v1", server.uri()),
                api_key: Some("key".into()),
            };
            let (record, _) = core
                .create_session(NewSession {
                    provider: provider.clone(),
                    model: "mock".into(),
                    cwd: work.path().to_path_buf(),
                    title: Some("Recorded".into()),
                    seeded: vec![],
                    parent: None,
                })
                .unwrap();
            // The mock's commands run without asking (nobody is there to answer).
            core.runtime()
                .set_approval_mode(&record.id, hermes_tools::ApprovalMode::Yolo)
                .unwrap();
            let mut events = core.subscribe();
            let hooks = Arc::new(NoHooks {
                provider: Some(provider),
                options: Default::default(),
            });
            let (submit_core, id) = (core.clone(), record.id.clone());
            tokio::task::spawn_blocking(move || {
                submit_core.submit(
                    &id,
                    SubmitTurn {
                        text: PROMPT.into(),
                        ..SubmitTurn::default()
                    },
                    hooks,
                )
            })
            .await
            .unwrap()
            .unwrap();
            let mut recorded = Vec::new();
            let mut tool_ids = Vec::new();
            let mut live: Vec<(String, ToolCard)> = Vec::new();
            let wait = async {
                while let Some(event) = events.recv().await {
                    if event.session_id() != record.id {
                        continue;
                    }
                    let idle = matches!(event, CoreEvent::Idle { .. });
                    match &event {
                        CoreEvent::ToolStarted {
                            tool_id,
                            name,
                            args,
                            ..
                        } => live.push((tool_id.clone(), ToolCard::started(name, args))),
                        CoreEvent::ToolCompleted {
                            tool_id,
                            text,
                            is_error,
                            ..
                        } => {
                            if let Some((_, card)) = live.iter_mut().find(|(id, _)| id == tool_id) {
                                card.finish(text, *is_error);
                            }
                        }
                        _ => {}
                    }
                    if matches!(event, CoreEvent::ToolProgress { .. }) {
                        continue;
                    }
                    if let Some(mut one) = Recorded::from_core(&event, &mut tool_ids) {
                        match &mut one {
                            Recorded::ToolCompleted { text, .. } => {
                                *text = scrub_text(text, &folders)
                            }
                            Recorded::ToolStarted { args, .. } => {
                                *args =
                                    serde_json::from_str(&scrub_text(&args.to_string(), &folders))
                                        .unwrap();
                            }
                            Recorded::Subagent { payload, .. } => scrub(payload),
                            _ => {}
                        }
                        recorded.push(one);
                    }
                    if idle {
                        break;
                    }
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(120), wait)
                .await
                .expect("the turn's events within two minutes");
            let messages = core.runtime().store().load_messages(&record.id).unwrap();
            let stored: Vec<ToolCard> = hermes_core::transcript::entries(&messages)
                .into_iter()
                .filter_map(|entry| match entry {
                    hermes_core::transcript::Entry::Tool(tool) => Some(ToolCard::from_stored(
                        &tool.name,
                        &tool.args,
                        tool.result.as_deref(),
                        tool.is_error,
                    )),
                    _ => None,
                })
                .collect();
            drop(core);
            drop(server);
            Run {
                recording: Recording {
                    prompt: PROMPT.into(),
                    events: recorded,
                },
                live: live.into_iter().map(|(_, card)| card).collect(),
                stored,
            }
        });
        drop(runtime);
        run
    }

    #[core::prelude::v1::test]
    fn the_recorded_session_is_what_the_real_core_sends() {
        let Run {
            recording,
            live,
            stored,
        } = run_scenario();

        // Every kind the thread view takes in from a turn like this one is there, and every
        // tool of the scenario ran once.
        let has = |f: fn(&Recorded) -> bool| recording.events.iter().any(f);
        assert!(has(|e| matches!(e, Recorded::TurnStarted)));
        assert!(has(|e| matches!(e, Recorded::Reasoning { .. })));
        assert!(has(|e| matches!(e, Recorded::Text { .. })));
        assert!(has(|e| matches!(e, Recorded::Usage { .. })));
        assert!(has(
            |e| matches!(e, Recorded::Subagent { phase, .. } if phase == "subagent.start")
        ));
        assert!(has(
            |e| matches!(e, Recorded::Subagent { phase, .. } if phase == "subagent.tool")
        ));
        assert!(has(
            |e| matches!(e, Recorded::Subagent { phase, .. } if phase == "subagent.complete")
        ));
        assert!(has(|e| matches!(
            e,
            Recorded::TurnEnded {
                user_row: Some(_),
                last_row: Some(_),
                ..
            }
        )));
        assert!(matches!(recording.events.last(), Some(Recorded::Idle)));
        let names: Vec<&str> = recording
            .events
            .iter()
            .filter_map(|e| match e {
                Recorded::ToolStarted { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let wanted: Vec<&str> = calls().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, wanted);
        let completed = recording
            .events
            .iter()
            .filter(|e| matches!(e, Recorded::ToolCompleted { .. }))
            .count();
        assert_eq!(completed, wanted.len());

        // The cards the live events build are the ones the stored transcript builds.
        assert_eq!(live.len(), wanted.len());
        assert_eq!(
            live, stored,
            "a live card and its stored twin are the same card"
        );
        // Every kind of card is among them, and the failing ones failed.
        use super::super::tools::{Status, ToolKind};
        let kinds: Vec<ToolKind> = live.iter().map(|c| c.kind).collect();
        for kind in [
            ToolKind::Command,
            ToolKind::Edit,
            ToolKind::Read,
            ToolKind::Search,
            ToolKind::Web,
            ToolKind::Delegate,
            ToolKind::Memory,
            ToolKind::Skill,
        ] {
            assert!(kinds.contains(&kind), "{kind:?}");
        }
        let status: Vec<(&str, Status)> =
            live.iter().map(|c| (c.name.as_str(), c.status)).collect();
        assert_eq!(status[0], ("terminal", Status::Done), "{status:?}");
        assert_eq!(
            status.last().unwrap(),
            &("read_file", Status::Failed),
            "{status:?}"
        );
        assert!(
            status.contains(&("web_extract", Status::Failed)),
            "the blocked fetch fails: {status:?}"
        );
        // And nothing else did (no false failure: the command that exits 1 ran, it shows its
        // exit code; the core flagged exactly the two failed calls).
        let failed = status.iter().filter(|(_, s)| *s == Status::Failed).count();
        assert_eq!(failed, 2, "{status:?}");
        let flagged = recording
            .events
            .iter()
            .filter(|e| matches!(e, Recorded::ToolCompleted { is_error: true, .. }))
            .count();
        assert!(flagged >= 1, "the core's error flag is recorded");

        let file = fixture_path();
        let written = serde_json::to_string_pretty(&recording).unwrap() + "\n";
        let update = std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1");
        if update {
            std::fs::write(&file, &written).unwrap();
            println!("fixture-written: {}", file.display());
            return;
        }
        let stored = Recording::fixture();
        assert_eq!(
            stored,
            recording,
            "the core's events differ from {}: look at the difference, then accept with \
             UPDATE_FIXTURES=1",
            file.display()
        );
    }

    #[core::prelude::v1::test]
    fn a_recording_replays_as_the_core_events_it_was_made_from() {
        let recording = Recording::fixture();
        let events = recording.events_for("s1");
        assert_eq!(events.len(), recording.events.len());
        let mut ids = Vec::new();
        let again: Vec<Recorded> = events
            .iter()
            .filter_map(|e| Recorded::from_core(e, &mut ids))
            .collect();
        assert_eq!(again, recording.events);
        assert!(events.iter().all(|e| e.session_id() == "s1"));
    }
}
