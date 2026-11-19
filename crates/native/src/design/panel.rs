//! `Panel`: a surface. Two kinds. A **card** is a floating glass box (the palette, a toast, the
//! small panel at the top right of t3code's thread): `panel_surface` over the canvas, a 1 px
//! `panel_edge`, 12 px corners. A **pane** is a full-height region (the right panel, the bottom
//! drawer): the same surface with a border on one side only and no corners. Either can have a
//! header row (a title and an action) with a divider under it.
//!
//! t3code: the `dropdown-glass` utility in `apps/web/src/index.css` (background: the popover at
//! 18% mixed with the popover at the glass opacity; border: the foreground at 10%), used by
//! popovers and toasts (`components/ui/popover.tsx`, `toast.tsx`), and `RightPanelTabs.tsx` /
//! `DiffPanelShell.tsx` (`border-l border-border`, header `h-10 border-b`). Reference:
//! `sidebar-right.png` (a pane: #0f0f0f with a #252525 edge, the CP2.1 differences these two
//! tokens explain) and the top-right card in `app-empty-dark.png`. Gallery story: `panel`.
//!
//! Not copied: the backdrop blur.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use super::tokens::{radius, text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;
use gpui_kit::base::StyledExt as _;

/// The header row's height (`h-10`).
pub const HEADER_HEIGHT: f32 = 40.;

/// Which side of a pane carries the border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Left (the right panel).
    Left,
    /// Top (the bottom drawer).
    Top,
}

/// What kind of surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A floating glass card.
    Card,
    /// A region of the window, edged on one side.
    Pane(Edge),
}

/// A surface with an optional header and any content.
#[derive(IntoElement)]
pub struct Panel {
    id: gpui_kit::ElementId,
    kind: Kind,
    title: Option<SharedString>,
    action: Option<AnyElement>,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl Panel {
    /// A floating card called `id`.
    pub fn card(id: impl Into<gpui_kit::ElementId>) -> Self {
        Self::new(id, Kind::Card)
    }

    /// A pane edged on `edge`.
    pub fn pane(id: impl Into<gpui_kit::ElementId>, edge: Edge) -> Self {
        Self::new(id, Kind::Pane(edge))
    }

    fn new(id: impl Into<gpui_kit::ElementId>, kind: Kind) -> Self {
        Self {
            id: id.into(),
            kind,
            title: None,
            action: None,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    /// A header row with this title.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// An action at the right of the header.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl Styled for Panel {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Panel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Panel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        // The glass is translucent over the canvas; paint it as the opaque colour that gives
        // (no blur is drawn behind it, so what is behind is always the canvas).
        let surface = palette.panel_surface.over(palette.background).hsla();
        let edge = palette.panel_edge.hsla();
        let has_header = self.title.is_some() || self.action.is_some();
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .bg(surface)
            .text_color(palette.popover_foreground.hsla())
            .map(|d| match self.kind {
                Kind::Card => d.rounded(radius::XL).border_1().border_color(edge),
                Kind::Pane(Edge::Left) => d.border_l_1().border_color(edge),
                Kind::Pane(Edge::Top) => d.border_t_1().border_color(edge),
            })
            .when(has_header, |d| {
                d.child(
                    div()
                        .id("header")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(HEADER_HEIGHT))
                        .flex_shrink_0()
                        .px(px(12.))
                        .border_b_1()
                        .border_color(palette.border.hsla())
                        .text_size(text::SM.size)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .when_some(self.title, |h, title| {
                            h.child(
                                div()
                                    .id("title")
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(title)
                                    .probe(),
                            )
                        })
                        .when_some(self.action, |h, action| h.child(action))
                        .probe(),
                )
            })
            .children(self.children)
            .refine_style(&self.style)
            .probe()
    }
}
