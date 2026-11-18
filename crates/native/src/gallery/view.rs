//! The gallery window (`hermes-native-gallery`): the stories on the left, the selected story in
//! each of its states on the right, and switches for the theme (t3code's default, GPUI Kit's own
//! or Zed's One) and for light or dark. Hover, press and focus are the person's to do with the mouse and Tab;
//! the snapshot harness reaches them on its own.

use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, Selectable as _,
        button::{Button, ButtonVariants as _},
    },
    div, px,
};

use super::{Interaction, all, story_cell};
use crate::dev::ProbeExt as _;
use crate::theme::{ThemeChoice, apply_theme};

/// The gallery's root view.
pub struct GalleryView {
    selected: usize,
    theme: ThemeChoice,
}

impl GalleryView {
    /// The gallery showing the first story under `theme` (already applied).
    pub fn new(theme: ThemeChoice) -> Self {
        Self { selected: 0, theme }
    }

    fn set_theme(&mut self, theme: ThemeChoice, cx: &mut Context<Self>) {
        self.theme = theme;
        apply_theme(theme, cx);
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let switch = |id: &'static str, label: &'static str, selected: bool, to: ThemeChoice| {
            Button::new(id)
                .label(label)
                .ghost()
                .selected(selected)
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(to, cx)))
        };
        div()
            .id("gallery-header")
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Hermes native gallery"),
            )
            .child(switch(
                "theme-t3",
                "Default",
                theme.is_t3(),
                ThemeChoice::T3Light.with_dark(theme.is_dark()),
            ))
            .child(switch(
                "theme-kit",
                "GPUI Kit",
                !theme.is_zed() && !theme.is_t3(),
                ThemeChoice::DefaultLight.with_dark(theme.is_dark()),
            ))
            .child(switch(
                "theme-zed",
                "Zed One",
                theme.is_zed(),
                ThemeChoice::OneLight.with_dark(theme.is_dark()),
            ))
            .child(div().w(px(12.)))
            .child(switch(
                "mode-light",
                "Light",
                !theme.is_dark(),
                theme.with_dark(false),
            ))
            .child(switch(
                "mode-dark",
                "Dark",
                theme.is_dark(),
                theme.with_dark(true),
            ))
            .probe()
            .into_any_element()
    }
}

impl Render for GalleryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stories = all();
        let story = stories[self.selected.min(stories.len() - 1)];
        let cells: Vec<_> = story
            .states
            .iter()
            .enumerate()
            .map(|(ix, state)| {
                let hint: SharedString = match state.interaction {
                    Interaction::None => state.name.into(),
                    Interaction::Hover(_) => format!("{} (point at it)", state.name).into(),
                    Interaction::Press(_) => {
                        format!("{} (hold the button down)", state.name).into()
                    }
                    Interaction::Focus(_) => format!("{} (press Tab)", state.name).into(),
                };
                div()
                    .id(("state", ix))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(hint),
                    )
                    .child(story_cell(story, ix, window, cx))
            })
            .collect();
        let list: Vec<_> = stories
            .iter()
            .enumerate()
            .map(|(ix, story)| {
                Button::new(("story", ix))
                    .label(story.title)
                    .ghost()
                    .selected(ix == self.selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = ix;
                        cx.notify();
                    }))
            })
            .collect();
        let header = self.header(cx);
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .min_h_0()
                    .child(
                        div()
                            .id("stories")
                            .w(px(220.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .overflow_y_scroll()
                            .children(list)
                            .probe(),
                    )
                    .child(
                        div()
                            .id("states")
                            .flex_1()
                            .flex()
                            .flex_wrap()
                            .content_start()
                            .gap_4()
                            .p_4()
                            .overflow_y_scroll()
                            .children(cells)
                            .probe(),
                    ),
            )
    }
}
