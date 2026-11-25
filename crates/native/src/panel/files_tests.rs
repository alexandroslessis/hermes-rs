//! The Files tab, headless with real text: the tree (lazy listings, what git ignores, keys, the
//! layout at three window sizes), the file view for every kind of file (source, markdown, SVG,
//! picture, binary, lossy, large), jump to a line, every refusal, "Open in editor", "Diff", and
//! what the app's file-link action does. Over the fixture backend's virtual project, and over real
//! temporary folders ([`Source::Disk`]) where `.gitignore` files or the disk's own behaviour
//! matter. The finder is in `finder_tests.rs`; the disk rules themselves are in `fs.rs`.

use gpui_kit::Entity;

use super::Tab;
use super::files::{self, FilesPanel, Mode};
use super::files::{TreeActivate, TreeCollapse, TreeDown, TreeExpand, TreeFirst, TreeLast, TreeUp};
use super::fixture_files::Source;
use super::fs::Refused;
use super::tests::{LARGE, MEDIUM, Rig, SMALL, rig};
use super::viewer::{FileView, Kind, ViewMode};
use crate::thread::OpenFileInPanel;

pub(super) fn files_rig(window: (f32, f32)) -> Rig {
    let mut rig = rig(window, &[]);
    rig.cx.update(|cx| {
        files::bind(cx);
        super::finder::bind(cx);
    });
    rig
}

/// Opens the Files tab (its first read happens now).
pub(super) fn go_files(rig: &mut Rig) {
    rig.click("panel-tab-files");
}

pub(super) fn files_panel(rig: &mut Rig) -> Entity<FilesPanel> {
    let right = rig.right();
    rig.cx.update(|cx| right.read(cx).files().clone())
}

pub(super) fn view(rig: &mut Rig) -> Entity<FileView> {
    let files = files_panel(rig);
    rig.cx.update(|cx| files.read(cx).view().clone())
}

/// The visible rows as `depth * 2 spaces + name`.
fn rows(rig: &mut Rig) -> Vec<String> {
    let files = files_panel(rig);
    rig.cx.update(|cx| {
        files
            .read(cx)
            .tree()
            .rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    })
}

fn row_ix(rig: &mut Rig, path: &str) -> usize {
    let files = files_panel(rig);
    rig.cx
        .update(|cx| files.read(cx).tree().row_of(path))
        .unwrap_or_else(|| panic!("{path} is not a visible row"))
}

fn click_row(rig: &mut Rig, path: &str) {
    let ix = row_ix(rig, path);
    rig.click(&format!("file-row:{ix}"));
}

fn lists(rig: &mut Rig) -> Vec<String> {
    let files = files_panel(rig);
    rig.cx.update(|cx| files.read(cx).lists().to_vec())
}

fn selected(rig: &mut Rig) -> Option<String> {
    let files = files_panel(rig);
    rig.cx
        .update(|cx| files.read(cx).tree().selected().map(str::to_string))
}

fn mode(rig: &mut Rig) -> Mode {
    let files = files_panel(rig);
    rig.cx.update(|cx| files.read(cx).mode())
}

fn open_link(rig: &mut Rig, path: &str, line: Option<u32>) {
    rig.dispatch(OpenFileInPanel {
        path: path.to_string(),
        line,
    });
    // A read, a decode and a tree catching up: a few turns of the executor.
    for _ in 0..4 {
        rig.settle();
    }
}

fn kind(rig: &mut Rig) -> Option<Kind> {
    let v = view(rig);
    rig.cx.update(|cx| v.read(cx).kind())
}

fn refused(rig: &mut Rig) -> Option<Refused> {
    let v = view(rig);
    rig.cx.update(|cx| v.read(cx).refused().cloned())
}

fn editor_text(rig: &mut Rig) -> String {
    let v = view(rig);
    rig.cx.update(|cx| {
        let editor = v.read(cx).editor().expect("a code editor").clone();
        editor.read(cx).text().to_string()
    })
}

fn shown(rig: &mut Rig) -> Option<String> {
    let v = view(rig);
    rig.cx.update(|cx| v.read(cx).shown().map(str::to_string))
}

// --- the tree ---------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_tab_reads_only_the_root_and_a_folder_only_when_it_is_opened() {
    let mut rig = files_rig(MEDIUM);
    // Not on the Files tab yet: nothing was read.
    assert!(lists(&mut rig).is_empty());
    go_files(&mut rig);
    assert_eq!(lists(&mut rig), [""]);
    assert_eq!(
        rows(&mut rig),
        [
            "assets",
            "crates",
            "data",
            "docs",
            "node_modules",
            "outside",
            "src",
            "target",
            "tools",
            ".env",
            ".gitignore",
            "Cargo.toml",
            "debug.log",
            "README.md"
        ]
    );
    click_row(&mut rig, "src");
    assert_eq!(lists(&mut rig), ["", "src"]);
    assert!(rows(&mut rig).contains(&"  lib.rs".to_string()));
    // Closed and opened again: not read again. The ignored folders were never read.
    click_row(&mut rig, "src");
    click_row(&mut rig, "src");
    assert_eq!(lists(&mut rig), ["", "src"]);
    // What git ignores is marked.
    let files = files_panel(&mut rig);
    let ignored: Vec<String> = rig.cx.update(|cx| {
        files
            .read(cx)
            .tree()
            .rows()
            .iter()
            .filter(|r| r.ignored)
            .map(|r| r.name.clone())
            .collect()
    });
    assert_eq!(ignored, ["node_modules", "target", "debug.log"]);
    // Opening an ignored folder lists it as ignored (everything in it is).
    click_row(&mut rig, "target");
    let log = rig.backend.project().listed.clone();
    assert_eq!(log.last().unwrap().1, "target");
    assert!(
        log.last().unwrap().2,
        "listed as the inside of an ignored folder"
    );
    assert!(rows(&mut rig).contains(&"  debug".to_string()));
}

#[core::prelude::v1::test]
fn a_hidden_panel_reads_nothing_and_another_thread_starts_over() {
    let mut rig = files_rig(MEDIUM);
    rig.toggle_panel();
    rig.open("t2");
    assert!(lists(&mut rig).is_empty());
    rig.toggle_panel();
    go_files(&mut rig);
    assert_eq!(lists(&mut rig), [""]);
    assert_eq!(rig.backend.project().listed.last().unwrap().0, "t2");
    click_row(&mut rig, "src");
    rig.open("t1");
    // The tree and the file were forgotten; the tab reads the root again for the new thread.
    assert_eq!(rows(&mut rig).len(), 14);
    assert_eq!(lists(&mut rig), [""]);
    assert_eq!(mode(&mut rig), Mode::Tree);
}

#[core::prelude::v1::test]
fn the_tree_lays_out_at_three_window_sizes_with_names_that_never_overflow() {
    for window in [SMALL, MEDIUM, LARGE] {
        let mut rig = files_rig(window);
        {
            // A name far longer than the panel.
            let mut project = rig.backend.project();
            if let Source::Virtual(p) = &mut project.source {
                p.files.insert(
                    format!("{}.txt", "very-long-file-name-".repeat(12)),
                    b"x".to_vec(),
                );
            }
        }
        go_files(&mut rig);
        click_row(&mut rig, "src");
        click_row(&mut rig, "src/util");
        let n = rows(&mut rig).len();
        let l = rig.layout();
        let mut c = l.check();
        c.inside("files-panel", "right-panel")
            .inside("files-toolbar", "files-panel")
            .inside("files-list", "files-panel")
            .same_row("files-refresh", "files-search")
            .same_row("files-search", "files-collapse")
            .same_height(&["files-refresh", "files-collapse"])
            .inside("files-search", "files-toolbar")
            .no_overflow("files-toolbar");
        for i in 0..n {
            // A row cut by the bottom of the list is drawn only in part.
            let Ok(row) = l.get(&format!("file-row:{i}")) else {
                continue;
            };
            let list = l.bounds("files-list");
            if row.bounds.origin.y + row.bounds.size.height > list.origin.y + list.size.height {
                continue;
            }
            c.inside(&format!("file-row:{i}"), "files-list")
                .same_row(&format!("file-row:{i}"), &format!("file-name:{i}"))
                .same_row(&format!("file-row:{i}"), &format!("file-icon:{i}"))
                .inside(&format!("file-name:{i}"), &format!("file-row:{i}"));
        }
        c.assert();
        assert_eq!(l.bounds("files-toolbar").size.height.as_f32(), 40.);
        let x = |name: &str| l.bounds(name).origin.x.as_f32();
        let y = |name: &str| l.bounds(name).origin.y.as_f32();
        // Rows are stacked at a fixed step; depth 0 rows share an icon column; each level of
        // folders moves its icon right by the same indent.
        for i in 1..n.min(10) {
            assert_eq!(
                y(&format!("file-row:{i}")) - y("file-row:0"),
                i as f32 * files::ROW_HEIGHT
            );
        }
        let src = row_ix(&mut rig, "src");
        let util = row_ix(&mut rig, "src/util");
        let lib = row_ix(&mut rig, "src/lib.rs");
        let strings = row_ix(&mut rig, "src/util/strings.rs");
        let icon = |ix: usize| x(&format!("file-icon:{ix}"));
        assert_eq!(icon(src), icon(0), "depth 0 icons align");
        assert_eq!(icon(lib) - icon(src), files::INDENT);
        assert_eq!(icon(strings) - icon(util), files::INDENT);
        // A folder's chevron sits left of its icon; a file keeps the same column free.
        assert!(x(&format!("file-chevron:{src}")) < icon(src));
        assert_eq!(x(&format!("file-chevron:{src}")), x("file-chevron:0"));
    }
}

#[core::prelude::v1::test]
fn the_keys_walk_the_tree_open_folders_and_open_files() {
    let mut rig = files_rig(MEDIUM);
    go_files(&mut rig);
    // A click on a folder opens it and gives the tree the focus.
    click_row(&mut rig, "docs");
    assert_eq!(selected(&mut rig).as_deref(), Some("docs"));
    rig.dispatch(TreeDown);
    assert_eq!(selected(&mut rig).as_deref(), Some("docs/guide"));
    rig.dispatch(TreeExpand);
    rig.dispatch(TreeExpand);
    assert_eq!(selected(&mut rig).as_deref(), Some("docs/guide/intro.md"));
    rig.dispatch(TreeCollapse);
    assert_eq!(selected(&mut rig).as_deref(), Some("docs/guide"));
    rig.dispatch(TreeCollapse);
    assert!(
        !rows(&mut rig).contains(&"    intro.md".to_string()),
        "Left closed the folder"
    );
    rig.dispatch(TreeLast);
    assert_eq!(selected(&mut rig).as_deref(), Some("README.md"));
    rig.dispatch(TreeDown);
    assert_eq!(
        selected(&mut rig).as_deref(),
        Some("README.md"),
        "the end stays the end"
    );
    rig.dispatch(TreeFirst);
    assert_eq!(selected(&mut rig).as_deref(), Some("assets"));
    rig.dispatch(TreeUp);
    assert_eq!(selected(&mut rig).as_deref(), Some("assets"));
    // Enter on a folder toggles it, on a file opens it.
    rig.dispatch(TreeActivate);
    assert!(rows(&mut rig).contains(&"  logo.png".to_string()));
    rig.dispatch(TreeDown);
    rig.dispatch(TreeActivate);
    for _ in 0..4 {
        rig.settle();
    }
    assert_eq!(mode(&mut rig), Mode::File);
    assert_eq!(shown(&mut rig).as_deref(), Some("assets/data.bin"));
    assert_eq!(kind(&mut rig), Some(Kind::Binary));
}

#[core::prelude::v1::test]
fn refresh_lists_the_open_folders_again_and_keeps_the_rows() {
    let dir = hermes_testdir::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
    let mut rig = files_rig(MEDIUM);
    rig.backend.project().source = Source::Disk(dir.path().to_path_buf());
    go_files(&mut rig);
    click_row(&mut rig, "src");
    assert_eq!(rows(&mut rig), ["src", "  a.rs"]);
    std::fs::write(dir.path().join("src/b.rs"), "").unwrap();
    std::fs::write(dir.path().join("top.txt"), "").unwrap();
    rig.click("files-refresh");
    assert_eq!(rows(&mut rig), ["src", "  a.rs", "  b.rs", "top.txt"]);
    assert_eq!(lists(&mut rig), ["", "src", "", "src"]);
    // Collapse all closes every folder.
    rig.click("files-collapse");
    assert_eq!(rows(&mut rig), ["src", "top.txt"]);
    drop(rig);
    drop(dir);
}

#[core::prelude::v1::test]
fn a_real_folders_gitignore_files_shape_the_tree() {
    let dir = super::fs::tests::project();
    let mut rig = files_rig(MEDIUM);
    rig.backend.project().source = Source::Disk(dir.path().to_path_buf());
    go_files(&mut rig);
    click_row(&mut rig, "src");
    click_row(&mut rig, "src/gen");
    let files = files_panel(&mut rig);
    let flags: Vec<(String, bool)> = rig.cx.update(|cx| {
        files
            .read(cx)
            .tree()
            .rows()
            .iter()
            .filter(|r| r.depth == 2)
            .map(|r| (r.name.clone(), r.ignored))
            .collect()
    });
    assert_eq!(
        flags,
        [
            ("deep".to_string(), false),
            (".gitignore".to_string(), false),
            ("a.tmp".to_string(), true),
            ("b.rs".to_string(), false),
            ("keep.tmp".to_string(), false),
        ]
    );
    drop(rig);
    drop(dir);
}

// --- the file view -----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_click_on_a_file_shows_its_source_with_a_header_and_back_returns_to_the_tree() {
    let mut rig = files_rig(MEDIUM);
    go_files(&mut rig);
    click_row(&mut rig, "Cargo.toml");
    for _ in 0..4 {
        rig.settle();
    }
    assert_eq!(mode(&mut rig), Mode::File);
    assert_eq!(kind(&mut rig), Some(Kind::Code));
    assert!(editor_text(&mut rig).starts_with("[package]\nname = \"halo\""));
    let v = view(&mut rig);
    assert_eq!(
        rig.cx
            .update(|cx| v.read(cx).language().map(str::to_string))
            .as_deref(),
        Some("toml")
    );
    let l = rig.layout();
    l.check()
        .inside("file-view", "files-panel")
        .inside("file-header", "file-view")
        .same_row("file-back", "file-path")
        .same_row("file-path", "file-copy-path")
        .same_row("file-copy-path", "file-editor")
        .inside("file-subbar", "file-view")
        .inside("file-code", "file-view")
        .no_overflow("file-header")
        .no_overflow("file-subbar")
        .assert();
    assert_eq!(l.bounds("file-header").size.height.as_f32(), 40.);
    assert!(l.get("file-modes").is_err(), "no switch for plain source");
    // The tree knows where the file is.
    assert_eq!(selected(&mut rig).as_deref(), Some("Cargo.toml"));
    rig.click("file-back");
    assert_eq!(mode(&mut rig), Mode::Tree);
    assert!(rig.layout().get("files-list").is_ok());
}

#[core::prelude::v1::test]
fn a_link_with_a_line_selects_it_scrolls_to_it_and_opens_the_folders_above_it() {
    let mut rig = files_rig(LARGE);
    open_link(&mut rig, "src/generated.rs", Some(1500));
    assert_eq!(mode(&mut rig), Mode::File);
    let v = view(&mut rig);
    let (selection, text) = rig.cx.update(|cx| {
        let editor = v.read(cx).editor().unwrap().clone();
        let e = editor.read(cx);
        (e.selected_range(), e.text().to_string())
    });
    assert_eq!(
        &text[selection],
        "pub fn item_1500(value: u32) -> u32 { value.wrapping_mul(1500) + 1 }"
    );
    // Drawn: the line is among the rows the editor laid out, with context above it.
    for _ in 0..3 {
        rig.settle();
    }
    let range = rig
        .cx
        .update(|cx| v.read(cx).editor().unwrap().read(cx).visible_row_range())
        .expect("the editor laid out");
    assert!(range.contains(&1499), "{range:?}");
    assert!(range.start > 1400, "scrolled, not at the top: {range:?}");
    // The tree followed: folders above are open, the file is selected.
    rig.click("file-back");
    assert_eq!(selected(&mut rig).as_deref(), Some("src/generated.rs"));
    assert!(rows(&mut rig).iter().any(|r| r == "  generated.rs"));
    // A line past the end is the last line (the empty one after the final newline).
    open_link(&mut rig, "src/main.rs", Some(9999));
    let v = view(&mut rig);
    let (text, selection) = rig.cx.update(|cx| {
        let editor = v.read(cx).editor().unwrap().clone();
        let e = editor.read(cx);
        (e.text().to_string(), e.selected_range())
    });
    assert!(text[selection].is_empty());
}

#[core::prelude::v1::test]
fn markdown_opens_rendered_with_a_switch_to_its_source() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "README.md", None);
    assert_eq!(kind(&mut rig), Some(Kind::Markdown));
    let l = rig.layout();
    assert!(l.get("file-markdown").is_ok() && l.get("file-code").is_err());
    l.check()
        .same_row("file-mode-preview", "file-mode-source")
        .inside("file-modes", "file-subbar")
        .inside("file-markdown", "file-view")
        .assert();
    rig.click("file-mode-source");
    let v = view(&mut rig);
    assert_eq!(rig.cx.update(|cx| v.read(cx).mode()), ViewMode::Source);
    assert!(editor_text(&mut rig).starts_with("# halo"));
    let l = rig.layout();
    assert!(l.get("file-code").is_ok() && l.get("file-markdown").is_err());
    rig.click("file-mode-preview");
    assert!(rig.layout().get("file-markdown").is_ok());
    // A link with a line shows the source (it is the line that was asked for).
    open_link(&mut rig, "README.md", Some(3));
    assert_eq!(rig.cx.update(|cx| v.read(cx).mode()), ViewMode::Source);
}

#[core::prelude::v1::test]
fn an_svg_is_a_picture_with_a_switch_to_its_xml() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "assets/logo.svg", None);
    assert_eq!(kind(&mut rig), Some(Kind::Svg));
    assert!(rig.layout().get("file-svg").is_ok());
    rig.click("file-mode-source");
    assert!(editor_text(&mut rig).starts_with("<svg xmlns"));
    let v = view(&mut rig);
    assert_eq!(
        rig.cx
            .update(|cx| v.read(cx).language().map(str::to_string))
            .as_deref(),
        Some("xml")
    );
}

#[core::prelude::v1::test]
fn a_picture_is_decoded_off_the_ui_thread_and_drawn_with_its_size() {
    let mut rig = files_rig(MEDIUM);
    // Not `dispatch`: that lets the executor run to the end.
    rig.cx
        .update_window(rig.window, |_, window, cx| {
            window.dispatch_action(
                Box::new(OpenFileInPanel {
                    path: "assets/logo.png".into(),
                    line: None,
                }),
                cx,
            )
        })
        .unwrap();
    let v = view(&mut rig);
    assert!(
        rig.cx.update(|cx| v.read(cx).is_loading()),
        "the read is a background task"
    );
    for _ in 0..4 {
        rig.settle();
    }
    assert_eq!(kind(&mut rig), Some(Kind::Picture));
    let picture = rig
        .cx
        .update(|cx| v.read(cx).picture().map(|p| (p.width, p.height, p.shrunk)));
    assert_eq!(picture, Some((160, 100, false)));
    // The decode is a `background_spawn` (the headless test platform runs those on the test's
    // own thread, so a thread id cannot tell; what is checked is that it ran once, after the
    // read had landed
    let decoded = rig
        .cx
        .update(|cx| v.read(cx).decoded_on.lock().unwrap().len());
    assert_eq!(decoded, 1);
    let l = rig.layout();
    l.check()
        .inside("file-picture", "file-view")
        .no_overflow("file-subbar")
        .assert();
    // Deterministic: the read landing does not decode (the decode is a task for later). Right
    // after `show_read`, inside the same update of the UI thread, nothing is decoded yet.
    let png = crate::thread::composer::attach::test_png(4000, 3000);
    let read = super::fs::FileRead {
        rel: "big.png".into(),
        name: "big.png".into(),
        size: png.len() as u64,
        content: super::fs::Content::Image(png),
    };
    let window = rig.window;
    let (decoding, decoded_before) = rig
        .cx
        .update_window(window, |_, window, cx| {
            v.update(cx, |v, cx| {
                let before = v.decoded_on.lock().unwrap().len();
                v.show_read(read, None, window, cx);
                (v.picture().is_none() && v.kind() == Some(Kind::Picture), before)
            })
        })
        .unwrap();
    assert!(decoding, "decoded inside the UI thread's update");
    rig.settle();
    let after = rig.cx.update(|cx| v.read(cx).decoded_on.lock().unwrap().len());
    assert_eq!(after, decoded_before + 1, "then decoded once by the task");
    let shrunk = rig
        .cx
        .update(|cx| v.read(cx).picture().map(|p| (p.width, p.height, p.shrunk)));
    assert_eq!(shrunk, Some((4000, 3000, true)));
}

#[core::prelude::v1::test]
fn a_binary_file_says_so_and_offers_the_editor_which_runs_only_when_clicked() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "assets/data.bin", None);
    assert_eq!(kind(&mut rig), Some(Kind::Binary));
    assert!(rig.layout().get("file-binary").is_ok());
    assert!(rig.backend.project().opened.is_empty());
    rig.click("file-open-editor");
    assert_eq!(
        rig.backend.project().opened,
        [("t1".to_string(), "assets/data.bin".to_string(), None)]
    );
    assert!(rig.toasts().iter().any(|(_, t)| t.starts_with("Opened in")));
}

#[core::prelude::v1::test]
fn text_that_is_not_utf8_and_text_that_is_large_say_how_they_are_shown() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "docs/old.txt", None);
    let v = view(&mut rig);
    assert!(rig.cx.update(|cx| v.read(cx).is_lossy()));
    assert!(editor_text(&mut rig).contains("caf\u{FFFD} au lait"));
    assert!(rig.layout().get("file-notes").is_ok());
    // Over 2 MB: plain text, with a note (40,000 lines).
    {
        let mut project = rig.backend.project();
        if let Source::Virtual(p) = &mut project.source {
            let line = "let value = compute(index) + offset; // a line of some length\n";
            p.files
                .insert("data/big.rs".to_string(), line.repeat(40_000).into_bytes());
        }
    }
    let started = std::time::Instant::now();
    open_link(&mut rig, "data/big.rs", Some(30_000));
    println!(
        "a 2.4 MB, 40,000-line file opened and drawn in {:?} (debug build)",
        started.elapsed()
    );
    assert!(rig.cx.update(|cx| v.read(cx).is_plain()));
    assert_eq!(
        rig.cx
            .update(|cx| v.read(cx).language().map(str::to_string))
            .as_deref(),
        Some("text")
    );
    assert!(rig.layout().get("file-notes").is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

/// A minified bundle: one 5 MB line. It opens as plain text and is drawn without freezing (the
/// editor lays out the visible part of the line).
#[core::prelude::v1::test]
fn a_five_megabyte_single_line_opens_as_plain_text_in_bounded_time() {
    let mut rig = files_rig(MEDIUM);
    {
        let mut project = rig.backend.project();
        if let Source::Virtual(p) = &mut project.source {
            let line = "var a=function(b){return b+1};".repeat(5 * 1024 * 1024 / 30);
            p.files
                .insert("dist/bundle.min.js".to_string(), line.into_bytes());
        }
    }
    let started = std::time::Instant::now();
    open_link(&mut rig, "dist/bundle.min.js", None);
    let opened = started.elapsed();
    let frame = std::time::Instant::now();
    let l = rig.layout();
    println!(
        "a 5 MB single-line file opened in {opened:?}, drawn again in {:?} (debug build)",
        frame.elapsed()
    );
    assert!(l.get("file-code").is_ok());
    // Cut for the view, and the view says so.
    assert!(l.get("file-notes").is_ok());
    let text = editor_text(&mut rig);
    assert!(text.len() < super::fs::LINE_MAX + 8 && text.ends_with('…'));
    let v = view(&mut rig);
    assert!(rig.cx.update(|cx| v.read(cx).is_plain()));
    assert!(opened < std::time::Duration::from_secs(30), "{opened:?}");
    assert!(frame.elapsed() < std::time::Duration::from_secs(10));
}

#[core::prelude::v1::test]
fn every_refusal_is_a_message_and_only_a_too_large_file_offers_the_editor() {
    let mut rig = files_rig(MEDIUM);
    let cases: [(&str, fn(&Refused) -> bool); 8] = [
        ("../outside.txt", |r| matches!(r, Refused::Outside(_))),
        ("C:/Windows/win.ini", |r| matches!(r, Refused::Outside(_))),
        ("src/../../x", |r| matches!(r, Refused::Outside(_))),
        (".env", |r| matches!(r, Refused::Sensitive)),
        ("outside/secret.txt", |r| matches!(r, Refused::Escapes(_))),
        ("data/huge.sql", |r| matches!(r, Refused::TooLarge { .. })),
        ("src", |r| matches!(r, Refused::NotAFile(_))),
        ("src/missing.rs", |r| matches!(r, Refused::NotFound(_))),
    ];
    for (path, expected) in cases {
        open_link(&mut rig, path, Some(3));
        let got = refused(&mut rig).unwrap_or_else(|| panic!("{path} was not refused"));
        assert!(expected(&got), "{path}: {got:?}");
        let l = rig.layout();
        assert!(l.get("file-refused").is_ok(), "{path}");
        assert_eq!(
            l.get("file-open-editor").is_ok(),
            matches!(got, Refused::TooLarge { .. }),
            "{path}: the editor is offered only for a file that is too large"
        );
        l.check().no_overflow("file-header").assert();
    }
    assert!(
        rig.backend.project().opened.is_empty(),
        "nothing was opened or run"
    );
    // The editor refuses the same paths.
    assert!(rig.backend.project().open("t1", ".env", None).is_err());
    assert!(rig.backend.project().open("t1", "../x", None).is_err());
}

#[core::prelude::v1::test]
fn open_in_editor_from_the_header_asks_for_this_file_at_its_line() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "src/main.rs", Some(5));
    rig.click("file-editor");
    assert_eq!(
        rig.backend.project().opened,
        [("t1".to_string(), "src/main.rs".to_string(), Some(5))]
    );
    assert!(rig.toasts().iter().any(|(_, t)| t.starts_with("Opened in")));
}

#[core::prelude::v1::test]
fn a_changed_file_offers_its_diff_and_an_unchanged_one_does_not() {
    let mut rig = files_rig(MEDIUM);
    open_link(&mut rig, "README.md", None);
    assert!(rig.layout().get("file-diff").is_err());
    open_link(&mut rig, "Cargo.toml", None);
    assert!(rig.layout().get("file-diff").is_ok());
    rig.click("file-diff");
    let right = rig.right();
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        Tab::Diff
    );
}

#[core::prelude::v1::test]
fn the_file_link_action_opens_the_panel_on_the_files_tab_and_a_second_link_replaces_the_file() {
    let mut rig = files_rig(MEDIUM);
    rig.toggle_panel();
    assert!(!rig.state(|s| s.right_panel.open));
    open_link(&mut rig, "src/lib.rs", Some(2));
    assert!(rig.state(|s| s.right_panel.open));
    let right = rig.right();
    assert_eq!(
        rig.cx.update(|cx| right.read(cx).tabs().active()),
        Tab::Files
    );
    assert_eq!(shown(&mut rig).as_deref(), Some("src/lib.rs"));
    open_link(&mut rig, "docs/guide/intro.md", None);
    assert_eq!(shown(&mut rig).as_deref(), Some("docs/guide/intro.md"));
    assert!(rig.toasts().is_empty());
    // The strip's tabs switch bodies; the file is still there after a round trip.
    rig.click("panel-tab-diff");
    assert!(rig.layout().get("files-panel").is_err());
    rig.click("panel-tab-files");
    assert!(rig.layout().get("file-view").is_ok());
}
