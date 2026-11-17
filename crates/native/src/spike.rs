//! The GPUI Kit spike window (CP0.1 of `docs/native-ui/PHASES.md`): throwaway, on fake data.
//! It checks the pieces the native UI depends on: a virtual list of 2,000 mixed-height messages
//! pinned to the bottom while the last one streams, markdown with highlighted code, selection and
//! copy, a multiline input (IME, undo, paste), a resizable three-pane layout and a Zed theme.
//! The verdict is in `docs/native-ui/progress/phase-0.md`.

use std::{rc::Rc, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Theme, ThemeMode, ThemeRegistry,
    button::{Button, ButtonVariants as _},
    h_resizable,
    input::{Textarea, TextareaState},
    message_scroller::{MessageScroller, MessageScrollerState},
    resizable_panel,
    text::{TextView, TextViewState},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    fake::{self, Message, Role},
    zed_theme,
};

/// Messages in the fake conversation.
pub const MESSAGE_COUNT: usize = 2_000;
/// How often the streaming message grows, and by how many words.
const STREAM_INTERVAL: Duration = Duration::from_millis(60);
const WORDS_PER_TICK: usize = 3;
/// After this many words the streamed message is finished and a new one starts below it.
const WORDS_PER_MESSAGE: usize = 240;

/// The Zed theme file the spike maps by hand (`assets/themes/one/one.json`, Zed's One theme).
const ONE_THEME: &str = include_str!("../../../assets/themes/one/one.json");

/// The spike's root view.
pub struct Spike {
    /// The finished messages; the row renderer holds a clone of the `Rc` for its frame.
    messages: Rc<Vec<Message>>,
    scroller: Entity<MessageScrollerState>,
    /// The last row: an assistant message that grows while the spike streams.
    streaming: Entity<TextViewState>,
    /// The streaming message's source so far, kept here to move into `messages` when it ends.
    streamed_text: String,
    streamed_words: usize,
    streaming_on: bool,
    composer: Entity<TextareaState>,
    theme_label: SharedString,
    _stream_task: Option<Task<()>>,
}

impl Spike {
    /// The spike with its fake conversation; `stream` starts the timer that appends words.
    pub fn new(stream: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let messages = Rc::new(fake::conversation(MESSAGE_COUNT));
        // One extra row: the streaming message.
        let rows = messages.len() + 1;
        let scroller = cx.new(|cx| MessageScrollerState::new(rows, cx));
        let streaming = cx.new(|cx| TextViewState::markdown("", cx).selectable(true));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Type here. Try Win+. for emoji, an IME, Ctrl+Z and Ctrl+V.")
        });
        let mut spike = Self {
            messages,
            scroller,
            streaming,
            streamed_text: String::new(),
            streamed_words: 0,
            streaming_on: false,
            composer,
            theme_label: cx.theme().theme_name().clone(),
            _stream_task: None,
        };
        if stream {
            spike.start_streaming(cx);
        }
        spike
    }

    fn start_streaming(&mut self, cx: &mut Context<Self>) {
        self.streaming_on = true;
        self._stream_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STREAM_INTERVAL).await;
                if this.update(cx, |this, cx| this.stream_tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn stop_streaming(&mut self) {
        self.streaming_on = false;
        self._stream_task = None;
    }

    /// Appends a few words to the streaming message; a full one becomes a finished row.
    pub fn stream_tick(&mut self, cx: &mut Context<Self>) {
        if self.streamed_words >= WORDS_PER_MESSAGE {
            let text = std::mem::take(&mut self.streamed_text);
            Rc::make_mut(&mut self.messages).push(Message {
                role: Role::Assistant,
                text,
            });
            self.streaming
                .update(cx, |state, cx| state.set_text("", cx));
            self.streamed_words = 0;
            self.scroller.update(cx, |state, cx| {
                state.append(1, cx);
            });
        }
        let words = fake::STREAM_WORDS;
        let mut chunk = String::new();
        for _ in 0..WORDS_PER_TICK {
            if self.streamed_words > 0 {
                chunk.push(' ');
            }
            // A paragraph break now and then, so the message grows in height, not only width.
            if self.streamed_words > 0 && self.streamed_words.is_multiple_of(60) {
                chunk.push_str("\n\n");
            }
            chunk.push_str(words[self.streamed_words % words.len()]);
            self.streamed_words += 1;
        }
        self.streamed_text.push_str(&chunk);
        self.streaming
            .update(cx, |state, cx| state.push_str(&chunk, cx));
        let last = self.messages.len();
        self.scroller.update(cx, |state, cx| {
            state.remeasure_items(last..last + 1, cx);
        });
        cx.notify();
    }

    /// The scroller's state, for tests.
    #[cfg(test)]
    pub fn scroller(&self) -> &Entity<MessageScrollerState> {
        &self.scroller
    }

    /// The composer's state, for tests.
    #[cfg(test)]
    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    fn set_theme(&mut self, choice: ThemeChoice, cx: &mut Context<Self>) {
        self.theme_label = choice.label().into();
        apply_theme(choice, cx);
        cx.notify();
    }

    fn render_sidebar(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("sidebar")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Threads"),
            )
            .child(
                div()
                    .id("threads")
                    .flex_1()
                    .overflow_y_scroll()
                    .children((0..40usize).map(|ix| {
                        div()
                            .id(("thread", ix))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .hover(|s| s.bg(cx.theme().sidebar_accent))
                            .child(format!("Fake thread {ix}"))
                    })),
            )
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = self.messages.clone();
        let streaming = self.streaming.clone();
        let theme = cx.theme();
        div()
            .id("main")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                MessageScroller::new("timeline", self.scroller.clone(), move |ix, _, cx| {
                    render_row(ix, &messages, &streaming, cx)
                })
                .flex_1(),
            )
            .child(
                div()
                    .id("composer")
                    .p_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Textarea::new(&self.composer)),
            )
    }

    fn render_right(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let following = self.scroller.read(cx).is_following_tail();
        let rows = self.scroller.read(cx).item_count();
        div()
            .id("right-panel")
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme.sidebar)
            .text_sm()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Spike"))
            .child(format!("Theme: {}", self.theme_label))
            .children(ThemeChoice::ALL.map(|choice| {
                Button::new(choice.label())
                    .label(choice.label())
                    .ghost()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_theme(choice, cx)))
            }))
            .child(format!("Rows: {rows}"))
            .child(format!("Following the bottom: {following}"))
            .child(
                Button::new("toggle-stream")
                    .label(if self.streaming_on {
                        "Stop streaming"
                    } else {
                        "Start streaming"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.streaming_on {
                            this.stop_streaming();
                        } else {
                            this.start_streaming(cx);
                        }
                        cx.notify();
                    })),
            )
    }
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_resizable("panes")
                    .child(
                        resizable_panel()
                            .size(px(240.))
                            .size_range(px(160.)..px(420.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(resizable_panel().child(self.render_main(cx)))
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .size_range(px(200.)..px(520.))
                            .child(self.render_right(cx)),
                    ),
            )
    }
}

/// One timeline row: a finished message, or the streaming one after the last.
fn render_row(
    ix: usize,
    messages: &[Message],
    streaming: &Entity<TextViewState>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (role, body) = match messages.get(ix) {
        Some(message) => (
            message.role,
            TextView::markdown(("message", ix), message.text.clone())
                .selectable(true)
                .into_any_element(),
        ),
        None => (
            Role::Assistant,
            TextView::new(streaming).selectable(true).into_any_element(),
        ),
    };
    let user = role == Role::User;
    div()
        .id(("row", ix))
        .w_full()
        .px_4()
        .py_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .when(user, |d| d.bg(theme.muted))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if user {
                            format!("You · {ix}")
                        } else {
                            format!("Hermes · {ix}")
                        }),
                )
                .child(body),
        )
        .into_any_element()
}

/// The themes the right panel offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    /// GPUI Kit's default dark theme.
    DefaultDark,
    /// GPUI Kit's default light theme.
    DefaultLight,
    /// Zed's One Dark, mapped by [`zed_theme::kit_theme`].
    OneDark,
    /// Zed's One Light, mapped by [`zed_theme::kit_theme`].
    OneLight,
}

impl ThemeChoice {
    const ALL: [ThemeChoice; 4] = [
        Self::DefaultDark,
        Self::DefaultLight,
        Self::OneDark,
        Self::OneLight,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::DefaultDark => "Default dark",
            Self::DefaultLight => "Default light",
            Self::OneDark => "One Dark",
            Self::OneLight => "One Light",
        }
    }
}

/// Switches the whole app to `choice`.
pub fn apply_theme(choice: ThemeChoice, cx: &mut App) {
    let zed_name = match choice {
        ThemeChoice::OneDark => Some("One Dark"),
        ThemeChoice::OneLight => Some("One Light"),
        _ => None,
    };
    if let Some(name) = zed_name {
        match zed_theme::kit_theme(ONE_THEME, name) {
            Ok(config) => Theme::update(cx, |theme| theme.apply_config(&Rc::new(config))),
            Err(error) => eprintln!("theme {name}: {error:#}"),
        }
        return;
    }
    // Put GPUI Kit's own themes back before switching mode, or the mode keeps the Zed colours.
    let registry = ThemeRegistry::global(cx);
    let light = registry.default_light_theme().clone();
    let dark = registry.default_dark_theme().clone();
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    let mode = if choice == ThemeChoice::DefaultLight {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);
}

#[cfg(test)]
mod tests {
    //! Headless checks through GPUI Kit's test support. They cover what a test platform can:
    //! the input handler's composition calls (not an OS IME), shortcuts, the test clipboard (not
    //! the Windows one) and the list's tail following. The rest is in the user's checklist.

    use super::*;
    // `gpui_kit::*` (through `super::*`) brings GPUI's `test` macro, which would shadow Rust's
    // `#[test]` that `#[gpui_kit::test]` expands to; name the built-in one explicitly.
    #[allow(unused_imports)]
    use core::prelude::v1::test;
    use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
    use gpui_kit::{TestAppContext, VisualTestContext};

    fn open(cx: &mut TestAppContext) -> (Entity<Spike>, VisualTestContext) {
        cx.update(gpui_kit::init);
        let spike = cx.update(|cx| {
            let bounds = Bounds {
                origin: Point::default(),
                size: size(px(1200.), px(800.)),
            };
            let (window, spike) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Spike::new(false, window, cx)),
            )
            .expect("open the spike window");
            (window, spike)
        });
        let (window, spike) = spike;
        let vcx = VisualTestContext::from_window(window, cx);
        vcx.run_until_parked();
        (spike, vcx)
    }

    #[gpui_kit::test]
    fn the_list_follows_the_bottom_while_text_streams(cx: &mut TestAppContext) {
        let (spike, mut cx) = open(cx);
        assert!(spike.read_with(&cx, |s, cx| s.scroller().read(cx).is_following_tail()));
        for _ in 0..(WORDS_PER_MESSAGE / WORDS_PER_TICK + 5) {
            spike.update(&mut cx, |s, cx| s.stream_tick(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        spike.read_with(&cx, |s, cx| {
            let scroller = s.scroller().read(cx);
            assert_eq!(
                scroller.item_count(),
                MESSAGE_COUNT + 2,
                "a finished message became a row"
            );
            assert!(scroller.is_following_tail(), "still pinned to the bottom");
            assert!(!scroller.is_scrolled_up());
            // The viewport is at the end of the list, not just flagged as following.
            let top = scroller.logical_scroll_top().item_ix;
            assert!(top > MESSAGE_COUNT - 50, "viewport top is row {top}");
        });
        // What the text views painted (Kit's test support sees `TextView`s, not plain labels):
        // streamed words are on screen, the first message is not, and only a screenful of the
        // 2,000 rows was laid out (the list is virtual).
        cx.update(|window, _| {
            let painted: Vec<String> = gpui_kit::base::test_support::texts(window)
                .iter()
                .map(|t| t.text().to_string())
                .collect();
            assert!(
                painted.iter().any(|t| t.contains("The agent reads")),
                "no streamed text painted"
            );
            assert!(
                !painted.iter().any(|t| t.contains("Message 0:")),
                "the first message was painted"
            );
            let user_rows = painted.iter().filter(|t| t.contains("Message ")).count();
            assert!(user_rows < 50, "{user_rows} user rows painted: not virtual");
        });
    }

    #[gpui_kit::test]
    fn the_composer_types_undoes_pastes_and_composes(cx: &mut TestAppContext) {
        let (spike, mut cx) = open(cx);
        let composer = spike.read_with(&cx, |s, _| s.composer().clone());
        cx.update(|window, cx| {
            composer.update(cx, |state, cx| state.focus(window, cx));
            window.render_frame(cx);
            window.input("hello", cx);
            window.press("enter", cx);
            window.input("world", cx);
        });
        let value = |cx: &mut VisualTestContext| composer.read_with(cx, |s, _| s.value());
        assert_eq!(value(&mut cx), "hello\nworld");

        cx.update(|window, cx| {
            window.press("ctrl-z", cx);
        });
        let undone = value(&mut cx);
        assert!(
            undone.len() < "hello\nworld".len() && "hello\nworld".starts_with(undone.as_ref()),
            "undo took back typed text, got {undone:?}"
        );

        cx.update(|window, cx| {
            window.press("ctrl-a", cx);
            cx.write_to_clipboard(ClipboardItem::new_string("pasted 📋".into()));
            window.press("ctrl-v", cx);
        });
        assert_eq!(value(&mut cx), "pasted 📋");

        // An IME's preedit, then its commit, through the same input handler the platform uses.
        cx.update(|window, cx| {
            let bounds = window.bounds();
            let mut handler = ElementInputHandler::new(
                Bounds {
                    origin: Point::default(),
                    size: bounds.size,
                },
                composer.clone(),
            );
            handler.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
            assert!(handler.marked_text_range(window, cx).is_some());
            handler.replace_text_in_range(None, "日本", window, cx);
            assert!(handler.marked_text_range(window, cx).is_none());
        });
        assert_eq!(value(&mut cx), "pasted 📋日本");
    }

    #[gpui_kit::test]
    fn the_zed_theme_applies(cx: &mut TestAppContext) {
        let (_spike, mut cx) = open(cx);
        cx.update(|_, cx| apply_theme(ThemeChoice::OneDark, cx));
        cx.update(|_, cx| {
            assert!(cx.theme().is_dark());
            let expected = gpui_kit::component::try_parse_color("#282c33ff").unwrap();
            assert_eq!(cx.theme().background, expected);
        });
        cx.update(|_, cx| apply_theme(ThemeChoice::DefaultLight, cx));
        cx.update(|_, cx| assert!(!cx.theme().is_dark()));
    }

    /// Selection by pointer and copy, in prose and in a fenced code block. Rendered as a single
    /// text view (not inside the virtual list) so the drag positions are stable.
    #[gpui_kit::test]
    fn markdown_selection_copies_prose_and_code(cx: &mut TestAppContext) {
        struct Doc(Entity<TextViewState>);
        impl Render for Doc {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .child(TextView::new(&self.0).selectable(true))
            }
        }
        cx.update(gpui_kit::init);
        let source = format!("Alpha beta gamma\n\n{}", fake::code_block("rust"));
        let (window, doc) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(800.), px(600.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(|cx| Doc(cx.new(|cx| TextViewState::markdown(&source, cx)))),
            )
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window, cx);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = doc.read_with(&cx, |d, _| d.0.clone());

        let copy = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("NOTHING COPIED".into()));
                window.dispatch_action(Box::new(gpui_kit::component::input::Copy), cx);
            });
            cx.run_until_parked();
            cx.update(|_, cx| {
                cx.read_from_clipboard()
                    .and_then(|i| i.text())
                    .unwrap_or_default()
            })
        };

        // Drag across the first line of prose.
        let bounds = state.read_with(&cx, |s, _| s.bounds());
        let y = bounds.origin.y + px(8.);
        let start = point(bounds.origin.x + px(1.), y);
        let end = point(bounds.origin.x + px(400.), y);
        drag(&mut cx, start, end);
        let selected = state.read_with(&cx, |s, _| s.selected_text());
        assert!(selected.contains("Alpha beta"), "selected {selected:?}");
        assert!(copy(&mut cx).contains("Alpha beta"));

        // Drag inside the code block: its first line (`fn main() {`) sits below the paragraph.
        let mut found = false;
        for dy in (24..200).step_by(4) {
            let y = bounds.origin.y + px(dy as f32);
            drag(
                &mut cx,
                point(bounds.origin.x + px(20.), y),
                point(bounds.origin.x + px(300.), y),
            );
            let selected = state.read_with(&cx, |s, _| s.selected_text());
            if selected.contains("fn main") || selected.contains("let items") {
                assert!(copy(&mut cx).contains(selected.trim()));
                found = true;
                break;
            }
        }
        assert!(found, "a drag inside the code block selected its text");
    }

    /// Tables, lists, headings and links render as text (painted text runs, not their colours).
    #[gpui_kit::test]
    fn markdown_tables_lists_and_links_are_painted(cx: &mut TestAppContext) {
        struct Doc;
        impl Render for Doc {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(TextView::markdown(
                    "doc",
                    "# Heading\n\n| Step | Before |\n|---|---|\n| build | 41 s |\n\n\
                     - first item\n- second item\n\nSee [the plan](https://example.com/plan).",
                ))
            }
        }
        cx.update(gpui_kit::init);
        let (window, _) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Doc)).unwrap()
        });
        let mut cx = VisualTestContext::from_window(window, cx);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let painted: Vec<String> = gpui_kit::base::test_support::texts(window)
                .iter()
                .map(|t| t.text().to_string())
                .collect();
            let all = painted.join("\n");
            for expected in ["Heading", "Before", "41 s", "first item", "the plan"] {
                assert!(
                    all.contains(expected),
                    "{expected:?} not painted in {all:?}"
                );
            }
        });
    }

    fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
        cx.update(|window, cx| {
            window.pointer_down(from, MouseButton::Left, cx);
            window.pointer_move(to, Some(MouseButton::Left), cx);
            window.pointer_up(to, MouseButton::Left, cx);
        });
        cx.run_until_parked();
    }
}
