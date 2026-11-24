//! The thread header's story (CP4.3): the title bar with the open thread's header in the states
//! that differ, each bar 700 px wide with the panel toggles and the caption buttons as in the
//! app. `default`: a clean repository, a dirty one ahead and behind, no repository. `edge`: a
//! title, project and branch far too long, a detached head with a missing folder, the title being
//! renamed, and the narrow bar where the chips have left. t3code's two themes; references:
//! `app-thread-dark.png` (the breadcrumb), `button-sidebar_toggle-hover.png` (the toggles).
//!
//! The headers live in the story window's element state (`use_keyed_state`), so their background
//! reads survive the frames the harness draws.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Window, assets::IconName, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    design::button::{self, Variant, icon_button},
    dev::ProbeExt as _,
    model::{
        AppModel, FixtureBackend,
        fixture::{self, NOW_MS},
    },
    thread::{
        ThreadHeader,
        header_data::{GitState, GitSummary, HeaderInfo, MODEL_ROOM},
    },
    titlebar::TitleBar,
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const STATES: [State; 2] = [at_rest("default"), at_rest("edge")];

/// One bar of a story.
struct Bar {
    title: &'static str,
    project: &'static str,
    info: HeaderInfo,
    /// The width the header may use (the frame decides this in the app).
    room: f32,
    renaming: bool,
}

fn repo(branch: &str, ahead: i64, behind: i64, files: usize, added: u64, removed: u64) -> GitState {
    GitState::Repo(GitSummary {
        branch: Some(branch.to_string()),
        ahead,
        behind,
        changed: files,
        unstaged: files,
        added,
        removed,
        ..GitSummary::default()
    })
}

fn info(model: &str, git: GitState) -> HeaderInfo {
    HeaderInfo {
        cwd: r"C:\work\structure".to_string(),
        model: model.to_string(),
        provider: "openrouter".to_string(),
        archived: false,
        git,
    }
}

fn bars(state: &str) -> Vec<Bar> {
    match state {
        "default" => vec![
            Bar {
                title: "halo",
                project: "structure",
                info: info("openrouter/free", repo("master", 0, 0, 0, 0, 0)),
                room: MODEL_ROOM,
                renaming: false,
            },
            Bar {
                title: "Fix the sidebar width",
                project: "hermes-rs",
                info: info(
                    "qwen/qwen3-coder",
                    repo("native/p4-thread", 2, 1, 4, 6770, 3),
                ),
                room: MODEL_ROOM,
                renaming: false,
            },
            Bar {
                title: "Quick question about regex",
                project: "No project",
                info: info("openrouter/free", GitState::NoRepo),
                room: MODEL_ROOM,
                renaming: false,
            },
        ],
        _ => vec![
            Bar {
                title: "A very long thread title that goes on and on and on and then some more",
                project: "a-project-with-a-name-that-is-just-as-long-as-it-can-be",
                info: info(
                    "provider/a-huge-model-name-with-many-parts",
                    repo(
                        "feature/a-very-long-branch-name-for-the-header",
                        12,
                        345,
                        7,
                        123_456,
                        7_890,
                    ),
                ),
                room: MODEL_ROOM,
                renaming: false,
            },
            Bar {
                title: "Detached and missing",
                project: "engine",
                info: info(
                    "openrouter/free",
                    GitState::Repo(GitSummary {
                        detached: true,
                        ..GitSummary::default()
                    }),
                ),
                room: MODEL_ROOM,
                renaming: false,
            },
            Bar {
                title: "Folder missing",
                project: "docs",
                info: info("openrouter/free", GitState::NoFolder),
                room: MODEL_ROOM,
                renaming: true,
            },
            Bar {
                title: "A narrow bar: the chips leave",
                project: "structure",
                info: info("openrouter/free", repo("master", 1, 0, 2, 12, 4)),
                room: 200.,
                renaming: false,
            },
        ],
    }
}

/// The app model, thread model and header of one bar, over the fixture thread `t1` made to look
/// like the bar's description.
fn make(bar: &Bar, window: &mut Window, cx: &mut App) -> Entity<ThreadHeader> {
    let mut snapshot = fixture::snapshot();
    snapshot.threads[0].title = bar.title.to_string();
    snapshot.projects[0].name = bar.project.to_string();
    let backend = FixtureBackend::new(snapshot.clone());
    backend.set_header("t1", bar.info.clone());
    let app = cx.new(|cx| {
        AppModel::with_snapshot(Arc::clone(&backend) as _, snapshot, cx).with_clock(|| NOW_MS)
    });
    app.update(cx, |app, cx| app.open("t1", cx));
    let thread = app
        .read(cx)
        .thread()
        .cloned()
        .expect("the open thread's model");
    let renaming = bar.renaming;
    cx.new(|cx| {
        let mut header = ThreadHeader::new(app, thread, window, cx);
        if renaming {
            header.start_rename(window, cx);
        }
        header
    })
}

fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let headers = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("thread-header-story-{name}").into()),
        cx,
        |window, cx| {
            bars(name)
                .iter()
                .map(|bar| make(bar, window, cx))
                .collect::<Vec<_>>()
        },
    );
    let list = bars(name);
    let rows: Vec<AnyElement> = headers
        .read(cx)
        .clone()
        .into_iter()
        .zip(list)
        .enumerate()
        .map(|(ix, (header, bar))| {
            header.update(cx, |header, _| header.set_room(bar.room));
            let toggles = div()
                .flex()
                .items_center()
                .gap(px(4.))
                .pr(px(8.))
                .child(
                    icon_button("drawer", IconName::PanelBottom, "Toggle terminal drawer")
                        .variant(Variant::Ghost)
                        .size(button::Size::Sm),
                )
                .child(
                    icon_button("right", IconName::PanelRight, "Toggle right panel")
                        .variant(Variant::Ghost)
                        .size(button::Size::Sm),
                );
            div()
                .id(gpui_kit::ElementId::Name(format!("bar-{ix}").into()))
                .w(px(860.))
                .child(TitleBar::new().header(header).right(toggles))
                .into_any_element()
        })
        .collect();
    div()
        .id("header-frame")
        .flex()
        .flex_col()
        .gap(px(6.))
        .children(rows)
        .probe()
        .into_any_element()
}

/// The thread header in the title bar: clean, dirty and no repository; then the long names, a
/// detached head, a missing folder, the title field and the narrow bar.
pub static THREAD_HEADER: Story = Story {
    name: "thread-header",
    title: "Thread header",
    size: (900., 280.),
    themes: Themes::T3,
    states: &STATES,
    render: cell,
};
