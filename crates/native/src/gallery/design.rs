//! The stories of the design layer (`crate::design`): every component in every state, under
//! t3code's light and dark themes, with layout checks over each state. Each story's states and
//! size follow the t3code reference its component's module header names, so a snapshot and the
//! screenshot can be put side by side.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    assets::IconName, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::design::{
    Badge, Button, EmptyState, Kbd, KbdGroup, ListRow, MiddleTruncate, Panel, SectionHeader,
    ThreadRow, Toast, Tooltip, badge,
    button::{self, Size, Variant},
    list_row::Status,
    panel::Edge,
    toast::{Kind, ToastSpec},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const fn interact(name: &'static str, interaction: Interaction) -> State {
    State { name, interaction }
}

/// The five states of anything clickable, targeting the element called `button`.
const CLICKABLE: [State; 5] = [
    at_rest("normal"),
    interact("hover", Interaction::Hover("button")),
    interact("pressed", Interaction::Press("button")),
    interact("focused", Interaction::Focus("button")),
    at_rest("disabled"),
];

/// One variant of the labelled button, in the five states.
fn variant_button(variant: Variant, state: &State) -> AnyElement {
    Button::new("button", "Save changes")
        .variant(variant)
        .disabled(state.name == "disabled")
        .into_any_element()
}

macro_rules! variant_story {
    ($ident:ident, $name:literal, $title:literal, $variant:expr) => {
        /// A button variant in its five states.
        pub static $ident: Story = Story {
            name: $name,
            title: $title,
            size: (170., 60.),
            themes: Themes::T3,
            states: &CLICKABLE,
            render: |state, _, _| variant_button($variant, state),
        };
    };
}

variant_story!(
    BUTTON_DEFAULT,
    "button-default",
    "Button: default",
    Variant::Default
);
variant_story!(
    BUTTON_OUTLINE,
    "button-outline",
    "Button: outline",
    Variant::Outline
);
variant_story!(
    BUTTON_GHOST,
    "button-ghost",
    "Button: ghost",
    Variant::Ghost
);
variant_story!(
    BUTTON_GHOST_MUTED,
    "button-ghost-muted",
    "Button: ghost-muted",
    Variant::GhostMuted
);
variant_story!(
    BUTTON_DESTRUCTIVE,
    "button-destructive",
    "Button: destructive",
    Variant::Destructive
);

const SIZE_STATES: [State; 5] = [
    at_rest("micro"),
    at_rest("xs"),
    at_rest("sm"),
    at_rest("default"),
    at_rest("lg"),
];

/// The size named by a `SIZE_STATES` state.
pub(super) fn size_of(state: &State) -> Size {
    Size::ALL
        .into_iter()
        .find(|s| s.name() == state.name)
        .expect("a size state")
}

/// The labelled and the icon-only button at each size.
pub static BUTTON_SIZES: Story = Story {
    name: "button-sizes",
    title: "Button: sizes",
    size: (200., 60.),
    themes: Themes::T3,
    states: &SIZE_STATES,
    render: |state, _, _| {
        let size = size_of(state);
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                Button::new("button", "Button")
                    .variant(Variant::Outline)
                    .size(size),
            )
            .child(
                button::icon_button("icon", IconName::Settings, "Settings")
                    .variant(Variant::Outline)
                    .size(size),
            )
            .into_any_element()
    },
};

const ICON_STATES: [State; 5] = [
    at_rest("normal"),
    interact("hover", Interaction::Hover("button")),
    at_rest("selected"),
    interact("focused", Interaction::Focus("button")),
    at_rest("disabled"),
];

/// The ghost icon button of the sidebar and header toggles.
pub static ICON_BUTTON: Story = Story {
    name: "icon-button",
    title: "Icon button (ghost)",
    size: (80., 60.),
    themes: Themes::T3,
    states: &ICON_STATES,
    render: |state, _, _| {
        button::icon_button("button", IconName::PanelBottom, "Toggle terminal drawer")
            .variant(Variant::Ghost)
            .size(Size::Sm)
            .selected(state.name == "selected")
            .disabled(state.name == "disabled")
            .into_any_element()
    },
};

/// A sidebar-coloured box around `child`.
fn on_sidebar(width: f32, child: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .id("sidebar")
        .w(px(width))
        .p(px(8.))
        .bg(cx.design_theme().palette.sidebar.hsla())
        .child(child)
        .into_any_element()
}

const ROW_STATES: [State; 3] = [
    at_rest("normal"),
    interact("hover", Interaction::Hover("row")),
    at_rest("selected"),
];

/// A one-line sidebar row.
pub static LIST_ROW: Story = Story {
    name: "list-row",
    title: "List row",
    size: (270., 76.),
    themes: Themes::T3,
    states: &ROW_STATES,
    render: |state, _, cx| {
        on_sidebar(
            240.,
            ListRow::new("row", "New thread")
                .icon(IconName::Plus)
                .meta("Ctrl+N")
                .selected(state.name == "selected"),
            cx,
        )
    },
};

const THREAD_STATES: [State; 4] = [
    at_rest("normal"),
    interact("hover", Interaction::Hover("row")),
    at_rest("selected"),
    at_rest("done"),
];

/// The three-line thread entry (`button-session-variants-selected.png`).
pub static THREAD_ROW: Story = Story {
    name: "thread-row",
    title: "Thread row",
    size: (290., 116.),
    themes: Themes::T3,
    states: &THREAD_STATES,
    render: |state, _, cx| {
        let row = ThreadRow::new("row", "engine", "ayo")
            .branch("main")
            .time("5m")
            .selected(state.name == "selected");
        let row = if state.name == "done" {
            row.status(Status::Done)
        } else {
            row
        };
        on_sidebar(254., row, cx)
    },
};

/// The status a `STATUS_STATES` state shows.
fn status_of(name: &str) -> Status {
    match name {
        "working" => Status::Working,
        "waiting" => Status::Waiting,
        "failed" => Status::Failed,
        "unread" => Status::Unread,
        _ => Status::Done,
    }
}

const STATUS_STATES: [State; 5] = [
    at_rest("working"),
    at_rest("waiting"),
    at_rest("failed"),
    at_rest("unread"),
    at_rest("done"),
];

/// A thread row in each status.
pub static THREAD_STATUS: Story = Story {
    name: "thread-row-status",
    title: "Thread row: status",
    size: (290., 116.),
    themes: Themes::T3,
    states: &STATUS_STATES,
    render: |state, _, cx| {
        let status = status_of(state.name);
        on_sidebar(
            254.,
            ThreadRow::new("row", "hermes-rs", "Fix the sidebar")
                .branch("native/p2-design")
                .status(status),
            cx,
        )
    },
};

const HEADER_STATES: [State; 3] = [
    at_rest("open"),
    at_rest("closed"),
    interact("hover", Interaction::Hover("header")),
];

/// A group heading.
pub static SECTION_HEADER: Story = Story {
    name: "section-header",
    title: "Section header",
    size: (270., 76.),
    themes: Themes::T3,
    states: &HEADER_STATES,
    render: |state, _, cx| {
        let header = SectionHeader::new("header", "Settled")
            .meta("7")
            .collapsible(state.name != "closed");
        on_sidebar(240., header, cx)
    },
};

const PANEL_STATES: [State; 3] = [at_rest("card"), at_rest("pane-left"), at_rest("pane-top")];

/// The glass card and the two edged panes.
pub static PANEL: Story = Story {
    name: "panel",
    title: "Panel",
    size: (300., 200.),
    themes: Themes::T3,
    states: &PANEL_STATES,
    render: |state, _, _| {
        let body = div()
            .id("body")
            .p(px(12.))
            .child("Open in Zed\nAdd project script".replace('\n', " · "));
        let panel = match state.name {
            "card" => Panel::card("panel").title("Details"),
            "pane-left" => Panel::pane("panel", Edge::Left).title("Changes"),
            _ => Panel::pane("panel", Edge::Top).title("Terminal"),
        };
        div()
            .w(px(260.))
            .h(px(140.))
            .flex()
            .child(div().w(px(260.)).child(panel.child(body)))
            .into_any_element()
    },
};

const BADGE_STATES: [State; 7] = [
    at_rest("default"),
    at_rest("secondary"),
    at_rest("outline"),
    at_rest("info"),
    at_rest("success"),
    at_rest("warning"),
    at_rest("error"),
];

/// Each badge variant at the three sizes.
pub static BADGE: Story = Story {
    name: "badge",
    title: "Badge",
    size: (110., 120.),
    themes: Themes::T3,
    states: &BADGE_STATES,
    render: |state, _, _| {
        let variant = badge::Variant::ALL
            .into_iter()
            .find(|v| v.name() == state.name)
            .expect("a badge variant");
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(8.))
            .child(
                Badge::new("sm", state.name)
                    .variant(variant)
                    .size(badge::Size::Sm),
            )
            .child(Badge::new("md", state.name).variant(variant))
            .child(
                Badge::new("lg", state.name)
                    .variant(variant)
                    .size(badge::Size::Lg),
            )
            .into_any_element()
    },
};

const KBD_STATES: [State; 3] = [at_rest("key"), at_rest("chord"), at_rest("outline")];

/// A key and a chord.
pub static KBD: Story = Story {
    name: "kbd",
    title: "Kbd",
    size: (180., 50.),
    themes: Themes::T3,
    states: &KBD_STATES,
    render: |state, _, _| match state.name {
        "key" => Kbd::new("K").into_any_element(),
        "chord" => KbdGroup::new("Ctrl+Shift+P").into_any_element(),
        _ => KbdGroup::new("Ctrl+K").outline().into_any_element(),
    },
};

const TOOLTIP_STATES: [State; 2] = [at_rest("text"), at_rest("shortcut")];

/// The tooltip bubble.
pub static TOOLTIP: Story = Story {
    name: "tooltip",
    title: "Tooltip",
    size: (300., 50.),
    themes: Themes::T3,
    states: &TOOLTIP_STATES,
    render: |state, _, _| match state.name {
        "text" => Tooltip::new("Toggle sidebar").into_any_element(),
        _ => Tooltip::new("Toggle terminal drawer")
            .shortcut("Ctrl+J")
            .into_any_element(),
    },
};

const TOAST_STATES: [State; 6] = [
    at_rest("info"),
    at_rest("success"),
    at_rest("warning"),
    at_rest("error"),
    at_rest("loading"),
    at_rest("action"),
];

/// The spec each toast state shows.
pub(super) fn toast_spec(state: &State) -> ToastSpec {
    match state.name {
        "info" => ToastSpec::new(Kind::Info, "Copied the thread").description("As Markdown."),
        "success" => ToastSpec::new(Kind::Success, "Review finished")
            .description("2 memory notes and 1 skill were saved."),
        "warning" => ToastSpec::new(Kind::Warning, "The endpoint is slow")
            .description("The last request took 41 s."),
        "error" => ToastSpec::new(Kind::Error, "The turn failed")
            .description("Connection refused (127.0.0.1:11434)."),
        "loading" => ToastSpec::new(Kind::Loading, "Importing from Hermes"),
        _ => ToastSpec::new(Kind::Info, "A queued prompt could not start")
            .description("Another turn is running.")
            .action("Open thread"),
    }
}

/// A toast of each kind.
pub static TOAST: Story = Story {
    name: "toast",
    title: "Toast",
    size: (400., 130.),
    themes: Themes::T3,
    states: &TOAST_STATES,
    render: |state, _, _| Toast::new("toast", &toast_spec(state)).into_any_element(),
};

const EMPTY_STATES: [State; 3] = [at_rest("compact"), at_rest("default"), at_rest("hero")];

/// The empty-state screen at its three sizes.
pub static EMPTY_STATE: Story = Story {
    name: "empty-state",
    title: "Empty state",
    size: (460., 300.),
    themes: Themes::T3,
    states: &EMPTY_STATES,
    render: |state, _, _| {
        let size = match state.name {
            "compact" => crate::design::empty_state::Size::Compact,
            "default" => crate::design::empty_state::Size::Default,
            _ => crate::design::empty_state::Size::Hero,
        };
        div()
            .id("frame")
            .w(px(420.))
            .h(px(260.))
            .flex()
            .probe()
            .child(
                EmptyState::new("empty", "No thread open")
                    .icon(IconName::MessageSquare)
                    .description("Pick a thread in the sidebar or start a new one.")
                    .size(size)
                    .child(Button::new("new", "New thread"))
                    .child(Button::new("add", "Add project").variant(Variant::Outline)),
            )
            .into_any_element()
    },
};

const TRUNCATE_STATES: [State; 2] = [at_rest("fits"), at_rest("cut")];

/// The path that fits and the one that is cut in the middle.
pub static MIDDLE_TRUNCATE: Story = Story {
    name: "middle-truncate",
    title: "Middle truncate",
    size: (260., 50.),
    themes: Themes::T3,
    states: &TRUNCATE_STATES,
    render: |state, _, _| {
        let value = if state.name == "fits" {
            "src/lib.rs"
        } else {
            "crates/native/src/design/middle_truncate.rs"
        };
        div()
            .id("box")
            .w(px(200.))
            .text_size(px(13.))
            .child(MiddleTruncate::new("path", value))
            .probe()
            .into_any_element()
    },
};

#[cfg(test)]
mod tests {
    use gpui_kit::component::ActiveTheme as _;

    use super::*;
    use crate::design::{
        Palette, badge::colors as badge_colors, button::variant_colors, list_row::row_fill,
        tokens::Color,
    };
    use crate::gallery::open_story;
    use crate::testing::{Checks, Layout, headless_app, layout};
    use crate::theme::ThemeChoice;

    const T3: [ThemeChoice; 2] = [ThemeChoice::T3Light, ThemeChoice::T3Dark];

    fn palette_of(theme: ThemeChoice) -> Palette {
        if theme.is_dark() {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    /// Runs `check` over every state of `story` under both t3code themes.
    fn each_state(
        story: &'static Story,
        mut check: impl FnMut(&State, &Layout, &mut Checks, ThemeChoice, &Palette),
    ) {
        let mut cx = headless_app();
        for theme in T3 {
            for (ix, state) in story.states.iter().enumerate() {
                let window = open_story(&mut cx, story, ix, theme);
                let layout = layout(&mut cx, window);
                let mut checks = layout.check();
                checks.no_overflow("story");
                check(state, &layout, &mut checks, theme, &palette_of(theme));
                if !checks.failures().is_empty() {
                    eprintln!("{} / {} / {}:", story.name, theme.label(), state.name);
                }
                checks.assert();
            }
        }
    }

    fn color(c: Color) -> gpui_kit::Hsla {
        c.hsla()
    }

    #[core::prelude::v1::test]
    fn every_design_story_is_registered_and_fits_its_window() {
        let names: Vec<_> = crate::gallery::all().iter().map(|s| s.name).collect();
        let own: Vec<_> = crate::gallery::all()
            .iter()
            .filter(|s| s.themes == Themes::T3)
            .collect();
        assert!(
            own.len() >= 18,
            "the design stories are registered: {}",
            own.len()
        );
        for story in own {
            assert!(
                names.contains(&story.name),
                "{} is not registered",
                story.name
            );
            // Names are the snapshot file stems: kebab-case, unique.
            assert_eq!(names.iter().filter(|n| **n == story.name).count(), 1);
            assert!(
                story
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-')
            );
            each_state(story, |_, _, _, _, _| {});
        }
    }

    #[core::prelude::v1::test]
    fn buttons_in_all_variants_and_states_paint_their_tokens() {
        let variants = [
            (&BUTTON_DEFAULT, Variant::Default),
            (&BUTTON_OUTLINE, Variant::Outline),
            (&BUTTON_GHOST, Variant::Ghost),
            (&BUTTON_GHOST_MUTED, Variant::GhostMuted),
            (&BUTTON_DESTRUCTIVE, Variant::Destructive),
        ];
        for (story, variant) in variants {
            let mut heights = Vec::new();
            each_state(story, |state, layout, check, theme, palette| {
                let colors = variant_colors(variant, palette, theme.is_dark());
                check
                    .inside("button/label", "button")
                    .no_overflow("button")
                    .width_within("button", 60., 170.);
                match state.name {
                    "normal" => {
                        check
                            .background("button", color(colors.background))
                            .text_color("label", color(colors.foreground))
                            .state("button", Some(false), Some(false), Some(false));
                    }
                    "hover" => {
                        check
                            .background("button", color(colors.hover_background))
                            .text_color("label", color(colors.hover_foreground))
                            .state("button", Some(true), Some(false), Some(false));
                    }
                    "pressed" => {
                        check
                            .background("button", color(colors.hover_background))
                            .state("button", Some(true), None, Some(false));
                    }
                    "focused" => {
                        check.state("button", Some(false), Some(true), Some(false));
                    }
                    "disabled" => {
                        check.state("button", None, None, Some(true));
                    }
                    other => panic!("unexpected state {other}"),
                }
                heights.push(layout.bounds("button").size.height.as_f32());
            });
            // t3code's default size is 32 px high in every variant and every state.
            assert!(
                heights.iter().all(|h| *h == 32.),
                "{}: {heights:?}",
                story.name
            );
        }
    }

    #[core::prelude::v1::test]
    fn button_sizes_are_t3codes_scale_and_icon_buttons_are_square() {
        each_state(&BUTTON_SIZES, |state, layout, check, _, _| {
            let size = size_of(state);
            check
                .same_row("button", "icon")
                .inside("button/label", "button")
                .no_overflow("button")
                .no_overflow("icon");
            let (button, icon) = (layout.bounds("button"), layout.bounds("icon"));
            assert_eq!(button.size.height, size.height(), "{} height", state.name);
            assert_eq!(icon.size.height, size.height());
            assert_eq!(
                icon.size.width,
                size.height(),
                "{} icon button is square",
                state.name
            );
        });
    }

    #[core::prelude::v1::test]
    fn the_ghost_icon_button_hovers_and_selects_with_the_accent() {
        each_state(&ICON_BUTTON, |state, layout, check, _, palette| {
            check.no_overflow("button");
            let bounds = layout.bounds("button");
            assert_eq!(
                (bounds.size.width.as_f32(), bounds.size.height.as_f32()),
                (28., 28.)
            );
            match state.name {
                "normal" | "focused" => {
                    check.background("button", color(Color::rgba(0)));
                }
                "hover" | "selected" => {
                    check.background("button", color(palette.accent));
                }
                "disabled" => {
                    check.state("button", None, None, Some(true));
                }
                other => panic!("unexpected state {other}"),
            }
        });
    }

    #[core::prelude::v1::test]
    fn a_list_row_is_one_28px_line_with_the_sidebars_tints() {
        each_state(&LIST_ROW, |state, layout, check, _, palette| {
            check
                .inside("label", "row")
                .inside("meta", "row")
                .same_row("label", "meta")
                .same_height(&["row"]);
            assert_eq!(layout.bounds("row").size.height.as_f32(), 28.);
            let want = row_fill(palette, state.name == "selected", state.name == "hover");
            match want {
                Some(fill) => {
                    check.background("row", color(fill));
                }
                None => {
                    check.background("row", color(Color::rgba(0)));
                }
            }
            // The meta sits at the right edge, the label takes the rest.
            let (row, meta) = (layout.bounds("row"), layout.bounds("meta"));
            assert!(
                (row.right() - meta.right()).as_f32() <= 8.5,
                "meta is at the right"
            );
        });
    }

    #[core::prelude::v1::test]
    fn a_thread_row_stacks_project_title_and_branch() {
        for story in [&THREAD_ROW, &THREAD_STATUS] {
            each_state(story, |state, layout, check, _, palette| {
                check
                    .inside("row/head", "row")
                    .inside("title", "row")
                    .inside("row/meta", "row")
                    .no_overflow("row")
                    .same_row("project", "monogram");
                let head = layout.bounds("row/head");
                let title = layout.bounds("title");
                let meta = layout.bounds("row/meta");
                assert!(
                    head.bottom() <= title.top() + gpui_kit::px(0.5),
                    "project above title"
                );
                assert!(
                    title.bottom() <= meta.top() + gpui_kit::px(0.5),
                    "title above branch"
                );
                assert_eq!(title.size.height.as_f32(), 20.);
                if story.name == "thread-row" {
                    match state.name {
                        "selected" => {
                            check.background("row", color(palette.sidebar_row_active));
                        }
                        "hover" => {
                            check.background("row", color(palette.sidebar_row_hover));
                        }
                        _ => {}
                    }
                }
                if state.name == "done" || story.name == "thread-row-status" {
                    // The status is t3code's icon and a word on one line, in the status colour.
                    check
                        .same_row("status-icon", "word")
                        .inside("status", "row/head");
                    let status = status_of(state.name);
                    check.text_color("word", color(status.color(palette)));
                } else {
                    check.same_row("project", "time");
                }
            });
        }
    }

    #[core::prelude::v1::test]
    fn a_section_header_is_28px_with_its_count_on_the_right() {
        each_state(&SECTION_HEADER, |state, layout, check, _, palette| {
            check
                .inside("title", "header")
                .inside("meta", "header")
                .same_row("title", "meta");
            assert_eq!(layout.bounds("header").size.height.as_f32(), 28.);
            // The chevron comes last, right of the count (t3code's "Settled 7 >").
            check
                .same_row("meta", "chevron")
                .inside("chevron", "header");
            assert!(layout.bounds("chevron").left() >= layout.bounds("meta").right());
            if state.name == "hover" {
                check.background("header", color(palette.sidebar_row_hover));
            }
        });
    }

    #[core::prelude::v1::test]
    fn panels_have_a_header_row_and_the_glass_surface() {
        each_state(&PANEL, |state, layout, check, _, palette| {
            check
                .inside("header", "panel")
                .inside("panel/header/title", "panel/header");
            assert_eq!(layout.bounds("panel/header").size.height.as_f32(), 40.);
            check.background(
                "panel",
                color(palette.panel_surface.over(palette.background)),
            );
            let _ = state;
        });
    }

    #[core::prelude::v1::test]
    fn badges_are_16_18_and_22_px_high() {
        each_state(&BADGE, |state, layout, check, theme, palette| {
            check.same_row("sm", "sm");
            let h = |name: &str| layout.bounds(name).size.height.as_f32();
            assert_eq!(
                (h("sm"), h("md"), h("lg")),
                (16., 18., 22.),
                "{}",
                state.name
            );
            let variant = badge::Variant::ALL
                .into_iter()
                .find(|v| v.name() == state.name)
                .unwrap();
            let (fill, ink, _) = badge_colors(variant, palette, theme.is_dark());
            check
                .background("md", color(fill))
                .text_color("md/label", color(ink))
                .inside("md/label", "md");
        });
    }

    #[core::prelude::v1::test]
    fn kbd_keys_are_20px_high() {
        each_state(&KBD, |state, layout, check, _, _| {
            let name = if state.name == "key" {
                "kbd"
            } else {
                "kbd-group"
            };
            assert_eq!(layout.bounds(name).size.height.as_f32(), 20.);
            check.inside(name, "story");
        });
    }

    #[core::prelude::v1::test]
    fn a_tooltip_wraps_its_text_with_8_and_4_px_padding() {
        each_state(&TOOLTIP, |_, layout, check, _, _| {
            check.inside("text", "tooltip");
            let (tooltip, text) = (layout.bounds("tooltip"), layout.bounds("text"));
            assert_eq!((text.left() - tooltip.left()).as_f32(), 9.); // 8 padding + 1 border
            assert_eq!((text.top() - tooltip.top()).as_f32(), 5.);
        });
    }

    #[core::prelude::v1::test]
    fn a_toast_is_360px_with_its_icon_on_the_title_line() {
        each_state(&TOAST, |state, layout, check, _, _| {
            check
                .inside("icon", "toast")
                .inside("title", "toast")
                .inside("dismiss", "toast")
                .same_row("icon", "title");
            assert_eq!(layout.bounds("toast").size.width.as_f32(), 360.);
            // The dismiss button is at the right, the body takes the rest.
            let (toast, dismiss) = (layout.bounds("toast"), layout.bounds("dismiss"));
            assert!((toast.right() - dismiss.right()).as_f32() <= 13.);
            if state.name == "action" {
                check.inside("action", "toast");
                assert!(layout.bounds("action").top() > layout.bounds("title").bottom());
            }
        });
    }

    #[core::prelude::v1::test]
    fn an_empty_state_centres_its_media_title_and_actions() {
        each_state(&EMPTY_STATE, |_, layout, check, _, _| {
            check
                .inside("media", "empty")
                .inside("title", "empty")
                .inside("description", "empty")
                .inside("actions", "empty");
            let order = ["media", "title", "description", "actions"].map(|n| layout.bounds(n));
            for pair in order.windows(2) {
                assert!(pair[0].bottom() <= pair[1].top() + gpui_kit::px(0.5));
            }
            let frame = layout.bounds("frame");
            for name in ["media", "title", "actions"] {
                let b = layout.bounds(name);
                assert!(
                    (b.center().x - frame.center().x).as_f32().abs() <= 1.,
                    "{name} is centred"
                );
            }
        });
    }

    #[core::prelude::v1::test]
    fn a_long_path_keeps_its_tail_and_a_short_one_is_whole() {
        each_state(&MIDDLE_TRUNCATE, |state, layout, check, _, _| {
            check.inside("path", "box").no_overflow("path");
            let path = layout.bounds("path");
            assert!(path.size.width.as_f32() <= 200.5);
            if state.name == "cut" {
                // Both parts exist, the head is the one that gave way.
                check.inside("tail", "path");
                let (head, tail) = (layout.bounds("head"), layout.bounds("tail"));
                assert!(head.size.width.as_f32() > 20.);
                assert!(tail.left() >= head.right() - gpui_kit::px(0.5));
                assert!(tail.right() <= path.right() + gpui_kit::px(0.5));
            } else {
                assert!(layout.get("whole").is_ok(), "a short path is not split");
            }
        });
    }

    /// The design layer reads the active theme: the same button is another colour in dark.
    #[core::prelude::v1::test]
    fn the_light_and_dark_themes_paint_the_same_story_differently() {
        let mut cx = headless_app();
        let bg = |cx: &mut gpui_kit::HeadlessAppContext, theme| {
            let window = open_story(cx, &BUTTON_OUTLINE, 0, theme);
            layout(cx, window).color_of("button")
        };
        let (light, dark) = (
            bg(&mut cx, ThemeChoice::T3Light),
            bg(&mut cx, ThemeChoice::T3Dark),
        );
        assert_ne!(light, dark);
        let theme_bg = cx.update(|cx| cx.theme().background);
        let _ = theme_bg;
    }
}
