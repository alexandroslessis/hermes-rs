//! The thread header, headless with real text: where it sits in the title bar at three window
//! sizes (sidebar open and collapsed) with names and paths far too long, the chips that leave when
//! there is no room, renaming in place (commit, cancel, empty, unchanged, and that it is the
//! sidebar's rename), the toggles and their shortcuts, the actions menu's entries and what each
//! does at the backend, the git state read in the background (at open, at the end of a turn, when
//! stale, never per frame) and `git_state` against real repositories, and a real core's
//! `header_info` and `export`.

use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Entity, HeadlessAppContext, InputEvent as _,
    MouseButton, MouseDownEvent, MouseUpEvent, Window, px, size,
    test::{TestEventExt as _, TestWindowExt as _},
};
use hermes_core::{CoreEvent, export::Format};
use hermes_testdir::tempdir;

use super::data::HistoryItem;
use super::header::ThreadHeader;
use super::header_data::{
    Chips, GitState, GitSummary, HeaderInfo, MenuAction, MenuEntry, menu_entries, title_min_width,
};
use super::model::ThreadModel;
use super::recording::Recorded;
use crate::design::toast;
use crate::model::{
    AppModel, FixtureBackend, Selection,
    fixture::{self, NOW_MS},
};
use crate::sidebar::data::{Op, Snapshot};
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::ui_state::UiState;
use crate::workspace::Workspace;

const SMALL: (f32, f32) = (640., 480.);
const MEDIUM: (f32, f32) = (1100., 700.);
const LARGE: (f32, f32) = (1920., 1080.);

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    app: Entity<AppModel>,
    workspace: Entity<Workspace>,
}

/// A workspace over the fixture with thread `t1` ("halo", project "structure", branch "master")
/// open, answered once (so it has something to branch from and export).
fn rig(window: (f32, f32), sidebar_open: bool) -> Rig {
    rig_with(window, sidebar_open, |_, _| {})
}

fn rig_with(
    window: (f32, f32),
    sidebar_open: bool,
    tweak: impl FnOnce(&mut Snapshot, &FixtureBackend),
) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        crate::keymap::bind(cx);
        super::header::bind(cx);
    });
    let mut snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let mut items = fixture::history("t1");
    items.push(HistoryItem::TurnFooter {
        files: Vec::new(),
        user_row: Some(1),
        last_row: Some(2),
    });
    backend.set_history("t1", items);
    tweak(&mut snapshot, &backend);
    backend.set_snapshot(snapshot.clone());
    let app = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS))
        })
    };
    let root = app.clone();
    let mut state = UiState::default();
    state.sidebar.open = sidebar_open;
    let handle = open_window(
        &mut cx,
        size(px(window.0), px(window.1)),
        move |window, cx| cx.new(|cx| Workspace::with_model(state, None, root, window, cx)),
    );
    let workspace = cx
        .update_window(handle.into(), |root, _, _| {
            root.downcast::<Workspace>()
                .expect("the root is the workspace")
        })
        .unwrap();
    let mut rig = Rig {
        cx,
        window: handle.into(),
        backend,
        app,
        workspace,
    };
    rig.focus_frame();
    rig.open("t1");
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

    fn focus_frame(&mut self) {
        let ws = self.workspace.clone();
        self.cx
            .update_window(self.window, |_, window, cx| {
                ws.update(cx, |ws, cx| ws.focus(window, cx))
            })
            .unwrap();
        self.settle();
    }

    fn open(&mut self, id: &str) {
        let app = self.app.clone();
        self.cx.update(|cx| app.update(cx, |m, cx| m.open(id, cx)));
        self.settle();
    }

    fn header_entity(&mut self) -> Entity<ThreadHeader> {
        let ws = self.workspace.clone();
        self.cx
            .update(|cx| ws.read(cx).thread_header().cloned())
            .expect("a header")
    }

    fn thread(&mut self) -> Entity<ThreadModel> {
        let header = self.header_entity();
        self.cx.update(|cx| header.read(cx).thread().clone())
    }

    fn header<R>(
        &mut self,
        f: impl FnOnce(&mut ThreadHeader, &mut Window, &mut Context<ThreadHeader>) -> R,
    ) -> R {
        let header = self.header_entity();
        let r = self
            .cx
            .update_window(self.window, |_, window, cx| {
                header.update(cx, |h, cx| f(h, window, cx))
            })
            .unwrap();
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

    fn click(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.pointer_move(at, None, cx);
                window.pointer_down(at, MouseButton::Left, cx);
                window.pointer_up(at, MouseButton::Left, cx);
            })
            .unwrap();
        self.settle();
    }

    fn double_click(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.pointer_move(at, None, cx);
                // The test helpers always say "one click": the second press says two.
                for click_count in 1..=2 {
                    window.dispatch_event(
                        MouseDownEvent {
                            button: MouseButton::Left,
                            position: at,
                            modifiers: window.modifiers(),
                            click_count,
                            first_mouse: false,
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.dispatch_event(
                        MouseUpEvent {
                            button: MouseButton::Left,
                            position: at,
                            modifiers: window.modifiers(),
                            click_count,
                        }
                        .to_platform_input(),
                        cx,
                    );
                }
            })
            .unwrap();
        self.settle();
    }

    fn ops(&self) -> Vec<Op> {
        self.backend.ops()
    }

    fn title(&mut self) -> String {
        let app = self.app.clone();
        self.cx.update(|cx| {
            app.read(cx)
                .snapshot()
                .thread("t1")
                .map(|t| t.title.clone())
                .unwrap_or_default()
        })
    }

    fn selected(&mut self) -> Selection {
        let app = self.app.clone();
        self.cx.update(|cx| app.read(cx).selected().clone())
    }

    fn state<R>(&mut self, f: impl FnOnce(&UiState) -> R) -> R {
        let ws = self.workspace.clone();
        self.cx.update(|cx| f(ws.read(cx).state()))
    }

    fn toasts(&mut self) -> Vec<(toast::Kind, String)> {
        let ws = self.workspace.clone();
        self.cx.update(|cx| {
            ws.read(cx)
                .toasts()
                .visible()
                .iter()
                .map(|t| (t.spec.kind, t.spec.title.clone()))
                .collect()
        })
    }
}

fn repo(
    branch: &str,
    ahead: i64,
    behind: i64,
    changed: usize,
    added: u64,
    removed: u64,
) -> GitState {
    GitState::Repo(GitSummary {
        branch: Some(branch.to_string()),
        ahead,
        behind,
        changed,
        unstaged: changed,
        added,
        removed,
        ..GitSummary::default()
    })
}

fn info(git: GitState) -> HeaderInfo {
    HeaderInfo {
        cwd: r"C:\work\structure".into(),
        model: "openrouter/free".into(),
        provider: "openrouter".into(),
        archived: false,
        git,
    }
}

// --- placement ---------------------------------------------------------------------------------

/// The header is the title bar's middle at every window size with the sidebar open or collapsed:
/// the project, a slash and the title on one row inside the bar, before the toggles and the caption
/// buttons, with the drag zone keeping its 24 px.
#[core::prelude::v1::test]
fn the_header_sits_in_the_title_bar_at_three_sizes_with_the_sidebar_open_or_collapsed() {
    for window in [SMALL, MEDIUM, LARGE] {
        for sidebar in [true, false] {
            let ctx = format!("{window:?} sidebar {sidebar}");
            let mut rig = rig(window, sidebar);
            let l = rig.layout();
            let mut check = l.check();
            check
                .inside("thread-header", "titlebar")
                .no_overflow("thread-header")
                .same_row("header-project", "header-title")
                .same_row("header-project", "header-sep")
                .same_row("header-title", "panel-toggles")
                .same_row("header-title", "caption-buttons")
                .inside("panel-toggles", "titlebar")
                .inside("caption-buttons", "titlebar");
            check.assert();
            assert_eq!(l.bounds("titlebar").size.height.as_f32(), 40., "{ctx}");
            // Left to right: project, slash, title (and its menu), drag zone, toggles, caption.
            let b = |name: &str| l.bounds(name);
            assert!(
                b("header-project").right() <= b("header-sep").left(),
                "{ctx}"
            );
            assert!(b("header-sep").right() <= b("header-title").left(), "{ctx}");
            assert!(
                b("header-title").right() <= b("header-actions-slot").left() + px(0.5),
                "{ctx}"
            );
            assert!(
                b("thread-header").right() <= b("drag").left() + px(0.5),
                "{ctx}"
            );
            assert!(
                b("drag").right() <= b("panel-toggles").left() + px(0.5),
                "{ctx}"
            );
            assert!(
                b("drag").size.width.as_f32() >= crate::titlebar::DRAG_MIN - 0.5,
                "{ctx}"
            );
            assert!(
                b("panel-toggles").right() <= b("caption-buttons").left() + px(0.5),
                "{ctx}"
            );
            // The thread's title is not also printed as the drag zone's text.
            assert!(l.get("drag/title").is_err(), "{ctx}");
            // The collapsed sidebar's toggle is before the header.
            if !sidebar {
                assert!(
                    b("toggle-sidebar").right() <= b("thread-header").left() + px(0.5),
                    "{ctx}"
                );
            }
        }
    }
}

/// A title of 300 characters, a project name and a branch just as long, a folder deeper than the
/// window: nothing leaves the bar, the toggles stay clear, the title keeps its 40 px, the drag zone
/// its 24.
#[core::prelude::v1::test]
fn very_long_titles_names_and_branches_never_overflow() {
    let long = "an extremely long name that goes on and on ".repeat(8);
    let branch = format!("feature/{}", "very-long-branch-name-".repeat(10));
    // The three sizes, and the widths where the chips come in (the tightest fits).
    let widths = [
        SMALL,
        (760., 600.),
        (820., 600.),
        (900., 600.),
        (1000., 600.),
        MEDIUM,
        LARGE,
    ];
    for window in widths {
        for sidebar in [true, false] {
            let ctx = format!("{window:?} sidebar {sidebar}");
            let (long, branch) = (long.clone(), branch.clone());
            let mut rig = rig_with(window, sidebar, |snapshot, backend| {
                snapshot.threads[0].title = long.clone();
                snapshot.projects[0].name = long.clone();
                backend.set_header(
                    "t1",
                    HeaderInfo {
                        cwd: format!(r"C:\work\{}", "deeply\\nested\\".repeat(30)),
                        model: "provider/".to_string() + &"huge-model-name-".repeat(10),
                        ..info(repo(&branch, 12, 345, 7, 123_456, 7_890))
                    },
                );
            });
            let l = rig.layout();
            let mut check = l.check();
            check
                .inside("thread-header", "titlebar")
                .no_overflow("thread-header")
                .same_row("header-project", "header-title")
                .inside("header-title", "thread-header")
                .inside("header-project", "thread-header");
            if l.get("header-git").is_ok() {
                check.inside("header-git", "thread-header");
                check.same_row("header-git", "header-title");
            }
            if l.get("header-model").is_ok() {
                check.inside("header-model", "thread-header");
                check.same_row("header-model", "header-title");
            }
            check.assert();
            let b = |name: &str| l.bounds(name);
            assert!(
                b("thread-header").right() <= b("panel-toggles").left() + px(0.5),
                "{ctx}"
            );
            assert!(
                b("header-title-wrap").size.width.as_f32() >= 40. - 0.5,
                "{ctx}: the title and its menu keep 40 px"
            );
            // The title keeps a readable share (CP4.3 review: it was a bare "..." beside a long
            // project name).
            let shown = Chips {
                git: l.get("header-git").is_ok(),
                model: l.get("header-model").is_ok(),
            };
            // The room the frame told the header (its estimate of the bar's free width).
            let header = rig.header_entity();
            let room = rig.cx.update(|cx| header.read(cx).room());
            let claim = title_min_width(&long, room - shown.reserved());
            assert!(
                b("header-title-wrap").size.width.as_f32() >= claim - 0.5,
                "{ctx}: the title keeps its claim of {claim} in {room}"
            );
            assert!(claim >= 60. || room < 400., "{ctx}: {claim} in {room}");

            assert!(
                b("header-project").size.width.as_f32() <= 180. + 0.5,
                "{ctx}"
            );
            assert!(
                b("drag").size.width.as_f32() >= crate::titlebar::DRAG_MIN - 0.5,
                "{ctx}"
            );
            // The 300-character title shows an ellipsis, not the whole: the text needs more
            // than the room it was given.
            let title = l.get("header-title").unwrap();
            assert!(
                title.text_width.unwrap() > title.bounds.size.width,
                "{ctx}: the long title is cut with an ellipsis"
            );
        }
    }
}

/// The chips leave when the room is short, git last: none in the smallest window, both in a
/// roomy one, and the title bar never has more than the room says.
#[core::prelude::v1::test]
fn the_git_and_model_chips_come_and_go_with_the_room() {
    let mut small = rig(SMALL, true);
    let l = small.layout();
    assert!(l.get("header-git").is_err() && l.get("header-model").is_err());
    let mut medium = rig(MEDIUM, true);
    let l = medium.layout();
    assert!(l.get("header-git").is_ok() && l.get("header-model").is_ok());
    let mut check = l.check();
    check
        .same_row("header-title", "header-git")
        .same_row("header-title", "header-model")
        .inside("header-git", "thread-header")
        .inside("header-model", "thread-header");
    check.assert();
    // Between: room for the branch, not for the model.
    let mut mid = rig((900., 600.), true);
    let l = mid.layout();
    assert!(l.get("header-git").is_ok());
    assert!(
        l.get("header-model").is_err(),
        "the model chip needs more room"
    );
}

// --- the git chip -------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_git_chip_shows_branch_arrows_and_counts_or_nothing_without_a_repository() {
    let cases: Vec<(&str, GitState, bool)> = vec![
        ("clean", repo("main", 0, 0, 0, 0, 0), true),
        ("dirty", repo("main", 2, 1, 4, 6770, 3), true),
        (
            "detached",
            GitState::Repo(GitSummary {
                detached: true,
                ..GitSummary::default()
            }),
            true,
        ),
        ("no repo", GitState::NoRepo, false),
        ("no folder", GitState::NoFolder, true),
    ];
    for (name, git, shown) in cases {
        let mut rig = rig_with(MEDIUM, true, |_, backend| {
            backend.set_header("t1", info(git.clone()))
        });
        let l = rig.layout();
        assert_eq!(l.get("header-git").is_ok(), shown, "{name}");
        if shown {
            l.check().inside("header-git", "thread-header").assert();
        }
        match name {
            "clean" => {
                assert!(
                    l.get("changes").is_err() && l.get("sync").is_err(),
                    "{name}"
                );
            }
            "dirty" => {
                assert!(l.get("changes").is_ok() && l.get("sync").is_ok(), "{name}");
                let (added, removed) = (l.bounds("added"), l.bounds("removed"));
                assert!(added.right() <= removed.left() + px(0.5), "{name}");
            }
            _ => {}
        }
    }
}

/// The colours: additions green, removals red.
#[core::prelude::v1::test]
fn the_change_counts_use_the_success_and_error_colours() {
    use crate::theme::DesignTheme as _;
    let mut rig = rig_with(MEDIUM, true, |_, b| {
        b.set_header("t1", info(repo("main", 0, 0, 2, 10, 2)))
    });
    let palette = rig.cx.update(|cx| cx.design_theme().palette);
    let l = rig.layout();
    let mut check = l.check();
    check
        .text_color("added", palette.success_foreground.hsla())
        .text_color("removed", palette.error_foreground.hsla());
    check.assert();
}

// --- the header's data is read in the background -------------------------------------------------

#[core::prelude::v1::test]
fn the_header_is_read_when_the_thread_opens_and_when_a_turn_ends_never_per_frame() {
    let mut rig = rig(MEDIUM, true);
    assert_eq!(
        rig.backend.header_reads(),
        1,
        "once, when the thread opened"
    );
    // Many frames and redraws: no more reads.
    for _ in 0..30 {
        draw(&mut rig.cx, rig.window);
    }
    rig.settle();
    assert_eq!(rig.backend.header_reads(), 1, "drawing never reads");
    // A turn runs: deltas read nothing.
    let before = rig.backend.header_reads();
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.backend.emit(CoreEvent::TextDelta {
        session_id: "t1".into(),
        text: "done".into(),
    });
    rig.settle();
    assert_eq!(rig.backend.header_reads(), before, "deltas read nothing");
    // The turn ends: the git state may have changed.
    rig.backend
        .set_header("t1", info(repo("after-turn", 0, 0, 1, 1, 0)));
    rig.backend.emit(
        Recorded::TurnEnded {
            outcome: "complete".into(),
            error: None,
            text: String::new(),
            user_row: None,
            last_row: None,
        }
        .to_core("t1"),
    );
    rig.settle();
    assert_eq!(
        rig.backend.header_reads(),
        before + 1,
        "a turn's end reads once"
    );
    let thread = rig.thread();
    let branch = rig.cx.update(|cx| {
        thread.read(cx).header().and_then(|h| match &h.git {
            GitState::Repo(g) => g.branch.clone(),
            _ => None,
        })
    });
    assert_eq!(branch.as_deref(), Some("after-turn"));
}

#[core::prelude::v1::test]
fn coming_to_the_front_reads_again_only_when_the_last_read_is_old() {
    let mut rig = rig(MEDIUM, true);
    let thread = rig.thread();
    let reads = rig.backend.header_reads();
    // Just read: the window coming forward again does nothing.
    rig.cx
        .update(|cx| thread.update(cx, |t, cx| t.refresh_header_if_stale(cx)));
    rig.settle();
    assert_eq!(rig.backend.header_reads(), reads, "fresh");
    // An old read (the staleness lowered to nothing): it reads.
    rig.cx.update(|cx| {
        thread.update(cx, |t, cx| {
            t.set_header_stale(std::time::Duration::ZERO);
            t.refresh_header_if_stale(cx)
        })
    });
    rig.settle();
    assert_eq!(rig.backend.header_reads(), reads + 1, "stale");
    // Requests made while one runs coalesce into one more.
    rig.cx.update(|cx| {
        thread.update(cx, |t, cx| {
            t.refresh_header(cx);
            t.refresh_header(cx);
            t.refresh_header(cx);
        })
    });
    rig.settle();
    assert_eq!(
        rig.backend.header_reads(),
        reads + 3,
        "a running read plus one more"
    );
}

#[core::prelude::v1::test]
fn a_thread_whose_details_cannot_be_read_still_has_its_title_and_menu() {
    let mut rig = rig(MEDIUM, true);
    // The backend no longer knows the thread; the list (the app model's) still has it.
    rig.backend.update(|s| s.threads.retain(|t| t.id != "t1"));
    let thread = rig.thread();
    rig.cx
        .update(|cx| thread.update(cx, |t, cx| t.refresh_header(cx)));
    rig.settle();
    let error = rig
        .cx
        .update(|cx| thread.read(cx).header_error().map(str::to_string));
    assert_eq!(error.as_deref(), Some("That thread no longer exists."));
    let l = rig.layout();
    assert!(l.get("header-title").is_ok() && l.get("header-actions").is_ok());
    l.check().inside("thread-header", "titlebar").assert();
}

// --- renaming ------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_double_click_on_the_title_opens_the_field_and_enter_renames_like_the_sidebar() {
    let mut rig = rig(MEDIUM, true);
    rig.double_click("header-title");
    assert!(rig.header(|h, _, _| h.renaming()));
    let l = rig.layout();
    assert!(l.get("header-title-field").is_ok() && l.get("header-title").is_err());
    l.check()
        .inside("header-title-field", "thread-header")
        .same_row("header-title-field", "header-project")
        .assert();
    rig.press("ctrl-a");
    rig.type_text("Shiny new name");
    rig.press("enter");
    // The same operation the sidebar's rename sends.
    assert_eq!(
        rig.ops(),
        vec![Op::Rename {
            id: "t1".into(),
            title: "Shiny new name".into()
        }]
    );
    assert_eq!(rig.title(), "Shiny new name");
    assert!(!rig.header(|h, _, _| h.renaming()));
    let l = rig.layout();
    assert!(l.get("header-title").is_ok() && l.get("header-title-field").is_err());
}

#[core::prelude::v1::test]
fn escape_cancels_an_empty_title_is_refused_and_an_unchanged_one_does_nothing() {
    let mut rig = rig(MEDIUM, true);
    // Escape: the old title stays, nothing is sent.
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("ctrl-a");
    rig.type_text("Dropped");
    rig.press("escape");
    assert!(rig.ops().is_empty());
    assert_eq!(rig.title(), "halo");
    assert!(!rig.header(|h, _, _| h.renaming()));
    assert!(rig.toasts().is_empty());

    // Unchanged (Enter on the old title): nothing.
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("enter");
    assert!(rig.ops().is_empty() && rig.toasts().is_empty());

    // Blanks: refused with a warning, the title stays.
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("ctrl-a");
    rig.type_text("   ");
    rig.press("enter");
    assert!(rig.ops().is_empty());
    assert_eq!(rig.title(), "halo");
    assert_eq!(
        rig.toasts(),
        vec![(
            toast::Kind::Warning,
            "Thread title cannot be empty".to_string()
        )]
    );

    // Spaces around a real change are trimmed.
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("ctrl-a");
    rig.type_text("  Trimmed  ");
    rig.press("enter");
    assert_eq!(
        rig.ops(),
        vec![Op::Rename {
            id: "t1".into(),
            title: "Trimmed".into()
        }]
    );
}

#[core::prelude::v1::test]
fn a_rename_that_the_backend_refuses_comes_back_as_the_old_title_with_a_toast() {
    let mut rig = rig(MEDIUM, true);
    rig.backend.fail_with(Some("disk full"));
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("ctrl-a");
    rig.type_text("Nope");
    rig.press("enter");
    assert_eq!(rig.ops().len(), 1, "it was tried");
    assert_eq!(rig.title(), "halo", "the list went back");
    assert!(
        rig.toasts()
            .iter()
            .any(|(k, t)| *k == toast::Kind::Error && t == "Could not rename the thread")
    );
}

#[core::prelude::v1::test]
fn the_title_in_the_sidebar_follows_the_headers_rename() {
    let mut rig = rig(MEDIUM, true);
    rig.header(|h, window, cx| h.start_rename(window, cx));
    rig.press("ctrl-a");
    rig.type_text("One name");
    rig.press("enter");
    assert_eq!(rig.title(), "One name");
    let l = rig.layout();
    assert!(l.get("title-t1").is_ok() && l.get("header-title").is_ok());
}

// --- the toggles ---------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_toggles_work_the_panel_and_the_drawer_and_their_tooltips_name_the_shortcuts() {
    assert_eq!(
        crate::keymap::tooltip("terminal.toggle", "Toggle terminal drawer"),
        "Toggle terminal drawer (Ctrl+J)"
    );
    assert_eq!(
        crate::keymap::tooltip("rightPanel.toggle", "Toggle right panel"),
        "Toggle right panel (Ctrl+Alt+B)"
    );
    assert_eq!(
        crate::keymap::tooltip("sidebar.toggle", "Toggle sidebar"),
        "Toggle sidebar (Ctrl+B)"
    );
    // A command with no shortcut has the text alone; an unknown one too.
    assert_eq!(
        crate::keymap::tooltip("no.such.command", "Nothing"),
        "Nothing"
    );
    let mut rig = rig(MEDIUM, true);
    assert!(!rig.state(|s| s.right_panel.open) && !rig.state(|s| s.drawer.open));
    rig.click("toggle-right");
    assert!(rig.state(|s| s.right_panel.open));
    assert!(rig.layout().get("right-panel").is_ok());
    rig.click("toggle-drawer");
    assert!(rig.state(|s| s.drawer.open));
    assert!(rig.layout().get("drawer").is_ok());
    // The header is still there on one row with the toggles.
    let l = rig.layout();
    l.check()
        .inside("thread-header", "titlebar")
        .same_row("header-title", "toggle-right")
        .assert();
    // The shortcuts do the same.
    rig.press("ctrl-alt-b");
    rig.press("ctrl-j");
    assert!(!rig.state(|s| s.right_panel.open) && !rig.state(|s| s.drawer.open));
}

// --- the actions menu ----------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_menu_entries_follow_the_threads_state() {
    let mut rig = rig(MEDIUM, true);
    let state = rig.header(|h, _, cx| h.menu_state(cx));
    // halo has a finished reply and messages; it is not pinned or archived.
    assert!(state.can_branch && state.can_export && !state.archived && !state.pinned);
    let entries = menu_entries(&state);
    assert!(
        entries
            .iter()
            .filter(|e| e.action().is_some())
            .all(MenuEntry::enabled)
    );

    // A thread with no messages: nothing to export or branch from.
    let mut empty = rig_with(MEDIUM, true, |_, backend| {
        backend.set_history("t1", Vec::new())
    });
    let state = empty.header(|h, _, cx| h.menu_state(cx));
    assert!(!state.can_branch && !state.can_export);
    let entries = menu_entries(&state);
    let enabled = |action| {
        entries
            .iter()
            .find(|e| e.action() == Some(action))
            .is_some_and(MenuEntry::enabled)
    };
    assert!(!enabled(MenuAction::Branch) && !enabled(MenuAction::Export(Format::Markdown)));
    assert!(enabled(MenuAction::Archive) && enabled(MenuAction::Rename));

    // Archived (read from the header): archive is off, and asking does nothing.
    let mut archived = rig_with(MEDIUM, true, |_, backend| {
        backend.set_header(
            "t1",
            HeaderInfo {
                archived: true,
                ..info(GitState::NoRepo)
            },
        )
    });
    let state = archived.header(|h, _, cx| h.menu_state(cx));
    assert!(state.archived);
    archived.header(|h, window, cx| h.run(MenuAction::Archive, window, cx));
    assert!(
        archived.ops().is_empty(),
        "an archived thread is not archived again"
    );

    // While a turn runs, branch waits.
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.settle();
    assert!(!rig.header(|h, _, cx| h.menu_state(cx)).can_branch);
}

#[core::prelude::v1::test]
fn each_menu_entry_reaches_the_right_operation() {
    let mut rig = rig(MEDIUM, true);
    // Pin, then (the list says pinned) unpin.
    rig.header(|h, window, cx| h.run(MenuAction::TogglePin, window, cx));
    assert_eq!(
        rig.ops(),
        vec![Op::Pin {
            id: "t1".into(),
            pinned: true
        }]
    );
    assert!(rig.header(|h, _, cx| h.menu_state(cx)).pinned);
    rig.header(|h, window, cx| h.run(MenuAction::TogglePin, window, cx));
    assert_eq!(
        rig.ops()[1],
        Op::Pin {
            id: "t1".into(),
            pinned: false
        }
    );
    // Rename opens the field.
    rig.header(|h, window, cx| h.run(MenuAction::Rename, window, cx));
    assert!(rig.header(|h, _, _| h.renaming()));
    rig.press("escape");
    // Copy the id.
    rig.header(|h, window, cx| h.run(MenuAction::CopyId, window, cx));
    let copied = rig
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|i| i.text()));
    assert_eq!(copied.as_deref(), Some("t1"));
    // Branch: the footer's operation on the latest reply (row 2).
    rig.header(|h, window, cx| h.run(MenuAction::Branch, window, cx));
    assert_eq!(
        rig.backend.thread_ops(),
        vec![crate::model::backend::ThreadOp::Branch {
            session: "t1".into(),
            through_row: 2
        }]
    );
    // The branch opens as the footer's does.
    assert_eq!(rig.selected(), Selection::Thread("t1-branch".into()));
    // Archive (on the original thread, reopened): the sidebar's operation, and the thread closes.
    rig.open("t1");
    rig.header(|h, window, cx| h.run(MenuAction::Archive, window, cx));
    assert!(rig.ops().contains(&Op::Archive { id: "t1".into() }));
    assert_eq!(rig.selected(), Selection::None);
    let l = rig.layout();
    assert!(
        l.get("thread-header").is_err(),
        "the header goes with the thread"
    );
}

/// A failed branch tells the user (a notice in the thread) and leaves the menu usable.
#[core::prelude::v1::test]
fn a_failed_branch_is_a_notice_in_the_thread() {
    let mut rig = rig(MEDIUM, true);
    rig.backend.fail_with(Some("no room"));
    rig.header(|h, window, cx| h.run(MenuAction::Branch, window, cx));
    let thread = rig.thread();
    let notice = rig.cx.update(|cx| {
        thread.read(cx).rows().iter().any(|r| {
            matches!(&r.kind, super::model::RowKind::Notice { error: true, text }
                if text.contains("no room"))
        })
    });
    assert!(notice);
    assert!(
        rig.header(|h, _, cx| h.menu_state(cx)).can_branch,
        "the footer is free again"
    );
}

/// A click on the chevron opens the dropdown with the entries.
#[core::prelude::v1::test]
fn the_chevron_opens_the_menu() {
    let mut rig = rig(MEDIUM, true);
    let before = rig.layout().dump().entries.len();
    rig.click("header-actions");
    let l = rig.layout();
    let text = l.dump().to_text();
    assert!(
        l.dump().entries.len() > before,
        "the dropdown adds elements: {text}"
    );
}

// --- export --------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn export_writes_the_threads_file_where_the_dialog_says_and_cancel_writes_nothing() {
    let dir = tempdir().unwrap();
    let mut rig = rig(MEDIUM, true);
    let path = dir.path().join("halo.md");
    let target = path.clone();
    rig.header(move |h, _, _| h.set_save_path(Some(target)));
    rig.header(|h, window, cx| h.run(MenuAction::Export(Format::Markdown), window, cx));
    // The background write lands.
    for _ in 0..100 {
        if path.exists() && !rig.toasts().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        rig.settle();
    }
    assert_eq!(
        rig.backend.exports(),
        vec![("t1".to_string(), Format::Markdown)]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# halo\n");
    assert!(
        rig.toasts()
            .iter()
            .any(|(k, t)| *k == toast::Kind::Success && t == "Thread exported"),
        "{:?}",
        rig.toasts()
    );

    // The dialog cancelled: nothing is written, nothing is said.
    let other = dir.path().join("cancelled.html");
    rig.header(|h, _, _| h.set_save_path(None));
    let before = rig.toasts().len();
    rig.header(|h, window, cx| h.run(MenuAction::Export(Format::Html), window, cx));
    rig.settle();
    assert_eq!(rig.backend.exports().len(), 2);
    assert!(!other.exists());
    assert_eq!(rig.toasts().len(), before);
}

#[core::prelude::v1::test]
fn an_export_that_fails_says_so_and_an_empty_thread_exports_nothing() {
    let mut rig = rig(MEDIUM, true);
    rig.backend.fail_with(Some("the store is locked"));
    rig.header(|h, window, cx| h.run(MenuAction::Export(Format::Markdown), window, cx));
    rig.settle();
    assert!(
        rig.toasts()
            .iter()
            .any(|(k, t)| *k == toast::Kind::Error && t == "Could not export the thread"),
        "{:?}",
        rig.toasts()
    );
    // An empty thread: the entry is off, so asking does nothing at all.
    let mut empty = rig_with(MEDIUM, true, |_, b| b.set_history("t1", Vec::new()));
    empty.header(|h, window, cx| h.run(MenuAction::Export(Format::Html), window, cx));
    assert!(empty.backend.exports().is_empty());
}

/// The file is written whole or not at all: an existing file (the dialog asked before replacing
/// it) is replaced, a unicode path works, a folder that is not there fails and leaves nothing.
#[core::prelude::v1::test]
fn an_export_file_is_written_whole_or_not_at_all() {
    use super::header::write_whole;
    let dir = tempdir().unwrap();
    let path = dir.path().join("Ελληνικά νήμα 🦀.md");
    std::fs::write(&path, "old and longer content").unwrap();
    write_whole(&path, "new".as_bytes()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    let missing = dir.path().join("no-such-folder").join("x.html");
    assert!(write_whole(&missing, b"<p>").is_err());
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "no partial file is left: {left:?}");
}

// --- git_state on real repositories ----------------------------------------------------------------

mod git {
    use hermes_core::git::git;

    use super::*;
    use crate::model::backend::git_state;

    #[core::prelude::v1::test]
    fn a_missing_folder_a_plain_folder_and_a_repository_read_differently() {
        let dir = tempdir().unwrap();
        // No folder at all (and an empty path).
        assert_eq!(git_state(""), GitState::NoFolder);
        assert_eq!(
            git_state(&dir.path().join("gone").display().to_string()),
            GitState::NoFolder
        );
        // A plain folder (the test folders are under the temp dir, not in a repository).
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(git_state(&plain.display().to_string()), GitState::NoRepo);

        // A repository with one untracked file: branch main, one change, two lines added.
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(&repo, &["init", "-q", "-b", "main"]).0);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        let GitState::Repo(summary) = git_state(&repo.display().to_string()) else {
            panic!("a repository");
        };
        assert_eq!(summary.branch.as_deref(), Some("main"));
        assert!(!summary.detached && !summary.is_clean());
        assert_eq!((summary.changed, summary.untracked), (1, 1));
        assert_eq!(summary.added, 2);

        // Committed: clean. Edited: modified with +/- counts.
        let commit = |msg: &str| {
            assert!(
                git(
                    &repo,
                    &[
                        "-c",
                        "user.name=test",
                        "-c",
                        "user.email=test@example.com",
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "-q",
                        "--allow-empty",
                        "-am",
                        msg,
                    ]
                )
                .0
            );
        };
        assert!(git(&repo, &["add", "."]).0);
        commit("first");
        let GitState::Repo(clean) = git_state(&repo.display().to_string()) else {
            panic!("a repository");
        };
        assert!(clean.is_clean());
        assert_eq!(clean.change_counts(), None);
        std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let GitState::Repo(dirty) = git_state(&repo.display().to_string()) else {
            panic!("a repository");
        };
        assert_eq!((dirty.changed, dirty.unstaged, dirty.untracked), (1, 1, 0));
        assert_eq!((dirty.added, dirty.removed), (2, 1));

        // Detached: no branch name.
        commit("second");
        assert!(git(&repo, &["checkout", "-q", "--detach", "HEAD~1"]).0);
        let GitState::Repo(detached) = git_state(&repo.display().to_string()) else {
            panic!("a repository");
        };
        assert!(detached.detached && detached.branch.is_none());
        assert_eq!(detached.branch_label(), "detached HEAD");
        // A subfolder of the repository reads as the repository.
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert!(matches!(
            git_state(&repo.join("sub").display().to_string()),
            GitState::Repo(_)
        ));
    }
}

// --- the real core ---------------------------------------------------------------------------------

mod core_backend {
    use hermes_core::{Core, NewSession};
    use hermes_llm::conversation::message::Message;
    use hermes_runtime::{ProviderConfig, RuntimeConfig};

    use super::*;
    use crate::model::{Backend, CoreBackend};

    #[core::prelude::v1::test]
    fn header_info_and_export_come_from_the_stored_thread() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let config_dir = tempdir().unwrap();
        let work = tempdir().unwrap();
        let config = RuntimeConfig {
            data_dir: config_dir.path().to_path_buf(),
            ..RuntimeConfig::for_current_user().unwrap()
        };
        let core = Core::new(config, runtime.handle().clone()).unwrap();
        let seeded: Vec<Message> = vec![
            Message::user().with_text("my key is sk-proj-abcdefghij1234567890XYZ"),
            Message::assistant().with_text("Noted. <script>alert(1)</script>"),
        ]
        .into_iter()
        .map(Message::with_generated_id)
        .collect();
        let (record, _) = core
            .create_session(NewSession {
                provider: ProviderConfig {
                    id: "mock".into(),
                    base_url: "http://127.0.0.1:9/v1".into(),
                    api_key: None,
                },
                model: "mock-model".into(),
                cwd: work.path().to_path_buf(),
                title: Some("Keys & things".into()),
                seeded,
                parent: None,
            })
            .unwrap();
        let backend = CoreBackend::new(core.clone());

        let header = backend.header_info(&record.id).unwrap();
        assert_eq!(header.model, "mock-model");
        assert_eq!(header.provider, "mock");
        assert!(!header.archived);
        assert!(!header.cwd.is_empty());
        // The temp folder is not a repository.
        assert_eq!(header.git, GitState::NoRepo);
        // Archived from elsewhere: the next read says so.
        core.set_archived(&record.id, true).unwrap();
        assert!(backend.header_info(&record.id).unwrap().archived);
        // A thread that is not there.
        assert!(backend.header_info("nope").is_err());

        // Export: Markdown and HTML hold the conversation, with the key masked.
        let md = backend.export(&record.id, Format::Markdown).unwrap();
        assert!(
            md.file_name.ends_with(".md") && md.file_name.contains("keys"),
            "{}",
            md.file_name
        );
        assert!(md.content.starts_with("# Keys & things"));
        assert!(md.content.contains("Noted."));
        assert!(
            md.content.contains("sk-pro...0XYZ"),
            "masked: {}",
            md.content
        );
        assert!(!md.content.contains("sk-proj-abcdefghij1234567890XYZ"));
        let html = backend.export(&record.id, Format::Html).unwrap();
        assert!(html.file_name.ends_with(".html"));
        assert!(html.content.contains("Noted."));
        // Markup in a message or the title is text, never markup.
        assert!(!html.content.contains("<script>"), "{}", html.content);
        assert!(
            html.content
                .contains("&lt;script&gt;alert(1)&lt;/script&gt;")
        );
        assert!(html.content.contains("<title>Keys &amp; things</title>"));
        // JSON is the page's shape, built by crates/desktop: not offered here.
        assert!(backend.export(&record.id, Format::Json).is_err());
        assert!(backend.export("nope", Format::Markdown).is_err());
        drop(backend);
        drop(core);
        drop(runtime);
        drop(work);
        drop(config_dir);
    }
}
