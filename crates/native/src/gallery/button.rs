//! The sample story of CP0.2 (`docs/native-ui/PHASES.md`): GPUI Kit's default button in its five
//! states (normal, hover, pressed, focused, disabled), with layout checks over each state and a
//! deliberately squeezed variant the checks and the snapshot comparison must catch. Phase 2's
//! design layer (`design/`) replaces it with our own `Button` stories.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Styled as _,
    Window,
    component::{Disableable as _, button::Button},
    div,
};

use super::{Interaction, State, Story};
use crate::dev::ProbeExt as _;

/// The button's text.
const LABEL: &str = "Save changes";

/// The five states.
pub const STATES: [State; 5] = [
    State {
        name: "normal",
        interaction: Interaction::None,
    },
    State {
        name: "hover",
        interaction: Interaction::Hover("button"),
    },
    State {
        name: "pressed",
        interaction: Interaction::Press("button"),
    },
    State {
        name: "focused",
        interaction: Interaction::Focus("button"),
    },
    State {
        name: "disabled",
        interaction: Interaction::None,
    },
];

/// GPUI Kit's default button in its five states.
pub static STATES_STORY: Story = Story {
    name: "button-states",
    title: "Button: five states",
    size: (160., 60.),
    states: &STATES,
    render,
};

fn render(state: &State, _: &mut Window, _: &mut App) -> AnyElement {
    button(state.name == "disabled", None)
}

/// The button; `width` forces a width (the broken variant squeezes it).
fn button(disabled: bool, width: Option<Pixels>) -> AnyElement {
    let button = Button::new("button")
        .accessibility_label(LABEL)
        .disabled(disabled)
        .child(div().id("caption").child(LABEL).probe_text(LABEL));
    match width {
        Some(width) => button.w(width).into_any_element(),
        None => button.into_any_element(),
    }
}

/// The deliberately broken variant: squeezed to 64 px, too narrow for its label. Never
/// registered; the tests render it in place of the real story to prove the harness notices.
#[cfg(test)]
pub(super) static SQUEEZED_STORY: Story = Story {
    name: "button-states",
    title: "Button: squeezed (broken on purpose)",
    size: (160., 60.),
    states: &STATES,
    render: |state, _, _| button(state.name == "disabled", Some(gpui_kit::px(64.))),
};

#[cfg(test)]
mod tests {
    use gpui_kit::component::ActiveTheme as _;

    use super::*;
    use crate::gallery::open_story as open;
    use crate::testing::{Checks, headless_app, layout};
    use crate::themes::ThemeChoice;

    /// The checks every state of a button story must pass.
    fn common_checks(check: &mut Checks) {
        check
            .inside("button", "story")
            .inside("caption", "button")
            .no_overflow("story")
            .no_overflow("button")
            .no_overflow("caption")
            .same_row("caption", "button");
    }

    #[core::prelude::v1::test]
    fn the_button_in_its_five_states_lays_out_and_colours_right() {
        let mut cx = headless_app();
        for theme in [ThemeChoice::DefaultLight, ThemeChoice::DefaultDark] {
            // The colour checks below only mean something if the states differ in colour.
            cx.update(|cx| crate::themes::apply_theme(theme, cx));
            let colors = cx.update(|cx| cx.theme().colors);
            let distinct: std::collections::HashSet<_> =
                [colors.button, colors.button_hover, colors.button_active]
                    .map(crate::dev::Color::from)
                    .into_iter()
                    .collect();
            assert_eq!(
                distinct.len(),
                3,
                "{}: button colours coincide",
                theme.label()
            );
            let mut heights = Vec::new();
            for (ix, state) in STATES.iter().enumerate() {
                let window = open(&mut cx, &STATES_STORY, ix, theme);
                let layout = layout(&mut cx, window);
                let theme_colors = cx.update(|cx| cx.theme().colors);
                let mut check = layout.check();
                common_checks(&mut check);
                check.width_within("button", 0., 160.);
                match state.name {
                    "normal" => {
                        check
                            .background("button", theme_colors.button)
                            .text_color("caption", theme_colors.button_foreground)
                            .state("button", Some(false), Some(false), Some(false));
                    }
                    "hover" => {
                        check.background("button", theme_colors.button_hover).state(
                            "button",
                            Some(true),
                            Some(false),
                            Some(false),
                        );
                    }
                    "pressed" => {
                        check
                            .background("button", theme_colors.button_active)
                            .state("button", Some(true), None, Some(false));
                    }
                    "focused" => {
                        check.state("button", Some(false), Some(true), Some(false));
                    }
                    "disabled" => {
                        check
                            .text_color("caption", theme_colors.muted_foreground.opacity(0.5))
                            // A disabled button tracks no focus at all (`focused` is unknown).
                            .state("button", None, None, Some(true));
                    }
                    other => panic!("unexpected state {other}"),
                }
                if !check.failures().is_empty() {
                    eprintln!("{} / {}:", theme.label(), state.name);
                }
                check.assert();
                heights.push(layout.bounds("button").size.height);
            }
            assert!(
                heights.iter().all(|h| *h == heights[0]),
                "{}: the button's height changes between states: {heights:?}",
                theme.label()
            );
        }
    }

    #[core::prelude::v1::test]
    fn a_squeezed_button_is_caught_by_the_layout_checks() {
        let mut cx = headless_app();
        let window = open(&mut cx, &SQUEEZED_STORY, 0, ThemeChoice::DefaultLight);
        let layout = layout(&mut cx, window);
        let mut check = layout.check();
        common_checks(&mut check);
        let failures = check.failures();
        // The label keeps the width its text needs and sticks out of the 64 px button.
        for expected in ["inside(caption, button)", "no_overflow(button)"] {
            assert!(
                failures.iter().any(|f| f.starts_with(expected)),
                "{expected} did not report the squeezed label: {failures:?}"
            );
        }
    }
}
