//! Stories of the sidebar (CP3.1): the whole column over fixture data in each state a row or a
//! folder can be in, and the two empty states of the main area. Same rules as `design.rs`: t3code's
//! two themes, the reference named in the module header of `sidebar/render.rs`
//! (`app-sidebar_closeup-dark.png`, `sidebar.png`, `button-session-variants-*.png`,
//! `buttons-sidebar-down.png`; the grouped rows have no screenshot of their own).

use gpui_kit::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    dev::ProbeExt as _,
    model::{
        AppModel, FixtureBackend,
        fixture::{self, NOW_MS},
    },
    sidebar::{Sidebar, data::Prefs, empty},
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const ROW_STATES: [State; 5] = [
    at_rest("default"),
    State {
        name: "hover",
        interaction: Interaction::Hover("thread-t3"),
    },
    at_rest("open"),
    at_rest("statuses"),
    at_rest("pinned"),
];

const MORE_STATES: [State; 4] = [
    at_rest("closed-folders"),
    at_rest("filtered"),
    at_rest("renaming"),
    at_rest("confirm-delete"),
];

/// One sidebar, 256 px wide (t3code's default), in a state.
fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let mut snapshot = fixture::snapshot();
    let mut prefs = Prefs::default();
    let name = state.name;
    if name == "statuses" {
        snapshot.threads[0].working = true; // t1
        snapshot.threads[1].waiting = true; // t2
    }
    if name == "pinned" {
        snapshot.threads[2].pinned = true;
        snapshot.threads[4].pinned = true;
    }
    if name == "closed-folders" {
        snapshot.threads[3].waiting = true; // t4, in hermes-rs: its closed folder shows the dot
        prefs.collapsed.insert("prj_hermes".into());
        prefs.collapsed.insert("prj_engine".into());
    }
    let backend = FixtureBackend::new(snapshot.clone());
    let open = matches!(name, "open" | "statuses" | "renaming");
    let statuses = name == "statuses";
    let model = cx.new(|cx| {
        let mut model = AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS);
        if open {
            model = model.with_open("t4");
        }
        if statuses {
            model = model.with_failed(&["t3"]).with_unread(&["t5"]);
        }
        model
    });
    let sidebar = cx.new(|cx| {
        let sidebar = Sidebar::new(model, prefs, cx);
        match name {
            "filtered" => sidebar.with_filter("pal", window, cx),
            "renaming" => sidebar.with_rename_of("t3", window, cx),
            "confirm-delete" => sidebar.with_delete_question("t5", cx),
            _ => sidebar,
        }
    });
    div()
        .id("sidebar-frame")
        .w(px(256.))
        .h(px(600.))
        .child(sidebar)
        .probe()
        .into_any_element()
}

/// The whole sidebar column: a thread at rest, under the pointer (its actions show), the open
/// one, every status, pinned threads.
pub static SIDEBAR: Story = Story {
    name: "sidebar",
    title: "Sidebar",
    size: (1400., 640.),
    themes: Themes::T3,
    states: &ROW_STATES,
    render: cell,
};

/// The sidebar with closed folders (and their dots), filtered, a rename in place, the delete
/// question.
pub static SIDEBAR_MORE: Story = Story {
    name: "sidebar-more",
    title: "Sidebar, more states",
    size: (1130., 640.),
    themes: Themes::T3,
    states: &MORE_STATES,
    render: cell,
};

const EMPTY: [State; 3] = [
    at_rest("no-projects"),
    at_rest("no-active-thread"),
    at_rest("sidebar-empty"),
];

/// `NoProjectsHero`, `NoActiveThreadState`, and the sidebar with nothing in it.
pub static EMPTY_STATES: Story = Story {
    name: "empty-states",
    title: "Empty states",
    size: (1700., 420.),
    themes: Themes::T3,
    states: &EMPTY,
    render: |state, _, cx| match state.name {
        "no-projects" => div()
            .id("empty-frame")
            .w(px(560.))
            .h(px(380.))
            .flex()
            .child(empty::no_projects_hero(|_, _| {}, |_, _| {}))
            .probe()
            .into_any_element(),
        "no-active-thread" => div()
            .id("empty-frame")
            .w(px(560.))
            .h(px(380.))
            .flex()
            .child(empty::no_active_thread())
            .probe()
            .into_any_element(),
        _ => {
            let backend = FixtureBackend::new(fixture::empty());
            let model = cx.new(|cx| {
                AppModel::with_snapshot(backend, fixture::empty(), cx).with_clock(|| NOW_MS)
            });
            let sidebar = cx.new(|cx| Sidebar::new(model, Prefs::default(), cx));
            div()
                .id("empty-frame")
                .w(px(256.))
                .h(px(380.))
                .child(sidebar)
                .probe()
                .into_any_element()
        }
    },
};

const WAITING: [State; 2] = [at_rest("approval-and-input"), at_rest("closed-folder")];

/// The sidebar while threads wait for the user (CP5.3): one for an approval ("Approval", the
/// warning shield), one for a question ("Input"), and a closed folder with the attention dot.
pub static SIDEBAR_WAITING: Story = Story {
    name: "sidebar-waiting",
    title: "Sidebar: threads that wait",
    size: (580., 640.),
    themes: Themes::T3,
    states: &WAITING,
    render: |state, _, cx| {
        let mut snapshot = fixture::snapshot();
        let mut prefs = Prefs::default();
        if state.name == "approval-and-input" {
            snapshot.threads[0].working = true; // t1 works
            snapshot.threads[1].waiting = true; // t2 waits for an approval
            snapshot.threads[2].waiting = true; // t3 waits for an answer
            snapshot.threads[2].asking = true;
        } else {
            snapshot.threads[3].waiting = true; // t4, in hermes-rs
            snapshot.threads[3].asking = true;
            prefs.collapsed.insert("prj_hermes".into());
            prefs.collapsed.insert("prj_engine".into());
        }
        let backend = FixtureBackend::new(snapshot.clone());
        let model =
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS));
        let sidebar = cx.new(|cx| Sidebar::new(model, prefs, cx));
        div()
            .id("sidebar-frame")
            .w(px(256.))
            .h(px(600.))
            .child(sidebar)
            .probe()
            .into_any_element()
    },
};
