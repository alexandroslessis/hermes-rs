//! The right panel's diff tab, headless with real text, over the fixture repository
//! ([`super::fixture`]): where everything sits at three window sizes (tab strip, toolbar, scope
//! switch, the file list's rows, letters and counts), very long paths and lines, scope switching,
//! the file tree, the revert flow and its confirmation, open-in-editor, what the app's actions
//! ("Open diff", a file link) do to the panel, a 5,000-line diff, and what a failed read and a stale
//! answer do. The same panel over a real `Core` and temporary git repositories is in
//! `real_tests.rs`; the patch parsing and the editor command are in `data.rs`.

use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, HeadlessAppContext, MouseButton, px, size,
    test::TestEventExt as _,
};
use hermes_core::CoreEvent;

use super::data::{Revert, Scope, Status, parse_patch};
use super::diff::{self, DiffPanel, ListRow, Phase, collapse_plan};
use super::fixture;
use super::{RightPanel, Tab};
use crate::design::toast;
use crate::model::{
    AppModel, FixtureBackend,
    fixture::{self as app_fixture, NOW_MS},
};
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::thread::data::HistoryItem;
use crate::thread::recording::Recorded;
use crate::thread::tools::ChangedFile;
use crate::thread::{OpenDiffTab, OpenFileInPanel};
use crate::ui_state::UiState;
use crate::workspace::Workspace;

pub(super) const SMALL: (f32, f32) = (640., 480.);
pub(super) const MEDIUM: (f32, f32) = (1100., 700.);
pub(super) const LARGE: (f32, f32) = (1920., 1080.);

pub(super) struct Rig {
    pub cx: HeadlessAppContext,
    pub window: AnyWindowHandle,
    pub backend: Arc<FixtureBackend>,
    pub app: Entity<AppModel>,
    pub workspace: Entity<Workspace>,
}

/// A workspace over the fixture with thread `t1` open and the right panel open; the sidebar is
/// collapsed so the panel gets room. The thread's newest footer names `footer_files`.
pub(super) fn rig(window: (f32, f32), footer_files: &[&str]) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        crate::keymap::bind(cx);
    });
    let snapshot = app_fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let mut items = app_fixture::history("t1");
    items.push(HistoryItem::TurnFooter {
        files: footer_files
            .iter()
            .map(|p| ChangedFile {
                path: (*p).to_string(),
                additions: 1,
                deletions: 0,
            })
            .collect(),
        user_row: Some(1),
        last_row: Some(2),
    });
    backend.set_history("t1", items);
    let app = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS))
        })
    };
    let root = app.clone();
    let mut state = UiState::default();
    state.sidebar.open = false;
    state.right_panel.open = true;
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
    pub fn settle(&mut self) {
        for _ in 0..4 {
            self.cx.run_until_parked();
            draw(&mut self.cx, self.window);
        }
    }

    pub fn layout(&mut self) -> Layout {
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

    pub fn open(&mut self, id: &str) {
        let app = self.app.clone();
        self.cx.update(|cx| app.update(cx, |m, cx| m.open(id, cx)));
        self.settle();
    }

    pub fn right(&mut self) -> Entity<RightPanel> {
        let ws = self.workspace.clone();
        self.cx.update(|cx| ws.read(cx).right_panel().clone())
    }

    pub fn panel(&mut self) -> Entity<DiffPanel> {
        let right = self.right();
        self.cx.update(|cx| right.read(cx).diff().clone())
    }

    pub fn scope(&mut self) -> Scope {
        let panel = self.panel();
        self.cx.update(|cx| panel.read(cx).scope())
    }

    /// `(path, letter, added, removed, binary)` of every listed file.
    pub fn entries(&mut self) -> Vec<(String, char, usize, usize, bool)> {
        let panel = self.panel();
        self.cx.update(|cx| {
            panel
                .read(cx)
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.path.clone(),
                        e.status.letter(),
                        e.added,
                        e.removed,
                        e.binary,
                    )
                })
                .collect()
        })
    }

    pub fn paths(&mut self) -> Vec<String> {
        self.entries().into_iter().map(|e| e.0).collect()
    }

    pub fn click(&mut self, name: &str) {
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

    pub fn panel_do<R>(
        &mut self,
        f: impl FnOnce(&mut DiffPanel, &mut gpui_kit::Context<DiffPanel>) -> R,
    ) -> R {
        let panel = self.panel();
        let r = self.cx.update(|cx| panel.update(cx, f));
        self.settle();
        r
    }

    pub fn toasts(&mut self) -> Vec<(toast::Kind, String)> {
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

    pub fn dispatch<A: gpui_kit::Action>(&mut self, action: A) {
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.dispatch_action(Box::new(action), cx)
            })
            .unwrap();
        self.settle();
    }

    pub fn state<R>(&mut self, f: impl FnOnce(&UiState) -> R) -> R {
        let ws = self.workspace.clone();
        self.cx.update(|cx| f(ws.read(cx).state()))
    }

    pub fn toggle_panel(&mut self) {
        let ws = self.workspace.clone();
        self.cx
            .update(|cx| ws.update(cx, |w, cx| w.toggle_right_panel(cx)));
        self.settle();
    }
}

// --- what is read, and listed ---------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_working_tree_is_read_once_when_the_panel_is_on_screen_and_listed_with_letters_and_counts() {
    let mut rig = rig(MEDIUM, &[]);
    let panel = rig.panel();
    let (reads, phase, scope) = rig.cx.update(|cx| {
        (
            panel.read(cx).reads(),
            panel.read(cx).phase().clone(),
            panel.read(cx).scope(),
        )
    });
    assert_eq!((reads, phase, scope), (1, Phase::Ready, Scope::WorkingTree));
    assert_eq!(
        rig.backend.diffs().reads.len(),
        1,
        "one read for the thread opening"
    );
    assert_eq!(
        rig.entries(),
        vec![
            ("assets/logo.png".to_string(), 'M', 0, 0, true),
            ("Cargo.toml".to_string(), 'M', 1, 1, false),
            ("data/long.json".to_string(), 'M', 1, 1, false),
            ("docs/guide/intro.md".to_string(), 'R', 1, 1, false),
            ("notes/old.md".to_string(), 'D', 0, 3, false),
            ("src/main.rs".to_string(), 'M', 4, 2, false),
            ("tools/greet.py".to_string(), 'A', 8, 0, false),
        ]
    );
    // The renamed file remembers where it came from.
    let origin = rig.cx.update(|cx| {
        panel
            .read(cx)
            .entries()
            .iter()
            .find(|e| e.status == Status::Renamed)
            .and_then(|e| e.original.clone())
    });
    assert_eq!(origin.as_deref(), Some("docs/intro.md"));

    // A hidden panel reads nothing, even when the thread changes under it.
    let mut closed = self::rig(SMALL, &[]);
    closed.toggle_panel();
    assert!(!closed.state(|s| s.right_panel.open));
    let before = closed.backend.diffs().reads.len();
    closed.open("t2");
    assert_eq!(
        closed.backend.diffs().reads.len(),
        before,
        "a hidden panel reads nothing"
    );
    // Opening it reads the new thread's changes.
    closed.toggle_panel();
    assert_eq!(closed.backend.diffs().reads.len(), before + 1);
    assert_eq!(closed.backend.diffs().reads.last().unwrap().session, "t2");
}

#[core::prelude::v1::test]
fn a_failed_read_says_so_and_try_again_reads_again() {
    let mut rig = rig(MEDIUM, &[]);
    rig.backend.diffs().failure = Some("git did not answer within 30 s and was stopped".into());
    rig.panel_do(|p, cx| p.refresh(cx));
    // The old list stays under a failed refresh (it must not blank the panel)...
    let panel = rig.panel();
    let phase = rig.cx.update(|cx| panel.read(cx).phase().clone());
    assert!(matches!(phase, Phase::Failed(_)), "{phase:?}");
    assert_eq!(rig.entries().len(), 7);
    assert!(
        rig.layout().get("diff-notes").is_ok(),
        "the failure is said under the list"
    );
    // ...a thread switch drops it, and a failure then fills the body, with a retry.
    rig.backend.diffs().failure = Some("git did not answer".into());
    rig.open("t2");
    let l = rig.layout();
    assert!(l.get("diff-failed").is_ok() && l.get("diff-retry").is_ok());
    rig.click("diff-retry");
    let l = rig.layout();
    assert!(l.get("diff-failed").is_err(), "the retry read it");
    assert_eq!(rig.entries().len(), 7);
}

#[core::prelude::v1::test]
fn an_answer_that_arrives_after_a_newer_read_is_dropped() {
    let mut rig = rig(MEDIUM, &[]);
    // Two reads in a row without letting the first land: the second one is what shows.
    let panel = rig.panel();
    rig.cx.update(|cx| {
        panel.update(cx, |p, cx| {
            p.refresh(cx);
            p.set_scope(Scope::Thread, cx);
        })
    });
    rig.settle();
    assert_eq!(rig.scope(), Scope::Thread);
    assert_eq!(rig.paths(), ["Cargo.toml", "src/main.rs", "tools/greet.py"]);
}

// --- layout -----------------------------------------------------------------------------------------

fn check_rows(rig: &mut Rig, count: usize) {
    let l = rig.layout();
    let mut c = l.check();
    c.inside("diff-panel", "right-panel")
        .inside("diff-toolbar", "diff-panel")
        .inside("diff-list", "diff-panel")
        .inside("diff-view", "diff-panel")
        .no_overflow("diff-toolbar")
        .no_overflow("panel-tabs");
    let rows: Vec<String> = (0..count).map(|i| format!("diff-row:{i}")).collect();
    let row_names: Vec<&str> = rows.iter().map(String::as_str).collect();
    c.same_height(&row_names);
    for (i, row) in rows.iter().enumerate() {
        c.inside(row, "diff-list");
        c.same_row(row, &format!("diff-letter:{i}"));
        c.same_row(row, &format!("diff-stat:{i}"));
        c.inside(&format!("diff-letter:{i}"), row);
        c.inside(&format!("diff-stat:{i}"), row);
    }
    c.assert();
    // The letters share one column, the counts one right edge, the rows are stacked at a fixed step.
    let left = |name: &str| l.bounds(name).origin.x.as_f32();
    let right = |name: &str| (l.bounds(name).origin.x + l.bounds(name).size.width).as_f32();
    for i in 1..count {
        assert_eq!(
            left("diff-letter:0"),
            left(&format!("diff-letter:{i}")),
            "row {i}'s letter"
        );
        assert_eq!(
            right("diff-stat:0"),
            right(&format!("diff-stat:{i}")),
            "row {i}'s counts"
        );
        let step = l.bounds(&format!("diff-row:{i}")).origin.y - l.bounds("diff-row:0").origin.y;
        assert_eq!(step.as_f32(), i as f32 * diff::LIST_ROW, "row {i}'s place");
    }
}

#[core::prelude::v1::test]
fn the_panel_lays_out_at_three_window_sizes() {
    for window in [SMALL, MEDIUM, LARGE] {
        let mut rig = rig(window, &[]);
        let l = rig.layout();
        for name in [
            "right-panel",
            "panel-tabs",
            "panel-tab-diff",
            "diff-toolbar",
            "diff-scopes",
            "diff-scope-turn",
            "diff-scope-thread",
            "diff-scope-tree",
            "diff-summary",
            "diff-tools",
            "diff-refresh",
            "diff-collapse",
            "diff-split",
            "diff-tree",
            "diff-list",
            "diff-view",
        ] {
            assert!(l.get(name).is_ok(), "{window:?}: {name} is missing");
        }
        // Two tabs, Diff and Files; the strip is the panel's header row.
        assert_eq!(l.bounds("panel-tabs").size.height.as_f32(), 40.);
        assert!(l.get("panel-tab-files").is_ok());
        l.check().same_row("panel-tab-diff", "panel-tab-files").assert();
        check_rows(&mut rig, 7);
        let mut c = l.check();
        c.same_height(&["diff-refresh", "diff-collapse", "diff-split", "diff-tree"]);
        c.assert();
        // The list stops at its maximum height (7 rows of 26) and the diff takes the rest.
        assert_eq!(
            l.bounds("diff-list").size.height.as_f32(),
            7. * diff::LIST_ROW
        );
        assert!(
            l.bounds("diff-view").size.height.as_f32() > 100.,
            "{window:?}"
        );
    }
}

#[core::prelude::v1::test]
fn very_long_paths_and_lines_stay_inside_the_panel_wrapped_or_scrolling() {
    let deep = format!(
        "{}/file-with-a-long-name-{}.rs",
        "folder-with-a-long-name",
        "x".repeat(60)
    );
    let long = "word ".repeat(300);
    let patch = format!(
        "diff --git a/{deep} b/{deep}\n--- a/{deep}\n+++ b/{deep}\n@@ -1,2 +1,2 @@\n-let a = \"{long}old\";\n+let a = \"{long}new\";\n keep\n"
    );
    for window in [SMALL, MEDIUM] {
        for wrap in [true, false] {
            let mut rig = rig(window, &[]);
            rig.backend.diffs().working = patch.clone();
            rig.panel_do(|p, cx| {
                p.refresh(cx);
                p.set_wrap(wrap, cx);
            });
            assert_eq!(rig.entries().len(), 1);
            let l = rig.layout();
            let mut c = l.check();
            c.inside("diff-panel", "right-panel")
                .inside("diff-code", "diff-view")
                .no_overflow("diff-toolbar")
                .no_overflow("diff-list")
                .no_overflow("diff-panel");
            c.assert();
            // The panel keeps its width whatever the lines are: nothing pushes it wider.
            let panel_w = l.bounds("diff-panel").size.width.as_f32();
            let right_w = l.bounds("right-panel").size.width.as_f32();
            assert!(
                panel_w <= right_w + 1.,
                "{window:?} wrap={wrap}: {panel_w} > {right_w}"
            );
            assert!(l.bounds("diff-code").size.width.as_f32() <= right_w + 1.);
            // The long path is cut in its row, never wrapped to a second line.
            assert_eq!(l.bounds("diff-row:0").size.height.as_f32(), diff::LIST_ROW);
        }
    }
}

#[core::prelude::v1::test]
fn the_folder_tree_nests_rows_and_closes_a_folder() {
    let mut rig = rig(MEDIUM, &[]);
    rig.panel_do(|p, cx| p.set_tree(true, cx));
    let panel = rig.panel();
    let rows = rig.cx.update(|cx| panel.read(cx).rows().to_vec());
    let dirs: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            ListRow::Dir { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        dirs,
        ["assets", "data", "docs/guide", "notes", "src", "tools"]
    );
    let files = rows
        .iter()
        .filter(|r| matches!(r, ListRow::File { .. }))
        .count();
    assert_eq!(files, 7, "Cargo.toml and six in folders");
    // Every row is one line high.
    let l = rig.layout();
    for i in 0..rows.len().min(7) {
        assert_eq!(
            l.bounds(&format!("diff-row:{i}")).size.height.as_f32(),
            diff::LIST_ROW
        );
    }
    // Closing a folder takes its rows out of the list.
    let before = rows.len();
    rig.panel_do(|p, cx| p.toggle_folder("src", cx));
    let after = rig.cx.update(|cx| panel.read(cx).rows().len());
    assert_eq!(after, before - 1, "src had one file");
    // Back to the flat list: a row per file again.
    rig.panel_do(|p, cx| p.set_tree(false, cx));
    assert_eq!(rig.cx.update(|cx| panel.read(cx).rows().len()), 7);
    check_rows(&mut rig, 7);
}

// --- scopes ---------------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn each_scope_lists_its_own_files_through_the_scope_buttons() {
    let mut rig = rig(MEDIUM, &[]);
    assert_eq!(rig.entries().len(), 7);
    rig.click("diff-scope-thread");
    assert_eq!(rig.paths(), ["Cargo.toml", "src/main.rs", "tools/greet.py"]);
    let reads = rig.backend.diffs().reads.clone();
    assert_eq!(reads.last().unwrap().scope, Scope::Thread);
    // This turn, with nothing known about it (the thread was opened from the store and its newest
    // footer names no file): an empty list with the note, not the whole working tree.
    rig.click("diff-scope-turn");
    assert!(rig.entries().is_empty());
    assert!(rig.layout().get("diff-empty").is_ok());
    // Back to the working tree.
    rig.click("diff-scope-tree");
    assert_eq!(rig.entries().len(), 7);
    assert_eq!(rig.scope(), Scope::WorkingTree);
}

#[core::prelude::v1::test]
fn this_turn_uses_the_commit_the_turn_began_at_else_the_files_its_footer_names() {
    // The footer names two files and no base commit is known: the working tree narrowed to them.
    let mut rig = rig(MEDIUM, &["src/main.rs", "tools/greet.py"]);
    rig.click("diff-scope-turn");
    assert_eq!(rig.paths(), ["src/main.rs", "tools/greet.py"]);
    let request = rig.backend.diffs().reads.last().cloned().unwrap();
    assert_eq!(request.turn.base, None);
    assert_eq!(request.turn.files, ["src/main.rs", "tools/greet.py"]);
    // A turn seen starting in this run has a base: the backend's turn patch is what shows.
    rig.backend.diffs().head = Some("0123456789abcdef".into());
    let thread = rig
        .cx
        .update(|cx| rig.app.read(cx).thread().cloned())
        .unwrap();
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.settle();
    let context = rig.cx.update(|cx| thread.read(cx).turn_context());
    assert_eq!(context.base.as_deref(), Some("0123456789abcdef"));
    rig.backend.diffs().turn = fixture::python_file();
    rig.panel_do(|p, cx| p.refresh(cx));
    assert_eq!(rig.paths(), ["tools/greet.py"]);
    assert_eq!(
        rig.backend
            .diffs()
            .reads
            .last()
            .unwrap()
            .turn
            .base
            .as_deref(),
        Some("0123456789abcdef")
    );
}

#[core::prelude::v1::test]
fn a_turn_ending_reads_the_diff_again_and_the_footer_follows_git() {
    let mut rig = rig(MEDIUM, &[]);
    rig.backend.diffs().head = Some("abc123".into());
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.settle();
    let before = rig.backend.diffs().reads.len();
    // A live turn ends with no tool call: git, asked for the turn's files, lists the fixture's
    // turn patch (main.rs modified, greet.py new); the footer, which named none, then names the
    // tracked one (a new file counts only when a card wrote it).
    rig.backend.emit(
        Recorded::TurnEnded {
            outcome: "complete".into(),
            error: None,
            text: "done".into(),
            user_row: None,
            last_row: None,
        }
        .to_core("t1"),
    );
    rig.settle();
    let reads = rig.backend.diffs().reads.clone();
    assert!(
        reads.len() >= before + 2,
        "the footer's turn read and the panel's refresh: {before} -> {}",
        reads.len()
    );
    let thread = rig
        .cx
        .update(|cx| rig.app.read(cx).thread().cloned())
        .unwrap();
    let context = rig.cx.update(|cx| thread.read(cx).turn_context());
    assert_eq!(context.files, ["src/main.rs"]);
}

// --- revert and the editor -------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_revert_needs_a_confirmation_and_then_takes_the_file_out_of_the_list() {
    let mut rig = rig(MEDIUM, &[]);
    // Nothing happens without asking first.
    rig.panel_do(|p, cx| p.confirm_revert(cx));
    assert!(rig.backend.diffs().reverted.is_empty());
    // The button in the file's header asks (Cargo.toml is the second file: on screen).
    let ask = "diff-revert:Cargo.toml";
    assert!(
        rig.layout().get(ask).is_ok(),
        "the revert button is in the file's header"
    );
    rig.click(ask);
    let panel = rig.panel();
    let confirm = rig
        .cx
        .update(|cx| panel.read(cx).confirm().cloned())
        .unwrap();
    assert_eq!(
        (confirm.path.as_str(), confirm.kind),
        ("Cargo.toml", Revert::ToHead)
    );
    assert!(rig.layout().get("diff-confirm").is_ok());
    assert!(
        rig.backend.diffs().reverted.is_empty(),
        "asking reverts nothing"
    );
    // Cancel: still nothing, the bar goes.
    rig.click("diff-confirm-cancel");
    assert!(rig.layout().get("diff-confirm").is_err());
    assert!(rig.backend.diffs().reverted.is_empty());
    // Ask again and confirm: the backend reverts exactly that file, the list reads again.
    let reads = rig.backend.diffs().reads.len();
    rig.click(ask);
    rig.click("diff-confirm-ok");
    assert_eq!(
        rig.backend.diffs().reverted,
        vec![(Scope::WorkingTree, "Cargo.toml".to_string())]
    );
    // With the blob the diff showed, so a file changed since is refused (the real backend).
    assert_eq!(
        rig.backend.diffs().reverted_seen,
        vec![Some("9c8d7e6".to_string())]
    );
    assert_eq!(rig.backend.diffs().reads.len(), reads + 1);
    assert!(!rig.paths().contains(&"Cargo.toml".to_string()));
    assert_eq!(rig.entries().len(), 6);
    assert!(
        rig.toasts()
            .iter()
            .any(|(kind, title)| *kind == toast::Kind::Success && title == "File reverted")
    );
    assert!(rig.layout().get("diff-confirm").is_err());
}

#[core::prelude::v1::test]
fn a_confirmation_goes_when_the_file_changes_before_it_is_confirmed() {
    let mut rig = rig(MEDIUM, &[]);
    rig.panel_do(|p, cx| p.ask_revert("Cargo.toml", cx));
    assert!(rig.layout().get("diff-confirm").is_ok());
    // A read in between (the window came forward, a turn ended) finds Cargo.toml changed again.
    {
        let mut diffs = rig.backend.diffs();
        let toml = super::fixture::toml_file();
        let changed = toml.replace("3f2a1b0..9c8d7e6", "3f2a1b0..1234abc");
        diffs.working = diffs.working.replace(&toml, &changed);
    }
    rig.panel_do(|p, cx| p.refresh(cx));
    assert!(
        rig.layout().get("diff-confirm").is_err(),
        "the user confirms what they saw, not what is there now"
    );
    rig.panel_do(|p, cx| p.confirm_revert(cx));
    assert!(rig.backend.diffs().reverted.is_empty());
    // A read that finds the same file keeps the question.
    rig.panel_do(|p, cx| p.ask_revert("Cargo.toml", cx));
    rig.panel_do(|p, cx| p.refresh(cx));
    assert!(rig.layout().get("diff-confirm").is_ok());
}

#[core::prelude::v1::test]
fn a_failed_revert_says_why_and_leaves_the_list() {
    let mut rig = rig(MEDIUM, &[]);
    rig.backend.diffs().revert_failure = Some("could not lock the index".into());
    rig.panel_do(|p, cx| {
        p.ask_revert("Cargo.toml", cx);
        p.confirm_revert(cx);
    });
    assert!(rig.backend.diffs().reverted.is_empty());
    assert!(
        rig.toasts()
            .iter()
            .any(|(kind, _)| *kind == toast::Kind::Error)
    );
    assert_eq!(rig.entries().len(), 7);
}

#[core::prelude::v1::test]
fn reverting_depends_on_the_scope_and_the_kind_of_change() {
    let mut rig = rig(MEDIUM, &[]);
    let panel = rig.panel();
    let state = |rig: &mut Rig, path: &str| {
        let panel = panel.clone();
        let path = path.to_string();
        rig.cx.update(|cx| {
            let p = panel.read(cx);
            let entry = p
                .entries()
                .iter()
                .find(|e| e.path == path)
                .cloned()
                .unwrap();
            p.revert_state(&entry)
        })
    };
    // Working tree: to HEAD, except a renamed file.
    assert_eq!(state(&mut rig, "Cargo.toml"), Ok(Revert::ToHead));
    assert_eq!(
        state(&mut rig, "notes/old.md"),
        Ok(Revert::ToHead),
        "a deleted file comes back"
    );
    assert!(
        state(&mut rig, "docs/guide/intro.md").is_err(),
        "a rename is not reverted from here"
    );
    rig.panel_do(|p, cx| p.ask_revert("docs/guide/intro.md", cx));
    assert!(rig.cx.update(|cx| panel.read(cx).confirm().is_none()));
    // This thread: back to before the thread's first edit (checkpointed, so undoable).
    rig.click("diff-scope-thread");
    assert_eq!(state(&mut rig, "src/main.rs"), Ok(Revert::ToThreadStart));
    rig.panel_do(|p, cx| p.ask_revert("src/main.rs", cx));
    assert_eq!(
        rig.cx
            .update(|cx| panel.read(cx).confirm().cloned())
            .unwrap()
            .kind,
        Revert::ToThreadStart
    );
    rig.panel_do(|p, cx| p.cancel_revert(cx));
    // This turn: no revert (it would go to HEAD, not to before the turn), and no buttons.
    rig.backend.diffs().head = Some("abc".into());
    rig.backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.settle();
    rig.panel_do(|p, cx| p.set_scope(Scope::Turn, cx));
    assert!(!rig.entries().is_empty());
    let l = rig.layout();
    // (Ids that hold a slash are matched on the last segment of the dump's paths.)
    let named = |name: &str| {
        l.dump()
            .entries
            .iter()
            .any(|e| e.path.last().is_some_and(|n| n == name))
    };
    assert!(
        named("diff-open:src/main.rs"),
        "the file's header has its editor button"
    );
    assert!(
        !named("diff-revert:src/main.rs"),
        "and no revert button in this scope"
    );
}

#[core::prelude::v1::test]
fn open_in_editor_asks_the_backend_for_that_file() {
    let mut rig = rig(MEDIUM, &[]);
    assert!(rig.layout().get("diff-open:Cargo.toml").is_ok());
    rig.click("diff-open:Cargo.toml");
    assert_eq!(
        rig.backend.diffs().opened,
        vec![("t1".to_string(), "Cargo.toml".to_string(), None)]
    );
    assert!(
        rig.toasts()
            .iter()
            .any(|(_, title)| title.starts_with("Opened in"))
    );
}

// --- the app's actions ------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn open_diff_opens_the_panel_on_the_diff_tab_and_a_path_on_the_turn_when_it_is_known() {
    let mut rig = rig(MEDIUM, &["src/main.rs"]);
    rig.toggle_panel();
    assert!(!rig.state(|s| s.right_panel.open));
    rig.dispatch(OpenDiffTab { path: None });
    assert!(
        rig.state(|s| s.right_panel.open),
        "the action opens the panel"
    );
    let right = rig.right();
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        Tab::Diff
    );
    assert_eq!(rig.scope(), Scope::WorkingTree);
    // With a path (a card or the footer) the turn's changes show, scrolled to the file.
    rig.dispatch(OpenDiffTab {
        path: Some("src/main.rs".into()),
    });
    assert_eq!(rig.scope(), Scope::Turn);
    assert_eq!(rig.paths(), ["src/main.rs"]);
    assert!(rig.layout().get("diff-panel").is_ok());
    assert!(rig.toasts().is_empty(), "no toast: the panel is the answer");
}

#[core::prelude::v1::test]
fn a_file_link_opens_the_files_tab_on_that_file_and_the_diff_is_one_click_away() {
    let mut rig = rig(MEDIUM, &[]);
    // A file the working tree changes: the Files tab shows it and offers its diff.
    rig.dispatch(OpenFileInPanel {
        path: "src/main.rs".into(),
        line: Some(3),
    });
    let right = rig.right();
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        Tab::Files
    );
    let l = rig.layout();
    assert!(l.get("file-view").is_ok() && l.get("diff-panel").is_err());
    assert!(l.get("file-diff").is_ok(), "a changed file offers its diff");
    assert!(rig.toasts().is_empty());
    // "Diff" opens the Diff tab on the working tree, without reading again.
    let reads = rig.backend.diffs().reads.len();
    rig.click("file-diff");
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        Tab::Diff
    );
    assert!(rig.layout().get("diff-panel").is_ok());
    assert_eq!(rig.scope(), Scope::WorkingTree);
    assert_eq!(rig.backend.diffs().reads.len(), reads);
}

#[core::prelude::v1::test]
fn the_tab_model_keeps_diff_first_and_only_the_open_tabs() {
    let mut tabs = super::Tabs::default();
    assert_eq!(
        (tabs.open(), tabs.active()),
        (&[Tab::Diff, Tab::Files][..], Tab::Diff)
    );
    assert!(tabs.activate(Tab::Files));
    tabs.show(Tab::Diff);
    tabs.show(Tab::Files);
    assert_eq!(tabs.open().len(), 2, "no duplicates");
    tabs.close(Tab::Diff);
    tabs.close(Tab::Files);
    assert_eq!(tabs.open().len(), 2, "Diff and Files stay");
}

// --- big diffs ----------------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn huge_files_and_everything_past_the_budget_open_collapsed() {
    let entry = |path: &str, added: usize| super::data::Entry {
        path: path.to_string(),
        original: None,
        status: Status::Added,
        added,
        removed: 0,
        binary: false,
        truncated: false,
    };
    // One huge file collapses alone.
    let plan = collapse_plan(&[entry("small", 10), entry("huge", 1001), entry("last", 10)]);
    assert_eq!(plan, ["huge"]);
    // Six files of 1,000 lines: five fill the budget, the sixth opens collapsed.
    let six: Vec<_> = (0..6).map(|n| entry(&format!("f{n}"), 1000)).collect();
    assert_eq!(collapse_plan(&six), ["f5"]);
    assert!(collapse_plan(&[entry("a", 5), entry("b", 7)]).is_empty());
    // Syntax colours: a huge file is plain, and so is what comes after 2,000 lines (a small
    // file after that still fits when it is within the budget).
    let plain = super::diff::plain_plan(&[
        entry("small", 10),
        entry("huge", 1001),
        entry("big", 1000),
        entry("big2", 995),
        entry("tiny", 5),
    ]);
    assert_eq!(plain, ["huge", "big2"]);
}

#[core::prelude::v1::test]
fn a_six_thousand_line_diff_reads_parses_and_draws_per_visible_row() {
    let patch = fixture::big_patch(6, 1000);
    let started = std::time::Instant::now();
    let parsed = parse_patch(&patch);
    let parse_ms = started.elapsed().as_secs_f64() * 1000.;
    assert_eq!(parsed.totals(), (6, 6000, 0));

    let mut rig = rig(MEDIUM, &[]);
    rig.backend.diffs().working = patch.clone();
    // What the UI thread pays when the patch arrives: Kit's rows for 6,000 lines and the first
    // frame (the read and the parse ran on a background thread).
    let panel = rig.panel();
    let started = std::time::Instant::now();
    rig.cx.update(|cx| {
        panel.update(cx, |p, cx| {
            p.show_data(
                Scope::WorkingTree,
                super::data::DiffData {
                    patch,
                    ..Default::default()
                },
                cx,
            )
        })
    });
    draw(&mut rig.cx, rig.window);
    let install_ms = started.elapsed().as_secs_f64() * 1000.;
    // Then the background preparation (syntax colours, word-level changes) until it is all in.
    let started = std::time::Instant::now();
    rig.settle();
    let prepare_ms = started.elapsed().as_secs_f64() * 1000.;
    assert_eq!(rig.entries().len(), 6);
    // Nothing is "huge" (1,000 lines each); the budget of 5,000 open lines collapses the last file.
    let panel = rig.panel();
    let collapsed: Vec<bool> = rig.cx.update(|cx| {
        let state = panel.read(cx).state().read(cx);
        panel
            .read(cx)
            .entries()
            .iter()
            .map(|e| state.is_file_collapsed(&e.path))
            .collect()
    });
    assert_eq!(collapsed, [false, false, false, false, false, true]);
    // Syntax colours for the first 2,000 changed lines only: the rest is plain text, so Kit's
    // background preparation does not run for seconds.
    let languages: Vec<String> = rig.cx.update(|cx| {
        let state = panel.read(cx).state().read(cx);
        state.files().iter().map(|f| f.language().to_string()).collect()
    });
    assert_eq!(
        languages.iter().filter(|l| l.as_str() != "text").count(),
        2,
        "{languages:?}"
    );

    // Frames while the diff is scrolled through: a frame draws what is visible only.
    let mut frames = Vec::new();
    for _ in 0..30 {
        let started = std::time::Instant::now();
        draw(&mut rig.cx, rig.window);
        frames.push(started.elapsed().as_secs_f64() * 1000.);
    }
    let state = rig.cx.update(|cx| panel.read(cx).state().clone());
    for n in 0..5 {
        let path = format!("generated/part{n:03}.rs");
        rig.cx
            .update(|cx| state.update(cx, |s, cx| s.scroll_to_file(&path, cx)));
        let started = std::time::Instant::now();
        rig.cx.run_until_parked();
        draw(&mut rig.cx, rig.window);
        frames.push(started.elapsed().as_secs_f64() * 1000.);
    }
    let mean = frames.iter().sum::<f64>() / frames.len() as f64;
    let max = frames.iter().cloned().fold(0., f64::max);
    println!(
        "panel-perf: parse of 6,000 lines {parse_ms:.1} ms; installing it and the first frame \
         {install_ms:.0} ms on the UI thread; syntax and word-level preparation {prepare_ms:.0} ms in the \
         background; {} frames: mean {mean:.1} ms, max {max:.1} ms (debug build, headless)",
        frames.len()
    );
    assert!(parse_ms < 2000., "parse took {parse_ms} ms");
    assert!(install_ms < 5000., "installing took {install_ms} ms");
    // A frame is not proportional to the diff: 6,000 lines are not all drawn.
    assert!(mean < 400., "mean frame {mean} ms");
}

#[core::prelude::v1::test]
fn collapse_all_and_open_all_toggle_every_file() {
    let mut rig = rig(MEDIUM, &[]);
    let panel = rig.panel();
    let collapsed = |rig: &mut Rig| -> usize {
        let panel = panel.clone();
        rig.cx.update(|cx| {
            let state = panel.read(cx).state().read(cx);
            panel
                .read(cx)
                .entries()
                .iter()
                .filter(|e| state.is_file_collapsed(&e.path))
                .count()
        })
    };
    assert_eq!(collapsed(&mut rig), 0);
    rig.click("diff-collapse");
    assert_eq!(collapsed(&mut rig), 7);
    rig.click("diff-collapse");
    assert_eq!(collapsed(&mut rig), 0);
}

#[core::prelude::v1::test]
fn split_wrap_and_the_file_list_are_toggles_and_a_row_opens_its_file() {
    let mut rig = rig(MEDIUM, &[]);
    let panel = rig.panel();
    let flags = |rig: &mut Rig| {
        let panel = panel.clone();
        rig.cx.update(|cx| {
            let p = panel.read(cx);
            (p.is_split(), p.is_wrapped(), p.list_open())
        })
    };
    assert_eq!(flags(&mut rig), (false, true, true));
    rig.click("diff-split");
    assert_eq!(flags(&mut rig).0, true);
    rig.click("diff-split");
    assert_eq!(flags(&mut rig).0, false);
    rig.click("diff-wrap");
    assert_eq!(flags(&mut rig).1, false);
    rig.click("diff-summary");
    assert_eq!(flags(&mut rig).2, false);
    assert!(
        rig.layout().get("diff-list").is_err(),
        "the list is gone, the diff stays"
    );
    assert!(rig.layout().get("diff-view").is_ok());
    rig.click("diff-summary");
    assert!(rig.layout().get("diff-list").is_ok());
    // A click on a row of the list scrolls the diff to that file and opens it.
    rig.click("diff-collapse");
    rig.click("diff-row:3");
    let collapsed = rig.cx.update(|cx| {
        panel
            .read(cx)
            .state()
            .read(cx)
            .is_file_collapsed("docs/guide/intro.md")
    });
    assert!(!collapsed, "the row's file is opened");
}
