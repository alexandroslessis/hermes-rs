//! `EmptyState`: what a screen shows when there is nothing to show yet (no project, no thread
//! open, nothing found). An optional icon in a small bordered tile, a title, a muted
//! description and any actions beneath, centred in the space it is given. Sizes `compact`
//! (17 px title, for panels), `default` (20 px) and `hero` (24 px, the first-run screens).
//!
//! t3code: `apps/web/src/components/ui/empty.tsx` (`Empty`, `EmptyMedia` `icon` variant,
//! `EmptyTitle`, `EmptyDescription`, `EmptyContent`; `emptySizeClassName`), used by
//! `NoProjectsHero.tsx` and `NoActiveThreadState.tsx`. Reference: `app-empty-dark.png` ("What
//! should we build in structure?", the title centred above the composer). Gallery story:
//! `empty-state`.
//!
//! Not copied: the two tilted tiles behind the icon tile.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled as _, Window, assets::IconName, component::Icon, component::Sizable as _,
    component::Size as KitSize, div, prelude::FluentBuilder as _, px,
};

use super::tokens::{radius, text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// How big the title is and how much room there is around.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Size {
    /// For panels: 17 px title, 16 px gaps.
    Compact,
    /// 20 px title.
    #[default]
    Default,
    /// 24 px title (t3code uses 30 on wide screens), for first-run screens.
    Hero,
}

impl Size {
    /// The title's font size.
    pub fn title(self) -> f32 {
        match self {
            Size::Compact => 17.,
            Size::Default => 20.,
            Size::Hero => 24.,
        }
    }
}

/// An empty screen.
#[derive(IntoElement)]
pub struct EmptyState {
    id: gpui_kit::ElementId,
    icon: Option<IconName>,
    title: SharedString,
    description: Option<SharedString>,
    size: Size,
    actions: Vec<AnyElement>,
}

impl EmptyState {
    /// An empty state called `id` with a title.
    pub fn new(id: impl Into<gpui_kit::ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            title: title.into(),
            description: None,
            size: Size::Default,
            actions: Vec::new(),
        }
    }

    /// The icon in the tile above the title.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The muted line under the title.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The size.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
}

impl ParentElement for EmptyState {
    /// Children are the actions (buttons) under the text.
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.actions.extend(elements);
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let title = self.size.title();
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap(px(if self.size == Size::Compact { 16. } else { 24. }))
            .p(px(if self.size == Size::Compact { 24. } else { 32. }))
            .text_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .max_w(px(384.))
                    .when_some(self.icon, |d, icon| {
                        d.child(
                            div()
                                .id("media")
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(36.))
                                .mb(px(16.))
                                .rounded(radius::MD)
                                .border_1()
                                .border_color(palette.border.hsla())
                                .bg(palette.card.hsla())
                                .text_color(palette.foreground.hsla())
                                .child(Icon::new(icon).with_size(KitSize::Size(px(18.))))
                                .probe(),
                        )
                    })
                    .child(
                        div()
                            .id("title")
                            .text_size(px(title))
                            .line_height(px(title + 8.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(self.title)
                            .probe(),
                    )
                    .when_some(self.description, |d, description| {
                        d.child(
                            div()
                                .id("description")
                                .mt(px(4.))
                                .text_size(text::SM.size)
                                .line_height(text::SM.line_height)
                                .text_color(palette.muted_foreground.hsla())
                                .child(description)
                                .probe(),
                        )
                    }),
            )
            .when(!self.actions.is_empty(), |d| {
                d.child(
                    div()
                        .id("actions")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(self.actions)
                        .probe(),
                )
            })
            .probe()
    }
}
