//! The thread view (Phase 4, CP4.1 "Timeline and markdown"): the open thread's conversation in
//! the main area. [`ThreadModel`] holds the rows, the history pages and the streaming turn;
//! [`ThreadView`] draws them as a virtual list; [`markdown`] is GPUI Kit's markdown view with our
//! extensions (code block header, GitHub alerts, sideways tables, file links); [`data`] has the
//! parts with no GPUI in them; [`recording`] is a real turn kept as a test fixture.
//!
//! t3code: `apps/web/src/components/ChatView.tsx`, `chat/ChatCanvas.tsx`, `ChatMarkdown.tsx`,
//! `chat/MessagesTimeline.tsx`. References: `app-thread-dark.png`,
//! `app-codeblock_closeup-dark.png`, `app-hover_reply_closeup-dark.png`.

use gpui_kit::Action;
use serde::Deserialize;

pub mod data;
pub mod markdown;
pub mod model;
pub mod recording;
#[cfg(test)]
mod tests;
pub mod view;

pub use model::{ThreadEvent, ThreadModel};
pub use view::ThreadView;

/// Open a file in the right panel, at a line: what a file link in a message asks for. The right
/// panel's file view is Phase 6; until then the workspace opens the panel and says which file.
#[derive(Action, Clone, Debug, PartialEq, Eq, Deserialize)]
#[action(namespace = thread, no_json)]
pub struct OpenFileInPanel {
    pub path: String,
    pub line: Option<u32>,
}

impl OpenFileInPanel {
    /// `path:line`, or the path alone.
    pub fn label(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}
