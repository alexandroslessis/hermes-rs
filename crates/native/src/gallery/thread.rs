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

use serde_json::json;

use super::{Interaction, State, Story, Themes};
use crate::{
    dev::ProbeExt as _,
    model::{
        FixtureBackend,
        fixture::{self, NOW_MS},
    },
    thread::{
        ThreadModel, ThreadView,
        recording::{Recorded, Recording},
    },
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
                                user_row: None,
                                last_row: None,
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

// --- the work log (CP4.2) ----------------------------------------------------------------------

/// How much of a story's cards is open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    Nothing,
    /// The group's list, every card still closed.
    Group,
    /// Everything: the group, each card, each subagent task.
    All,
}

/// One state of a tool story: which calls of the recorded turn it shows and how.
struct Spec {
    calls: &'static [&'static str],
    /// The usage and the end of the turn too (its footer).
    end: bool,
    /// This call has not finished: its result is left out (a command gets live output).
    running: Option<&'static str>,
    /// The turn is stopped after the calls: an `interrupted` end.
    stopped: bool,
    open: Open,
    /// The footer's folders are closed.
    close_dirs: bool,
    /// The footer's actions wait.
    busy: bool,
    /// Calls the recording does not have (shapes documented in `thread/tools.rs`).
    synthetic: Option<fn() -> Vec<Recorded>>,
}

const SPEC: Spec = Spec {
    calls: &[],
    end: false,
    running: None,
    stopped: false,
    open: Open::All,
    close_dirs: false,
    busy: false,
    synthetic: None,
};

/// A web fetch that worked: `web_extract`'s result, `{results: [{url, title, content, error}]}`.
fn web_ok() -> Vec<Recorded> {
    vec![
        Recorded::ToolStarted {
            tool: "s1".into(),
            name: "web_extract".into(),
            args: json!({"urls": ["https://example.com/gpui-timings"]}),
        },
        Recorded::ToolCompleted {
            tool: "s1".into(),
            name: "web_extract".into(),
            text: json!({"results": [{"url": "https://example.com/gpui-timings", "title": "GPUI timings",
                "content": "# GPUI timings\n\nA list re-measures only the rows marked dirty.\n", "error": null}]})
            .to_string(),
            is_error: false,
        },
    ]
}

/// A tool the cards do not know: its name, arguments and result as they are.
fn generic() -> Vec<Recorded> {
    vec![
        Recorded::ToolStarted {
            tool: "s2".into(),
            name: "todo_list".into(),
            args: json!({"todos": [{"id": "1", "content": "Measure the list", "status": "in_progress"}]}),
        },
        Recorded::ToolCompleted {
            tool: "s2".into(),
            name: "todo_list".into(),
            text: json!({"todos": [{"id": "1", "content": "Measure the list", "status": "in_progress"}], "summary": {"total": 1}})
                .to_string(),
            is_error: false,
        },
    ]
}

const ALL_CALLS: &[&str] = &[
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10",
];

fn spec(name: &str) -> Spec {
    match name {
        // The command card: finished, failed with its exit code, running with live output, closed.
        "command-ok" => Spec {
            calls: &["t0"],
            ..SPEC
        },
        "command-failed" => Spec {
            calls: &["t1"],
            ..SPEC
        },
        "command-running" => Spec {
            calls: &["t0"],
            running: Some("t0"),
            ..SPEC
        },
        "command-closed" => Spec {
            calls: &["t0"],
            open: Open::Nothing,
            ..SPEC
        },
        // File edits: the diff of a patch, a written file, a closed card, a failed read.
        "patch-open" => Spec {
            calls: &["t4"],
            ..SPEC
        },
        "write-open" => Spec {
            calls: &["t5"],
            ..SPEC
        },
        "patch-closed" => Spec {
            calls: &["t4"],
            open: Open::Nothing,
            ..SPEC
        },
        "read-failed" => Spec {
            calls: &["t10"],
            ..SPEC
        },
        // Look-ups: a read, a search, a blocked and a good web fetch.
        "read-open" => Spec {
            calls: &["t2"],
            ..SPEC
        },
        "search-open" => Spec {
            calls: &["t3"],
            ..SPEC
        },
        "web-blocked" => Spec {
            calls: &["t8"],
            ..SPEC
        },
        "web-ok" => Spec {
            synthetic: Some(web_ok),
            ..SPEC
        },
        // Delegation, memory, skill, a tool nobody knows.
        "delegate-open" => Spec {
            calls: &["t9"],
            ..SPEC
        },
        "memory-open" => Spec {
            calls: &["t6"],
            ..SPEC
        },
        "skill-open" => Spec {
            calls: &["t7"],
            ..SPEC
        },
        "generic-open" => Spec {
            synthetic: Some(generic),
            ..SPEC
        },
        // Groups of calls.
        "group-folded" => Spec {
            calls: ALL_CALLS,
            open: Open::Nothing,
            ..SPEC
        },
        "group-open" => Spec {
            calls: ALL_CALLS,
            open: Open::Group,
            ..SPEC
        },
        "group-running" => Spec {
            calls: &["t2", "t3", "t4", "t0"],
            running: Some("t0"),
            open: Open::Group,
            ..SPEC
        },
        "group-stopped" => Spec {
            calls: &["t2", "t3", "t4", "t0"],
            running: Some("t0"),
            stopped: true,
            open: Open::Group,
            ..SPEC
        },
        // The turn's footer.
        "footer-tree" => Spec {
            calls: &["t4", "t5"],
            end: true,
            open: Open::Nothing,
            ..SPEC
        },
        "footer-closed" => Spec {
            calls: &["t4", "t5"],
            end: true,
            open: Open::Nothing,
            close_dirs: true,
            ..SPEC
        },
        "footer-usage" => Spec {
            calls: &["t0"],
            end: true,
            open: Open::Nothing,
            ..SPEC
        },
        _ => Spec {
            calls: &["t4", "t5"],
            end: true,
            open: Open::Nothing,
            busy: true,
            ..SPEC
        },
    }
}

/// The model of a story state: the recorded turn's calls (or made-up ones) replayed through the
/// real model code, then opened as the state says.
pub fn tools_model(
    name: &str,
    cx: &mut gpui_kit::Context<ThreadModel>,
    backend: Arc<FixtureBackend>,
) -> ThreadModel {
    let spec = spec(name);
    let mut model = ThreadModel::new("rec", backend as _, cx).with_clock(|| NOW_MS);
    let mut recording = Recording::fixture().only_calls(spec.calls, spec.end);
    if let Some(running) = spec.running {
        recording
            .events
            .retain(|e| !matches!(e, Recorded::ToolCompleted { tool, .. } if tool == running));
        // Live output of the command: its tail so far, as the core relays it.
        let is_command = recording.events.iter().any(|e| {
            matches!(e, Recorded::ToolStarted { tool, name, .. } if tool == running && name == "terminal")
        });
        if is_command {
            recording.events.push(Recorded::ToolProgress {
                tool: running.into(),
                name: "terminal".into(),
                message: "\u{1b}[32mpass\u{1b}[0m 12 tests\nrunning 1 more...".into(),
            });
        }
    }
    if let Some(synthetic) = spec.synthetic {
        recording.events.extend(synthetic());
    }
    if spec.stopped {
        recording.events.push(Recorded::TurnEnded {
            outcome: "interrupted".into(),
            error: None,
            text: String::new(),
            user_row: Some(1),
            last_row: None,
        });
    }
    model.replay_turn(&recording, cx);
    match spec.open {
        Open::All => model.expand_all(cx),
        Open::Group => model.open_groups(cx),
        Open::Nothing => {}
    }
    if spec.close_dirs {
        model.close_all_dirs(cx);
    }
    if spec.busy {
        model.mark_footer_busy(cx);
    }
    model
}

fn tools_cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let view = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("tools-story-{name}").into()),
        cx,
        |window, cx| {
            let backend = FixtureBackend::new(fixture::snapshot());
            backend.set_history("rec", Vec::new());
            let model = cx.new(|cx| tools_model(name, cx, backend));
            ThreadView::new(model, false, window, cx)
        },
    );
    div()
        .id("thread-frame")
        .w(px(560.))
        .h(px(300.))
        .flex()
        .child(view)
        .probe()
        .into_any_element()
}

macro_rules! tool_story {
    ($ident:ident, $name:literal, $title:literal, [$($state:literal),*]) => {
        pub static $ident: Story = Story {
            name: $name,
            title: $title,
            size: (620., 340.),
            themes: Themes::T3,
            states: &[$(at_rest($state)),*],
            render: tools_cell,
        };
    };
}

tool_story!(
    TOOLS_COMMAND,
    "thread-tools-command",
    "Thread: command cards",
    [
        "command-ok",
        "command-failed",
        "command-running",
        "command-closed"
    ]
);
tool_story!(
    TOOLS_EDIT,
    "thread-tools-edit",
    "Thread: file edit cards",
    ["patch-open", "write-open", "patch-closed", "read-failed"]
);
tool_story!(
    TOOLS_LOOKUP,
    "thread-tools-lookup",
    "Thread: read, search, web cards",
    ["read-open", "search-open", "web-blocked", "web-ok"]
);
tool_story!(
    TOOLS_AGENT,
    "thread-tools-agent",
    "Thread: delegate, memory, skill, generic",
    ["delegate-open", "memory-open", "skill-open", "generic-open"]
);
tool_story!(
    TOOLS_GROUP,
    "thread-tools-group",
    "Thread: a run of calls",
    [
        "group-folded",
        "group-open",
        "group-running",
        "group-stopped"
    ]
);
tool_story!(
    TOOLS_FOOTER,
    "thread-tools-footer",
    "Thread: the turn's footer",
    [
        "footer-tree",
        "footer-closed",
        "footer-usage",
        "footer-busy"
    ]
);
