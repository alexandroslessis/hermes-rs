//! The Files tab's stories (CP6.2): `panel-files`, the right panel as the app draws it (a 380 x 620
//! pane) on the Files tab over the fake project ([`crate::panel::fixture_files`]) in seven states:
//! `tree` (two folders open, a file selected, what git ignores dimmed), `code` (a Rust file with
//! line 5 selected, syntax colours, line numbers), `markdown` (a rendered README), `image` (a
//! decoded picture), `svg`, `binary`, and `refused` (a credentials file). And `file-finder`, the
//! `Ctrl+P` dialog in three states: results with the matched characters bold, no match, and an
//! index still being built. References: `sidebar-filetree.png` (the tree), `sidebar-right.png`
//! (the strip); t3code has no capture of the file view or the finder (the progress note says so).
//!
//! The panels are given their data directly ([`crate::panel::files::FilesPanel::seed`],
//! [`crate::panel::viewer::FileView::show_read`]): a story is drawn twice by the harness and cannot
//! wait for a background read.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Window, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    design::{Panel, panel::Edge},
    dev::ProbeExt as _,
    model::{
        AppModel, FixtureBackend,
        fixture::{self, NOW_MS},
    },
    panel::{
        RightPanel, Tab,
        finder::{FileFinder, Indexing},
        fixture_files::sample_project,
        fs::Refused,
    },
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const STATES: [State; 7] = [
    at_rest("tree"),
    at_rest("code"),
    at_rest("markdown"),
    at_rest("image"),
    at_rest("svg"),
    at_rest("binary"),
    at_rest("refused"),
];

pub const WIDTH: f32 = 380.;
pub const HEIGHT: f32 = 620.;

fn app(cx: &mut App) -> Entity<AppModel> {
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let app = cx.new(|cx| {
        AppModel::with_snapshot(Arc::clone(&backend) as _, snapshot, cx).with_clock(|| NOW_MS)
    });
    app.update(cx, |app, cx| app.open("t1", cx));
    app
}

fn make(name: &str, window: &mut Window, cx: &mut App) -> Entity<RightPanel> {
    let app = app(cx);
    let thread = app.read(cx).thread().cloned();
    let project = sample_project();
    let name = name.to_string();
    cx.new(|cx| {
        let mut panel = RightPanel::new(app, window, cx);
        panel.set_thread(thread, cx);
        panel.show_tab(Tab::Files, cx);
        let files = panel.files().clone();
        files.update(cx, |f, cx| match name.as_str() {
            "tree" => {
                let listing = |dir: &str| (dir.to_string(), project.list(dir, false).unwrap());
                f.seed(
                    vec![listing(""), listing("src"), listing("docs"), listing("docs/guide")],
                    &["src", "docs", "docs/guide"],
                    Some("src/lib.rs"),
                    cx,
                );
            }
            other => {
                let view = f.view().clone();
                f.show_file_mode(cx);
                view.update(cx, |v, cx| {
                    let read = |path: &str| project.read(path).unwrap();
                    match other {
                        "code" => v.show_read(read("src/main.rs"), Some(5), window, cx),
                        "markdown" => v.show_read(read("README.md"), None, window, cx),
                        "image" => {
                            v.set_inline_decode(true);
                            v.show_read(read("assets/logo.png"), None, window, cx)
                        }
                        "svg" => v.show_read(read("assets/logo.svg"), None, window, cx),
                        "binary" => v.show_read(read("assets/data.bin"), None, window, cx),
                        _ => v.show_refused(".env", Refused::Sensitive, cx),
                    }
                });
            }
        });
        panel
    })
}

fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let panel = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("panel-files-story-{name}").into()),
        cx,
        |window, cx| make(name, window, cx),
    );
    div()
        .id("panel-frame")
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .child(
            Panel::pane("right-panel", Edge::Left)
                .size_full()
                .child(panel.read(cx).clone()),
        )
        .probe()
        .into_any_element()
}

/// The Files tab in seven states.
pub static PANEL_FILES: Story = Story {
    name: "panel-files",
    title: "Right panel: files",
    size: (WIDTH + 24., HEIGHT + 24.),
    themes: Themes::T3,
    states: &STATES,
    render: cell,
};

// --- the finder -------------------------------------------------------------------------------------

const FINDER_STATES: [State; 3] = [at_rest("results"), at_rest("none"), at_rest("indexing")];

fn finder_cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let finder = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("finder-story-{name}").into()),
        cx,
        |window, cx| {
            let app = app(cx);
            let paths: Vec<String> = {
                let mut all = Vec::new();
                sample_project().index(&mut |b| {
                    all.extend(b);
                    true
                });
                all
            };
            cx.new(|cx| match name {
                "results" => FileFinder::with_files(
                    app,
                    paths,
                    Indexing::Done { truncated: false },
                    "ma",
                    window,
                    cx,
                ),
                "none" => FileFinder::with_files(
                    app,
                    paths,
                    Indexing::Done { truncated: false },
                    "zzzz",
                    window,
                    cx,
                ),
                _ => FileFinder::with_files(app, paths, Indexing::Running, "", window, cx),
            })
        },
    );
    div()
        .id("finder-frame")
        .child(finder.read(cx).clone())
        .probe()
        .into_any_element()
}

/// The file finder in three states.
pub static FILE_FINDER: Story = Story {
    name: "file-finder",
    title: "File finder",
    size: (crate::panel::finder::WIDTH + 24., 450.),
    themes: Themes::T3,
    states: &FINDER_STATES,
    render: finder_cell,
};
