//! `Button` and `IconButton`: t3code's buttons, on GPUI Kit's unstyled `Button` (which owns
//! activation by pointer, Enter and Space, focus and Tab order, the disabled state and the
//! accessible name) and our tokens. Variants: `default` (the primary colour), `outline`, `ghost`,
//! `ghost-muted` and `destructive`; sizes `micro` 20, `xs` 24, `sm` 28, `default` 32 and `lg` 36
//! px high (`IconButton`: the same, square). Every button has a 1 px border (transparent for the
//! ghosts), so the sizes do not change between variants and states.
//!
//! t3code: `apps/web/src/components/ui/button.tsx` (`buttonVariants`: variants and size classes
//! at `sm:` and up, which is desktop; the `[:hover,[data-pressed]]` rules are the hover and
//! selected colours; `disabled:opacity-64`). Reference screenshots:
//! `button-session-variants-default.png` (ghost icon buttons), `button-sidebar_toggle-hover.png`
//! (ghost icon button, hover and selected), `buttons-sidebar-down.png`. The gallery stories are
//! `button-variants`, `button-sizes` and `icon-buttons`.
//!
//! Not copied: the 0.97 press scale, the inset highlight shadow of the filled variants and the
//! `before:` hairline shadow of the outline (GPUI has no cheap equivalent; the effect is a pixel
//! of light).

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, assets::IconName, base,
    component::Icon, component::Sizable as _, component::Size as KitSize, div,
    prelude::FluentBuilder as _, px,
};

use super::{
    Color, Palette,
    tokens::{TextSize, control, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// t3code's button variants (`variant` in `buttonVariants`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Variant {
    /// The primary colour: the one action of a view.
    #[default]
    Default,
    /// A bordered button on the popover colour.
    Outline,
    /// No fill until hovered.
    Ghost,
    /// A ghost whose text and icon are muted until hovered.
    GhostMuted,
    /// A destructive action.
    Destructive,
}

impl Variant {
    /// Every variant, in t3code's order.
    pub const ALL: [Variant; 5] = [
        Variant::Default,
        Variant::Outline,
        Variant::Ghost,
        Variant::GhostMuted,
        Variant::Destructive,
    ];

    /// The name t3code and the gallery use.
    pub fn name(self) -> &'static str {
        match self {
            Variant::Default => "default",
            Variant::Outline => "outline",
            Variant::Ghost => "ghost",
            Variant::GhostMuted => "ghost-muted",
            Variant::Destructive => "destructive",
        }
    }
}

/// t3code's button sizes (`size` in `buttonVariants`, desktop values).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Size {
    /// `micro` / `icon-micro`: 20 px.
    Micro,
    /// `xs` / `icon-xs`: 24 px.
    Xs,
    /// `sm` / `icon-sm`: 28 px.
    Sm,
    /// `default` / `icon`: 32 px.
    #[default]
    Default,
    /// `lg` / `icon-lg`: 36 px.
    Lg,
}

impl Size {
    /// Every size, smallest first.
    pub const ALL: [Size; 5] = [Size::Micro, Size::Xs, Size::Sm, Size::Default, Size::Lg];

    /// The height (and the width of an icon button).
    pub fn height(self) -> Pixels {
        match self {
            Size::Micro => control::MICRO,
            Size::Xs => control::XS,
            Size::Sm => control::SM,
            Size::Default => control::DEFAULT,
            Size::Lg => control::LG,
        }
    }

    /// The name t3code and the gallery use.
    pub fn name(self) -> &'static str {
        match self {
            Size::Micro => "micro",
            Size::Xs => "xs",
            Size::Sm => "sm",
            Size::Default => "default",
            Size::Lg => "lg",
        }
    }

    /// Horizontal padding inside the 1 px border (`px-[calc(--spacing(n)-1px)]`).
    fn padding_x(self) -> Pixels {
        px(match self {
            Size::Micro => 5.,
            Size::Xs => 7.,
            Size::Sm => 9.,
            Size::Default => 11.,
            Size::Lg => 13.,
        })
    }

    /// The gap between an icon and the label.
    fn gap(self) -> Pixels {
        px(match self {
            Size::Micro | Size::Xs => 4.,
            Size::Sm => 6.,
            Size::Default | Size::Lg => 8.,
        })
    }

    fn text(self) -> TextSize {
        match self {
            Size::Micro => text::XXS2,
            Size::Xs => text::XS,
            _ => text::SM,
        }
    }

    /// The icon's side: 12, 14, then 16 px.
    fn icon(self) -> Pixels {
        px(match self {
            Size::Micro => 12.,
            Size::Xs => 14.,
            _ => 16.,
        })
    }

    fn radius(self) -> Pixels {
        match self {
            Size::Micro => radius::SM,
            _ => radius::LG,
        }
    }
}

/// What a variant paints in each state, from the palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariantColors {
    /// The resting fill (may be clear).
    pub background: Color,
    /// The text colour.
    pub foreground: Color,
    /// The icon colour (t3code's `--control-icon-color`).
    pub icon: Color,
    /// The 1 px border.
    pub border: Color,
    /// The fill under the pointer or when selected.
    pub hover_background: Color,
    /// The text colour under the pointer or when selected.
    pub hover_foreground: Color,
}

/// The colours of `variant` in a light or dark palette.
pub fn variant_colors(variant: Variant, palette: &Palette, dark: bool) -> VariantColors {
    let clear = Color::rgba(0);
    let scale = |color: Color, by: f32| color.with_alpha((f32::from(color.a) * by).round() as u8);
    match variant {
        Variant::Default => VariantColors {
            background: palette.primary,
            foreground: palette.primary_foreground,
            icon: palette.primary_foreground,
            border: palette.primary,
            hover_background: scale(palette.primary, 0.9),
            hover_foreground: palette.primary_foreground,
        },
        Variant::Outline => VariantColors {
            // Light: the popover colour; dark: `bg-input/32`.
            background: if dark {
                scale(palette.input, 0.32)
            } else {
                palette.popover
            },
            foreground: palette.foreground,
            icon: palette.muted_foreground,
            border: palette.input,
            hover_background: if dark {
                scale(palette.input, 0.64)
            } else {
                scale(palette.accent, 0.5)
            },
            hover_foreground: palette.foreground,
        },
        Variant::Ghost => VariantColors {
            background: clear,
            foreground: palette.foreground,
            icon: palette.muted_foreground,
            border: clear,
            hover_background: palette.accent,
            hover_foreground: palette.foreground,
        },
        Variant::GhostMuted => VariantColors {
            background: clear,
            foreground: palette.muted_foreground,
            icon: palette.muted_foreground,
            border: clear,
            hover_background: palette.accent,
            hover_foreground: palette.foreground,
        },
        Variant::Destructive => VariantColors {
            background: palette.destructive,
            foreground: Color::rgb(0xffffff),
            icon: Color::rgb(0xffffff),
            border: palette.destructive,
            hover_background: scale(palette.destructive, 0.9),
            hover_foreground: Color::rgb(0xffffff),
        },
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A button with an optional icon and label.
#[derive(IntoElement)]
pub struct Button {
    id: gpui_kit::ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    variant: Variant,
    size: Size,
    disabled: bool,
    selected: bool,
    icon_only: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    /// The key that does the same, quietly after the label (the approval panel's `1`, `Esc`).
    hint: Option<SharedString>,
}

impl Button {
    /// A button called `id` (the name layout checks use) with a text label.
    pub fn new(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            icon: None,
            variant: Variant::Default,
            size: Size::Default,
            disabled: false,
            selected: false,
            icon_only: false,
            tooltip: None,
            on_click: None,
            hint: None,
        }
    }

    /// A key hint after the label (`1`, `Esc`, `↵`), drawn at 60%.
    pub fn hint(mut self, key: impl Into<SharedString>) -> Self {
        self.hint = Some(key.into());
        self
    }

    /// A square button with only an icon (t3code's `icon*` sizes); give it an accessible name
    /// with [`Button::tooltip`].
    pub fn icon_only(id: impl Into<gpui_kit::ElementId>, icon: IconName) -> Self {
        Self {
            label: None,
            icon: Some(icon),
            icon_only: true,
            ..Self::new(id, "")
        }
    }

    /// An icon before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
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

    /// Ignores the pointer and the keyboard, drawn at 64%.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Drawn as hovered and kept so (t3code's `data-pressed`: a toggle that is on, a menu's
    /// trigger while it is open).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The tooltip, which is also the accessible name of an icon-only button.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Called on a click, or Enter or Space while focused.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (palette, dark) = {
            let theme = cx.design_theme();
            (theme.palette, theme.appearance.is_dark())
        };
        let colors = variant_colors(self.variant, &palette, dark);
        let size = self.size;
        let radius = size.radius();
        let focus_handle = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        // t3code's ring is `focus-visible`: shown for keyboard focus only, not after a click.
        let focused =
            focus_handle.is_focused(window) && window.last_input_was_keyboard() && !self.disabled;
        let tooltip = self.tooltip.clone();
        let label_text = self.label.clone();
        let icon_color = colors.icon;
        let hover_bg = colors.hover_background;
        let hover_fg = colors.hover_foreground;

        let content = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(size.gap())
            .when_some(self.icon, |row, name| {
                row.child(
                    Icon::new(name)
                        .with_size(KitSize::Size(size.icon()))
                        .text_color(icon_color.hsla()),
                )
            })
            .when_some(label_text, |row, label| {
                row.child(
                    div()
                        .id("label")
                        .text_size(size.text().size)
                        .child(label.clone())
                        .probe_text(label),
                )
            })
            .when_some(self.hint.clone(), |row, hint| {
                row.child(
                    div()
                        .id("hint")
                        .opacity(0.6)
                        .text_size(text::XXS2.size)
                        .child(hint.clone())
                        .probe_text(hint),
                )
            });

        let mut button = base::Button::new(self.id.clone())
            .tab_stop(true)
            .disabled(self.disabled)
            .selected(self.selected)
            .relative()
            .flex_shrink_0()
            .h(size.height())
            .when(self.icon_only, |b| b.w(size.height()))
            .when(!self.icon_only, |b| b.px(size.padding_x()))
            .rounded(radius)
            .border_1()
            .border_color(colors.border.hsla())
            .bg(colors.background.hsla())
            .text_color(colors.foreground.hsla())
            .text_size(size.text().size)
            .when(!self.disabled, |b| {
                b.hover(move |s| s.bg(hover_bg.hsla()).text_color(hover_fg.hsla()))
                    .active(move |s| s.bg(hover_bg.hsla()).text_color(hover_fg.hsla()))
            })
            .styles(move |s| {
                s.selected(move |s| s.bg(hover_bg.hsla()).text_color(hover_fg.hsla()))
                    .disabled(|s| s.opacity(0.64))
            })
            .track_focus(&focus_handle)
            .child(content)
            .when(focused, |b| {
                // t3code's `focus-visible:ring-2 ring-inset ring-ring`.
                b.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(radius)
                        .border_2()
                        .border_color(palette.ring.hsla()),
                )
            });
        if let Some(on_click) = self.on_click {
            button = button.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        if let Some(label) = self.tooltip.clone().filter(|_| self.icon_only) {
            button = button.accessibility_label(label);
        } else if let Some(label) = &self.label {
            button = button.accessibility_label(label.clone());
        }
        // The hover tooltip: GPUI shows it after its delay; the view is `design::tooltip`'s.
        if let Some(text) = tooltip {
            button = button
                .tooltip(move |window, cx| super::tooltip::view(text.clone(), None, window, cx));
        }
        button
    }
}

/// A square button with an icon only: [`Button::icon_only`] with t3code's `icon-*` sizes.
pub type IconButton = Button;

/// An [`IconButton`] called `id` showing `icon`; `tooltip` names it.
pub fn icon_button(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    tooltip: impl Into<SharedString>,
) -> Button {
    Button::icon_only(id, icon).tooltip(tooltip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::tokens::Palette;

    #[core::prelude::v1::test]
    fn sizes_are_t3codes_scale() {
        let heights: Vec<f32> = Size::ALL.iter().map(|s| s.height().as_f32()).collect();
        assert_eq!(heights, [20., 24., 28., 32., 36.]);
    }

    #[core::prelude::v1::test]
    fn variants_differ_and_ghosts_have_no_resting_fill() {
        for dark in [false, true] {
            let palette = if dark {
                Palette::dark()
            } else {
                Palette::light()
            };
            let all = Variant::ALL.map(|v| variant_colors(v, &palette, dark));
            for ghost in [Variant::Ghost, Variant::GhostMuted] {
                let c = variant_colors(ghost, &palette, dark);
                assert_eq!(c.background.a, 0);
                assert_eq!(c.border.a, 0);
                assert_eq!(c.hover_background, palette.accent);
            }
            // The five variants are five different looks.
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert_ne!(a, b);
                }
            }
            // The filled variants darken a little on hover: the same colour at 90%.
            let default = variant_colors(Variant::Default, &palette, dark);
            assert_eq!(default.hover_background.a, 230);
            assert_eq!(default.background, palette.primary);
        }
    }
}
