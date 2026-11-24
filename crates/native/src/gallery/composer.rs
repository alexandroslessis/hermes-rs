//! Stories of the composer (CP5.1): the idle card, one with text, the input grown to its maximum
//! (and scrolling), a running turn (the Stop button and the context meter), the queue above the
//! card, the model and effort picker, the permissions menu and the new-thread screen. t3code's two
//! themes; the references are named in `thread/composer/view.rs` (`app-thread-dark.png`,
//! `app-empty-dark.png`, `composer-model-picker.png`, `composer-model-picker-permission.png`).
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
