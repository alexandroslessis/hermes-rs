//! A recorded turn: the events a mock provider's turn sent through the real core, kept as a test
//! fixture (`crates/native/fixtures/thread-session.json`) so the gallery, the snapshots and the
//! tests replay a real turn without a provider: a prompt, reasoning, a reply, a tool call and its
//! result, the final markdown reply (a code block, a table, a GitHub alert, a file link), the
//! turn's end and the session settling. Only the kinds of event the thread view takes in are
//! kept ([`Recorded`]); usage and titles are the sidebar's and the header's.
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
    },
    Status {
        status: String,
        text: String,
    },
    /// `outcome`: `complete`, `interrupted` or `error`.
    TurnEnded {
        outcome: String,
        error: Option<String>,
        text: String,
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
                ..
            } => Recorded::ToolCompleted {
                tool: tool(tool_id),
                name: name.clone(),
                text: text.clone(),
            },
            CoreEvent::Status { kind, text, .. } => Recorded::Status {
                status: kind.clone(),
                text: text.clone(),
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
            Recorded::ToolCompleted { tool, name, text } => CoreEvent::ToolCompleted {
                session_id,
                tool_id: tool.clone(),
                name: name.clone(),
                text: text.clone(),
            },
            Recorded::Status { status, text } => CoreEvent::Status {
                session_id,
                kind: status.clone(),
                text: text.clone(),
            },
            Recorded::TurnEnded {
                outcome,
                error,
                text,
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
                            user_row_ids: Vec::new(),
                            final_assistant_row_id: None,
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

    /// Request 0: reasoning, a short reply and a `read_file` call; request 1: the final reply in
    /// chunks; any later one (a title): one word.
    struct Turn(AtomicUsize);

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

    impl Respond for Turn {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let mut chunks = vec![delta(n, json!({"role": "assistant", "content": ""}), None)];
            match n {
                0 => {
                    for part in ["The user wants ", "the notes first, ", "then a fix."] {
                        chunks.push(delta(n, json!({"reasoning_content": part}), None));
                    }
                    for part in ["Let me read ", "the notes."] {
                        chunks.push(delta(n, json!({"content": part}), None));
                    }
                    chunks.push(delta(
                        n,
                        json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                            "function": {"name": "read_file",
                                         "arguments": "{\"path\": \"notes.md\"}"}}]}),
                        Some("tool_calls"),
                    ));
                }
                1 => {
                    for part in FINAL {
                        chunks.push(delta(n, json!({"content": part}), None));
                    }
                    chunks.push(delta(n, json!({}), Some("stop")));
                }
                _ => chunks.push(delta(n, json!({"content": "Notes"}), Some("stop"))),
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

    #[core::prelude::v1::test]
    fn the_recorded_session_is_what_the_real_core_sends() {
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
        let recording = runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(Turn(AtomicUsize::new(0)))
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
            let work_path = work.path().display().to_string();
            let wait = async {
                while let Some(event) = events.recv().await {
                    if event.session_id() != record.id {
                        continue;
                    }
                    let idle = matches!(event, CoreEvent::Idle { .. });
                    if let Some(mut one) = Recorded::from_core(&event, &mut tool_ids) {
                        // The temporary folder's name changes every run.
                        if let Recorded::ToolCompleted { text, .. } = &mut one {
                            *text = text.replace(&work_path, "<work>");
                        }
                        recorded.push(one);
                    }
                    if idle {
                        break;
                    }
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(60), wait)
                .await
                .expect("the turn's events within a minute");
            drop(core);
            drop(server);
            Recording {
                prompt: PROMPT.into(),
                events: recorded,
            }
        });
        drop(runtime);

        // Every kind the thread view takes in from a turn like this one is there.
        let has = |f: fn(&Recorded) -> bool| recording.events.iter().any(f);
        assert!(has(|e| matches!(e, Recorded::TurnStarted)));
        assert!(has(|e| matches!(e, Recorded::Reasoning { .. })));
        assert!(has(|e| matches!(e, Recorded::Text { .. })));
        assert!(has(|e| matches!(e, Recorded::ToolStarted { .. })));
        assert!(has(|e| matches!(e, Recorded::ToolCompleted { .. })));
        assert!(has(|e| matches!(e, Recorded::TurnEnded { .. })));
        assert!(matches!(recording.events.last(), Some(Recorded::Idle)));

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
