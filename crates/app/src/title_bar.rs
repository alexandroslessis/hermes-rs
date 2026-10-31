//! Themed title bar drawn by the app (the native one is hidden with `appears_transparent`).
//! Caption buttons copied from Zed's `platform_title_bar/src/platforms/platform_windows.rs`
//! (GPL-3.0-or-later); the bar is a drag area so Windows snapping and double-click work.

use gpui::{AnyElement, Hsla, Rgba, WindowControlArea, prelude::*, px};
use ui::prelude::*;

pub const TITLE_BAR_HEIGHT: Pixels = px(32.);

/// Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10.
fn caption_font() -> &'static str {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status.is_ok() && version.dwBuildNumber >= 22000 {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

#[derive(IntoElement)]
pub struct TitleBar {
    left: Vec<AnyElement>,
    right: Vec<AnyElement>,
}

impl TitleBar {
    pub fn new() -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    pub fn left(mut self, element: impl IntoElement) -> Self {
        // occlude: without it the drag area below swallows the click (Windows treats it as caption).
        self.left
            .push(div().occlude().child(element).into_any_element());
        self
    }

    pub fn right(mut self, element: impl IntoElement) -> Self {
        // occlude: without it the drag area below swallows the click (Windows treats it as caption).
        self.right
            .push(div().occlude().child(element).into_any_element());
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let background = if window.is_window_active() {
            colors.title_bar_background
        } else {
            colors.title_bar_inactive_background
        };
        h_flex()
            .id("title-bar")
            .window_control_area(WindowControlArea::Drag)
            .w_full()
            .h(TITLE_BAR_HEIGHT)
            .flex_none()
            .justify_between()
            .bg(background)
            .border_b_1()
            .border_color(colors.border_variant)
            .child(h_flex().h_full().pl_2().gap_1().children(self.left))
            .child(
                h_flex()
                    .h_full()
                    .gap_1()
                    .children(self.right)
                    .child(WindowControls),
            )
    }
}

#[derive(IntoElement)]
struct WindowControls;

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id("windows-window-controls")
            .font_family(caption_font())
            .flex()
            .flex_row()
            .justify_center()
            .content_stretch()
            .h_full()
            .child(CaptionButton::Minimize)
            .child(if window.is_maximized() {
                CaptionButton::Restore
            } else {
                CaptionButton::Maximize
            })
            .child(CaptionButton::Close)
    }
}

#[derive(IntoElement)]
enum CaptionButton {
    Minimize,
    Restore,
    Maximize,
    Close,
}

impl CaptionButton {
    fn id(&self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Minimize => "\u{e921}",
            Self::Restore => "\u{e923}",
            Self::Maximize => "\u{e922}",
            Self::Close => "\u{e8bb}",
        }
    }

    fn control_area(&self) -> WindowControlArea {
        match self {
            Self::Close => WindowControlArea::Close,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Minimize => WindowControlArea::Min,
        }
    }
}

impl RenderOnce for CaptionButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (hover_bg, hover_fg, active_bg, active_fg) = match self {
            Self::Close => {
                let color: Hsla = Rgba {
                    r: 232.0 / 255.0,
                    g: 17.0 / 255.0,
                    b: 32.0 / 255.0,
                    a: 1.0,
                }
                .into();
                (
                    color,
                    gpui::white(),
                    color.opacity(0.8),
                    gpui::white().opacity(0.8),
                )
            }
            _ => (
                cx.theme().colors().ghost_element_hover,
                cx.theme().colors().text,
                cx.theme().colors().ghost_element_active,
                cx.theme().colors().text,
            ),
        };
        h_flex()
            .id(self.id())
            .justify_center()
            .content_center()
            .occlude()
            .w(px(46.))
            .h_full()
            .text_size(px(10.0))
            .text_color(cx.theme().colors().text)
            .hover(|style| style.bg(hover_bg).text_color(hover_fg))
            .active(|style| style.bg(active_bg).text_color(active_fg))
            .window_control_area(self.control_area())
            .child(self.icon())
    }
}
