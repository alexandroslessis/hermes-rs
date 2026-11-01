//! Zed's pairing of a scrolling pane with its scrollbar.

use gpui::{App, Div, ScrollHandle, Stateful, Window, prelude::*};
use ui::WithScrollbar;

/// `content` scrolls, tracking `handle`, inside `wrapper`, which draws the scrollbar along its
/// right edge. The scrollbar belongs on the parent: put on the scrolling element itself it scrolls
/// away with the content and never follows it. `wrapper` carries the pane's size and frame;
/// `content` fills it.
#[track_caller]
pub fn with_scrollbar(
    wrapper: Div,
    content: Stateful<Div>,
    handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    wrapper
        .child(content.size_full().overflow_y_scroll().track_scroll(handle))
        .vertical_scrollbar_for(handle, window, cx)
}
