//! The file finder, headless: where the dialog sits, the results for a query over the fixture
//! project, the keys, what Enter and Escape do, the toolbar field that opens it, an index that
//! lands in batches while a query is already typed, and a real folder of 20,000 files. The ranking
//! itself (and the 100,000-path timing) is in `finder.rs`; the index walk (100,000 real files) is
//! in `fs.rs`.

use gpui_kit::Entity;

use super::files_tests::{files_rig, go_files, view};
use super::finder::{FileFinder, Indexing, LIMIT, WIDTH};
use super::fixture_files::Source;
use super::tests::{MEDIUM, Rig, SMALL};
use crate::keymap::OpenFileFinder;
use crate::palette::{Confirm, Dismiss, SelectNext, SelectPrevious};

fn finder(rig: &mut Rig) -> Option<Entity<FileFinder>> {
    let ws = rig.workspace.clone();
    rig.cx.update(|cx| ws.read(cx).finder().cloned())
}

fn open_finder(rig: &mut Rig) -> Entity<FileFinder> {
    rig.dispatch(OpenFileFinder);
    for _ in 0..3 {
        rig.settle();
    }
    finder(rig).expect("the finder is open")
}

fn type_query(rig: &mut Rig, finder: &Entity<FileFinder>, query: &str) {
    let finder = finder.clone();
    let query = query.to_string();
    rig.cx
        .update_window(rig.window, |_, window, cx| {
            finder.update(cx, |f, cx| f.set_query(&query, window, cx))
        })
        .unwrap();
    // Ranking runs on the background executor: let it finish (and rank again if it was asked to).
    for _ in 0..20 {
        rig.settle();
        let f = finder.clone();
        if !rig.cx.update(|cx| f.read(cx).busy()) {
            break;
        }
    }
}

fn paths(rig: &mut Rig, finder: &Entity<FileFinder>) -> Vec<String> {
    let f = finder.clone();
    rig.cx
        .update(|cx| f.read(cx).hits().iter().map(|h| h.path.clone()).collect())
}

#[core::prelude::v1::test]
fn ctrl_p_opens_a_dialog_in_the_palettes_place_that_ranks_the_projects_files() {
    for window in [SMALL, MEDIUM] {
        let mut rig = files_rig(window);
        let f = open_finder(&mut rig);
        let l = rig.layout();
        l.check()
            .inside("finder-search", "finder")
            .inside("finder-status", "finder")
            .inside("finder", "finder-layer")
            .no_overflow("finder-search")
            .assert();
        let b = l.bounds("finder");
        assert_eq!(b.size.width.as_f32(), WIDTH.min(window.0));
        let centre = b.origin.x.as_f32() + b.size.width.as_f32() / 2.;
        assert!((centre - window.0 / 2.).abs() < 1., "{window:?}: centred");
        assert!(
            (b.origin.y.as_f32() - (window.1 * 0.12).max(24.)).abs() < 1.,
            "{window:?}: 12% from the top, as the palette"
        );
        // Without a query: the first files; the index is complete (the sample has no ignored file in it).
        assert!(!paths(&mut rig, &f).is_empty());
        let files = rig.cx.update(|cx| f.read(cx).files());
        assert!(files > 15);
        assert_eq!(
            rig.cx.update(|cx| f.read(cx).indexing().clone()),
            Indexing::Done { truncated: false }
        );
        type_query(&mut rig, &f, "main");
        let hits = paths(&mut rig, &f);
        assert_eq!(hits[0], "src/main.rs", "{hits:?}");
        // The ignored folder's files and the credentials file are not offered.
        type_query(&mut rig, &f, "halo.exe");
        assert!(paths(&mut rig, &f).is_empty());
        type_query(&mut rig, &f, "env");
        assert!(!paths(&mut rig, &f).contains(&".env".to_string()));
        type_query(&mut rig, &f, "zzzz");
        assert!(rig.layout().get("finder-none").is_ok());
        type_query(&mut rig, &f, "gen");
        let l = rig.layout();
        assert!(l.get("finder-results").is_ok());
        l.check()
            .same_height(&["found:0", "found:1"])
            .same_row("found:0", "found-name:0")
            .assert();
    }
}

#[core::prelude::v1::test]
fn the_keys_move_enter_opens_the_file_and_escape_closes() {
    let mut rig = files_rig(MEDIUM);
    let f = open_finder(&mut rig);
    type_query(&mut rig, &f, "src");
    let hits = paths(&mut rig, &f);
    assert!(hits.len() >= 2);
    let sel = |rig: &mut Rig| {
        let f = f.clone();
        rig.cx.update(|cx| f.read(cx).selected())
    };
    assert_eq!(sel(&mut rig), 0);
    rig.dispatch(SelectNext);
    assert_eq!(sel(&mut rig), 1);
    rig.dispatch(SelectPrevious);
    rig.dispatch(SelectPrevious);
    assert_eq!(sel(&mut rig), hits.len() - 1, "wraps");
    rig.dispatch(SelectNext);
    // Escape closes it and gives the frame the focus back.
    rig.dispatch(Dismiss);
    assert!(finder(&mut rig).is_none());
    // Enter opens the selected file in the Files tab, at no particular line.
    let f = open_finder(&mut rig);
    type_query(&mut rig, &f, "intro");
    rig.dispatch(Confirm);
    for _ in 0..4 {
        rig.settle();
    }
    assert!(finder(&mut rig).is_none());
    let v = view(&mut rig);
    assert_eq!(
        rig.cx
            .update(|cx| v.read(cx).shown().map(str::to_string))
            .as_deref(),
        Some("docs/guide/intro.md")
    );
    let right = rig.right();
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        super::Tab::Files
    );
    // Ctrl+P again while open closes it.
    open_finder(&mut rig);
    rig.dispatch(OpenFileFinder);
    assert!(finder(&mut rig).is_none());
}

#[core::prelude::v1::test]
fn closing_the_finder_gives_the_focus_back_where_it_was() {
    use gpui_kit::Focusable as _;
    let mut rig = files_rig(MEDIUM);
    go_files(&mut rig);
    let files = super::files_tests::files_panel(&mut rig);
    let window = rig.window;
    rig.cx
        .update_window(window, |_, window, cx| {
            let handle = files.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        })
        .unwrap();
    open_finder(&mut rig);
    let tree_focused = |rig: &mut Rig| {
        let files = files.clone();
        rig.cx
            .update_window(window, |_, window, cx| {
                files.read(cx).focus_handle(cx).is_focused(window)
            })
            .unwrap()
    };
    assert!(!tree_focused(&mut rig), "the finder's input has it");
    rig.dispatch(Dismiss);
    assert!(finder(&mut rig).is_none());
    assert!(tree_focused(&mut rig), "back on the tree");
}

#[core::prelude::v1::test]
fn the_search_field_of_the_files_toolbar_opens_the_finder_and_no_thread_says_so() {
    let mut rig = files_rig(MEDIUM);
    go_files(&mut rig);
    rig.click("files-search");
    assert!(finder(&mut rig).is_some());
    rig.dispatch(Dismiss);
    // With no thread open there is no folder to search.
    let app = rig.app.clone();
    rig.cx
        .update(|cx| app.update(cx, |m, cx| m.clear_selection(cx)));
    rig.settle();
    rig.dispatch(OpenFileFinder);
    assert!(finder(&mut rig).is_none());
    assert!(
        rig.toasts()
            .iter()
            .any(|(_, t)| t.contains("Open a thread"))
    );
}

#[core::prelude::v1::test]
fn a_real_folder_of_twenty_thousand_files_is_searched_while_it_is_indexed_and_capped() {
    let dir = super::fs::tests::big_tree(20_000, 400);
    std::fs::write(dir.path().join(".gitignore"), "pkg0007/\n").unwrap();
    let mut rig = files_rig(MEDIUM);
    rig.backend.project().source = Source::Disk(dir.path().to_path_buf());
    let started = std::time::Instant::now();
    let f = open_finder(&mut rig);
    for _ in 0..200 {
        let f2 = f.clone();
        if rig.cx.update(|cx| f2.read(cx).indexing().clone()) != Indexing::Running {
            break;
        }
        rig.settle();
    }
    let indexed = started.elapsed();
    let files = rig.cx.update(|cx| f.read(cx).files());
    // 20,000 files in 400 folders, minus the 50 of the ignored `pkg0007` and plus `.gitignore`.
    assert_eq!(files, 20_000 - 50 + 1);
    type_query(&mut rig, &f, "file000420");
    let hits = paths(&mut rig, &f);
    assert_eq!(hits[0], "pkg0020/src/file000420.rs");
    let started = std::time::Instant::now();
    type_query(&mut rig, &f, "rs");
    let typed = started.elapsed();
    assert_eq!(paths(&mut rig, &f).len(), LIMIT, "capped");
    assert!(
        !paths(&mut rig, &f)
            .iter()
            .any(|p| p.starts_with("pkg0007/")),
        "ignored"
    );
    println!(
        "finder over 20,000 real files: indexed in {indexed:?}, a 2-letter query ranked in {typed:?}"
    );
    assert!(typed < std::time::Duration::from_secs(5));
    // A closed finder stops its walk: no task is left holding the index.
    rig.dispatch(Dismiss);
    drop(rig);
    drop(dir);
}
