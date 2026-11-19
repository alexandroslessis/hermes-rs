//! `SectionHeader`: the small heading above a group of rows (a project in the sidebar, "Settled 7
//! >", a settings group): the title in muted 12 px medium text, a count or other meta on the
//! right, an optional trailing action, and last the chevron when it can be collapsed (pointing
//! down when open, right when closed, as t3code's). 28 px high, 8 px side padding, the
//! hover fill of a sidebar row when it can be collapsed.
//!
//! t3code: `apps/web/src/components/sidebar/SidebarProjectFolderHeader.tsx` (the folder header
//! row) and `components/ui/sidebar.tsx` (`SidebarGroupLabel`). Reference: the "Settled 7 >" row
//! at the foot of the sidebar in `app-empty-dark.png` and `sidebar.png`. Gallery story:
//! `section-header`.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, assets::IconName, component::Icon, component::Sizable as _,
    component::Size as KitSize, div, prelude::FluentBuilder as _, px,
};

use super::tokens::{radius, text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The height of a header row.
pub const HEIGHT: f32 = 28.;

/// A group heading.
#[derive(IntoElement)]
pub struct SectionHeader {
    id: gpui_kit::ElementId,
    title: SharedString,
    meta: Option<SharedString>,
    open: Option<bool>,
    hovered: bool,
    action: Option<AnyElement>,
}

impl SectionHeader {
    /// A header called `id` saying `title`.
    pub fn new(id: impl Into<gpui_kit::ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            meta: None,
            open: None,
            hovered: false,
            action: None,
        }
    }

    /// A count or other note at the right.
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Makes it collapsible: a chevron, pointing down when `open`.
    pub fn collapsible(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    /// Draws the hover fill (the gallery's hover state; a live header gets it from the pointer).
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    /// An action at the far right (an `IconButton`).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let hover = palette.sidebar_row_hover.hsla();
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(HEIGHT))
            .px(px(8.))
            .rounded(radius::LG)
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .text_color(palette.sidebar_muted_foreground.hsla())
            .when(self.hovered, |d| d.bg(hover))
            .when(self.open.is_some(), |d| d.hover(move |s| s.bg(hover)))
            .child(
                div()
                    .id("title")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(self.title.clone())
                    .probe(),
            )
            .when_some(self.meta, |d, meta| {
                d.child(
                    div()
                        .id("meta")
                        .flex_shrink_0()
                        .text_color(palette.sidebar_secondary_label.hsla())
                        .child(meta.clone())
                        .probe_text(meta),
                )
            })
            .when_some(self.action, |d, action| d.child(action))
            // The chevron is last, after the count and the action (t3code's folder header:
            // `ChevronDownIcon`, turned to point right while collapsed; "Settled 7 >" in
            // `sidebar.png`).
            .when_some(self.open, |d, open| {
                d.child(
                    div()
                        .id("chevron")
                        .flex_shrink_0()
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(KitSize::Size(px(12.))),
                        )
                        .probe(),
                )
            })
            .probe()
    }
}
