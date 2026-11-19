//! Stories of the thread view (CP4.1): each message kind over fixture histories, and the
//! recorded turn (`crates/native/fixtures/thread-session.json`, made by a real core) replayed
//! whole, cut off mid-stream, ending in an error, and a long thread scrolled up (the "Scroll
//! to end" button). t3code's two themes; the references are named in `thread/view.rs` and
//! `thread/markdown.rs` (`app-thread-dark.png`, `app-codeblock_closeup-dark.png`).
//!
//! The views live in the story window's element state (`use_keyed_state`), so the history read
//! and the parses survive the frames the harness draws.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    dev::ProbeExt as _,
    model::{
        FixtureBackend,
        fixture::{self, NOW_MS},
    },
    thread::{ThreadModel, ThreadView, recording::Recording},
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const MESSAGE_STATES: [State; 4] = [
    at_rest("reply"),
    at_rest("reasoning-open"),
    at_rest("attachments-code"),
    at_rest("rich"),
];

const LIVE_STATES: [State; 4] = [
    at_rest("recorded"),
    at_rest("streaming"),
    at_rest("error"),
    at_rest("scrolled-up"),
];

/// The thread view of `session` over the fixture histories, in a 720 x 560 frame.
fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let view = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("thread-story-{name}").into()),
        cx,
        |window, cx| {
            let backend = FixtureBackend::new(fixture::snapshot());
            backend.set_history("rec", Vec::new());
            backend.set_history("long", fixture::long_history(200));
            let session = match name {
                "reply" | "reasoning-open" => "t1",
                "attachments-code" => "t2",
                "rich" => "t4",
                "scrolled-up" => "long",
                _ => "rec",
            };
            let model = cx.new(|cx| {
                let mut model =
                    ThreadModel::new(session, Arc::clone(&backend) as _, cx).with_clock(|| NOW_MS);
                if name == "reasoning-open" {
                    model = model.with_reasoning_open();
                }
                let recording = Recording::fixture();
                match name {
                    "recorded" => model.replay(&recording, cx),
                    // Cut after the code block's first chunk: the fence is still open.
                    "streaming" => model.replay(&recording.truncated(13), cx),
                    "error" => {
                        let mut failed = recording.truncated(7);
                        failed
                            .events
                            .push(crate::thread::recording::Recorded::TurnEnded {
                                outcome: "error".into(),
                                error: Some(
                                    "The model provider is rate-limiting requests (mock). Your \
                                 message was not answered.\nWait a moment, then retry."
                                        .into(),
                                ),
                                text: String::new(),
                            });
                        model.replay(&failed, cx);
                    }
                    _ => {}
                }
                model
            });
            ThreadView::new(model, false, window, cx)
        },
    );
    if name == "scrolled-up" {
        // Once the history is in, scroll up once (a person's wheel; the button shows).
        let done = window.use_keyed_state("thread-story-scrolled", cx, |_, _| false);
        let model = view.read(cx).model().clone();
        if !*done.read(cx) && !model.read(cx).rows().is_empty() {
            done.update(cx, |d, _| *d = true);
            let scroller = model.read(cx).scroller().clone();
            scroller.update(cx, |s, cx| {
                s.scroll_to_item(40, cx);
            });
        }
    }
    div()
        .id("thread-frame")
        .w(px(720.))
        .h(px(560.))
        .flex()
        .child(view)
        .probe()
        .into_any_element()
}

/// Each message kind: a reply with its reasoning closed and open, a user message with images and
/// a code reply, the rich reply (alerts, a wide table, file links, code, a tool line, a failed
/// turn).
pub static THREAD: Story = Story {
    name: "thread",
    title: "Thread: messages",
    size: (760., 600.),
    themes: Themes::T3,
    states: &MESSAGE_STATES,
    render: cell,
};

/// The recorded turn replayed: whole, mid-stream, ending in an error; a long thread scrolled up.
pub static THREAD_LIVE: Story = Story {
    name: "thread-live",
    title: "Thread: live turn",
    size: (760., 600.),
    themes: Themes::T3,
    states: &LIVE_STATES,
    render: cell,
};
