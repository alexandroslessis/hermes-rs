//! The right panel's diff story (CP6.1): the panel as the app draws it (a 380 x 620 pane: the tab
//! strip, the toolbar, the changed files, the diff) over a fake repository
//! ([`crate::panel::fixture`]) in six states. `working`: the working tree as a flat list with the
//! first files open; `tree`: the same as a folder tree; `split`: side by side; `confirm`: a file's
//! revert waiting for its confirmation; `empty`: a turn that changed nothing, with the note;
//! `files`: the Files tab's placeholder after a file link to a file that is not changed.
//! References: `sidebar-diff.png`, `sidebar-diff-content.png`, `sidebar-right.png`.
//!
//! The panel is given its data directly ([`crate::panel::diff::DiffPanel::show_data`]): a story is
//! drawn twice by the harness and must not wait for a background read. The panels live in the story
//! window's element state, so what Kit prepares in the background (syntax colours, word-level
//! emphasis) is there for the second frame.

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
        RightPanel,
        data::{DiffData, Revert, Scope},
        fixture::FixtureDiffs,
    },
    thread::OpenFileInPanel,
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const STATES: [State; 6] = [
    at_rest("working"),
    at_rest("tree"),
    at_rest("split"),
    at_rest("confirm"),
    at_rest("empty"),
    at_rest("files"),
];

/// The width of the pane in the app at its default size, and a height that shows a diff.
pub const WIDTH: f32 = 380.;
pub const HEIGHT: f32 = 620.;

fn data(patch: String, revert: Revert) -> DiffData {
    DiffData {
        patch,
        revert,
        ..DiffData::default()
    }
}

/// The right panel of one state, over the fixture thread `t1`.
fn make(name: &str, window: &mut Window, cx: &mut App) -> Entity<RightPanel> {
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let app = cx.new(|cx| {
        AppModel::with_snapshot(Arc::clone(&backend) as _, snapshot, cx).with_clock(|| NOW_MS)
    });
    app.update(cx, |app, cx| app.open("t1", cx));
    let thread = app.read(cx).thread().cloned();
    let name = name.to_string();
    cx.new(|cx| {
        let mut panel = RightPanel::new(app, window, cx);
        panel.set_thread(thread, cx);
        let sample = FixtureDiffs::sample();
        let diff = panel.diff().clone();
        diff.update(cx, |d, cx| match name.as_str() {
            "empty" => {
                d.set_scope_quietly(Scope::Turn);
                d.show_data(
                    Scope::Turn,
                    DiffData {
                        note: Some("No edits of the latest turn are known.".to_string()),
                        ..DiffData::default()
                    },
                    cx,
                );
            }
            _ => {
                d.show_data(
                    Scope::WorkingTree,
                    data(sample.working.clone(), Revert::ToHead),
                    cx,
                );
                match name.as_str() {
                    "tree" => d.set_tree(true, cx),
                    "split" => d.set_split(true, cx),
                    "confirm" => d.ask_revert("src/main.rs", cx),
                    _ => {}
                }
            }
        });
        if name == "files" {
            panel.open_file(
                &OpenFileInPanel {
                    path: "crates/native/src/workspace.rs".to_string(),
                    line: Some(89),
                },
                cx,
            );
        }
        panel
    })
}

fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let panel = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("panel-diff-story-{name}").into()),
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

/// The right panel's diff in six states.
pub static PANEL_DIFF: Story = Story {
    name: "panel-diff",
    title: "Right panel: diff",
    size: (WIDTH + 24., HEIGHT + 24.),
    themes: Themes::T3,
    states: &STATES,
    render: cell,
};
