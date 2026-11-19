//! `Tooltip`: the small popover that names a control, with its shortcut in parentheses after the
//! text ("Toggle terminal drawer (Ctrl+J)"). [`Tooltip`] is the bubble as an element (what the
//! gallery and the tests show); [`view`] is what a control hands to GPUI's `.tooltip(..)`, which
//! positions it at the pointer and shows it after its delay. GPUI Kit's own tooltip (a `Root`
//! overlay) is not used: this one is a popover-coloured box with t3code's measurements.
//!
//! t3code: `apps/web/src/components/ui/tooltip.tsx` (`TooltipPopup`: `rounded-md border
//! bg-popover text-xs`, `px-2 py-1`, `max-w-80`) and the call sites that append
//! `shortcutLabelForCommand` in parentheses (`AppSidebarLayout.tsx`). Reference:
//! `button-sidebar_toggle-hover.png` (a rounded popover under the hovered button, "Toggle terminal
//! drawer (Ctrl+J)"). Gallery story: `tooltip`.
//!
//! Not copied: the glass blur and the scale/fade transition.

use gpui_kit::{
    AnyView, App, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, SharedString, Styled as _, Window, div, px,
};

use super::tokens::{radius, text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The widest a tooltip gets before it wraps (`max-w-80`).
pub const MAX_WIDTH: f32 = 320.;

/// A tooltip bubble.
#[derive(IntoElement, Clone)]
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    /// A tooltip saying `text`.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }

    /// Adds the shortcut: `Toggle terminal drawer (Ctrl+J)`.
    pub fn shortcut(mut self, shortcut: impl AsRef<str>) -> Self {
        self.text = format!("{} ({})", self.text, shortcut.as_ref()).into();
        self
    }
}

impl RenderOnce for Tooltip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        div()
            .id("tooltip")
            .max_w(px(MAX_WIDTH))
            .px(px(8.))
            .py(px(4.))
            .rounded(radius::MD)
            .border_1()
            .border_color(palette.border.hsla())
            .bg(palette.popover.over(palette.background).hsla())
            .text_color(palette.popover_foreground.hsla())
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .child(
                div()
                    .id("text")
                    .child(self.text.clone())
                    .probe_text(self.text),
            )
            .probe()
    }
}

/// The entity GPUI keeps while a tooltip is showing.
struct TooltipView(Tooltip);

impl Render for TooltipView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
    }
}

/// What `.tooltip(..)` takes: the view of a [`Tooltip`] with `text` and an optional shortcut.
pub fn view(
    text: SharedString,
    shortcut: Option<SharedString>,
    _: &mut Window,
    cx: &mut App,
) -> AnyView {
    let mut tooltip = Tooltip::new(text);
    if let Some(shortcut) = shortcut {
        tooltip = tooltip.shortcut(shortcut);
    }
    cx.new(|_| TooltipView(tooltip)).into()
}
