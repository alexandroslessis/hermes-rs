//! The token swatches story (CP2.1, `docs/native-ui/PHASES.md`): every colour of t3code's light
//! and dark palette as a chip with its token name, the control heights, the radii and a type
//! specimen in the interface and code fonts. It draws from [`Palette`] directly, so it looks the
//! same under any gallery theme; the two states are the two palettes. The snapshot is what a
//! change to a token value shows up in.
//!
//! t3code: `apps/web/src/index.css` (`:root`, `@variant dark`), `components/ui/button.tsx` (the
//! control heights); compare the colours with `docs/native-ui/reference/t3code/app-thread-dark.png`.

use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};

use super::{Interaction, State, Story};
use crate::design::tokens::{Palette, control, radius, space, text};
use crate::dev::ProbeExt as _;
use crate::theme::Fonts;

/// Tokens in a column (four columns hold all of them).
const ROWS: usize = 15;

/// Width of a name label.
const LABEL_WIDTH: f32 = 160.;

/// The two palettes.
pub static SWATCHES_STORY: Story = Story {
    name: "tokens-swatches",
    title: "Tokens: colours, sizes, type",
    size: (920., 660.),
    states: &[
        State {
            name: "light",
            interaction: Interaction::None,
        },
        State {
            name: "dark",
            interaction: Interaction::None,
        },
    ],
    render,
};

/// The palette a state shows.
pub fn palette_of(state: &State) -> Palette {
    if state.name == "dark" {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// The element id of a token's chip.
pub fn chip_id(token: &str) -> SharedString {
    format!("chip-{token}").into()
}

fn label(text_size: gpui_kit::Pixels, s: String) -> AnyElement {
    div().text_size(text_size).child(s).into_any_element()
}

fn render(state: &State, _: &mut Window, cx: &mut App) -> AnyElement {
    let palette = palette_of(state);
    let fonts = Fonts::install(cx);
    let entries = palette.entries();
    let columns = entries.chunks(ROWS).map(|rows| {
        div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .children(rows.iter().map(|(name, color)| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(2.))
                    .child(
                        div()
                            .id(chip_id(name))
                            .w(px(28.))
                            .h(px(16.))
                            .rounded(radius::SM)
                            .border_1()
                            .border_color(palette.border.hsla())
                            .bg(color.hsla())
                            .probe(),
                    )
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .text_size(text::XXS2.size)
                            .child(*name),
                    )
            }))
    });

    let heights = [
        ("micro", control::MICRO),
        ("xs", control::XS),
        ("sm", control::SM),
        ("default", control::DEFAULT),
        ("lg", control::LG),
    ];
    let controls = div()
        .flex()
        .items_end()
        .gap(space(3.))
        .children(heights.map(|(name, h)| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(space(1.))
                .child(
                    div()
                        .id(SharedString::from(format!("control-{name}")))
                        .w(px(56.))
                        .h(h)
                        .rounded(radius::LG)
                        .border_1()
                        .border_color(palette.input.hsla())
                        .bg(palette.accent.hsla())
                        .probe(),
                )
                .child(label(text::XXS2.size, format!("{name} {}", h.as_f32())))
        }));
    let radii = div().flex().items_end().gap(space(3.)).children(
        [
            ("sm", radius::SM),
            ("md", radius::MD),
            ("lg", radius::LG),
            ("xl", radius::XL),
        ]
        .map(|(name, r)| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(space(1.))
                .child(
                    div()
                        .w(px(40.))
                        .h(px(40.))
                        .rounded(r)
                        .border_1()
                        .border_color(palette.input.hsla())
                        .bg(palette.card.hsla()),
                )
                .child(label(text::XXS2.size, format!("{name} {}", r.as_f32())))
        }),
    );

    let specimen = div()
        .id("specimen")
        .flex()
        .flex_col()
        .gap(space(1.))
        .child(
            div()
                .font_family(fonts.ui.clone())
                .text_size(text::SM.size)
                .child("Halo! How can I help you today? Worked for 2.8s"),
        )
        .child(
            div()
                .font_family(fonts.ui.clone())
                .font_weight(FontWeight::BOLD)
                .text_size(text::SM.size)
                .child("ayo  main  hermes-rs  18k / 200k"),
        )
        .child(
            div()
                .font_family(fonts.ui.clone())
                .text_color(palette.muted_foreground.hsla())
                .text_size(text::XXS2.size)
                .child("20h  now  Uncommitted  +6,770 -0"),
        )
        .child(
            div()
                .font_family(fonts.mono.clone())
                .text_size(text::CODE.size)
                .child("fn main() { let x = 0xFF; } // crates/native/src/lib.rs"),
        );

    div()
        .id("tokens")
        .flex()
        .flex_col()
        .gap(space(3.))
        .p(space(3.))
        .w(px(880.))
        .bg(palette.background.hsla())
        .text_color(palette.foreground.hsla())
        .font_family(fonts.ui)
        .child(div().flex().gap(space(6.)).children(columns))
        .child(controls)
        .child(radii)
        .child(specimen)
        .probe()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gallery::open_story;
    use crate::testing::{headless_app, layout};
    use crate::theme::ThemeChoice;

    #[core::prelude::v1::test]
    fn every_token_has_a_chip_in_its_colour_and_everything_fits() {
        let mut cx = headless_app();
        for (ix, state) in SWATCHES_STORY.states.iter().enumerate() {
            let window = open_story(&mut cx, &SWATCHES_STORY, ix, ThemeChoice::T3Light);
            let layout = layout(&mut cx, window);
            let palette = palette_of(state);
            let mut check = layout.check();
            check.inside("tokens", "story").no_overflow("story");
            let mut chips = Vec::new();
            for (token, color) in palette.entries() {
                let chip = chip_id(token).to_string();
                check
                    .background(&chip, color.hsla())
                    .inside(&chip, "tokens");
                chips.push(chip);
            }
            let chips: Vec<&str> = chips.iter().map(String::as_str).collect();
            check.same_height(&chips);
            for (name, height) in [
                ("micro", 20.),
                ("xs", 24.),
                ("sm", 28.),
                ("default", 32.),
                ("lg", 36.),
            ] {
                let id = format!("control-{name}");
                check.inside(&id, "tokens");
                let have = layout.bounds(&id).size.height.as_f32();
                assert_eq!(have, height, "{} control {name}", state.name);
            }
            check.assert();
        }
    }
}
