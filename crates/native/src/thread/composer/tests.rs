//! The composer, headless with real text (`testing::headless_app`) over a [`FixtureBackend`]: its
//! behaviour (Enter sends, Shift+Enter is a new line, nothing for white space, an IME
//! composition's Enter is the IME's, undo, a failed send keeps the text and toasts, the queue
//! behind a running turn comes from the core's, Stop, steer, edit a queued prompt, the model,
//! effort and mode menus) and its layout (the card at three window widths, the input growing to
//! its maximum and then scrolling, the footer on one row, nothing wider than the card). The
//! wiring to the app model and the workspace is `wiring_tests.rs`; a real core is `real_tests.rs`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Entity, EntityInputHandler as _, HeadlessAppContext,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Point, Render,
    Styled as _, Subscription, Window, div, point, px, size,
    test::{TestEventExt as _, TestWindowExt as _},
};
use hermes_core::CoreEvent;

use super::data::{Controls, Mode, QueueOp, QueuedItem};
use super::view::{LANE, bind};
use super::*;
use crate::design::toast::Kind;
use crate::dev::ProbeExt as _;
use crate::model::{
    FixtureBackend,
    backend::ComposerOp,
    fixture::{self, NOW_MS},
};
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::{DesignTheme as _, ThemeChoice};
use crate::thread::ThreadModel;
use crate::thread::model::RowKind;

/// The window's root: the composer at the bottom of the window, as under a timeline.
struct Shell {
    composer: Entity<ComposerView>,
    _events: Subscription,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        div()
            .id("shell")
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .bg(palette.background.hsla())
            .child(self.composer.clone())
            .probe()
    }
}

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    thread: Option<Entity<ThreadModel>>,
    model: Entity<ComposerModel>,
    view: Entity<ComposerView>,
    events: Rc<RefCell<Vec<ComposerViewEvent>>>,
}

fn controls(f: impl FnOnce(&mut Controls)) -> Controls {
    let mut c = FixtureBackend::default_controls();
    f(&mut c);
    c
}

fn queued(id: &str, text: &str) -> QueuedItem {
    QueuedItem {
        id: id.into(),
        text: text.into(),
        shown: false,
    }
}

/// A composer for thread `t1` in an 800 x 600 window.
fn rig() -> Rig {
    rig_with(
        Target::Thread("t1".into()),
        (800., 600.),
        |_: &FixtureBackend| {},
    )
}

/// A composer of `t1` whose turn is running, with `queue` waiting.
fn running_rig(queue: Vec<QueuedItem>) -> Rig {
    rig_with(
        Target::Thread("t1".into()),
        (800., 600.),
        move |b: &FixtureBackend| {
            b.set_controls(
                "t1",
                controls(|c| {
                    c.running = true;
                    c.queue = queue;
                    c.context = Some((18_000, 200_000));
                }),
            )
        },
    )
}

fn rig_with(target: Target, (w, h): (f32, f32), prep: impl FnOnce(&FixtureBackend)) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        bind(cx);
    });
    let backend = FixtureBackend::new(fixture::snapshot());
    prep(&backend);
    let thread =
        match &target {
            Target::Thread(id) => {
                let (backend, id) = (backend.clone(), id.clone());
                Some(cx.update(|cx| {
                    cx.new(|cx| ThreadModel::new(&id, backend, cx).with_clock(|| NOW_MS))
                }))
            }
            Target::Draft(_) => None,
        };
    let model = {
        let (backend, thread, target) = (backend.clone(), thread.clone(), target.clone());
        cx.update(|cx| cx.new(|cx| ComposerModel::new(target, backend, thread, cx)))
    };
    let events = Rc::new(RefCell::new(Vec::new()));
    let key = match &target {
        Target::Thread(id) => id.clone(),
        Target::Draft(project) => data::new_thread_key(project),
    };
    let (shared, m) = (events.clone(), model.clone());
    let window = open_window(&mut cx, size(px(w), px(h)), move |window, cx| {
        let composer = cx.new(|cx| ComposerView::new(m, key, None, window, cx));
        cx.new(|cx| Shell {
            _events: cx.subscribe(&composer, move |_, _, e: &ComposerViewEvent, _| {
                shared.borrow_mut().push(e.clone())
            }),
            composer,
        })
    });
    let view = cx
        .update_window(window.into(), |root, _, cx| {
            root.downcast::<Shell>()
                .expect("the root is the shell")
                .read(cx)
                .composer
                .clone()
        })
        .unwrap();
    let mut rig = Rig {
        cx,
        window: window.into(),
        backend,
        thread,
        model,
        view,
        events,
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

    fn layout(&mut self) -> Layout {
        layout(&mut self.cx, self.window)
    }

    fn read<R>(&mut self, f: impl FnOnce(&gpui_kit::App) -> R) -> R {
        self.cx.update(|cx| f(cx))
    }

    fn with_window(&mut self, f: impl FnOnce(&mut Window, &mut gpui_kit::App)) {
        self.cx
            .update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap();
        self.settle();
    }

    fn text(&mut self) -> String {
        let view = self.view.clone();
        self.read(|cx| view.read(cx).text(cx))
    }

    fn set_text(&mut self, text: &str) {
        let (view, text) = (self.view.clone(), text.to_string());
        self.with_window(move |window, cx| {
            view.update(cx, |v, cx| {
                v.set_text(&text, window, cx);
                v.focus_input(window, cx);
            })
        });
    }

    fn focus_input(&mut self) {
        let view = self.view.clone();
        self.with_window(move |window, cx| view.update(cx, |v, cx| v.focus_input(window, cx)));
    }

    fn press(&mut self, keys: &str) {
        self.focus_input();
        self.with_window(|window, cx| window.press(keys, cx));
    }

    fn type_text(&mut self, text: &str) {
        self.focus_input();
        self.with_window(|window, cx| window.input(text, cx));
    }

    fn click_at(&mut self, at: Point<gpui_kit::Pixels>) {
        self.with_window(|window, cx| {
            window.pointer_move(at, None, cx);
            window.pointer_down(at, MouseButton::Left, cx);
            window.pointer_up(at, MouseButton::Left, cx);
        });
    }

    fn click(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.click_at(at);
    }

    fn present(&mut self, name: &str) -> bool {
        self.layout().get(name).is_ok()
    }

    fn model<R>(
        &mut self,
        f: impl FnOnce(&mut ComposerModel, &mut Context<ComposerModel>) -> R,
    ) -> R {
        let model = self.model.clone();
        let r = self.cx.update(|cx| model.update(cx, f));
        self.settle();
        r
    }

    /// An event of the core for `t1`, taken in by the thread's model and the composer's, as the
    /// app model hands it on.
    fn emit(&mut self, event: CoreEvent) {
        if let Some(thread) = self.thread.clone() {
            let e = event.clone();
            self.cx
                .update(|cx| thread.update(cx, |t, cx| t.on_event(&e, cx)));
        }
        self.model(|m, cx| m.on_event(&event, cx));
    }

    fn ops(&self) -> Vec<ComposerOp> {
        self.backend.composer_ops()
    }

    fn sends(&self) -> Vec<(String, bool)> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                ComposerOp::Send { request, .. } => Some((request.text, request.steer)),
                _ => None,
            })
            .collect()
    }

    fn rows(&mut self) -> usize {
        let thread = self.thread.clone().unwrap();
        self.read(|cx| thread.read(cx).rows().len())
    }

    fn last_user_row(&mut self) -> Option<String> {
        let thread = self.thread.clone().unwrap();
        self.read(|cx| match &thread.read(cx).rows().last()?.kind {
            RowKind::User { text, .. } => Some(text.to_string()),
            _ => None,
        })
    }

    fn expected(&mut self) -> usize {
        let thread = self.thread.clone().unwrap();
        self.read(|cx| thread.read(cx).expected_prompts())
    }

    fn notices(&self) -> Vec<(Kind, String)> {
        self.events
            .borrow()
            .iter()
            .filter_map(|e| match e {
                ComposerViewEvent::Notice(spec) => Some((spec.kind, spec.title.clone())),
                _ => None,
            })
            .collect()
    }

    fn drafts(&self) -> Vec<(String, String)> {
        self.events
            .borrow()
            .iter()
            .filter_map(|e| match e {
                ComposerViewEvent::Draft { key, text } => Some((key.clone(), text.clone())),
                _ => None,
            })
            .collect()
    }

    fn admission_of_last_send(&self) -> String {
        self.ops()
            .into_iter()
            .rev()
            .find_map(|op| match op {
                ComposerOp::Send { request, .. } => Some(request.admission_id),
                _ => None,
            })
            .expect("a send")
    }

    fn controls(&mut self) -> Controls {
        let model = self.model.clone();
        self.read(|cx| model.read(cx).controls().clone())
    }

    fn open_menu(&mut self) -> Option<Menu> {
        let view = self.view.clone();
        self.read(|cx| view.read(cx).open_menu())
    }
}

fn started(id: &str) -> CoreEvent {
    CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: id.into(),
    }
}

// --- Enter, Shift+Enter, nothing, the IME, undo ----------------------------------------------

#[core::prelude::v1::test]
fn enter_sends_the_trimmed_text_clears_the_input_and_the_row_shows_when_the_turn_starts() {
    let mut rig = rig();
    let rows = rig.rows();
    rig.set_text("  hello there \n");
    rig.press("enter");
    assert_eq!(rig.sends(), [("hello there".to_string(), false)]);
    assert_eq!(
        rig.text(),
        "",
        "the input is cleared once the send went through"
    );
    // No row yet: it shows when the turn with this prompt starts.
    assert_eq!(rig.rows(), rows);
    assert_eq!(rig.expected(), 1);
    let id = rig.admission_of_last_send();
    assert!(!id.is_empty());
    rig.emit(started(&id));
    assert_eq!(rig.last_user_row().as_deref(), Some("hello there"));
    assert_eq!(rig.expected(), 0);
    // A turn that was not the composer's shows no row of its own.
    let rows = rig.rows();
    rig.emit(started("someone-else"));
    assert_eq!(rig.rows(), rows);
}

#[core::prelude::v1::test]
fn shift_enter_is_a_new_line_and_sends_nothing() {
    let mut rig = rig();
    rig.type_text("one");
    rig.press("shift-enter");
    rig.type_text("two");
    assert_eq!(rig.text(), "one\ntwo");
    assert!(rig.sends().is_empty());
    // Enter then sends both lines.
    rig.press("enter");
    assert_eq!(rig.sends(), [("one\ntwo".to_string(), false)]);
}

#[core::prelude::v1::test]
fn empty_and_white_space_only_text_is_not_sent() {
    let mut rig = rig();
    rig.press("enter");
    rig.set_text("   \n\t ");
    rig.press("enter");
    assert!(rig.ops().is_empty(), "{:?}", rig.ops());
    assert_eq!(rig.text(), "   \n\t ", "and the input is left alone");
    // The send button is disabled for both.
    assert!(rig.layout().get("composer-send").unwrap().disabled);
    rig.set_text("go");
    assert!(!rig.layout().get("composer-send").unwrap().disabled);
}

#[core::prelude::v1::test]
fn an_enter_that_ends_an_ime_composition_does_not_send() {
    let mut rig = rig();
    rig.set_text("");
    let view = rig.view.clone();
    let input = rig.read(|cx| view.read(cx).input().clone());
    // The IME is composing "ni": marked text.
    let marked = input.clone();
    rig.with_window(move |window, cx| {
        marked.update(cx, |state, cx| {
            state.replace_and_mark_text_in_range(None, "ni", None, window, cx)
        });
    });
    assert_eq!(rig.text(), "ni");
    rig.press("enter");
    assert!(
        rig.sends().is_empty(),
        "Enter belongs to the IME while composing"
    );
    // The composition ends; now Enter sends.
    rig.with_window(move |window, cx| {
        input.update(cx, |state, cx| state.unmark_text(window, cx));
    });
    rig.press("enter");
    assert_eq!(rig.sends(), [("ni".to_string(), false)]);
}

#[core::prelude::v1::test]
fn undo_takes_back_what_was_typed() {
    let mut rig = rig();
    rig.type_text("hello");
    assert_eq!(rig.text(), "hello");
    rig.press("ctrl-z");
    assert_ne!(rig.text(), "hello", "undo changed the text");
    rig.press("ctrl-y");
    assert_eq!(rig.text(), "hello", "and redo brings it back");
}

// --- a failed send ---------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_failed_send_keeps_the_draft_and_shows_a_toast_and_the_next_try_works() {
    let mut rig = rig();
    rig.backend.fail_with(Some("No provider is configured."));
    rig.set_text("keep me");
    rig.press("enter");
    assert_eq!(rig.text(), "keep me", "the text stays");
    assert_eq!(
        rig.notices(),
        [(Kind::Error, "Could not send the message".to_string())]
    );
    let spec = rig
        .events
        .borrow()
        .iter()
        .find_map(|e| match e {
            ComposerViewEvent::Notice(spec) => Some(spec.clone()),
            _ => None,
        })
        .unwrap();
    assert!(spec.description.unwrap().contains("No provider"));
    assert_eq!(rig.expected(), 0, "no row is waited for");
    let model = rig.model.clone();
    assert!(!rig.read(|cx| model.read(cx).sending()));
    // Fixed: the same text goes through.
    rig.backend.fail_with(None);
    rig.press("enter");
    assert_eq!(rig.sends().len(), 2);
    assert_eq!(rig.text(), "");
}

#[core::prelude::v1::test]
fn what_was_typed_while_the_send_was_on_its_way_is_not_cleared() {
    let mut rig = rig();
    rig.set_text("second thought");
    rig.model(|_, cx| {
        cx.emit(ComposerEvent::Sent {
            text: "first".into(),
        })
    });
    assert_eq!(rig.text(), "second thought");
    rig.set_text("first");
    rig.model(|_, cx| {
        cx.emit(ComposerEvent::Sent {
            text: "first".into(),
        })
    });
    assert_eq!(rig.text(), "");
}

// --- running: stop, queue, steer ----------------------------------------------------------------

#[core::prelude::v1::test]
fn while_a_turn_runs_an_empty_composer_offers_stop_and_stop_stops_it() {
    let mut rig = running_rig(Vec::new());
    assert!(rig.present("composer-stop"));
    assert!(!rig.present("composer-send"));
    rig.click("composer-stop");
    assert_eq!(rig.ops(), [ComposerOp::Stop("t1".into())]);
    // With text to send the same place is the queue button.
    rig.set_text("and then");
    assert!(rig.present("composer-send"));
    assert!(!rig.present("composer-stop"));
    // A failing stop says so.
    rig.backend.fail_with(Some("no such turn"));
    rig.set_text("");
    rig.click("composer-stop");
    assert_eq!(
        rig.notices(),
        [(Kind::Error, "Could not stop the turn".to_string())]
    );
}

#[core::prelude::v1::test]
fn sending_while_a_turn_runs_queues_it_and_the_strip_shows_what_the_core_queued() {
    let mut rig = running_rig(Vec::new());
    assert!(!rig.present("composer-queue"));
    let rows = rig.rows();
    rig.set_text("then this");
    rig.press("enter");
    // The core queued it (the fixture lists it as the core's queue does) ...
    assert!(rig.present("composer-queue"));
    assert!(rig.present("queued-0"));
    assert_eq!(rig.text(), "");
    assert_eq!(rig.rows(), rows, "no row yet");
    assert_eq!(rig.expected(), 1, "its row waits for its turn");
    let ours = rig.admission_of_last_send();
    // ... and a prompt the core queued without this composer (another front end, a retry) shows
    // too, as soon as the core says its queue changed.
    rig.backend
        .update_controls("t1", |c| c.queue.push(queued("other", "from elsewhere")));
    rig.emit(CoreEvent::QueueChanged {
        session_id: "t1".into(),
    });
    assert!(rig.present("queued-1"));
    // Each is removable.
    rig.click("queued-remove-0");
    assert_eq!(
        rig.ops().last(),
        Some(&ComposerOp::Queue {
            session: "t1".into(),
            op: QueueOp::Remove(ours),
        })
    );
    assert!(!rig.present("queued-1"), "one left");
    assert!(rig.present("queued-0"));
    assert_eq!(
        rig.expected(),
        0,
        "the removed prompt's row is not waited for"
    );
}

#[core::prelude::v1::test]
fn a_queued_prompt_that_starts_shows_its_row_then() {
    let mut rig = running_rig(Vec::new());
    rig.set_text("queued one");
    rig.press("enter");
    let id = rig.admission_of_last_send();
    let rows = rig.rows();
    rig.emit(started(&id));
    assert_eq!(rig.rows(), rows + 1);
    assert_eq!(rig.last_user_row().as_deref(), Some("queued one"));
}

#[core::prelude::v1::test]
fn a_long_queue_shows_two_and_all_when_asked() {
    let items = ["a", "b", "c", "d"]
        .map(|n| queued(n, &format!("prompt {n}")))
        .to_vec();
    let mut rig = running_rig(items);
    assert!(rig.present("queued-0") && rig.present("queued-1"));
    assert!(!rig.present("queued-2"), "two of four");
    rig.click("queue-toggle");
    assert!(rig.present("queued-3"));
    rig.click("queue-toggle");
    assert!(!rig.present("queued-2"));
}

#[core::prelude::v1::test]
fn send_now_steers_a_queued_prompt_into_the_turn() {
    // Queued by another front end (not this composer): its text is the queue's.
    let mut rig = running_rig(vec![queued("q1", "do it now")]);
    let rows = rig.rows();
    rig.click("queued-send-0");
    assert_eq!(rig.rows(), rows + 1, "it joins the turn: its row shows now");
    assert_eq!(rig.last_user_row().as_deref(), Some("do it now"));
    assert_eq!(
        rig.ops(),
        [ComposerOp::Queue {
            session: "t1".into(),
            op: QueueOp::Steer("q1".into())
        }]
    );
    assert!(!rig.present("queued-0"));
}

#[core::prelude::v1::test]
fn ctrl_enter_steers_the_running_turn_and_its_row_shows_at_once() {
    let mut rig = running_rig(Vec::new());
    let rows = rig.rows();
    rig.set_text("change course");
    rig.press("ctrl-enter");
    assert_eq!(rig.sends(), [("change course".to_string(), true)]);
    assert_eq!(rig.text(), "");
    assert_eq!(rig.rows(), rows + 1);
    assert_eq!(rig.last_user_row().as_deref(), Some("change course"));
    assert!(!rig.present("composer-queue"), "a steer is not queued");
    // Not running: Ctrl+Enter is a plain send.
    let mut idle = rig_with(Target::Thread("t1".into()), (800., 600.), |_| {});
    idle.set_text("plain");
    idle.press("ctrl-enter");
    assert_eq!(idle.sends(), [("plain".to_string(), false)]);
}

#[core::prelude::v1::test]
fn a_queued_prompt_the_core_cannot_start_comes_back_with_a_toast() {
    let mut rig = running_rig(Vec::new());
    rig.set_text("this will fail");
    rig.press("enter");
    let id = rig.admission_of_last_send();
    assert_eq!(rig.text(), "");
    rig.emit(CoreEvent::QueuedFailed {
        session_id: "t1".into(),
        admission_id: id,
        error: "the image is gone".into(),
    });
    assert_eq!(rig.text(), "this will fail");
    assert_eq!(
        rig.notices(),
        [(
            Kind::Warning,
            "A queued message could not start".to_string()
        )]
    );
    assert_eq!(rig.expected(), 0);
}

#[core::prelude::v1::test]
fn a_queued_prompt_is_edited_in_the_composer_and_the_edit_replaces_it() {
    let mut rig = running_rig(Vec::new());
    // Queued by this composer: its row is waited for under its admission.
    rig.set_text("first");
    rig.press("enter");
    let q1 = rig.admission_of_last_send();
    assert_eq!(rig.expected(), 1);
    rig.set_text("my draft");
    rig.click("queued-edit-0");
    assert_eq!(rig.text(), "first");
    assert!(rig.present("queued-cancel-0"));
    rig.set_text("first, edited");
    rig.press("enter");
    let ops = rig.ops();
    let new_id = match ops.last() {
        Some(ComposerOp::Queue {
            session,
            op: QueueOp::Replace { id, text, new_id },
        }) if session == "t1" && *id == q1 && text == "first, edited" => new_id.clone(),
        other => panic!("a replacement: {other:?}"),
    };
    assert!(!new_id.is_empty() && new_id != q1, "a new admission");
    assert_eq!(rig.text(), "my draft", "the draft in progress comes back");
    assert_eq!(rig.expected(), 1, "the edit is waited for, the replaced one is not");
    // The edit's row shows when its turn starts (under the new admission), once.
    let rows = rig.rows();
    rig.emit(started(&q1));
    assert_eq!(rig.rows(), rows, "the replaced admission never shows");
    rig.emit(started(&new_id));
    assert_eq!(rig.rows(), rows + 1);
    assert_eq!(rig.last_user_row().as_deref(), Some("first, edited"));
    assert_eq!(rig.expected(), 0, "nothing left waiting");
    // Cancelling gives the draft back too and leaves the prompt as it was.
    let mut rig = running_rig(vec![queued("q1", "first")]);
    rig.set_text("draft two");
    rig.click("queued-edit-0");
    assert_eq!(rig.text(), "first");
    rig.click("queued-cancel-0");
    assert_eq!(rig.text(), "draft two");
    assert!(rig.ops().is_empty());
    assert!(rig.present("queued-edit-0"));
}

// --- the footer's picks -------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_model_picker_lists_the_endpoints_models_searches_and_a_pick_is_the_threads() {
    let mut rig = rig();
    let before = rig.controls();
    assert_eq!(
        (before.model.as_str(), before.effort.as_str()),
        ("fixture-model", "medium")
    );
    rig.click("model-chip");
    assert!(rig.present("model-panel"));
    for ix in 0..4 {
        assert!(rig.present(&format!("model-option-{ix}")), "option {ix}");
    }
    // Search narrows the list.
    let view = rig.view.clone();
    let search = rig.read(|cx| view.read(cx).search().clone());
    rig.with_window(|window, cx| search.update(cx, |s, cx| s.set_value("llama", window, cx)));
    assert!(rig.present("model-option-0"));
    assert!(!rig.present("model-option-1"));
    // Enter in the search takes the first match; the picker closes.
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(
        rig.ops(),
        [ComposerOp::Pick {
            session: "t1".into(),
            pick: data::Pick::Model {
                provider: "local".into(),
                model: "llama-3.3-70b".into()
            }
        }]
    );
    assert!(!rig.present("model-panel"));
    assert_eq!(rig.controls().model, "llama-3.3-70b");
    // A refused pick says so.
    rig.backend.fail_with(Some("unknown provider"));
    rig.click("model-chip");
    rig.click("model-option-1");
    assert_eq!(
        rig.notices(),
        [(Kind::Error, "Could not change the model".to_string())]
    );
}

#[core::prelude::v1::test]
fn the_effort_picker_marks_the_current_effort_and_picks_stay_in_the_menu() {
    let mut rig = rig();
    rig.click("model-chip");
    for id in [
        "off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ] {
        assert!(rig.present(&format!("effort-{id}")), "{id}");
    }
    rig.click("effort-high");
    assert_eq!(
        rig.ops(),
        [ComposerOp::Pick {
            session: "t1".into(),
            pick: data::Pick::Effort(Some("high".into()))
        }]
    );
    assert_eq!(rig.controls().effort, "high");
    assert!(rig.present("model-panel"), "the menu stays open for effort");
    assert_eq!(rig.controls().effort_text(), "High");
}

#[core::prelude::v1::test]
fn the_mode_menu_picks_a_mode_and_closes() {
    let mut rig = rig();
    assert_eq!(rig.controls().mode, Mode::AcceptEdits);
    rig.click("mode-chip");
    assert!(rig.present("mode-panel"));
    for mode in Mode::ALL {
        assert!(
            rig.present(&format!("mode-option-{}", mode.id())),
            "{mode:?}"
        );
    }
    rig.click("mode-option-plan");
    assert_eq!(
        rig.ops(),
        [ComposerOp::Pick {
            session: "t1".into(),
            pick: data::Pick::Mode(Mode::Plan)
        }]
    );
    assert!(!rig.present("mode-panel"));
    assert_eq!(rig.controls().mode, Mode::Plan);
}

#[core::prelude::v1::test]
fn a_menu_closes_on_escape_and_on_a_click_outside_and_the_chip_toggles_it() {
    let mut rig = rig();
    rig.click("model-chip");
    assert_eq!(rig.open_menu(), Some(Menu::Model));
    rig.with_window(|window, cx| window.press("escape", cx));
    assert!(!rig.present("model-panel"));
    // Outside the menu.
    rig.click("mode-chip");
    assert!(rig.present("mode-panel"));
    rig.click_at(point(px(5.), px(5.)));
    assert!(!rig.present("mode-panel"));
    // At once, with no pause: the next press on the chip is another press, so it opens.
    // The chip that is clicked while its menu is open closes it (the menu closed itself on the
    // press; the click of that same press does not reopen it).
    rig.click("mode-chip");
    assert!(rig.present("mode-panel"));
    rig.click("mode-chip");
    assert!(!rig.present("mode-panel"));
    // Only one menu at a time.
    rig.click("mode-chip");
    rig.click("model-chip");
    assert!(!rig.present("mode-panel"));
    assert!(rig.present("model-panel"));
}

#[core::prelude::v1::test]
fn a_model_change_while_a_turn_runs_neither_stops_nor_starts_a_turn() {
    // The fixture stands in for the core's deferral (`real_tests` has the real one): the footer
    // shows the pick at once and the only thing asked of the core is the pick.
    let mut rig = running_rig(Vec::new());
    rig.click("model-chip");
    rig.click("model-option-2");
    assert_eq!(rig.controls().model, "qwen3-coder");
    assert!(rig.controls().running);
    assert!(
        rig.ops()
            .iter()
            .all(|op| matches!(op, ComposerOp::Pick { .. })),
        "{:?}",
        rig.ops()
    );
}

// --- the new-thread screen ----------------------------------------------------------------------

fn draft_rig() -> Rig {
    rig_with(
        Target::Draft("prj_structure".into()),
        (800., 600.),
        |b: &FixtureBackend| {
            b.set_controls(
                "",
                controls(|c| {
                    c.provider = "fixture".into();
                    c.model = "fixture-model".into();
                }),
            )
        },
    )
}

#[core::prelude::v1::test]
fn a_send_from_the_new_thread_screen_creates_the_thread_first() {
    let mut rig = draft_rig();
    rig.set_text("build it");
    rig.press("enter");
    assert_eq!(
        rig.ops(),
        [ComposerOp::Create {
            project: "prj_structure".into(),
            model: None
        }]
    );
    // The first prompt is sent by the app model once the thread is open (`wiring_tests`); the
    // draft of this screen is forgotten.
    assert_eq!(
        rig.drafts().last(),
        Some(&("new:prj_structure".to_string(), String::new()))
    );
    // Until the new thread opens, the screen keeps the text and a second Enter makes nothing.
    assert_eq!(rig.text(), "build it");
    rig.press("enter");
    assert_eq!(rig.ops().len(), 1, "one thread: {:?}", rig.ops());
    assert!(rig.layout().get("composer-send").unwrap().disabled);
}

#[core::prelude::v1::test]
fn a_one_megabyte_paste_is_sent_whole_without_a_long_stall() {
    let mut rig = rig();
    let big = "0123456789abcdef ".repeat(64 * 1024); // 1.1 MB on one long wrapped line
    let started = std::time::Instant::now();
    rig.set_text(&big);
    rig.press("enter");
    let took = started.elapsed();
    let sent = rig.sends();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, big.trim());
    assert_eq!(rig.text(), "", "cleared once the send went through");
    // Generous for a debug build on a loaded machine; a freeze is many times this.
    assert!(took < std::time::Duration::from_secs(20), "took {took:?}");
    eprintln!("1 MB paste, laid out and sent in {took:?}");
}

#[core::prelude::v1::test]
fn a_second_enter_while_the_send_is_on_its_way_sends_nothing() {
    let mut rig = rig();
    rig.model(|m, cx| {
        m.send("once", false, cx);
        m.send("once", false, cx);
    });
    assert_eq!(rig.sends(), [("once".to_string(), false)]);
}

#[core::prelude::v1::test]
fn a_refused_new_thread_keeps_the_text_and_toasts() {
    let mut rig = draft_rig();
    rig.backend.fail_with(Some("No provider is configured."));
    rig.set_text("build it");
    rig.press("enter");
    assert_eq!(rig.text(), "build it");
    assert_eq!(
        rig.notices(),
        [(Kind::Error, "Could not send the message".to_string())]
    );
}

#[core::prelude::v1::test]
fn picks_on_the_new_thread_screen_wait_for_the_thread_and_the_model_becomes_the_default() {
    let mut rig = draft_rig();
    rig.click("mode-chip");
    rig.click("mode-option-auto");
    rig.click("model-chip");
    rig.click("effort-low");
    rig.click("model-option-3");
    // No thread yet: mode and effort are kept for it; the model is the default for new threads.
    let model = rig.model.clone();
    let picks = rig.read(|cx| model.read(cx).picks().clone());
    assert_eq!(picks.mode, Some(Mode::Auto));
    assert_eq!(picks.effort, Some(Some("low".to_string())));
    assert_eq!(
        rig.ops(),
        [ComposerOp::DefaultModel {
            provider: "local".into(),
            model: "llama-3.3-70b".into()
        }]
    );
    let shown = rig.controls();
    assert_eq!(
        (shown.mode, shown.effort.as_str(), shown.model.as_str()),
        (Mode::Auto, "low", "llama-3.3-70b")
    );
    // The thread is made with that model.
    rig.set_text("go");
    rig.press("enter");
    assert!(rig.ops().iter().any(|op| matches!(
        op,
        ComposerOp::Create { model: Some((p, m)), .. } if p == "local" && m == "llama-3.3-70b"
    )));
}

// --- the draft ------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn typing_reports_the_draft_and_a_send_clears_it() {
    let mut rig = rig();
    rig.type_text("half a thought");
    assert_eq!(
        rig.drafts().last(),
        Some(&("t1".to_string(), "half a thought".to_string()))
    );
    rig.press("enter");
    assert_eq!(
        rig.drafts().last(),
        Some(&("t1".to_string(), String::new())),
        "sent: no draft"
    );
}

// --- layout ---------------------------------------------------------------------------------------

fn card_width(window: f32) -> f32 {
    (window - 32.).min(LANE)
}

fn long_model(c: &mut Controls) {
    c.model = format!("openrouter/{}", "very-long-model-name-".repeat(14));
    c.context = Some((150_000, 200_000));
}

#[core::prelude::v1::test]
fn the_card_is_the_lane_wide_centred_and_its_footer_is_one_row_at_three_window_widths() {
    for w in [640., 1100., 1920.] {
        let mut rig = rig_with(
            Target::Thread("t1".into()),
            (w, 600.),
            |b: &FixtureBackend| {
                b.set_controls("t1", controls(|c| c.context = Some((18_000, 200_000))))
            },
        );
        let layout = rig.layout();
        let card = layout.bounds("composer");
        assert!(
            (card.size.width.as_f32() - card_width(w)).abs() < 1.,
            "{w}: card {}",
            card.size.width.as_f32()
        );
        let left = card.left().as_f32();
        let right = w - card.right().as_f32();
        assert!((left - right).abs() < 1., "{w}: centred ({left} / {right})");
        assert!(
            (layout.bounds("shell").bottom().as_f32() - card.bottom().as_f32() - 16.).abs() < 1.,
            "{w}: 16 px under the card"
        );
        let mut check = layout.check();
        check
            .inside("composer-footer", "composer")
            .inside("composer-input", "composer")
            .inside("composer-send", "composer")
            .inside("mode-chip", "composer-footer")
            .inside("model-chip", "composer-footer")
            .inside("context-meter", "composer-footer")
            .same_row("mode-chip", "model-chip")
            .same_row("model-chip", "composer-send")
            .same_row("context-meter", "composer-send")
            .no_overflow("mode-chip/label")
            .no_overflow("model-chip/label");
        check.assert();
        // The send button is a 32 px circle at the footer's right end.
        let send = layout.bounds("composer-send");
        assert_eq!(
            (send.size.width.as_f32(), send.size.height.as_f32()),
            (32., 32.)
        );
        assert!(layout.bounds("model-chip").right() < send.left());
    }
}

#[core::prelude::v1::test]
fn a_very_long_model_name_is_cut_and_never_pushes_the_send_button_out_of_the_card() {
    for w in [384., 640., 1100.] {
        let mut rig = rig_with(
            Target::Thread("t1".into()),
            (w, 600.),
            |b: &FixtureBackend| b.set_controls("t1", controls(long_model)),
        );
        let layout = rig.layout();
        let card = layout.bounds("composer");
        let send = layout.bounds("composer-send");
        let chip = layout.bounds("model-chip");
        assert!(send.right() <= card.right(), "{w}: send inside the card");
        assert!(
            chip.right() <= send.left(),
            "{w}: chip before the send button"
        );
        assert!(chip.left() >= card.left(), "{w}: chip inside the card");
        let mut check = layout.check();
        check.same_row("model-chip", "composer-send");
        check.assert();
    }
}

#[core::prelude::v1::test]
fn the_input_grows_to_its_maximum_and_then_scrolls() {
    let mut rig = rig();
    let height = |rig: &mut Rig, lines: usize| {
        let text = (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        rig.set_text(&text);
        let layout = rig.layout();
        (
            layout.bounds("composer-input").size.height.as_f32(),
            layout.bounds("composer").size.height.as_f32(),
            layout.bounds("composer").bottom().as_f32(),
        )
    };
    let (one, card_one, bottom_one) = height(&mut rig, 1);
    let (two, ..) = height(&mut rig, 2);
    let (five, ..) = height(&mut rig, 5);
    let (nine, ..) = height(&mut rig, view::MAX_ROWS);
    let (twelve, card_twelve, bottom_twelve) = height(&mut rig, 12);
    let (thirty, ..) = height(&mut rig, 30);
    assert!((one - two).abs() < 1., "two lines at least: {one} {two}");
    assert!(five > two + 20., "it grows: {two} {five}");
    assert!(nine > five, "{five} {nine}");
    assert!(
        (twelve - nine).abs() < 1. && (thirty - nine).abs() < 1.,
        "and stops at its maximum ({nine}): {twelve} {thirty}"
    );
    // The card grows upward: its bottom edge stays where it was.
    assert!(
        (bottom_one - bottom_twelve).abs() < 1.,
        "{bottom_one} {bottom_twelve}"
    );
    assert!(card_twelve > card_one + 100.);
    // The maximum is about t3code's `max-h-52` (208 px).
    assert!((170.0..=240.0).contains(&nine), "maximum height {nine}");
    // Scrolled, not cut: the whole text is still in the input.
    assert!(rig.text().ends_with("line 30"));
}

#[core::prelude::v1::test]
fn the_queue_sits_above_the_card_and_each_item_has_its_buttons_on_its_row() {
    let items = vec![
        queued("a", "Then run the tests"),
        queued(
            "b",
            &"a very long prompt that wants more than two lines ".repeat(12),
        ),
    ];
    let mut rig = running_rig(items);
    let layout = rig.layout();
    let card = layout.bounds("composer");
    let queue = layout.bounds("composer-queue");
    assert!(queue.bottom() <= card.top(), "the queue is above the card");
    assert!(queue.right() <= card.right() + px(1.));
    let mut check = layout.check();
    check
        .inside("composer-queue", "composer-area")
        .same_row("queued-send-0", "queued-text-0")
        .same_row("queued-remove-0", "queued-text-0")
        .same_row("queued-send-1", "queued-remove-1")
        .inside("queued-text-1", "composer-queue")
        .no_overflow("queued-send-0/label");
    check.assert();
    // A long prompt is cut at two lines, not grown without end.
    let long = layout.bounds("queued-text-1").size.height.as_f32();
    let short = layout.bounds("queued-text-0").size.height.as_f32();
    assert!(long < short * 3., "{long} vs {short}");
    // The bubbles are right-aligned like the user's own messages.
    assert!((layout.bounds("queued-0").right() - card.right()).abs() < px(1.));
}

#[core::prelude::v1::test]
fn the_menus_open_above_the_card_inside_the_window() {
    let mut rig = rig();
    rig.click("model-chip");
    let layout = rig.layout();
    let (panel, card) = (layout.bounds("model-panel"), layout.bounds("composer"));
    assert!(
        panel.bottom() <= card.bottom() - px(20.),
        "above the footer"
    );
    assert!(panel.right() <= card.right() + px(1.));
    assert!(panel.top() >= px(0.) && panel.left() >= px(0.));
    let mut check = layout.check();
    check
        .inside("model-search", "model-panel")
        .inside("effort-list", "model-panel");
    check.assert();
    rig.click("mode-chip");
    let layout = rig.layout();
    let panel = layout.bounds("mode-panel");
    assert!(panel.left() >= card.left() - px(1.));
    let mut check = layout.check();
    check
        .inside("mode-option-plan", "mode-panel")
        .same_row("mode-option-manual", "mode-option-yolo");
    check.assert();
}

#[core::prelude::v1::test]
fn the_context_meter_is_there_only_when_the_core_knows_how_full_the_context_is() {
    let mut rig = rig();
    assert!(
        !rig.present("context-meter"),
        "nothing is known before a call"
    );
    let mut rig = running_rig(Vec::new());
    assert!(rig.present("context-meter"));
}

mod input;
