//! The window's title bar: t3code's chrome. The window has no system title bar
//! (`TitlebarOptions::appears_transparent`), so the top 40 px row is ours: on the left the
//! sidebar toggle and the app name (while the sidebar is collapsed), in the middle the title
//! (the draggable part), on the right the panel toggles and the three Windows caption buttons
//! (46 px wide each, minimise, maximise or restore, close; close turns red under the pointer).
//!
//! **How the caption buttons work.** They are not wired to click handlers. Each carries a GPUI
//! `window_control_area` (`Min`, `Max`, `Close`) and GPUI answers Windows' non-client hit test
//! (`WM_NCHITTEST`, `gpui-pre-windows` `events.rs` `handle_hit_test_msg`) with `HTMINBUTTON`,
//! `HTMAXBUTTON` and `HTCLOSE` over them, and `HTCAPTION` over the drag zone. So the system does
//! the rest, as for a native bar: Windows 11 shows its **snap layouts flyout** when the pointer
//! rests on the maximise button (that needs `HTMAXBUTTON`, which GPUI returns), double-clicking
//! the drag zone maximises, dragging it moves the window (and snaps it at the screen edges),
//! right-click gives the system menu. Limits: only a *non-interactive* element may be inside a
//! drag zone (the hit test returns the first control area under the pointer whatever is on top,
//! so a button inside it would never get its click); the toggles are therefore siblings of the
//! drag zone, not children. Hover and pressed colours of the three buttons come from GPUI's own
//! pointer tracking, which the non-client mouse messages feed. None of this can be tested
//! headlessly (there is no Windows hit test in the test platform): the layout and the areas are
//! tested, the flyout is on the user's checklist.
//!
//! t3code: `apps/web/src/components/AppSidebarLayout.tsx` (the sidebar and topbar layout; the
//! caption area is Electron's `titleBarOverlay`, `--workspace-topbar-height` 40 px at desktop),
//! `workspaceTitlebar.ts`. Reference: `app-empty-dark.png` (the whole top row: toggle, "T3 Code",
//! back and forward arrows, the title "structure / New thread", the three panel toggles and the
//! caption buttons) and `button-sidebar_toggle-hover.png` (caption buttons, a hovered ghost
//! toggle with its tooltip). Gallery story: `title-bar`.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, WindowControlArea,
    assets::IconName, component::Icon, component::Sizable as _, component::Size as KitSize, div,
    prelude::FluentBuilder as _, px,
};

use crate::design::{Color, tokens::text};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The title bar's height (t3code's `--workspace-topbar-height` on desktop).
pub const HEIGHT: f32 = 40.;
/// One caption button's width (the Windows standard).
pub const CAPTION_WIDTH: f32 = 46.;
/// The least the drag zone keeps beside a header, so the window can always be grabbed.
pub const DRAG_MIN: f32 = 24.;

/// Windows' own red for the close button's hover and its darker pressed shade.
const CLOSE_HOVER: Color = Color::rgb(0xe81123);
const CLOSE_PRESSED: Color = Color::rgb(0xc50f1f);

/// The three caption buttons.
#[derive(IntoElement)]
pub struct CaptionButtons {
    maximized: bool,
}

impl CaptionButtons {
    /// The buttons of a window that is (or is not) maximised: the middle one shows "restore".
    pub fn new(maximized: bool) -> Self {
        Self { maximized }
    }
}

fn caption(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    hover: Color,
    pressed: Color,
    hover_fg: Option<Color>,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let fg = palette.foreground;
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .w(px(CAPTION_WIDTH))
        .h_full()
        .text_color(fg.hsla())
        .window_control_area(area)
        .hover(move |s| s.bg(hover.hsla()).text_color(hover_fg.unwrap_or(fg).hsla()))
        .active(move |s| {
            s.bg(pressed.hsla())
                .text_color(hover_fg.unwrap_or(fg).hsla())
        })
        .child(Icon::new(icon).with_size(KitSize::Size(px(14.))))
        .probe()
        .into_any_element()
}

impl RenderOnce for CaptionButtons {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let dark = cx.design_theme().appearance.is_dark();
        // Neutral hover: the accent a step stronger than a ghost button's (10% ink).
        let ink = palette.foreground;
        let hover = ink.with_alpha(if dark { 26 } else { 20 });
        let pressed = ink.with_alpha(if dark { 38 } else { 30 });
        div()
            .id("caption-buttons")
            .flex()
            .h_full()
            .flex_shrink_0()
            .child(caption(
                "minimize",
                IconName::WindowMinimize,
                WindowControlArea::Min,
                hover,
                pressed,
                None,
                cx,
            ))
            .child(caption(
                if self.maximized {
                    "restore"
                } else {
                    "maximize"
                },
                if self.maximized {
                    IconName::WindowRestore
                } else {
                    IconName::WindowMaximize
                },
                WindowControlArea::Max,
                hover,
                pressed,
                None,
                cx,
            ))
            .child(caption(
                "close",
                IconName::WindowClose,
                WindowControlArea::Close,
                CLOSE_HOVER,
                CLOSE_PRESSED,
                Some(Color::rgb(0xffffff)),
                cx,
            ))
            .probe()
    }
}

/// The title bar row: `left` (buttons, not draggable), the draggable title, `right` (buttons),
/// then the caption buttons.
#[derive(IntoElement)]
pub struct TitleBar {
    left: Option<AnyElement>,
    header: Option<AnyElement>,
    title: Option<SharedString>,
    right: Option<AnyElement>,
    maximized: bool,
    caption: bool,
}

impl TitleBar {
    /// An empty bar.
    pub fn new() -> Self {
        Self {
            left: None,
            header: None,
            title: None,
            right: None,
            maximized: false,
            caption: true,
        }
    }

    /// Controls at the left (outside the drag zone).
    pub fn left(mut self, element: impl IntoElement) -> Self {
        self.left = Some(element.into_any_element());
        self
    }

    /// The thread header (CP4.3): interactive content (a title that edits, a menu, chips) that
    /// cannot sit inside the drag zone, so it is its sibling: it takes the room it needs (and
    /// shrinks when there is not enough) and the drag zone keeps the rest, at least 24 px.
    pub fn header(mut self, element: impl IntoElement) -> Self {
        self.header = Some(element.into_any_element());
        self
    }

    /// The title in the drag zone.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Controls before the caption buttons (outside the drag zone).
    pub fn right(mut self, element: impl IntoElement) -> Self {
        self.right = Some(element.into_any_element());
        self
    }

    /// Whether the window is maximised.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Leaves the caption buttons out (a bar that is not at the window's top right).
    pub fn without_caption_buttons(mut self) -> Self {
        self.caption = false;
        self
    }
}

impl Default for TitleBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for TitleBar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // No line under the bar: t3code's topbar (`ChatView.tsx`, the `drag-region` header) has
        // none; the canvas runs up to it (`app-empty-dark.png`).
        div()
            .id("titlebar")
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(HEIGHT))
            .w_full()
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .when_some(self.left, |d, left| d.child(left))
            .when_some(self.header, |d, header| d.child(header))
            .child(
                div()
                    .id("drag")
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w(px(DRAG_MIN))
                    .h_full()
                    .px(px(12.))
                    .window_control_area(WindowControlArea::Drag)
                    .when_some(self.title, |d, title| {
                        d.child(
                            div()
                                .id("title")
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(title)
                                .probe(),
                        )
                    })
                    .probe(),
            )
            .when_some(self.right, |d, right| d.child(right))
            .when(self.caption, |d| {
                d.child(CaptionButtons::new(self.maximized))
            })
            .probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn the_bar_and_buttons_have_the_windows_and_t3code_sizes() {
        assert_eq!(HEIGHT, 40.);
        assert_eq!(CAPTION_WIDTH, 46.);
    }
}
