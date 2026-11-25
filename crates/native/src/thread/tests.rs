//! The thread view, headless with real text (`testing::headless_app`): layout of the message kinds
//! (the user bubble on the right edge of the content lane, the reply on its left, the code block's
//! header), the virtual list (2,000 rows paint a screenful, older pages load at the top, the list
//! follows the bottom while a turn streams and the "Scroll to end" button appears when scrolled
//! up and brings it back, an older page does not move the rows in view), streaming (only the last
//! block is parsed again; finishing a message copies nothing; a turn seen from the middle is read
//! back from the store when it ends), reasoning collapsed and opened, selection and the copy buttons, the recorded
//! turn replayed, the app model and the workspace wiring, and a rough timing of streaming into a
//! 2,000-row thread.

use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, ClipboardItem, Entity, HeadlessAppContext, InputEvent as _,
    MouseButton, Point, ScrollDelta, ScrollEvent, Window, point, px, size, test::TestEventExt as _,
};
use hermes_core::CoreEvent;

use super::data::{HistoryItem, PAGE};
use super::model::{Body, RowKind};
use super::recording::{Recorded, Recording};
use super::*;
use crate::model::{
    AppModel, FixtureBackend, Selection,
    fixture::{self, NOW_MS},
};
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::{DesignTheme as _, ThemeChoice};

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    model: Entity<ThreadModel>,
}

/// A thread view of `session` (its history `items` when given) in an 800 x 600 window.
fn rig(session: &str, items: Option<Vec<HistoryItem>>, page: usize) -> Rig {
    rig_sized(session, items, page, (800., 600.), false)
}

fn rig_sized(
    session: &str,
    items: Option<Vec<HistoryItem>>,
    page: usize,
    (w, h): (f32, f32),
    _dev: bool,
) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| crate::theme::apply_theme(ThemeChoice::T3Dark, cx));
    let backend = FixtureBackend::new(fixture::snapshot());
    if let Some(items) = items {
        backend.set_history(session, items);
    }
    let model = {
        let backend = backend.clone();
        let session = session.to_string();
        cx.update(|cx| {
            cx.new(|cx| {
                ThreadModel::with_page_size(&session, backend, page, cx).with_clock(|| NOW_MS)
            })
        })
    };
    let root = model.clone();
    let window = open_window(&mut cx, size(px(w), px(h)), move |window, cx| {
        cx.new(|cx| ThreadView::new(root, None, window, cx))
    });
    let mut rig = Rig {
        cx,
        window: window.into(),
        backend,
        model,
    };
    rig.settle();
    rig
}

impl Rig {
    fn settle(&mut self) {
        for _ in 0..3 {
            self.cx.run_until_parked();
            draw(&mut self.cx, self.window);
        }
    }

    fn layout(&mut self) -> Layout {
        layout(&mut self.cx, self.window)
    }

    fn model<R>(
        &mut self,
        f: impl FnOnce(&mut ThreadModel, &mut gpui_kit::Context<ThreadModel>) -> R,
    ) -> R {
        let model = self.model.clone();
        let r = self.cx.update(|cx| model.update(cx, f));
        self.settle();
        r
    }

    fn read<R>(&mut self, f: impl FnOnce(&ThreadModel, &gpui_kit::App) -> R) -> R {
        let model = self.model.clone();
        self.cx.update(|cx| f(model.read(cx), cx))
    }

    fn emit(&mut self, event: CoreEvent) {
        self.model(|m, cx| m.on_event(&event, cx));
    }

    fn rows_painted(&mut self) -> usize {
        self.layout()
            .dump()
            .entries
            .iter()
            .filter(|e| e.path.last().is_some_and(|n| n.starts_with("row:")))
            .count()
    }

    fn following(&mut self) -> bool {
        self.read(|m, cx| m.scroller().read(cx).is_following_tail())
    }

    fn with_window(&mut self, f: impl FnOnce(&mut Window, &mut gpui_kit::App)) {
        self.cx
            .update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap();
        self.settle();
    }

    fn click_at(&mut self, at: Point<gpui_kit::Pixels>) {
        self.with_window(|window, cx| {
            window.pointer_move(at, None, cx);
            window.pointer_down(at, MouseButton::Left, cx);
            window.pointer_up(at, MouseButton::Left, cx);
        });
    }

    fn click(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.click_at(at);
    }

    fn hover(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.with_window(|window, cx| window.pointer_move(at, None, cx));
    }

    fn wheel(&mut self, dy: f32) {
        let at = self.layout().bounds("timeline").center();
        self.with_window(|window, cx| {
            window.pointer_move(at, None, cx);
            window.dispatch_event(
                ScrollEvent {
                    position: at,
                    delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    fn clipboard(&mut self) -> String {
        self.cx.update(|cx| {
            cx.read_from_clipboard()
                .and_then(|i| i.text())
                .unwrap_or_default()
        })
    }

    fn clear_clipboard(&mut self) {
        self.cx
            .update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("NOTHING COPIED".into())));
    }

    fn key_of(&mut self, ix: usize) -> u64 {
        self.read(|m, _| m.rows()[ix].key)
    }
}

fn text_delta(text: &str) -> CoreEvent {
    CoreEvent::TextDelta {
        session_id: "s".into(),
        text: text.into(),
    }
}

fn started() -> CoreEvent {
    CoreEvent::TurnStarted {
        session_id: "s".into(),
        admission_id: "a".into(),
    }
}

fn ended() -> CoreEvent {
    Recorded::TurnEnded {
        outcome: "complete".into(),
        error: None,
        text: String::new(),
        user_row: None,
        last_row: None,
    }
    .to_core("s")
}

// --- layout -------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_user_bubble_sits_at_the_lanes_right_edge_and_the_reply_at_its_left() {
    let mut rig = rig("t1", None, PAGE);
    let keys: Vec<u64> = rig.read(|m, _| m.rows().iter().map(|r| r.key).collect());
    assert_eq!(keys.len(), 3, "user, reasoning, reply");
    let (user, reasoning, reply) = (keys[0], keys[1], keys[2]);
    let layout = rig.layout();
    let row = layout.bounds(&format!("lane:{user}"));
    let bubble = layout.bounds(&format!("user-bubble:{user}"));
    let body = layout.bounds(&format!("assistant-body:{reply}"));
    let toggle = layout.bounds(&format!("reasoning-toggle:{reasoning}"));
    // The lane is 736 px centred in the row; the bubble ends where the
    // lane ends, the reply and the reasoning start where it starts.
    assert!(
        (bubble.right() - row.right()).abs() < px(1.),
        "bubble ends at {:?}, the row at {:?}",
        bubble.right(),
        row.right()
    );
    assert!((body.left() - row.left()).abs() < px(1.));
    assert!((toggle.left() - row.left()).abs() < px(1.));
    assert!(bubble.size.width <= row.size.width * 0.8 + px(1.));
    let mut check = layout.check();
    let palette = rig.cx.update(|cx| cx.design_theme().palette);
    check
        .background(
            &format!("user-bubble:{user}"),
            palette.message_surface.hsla(),
        )
        .no_overflow("timeline")
        .inside(&format!("user-bubble:{user}"), &format!("row:{user}"));
    check.assert();
}

#[core::prelude::v1::test]
fn a_code_block_has_its_language_and_copy_button_in_a_header_above_the_code() {
    let items = vec![HistoryItem::Assistant {
        text: "Here:\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```".into(),
        at_ms: NOW_MS,
    }];
    let mut rig = rig("code", Some(items), PAGE);
    let layout = rig.layout();
    let mut check = layout.check();
    check
        .same_row("code-lang", "copy-code")
        .inside("code-header", "timeline")
        .no_overflow("code-header");
    check.assert();
    let header = layout.bounds("code-header");
    let row = layout.bounds(&format!("row:{}", rig.key_of(0)));
    assert!(
        header.right() > row.center().x,
        "the header is at the right"
    );
    assert!(header.size.height <= px(super::markdown::CODE_HEADER));
    // The copy button copies the code alone.
    rig.clear_clipboard();
    rig.click("copy-code");
    assert_eq!(rig.clipboard(), "fn main() {\n    println!(\"hi\");\n}");
}

// --- the virtual list ---------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_two_thousand_message_thread_paints_a_screenful() {
    let mut rig = rig("long", Some(fixture::long_history(2_000)), 2_000);
    assert_eq!(rig.read(|m, _| m.rows().len()), 2_000);
    let painted = rig.rows_painted();
    assert!(
        (1..60).contains(&painted),
        "{painted} rows painted of 2,000: the list is not virtual"
    );
    assert!(rig.following(), "it opens at the newest row");
    let last = rig.key_of(1_999);
    let first = rig.key_of(0);
    let layout = rig.layout();
    assert!(layout.get(&format!("row:{last}")).is_ok());
    assert!(layout.get(&format!("row:{first}")).is_err());
}

#[core::prelude::v1::test]
fn older_pages_load_when_the_top_comes_near() {
    let mut rig = rig("long", Some(fixture::long_history(200)), 60);
    assert_eq!(rig.read(|m, _| m.rows().len()), 60);
    assert!(rig.read(|m, _| m.has_older()));
    assert_eq!(rig.backend.pages(), [("long".to_string(), None)]);
    // Scroll up, a few times: each time the top comes near, an older page comes in front.
    for _ in 0..60 {
        rig.wheel(3_000.);
        if !rig.read(|m, _| m.has_older()) {
            break;
        }
    }
    assert_eq!(rig.read(|m, _| m.rows().len()), 200, "every page loaded");
    assert_eq!(
        rig.backend.pages(),
        [
            ("long".to_string(), None),
            ("long".to_string(), Some(140)),
            ("long".to_string(), Some(80)),
            ("long".to_string(), Some(20)),
        ]
    );
    let first = rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::User { text, .. } => text.to_string(),
        _ => String::new(),
    });
    assert!(first.starts_with("Question 0:"), "{first}");
    assert!(!rig.following(), "the user scrolled up");
}

/// An older page put in front of the rows does not move what the reader sees: the row at the
/// top stays where it was, to the pixel.
#[core::prelude::v1::test]
fn an_older_page_does_not_move_the_rows_in_view() {
    let mut rig = rig("long", Some(fixture::long_history(200)), 60);
    // Row 20 of the 60 loaded at the top: no page is asked for yet.
    let scroller = rig.read(|m, _| m.scroller().clone());
    rig.cx
        .update(|cx| scroller.update(cx, |s, cx| s.scroll_to_item(20, cx)));
    rig.settle();
    assert_eq!(rig.backend.pages().len(), 1);
    let key = rig.key_of(20);
    let before = rig.layout().bounds(&format!("row:{key}")).top();
    rig.model(|m, cx| m.load_older(cx));
    assert_eq!(rig.read(|m, _| m.rows().len()), 120, "a page came in front");
    assert_eq!(rig.key_of(80), key);
    let after = rig.layout().bounds(&format!("row:{key}")).top();
    assert!(
        (after - before).abs() < px(1.),
        "the row in view moved from {before:?} to {after:?}"
    );
}

#[core::prelude::v1::test]
fn the_list_follows_a_streaming_reply_and_jump_to_latest_brings_it_back() {
    let mut rig = rig("s", Some(fixture::long_history(120)), PAGE);
    rig.emit(started());
    for i in 0..40 {
        rig.emit(text_delta(&format!("word{i} ")));
        if i % 10 == 9 {
            rig.emit(text_delta("\n\nA new paragraph. "));
        }
    }
    assert!(rig.following(), "pinned while streaming");
    let last = rig.read(|m, _| m.rows().last().unwrap().key);
    let layout = rig.layout();
    let row = layout.bounds(&format!("row:{last}"));
    let timeline = layout.bounds("timeline");
    assert!(
        row.bottom() <= timeline.bottom() + px(1.),
        "the streaming row is in view"
    );
    assert!(
        layout.get("jump-to-latest").is_err(),
        "no button at the bottom"
    );

    // The user scrolls up: the list stops following and the button shows.
    rig.wheel(800.);
    rig.emit(text_delta("more while scrolled up "));
    assert!(!rig.following());
    rig.layout().bounds("jump-to-latest");

    // The button: back at the bottom, following again, the button gone.
    rig.click("jump-to-latest");
    rig.emit(text_delta("and after "));
    assert!(rig.following());
    let layout = rig.layout();
    assert!(layout.get("jump-to-latest").is_err());
    let row = layout.bounds(&format!("row:{last}"));
    assert!(row.bottom() <= layout.bounds("timeline").bottom() + px(1.));

    // A small scroll up is enough to stop following; scrolling back to the bottom by hand
    // follows again and the button goes.
    rig.wheel(30.);
    rig.emit(text_delta("a little up "));
    assert!(!rig.following(), "30 px up stops following");
    rig.layout().bounds("jump-to-latest");
    rig.wheel(-3_000.);
    rig.emit(text_delta("back down "));
    assert!(rig.following(), "the bottom follows again");
    assert!(rig.layout().get("jump-to-latest").is_err());
}

// --- streaming ----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn streaming_parses_only_the_last_block_again() {
    use super::markdown::probe;
    let mut rig = rig("s", Some(Vec::new()), PAGE);
    rig.emit(started());
    // Five paragraphs, each streamed in four pieces.
    for p in 0..5 {
        for piece in 0..4 {
            rig.emit(text_delta(&format!("{}{p} piece {piece} ", probe::MARK)));
        }
        rig.emit(text_delta("\n\n"));
    }
    // A sixth starts: the fifth is no longer the last block.
    rig.emit(text_delta(&format!("{}5 start ", probe::MARK)));
    let counts = probe::take();
    assert_eq!(counts.len(), 6, "six paragraphs parsed: {counts:?}");
    // 30 more deltas into the sixth paragraph: the five before it are not parsed again.
    for i in 0..30 {
        rig.emit(text_delta(&format!("tail {i} ")));
    }
    let after = probe::take();
    let last = *counts.keys().max().unwrap();
    for offset in counts.keys().filter(|o| **o != last) {
        assert_eq!(
            after.get(offset),
            None,
            "paragraph at {offset} parsed again while a later one streamed: {after:?}"
        );
    }
    assert_eq!(
        after.keys().copied().collect::<Vec<_>>(),
        [last],
        "only the tail paragraph"
    );
}

#[core::prelude::v1::test]
fn finishing_a_streamed_message_copies_and_reparses_nothing() {
    let mut rig = rig("s", Some(fixture::long_history(300)), 300);
    let texts = |rig: &mut Rig| -> Vec<usize> {
        rig.read(|m, _| {
            m.rows()
                .iter()
                .filter_map(|r| match &r.kind {
                    RowKind::User { text, .. } => Some(text.as_ptr() as usize),
                    RowKind::Assistant {
                        body: Body::Text(t),
                    } => Some(t.as_ptr() as usize),
                    _ => None,
                })
                .collect()
        })
    };
    let before = texts(&mut rig);
    rig.emit(started());
    rig.emit(text_delta("Streaming **now**"));
    let state = rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::Assistant {
            body: Body::Live(state),
        } => state.clone(),
        _ => panic!("a live reply row"),
    });
    rig.emit(text_delta(", and more."));
    rig.emit(ended());
    rig.emit(CoreEvent::Idle {
        session_id: "s".into(),
    });
    // The row still holds the same state (no copy into a String, no new parse), and every
    // earlier row's text is the same allocation.
    rig.read(|m, cx| {
        // The turn's footer is the last row; the reply is the one before it.
        let reply = &m.rows()[m.rows().len() - 2];
        assert!(matches!(m.rows().last().unwrap().kind, RowKind::Footer(_)));
        match &reply.kind {
            RowKind::Assistant {
                body: Body::Live(after),
            } => assert_eq!(after.entity_id(), state.entity_id()),
            _ => panic!("the streamed row changed kind"),
        }
        assert_eq!(
            m.copy_text(reply.key, cx).unwrap(),
            "Streaming **now**, and more."
        );
    });
    assert_eq!(before, texts(&mut rig), "earlier rows were re-allocated");
    assert!(!rig.read(|m, _| m.is_live()));
}

/// A thread opened while a turn runs sees the turn from the middle (its first deltas went by
/// before the model existed): when the turn ends the store's rows replace the streamed ones, so
/// nothing is missing or doubled; a failure's notice stays after them.
#[core::prelude::v1::test]
fn a_turn_seen_from_the_middle_is_read_back_from_the_store_when_it_ends() {
    let question = HistoryItem::User {
        text: "q".into(),
        images: Vec::new(),
        at_ms: NOW_MS,
    };
    let mut rig = rig("s", Some(vec![question.clone()]), PAGE);
    // No `TurnStarted`: the middle of a reply.
    rig.emit(text_delta("the end of the reply"));
    assert_eq!(rig.read(|m, _| m.rows().len()), 2);
    // Meanwhile the core stored the whole reply.
    rig.backend.set_history(
        "s",
        vec![
            question,
            HistoryItem::Assistant {
                text: "The whole reply, the end of the reply".into(),
                at_ms: NOW_MS,
            },
        ],
    );
    rig.emit(
        Recorded::TurnEnded {
            outcome: "error".into(),
            error: Some("The provider failed.".into()),
            text: String::new(),
            user_row: None,
            last_row: None,
        }
        .to_core("s"),
    );
    let rows: Vec<String> = rig.read(|m, cx| {
        m.rows()
            .iter()
            .map(|r| match &r.kind {
                RowKind::User { text, .. } => format!("user:{text}"),
                RowKind::Assistant { body } => format!("reply:{}", body.text(cx)),
                RowKind::Notice { text, .. } => format!("notice:{text}"),
                _ => "other".to_string(),
            })
            .collect()
    });
    assert_eq!(
        rows,
        [
            "user:q",
            "reply:The whole reply, the end of the reply",
            "notice:The provider failed."
        ]
    );
    assert!(rig.following(), "the reloaded thread shows its end");

    // A turn seen whole is left as it streamed: no second read.
    let pages = rig.backend.pages().len();
    rig.emit(started());
    rig.emit(text_delta("next"));
    rig.emit(ended());
    assert_eq!(rig.backend.pages().len(), pages);
    // The prompt, the earlier turn's reply and notice, and this turn's reply and footer.
    assert_eq!(rig.read(|m, _| m.rows().len()), 5);
}

#[core::prelude::v1::test]
fn a_recorded_turn_replays_into_its_rows() {
    let mut rig = rig("rec", Some(Vec::new()), PAGE);
    rig.model(|m, cx| m.replay(&Recording::fixture(), cx));
    let kinds: Vec<String> = rig.read(|m, cx| {
        m.rows()
            .iter()
            .map(|r| match &r.kind {
                RowKind::User { .. } => "user".to_string(),
                RowKind::Assistant { body } => format!("reply:{}", body.text(cx).len()),
                RowKind::Reasoning {
                    duration_ms, live, ..
                } => format!("reasoning:{duration_ms:?}:{live}"),
                RowKind::Work(g) => format!("work:{}", g.entries.len()),
                RowKind::Footer(_) => "footer".to_string(),
                RowKind::Notice { .. } => "notice".to_string(),
                RowKind::Command { .. } => "command".to_string(),
            })
            .collect()
    });
    let recording = Recording::fixture();
    let final_len: usize = recording
        .events
        .iter()
        .skip_while(|e| !matches!(e, Recorded::ToolCompleted { .. }))
        .filter_map(|e| match e {
            Recorded::Text { text } => Some(text.len()),
            _ => None,
        })
        .sum();
    assert_eq!(
        kinds,
        [
            "user".to_string(),
            // Three reasoning deltas, then the text: 3 steps of 120 ms.
            "reasoning:Some(360):false".to_string(),
            "reply:19".to_string(),
            // Eleven calls with nothing between them are one group.
            "work:11".to_string(),
            format!("reply:{final_len}"),
            "footer".to_string(),
        ]
    );
}

// --- reasoning ----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn reasoning_is_collapsed_with_its_duration_and_opens_on_click() {
    let mut rig = rig("t1", None, PAGE);
    let key = rig.key_of(1);
    let layout = rig.layout();
    assert!(
        layout.get(&format!("reasoning-body:{key}")).is_err(),
        "closed"
    );
    let label = rig.read(|m, _| match &m.rows()[1].kind {
        RowKind::Reasoning {
            duration_ms, live, ..
        } => super::data::reasoning_label(*live, *duration_ms),
        _ => String::new(),
    });
    assert_eq!(label, "Thought for 2.8s");
    rig.click(&format!("reasoning-toggle:{key}"));
    let layout = rig.layout();
    let body = layout.bounds(&format!("reasoning-body:{key}"));
    assert!(body.top() >= layout.bounds(&format!("reasoning-toggle:{key}")).bottom());
    rig.click(&format!("reasoning-toggle:{key}"));
    assert!(
        rig.layout().get(&format!("reasoning-body:{key}")).is_err(),
        "closed again"
    );
}

#[core::prelude::v1::test]
fn live_reasoning_is_measured_from_its_first_delta_to_the_reply() {
    let mut rig = rig("s", Some(Vec::new()), PAGE);
    rig.model(|m, cx| {
        m.on_event_at(&started(), 1_000, cx);
        for (t, text) in [(1_000, "Think"), (1_500, "ing")] {
            m.on_event_at(
                &CoreEvent::ReasoningDelta {
                    session_id: "s".into(),
                    text: text.into(),
                },
                t,
                cx,
            );
        }
        m.on_event_at(&text_delta("Answer"), 3_800, cx);
    });
    rig.read(|m, cx| match &m.rows()[0].kind {
        RowKind::Reasoning {
            duration_ms,
            live,
            open,
            body,
        } => {
            assert_eq!((*duration_ms, *live, *open), (Some(2_800), false, false));
            assert_eq!(body.text(cx), "Thinking");
        }
        _ => panic!("a reasoning row first"),
    });
}

// --- selection and copy -------------------------------------------------------------------------

#[core::prelude::v1::test]
fn copy_message_puts_its_markdown_on_the_clipboard() {
    let items = vec![
        HistoryItem::User {
            text: "Copy *me*, please.".into(),
            images: Vec::new(),
            at_ms: NOW_MS,
        },
        HistoryItem::Assistant {
            text: fixture::RICH.into(),
            at_ms: NOW_MS,
        },
    ];
    // Tall enough for the whole reply and its footer.
    let mut rig = rig_sized("copy", Some(items), PAGE, (900., 1600.), false);
    let (user, reply) = (rig.key_of(0), rig.key_of(1));
    rig.hover(&format!("assistant-body:{reply}"));
    rig.clear_clipboard();
    rig.click(&format!("copy-message:{reply}"));
    assert_eq!(rig.clipboard(), fixture::RICH, "the reply, as markdown");
    rig.hover(&format!("user-bubble:{user}"));
    rig.clear_clipboard();
    rig.click(&format!("copy-message:{user}"));
    assert_eq!(rig.clipboard(), "Copy *me*, please.");
}

/// Selection runs in GPUI Kit's `TestAppContext` (fixed-width fake text): under the headless app
/// with the real DirectWrite text system a drag selects nothing (docs/PITFALLS.md).
#[gpui_kit::test]
fn text_selects_inside_a_message_and_in_code_and_copies(cx: &mut gpui_kit::TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
    });
    let backend = FixtureBackend::new(fixture::snapshot());
    backend.set_history(
        "sel",
        vec![HistoryItem::Assistant {
            text: "Alpha beta gamma delta.

```text
select this code line
```"
            .into(),
            at_ms: NOW_MS,
        }],
    );
    let (window, _) = cx.update(|cx| {
        gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                    origin: Point::default(),
                    size: size(px(800.), px(600.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let model = cx.new(|cx| ThreadModel::new("sel", backend, cx));
                cx.new(|cx| ThreadView::new(model, None, window, cx))
            },
        )
        .unwrap()
    });
    let mut cx = gpui_kit::VisualTestContext::from_window(window, cx);
    let frame = |cx: &mut gpui_kit::VisualTestContext| {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    };
    frame(&mut cx);
    frame(&mut cx);
    let body = cx.update(|window, _| {
        let layout = Layout::of(window);
        let entry = layout
            .dump()
            .entries
            .iter()
            .find(|e| {
                e.path
                    .last()
                    .is_some_and(|n| n.starts_with("assistant-body:"))
            })
            .expect("the reply is painted")
            .bounds;
        entry
    });
    let drag_copy =
        |cx: &mut gpui_kit::VisualTestContext, y: gpui_kit::Pixels, from: f32, to: f32| {
            cx.update(|window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("NOTHING COPIED".into()));
                window.pointer_down(point(body.left() + px(from), y), MouseButton::Left, cx);
                window.pointer_move(point(body.left() + px(to), y), Some(MouseButton::Left), cx);
                window.pointer_up(point(body.left() + px(to), y), MouseButton::Left, cx);
                window.dispatch_action(Box::new(gpui_kit::component::input::Copy), cx);
            });
            frame(cx);
            cx.update(|_, cx| {
                cx.read_from_clipboard()
                    .and_then(|i| i.text())
                    .unwrap_or_default()
            })
        };
    let copied = drag_copy(&mut cx, body.top() + px(6.), 1., 300.);
    assert!(copied.contains("Alpha beta"), "copied {copied:?}");
    // The code line sits below the paragraph and the block's header row.
    let found = (30..240).step_by(4).any(|dy| {
        let got = drag_copy(&mut cx, body.top() + px(dy as f32), 20., 600.);
        got.contains("this code line")
    });
    assert!(found, "a drag inside the code block selected its text");
}

// --- sending (the `--dev-send` driver's path; the composer's tests are `composer/tests.rs`) -----

#[core::prelude::v1::test]
fn submit_sends_through_the_backend_and_shows_the_users_row() {
    let mut rig = rig("t1", None, PAGE);
    rig.model(|m, cx| m.submit("hello there", cx));
    assert_eq!(
        rig.backend.sent(),
        [("t1".to_string(), "hello there".to_string())]
    );
    let last = rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::User { text, .. } => text.to_string(),
        _ => String::new(),
    });
    assert_eq!(last, "hello there");
    // A failure shows as a notice row.
    rig.backend.fail_with(Some("no provider"));
    rig.model(|m, cx| m.submit("again", cx));
    let notice = rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::Notice { error, text } => (*error, text.to_string()),
        _ => (false, String::new()),
    });
    assert_eq!(notice, (true, "Could not send: no provider".to_string()));
}

#[core::prelude::v1::test]
fn errors_and_statuses_become_notice_rows() {
    let mut rig = rig("s", Some(Vec::new()), PAGE);
    rig.emit(started());
    rig.emit(CoreEvent::Status {
        session_id: "s".into(),
        kind: "compacting".into(),
        text: "Compacting the conversation".into(),
    });
    rig.emit(
        Recorded::TurnEnded {
            outcome: "error".into(),
            error: Some("The provider failed.".into()),
            text: String::new(),
            user_row: None,
            last_row: None,
        }
        .to_core("s"),
    );
    let notices: Vec<(bool, String)> = rig.read(|m, _| {
        m.rows()
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Notice { error, text } => Some((*error, text.to_string())),
                _ => None,
            })
            .collect()
    });
    assert_eq!(
        notices,
        [
            (false, "Compacting the conversation".to_string()),
            (true, "The provider failed.".to_string())
        ]
    );
    // Events of another thread are not this one's.
    rig.emit(CoreEvent::TextDelta {
        session_id: "other".into(),
        text: "x".into(),
    });
    // The two notices and the footer of the turn that failed.
    assert_eq!(rig.read(|m, _| m.rows().len()), 3);
}

#[core::prelude::v1::test]
fn user_images_show_as_thumbnails_in_the_bubble() {
    let mut rig = rig("t2", None, PAGE);
    let key = rig.key_of(0);
    let layout = rig.layout();
    let thumbs: Vec<_> = layout
        .dump()
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.starts_with("thumb:")))
        .map(|e| e.bounds)
        .collect();
    assert_eq!(thumbs.len(), 2);
    assert_eq!(thumbs[0].top(), thumbs[1].top(), "side by side");
    assert_eq!(thumbs[0].size, size(px(101.), px(76.)));
    let mut check = layout.check();
    check.inside(&format!("thumbs:{key}"), &format!("user-bubble:{key}"));
    check.assert();
}

// --- the app model and the workspace ------------------------------------------------------------

#[core::prelude::v1::test]
fn opening_a_thread_in_the_sidebar_shows_it_and_its_events_stream_in() {
    let mut cx = headless_app();
    cx.update(|cx| crate::theme::apply_theme(ThemeChoice::T3Dark, cx));
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let model = {
        let backend = backend.clone();
        cx.update(|cx| cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx)))
    };
    let root_model = model.clone();
    let window = open_window(&mut cx, size(px(1100.), px(700.)), move |window, cx| {
        cx.new(|cx| {
            crate::workspace::Workspace::with_model(
                crate::ui_state::UiState::default(),
                None,
                root_model,
                window,
                cx,
            )
        })
    });
    let handle: AnyWindowHandle = window.into();
    let settle = |cx: &mut HeadlessAppContext| {
        for _ in 0..3 {
            cx.run_until_parked();
            draw(cx, handle);
        }
    };
    assert!(layout(&mut cx, handle).get("thread").is_err());
    cx.update(|cx| model.update(cx, |m, cx| m.open("t1", cx)));
    settle(&mut cx);
    assert_eq!(
        cx.update(|cx| model.read(cx).selected().clone()),
        Selection::Thread("t1".into())
    );
    let now = layout(&mut cx, handle);
    assert!(
        now.get("thread").is_ok(),
        "the thread view is in the main area"
    );
    let mut check = now.check();
    check.inside("thread", "main");
    check.assert();
    // A turn's text for the open thread lands in it.
    backend.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    backend.emit(CoreEvent::TextDelta {
        session_id: "t1".into(),
        text: "Streamed into the open thread".into(),
    });
    settle(&mut cx);
    let thread = cx.update(|cx| model.read(cx).thread().cloned()).unwrap();
    let last = cx.update(|cx| {
        let t = thread.read(cx);
        t.copy_text(t.rows().last().unwrap().key, cx)
    });
    assert_eq!(last.as_deref(), Some("Streamed into the open thread"));
    // A file link asks the right panel to open the file (from inside the frame, which the app
    // focuses at start).
    cx.update_window(handle, |root, window, cx| {
        let workspace = root.downcast::<crate::workspace::Workspace>().unwrap();
        workspace.update(cx, |w, cx| w.focus(window, cx));
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_action(
            Box::new(OpenFileInPanel {
                path: "src/stream.rs".into(),
                line: Some(42),
            }),
            cx,
        )
    })
    .unwrap();
    settle(&mut cx);
    // The fixture's working tree does not change that file: the Files tab's placeholder names it
    // (CP6.2 builds the file view).
    let (open, file) = cx.update(|cx| {
        let w = window.read(cx).unwrap();
        (
            w.state().right_panel.open,
            w.right_panel().read(cx).file().cloned(),
        )
    });
    assert!(open);
    assert_eq!(file.unwrap().label(), "src/stream.rs:42");
    assert!(layout(&mut cx, handle).get("panel-file").is_ok());
    // Closing the thread drops its model and view.
    cx.update(|cx| model.update(cx, |m, cx| m.clear_selection(cx)));
    settle(&mut cx);
    assert!(cx.update(|cx| model.read(cx).thread().is_none()));
    assert!(layout(&mut cx, handle).get("thread").is_err());
}

// --- performance --------------------------------------------------------------------------------

/// A rough timing (debug build, headless, real text): each delta into a 2,000-row thread, drawn.
/// Printed for the progress note; the bound only catches a frame that does per-row work.
#[core::prelude::v1::test]
fn streaming_into_a_two_thousand_row_thread_stays_cheap_per_frame() {
    /// Mean, p95 and max ms of 200 deltas, each drawn, into a thread of `rows` rows.
    fn measure(rows: usize) -> (f64, f64, f64, usize) {
        let mut rig = rig("s", Some(fixture::long_history(rows)), rows.max(1));
        rig.emit(started());
        let mut frames = Vec::new();
        for i in 0..200 {
            let event = text_delta(&format!("token{i} "));
            let model = rig.model.clone();
            let start = Instant::now();
            rig.cx
                .update(|cx| model.update(cx, |m, cx| m.on_event(&event, cx)));
            rig.cx.run_until_parked();
            draw(&mut rig.cx, rig.window);
            frames.push(start.elapsed().as_secs_f64() * 1000.);
            if i % 40 == 39 {
                rig.emit(text_delta(
                    "

",
                ));
            }
        }
        frames.sort_by(f64::total_cmp);
        let mean = frames.iter().sum::<f64>() / frames.len() as f64;
        let painted = rig.rows_painted();
        (
            mean,
            frames[frames.len() * 95 / 100],
            frames[frames.len() - 1],
            painted,
        )
    }
    let (small, small_p95, _, _) = measure(20);
    let (mean, p95, max, painted) = measure(2_000);
    println!(
        "stream-20: mean {small:.2} ms, p95 {small_p95:.2} ms; stream-2000: mean {mean:.2} ms, p95 {p95:.2} ms, max {max:.2} ms, {painted} rows painted"
    );
    assert!(painted < 60, "still virtual");
    // The thread's length must not show in a frame's cost (generous: timing noise).
    assert!(
        mean < small * 3. + 10.,
        "{mean:.1} ms against {small:.1} ms"
    );
}

// --- the real core --------------------------------------------------------------------------------

/// `CoreBackend::history` over a real core: the stored messages become the timeline's items
/// (reasoning before its reply, the attached image decoded), a page at a time, and reading them
/// does not open the session in the runtime (it would show as working in the sidebar).
#[core::prelude::v1::test]
fn the_core_backend_reads_a_stored_thread_a_page_at_a_time() {
    use base64::Engine as _;
    use hermes_core::{Core, NewSession};
    use hermes_llm::conversation::message::Message;
    use hermes_runtime::{ProviderConfig, RuntimeConfig};

    use crate::model::{Backend, CoreBackend};

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let data = hermes_testdir::tempdir().unwrap();
    let work = hermes_testdir::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = Core::new(config, runtime.handle().clone()).unwrap();
    let png = include_bytes!("../../fixtures/thumb-stripes.png");
    let mut seeded = vec![
        Message::user().with_text("Look at this").with_image(
            base64::engine::general_purpose::STANDARD.encode(png),
            "image/png",
        ),
        Message::assistant()
            .with_thinking("It is striped.", "sig")
            .with_text("Stripes."),
    ];
    for i in 0..70 {
        seeded.push(Message::user().with_text(format!("q{i}")));
        seeded.push(Message::assistant().with_text(format!("a{i}")));
    }
    let seeded: Vec<Message> = seeded.into_iter().map(Message::with_generated_id).collect();
    let (record, _) = core
        .create_session(NewSession {
            provider: ProviderConfig {
                id: "none".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: None,
            },
            model: "none".into(),
            cwd: work.path().to_path_buf(),
            title: Some("Stored".into()),
            seeded,
            parent: None,
        })
        .unwrap();
    let backend = CoreBackend::new(core.clone());
    let last = backend.history(&record.id, None, PAGE).unwrap();
    // 143 items from 142 messages (one reasoning item) and the footer of each of the 71
    // answered turns.
    assert_eq!((last.total, last.start, last.items.len()), (214, 154, 60));
    assert!(matches!(&last.items[58], HistoryItem::Assistant { text, .. } if text == "a69"));
    assert!(
        matches!(&last.items[59], HistoryItem::TurnFooter { files, user_row: Some(_), last_row: Some(_) } if files.is_empty())
    );
    let first = backend.history(&record.id, Some(PAGE), PAGE).unwrap();
    assert_eq!(first.start, 0);
    match &first.items[..3] {
        [
            HistoryItem::User { text, images, .. },
            HistoryItem::Reasoning { text: thought, .. },
            HistoryItem::Assistant { text: reply, .. },
        ] => {
            assert_eq!(text, "Look at this");
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].bytes.as_slice(), png.as_slice());
            assert_eq!(thought, "It is striped.");
            assert_eq!(reply, "Stripes.");
        }
        other => panic!("unexpected first items: {other:?}"),
    }
    assert!(!core.is_live(&record.id), "reading opened the session");

    // Retry: a row that is not a typed prompt is refused and nothing is cut.
    let stored =
        |core: &hermes_core::Core| core.runtime().store().load_messages(&record.id).unwrap();
    assert!(
        backend.retry(&record.id, 142).is_err(),
        "row 142 is a reply"
    );
    assert_eq!(stored(&core).len(), 142);
    // With no provider to send it to, the retry is refused before anything is cut (a rewind
    // followed by a failed send would have lost the turn).
    let refused = backend.retry(&record.id, 141).unwrap_err();
    assert!(refused.contains("Nothing was changed"), "{refused}");
    assert_eq!(stored(&core).len(), 142);
    // The newest prompt (row 141, "q69"): the conversation is cut before it and it is sent
    // again, in one step (the turn then fails on the unreachable provider: the cut stays, the
    // prompt is stored again).
    let sending =
        CoreBackend::new(core.clone()).with_dev_provider(Some("http://127.0.0.1:9/v1".into()));
    sending.retry(&record.id, 141).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while core.is_live(&record.id) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let after = stored(&core);
    assert_eq!(
        after[140].as_concat_text(),
        "q69",
        "the prompt was sent again"
    );
    assert!(
        !after.iter().any(|m| m.as_concat_text() == "a69"),
        "the retried turn's old reply is gone"
    );
    drop(backend);
    drop(core);
    drop(runtime);
}

mod tools;

// --- what the composer shows in the timeline (CP5.2) ------------------------------------------------

#[core::prelude::v1::test]
fn a_command_answer_is_a_quiet_card_in_the_lane_at_three_widths() {
    let report = "Session usage\n  input tokens     12,345\n  output tokens     1,234\n  cost            $0.0123\n\n\
                  A very long line that must wrap inside the card and not push it past the lane: \
                  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for w in [640., 1100., 1920.] {
        let mut rig = rig_sized("t1", None, PAGE, (w, 700.), false);
        rig.model(|m, cx| m.show_command_note("/usage", report, cx));
        let key = rig.read(|m, _| m.rows().last().unwrap().key);
        let layout = rig.layout();
        let (note, lane) = (format!("command-note:{key}"), format!("lane:{key}"));
        let mut check = layout.check();
        check
            .inside(&note, &lane)
            .inside(&format!("command-name:{key}"), &note)
            .inside(&format!("command-text:{key}"), &note);
        check.assert();
        let card = layout.bounds(&note);
        assert!(card.size.width.as_f32() <= LANE_FOR_TEST + 1., "{w}: within the lane");
        assert!(card.right().as_f32() <= w, "{w}: inside the window");
        // The row copies as its text, and is no message: it has no user or assistant row.
        let copied = rig.read(|m, cx| m.copy_text(key, cx));
        assert_eq!(copied.as_deref(), Some(report));
    }
}

const LANE_FOR_TEST: f32 = crate::thread::view::LANE;

#[core::prelude::v1::test]
fn a_sent_message_with_images_shows_their_thumbnails_at_once() {
    let mut rig = rig("t1", Some(Vec::new()), PAGE);
    let png = crate::thread::composer::attach::test_png(32, 24);
    let image = super::data::Attachment {
        name: "shot.png".into(),
        mime: "image/png".into(),
        bytes: Arc::new(png),
    };
    rig.model(|m, cx| m.show_user_with("what is this", vec![image.clone(), image], NOW_MS, cx));
    let key = rig.read(|m, _| m.rows().last().unwrap().key);
    let layout = rig.layout();
    let strip = format!("thumbs:{key}");
    let mut check = layout.check();
    check
        .inside("thumb:0", &strip)
        .inside("thumb:1", &strip)
        .same_row("thumb:0", "thumb:1");
    check.assert();
}
