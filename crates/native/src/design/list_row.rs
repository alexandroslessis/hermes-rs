//! `ListRow` and `ThreadRow`: the rows of the sidebar. A `ListRow` is one line (a leading icon,
//! a label, a trailing note); a `ThreadRow` is t3code's three-line thread entry: the project
//! (a monogram tile and its name) with the time or the status on the right, the title in 14 px
//! semibold, and the branch in 12 px secondary text. Both fill with the sidebar's hover tint under
//! the pointer and with the open-thread tint when `selected`; a `ThreadRow` with a [`Status`]
//! shows t3code's status icon (a check circle for "Done", a dashed circle for "Working"…) and
//! its word where the time would be.
//!
//! t3code: `apps/web/src/components/sidebar/SidebarThreadRow.tsx` (the row; `isActive` is
//! `selected`), `components/ui/sidebar.tsx` (`SidebarMenuButton`: `h-7 gap-2 px-2 rounded-lg
//! text-sm`), `components/ThreadStatusIndicators.tsx` and `Sidebar.logic.ts`
//! (`resolveThreadStatusPill`: the status colours, mapped to our `info`/`warning`/`error`/
//! `success` text tokens). Reference screenshots: `button-session-variants-selected.png` (three
//! thread rows, the middle one selected: the monogram tiles "SE", "EE", "HR"),
//! `sidebar-thread-details.png` is the *thread details* card, not a row (the README's guess was
//! wrong), `app-empty-dark.png` (the "Done" status, top right of the second row). Gallery stories:
//! `list-row` and `thread-row`.
//!
//! Not copied: pinned/unpin affordance, hover actions (Phase 3), the status pulse animation.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, assets::IconName,
    component::Icon, component::Sizable as _, component::Size as KitSize, div,
    prelude::FluentBuilder as _, px,
};

use super::{
    middle_truncate::MiddleTruncate,
    tokens::{Color, Palette, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The height of a [`ListRow`] (`h-7`).
pub const ROW_HEIGHT: f32 = 28.;

/// One line: icon, label, trailing note.
#[derive(IntoElement)]
pub struct ListRow {
    id: gpui_kit::ElementId,
    icon: Option<IconName>,
    label: SharedString,
    meta: Option<SharedString>,
    selected: bool,
    hovered: bool,
    on_click: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl ListRow {
    /// A row called `id` with a label.
    pub fn new(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            label: label.into(),
            meta: None,
            selected: false,
            hovered: false,
            on_click: None,
        }
    }

    /// The icon before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The note at the right (a count, a time).
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// The open one.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draws the hover tint (the gallery's hover state; a live row gets it from the pointer).
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    /// Called when the row is clicked.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

/// The fill of a row: none, hovered or selected.
pub fn row_fill(palette: &Palette, selected: bool, hovered: bool) -> Option<Color> {
    if selected {
        Some(palette.sidebar_row_active)
    } else if hovered {
        Some(palette.sidebar_row_hover)
    } else {
        None
    }
}

impl RenderOnce for ListRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let hover = palette.sidebar_row_hover.hsla();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .rounded(radius::LG)
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .text_color(palette.sidebar_foreground.hsla())
            .when_some(
                row_fill(&palette, self.selected, self.hovered),
                |d, fill| d.bg(fill.hsla()),
            )
            .when(!self.selected, |d| d.hover(move |s| s.bg(hover)))
            .when_some(on_click, |d, f| {
                d.on_click(move |_, window, cx| f(window, cx))
                    .cursor_pointer()
            })
            .when_some(self.icon, |d, icon| {
                d.child(
                    Icon::new(icon)
                        .with_size(KitSize::Size(px(16.)))
                        .text_color(palette.sidebar_muted_foreground.hsla()),
                )
            })
            .child(
                div()
                    .id("label")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.label)
                    .probe(),
            )
            .when_some(self.meta, |d, meta| {
                d.child(
                    div()
                        .id("meta")
                        .flex_shrink_0()
                        .text_size(text::XS.size)
                        .line_height(text::XS.line_height)
                        .text_color(palette.sidebar_secondary_label.hsla())
                        .child(meta.clone())
                        .probe_text(meta),
                )
            })
            .probe()
    }
}

/// A thread's state in the sidebar (t3code's `resolveThreadStatusPill`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The agent is working.
    Working,
    /// A command waits for the user's answer.
    Waiting,
    /// The turn failed.
    Failed,
    /// New output the user has not seen.
    Unread,
    /// Finished.
    Done,
    /// A command waits for the user's approval (t3code's "Approval": a shield with a question
    /// mark, in the warning colour). The sidebar shows it; it is not in [`Status::ALL`], which is
    /// the CP2.2 gallery's five (adding a state there would change its approved snapshot).
    Approval,
    /// The agent asked a question and waits for the answer (t3code's "Input").
    Input,
}

impl Status {
    /// Every status, in the gallery's order.
    pub const ALL: [Status; 5] = [
        Status::Working,
        Status::Waiting,
        Status::Failed,
        Status::Unread,
        Status::Done,
    ];

    /// The word shown beside the dot.
    pub fn label(self) -> &'static str {
        match self {
            Status::Working => "Working",
            Status::Waiting => "Waiting",
            Status::Failed => "Failed",
            Status::Unread => "New",
            Status::Done => "Done",
            Status::Approval => "Approval",
            Status::Input => "Input",
        }
    }

    /// The icon and word's colour (t3code's `topStatus.className`; waiting is muted there).
    pub fn color(self, palette: &Palette) -> Color {
        match self {
            Status::Working => palette.info_foreground,
            Status::Waiting => palette.muted_foreground,
            Status::Failed => palette.error_foreground,
            Status::Unread => palette.primary,
            Status::Done => palette.success_foreground,
            Status::Approval => palette.warning_foreground,
            Status::Input => palette.info_foreground,
        }
    }

    /// The icon before the word (t3code's `TOP_STATUS_ICONS`; `Unread` is ours, t3code shows an
    /// unread finished thread as "Done").
    pub fn icon(self) -> IconName {
        match self {
            Status::Working => IconName::CircleDashed,
            Status::Waiting => IconName::CircleDotDashed,
            Status::Failed => IconName::CircleAlert,
            Status::Unread => IconName::CircleDot,
            Status::Done => IconName::CircleCheck,
            Status::Approval => IconName::ShieldQuestionMark,
            Status::Input => IconName::MessageCircleQuestionMark,
        }
    }
}

/// The three-line thread entry.
#[derive(IntoElement)]
pub struct ThreadRow {
    id: gpui_kit::ElementId,
    project: SharedString,
    title: SharedString,
    branch: Option<SharedString>,
    time: Option<SharedString>,
    status: Option<Status>,
    selected: bool,
    hovered: bool,
    trailing: Option<AnyElement>,
}

impl ThreadRow {
    /// A thread row called `id`: the project it belongs to and its title.
    pub fn new(
        id: impl Into<gpui_kit::ElementId>,
        project: impl Into<SharedString>,
        title: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            project: project.into(),
            title: title.into(),
            branch: None,
            time: None,
            status: None,
            selected: false,
            hovered: false,
            trailing: None,
        }
    }

    /// The branch on the third line.
    pub fn branch(mut self, branch: impl Into<SharedString>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    /// The relative time at the top right (`now`, `5m`, `20h`).
    pub fn time(mut self, time: impl Into<SharedString>) -> Self {
        self.time = Some(time.into());
        self
    }

    /// A status, shown in place of the time.
    pub fn status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self
    }

    /// The open thread.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draws the hover tint (the gallery's hover state).
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    /// Something at the right of the branch line (a model icon in t3code).
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }
}

/// The monogram tile of a project: two letters on a tint of one of the accent colours picked by
/// the name, so a project keeps its colour (t3code's `ProjectMonogram`).
fn monogram(name: &str, palette: &Palette) -> (String, Color, Color) {
    let letters: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let hash = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    let (accent, ink) = match hash % 4 {
        0 => (palette.primary, palette.update_foreground),
        1 => (palette.success, palette.success_foreground),
        2 => (palette.warning, palette.warning_foreground),
        _ => (palette.info, palette.info_foreground),
    };
    (letters, accent.with_alpha(0x33), ink)
}

impl RenderOnce for ThreadRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let hover = palette.sidebar_row_hover.hsla();
        let (letters, tile, ink) = monogram(&self.project, &palette);
        let status = self.status;
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(8.))
            .py(px(6.))
            .rounded(radius::LG)
            .text_color(palette.sidebar_foreground.hsla())
            .when_some(
                row_fill(&palette, self.selected, self.hovered),
                |d, fill| d.bg(fill.hsla()),
            )
            .when(!self.selected, |d| d.hover(move |s| s.bg(hover)))
            .child(
                div()
                    .id("head")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(16.))
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .child(
                        div()
                            .id("monogram")
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .size(px(16.))
                            .rounded(px(4.))
                            .bg(tile.hsla())
                            .text_color(ink.hsla())
                            .text_size(px(8.))
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(letters)
                            .probe(),
                    )
                    .child(
                        div()
                            .id("project")
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(palette.sidebar_muted_foreground.hsla())
                            .child(self.project)
                            .probe(),
                    )
                    .map(|head| match (status, self.time) {
                        (Some(status), _) => {
                            let color = status.color(&palette).hsla();
                            head.child(
                                div()
                                    .id("status")
                                    .flex()
                                    .items_center()
                                    .flex_shrink_0()
                                    .gap(px(4.))
                                    .text_color(color)
                                    .child(
                                        div()
                                            .id("status-icon")
                                            .flex_shrink_0()
                                            .child(
                                                Icon::new(status.icon())
                                                    .with_size(KitSize::Size(px(14.)))
                                                    .text_color(color),
                                            )
                                            .probe(),
                                    )
                                    .child(
                                        div()
                                            .id("word")
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .child(status.label())
                                            .probe_text(status.label()),
                                    )
                                    .probe(),
                            )
                        }
                        (None, Some(time)) => head.child(
                            div()
                                .id("time")
                                .flex_shrink_0()
                                .text_color(palette.sidebar_secondary_label.hsla())
                                .child(time.clone())
                                .probe_text(time),
                        ),
                        (None, None) => head,
                    })
                    .probe(),
            )
            .child(
                div()
                    .id("title")
                    .h(px(20.))
                    .text_size(text::SM.size)
                    .line_height(text::SM.line_height)
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.title)
                    .probe(),
            )
            .child(
                div()
                    .id("meta")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(16.))
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .text_color(palette.sidebar_secondary_label.hsla())
                    .child(div().flex_1().min_w_0().child(MiddleTruncate::new(
                        "branch",
                        self.branch.unwrap_or_default(),
                    )))
                    .when_some(self.trailing, |d, t| d.child(t))
                    .probe(),
            )
            .probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn the_fill_is_selected_then_hovered_then_none() {
        let p = Palette::dark();
        assert_eq!(row_fill(&p, true, true), Some(p.sidebar_row_active));
        assert_eq!(row_fill(&p, false, true), Some(p.sidebar_row_hover));
        assert_eq!(row_fill(&p, false, false), None);
    }

    #[core::prelude::v1::test]
    fn a_projects_monogram_is_stable_and_two_letters() {
        let p = Palette::dark();
        let (a, ..) = monogram("hermes-rs", &p);
        assert_eq!(a, "HE");
        assert_eq!(monogram("hermes-rs", &p), monogram("hermes-rs", &p));
        assert_eq!(monogram("x", &p).0, "X");
    }

    #[core::prelude::v1::test]
    fn statuses_have_distinct_colours() {
        for p in [Palette::light(), Palette::dark()] {
            let colors: std::collections::HashSet<_> =
                Status::ALL.iter().map(|s| s.color(&p)).collect();
            assert_eq!(colors.len(), Status::ALL.len());
        }
    }
}
