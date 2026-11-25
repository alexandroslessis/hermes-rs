//! The frame's notices for a thread that waits (CP5.3), headless with a recording [`Notifier`]:
//! nothing is announced while the user looks at the thread, one notice per wait when they do not,
//! a request answered inside the hold makes none, two together are one, `notifications: false`
//! makes none, and a click on the notice opens the thread. The rules themselves are
//! `notify::tests`; this is the wiring (the model's events, the hold's timer, the window).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, px, size};
use hermes_core::{CoreEvent, QuestionKind};

use crate::model::{AppModel, FixtureBackend, fixture};
use crate::notify::{Attention, HOLD, Notifier};
use crate::testing::{draw, headless_app, open_window};
use crate::theme::ThemeChoice;
use crate::ui_state::UiState;
use crate::workspace::Workspace;

/// Records what it was asked to show.
#[derive(Default)]
struct Recorder {
    flashes: RefCell<usize>,
    toasts: RefCell<Vec<Attention>>,
}

impl Notifier for Recorder {
    fn flash(&self, _: &gpui_kit::Window) {
        *self.flashes.borrow_mut() += 1;
    }

    fn toast(&self, attention: &Attention) {
        self.toasts.borrow_mut().push(attention.clone());
    }
}

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    model: Entity<AppModel>,
    workspace: Entity<Workspace>,
    recorder: Rc<Recorder>,
    clicks: std::sync::mpsc::Sender<String>,
}

fn rig(notifications: bool) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| crate::theme::apply_theme(ThemeChoice::T3Dark, cx));
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let model = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| fixture::NOW_MS))
        })
    };
    let recorder = Rc::new(Recorder::default());
    let (clicks, clicked) = std::sync::mpsc::channel();
    let (m, r) = (model.clone(), recorder.clone());
    let handle = open_window(&mut cx, size(px(1100.), px(700.)), move |window, cx| {
        cx.new(|cx| {
            let mut state = UiState::default();
            state.notifications = notifications;
            let mut workspace = Workspace::with_model(state, None, m, window, cx);
            workspace.set_notifier(r, Some(clicked), window, cx);
            workspace
        })
    });
    let window: AnyWindowHandle = handle.into();
    let workspace = cx
        .update_window(window, |root, _, _| {
            root.downcast::<Workspace>().expect("the workspace")
        })
        .unwrap();
    let mut rig = Rig {
        cx,
        window,
        backend,
        model,
        workspace,
        recorder,
        clicks,
    };
    rig.settle();
    rig
}

impl Rig {
    fn settle(&mut self) {
        for _ in 0..3 {
            self.cx.run_until_parked();
            draw(&mut self.cx, self.window);
        }
    }

    /// Lets the hold run out.
    fn hold(&mut self) {
        self.cx.advance_clock(HOLD + Duration::from_millis(50));
        self.settle();
    }

    fn active(&mut self, active: bool) {
        let ws = self.workspace.clone();
        self.cx
            .update(|cx| ws.update(cx, |w, _| w.force_window_active(Some(active))));
    }

    fn open(&mut self, id: &str) {
        let model = self.model.clone();
        self.cx.update(|cx| model.update(cx, |m, cx| m.open(id, cx)));
        self.settle();
    }

    fn ask(&mut self, id: &str, session: &str) {
        self.backend.ask(CoreEvent::ApprovalRequested {
            session_id: session.into(),
            prompt_id: id.into(),
            command: "rm -rf build".into(),
            description: "recursive delete".into(),
            choices: vec!["once", "deny"],
        });
        self.settle();
    }

    fn settle_ask(&mut self, id: &str, session: &str) {
        self.backend.emit(CoreEvent::Settled {
            session_id: session.into(),
            prompt_id: id.into(),
            kind: QuestionKind::Approval,
        });
        self.settle();
    }

    fn toasts(&self) -> Vec<Attention> {
        self.recorder.toasts.borrow().clone()
    }

    fn flashes(&self) -> usize {
        *self.recorder.flashes.borrow()
    }

    fn open_thread(&mut self) -> Option<String> {
        let model = self.model.clone();
        self.cx
            .update(|cx| model.read(cx).open_thread().map(str::to_string))
    }
}

#[core::prelude::v1::test]
fn a_background_thread_that_waits_flashes_the_taskbar_and_shows_one_notice() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(true);
    rig.ask("p1", "t2");
    assert!(rig.toasts().is_empty(), "held back first");
    rig.hold();
    let toasts = rig.toasts();
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].session, "t2");
    // What waits, never the command line itself (a toast shows on the lock screen).
    assert!(toasts[0].body.contains("command"), "{}", toasts[0].body);
    assert!(!toasts[0].text().contains("rm -rf build"), "{}", toasts[0].text());
    assert!(toasts[0].heading().starts_with("Hermes: "));
    assert!(!toasts[0].title.is_empty());
    // The window is in front: nothing to flash for.
    assert_eq!(rig.flashes(), 0);
    // A second request of the same wait adds no notice.
    rig.ask("p2", "t2");
    rig.hold();
    assert_eq!(rig.toasts().len(), 1);
}

#[core::prelude::v1::test]
fn a_window_behind_flashes_even_for_the_open_thread() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(false);
    rig.ask("p1", "t1");
    rig.hold();
    assert_eq!(rig.flashes(), 1);
    assert_eq!(rig.toasts().len(), 1);
}

#[core::prelude::v1::test]
fn the_open_thread_in_the_window_in_front_makes_no_notice() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(true);
    rig.ask("p1", "t1");
    rig.hold();
    assert!(rig.toasts().is_empty());
    assert_eq!(rig.flashes(), 0);
}

#[core::prelude::v1::test]
fn a_request_settled_before_the_hold_runs_out_makes_no_notice() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(false);
    rig.ask("p1", "t2");
    rig.settle_ask("p1", "t2");
    rig.hold();
    assert!(rig.toasts().is_empty());
    assert_eq!(rig.flashes(), 0);
    // The next wait is a new one and is announced.
    rig.ask("p2", "t2");
    rig.hold();
    assert_eq!(rig.toasts().len(), 1);
}

#[core::prelude::v1::test]
fn a_second_wait_after_the_first_was_answered_is_announced_again() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(false);
    rig.ask("p1", "t2");
    rig.hold();
    rig.settle_ask("p1", "t2");
    rig.ask("p2", "t2");
    rig.hold();
    assert_eq!(rig.toasts().len(), 2);
}

#[core::prelude::v1::test]
fn two_requests_arriving_together_are_one_notice_that_says_so() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.active(false);
    rig.ask("p1", "t2");
    rig.ask("p2", "t2");
    rig.hold();
    let toasts = rig.toasts();
    assert_eq!(toasts.len(), 1);
    assert!(toasts[0].body.contains("and 1 more"), "{}", toasts[0].body);
}

#[core::prelude::v1::test]
fn notifications_off_in_the_setting_makes_none() {
    let mut rig = rig(false);
    rig.open("t1");
    rig.active(false);
    rig.ask("p1", "t2");
    rig.hold();
    assert!(rig.toasts().is_empty());
    assert_eq!(rig.flashes(), 0);
    // The sidebar still marks the thread.
    let model = rig.model.clone();
    assert!(rig.cx.update(|cx| model.read(cx).snapshot().thread("t2").unwrap().waiting));
    // And switching it on announces the next wait.
    let ws = rig.workspace.clone();
    rig.cx.update(|cx| ws.update(cx, |w, _| w.set_notifications(true)));
    rig.ask("p2", "t3");
    rig.hold();
    assert_eq!(rig.toasts().len(), 1);
}

#[core::prelude::v1::test]
fn a_click_on_the_notice_opens_the_thread() {
    let mut rig = rig(true);
    rig.open("t1");
    rig.ask("p1", "t2");
    rig.clicks.send("t2".into()).unwrap();
    // The frame looks for clicks a few times a second.
    for _ in 0..3 {
        rig.cx.advance_clock(Duration::from_millis(300));
        rig.settle();
    }
    assert_eq!(rig.open_thread().as_deref(), Some("t2"));
}
