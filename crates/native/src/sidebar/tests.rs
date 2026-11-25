//! The sidebar, headless: layout of its rows over fixture data (heights, alignment of the status
//! icons, folder headers against their rows, nothing overflowing), and behaviour through real key
//! presses and clicks (keyboard navigation, rename, delete with its question, archive, pin,
//! move to a project, the filter, closed folders) checking which operation reaches the backend;
//! then [`CoreBackend`] on a real core over a temporary data folder, the model's reaction to core
//! events, and the app lock.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, Window,
    test::{TestEventExt as _, TestWindowExt as _},
};
use hermes_core::CoreEvent;

use super::data::{NO_PROJECT, Op, RowKey, Snapshot};
use super::*;
use crate::model::{
    AppModel, FixtureBackend, ModelEvent, Selection,
    fixture::{self, NOW_MS},
};
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    model: Entity<AppModel>,
    sidebar: Entity<Sidebar>,
}

fn rig_with(snapshot: Snapshot, size: (f32, f32), theme: ThemeChoice) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(theme, cx);
        keys::bind(cx);
    });
    let backend = FixtureBackend::new(snapshot.clone());
    let model = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS))
        })
    };
    let sidebar = {
        let model = model.clone();
        cx.update(|cx| cx.new(|cx| Sidebar::new(model, Default::default(), cx)))
    };
    let root = sidebar.clone();
    let window = open_window(
        &mut cx,
        gpui_kit::size(gpui_kit::px(size.0), gpui_kit::px(size.1)),
        move |_, _| root,
    );
    let window: AnyWindowHandle = window.into();
    let mut rig = Rig {
        cx,
        window,
        backend,
        model,
        sidebar,
    };
    rig.settle();
    rig
}

fn rig() -> Rig {
    rig_with(fixture::snapshot(), (256., 720.), ThemeChoice::T3Dark)
}

impl Rig {
    /// Lets background work finish and draws.
    fn settle(&mut self) {
        self.cx.run_until_parked();
        draw(&mut self.cx, self.window);
    }

    fn layout(&mut self) -> Layout {
        layout(&mut self.cx, self.window)
    }

    fn with<R>(
        &mut self,
        f: impl FnOnce(&mut Sidebar, &mut Window, &mut gpui_kit::Context<Sidebar>) -> R,
    ) -> R {
        let sidebar = self.sidebar.clone();
        let r = self
            .cx
            .update_window(self.window, |_, window, cx| {
                sidebar.update(cx, |s, cx| f(s, window, cx))
            })
            .expect("update the sidebar");
        self.settle();
        r
    }

    fn press(&mut self, keys: &str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.press(keys, cx))
            .unwrap();
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.input(text, cx))
            .unwrap();
        self.settle();
    }

    fn focus(&mut self) {
        self.with(|s, window, cx| s.focus(window, cx));
    }

    fn ops(&self) -> Vec<Op> {
        self.backend.ops()
    }

    fn selected(&mut self) -> Selection {
        let model = self.model.clone();
        self.cx.update(|cx| model.read(cx).selected().clone())
    }

    fn cursor(&mut self) -> Option<RowKey> {
        self.with(|s, _, _| s.cursor().cloned())
    }

    fn snapshot(&mut self) -> Snapshot {
        let model = self.model.clone();
        self.cx.update(|cx| model.read(cx).snapshot().clone())
    }

    fn click(&mut self, name: &str) {
        let center = self.layout().bounds(name).center();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.pointer_move(center, None, cx);
                window.pointer_down(center, gpui_kit::MouseButton::Left, cx);
                window.pointer_up(center, gpui_kit::MouseButton::Left, cx);
            })
            .unwrap();
        self.settle();
    }
}

// --- layout ---------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn rows_have_t3codes_heights_and_line_up_with_their_folder() {
    let mut rig = rig();
    let layout = rig.layout();
    let threads = [
        "thread-t1",
        "thread-t2",
        "thread-t3",
        "thread-t4",
        "thread-t5",
    ];
    let mut check = layout.check();
    check.same_height(&threads);
    check.same_height(&[
        "folder-prj_structure",
        "folder-prj_engine",
        "folder-prj_hermes",
    ]);
    check.assert();
    assert_eq!(
        layout.bounds("thread-t1").size.height.as_f32(),
        super::ROW_HEIGHT
    );
    assert_eq!(
        layout.bounds("folder-prj_engine").size.height.as_f32(),
        super::FOLDER_HEIGHT
    );
    // The title starts 36 px into its row (under the folder's name), the folder's name 10 + 16 + 10.
    let row = layout.bounds("thread-t1");
    let title = layout.bounds("title-t1");
    assert_eq!(
        title.origin.x.as_f32() - row.origin.x.as_f32(),
        super::ROW_INSET
    );
    let folder = layout.bounds("folder-prj_structure");
    let name = layout.bounds("name-prj_structure");
    assert_eq!(
        name.origin.x.as_f32() - folder.origin.x.as_f32(),
        super::ROW_PAD + 16. + 10.
    );
    // Rows and headers share their left edge and width.
    let edges = [
        layout.bounds("thread-t1"),
        layout.bounds("thread-t4"),
        layout.bounds("folder-prj_hermes"),
    ];
    for b in &edges {
        assert_eq!(b.origin.x, edges[0].origin.x);
        assert_eq!(b.size.width, edges[0].size.width);
    }
    // Inside the sidebar, and nothing clipped.
    let mut check = layout.check();
    check
        .inside("thread-t1", "sidebar")
        .inside("title-t1", "thread-t1")
        .inside("slot-t1", "thread-t1")
        .no_overflow("name-prj_hermes")
        .same_row("title-t1", "slot-t1");
    check.assert();
}

#[core::prelude::v1::test]
fn status_icons_and_times_line_up_down_the_list() {
    let mut snapshot = fixture::snapshot();
    snapshot.threads[0].working = true; // t1
    snapshot.threads[1].waiting = true; // t2
    snapshot.threads[3].working = true; // t4
    let mut rig = rig_with(snapshot, (256., 720.), ThemeChoice::T3Dark);
    let layout = rig.layout();
    let icons = ["status-icon-t1", "status-icon-t2", "status-icon-t4"];
    let first = layout.bounds(icons[0]);
    for name in icons {
        assert_eq!(layout.bounds(name).size, first.size, "{name}");
    }
    // A status and a time end at the same right edge (the slot is right-aligned).
    let right = |name: &str| {
        let b = layout.bounds(name);
        (b.origin.x + b.size.width).as_f32()
    };
    let time = right("time-t3");
    for status in ["status-t1", "status-t2", "status-t4"] {
        assert!((right(status) - time).abs() < 0.6, "{status}");
    }
    // The icon sits level with its word, and the status level with the title.
    let mut check = layout.check();
    check
        .same_row("status-icon-t1", "status-word-t1")
        .same_row("status-t1", "title-t1");
    check.assert();
}

#[core::prelude::v1::test]
fn a_long_title_is_cut_before_the_slot_and_never_pushes_it() {
    let mut snapshot = fixture::snapshot();
    snapshot.threads[3].title =
        "An extremely long title that cannot possibly fit in a 256 pixel sidebar row".into();
    snapshot.threads[3].working = true;
    let mut rig = rig_with(snapshot, (256., 720.), ThemeChoice::T3Light);
    let layout = rig.layout();
    let mut check = layout.check();
    check
        .inside("title-t4", "thread-t4")
        .inside("slot-t4", "thread-t4");
    check.assert();
    let title = layout.bounds("title-t4");
    let slot = layout.bounds("slot-t4");
    assert!(title.origin.x + title.size.width <= slot.origin.x + gpui_kit::px(0.6));
}

#[core::prelude::v1::test]
fn pinned_threads_are_listed_first_with_their_project_tile() {
    let mut snapshot = fixture::snapshot();
    snapshot.threads[2].pinned = true; // t3 "hi"
    let mut rig = rig_with(snapshot, (256., 720.), ThemeChoice::T3Dark);
    let layout = rig.layout();
    let pinned = layout.bounds("thread-t3");
    let first_folder = layout.bounds("folder-prj_structure");
    assert!(
        pinned.origin.y < first_folder.origin.y,
        "pinned above the folders"
    );
    assert!(layout.get("pinned-label").is_ok());
    assert!(layout.get("pin-t3").is_ok());
    // A pinned row is not indented like a thread in a folder: its tile takes the place.
    let row = layout.bounds("thread-t3");
    let tile = layout.bounds("monogram-t3");
    assert_eq!(
        tile.origin.x.as_f32() - row.origin.x.as_f32(),
        super::ROW_PAD
    );
}

#[core::prelude::v1::test]
fn the_empty_project_says_so_and_no_project_is_last() {
    let mut rig = rig();
    let layout = rig.layout();
    assert!(layout.get("empty-prj_docs").is_ok());
    let none = layout.bounds(&format!("folder-{NO_PROJECT}"));
    for folder in ["prj_structure", "prj_engine", "prj_hermes", "prj_docs"] {
        assert!(layout.bounds(&format!("folder-{folder}")).origin.y < none.origin.y);
    }
}

// --- behaviour ------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn arrow_keys_move_the_cursor_and_enter_opens_the_thread() {
    let mut rig = rig();
    rig.focus();
    rig.press("down");
    let first = rig.cursor().expect("a cursor");
    assert!(matches!(first, RowKey::Folder(_)));
    rig.press("down");
    let second = rig.cursor().unwrap();
    assert!(matches!(second, RowKey::Thread(_)));
    rig.press("enter");
    let RowKey::Thread(id) = second else {
        unreachable!()
    };
    assert_eq!(rig.selected(), Selection::Thread(id));
    rig.press("up");
    assert_eq!(rig.cursor(), Some(first));
    rig.press("end");
    assert!(
        matches!(rig.cursor(), Some(RowKey::Thread(_))),
        "the last row is a thread"
    );
    rig.press("home");
    assert!(matches!(rig.cursor(), Some(RowKey::Folder(_))));
}

#[core::prelude::v1::test]
fn left_and_right_close_and_open_a_folder_and_the_choice_is_reported() {
    let mut rig = rig();
    let events = Rc::new(RefCell::new(Vec::new()));
    {
        let events = events.clone();
        let sidebar = rig.sidebar.clone();
        rig.cx.update(|cx| {
            cx.subscribe(&sidebar, move |_, event: &SidebarEvent, _| {
                events.borrow_mut().push(event.clone());
            })
            .detach();
        });
    }
    rig.focus();
    rig.press("down"); // the first folder: hermes-rs (its newest thread is the newest)
    let RowKey::Folder(project) = rig.cursor().unwrap() else {
        panic!("a folder")
    };
    rig.press("left");
    assert!(rig.with(|s, _, _| s.prefs().collapsed.contains(&project)));
    assert!(matches!(
        events.borrow().last(),
        Some(SidebarEvent::PrefsChanged(p)) if p.collapsed.contains(&project)
    ));
    let layout = rig.layout();
    assert!(
        layout.get(&format!("count-{project}")).is_ok(),
        "a closed folder counts"
    );
    assert!(layout.get("thread-t4").is_err() || project != "prj_hermes");
    rig.press("right");
    assert!(!rig.with(|s, _, _| s.prefs().collapsed.contains(&project)));
    assert!(rig.layout().get(&format!("count-{project}")).is_err());
}

#[core::prelude::v1::test]
fn f2_renames_in_place_and_enter_sends_the_new_title() {
    let mut rig = rig();
    rig.focus();
    rig.with(|s, _, cx| s.set_cursor(Some(RowKey::Thread("t2".into())), cx));
    rig.press("f2");
    assert_eq!(
        rig.with(|s, _, _| s.renaming().cloned()),
        Some(RenameTarget::Thread("t2".into()))
    );
    assert!(rig.layout().get("rename-t2").is_ok());
    rig.press("ctrl-a");
    rig.type_text("Shiny new name");
    rig.press("enter");
    assert_eq!(
        rig.ops(),
        vec![Op::Rename {
            id: "t2".into(),
            title: "Shiny new name".into()
        }]
    );
    assert_eq!(rig.snapshot().thread("t2").unwrap().title, "Shiny new name");
    assert!(rig.with(|s, _, _| s.renaming().is_none()));
}

#[core::prelude::v1::test]
fn escape_cancels_a_rename_and_an_unchanged_or_empty_name_sends_nothing() {
    let mut rig = rig();
    rig.focus();
    rig.with(|s, window, cx| s.start_rename_thread("t2", window, cx));
    rig.press("ctrl-a");
    rig.type_text("Dropped");
    rig.press("escape");
    assert!(rig.ops().is_empty());
    assert_eq!(rig.snapshot().thread("t2").unwrap().title, "ayo");
    rig.with(|s, window, cx| s.start_rename_thread("t2", window, cx));
    rig.press("enter"); // the same name
    assert!(rig.ops().is_empty());
    rig.with(|s, window, cx| s.start_rename_thread("t2", window, cx));
    rig.press("ctrl-a");
    rig.press("backspace");
    rig.press("enter"); // empty
    assert!(rig.ops().is_empty());
}

#[core::prelude::v1::test]
fn delete_asks_first_and_only_the_answer_deletes() {
    let mut rig = rig();
    rig.focus();
    rig.with(|s, _, cx| s.open_thread("t5", cx));
    rig.press("delete");
    assert!(rig.ops().is_empty(), "nothing is deleted by the key alone");
    let confirm = rig
        .with(|s, _, _| s.confirming().cloned())
        .expect("a question");
    assert!(confirm.title.contains("Port the command palette"));
    assert!(rig.layout().get("confirm").is_ok());
    rig.press("escape");
    assert!(rig.ops().is_empty() && rig.with(|s, _, _| s.confirming().is_none()));
    rig.press("delete");
    rig.click("confirm-accept");
    assert_eq!(rig.ops(), vec![Op::Delete { id: "t5".into() }]);
    assert!(rig.snapshot().thread("t5").is_none());
    assert_eq!(rig.selected(), Selection::None, "the open thread is gone");
}

#[core::prelude::v1::test]
fn archive_pin_and_move_reach_the_backend_as_the_right_operations() {
    let mut rig = rig();
    rig.with(|s, _, cx| s.archive("t1", cx));
    rig.with(|s, _, cx| s.toggle_pin("t2", cx));
    rig.with(|s, _, cx| s.toggle_pin("t2", cx));
    rig.with(|s, _, cx| s.move_to_project("t3", "prj_engine", cx));
    // Not moves: the same project, a group without a folder.
    rig.with(|s, _, cx| s.move_to_project("t4", "prj_hermes", cx));
    rig.with(|s, _, cx| s.move_to_project("t4", NO_PROJECT, cx));
    assert_eq!(
        rig.ops(),
        vec![
            Op::Archive { id: "t1".into() },
            Op::Pin {
                id: "t2".into(),
                pinned: true
            },
            Op::Pin {
                id: "t2".into(),
                pinned: false
            },
            Op::Move {
                id: "t3".into(),
                project: "prj_engine".into()
            },
        ]
    );
    let snapshot = rig.snapshot();
    assert!(snapshot.thread("t1").is_none());
    assert_eq!(snapshot.thread("t3").unwrap().project, "prj_engine");
}

#[core::prelude::v1::test]
fn dragging_a_thread_onto_a_project_header_moves_it() {
    let mut rig = rig();
    let layout = rig.layout();
    let from = layout.bounds("thread-t3").center();
    let to = layout.bounds("folder-prj_engine").center();
    rig.cx
        .update_window(rig.window, |_, window, cx| window.drag(from, to, cx))
        .unwrap();
    rig.settle();
    assert_eq!(
        rig.ops(),
        vec![Op::Move {
            id: "t3".into(),
            project: "prj_engine".into()
        }]
    );
}

fn record(rig: &mut Rig) -> Rc<RefCell<Vec<ModelEvent>>> {
    let toasts = Rc::new(RefCell::new(Vec::new()));
    let (sink, model) = (toasts.clone(), rig.model.clone());
    rig.cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &ModelEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    toasts
}

#[core::prelude::v1::test]
fn a_failed_edit_shows_a_toast_and_the_list_goes_back() {
    let mut rig = rig();
    let toasts = record(&mut rig);
    rig.backend.fail_with(Some("disk full"));
    rig.with(|s, _, cx| s.archive("t1", cx));
    {
        let toasts = toasts.borrow();
        let ModelEvent::Toast { spec, .. } = &toasts[0] else {
            panic!("a toast");
        };
        assert!(spec.title.contains("archive"));
        assert_eq!(spec.description.as_deref(), Some("disk full"));
    }
    // The backend did not apply it: the re-read puts the thread back.
    assert!(rig.snapshot().thread("t1").is_some());
}

#[core::prelude::v1::test]
fn the_filter_narrows_by_title_and_closes_with_escape() {
    let mut rig = rig();
    rig.focus();
    rig.press("ctrl-f");
    assert!(rig.with(|s, _, _| s.filter_open()));
    assert!(rig.layout().get("filter-box").is_ok());
    rig.type_text("palette");
    assert_eq!(rig.with(|s, _, cx| s.filter_text(cx)), "palette");
    let layout = rig.layout();
    assert!(layout.get("thread-t5").is_ok());
    assert!(layout.get("thread-t1").is_err());
    rig.press("enter"); // the first match opens
    assert_eq!(rig.selected(), Selection::Thread("t5".into()));
    rig.with(|s, window, cx| s.set_filter("zzz", window, cx));
    assert!(rig.layout().get("list-empty").is_ok());
    rig.with(|s, window, cx| s.on_cancel(window, cx));
    assert!(!rig.with(|s, _, _| s.filter_open()));
    assert!(rig.layout().get("thread-t1").is_ok());
}

#[core::prelude::v1::test]
fn a_project_is_added_renamed_and_removed_through_the_model() {
    let mut rig = rig();
    rig.with(|s, _, cx| s.add_project(r"C:\work\new-thing".into(), cx));
    rig.with(|s, window, cx| s.start_rename_project("prj_docs", window, cx));
    rig.press("ctrl-a");
    rig.type_text("manuals");
    rig.press("enter");
    rig.with(|s, _, cx| s.request_remove_project("prj_engine", cx));
    rig.click("confirm-accept");
    assert_eq!(
        rig.ops(),
        vec![
            Op::AddProject {
                folder: r"C:\work\new-thing".into(),
                name: "new-thing".into()
            },
            Op::RenameProject {
                id: "prj_docs".into(),
                name: "manuals".into()
            },
            Op::RemoveProject {
                id: "prj_engine".into()
            },
        ]
    );
    assert!(
        rig.snapshot()
            .projects
            .iter()
            .any(|p| p.name == "new-thing")
    );
    // Only the user's own projects can be renamed.
    rig.with(|s, window, cx| s.start_rename_project(NO_PROJECT, window, cx));
    assert!(rig.with(|s, _, _| s.renaming().is_none()));
}

#[core::prelude::v1::test]
fn show_more_lists_the_rest_of_a_big_folder() {
    let mut rig = rig_with(fixture::many(12), (256., 900.), ThemeChoice::T3Dark);
    let layout = rig.layout();
    assert!(layout.get("thread-m7").is_ok());
    assert!(layout.get("thread-m8").is_err());
    rig.click("more-prj_big");
    let layout = rig.layout();
    assert!(layout.get("thread-m11").is_ok());
    assert!(layout.get("more-prj_big").is_err());
}

// --- the model and the core's events --------------------------------------------------------

#[core::prelude::v1::test]
fn core_events_set_the_marks_and_a_few_become_toasts() {
    let mut rig = rig();
    let toasts = record(&mut rig);
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t2".into(),
        admission_id: "a".into(),
    });
    rig.backend.emit(CoreEvent::ApprovalRequested {
        session_id: "t3".into(),
        prompt_id: "p".into(),
        command: "rm".into(),
        description: String::new(),
        choices: vec![],
    });
    rig.settle();
    let layout = rig.layout();
    assert!(layout.get("status-word-t2").is_ok(), "working");
    assert!(layout.get("status-word-t3").is_ok(), "approval");
    rig.backend.emit(CoreEvent::Settled {
        session_id: "t3".into(),
        prompt_id: "p".into(),
        kind: hermes_core::QuestionKind::Approval,
    });
    rig.backend.emit(CoreEvent::TitleChanged {
        session_id: "t1".into(),
        title: "A fresh title".into(),
    });
    rig.backend.emit(CoreEvent::QueuedFailed {
        session_id: "t2".into(),
        admission_id: "a".into(),
        error: "Another turn is running.".into(),
    });
    rig.backend.emit(CoreEvent::CuratorFinished {
        ok: true,
        lines: vec!["Merged 2 skills".into()],
    });
    rig.backend.emit(CoreEvent::CuratorFinished {
        ok: false,
        lines: vec!["no model".into()],
    });
    rig.backend.emit(CoreEvent::ReviewFinished {
        session_id: "t1".into(),
        actions: vec!["saved a note".into()],
        requested: false,
    });
    rig.settle();
    assert!(rig.layout().get("status-word-t3").is_err(), "settled");
    assert_eq!(rig.snapshot().thread("t1").unwrap().title, "A fresh title");
    let toasts = toasts.borrow();
    let titles: Vec<String> = toasts
        .iter()
        .filter_map(|event| match event {
            ModelEvent::Toast { spec, .. } => Some(spec.title.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        titles,
        [
            "A queued prompt could not start",
            "The skills curator finished",
            "The skills curator failed",
            "Review finished"
        ]
    );
    let Some(ModelEvent::Toast { open, .. }) = toasts
        .iter()
        .find(|e| matches!(e, ModelEvent::Toast { .. }))
    else {
        panic!("a toast");
    };
    assert_eq!(open.as_deref(), Some("t2"), "its action opens the thread");
}

#[core::prelude::v1::test]
fn opening_a_thread_and_a_new_thread_choose_what_the_main_area_shows() {
    let mut rig = rig();
    rig.with(|s, _, cx| s.new_thread("prj_engine", cx));
    assert_eq!(rig.selected(), Selection::Draft("prj_engine".into()));
    rig.with(|s, _, cx| s.new_thread_here(cx));
    assert_eq!(
        rig.selected(),
        Selection::Draft("prj_engine".into()),
        "in the same project"
    );
    rig.with(|s, _, cx| s.open_thread("nope", cx));
    assert_eq!(
        rig.selected(),
        Selection::Draft("prj_engine".into()),
        "an unknown id is ignored"
    );
}

// --- the app lock ---------------------------------------------------------------------------

#[cfg(windows)]
#[core::prelude::v1::test]
fn a_second_app_on_the_data_folder_is_told_hermes_is_open() {
    let dir = hermes_testdir::tempdir().unwrap();
    let first = hermes_core::AppLock::acquire(dir.path()).unwrap();
    assert!(first.is_held());
    let refused = hermes_core::AppLock::acquire(dir.path())
        .err()
        .expect("refused");
    assert!(refused.to_string().contains("Hermes is already open"));
    drop(first);
    assert!(hermes_core::AppLock::acquire(dir.path()).unwrap().is_held());
}

// --- review (CP3.1): ordering, focus, side data ---------------------------------------------

/// A backend whose first edit is slow: edits run in parallel would reach it out of order.
struct SlowFirst {
    inner: Arc<FixtureBackend>,
    seen: std::sync::atomic::AtomicUsize,
}

impl crate::model::Backend for SlowFirst {
    fn snapshot(&self) -> Result<Snapshot, String> {
        self.inner.snapshot()
    }

    fn run(&self, op: &Op) -> Result<(), String> {
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        self.inner.run(op)
    }
}

#[core::prelude::v1::test]
fn edits_reach_the_backend_one_at_a_time_in_the_users_order() {
    let mut cx = headless_app();
    let snapshot = fixture::snapshot();
    let inner = FixtureBackend::new(snapshot.clone());
    let backend = Arc::new(SlowFirst {
        inner: inner.clone(),
        seen: Default::default(),
    });
    let model = cx.update(|cx| cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx)));
    for title in ["Second name", "Third name"] {
        let model = model.clone();
        cx.update(|cx| {
            model.update(cx, |m, cx| {
                m.apply(
                    Op::Rename {
                        id: "t1".into(),
                        title: title.into(),
                    },
                    cx,
                )
            })
        });
    }
    assert_eq!(cx.update(|cx| model.read(cx).pending_edits()), 2);
    for _ in 0..50 {
        cx.run_until_parked();
        if cx.update(|cx| model.read(cx).pending_edits()) == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let titles: Vec<String> = inner
        .ops()
        .into_iter()
        .map(|op| match op {
            Op::Rename { title, .. } => title,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(titles, ["Second name", "Third name"]);
    assert_eq!(
        crate::model::Backend::snapshot(&*inner)
            .unwrap()
            .thread("t1")
            .unwrap()
            .title,
        "Third name"
    );
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| model
            .read(cx)
            .snapshot()
            .thread("t1")
            .unwrap()
            .title
            .clone()),
        "Third name",
        "the re-read after the last edit shows the last name"
    );
}

#[core::prelude::v1::test]
fn archiving_the_row_under_the_cursor_moves_the_cursor_to_its_neighbour() {
    let mut rig = rig();
    rig.focus();
    rig.with(|s, _, cx| s.set_cursor(Some(RowKey::Thread("t1".into())), cx));
    let keys = rig.with(|s, _, cx| s.tree(cx).keys());
    let at = keys
        .iter()
        .position(|k| *k == RowKey::Thread("t1".into()))
        .unwrap();
    let next = keys[at + 1].clone();
    rig.with(|s, _, cx| s.archive("t1", cx));
    assert_eq!(rig.cursor(), Some(next));
}

#[core::prelude::v1::test]
fn removing_a_project_forgets_its_colour_and_closed_folder() {
    let mut rig = rig();
    rig.with(|s, _, cx| s.set_project_color("prj_engine", Some("red"), cx));
    rig.with(|s, _, cx| s.set_folder_closed("prj_engine", true, cx));
    rig.with(|s, _, cx| s.set_project_color("prj_docs", Some("lime"), cx));
    rig.with(|s, _, cx| s.request_remove_project("prj_engine", cx));
    rig.click("confirm-accept");
    let prefs = rig.with(|s, _, _| s.prefs().clone());
    assert!(!prefs.colors.contains_key("prj_engine"));
    assert!(!prefs.collapsed.contains("prj_engine"));
    assert_eq!(
        prefs.colors.get("prj_docs").map(String::as_str),
        Some("lime")
    );
}

#[core::prelude::v1::test]
fn a_folder_that_already_is_a_project_is_not_added_again() {
    let mut rig = rig();
    // The fixture's engine is C:\work\engine: another case and a trailing separator are the same.
    rig.with(|s, _, cx| s.add_project(r"c:\WORK\engine\".into(), cx));
    assert!(rig.ops().is_empty());
    assert_eq!(rig.cursor(), Some(RowKey::Folder("prj_engine".into())));
}

struct Unreadable;

impl crate::model::Backend for Unreadable {
    fn snapshot(&self) -> Result<Snapshot, String> {
        Err("database disk image is malformed".into())
    }

    fn run(&self, _: &Op) -> Result<(), String> {
        Err("no".into())
    }
}

#[core::prelude::v1::test]
fn a_store_that_cannot_be_read_says_so_instead_of_an_empty_list() {
    let mut cx = headless_app();
    let model = cx.update(|cx| cx.new(|_| AppModel::starting()));
    {
        let model = model.clone();
        cx.update(|cx| model.update(cx, |m, cx| m.connect(Arc::new(Unreadable), cx)));
    }
    cx.run_until_parked();
    let phase = cx.update(|cx| model.read(cx).phase().clone());
    assert_eq!(
        phase,
        crate::model::Phase::Failed("database disk image is malformed".into())
    );
}

// --- the real core --------------------------------------------------------------------------

mod core_backend {
    use hermes_core::{Core, NewSession};
    use hermes_runtime::{ProviderConfig, RuntimeConfig};

    use super::super::data::{NO_PROJECT, Op, ProjectKind};
    use crate::model::{Backend, CoreBackend};

    struct Fx {
        core: std::sync::Arc<Core>,
        backend: CoreBackend,
        // After the core: the runtime holds files in these until it drops. The folders last.
        _runtime: tokio::runtime::Runtime,
        work: hermes_testdir::TestDir,
        _data: hermes_testdir::TestDir,
    }

    fn fx() -> Fx {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let data = hermes_testdir::tempdir().unwrap();
        let config = RuntimeConfig {
            data_dir: data.path().to_path_buf(),
            ..RuntimeConfig::for_current_user().unwrap()
        };
        let core = Core::new(config, runtime.handle().clone()).unwrap();
        core.start(false);
        Fx {
            backend: CoreBackend::new(core.clone()),
            core,
            work: hermes_testdir::tempdir().unwrap(),
            _runtime: runtime,
            _data: data,
        }
    }

    fn session(f: &Fx, folder: &std::path::Path, title: &str) -> String {
        let (record, _) = f
            .core
            .create_session(NewSession {
                provider: ProviderConfig {
                    id: "mock".into(),
                    base_url: "http://127.0.0.1:1/v1".into(),
                    api_key: None,
                },
                model: "mock".into(),
                cwd: folder.to_path_buf(),
                title: Some(title.into()),
                seeded: vec![],
                parent: None,
            })
            .unwrap();
        record.id
    }

    #[core::prelude::v1::test]
    fn the_core_backend_groups_edits_and_reads_back() {
        let f = fx();
        let one = f.work.path().join("one");
        let two = f.work.path().join("two");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let canon = |p: &std::path::Path| {
            let c = std::fs::canonicalize(p).unwrap().display().to_string();
            // Without the verbatim prefix canonicalize adds.
            c.trim_start_matches(['\\', '?']).to_string()
        };
        let (one_s, two_s) = (canon(&one), canon(&two));
        let a = session(&f, std::path::Path::new(&one_s), "First");
        let b = session(&f, std::path::Path::new(&two_s), "Second");
        let c = session(&f, f.work.path(), "Third");

        f.backend
            .run(&Op::AddProject {
                folder: one_s,
                name: "one".into(),
            })
            .unwrap();
        f.backend
            .run(&Op::AddProject {
                folder: two_s,
                name: "two".into(),
            })
            .unwrap();
        let snap = f.backend.snapshot().unwrap();
        let project = |name: &str| {
            snap.projects
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .clone()
        };
        assert_eq!(snap.thread(&a).unwrap().project, project("one").id);
        assert_eq!(snap.thread(&b).unwrap().project, project("two").id);
        assert_eq!(project("one").kind, ProjectKind::User);
        let (one_id, two_id) = (project("one").id, project("two").id);

        f.backend
            .run(&Op::Rename {
                id: a.clone(),
                title: "Renamed".into(),
            })
            .unwrap();
        f.backend
            .run(&Op::Pin {
                id: a.clone(),
                pinned: true,
            })
            .unwrap();
        let snap = f.backend.snapshot().unwrap();
        assert_eq!(snap.thread(&a).unwrap().title, "Renamed");
        assert!(snap.thread(&a).unwrap().pinned);

        // Move to a project: the thread's folder becomes the project's.
        f.backend
            .run(&Op::Move {
                id: a.clone(),
                project: two_id.clone(),
            })
            .unwrap();
        let snap = f.backend.snapshot().unwrap();
        assert_eq!(
            snap.thread(&a).unwrap().project,
            two_id,
            "{:?}",
            snap.thread(&a)
        );

        f.backend
            .run(&Op::RenameProject {
                id: two_id,
                name: "deux".into(),
            })
            .unwrap();
        assert!(
            f.backend
                .snapshot()
                .unwrap()
                .projects
                .iter()
                .any(|p| p.name == "deux")
        );

        // Archive leaves the list (the session stays); delete removes the session.
        f.backend.run(&Op::Archive { id: b.clone() }).unwrap();
        f.backend.run(&Op::Delete { id: c.clone() }).unwrap();
        let snap = f.backend.snapshot().unwrap();
        assert!(snap.thread(&b).is_none() && snap.thread(&c).is_none());
        assert!(f.core.runtime().store().session(&c).unwrap().is_none());
        assert!(f.core.runtime().store().session(&b).unwrap().is_some());

        // Removing a project keeps its threads.
        f.backend.run(&Op::RemoveProject { id: one_id }).unwrap();
        assert!(f.backend.snapshot().unwrap().thread(&a).is_some());

        // Failures are messages, not panics.
        assert!(
            f.backend
                .run(&Op::Archive {
                    id: "missing".into()
                })
                .is_err()
        );
        assert!(
            f.backend
                .run(&Op::Move {
                    id: a,
                    project: NO_PROJECT.into()
                })
                .is_err()
        );
    }

    /// The app's path end to end on a real core: the model (as the sidebar drives it) sends the
    /// edits, they land in the store, and a core opened afterwards on the same folder (a restart)
    /// lists them.
    #[core::prelude::v1::test]
    fn edits_through_the_model_survive_a_restart() {
        use gpui_kit::AppContext as _;

        use crate::model::AppModel;

        let f = fx();
        let one = f.work.path().join("one");
        std::fs::create_dir_all(&one).unwrap();
        let one_s = std::fs::canonicalize(&one)
            .unwrap()
            .display()
            .to_string()
            .trim_start_matches(['\\', '?'])
            .to_string();
        let loose = f.work.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        let a = session(&f, &loose, "Alpha");
        let b = session(&f, &loose, "Bravo");
        let c = session(&f, &loose, "Charlie");
        let d = session(&f, &loose, "Delta\nwith a newline 🚀");

        let mut cx = crate::testing::headless_app();
        let model = cx.update(|cx| cx.new(|_| AppModel::starting()));
        let backend = std::sync::Arc::new(CoreBackend::new(f.core.clone()));
        {
            let model = model.clone();
            cx.update(|cx| model.update(cx, |m, cx| m.connect(backend, cx)));
        }
        let wait = |cx: &mut gpui_kit::HeadlessAppContext| {
            for _ in 0..100 {
                cx.run_until_parked();
                if cx.update(|cx| model.read(cx).pending_edits()) == 0 {
                    cx.run_until_parked();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    cx.run_until_parked();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("the edits did not finish");
        };
        wait(&mut cx);
        let apply = |cx: &mut gpui_kit::HeadlessAppContext, op: Op| {
            let model = model.clone();
            cx.update(|cx| model.update(cx, |m, cx| m.apply(op, cx)));
        };
        apply(
            &mut cx,
            Op::AddProject {
                folder: one_s,
                name: "one".into(),
            },
        );
        apply(
            &mut cx,
            Op::Rename {
                id: a.clone(),
                title: "Alpha renamed".into(),
            },
        );
        apply(
            &mut cx,
            Op::Pin {
                id: a.clone(),
                pinned: true,
            },
        );
        apply(&mut cx, Op::Archive { id: b.clone() });
        apply(&mut cx, Op::Delete { id: c.clone() });
        wait(&mut cx);
        let project = cx.update(|cx| {
            model
                .read(cx)
                .snapshot()
                .projects
                .iter()
                .find(|p| p.name == "one")
                .map(|p| p.id.clone())
                .expect("the project, re-read with its store id")
        });
        apply(
            &mut cx,
            Op::Move {
                id: d.clone(),
                project: project.clone(),
            },
        );
        wait(&mut cx);
        let listed = cx.update(|cx| model.read(cx).snapshot().clone());
        assert_eq!(listed.thread(&d).unwrap().project, project);
        assert_eq!(
            listed.thread(&d).unwrap().display_title(),
            "Delta with a newline 🚀"
        );
        drop(cx);

        // A restart: another core on the same folder.
        let config = RuntimeConfig {
            data_dir: f._data.path().to_path_buf(),
            ..RuntimeConfig::for_current_user().unwrap()
        };
        let again = Core::new(config, f._runtime.handle().clone()).unwrap();
        let snap = CoreBackend::new(again.clone()).snapshot().unwrap();
        let alpha = snap.thread(&a).expect("alpha");
        assert_eq!(alpha.title, "Alpha renamed");
        assert!(alpha.pinned);
        assert!(snap.thread(&b).is_none(), "archived");
        assert!(
            again.runtime().store().session(&b).unwrap().is_some(),
            "kept"
        );
        assert!(
            again.runtime().store().session(&c).unwrap().is_none(),
            "deleted"
        );
        assert_eq!(snap.thread(&d).unwrap().project, project);
        assert!(
            snap.projects
                .iter()
                .any(|p| p.name == "one" && p.kind == ProjectKind::User)
        );

        // Removing the project keeps the folder and the thread.
        CoreBackend::new(again.clone())
            .run(&Op::RemoveProject { id: project })
            .unwrap();
        assert!(one.is_dir());
        assert!(
            CoreBackend::new(again)
                .snapshot()
                .unwrap()
                .thread(&d)
                .is_some()
        );
    }

    /// Fills a data folder with a synthetic history for running the app on it
    /// (`HERMES_NATIVE_SEED=<folder>`: an empty folder of your own such as
    /// `target\native-test-seed`, never the real one): three projects, fifty threads with mixed
    /// titles (emoji, a newline, a very long one, an untitled one), some pinned, some archived,
    /// eight in no project. `cargo test -p hermes-native seed_synthetic -- --ignored --nocapture`.
    #[core::prelude::v1::test]
    #[ignore = "needs HERMES_NATIVE_SEED"]
    fn seed_synthetic_data_folder() {
        let Some(folder) = std::env::var_os("HERMES_NATIVE_SEED") else {
            return;
        };
        let folder = std::path::PathBuf::from(folder);
        assert!(
            !folder.join("sessions.db").exists(),
            "seed an empty folder only"
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let config = RuntimeConfig {
            data_dir: folder.clone(),
            ..RuntimeConfig::for_current_user().unwrap()
        };
        let core = Core::new(config, runtime.handle().clone()).unwrap();
        let backend = CoreBackend::new(core.clone());
        let work = folder.join("work");
        let mut ids = Vec::new();
        for name in ["alpha", "beta-tools", "gamma 2"] {
            let dir = work.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            backend
                .run(&Op::AddProject {
                    folder: dir.display().to_string(),
                    name: name.into(),
                })
                .unwrap();
            for n in 0..14 {
                let title = match n {
                    0 => format!("{name}: very long title {}", "that goes on ".repeat(12)),
                    1 => format!("{name}: two\nlines"),
                    2 => format!("{name}: emoji 🚀✨"),
                    3 => String::new(),
                    _ => format!("{name} thread {n}"),
                };
                ids.push(raw_session(&core, &dir, &title));
            }
        }
        let loose = work.join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        for n in 0..8 {
            ids.push(raw_session(
                &core,
                &loose,
                &format!("No project thread {n}"),
            ));
        }
        for id in ids.iter().step_by(9) {
            backend
                .run(&Op::Pin {
                    id: id.clone(),
                    pinned: true,
                })
                .unwrap();
        }
        for id in ids.iter().skip(4).step_by(11) {
            backend.run(&Op::Archive { id: id.clone() }).unwrap();
        }
        let snap = backend.snapshot().unwrap();
        println!(
            "seeded {} threads ({} listed, {} pinned) in {} groups",
            ids.len(),
            snap.threads.len(),
            snap.threads.iter().filter(|t| t.pinned).count(),
            snap.projects.len()
        );
    }

    fn raw_session(core: &std::sync::Arc<Core>, folder: &std::path::Path, title: &str) -> String {
        let (record, _) = core
            .create_session(NewSession {
                provider: ProviderConfig {
                    id: "mock".into(),
                    base_url: "http://127.0.0.1:1/v1".into(),
                    api_key: None,
                },
                model: "mock".into(),
                cwd: folder.to_path_buf(),
                title: (!title.is_empty()).then(|| title.to_string()),
                seeded: vec![],
                parent: None,
            })
            .unwrap();
        record.id
    }
}

/// Opens the core on a COPY of a data folder (`HERMES_NATIVE_COPY=<folder>`, never the real one)
/// and prints what the sidebar would list. `cargo test -p hermes-native real_copy -- --ignored --nocapture`.
#[core::prelude::v1::test]
#[ignore = "needs HERMES_NATIVE_COPY"]
fn real_copy_lists_projects_and_threads() {
    use crate::model::{Backend, CoreBackend};
    let Some(folder) = std::env::var_os("HERMES_NATIVE_COPY") else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let config = hermes_runtime::RuntimeConfig {
        data_dir: folder.into(),
        ..hermes_runtime::RuntimeConfig::for_current_user().unwrap()
    };
    let core = hermes_core::Core::new(config, runtime.handle().clone()).unwrap();
    println!(
        "raw sessions: {:?}",
        core.runtime().store().list_sessions(5).map(|v| v.len())
    );
    let snapshot = CoreBackend::new(core).snapshot().unwrap();
    println!(
        "{} projects, {} threads, {} pinned",
        snapshot.projects.len(),
        snapshot.threads.len(),
        snapshot.threads.iter().filter(|t| t.pinned).count()
    );
    for p in &snapshot.projects {
        let n = snapshot
            .threads
            .iter()
            .filter(|t| t.project == p.id)
            .count();
        println!("  {:?} {} ({} threads)", p.kind, p.name, n);
    }
}
