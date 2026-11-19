//! `Badge`: a small status pill. Variants `default` (primary), `secondary`, `outline` and the four
//! tints `info`, `success`, `warning`, `error` (the accent at 8% in the light theme and 16% in the
//! dark, its text in the `*_foreground` token); sizes `sm` 16 px, `default` 18 px and `lg` 22 px
//! high on desktop. GPUI Kit's `Badge` is a notification dot over another element, a different
//! thing, so this one is ours.
//!
//! t3code: `apps/web/src/components/ui/badge.tsx` (`badgeVariants`, desktop `sm:` sizes). No
//! reference screenshot of a badge exists and t3code was not run to take one (progress note);
//! the nearest in the references is the "Done" status of a thread row in `app-empty-dark.png`.
//! Gallery story: `badge`.

use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, div, px,
};

use super::tokens::{Color, Palette, text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// A badge's colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Variant {
    /// The primary colour.
    #[default]
    Default,
    /// The quiet fill.
    Secondary,
    /// Bordered, on the canvas.
    Outline,
    /// Information.
    Info,
    /// Success.
    Success,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

impl Variant {
    /// Every variant, in the gallery's order.
    pub const ALL: [Variant; 7] = [
        Variant::Default,
        Variant::Secondary,
        Variant::Outline,
        Variant::Info,
        Variant::Success,
        Variant::Warning,
        Variant::Error,
    ];

    /// Its name in the gallery.
    pub fn name(self) -> &'static str {
        match self {
            Variant::Default => "default",
            Variant::Secondary => "secondary",
            Variant::Outline => "outline",
            Variant::Info => "info",
            Variant::Success => "success",
            Variant::Warning => "warning",
            Variant::Error => "error",
        }
    }
}

/// A badge's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Size {
    /// 16 px high, 10 px text.
    Sm,
    /// 18 px high, 12 px text.
    #[default]
    Default,
    /// 22 px high, 14 px text.
    Lg,
}

impl Size {
    /// The height.
    pub fn height(self) -> Pixels {
        px(match self {
            Size::Sm => 16.,
            Size::Default => 18.,
            Size::Lg => 22.,
        })
    }

    fn text(self) -> Pixels {
        match self {
            Size::Sm => px(10.),
            Size::Default => text::XS.size,
            Size::Lg => text::SM.size,
        }
    }

    fn padding_x(self) -> Pixels {
        px(match self {
            Size::Sm | Size::Default => 3.,
            Size::Lg => 5.,
        })
    }
}

/// The fill, text and border of `variant`.
pub fn colors(variant: Variant, palette: &Palette, dark: bool) -> (Color, Color, Color) {
    let tint = |accent: Color| accent.with_alpha(if dark { 41 } else { 20 });
    let clear = Color::rgba(0);
    match variant {
        Variant::Default => (palette.primary, palette.primary_foreground, clear),
        Variant::Secondary => (palette.secondary, palette.secondary_foreground, clear),
        Variant::Outline => (
            if dark {
                palette
                    .input
                    .with_alpha((f32::from(palette.input.a) * 0.32) as u8)
            } else {
                palette.background
            },
            palette.foreground,
            palette.input,
        ),
        Variant::Info => (tint(palette.info), palette.info_foreground, clear),
        Variant::Success => (tint(palette.success), palette.success_foreground, clear),
        Variant::Warning => (tint(palette.warning), palette.warning_foreground, clear),
        Variant::Error => (tint(palette.error), palette.error_foreground, clear),
    }
}

/// A status pill.
#[derive(IntoElement)]
pub struct Badge {
    id: gpui_kit::ElementId,
    label: SharedString,
    variant: Variant,
    size: Size,
}

impl Badge {
    /// A badge called `id` showing `label`.
    pub fn new(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: Variant::Default,
            size: Size::Default,
        }
    }

    /// The variant.
    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    /// The size.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App) -> impl IntoElement {
        let theme = cx.design_theme();
        let (fill, ink, border) = colors(self.variant, &theme.palette, theme.appearance.is_dark());
        div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .h(self.size.height())
            .min_w(self.size.height())
            .px(self.size.padding_x())
            .rounded(px(4.))
            .border_1()
            .border_color(border.hsla())
            .bg(fill.hsla())
            .text_color(ink.hsla())
            .text_size(self.size.text())
            .line_height(self.size.text() + px(2.))
            .child(
                div()
                    .id("label")
                    .child(self.label.clone())
                    .probe_text(self.label),
            )
            .probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn tints_are_the_accent_at_8_and_16_percent() {
        for (dark, alpha) in [(false, 20), (true, 41)] {
            let palette = if dark {
                Palette::dark()
            } else {
                Palette::light()
            };
            let (fill, ink, _) = colors(Variant::Success, &palette, dark);
            assert_eq!(fill, palette.success.with_alpha(alpha));
            assert_eq!(ink, palette.success_foreground);
        }
    }
}
