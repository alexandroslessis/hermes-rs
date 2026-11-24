//! The composer's model over a real `hermes_core::Core` on a temporary data folder and a mock
//! provider (wiremock): it sends, queues behind a running turn, removes a queued prompt, stops,
//! picks a model that applies from the next turn, and starts a new thread from the new-thread
//! screen, through [`CoreBackend`] and the app model as the app wires them. No window: the models
//! are entities of a headless GPUI app, and the core's events reach them through the channel the
//! app model drains. The first reply is held until the test releases it, so the turn runs for as
//! long as the test needs, with no timing assumed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, Entity, HeadlessAppContext};
use hermes_core::Core;
use hermes_runtime::RuntimeConfig;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::data::Mode;
use super::*;
use crate::model::{AppModel, CoreBackend, Selection};
use crate::testing::headless_app;
use crate::thread::model::RowKind;

/// Answers every chat request with `reply-<n>`; the first one only once the test releases it.
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

struct World {
    cx: HeadlessAppContext,
    core: Arc<Core>,
    model: Entity<AppModel>,
    session: String,
    project: String,
    /// Lets the first reply go (dropped before the server, so a held reply never outlives it).
    release: Option<std::sync::mpsc::Sender<()>>,
    // Drop order: the models and the core first, then the server, the runtime, the folders.
    _server: MockServer,
    _runtime: tokio::runtime::Runtime,
    _work: hermes_testdir::TestDir,
    // Last: the runtime above holds files in it until it drops.
    _data: hermes_testdir::TestDir,
}

fn world() -> World {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let data = hermes_testdir::tempdir().unwrap();
    let work = hermes_testdir::tempdir().unwrap();
    let (release, hold) = std::sync::mpsc::channel();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Replies {
                count: AtomicUsize::new(0),
                hold: Mutex::new(Some(hold)),
            })
            .mount(&server)
            .await;
        server
    });
    // The user's settings, as a synthetic data folder has them: one endpoint, two models.
    std::fs::write(
        data.path().join("config.json"),
        json!({
            "providers": [{
                "id": "mock", "name": "Mock endpoint",
                "base_url": format!("{}/v1", server.uri()),
                "models": ["mock", "mock-b"], "discover_models": false
            }],
            "provider": "mock", "model": "mock"
        })
        .to_string(),
    )
    .unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = runtime
        .block_on(async { Core::new(config, tokio::runtime::Handle::current()) })
        .unwrap();
    let project = core
        .create_project(
            "work",
            vec![work.path().display().to_string()],
            Some(work.path().display().to_string()),
        )
        .unwrap();
    let provider = hermes_runtime::ProviderConfig {
        id: "mock".into(),
        base_url: format!("{}/v1", server.uri()),
        api_key: Some("key".into()),
    };
    let (record, _) = core
        .create_session(hermes_core::NewSession {
            provider,
            model: "mock".into(),
            cwd: work.path().to_path_buf(),
            title: Some("Composer".into()),
            seeded: vec![],
            parent: None,
        })
        .unwrap();
    let mut cx = headless_app();
    let backend = Arc::new(CoreBackend::new(core.clone()));
    let model = cx.update(|cx| cx.new(|cx| AppModel::with_backend(backend, cx)));
    World {
        cx,
        core,
        model,
        session: record.id,
        project: project.id,
        release: Some(release),
        _server: server,
        _runtime: runtime,
        _work: work,
        _data: data,
    }
}

impl World {
    /// Runs the app until `done` holds (the core's events arrive from other threads).
    fn wait(&mut self, what: &str, mut done: impl FnMut(&mut HeadlessAppContext) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.cx.run_until_parked();
            if done(&mut self.cx) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn open(&mut self) -> (Entity<ComposerModel>, Entity<crate::thread::ThreadModel>) {
        let (model, id) = (self.model.clone(), self.session.clone());
        self.wait("the list", |cx| {
            cx.update(|cx| model.read(cx).snapshot().thread(&id).is_some())
        });
        self.cx
            .update(|cx| model.update(cx, |m, cx| m.open(&id, cx)));
        self.cx.run_until_parked();
        let composer = self
            .cx
            .update(|cx| model.read(cx).composer().cloned())
            .unwrap();
        let thread = self
            .cx
            .update(|cx| model.read(cx).thread().cloned())
            .unwrap();
        // The footer's first read has landed.
        let c = composer.clone();
        self.wait("the controls", |cx| cx.update(|cx| c.read(cx).loaded()));
        (composer, thread)
    }
}

fn user_rows(
    cx: &mut HeadlessAppContext,
    thread: &Entity<crate::thread::ThreadModel>,
) -> Vec<String> {
    cx.update(|cx| {
        thread
            .read(cx)
            .rows()
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::User { text, .. } => Some(text.to_string()),
                _ => None,
            })
            .collect()
    })
}

fn reply_rows(
    cx: &mut HeadlessAppContext,
    thread: &Entity<crate::thread::ThreadModel>,
) -> Vec<String> {
    cx.update(|cx| {
        thread
            .read(cx)
            .rows()
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Assistant { body } => Some(body.text(cx).to_string()),
                _ => None,
            })
            .collect()
    })
}

#[core::prelude::v1::test]
fn the_composer_sends_queues_removes_and_a_queued_prompt_runs_after_the_turn_on_a_real_core() {
    let mut w = world();
    let (composer, thread) = w.open();
    // The footer reads the real thread: its model, Settings' mode, nothing queued.
    let controls = w.cx.update(|cx| composer.read(cx).controls().clone());
    assert_eq!(
        (controls.provider.as_str(), controls.model.as_str()),
        ("mock", "mock")
    );
    assert_eq!(controls.provider_name, "Mock endpoint");
    assert_eq!(controls.mode, Mode::AcceptEdits);
    assert!(!controls.running && controls.queue.is_empty());

    // First prompt: the turn starts (held by the mock) and its row shows.
    w.cx.update(|cx| composer.update(cx, |c, cx| c.send("first", false, cx)));
    let c = composer.clone();
    w.wait("the turn to run", |cx| cx.update(|cx| c.read(cx).running()));
    let t = thread.clone();
    w.wait("the first row", |cx| user_rows(cx, &t) == ["first"]);

    // Two more wait behind it, in the core's queue; the composer lists them.
    w.cx.update(|cx| composer.update(cx, |c, cx| c.send("second", false, cx)));
    let c = composer.clone();
    w.wait("one queued", |cx| {
        cx.update(|cx| c.read(cx).queue().len() == 1)
    });
    w.cx.update(|cx| composer.update(cx, |c, cx| c.send("third", false, cx)));
    let c = composer.clone();
    w.wait("two queued", |cx| {
        cx.update(|cx| {
            let queue = c.read(cx).queue();
            queue.len() == 2 && queue[0].text == "second" && queue[1].text == "third"
        })
    });
    assert_eq!(
        w.core
            .queue(&w.session)
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>(),
        ["second", "third"],
        "the core's own queue is what the strip shows"
    );
    // No row yet for them.
    assert_eq!(user_rows(&mut w.cx, &thread), ["first"]);

    // Remove the third from the strip: the core's queue loses it.
    let third = w.cx.update(|cx| composer.read(cx).queue()[1].id.clone());
    w.cx.update(|cx| composer.update(cx, |c, cx| c.remove_queued(&third, cx)));
    let c = composer.clone();
    w.wait("the removal", |cx| {
        cx.update(|cx| c.read(cx).queue().len() == 1)
    });
    assert_eq!(w.core.queue(&w.session).len(), 1);

    // The first reply goes: both turns run, in order, and the queued one's row appears when
    // its turn starts (not before).
    w.release.take();
    let (c, t) = (composer.clone(), thread.clone());
    w.wait("both turns", |cx| {
        let done = !cx.update(|cx| c.read(cx).running());
        done && user_rows(cx, &t) == ["first", "second"] && reply_rows(cx, &t).len() == 2
    });
    assert_eq!(reply_rows(&mut w.cx, &thread), ["reply-0", "reply-1"]);
    assert!(w.cx.update(|cx| composer.read(cx).queue().is_empty()));
    let stored = w.core.runtime().store().load_messages(&w.session).unwrap();
    let typed: Vec<String> = stored
        .iter()
        .filter(|m| hermes_core::transcript::is_typed(m))
        .map(hermes_runtime::preinject::user_text)
        .collect();
    assert_eq!(typed, ["first", "second"], "the removed prompt never ran");
    // The context meter has numbers once a call reported its usage.
    let c = composer.clone();
    w.wait("the context", |cx| {
        cx.update(|cx| c.read(cx).controls().context.is_some())
    });
}

#[core::prelude::v1::test]
fn stop_cancels_the_running_turn_on_a_real_core() {
    let mut w = world();
    let (composer, _thread) = w.open();
    w.cx.update(|cx| composer.update(cx, |c, cx| c.send("never answered", false, cx)));
    let c = composer.clone();
    w.wait("the turn to run", |cx| cx.update(|cx| c.read(cx).running()));
    w.cx.update(|cx| composer.update(cx, |c, cx| c.stop(cx)));
    let c = composer.clone();
    w.wait("the turn to stop", |cx| {
        !cx.update(|cx| c.read(cx).running())
    });
    assert!(!w.core.is_live(&w.session));
    // Nothing is left to run and the session is ready for the next prompt.
    assert!(w.core.queue(&w.session).is_empty());
    w.release.take();
}

#[core::prelude::v1::test]
fn a_model_and_a_mode_picked_during_a_turn_apply_from_the_next_turn_on_a_real_core() {
    let mut w = world();
    let (composer, _thread) = w.open();
    // The picker lists the endpoint's models (the read-only list).
    w.cx.update(|cx| composer.update(cx, |c, cx| c.load_choices(cx)));
    let c = composer.clone();
    w.wait("the model list", |cx| {
        cx.update(|cx| matches!(c.read(cx).choices(), Choices::Ready(_)))
    });
    let listed: Vec<String> = w.cx.update(|cx| match composer.read(cx).choices() {
        Choices::Ready(choices) => choices.entries.iter().map(|e| e.model.clone()).collect(),
        _ => Vec::new(),
    });
    assert_eq!(listed, ["mock", "mock-b"]);

    w.cx.update(|cx| composer.update(cx, |c, cx| c.send("go", false, cx)));
    let c = composer.clone();
    w.wait("the turn to run", |cx| cx.update(|cx| c.read(cx).running()));
    // Picked mid-turn: shown at once, but the session's own model is still the old one.
    w.cx.update(|cx| {
        composer.update(cx, |c, cx| {
            c.pick_model("mock", "mock-b", cx);
            c.pick_effort(Some("high"), cx);
            c.pick_mode(Mode::Plan, cx);
        })
    });
    let c = composer.clone();
    w.wait("the picks to show", |cx| {
        cx.update(|cx| {
            let controls = c.read(cx).controls();
            controls.model == "mock-b" && controls.mode == Mode::Plan
        })
    });
    let record = w
        .core
        .runtime()
        .store()
        .session(&w.session)
        .unwrap()
        .unwrap();
    assert_eq!(record.model, "mock", "the running turn keeps its model");
    assert_eq!(
        w.core.runtime().approval_mode(&w.session),
        hermes_tools::ApprovalMode::Plan,
        "the mode is the session's at once"
    );
    assert_eq!(w.core.effort(&w.session), "high");
    // The turn ends: the pick is applied between turns.
    w.release.take();
    let (c, core, session) = (composer.clone(), w.core.clone(), w.session.clone());
    w.wait("the turn to end and the model to move", |cx| {
        // At no point does the footer go back to the old model: every read, before, during
        // and after the core applies the pick between turns, says the picked one.
        let shown = cx.update(|cx| c.read(cx).controls().model.clone());
        assert_eq!(shown, "mock-b", "the footer showed the old model");
        let idle = !cx.update(|cx| c.read(cx).running());
        idle && core
            .runtime()
            .store()
            .session(&session)
            .unwrap()
            .unwrap()
            .model
            == "mock-b"
    });
    assert_eq!(
        w.core.pending_route(&w.session),
        None,
        "the pick was applied"
    );
    // The reads the Idle started have landed (no timer: nothing is read later).
    w.cx.run_until_parked();
    let controls = w.cx.update(|cx| composer.read(cx).controls().clone());
    assert_eq!(controls.model, "mock-b");
    // A pick made while nothing runs is applied at once and leaves nothing pending.
    w.cx.update(|cx| composer.update(cx, |c, cx| c.pick_model("mock", "mock", cx)));
    let (core, session) = (w.core.clone(), w.session.clone());
    w.wait("the idle pick", |_| {
        core.runtime().store().session(&session).unwrap().unwrap().model == "mock"
    });
    let c = composer.clone();
    w.wait("the footer to follow", |cx| {
        cx.update(|cx| c.read(cx).controls().model == "mock")
    });
}

#[core::prelude::v1::test]
fn a_new_thread_from_the_new_thread_screen_is_made_opened_and_answered_on_a_real_core() {
    let mut w = world();
    let (model, project) = (w.model.clone(), w.project.clone());
    let m = model.clone();
    w.wait("the list", |cx| {
        cx.update(|cx| matches!(m.read(cx).phase(), crate::model::Phase::Ready))
    });
    w.cx.update(|cx| model.update(cx, |m, cx| m.new_thread(&project, cx)));
    w.cx.run_until_parked();
    let composer =
        w.cx.update(|cx| model.read(cx).composer().cloned())
            .unwrap();
    let c = composer.clone();
    w.wait("the new thread's controls", |cx| {
        cx.update(|cx| c.read(cx).loaded())
    });
    let controls = w.cx.update(|cx| composer.read(cx).controls().clone());
    assert_eq!(
        (controls.provider.as_str(), controls.model.as_str()),
        ("mock", "mock"),
        "Settings' default"
    );
    w.cx.update(|cx| {
        composer.update(cx, |c, cx| {
            c.pick_mode(Mode::Manual, cx);
            c.send("build it", false, cx);
        })
    });
    // The thread is made in the project's folder and opened; the prompt is answered.
    let m = model.clone();
    w.wait("the thread to open", |cx| {
        cx.update(|cx| matches!(m.read(cx).selected(), Selection::Thread(_)))
    });
    let id = w.cx.update(|cx| match model.read(cx).selected() {
        Selection::Thread(id) => id.clone(),
        other => panic!("{other:?}"),
    });
    let record = w.core.runtime().store().session(&id).unwrap().unwrap();
    assert_eq!(
        record.cwd.replace('/', "\\"),
        w._work.path().display().to_string().replace('/', "\\")
    );
    let thread = w.cx.update(|cx| model.read(cx).thread().cloned()).unwrap();
    w.release.take();
    let t = thread.clone();
    w.wait("the answer", |cx| {
        reply_rows(cx, &t).first().is_some_and(|r| r == "reply-0")
    });
    assert_eq!(user_rows(&mut w.cx, &thread), ["build it"]);
    assert_eq!(
        w.core.runtime().approval_mode(&id),
        hermes_tools::ApprovalMode::Manual,
        "the mode picked on the screen went to the new thread"
    );
}
