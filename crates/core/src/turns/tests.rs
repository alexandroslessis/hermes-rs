//! Turns over a real runtime and a mock provider, with no gateway: the core alone streams the
//! turn, queues behind it, runs the queue and stops.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermes_runtime::{ProviderConfig, RuntimeConfig};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::NewSession;

/// Answers every chat request with `reply-<n>`; the first one only once the test releases it
/// (sends on, or drops, the fixture's `release`): the first turn runs for as long as the test
/// needs, with no timing assumed. (Blocking here stalls the mock server's own thread, which has
/// nothing else to answer until the first reply goes out.)
struct Replies {
    count: AtomicUsize,
    hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Respond for Replies {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        if n == 0
            && let Some(hold) = self.hold.lock().unwrap().take()
        {
            let _ = hold.recv_timeout(Duration::from_secs(60));
        }
        let chunk = |delta: Value, finish: Option<&str>| {
            json!({"id": format!("r{n}"), "object": "chat.completion.chunk", "model": "mock",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        };
        let usage = json!({"id": format!("r{n}"), "object": "chat.completion.chunk",
            "model": "mock", "choices": [],
            "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}});
        let body = format!(
            "data: {}\n\ndata: {usage}\n\ndata: [DONE]\n\n",
            chunk(
                json!({"role": "assistant", "content": format!("reply-{n}")}),
                Some("stop")
            ),
        );
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }
}

struct Fixture {
    core: Arc<Core>,
    events: UnboundedReceiver<CoreEvent>,
    hooks: Arc<NoHooks>,
    session: String,
    /// Lets the first reply go (dropped before the server, so a held reply never outlives it).
    release: Option<std::sync::mpsc::Sender<()>>,
    _server: MockServer,
    _work: hermes_testdir::TestDir,
    // Last: the runtime above holds files in it until it drops.
    _data: hermes_testdir::TestDir,
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let (release, hold) = std::sync::mpsc::channel();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Replies {
            count: AtomicUsize::new(0),
            hold: Mutex::new(Some(hold)),
        })
        .mount(&server)
        .await;
    let data = hermes_testdir::tempdir().unwrap();
    let work = hermes_testdir::tempdir().unwrap();
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
            title: Some("Queue".into()),
            seeded: vec![],
            parent: None,
        })
        .unwrap();
    let events = core.subscribe();
    Fixture {
        core,
        events,
        hooks: Arc::new(NoHooks {
            provider: Some(provider),
            options: Default::default(),
        }),
        session: record.id,
        release: Some(release),
        _server: server,
        _work: work,
        _data: data,
    }
}

impl Fixture {
    /// Submits on a blocking thread, as a front end must.
    async fn submit(&self, turn: SubmitTurn) -> Result<Submitted, SubmitError> {
        let (core, id, hooks) = (self.core.clone(), self.session.clone(), self.hooks.clone());
        tokio::task::spawn_blocking(move || core.submit(&id, turn, hooks))
            .await
            .unwrap()
    }

    /// The events up to (and with) the first that `done` accepts.
    async fn until(&mut self, done: impl Fn(&CoreEvent) -> bool) -> Vec<CoreEvent> {
        let mut seen = Vec::new();
        let events = &mut self.events;
        let wait = async {
            while let Some(event) = events.recv().await {
                let last = done(&event);
                seen.push(event);
                if last {
                    break;
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .is_err()
        {
            panic!("timed out waiting for the turn's events");
        }
        seen
    }
}

fn prompt(text: &str) -> SubmitTurn {
    SubmitTurn {
        text: text.into(),
        ..SubmitTurn::default()
    }
}

fn texts(events: &[CoreEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            CoreEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn ended(events: &[CoreEvent]) -> Vec<(Outcome, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            CoreEvent::TurnEnded { end, .. } => Some((end.outcome, end.admission_id.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prompt_sent_while_a_turn_runs_queues_and_runs_next_with_no_gateway() {
    let mut f = fixture().await;
    let first = f
        .submit(SubmitTurn {
            admission_id: Some("a1".into()),
            ..prompt("first")
        })
        .await
        .unwrap();
    assert_eq!(
        first,
        Submitted::Started {
            admission_id: "a1".into()
        }
    );
    let second = f.submit(prompt("second")).await.unwrap();
    let Submitted::Queued { admission_id } = second else {
        panic!("not queued: {second:?}");
    };
    let queued = f.core.queue(&f.session);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].text, "second");
    assert_eq!(
        queued[0].admission_id.as_deref(),
        Some(admission_id.as_str())
    );
    // A cut can't wait behind the running turn.
    let cut = SubmitTurn {
        truncate: Some(Truncate::Nothing),
        ..prompt("again")
    };
    assert_eq!(f.submit(cut).await, Err(SubmitError::Busy));
    // The first reply goes now.
    f.release.take();

    let events = f.until(|e| matches!(e, CoreEvent::Idle { .. })).await;
    // Both turns streamed through the core alone, in order, the queued one under its admission.
    let started: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::TurnStarted { admission_id, .. } => Some(admission_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["a1", admission_id.as_str()]);
    assert_eq!(texts(&events), "reply-0reply-1");
    assert_eq!(
        ended(&events),
        [
            (Outcome::Complete, "a1".to_string()),
            (Outcome::Complete, admission_id.clone())
        ]
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CoreEvent::QueueChanged { .. }))
    );
    // The second turn's end names the rows it stored: its prompt and its reply.
    let end = events
        .iter()
        .rev()
        .find_map(|e| match e {
            CoreEvent::TurnEnded { end, .. } => Some(end),
            _ => None,
        })
        .unwrap();
    assert_eq!(end.text, "reply-1");
    assert_eq!(end.persisted.user_row_ids.len(), 1);
    assert!(end.persisted.final_assistant_row_id.is_some());
    assert!(f.core.queue(&f.session).is_empty());
    assert!(!f.core.is_live(&f.session));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_prompt_is_removed_and_a_stopped_turn_ends_interrupted() {
    // The first reply is held until the fixture drops: the turn runs until it is stopped.
    let mut f = fixture().await;
    f.submit(prompt("first")).await.unwrap();
    let Submitted::Queued { admission_id } = f.submit(prompt("second")).await.unwrap() else {
        panic!("not queued");
    };
    let hooks = f.hooks.clone();
    let removed = f
        .core
        .remove_queued(&f.session, &admission_id, &*hooks)
        .unwrap();
    assert_eq!(removed.text, "second");
    assert!(
        f.core
            .remove_queued(&f.session, &admission_id, &*hooks)
            .is_none()
    );
    // Steering a prompt that is no longer queued is refused.
    assert_eq!(
        f.core.steer_queued(&f.session, &admission_id, &*hooks),
        Err(SteerQueued::Started)
    );
    assert!(f.core.stop(&f.session), "a turn was running");
    let events = f.until(|e| matches!(e, CoreEvent::Idle { .. })).await;
    let outcomes: Vec<Outcome> = ended(&events).into_iter().map(|(o, _)| o).collect();
    assert_eq!(outcomes, [Outcome::Interrupted]);
    assert_eq!(texts(&events), "", "the delayed reply never streamed");
    // Idle now: nothing to steer into or redirect.
    assert_eq!(
        f.core.steer_queued(&f.session, &admission_id, &*hooks),
        Err(SteerQueued::Idle)
    );
    assert!(!f.core.redirect(&f.session, "instead", &*hooks));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_prompt_that_cannot_start_says_why_and_the_session_settles() {
    let mut f = fixture().await;
    f.submit(prompt("first")).await.unwrap();
    // Queued with an image that is gone by the time it runs: its turn can't start.
    let missing = f.core.data_dir().join("no-such-image.png");
    let Submitted::Queued { admission_id } = f
        .submit(SubmitTurn {
            images: vec![missing.display().to_string()],
            ..prompt("second")
        })
        .await
        .unwrap()
    else {
        panic!("not queued");
    };
    f.release.take();
    let events = f.until(|e| matches!(e, CoreEvent::Idle { .. })).await;
    let failed: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::QueuedFailed {
                admission_id,
                error,
                ..
            } => Some((admission_id.as_str(), error.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "{events:?}");
    assert_eq!(failed[0].0, admission_id);
    assert!(failed[0].1.contains("can't be read"), "{}", failed[0].1);
    // Only the first turn started and ended.
    assert_eq!(ended(&events).len(), 1);
    assert!(f.core.queue(&f.session).is_empty());
    assert!(!f.core.is_live(&f.session));
}
