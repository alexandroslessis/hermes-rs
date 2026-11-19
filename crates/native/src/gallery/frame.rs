//! Stories of the app frame: the title bar, the command palette and the whole frame with fake
//! data. Same rules as `design.rs`: t3code's themes, the reference named in each module's header.

use gpui_kit::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window, assets::IconName, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    design::button::{self, Variant, icon_button},
    dev::ProbeExt as _,
    palette::{self, Mode, Palette},
    titlebar::TitleBar,
    ui_state::UiState,
    workspace::Workspace,
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const TITLE_STATES: [State; 5] = [
    at_rest("normal"),
    at_rest("maximised"),
    State {
        name: "hover-minimise",
        interaction: Interaction::Hover("minimize"),
    },
    State {
        name: "hover-close",
        interaction: Interaction::Hover("close"),
    },
    State {
        name: "pressed-close",
        interaction: Interaction::Press("close"),
    },
];

/// The title bar: its panel toggles and the three caption buttons (`app-empty-dark.png`, top
/// right; `button-sidebar_toggle-hover.png`).
pub static TITLE_BAR: Story = Story {
    name: "title-bar",
    title: "Title bar",
    size: (404., 64.),
    themes: Themes::T3,
    states: &TITLE_STATES,
    render: |state, _, _| {
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
            .w(px(380.))
            .child(
                TitleBar::new()
                    .title("structure  /  halo")
                    .right(toggles)
                    .maximized(state.name == "maximised"),
            )
            .into_any_element()
    },
};

const PALETTE_STATES: [State; 3] = [at_rest("empty"), at_rest("filtered"), at_rest("no-match")];

/// The command palette over the fake data.
pub static PALETTE: Story = Story {
    name: "palette",
    title: "Command palette",
    size: (610., 470.),
    themes: Themes::T3,
    states: &PALETTE_STATES,
    render: |state, window, cx| palette_cell(state, window, cx),
};

/// The palette in one state: an empty query, a query with matches, one without.
fn palette_cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let query = match state.name {
        "filtered" => "tog",
        "no-match" => "zzzz",
        _ => "",
    };
    let entity = cx.new(|cx| {
        // A representative few of each kind: the whole list is taller than the story.
        let mut items = Vec::new();
        for (kind, count) in [
            (palette::Kind::Action, 5),
            (palette::Kind::Thread, 2),
            (palette::Kind::Project, 1),
        ] {
            items.extend(
                Workspace::palette_items()
                    .into_iter()
                    .filter(|i| i.kind == kind)
                    .take(count),
            );
        }
        let mut p = Palette::new(Mode::All, items, window, cx);
        if !query.is_empty() {
            p.set_query(query, window, cx);
        }
        p
    });
    div()
        .id("palette-frame")
        .child(entity)
        .probe()
        .into_any_element()
}

const FRAME_STATES: [State; 2] = [at_rest("default"), at_rest("panels-open")];

/// The whole frame, fake data, closed and with the right panel and drawer open.
pub static APP_FRAME: Story = Story {
    name: "app-frame",
    title: "App frame",
    size: (1024., 624.),
    themes: Themes::T3,
    states: &FRAME_STATES,
    render: |state, window, cx| {
        let mut ui = UiState::default();
        if state.name == "panels-open" {
            ui.right_panel.open = true;
            ui.drawer.open = true;
        }
        let frame = cx.new(|cx| Workspace::new(ui, None, window, cx));
        div()
            .id("frame")
            .w(px(1000.))
            .h(px(600.))
            .child(frame)
            .probe()
            .into_any_element()
    },
};
