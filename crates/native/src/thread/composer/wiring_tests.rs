//! The composer in the app: the app model makes it with the thread's model and hands it the
//! core's events; the new-thread screen creates a thread, opens it and sends the first prompt
//! there with the picks made on the screen; the workspace keeps the drafts in `native-ui.json`
//! (per thread, bounded) and lays the card out under the open thread and in the new-thread
//! screen at three window widths, with the sidebar open or collapsed and the right panel open.

use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, px, size};
use hermes_core::CoreEvent;
use hermes_testdir::tempdir;

use super::data::{Drafts, Mode};
use super::*;
use crate::model::{
    AppModel, FixtureBackend, Selection,
    backend::ComposerOp,
    fixture::{self, NOW_MS},
};
use crate::testing::{draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::thread::model::RowKind;
use crate::ui_state::UiState;
use crate::workspace::Workspace;

struct App {
    cx: HeadlessAppContext,
    backend: Arc<FixtureBackend>,
    model: Entity<AppModel>,
}

fn app() -> App {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        crate::thread::composer::view::bind(cx);
    });
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let model = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS))
        })
    };
    App { cx, backend, model }
}

impl App {
    fn settle(&mut self) {
        for _ in 0..4 {
            self.cx.run_until_parked();
        }
    }

    fn composer(&mut self) -> Entity<ComposerModel> {
        let model = self.model.clone();
        self.cx
            .update(|cx| model.read(cx).composer().cloned())
            .expect("a composer is open")
    }
}

#[core::prelude::v1::test]
fn opening_a_thread_makes_its_composer_and_the_cores_events_reach_it() {
    let mut app = app();
    let model = app.model.clone();
    assert!(app.cx.update(|cx| model.read(cx).composer().is_none()));
    app.cx
        .update(|cx| model.update(cx, |m, cx| m.open("t1", cx)));
    app.settle();
    let composer = app.composer();
    let target = app.cx.update(|cx| composer.read(cx).target().clone());
    assert_eq!(target, Target::Thread("t1".into()));
    assert!(!app.cx.update(|cx| composer.read(cx).running()));
    // A turn starts in the core: the composer shows it runs (Stop) and, with the queue changed,
    // lists what waits.
    app.backend.update_controls("t1", |c| {
        c.running = true;
        c.queue.push(data::QueuedItem {
            id: "q".into(),
            text: "later".into(),
            shown: false,
        });
    });
    app.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "x".into(),
    });
    app.settle();
    assert!(app.cx.update(|cx| composer.read(cx).running()));
    assert_eq!(app.cx.update(|cx| composer.read(cx).queue().len()), 1);
    // An event of another thread changes nothing here.
    app.backend.update_controls("t1", |c| c.running = false);
    app.backend.emit(CoreEvent::Idle {
        session_id: "t2".into(),
    });
    app.settle();
    assert!(app.cx.update(|cx| composer.read(cx).running()));
    app.backend.emit(CoreEvent::Idle {
        session_id: "t1".into(),
    });
    app.settle();
    assert!(!app.cx.update(|cx| composer.read(cx).running()));
    // Another thread, another composer; none when nothing is open.
    app.cx
        .update(|cx| model.update(cx, |m, cx| m.open("t2", cx)));
    app.settle();
    let second = app.composer();
    assert_eq!(
        app.cx.update(|cx| second.read(cx).target().clone()),
        Target::Thread("t2".into())
    );
    app.cx
        .update(|cx| model.update(cx, |m, cx| m.clear_selection(cx)));
    assert!(app.cx.update(|cx| model.read(cx).composer().is_none()));
}

#[core::prelude::v1::test]
fn the_new_thread_screen_creates_the_thread_opens_it_and_sends_the_first_prompt_with_its_picks() {
    let mut app = app();
    let model = app.model.clone();
    app.cx
        .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_structure", cx)));
    app.settle();
    let composer = app.composer();
    assert_eq!(
        app.cx.update(|cx| composer.read(cx).target().clone()),
        Target::Draft("prj_structure".into())
    );
    app.cx.update(|cx| {
        composer.update(cx, |c, cx| {
            c.pick_mode(Mode::Plan, cx);
            c.pick_effort(Some("high"), cx);
            c.send("build it", false, cx);
        })
    });
    app.settle();
    let ops = app.backend.composer_ops();
    let created = match &ops[0] {
        ComposerOp::Create { project, model } => {
            assert_eq!((project.as_str(), model), ("prj_structure", &None));
            "new-7".to_string()
        }
        other => panic!("first, a thread is made: {other:?}"),
    };
    // The thread is open (the fixture names it `new-<n>`) and its composer sent the prompt.
    let selected = app.cx.update(|cx| model.read(cx).selected().clone());
    let Selection::Thread(id) = selected else {
        panic!(
            "the new thread is open: {:?}",
            app.cx.update(|cx| model.read(cx).selected().clone())
        )
    };
    assert!(id.starts_with("new-"), "{id} ({created})");
    match &ops[1] {
        ComposerOp::Send {
            session,
            request,
            picks,
        } => {
            assert_eq!(session, &id);
            assert_eq!(request.text, "build it");
            assert_eq!(picks.mode, Some(Mode::Plan));
            assert_eq!(picks.effort, Some(Some("high".to_string())));
        }
        other => panic!("then the first prompt: {other:?}"),
    }
    assert_eq!(ops.len(), 2, "{ops:?}");
    // The new thread's composer is a thread's; its timeline shows the prompt when the turn starts.
    let second = app.composer();
    assert_eq!(
        app.cx.update(|cx| second.read(cx).target().clone()),
        Target::Thread(id.clone())
    );
    let admission = match &ops[1] {
        ComposerOp::Send { request, .. } => request.admission_id.clone(),
        _ => unreachable!(),
    };
    app.backend.emit(CoreEvent::TurnStarted {
        session_id: id,
        admission_id: admission,
    });
    app.settle();
    let thread = app
        .cx
        .update(|cx| model.read(cx).thread().cloned())
        .unwrap();
    let last = app
        .cx
        .update(|cx| match &thread.read(cx).rows().last().unwrap().kind {
            RowKind::User { text, .. } => text.to_string(),
            _ => String::new(),
        });
    assert_eq!(last, "build it");
}

#[core::prelude::v1::test]
fn a_new_thread_that_cannot_be_made_stays_on_the_screen_with_its_text() {
    let mut app = app();
    let model = app.model.clone();
    app.cx
        .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_structure", cx)));
    app.settle();
    app.backend.fail_with(Some("No provider is configured."));
    let composer = app.composer();
    app.cx
        .update(|cx| composer.update(cx, |c, cx| c.send("build it", false, cx)));
    app.settle();
    assert_eq!(
        app.cx.update(|cx| model.read(cx).selected().clone()),
        Selection::Draft("prj_structure".into())
    );
    assert_eq!(app.backend.composer_ops().len(), 1);
}

// --- the workspace: drafts and layout -------------------------------------------------------------

struct Ws {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    model: Entity<AppModel>,
    ws: Entity<Workspace>,
}

fn workspace(size_px: (f32, f32), state: UiState, data_dir: Option<std::path::PathBuf>) -> Ws {
    let mut app = app();
    let model = app.model.clone();
    let root_model = model.clone();
    let window = open_window(
        &mut app.cx,
        size(px(size_px.0), px(size_px.1)),
        move |window, cx| {
            cx.new(|cx| Workspace::with_model(state, data_dir, root_model, window, cx))
        },
    );
    let handle: AnyWindowHandle = window.into();
    let ws = app
        .cx
        .update_window(handle, |root, _, _| {
            root.downcast::<Workspace>().expect("the workspace")
        })
        .unwrap();
    Ws {
        cx: app.cx,
        window: handle,
        backend: app.backend,
        model,
        ws,
    }
}

impl Ws {
    fn settle(&mut self) {
        for _ in 0..3 {
            self.cx.run_until_parked();
            draw(&mut self.cx, self.window);
        }
    }

    fn open(&mut self, id: &str) {
        let model = self.model.clone();
        self.cx
            .update(|cx| model.update(cx, |m, cx| m.open(id, cx)));
        self.settle();
    }

    fn type_into_composer(&mut self, text: &str) {
        let ws = self.ws.clone();
        let view = self
            .cx
            .update(|cx| ws.read(cx).composer_view().cloned())
            .expect("a composer view");
        self.cx
            .update_window(self.window, |_, window, cx| {
                view.update(cx, |v, cx| v.set_text(text, window, cx))
            })
            .unwrap();
        self.settle();
    }

    fn composer_text(&mut self) -> String {
        let ws = self.ws.clone();
        self.cx.update(|cx| {
            let view = ws
                .read(cx)
                .composer_view()
                .cloned()
                .expect("a composer view");
            view.read(cx).text(cx)
        })
    }

    fn draft(&mut self, key: &str) -> Option<String> {
        let ws = self.ws.clone();
        self.cx
            .update(|cx| ws.read(cx).state().drafts.get(key).map(str::to_string))
    }
}

#[core::prelude::v1::test]
fn drafts_are_kept_per_thread_restored_on_return_saved_to_the_file_and_removed_when_empty() {
    let dir = tempdir().unwrap();
    let mut ws = workspace(
        (1100., 700.),
        UiState::default(),
        Some(dir.path().to_path_buf()),
    );
    ws.open("t1");
    assert_eq!(ws.composer_text(), "");
    ws.type_into_composer("half of t1");
    assert_eq!(ws.draft("t1").as_deref(), Some("half of t1"));
    // Another thread has its own composer, empty; its own draft.
    ws.open("t2");
    assert_eq!(ws.composer_text(), "", "t2 starts empty");
    ws.type_into_composer("t2 thing");
    assert_eq!(ws.draft("t2").as_deref(), Some("t2 thing"));
    // Back: what was typed is there.
    ws.open("t1");
    assert_eq!(ws.composer_text(), "half of t1");
    // The file has them (written debounced in the app; now, on quit).
    let wsc = ws.ws.clone();
    ws.cx.update(|cx| wsc.read(cx).save_now());
    let saved = UiState::load(dir.path());
    assert_eq!(saved.drafts.get("t1"), Some("half of t1"));
    assert_eq!(saved.drafts.get("t2"), Some("t2 thing"));
    // A new run starts from them.
    let mut again = workspace((1100., 700.), saved, None);
    again.open("t2");
    assert_eq!(again.composer_text(), "t2 thing");
    // Emptied: removed, not kept as "".
    ws.type_into_composer("");
    assert_eq!(ws.draft("t1"), None);
    ws.cx.update(|cx| wsc.read(cx).save_now());
    assert_eq!(UiState::load(dir.path()).drafts.get("t1"), None);
    assert_eq!(UiState::load(dir.path()).drafts.get("t2"), Some("t2 thing"));
}

#[core::prelude::v1::test]
fn a_sent_message_leaves_no_draft_behind() {
    let mut ws = workspace((1100., 700.), UiState::default(), None);
    ws.open("t1");
    ws.type_into_composer("send me");
    assert_eq!(ws.draft("t1").as_deref(), Some("send me"));
    let wsc = ws.ws.clone();
    let composer = ws
        .cx
        .update(|cx| ws.model.read(cx).composer().cloned())
        .unwrap();
    ws.cx
        .update_window(ws.window, |_, _, cx| {
            let _ = wsc;
            composer.update(cx, |c, cx| c.send("send me", false, cx))
        })
        .unwrap();
    ws.settle();
    assert_eq!(ws.composer_text(), "");
    assert_eq!(ws.draft("t1"), None);
}

#[core::prelude::v1::test]
fn drafts_of_threads_the_list_no_longer_has_are_dropped_after_a_week() {
    let now = crate::model::system_now_ms();
    let mut state = UiState::default();
    state
        .drafts
        .set("gone-old", "old", now - Drafts::STALE_MS - 1000);
    state.drafts.set("gone-recent", "recent", now - 1000);
    state
        .drafts
        .set("t2", "kept: still listed", now - 30 * 24 * 3600 * 1000);
    state
        .drafts
        .set("new:prj_missing", "old", now - Drafts::STALE_MS - 1000);
    state
        .drafts
        .set("new:prj_structure", "kept", now - Drafts::STALE_MS - 1000);
    let mut ws = workspace((1100., 700.), state, None);
    ws.open("t1");
    assert_eq!(ws.draft("gone-old"), None);
    assert_eq!(ws.draft("new:prj_missing"), None);
    assert_eq!(ws.draft("gone-recent").as_deref(), Some("recent"));
    assert_eq!(ws.draft("t2").as_deref(), Some("kept: still listed"));
    assert_eq!(ws.draft("new:prj_structure").as_deref(), Some("kept"));
}

#[core::prelude::v1::test]
fn the_composer_sits_at_the_bottom_of_the_thread_whatever_the_window_and_the_panels() {
    for window in [(640., 480.), (1100., 700.), (1920., 1080.)] {
        for (sidebar, right) in [(true, false), (false, false), (true, true), (false, true)] {
            let mut state = UiState::default();
            state.sidebar.open = sidebar;
            state.right_panel.open = right;
            let mut ws = workspace(window, state, None);
            ws.open("t1");
            let l = layout(&mut ws.cx, ws.window);
            let ctx = format!("{window:?} sidebar {sidebar} right {right}");
            let (main, card) = (l.bounds("main"), l.bounds("composer"));
            let mut check = l.check();
            check
                .inside("composer", "main")
                .inside("composer", "thread")
                .inside("composer-footer", "composer")
                .same_row("model-chip", "composer-send")
                .no_overflow("mode-chip/label");
            let failures = check.failures().to_vec();
            assert!(failures.is_empty(), "{ctx}: {failures:?}");
            // 16 px of margin each side at the least, centred in the main area, the lane's width
            // at the most, 16 px above the main area's bottom.
            let (left, right_gap) = (
                (card.left() - main.left()).as_f32(),
                (main.right() - card.right()).as_f32(),
            );
            assert!(
                left >= 15.9 && right_gap >= 15.9,
                "{ctx}: margins {left} {right_gap}"
            );
            assert!((left - right_gap).abs() < 1., "{ctx}: centred");
            assert!(card.size.width.as_f32() <= super::view::LANE + 0.5, "{ctx}");
            assert!(
                (main.bottom() - card.bottom()).as_f32() > 15.5
                    && (main.bottom() - card.bottom()).as_f32() < 16.5,
                "{ctx}: bottom"
            );
            // The timeline above it is still there and above the card.
            let timeline = l.bounds("timeline");
            assert!(timeline.bottom() <= card.top() + px(1.), "{ctx}");
        }
    }
}

#[core::prelude::v1::test]
fn the_new_thread_screen_centres_the_headline_and_the_composer() {
    for window in [(640., 480.), (1100., 700.), (1920., 1080.)] {
        let mut ws = workspace(window, UiState::default(), None);
        let model = ws.model.clone();
        ws.cx
            .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_structure", cx)));
        ws.settle();
        let l = layout(&mut ws.cx, ws.window);
        let (main, card, head) = (
            l.bounds("main"),
            l.bounds("composer"),
            l.bounds("new-thread-headline"),
        );
        let mut check = l.check();
        check
            .inside("composer", "new-thread-screen")
            .inside("new-thread-headline", "new-thread-screen")
            .inside("composer", "main");
        check.assert();
        assert!(
            head.bottom() <= card.top(),
            "{window:?}: the headline is above the card"
        );
        // Centred together: the group's middle is the main area's middle.
        let middle = (head.top().as_f32() + card.bottom().as_f32()) / 2.;
        let want = (main.top().as_f32() + main.bottom().as_f32()) / 2.;
        assert!(
            (middle - want).abs() < 40.,
            "{window:?}: {middle} vs {want}"
        );
        assert!(
            ((card.center().x - main.center().x).as_f32()).abs() < 1.,
            "{window:?}: centred across"
        );
    }
}

#[core::prelude::v1::test]
fn the_new_thread_screen_keeps_its_own_draft_per_project() {
    let mut ws = workspace((1100., 700.), UiState::default(), None);
    let model = ws.model.clone();
    ws.cx
        .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_structure", cx)));
    ws.settle();
    ws.type_into_composer("an idea for structure");
    assert_eq!(
        ws.draft("new:prj_structure").as_deref(),
        Some("an idea for structure")
    );
    ws.cx
        .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_engine", cx)));
    ws.settle();
    assert_eq!(ws.composer_text(), "");
    ws.cx
        .update(|cx| model.update(cx, |m, cx| m.new_thread("prj_structure", cx)));
    ws.settle();
    assert_eq!(ws.composer_text(), "an idea for structure");
    let _ = &ws.backend;
}

#[core::prelude::v1::test]
fn a_draft_with_a_mention_is_kept_as_text_and_comes_back_with_its_chip() {
    let mut ws = workspace((1100., 700.), UiState::default(), None);
    ws.open("t1");
    ws.type_into_composer("see @file:src/main.rs now");
    assert_eq!(ws.draft("t1").as_deref(), Some("see @file:src/main.rs now"));
    ws.open("t2");
    ws.open("t1");
    assert_eq!(ws.composer_text(), "see @file:src/main.rs now");
    let wsc = ws.ws.clone();
    let chips = ws.cx.update(|cx| {
        let view = wsc.read(cx).composer_view().cloned().unwrap();
        view.read(cx).input().read(cx).tokens().len()
    });
    assert_eq!(chips, 1, "the chip is back");
}

#[core::prelude::v1::test]
fn an_app_command_typed_in_the_composer_reaches_the_frame() {
    let mut ws = workspace((1100., 700.), UiState::default(), None);
    ws.open("t1");
    let wsc = ws.ws.clone();
    let composer = ws
        .cx
        .update(|cx| ws.model.read(cx).composer().cloned())
        .unwrap();
    // `/settings` is not built yet (Phase 7): the frame says so in a toast, as the palette does.
    ws.cx.update(|cx| composer.update(cx, |c, cx| c.send("/settings", false, cx)));
    ws.settle();
    let titles: Vec<String> = ws.cx.update(|cx| {
        wsc.read(cx)
            .toasts()
            .all()
            .iter()
            .map(|t| t.spec.title.clone())
            .collect()
    });
    assert!(titles.iter().any(|t| t == "Settings"), "{titles:?}");
}
