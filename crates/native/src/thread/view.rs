//! The thread view: the open thread's timeline in the main area. A virtual list (GPUI Kit's
//! `MessageScroller`, GPUI's `list` with tail following) of rows in t3code's content lane
//! (46rem wide, centred): user messages on the message surface at the right, replies as markdown,
//! reasoning collapsed under "Thought for 2.8s", a placeholder line per tool call (the cards are
//! CP4.2), notices. It stays pinned to the newest row while a turn streams unless the user scrolled
//! up; then t3code's "Scroll to end" button brings it back. Coming near the top loads an older page.
//!
//! Hovering a message shows its time and a copy button (the message as markdown); a code block's
//! header copies the code. Text selects inside a message and inside code (Kit's selection; across
//! messages is not built: the copy buttons cover it, docs/native-ui/README.md, *Risks*).
//!
//! The view reads its [`ThreadModel`] and asks it to do things (load older, toggle a reasoning
//! row, send the dev prompt); it never calls the core. In a `--dev` app (and only there) a strip
//! under the timeline sends a prompt, until Phase 5's composer.
//!
//! t3code: `apps/web/src/components/ChatView.tsx` and `chat/ChatCanvas.tsx` (the main area),
//! `chat/MessagesTimeline.tsx` (the rows, older history), `chat/TimelineOverlays.tsx`
//! (`ScrollToEndButton`),
//! `chat/TimelineUserMessageRow.tsx` (`bg-message`, `rounded-2xl`, `p-3`, `max-w-[80%]`; the
//! attachment grid `max-w-[210px] grid-cols-2 gap-2`, `aspect-[4/3] rounded-lg border`; actions
//! under the bubble on hover), `chat/TimelineAssistantRows.tsx` (the reply and its hover footer),
//! `chat/TimelineWorkEntryRow.tsx` (the reasoning row), `index.css` (`--chat-content-max-width:
//! 46rem`). References: `app-thread-dark.png` (the thread), `app-hover_reply_closeup-dark.png`
//! (the reply's hover footer: copy, time), `app-codeblock_closeup-dark.png` (code).

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    ImageSource, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _, StyledImage as _,
    Subscription, Window,
    assets::IconName,
    component::ActiveTheme as _,
    component::Sizable as _,
    component::Size as KitSize,
    component::input::{Input, InputEvent, InputState},
    component::message_scroller::MessageScroller,
    component::text::TextView,
    div, img,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::data::reasoning_label;
use super::markdown;
use super::model::{Body, Row, RowKind, ThreadModel, ThreadPhase, Thumb};
use crate::design::{
    Button, EmptyState,
    button::{self, Variant, icon_button},
    icons::icon,
    tokens::text,
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// t3code's `--chat-content-max-width` (46rem).
pub const LANE: f32 = 736.;
/// An older page loads when the list's top row is this close to the first loaded row.
pub const LOAD_MARGIN: usize = 8;
/// The space between two rows.
const ROW_GAP: f32 = 12.;

/// The open thread's view.
pub struct ThreadView {
    model: Entity<ThreadModel>,
    focus: FocusHandle,
    dev_input: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ThreadView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ThreadView {
    /// The view of `model`; `dev` adds the dev-only prompt strip.
    pub fn new(
        model: Entity<ThreadModel>,
        dev: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let scroller = model.read(cx).scroller().clone();
        let mut subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            // The list notifies on every scroll: redraw (the jump button) and load older rows
            // when the top comes near.
            cx.observe(&scroller, |this, _, cx| {
                this.maybe_load_older(cx);
                cx.notify();
            }),
            // A theme can change the fonts and their sizes: the heights the list measured for
            // rows out of view are stale then (it re-measures on a width change by itself).
            cx.observe_global::<crate::theme::Active>(|this, cx| {
                let scroller = this.model.read(cx).scroller().clone();
                scroller.update(cx, |list, cx| list.remeasure(cx));
            }),
        ];
        let dev_input = dev.then(|| {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(
                    "Dev only: send a prompt (the thread's provider, or --dev-provider)",
                )
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let text = input.read(cx).value().to_string();
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        this.model.update(cx, |model, cx| model.submit(&text, cx));
                    }
                },
            ));
            input
        });
        Self {
            model,
            focus: cx.focus_handle(),
            dev_input,
            _subscriptions: subscriptions,
        }
    }

    /// The thread's model.
    pub fn model(&self) -> &Entity<ThreadModel> {
        &self.model
    }

    /// The dev prompt's input, when there is one.
    pub fn dev_input(&self) -> Option<&Entity<InputState>> {
        self.dev_input.as_ref()
    }

    /// Asks for an older page when the top of the list is near the first loaded row.
    pub fn maybe_load_older(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let top = model.scroller().read(cx).logical_scroll_top().item_ix;
        if top < LOAD_MARGIN && model.has_older() && !model.loading_older() {
            self.model.update(cx, |model, cx| model.load_older(cx));
        }
    }
}

impl Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let model = self.model.read(cx);
        let phase = model.phase().clone();
        let scroller = model.scroller().clone();
        let scrolled_up = scroller.read(cx).is_scrolled_up();

        let timeline = match phase {
            ThreadPhase::Failed(why) => {
                EmptyState::new("thread-failed", "Could not open this thread")
                    .icon(IconName::CircleAlert)
                    .description(why)
                    .into_any_element()
            }
            // Local data: no spinner while the first page is read (it takes a frame or two).
            ThreadPhase::Loading | ThreadPhase::Ready => {
                let rows = self.model.clone();
                let list =
                    MessageScroller::new("timeline-list", scroller, move |ix, window, cx| {
                        render_row(&rows, ix, window, cx)
                    })
                    .jump_button(false)
                    .with_row_style(StyleRefinement::default().pb(px(ROW_GAP)).px(px(16.)))
                    .with_list_style(StyleRefinement::default().pt(px(20.)).pb(px(24.)));
                let model = self.model.clone();
                div()
                    .id("timeline")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .child(list)
                    .when(scrolled_up, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(16.))
                                .flex()
                                .justify_center()
                                // t3code's `ScrollToEndButton` (`TimelineOverlays.tsx`): an
                                // `xs` button with a chevron, floating over the rows.
                                .child(
                                    Button::new("jump-to-latest", "Scroll to end")
                                        .icon(IconName::ChevronDown)
                                        .variant(Variant::Outline)
                                        .size(button::Size::Xs)
                                        .on_click(move |_, _, cx| {
                                            model.update(cx, |m, cx| m.scroll_to_end(cx));
                                        }),
                                ),
                        )
                    })
                    .probe()
                    .into_any_element()
            }
        };

        div()
            .id("thread")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(palette.background.hsla())
            .text_color(palette.foreground.hsla())
            // t3code's chat text: `text-sm` (14 px).
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .child(timeline)
            .when_some(self.dev_input.clone(), |d, input| {
                d.child(
                    div()
                        .id("dev-prompt")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(palette.warning.hsla())
                        .bg(palette.warning_surface.hsla())
                        .child(
                            div()
                                .text_size(text::XXS2.size)
                                .line_height(text::XXS2.line_height)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.warning.hsla())
                                .child("DEV ONLY"),
                        )
                        .child(div().flex_1().child(Input::new(&input)))
                        .probe(),
                )
            })
            .probe()
    }
}

/// The time a message was written, as t3code's hover shows it (`6:25 PM`, local time).
pub fn time_label(at_ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|t| t.format("%-I:%M %p").to_string())
        .unwrap_or_default()
}

/// One row, in the content lane.
fn render_row(model: &Entity<ThreadModel>, ix: usize, _: &mut Window, cx: &mut App) -> AnyElement {
    let Some(row) = model.read(cx).rows().get(ix).cloned() else {
        return div().into_any_element();
    };
    let key = row.key as usize;
    let content = match &row.kind {
        RowKind::User { text, images } => user_row(model, &row, text, images, cx),
        RowKind::Assistant { body } => assistant_row(model, &row, body, cx),
        RowKind::Reasoning {
            body,
            duration_ms,
            open,
            live,
        } => reasoning_row(model, &row, body, *duration_ms, *open, *live, cx),
        RowKind::Tool {
            name,
            summary,
            done,
        } => tool_row(&row, name, summary, *done, cx),
        RowKind::Notice { error, text } => notice_row(&row, *error, text, cx),
    };
    div()
        .id(("row", key))
        .w_full()
        .flex()
        .justify_center()
        .child(
            div()
                .id(("lane", key))
                .w_full()
                .max_w(px(LANE))
                .min_w_0()
                .child(content)
                .probe(),
        )
        .probe()
        .into_any_element()
}

/// The markdown of a message body.
fn body_view(body: &Body, id: (&'static str, usize), cx: &App) -> TextView {
    let view = match body {
        Body::Text(text) => TextView::markdown(id, text.clone()),
        Body::Live(state) => TextView::new(state),
    };
    markdown::message(view, cx)
}

/// The time and the copy button under a message, shown while the pointer is over it.
fn message_actions(
    model: &Entity<ThreadModel>,
    row: &Row,
    group: &'static str,
    align_end: bool,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let key = row.key;
    let copy_from = model.clone();
    div()
        .id(("actions", key as usize))
        .flex()
        .items_center()
        .gap(px(8.))
        .when(align_end, |d| d.justify_end().pr(px(4.)))
        .text_size(text::XS.size)
        .line_height(text::XS.line_height)
        .text_color(palette.muted_foreground.hsla())
        .opacity(0.)
        .group_hover(group, |s| s.opacity(1.))
        .child(
            icon_button(
                ("copy-message", key as usize),
                IconName::Copy,
                "Copy message",
            )
            .variant(Variant::GhostMuted)
            .size(button::Size::Xs)
            .on_click(move |_, _, cx| {
                if let Some(text) = copy_from.read(cx).copy_text(key, cx) {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                }
            }),
        )
        .children(row.at_ms.map(time_label))
        .probe()
        .into_any_element()
}

fn user_row(
    model: &Entity<ThreadModel>,
    row: &Row,
    text: &SharedString,
    images: &[Thumb],
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let key = row.key as usize;
    let thumbs = (!images.is_empty()).then(|| {
        div()
            .id(("thumbs", key))
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .mb(px(8.))
            .max_w(px(210.))
            .children(images.iter().enumerate().map(|(i, thumb)| {
                div()
                    .id(("thumb", i))
                    .w(px(101.))
                    .h(px(76.))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(palette.border.with_alpha(204).hsla())
                    .bg(palette.background.with_alpha(178).hsla())
                    .child(
                        img(ImageSource::Image(thumb.image.clone()))
                            .size_full()
                            .object_fit(ObjectFit::Cover),
                    )
                    .tooltip({
                        let name = thumb.name.clone();
                        move |window, cx| {
                            crate::design::tooltip::view(name.clone(), None, window, cx)
                        }
                    })
                    .probe()
            }))
            .probe()
    });
    div()
        .id(("user", key))
        .group("user-row")
        .flex()
        .flex_col()
        .items_end()
        .gap(px(4.))
        .child(
            div()
                .id(("user-bubble", key))
                .max_w(relative(0.8))
                .min_w_0()
                .p(px(12.))
                .rounded(px(16.))
                .bg(palette.message_surface.hsla())
                .text_color(palette.message_foreground.hsla())
                .when_some(thumbs, |d, thumbs| d.child(thumbs))
                .when(!text.is_empty(), |d| {
                    d.child(markdown::message(
                        TextView::markdown(("user-text", key), text.clone()),
                        cx,
                    ))
                })
                .probe(),
        )
        .child(message_actions(model, row, "user-row", true, cx))
        .probe()
        .into_any_element()
}

fn assistant_row(model: &Entity<ThreadModel>, row: &Row, body: &Body, cx: &App) -> AnyElement {
    let key = row.key as usize;
    div()
        .id(("assistant", key))
        .group("assistant-row")
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .id(("assistant-body", key))
                .min_w_0()
                .child(body_view(body, ("message", key), cx))
                .probe(),
        )
        .child(message_actions(model, row, "assistant-row", false, cx))
        .probe()
        .into_any_element()
}

fn reasoning_row(
    model: &Entity<ThreadModel>,
    row: &Row,
    body: &Body,
    duration_ms: Option<u64>,
    open: bool,
    live: bool,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let key = row.key;
    let toggle = model.clone();
    let muted = palette.muted_foreground.hsla();
    div()
        .id(("reasoning", key as usize))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .id(("reasoning-toggle", key as usize))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .text_size(text::SM.size)
                .line_height(text::SM.line_height)
                .text_color(muted)
                .hover(|s| s.text_color(palette.foreground.hsla()))
                .child(icon(IconName::Brain).with_size(KitSize::Size(px(14.))))
                .child(reasoning_label(live, duration_ms))
                .child(
                    icon(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(KitSize::Size(px(14.))),
                )
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |m, cx| m.toggle_reasoning(key, cx));
                })
                .probe(),
        )
        .when(open, |d| {
            d.child(
                div()
                    .id(("reasoning-body", key as usize))
                    .pl(px(20.))
                    .border_l_1()
                    .border_color(palette.border.hsla())
                    .ml(px(6.))
                    .text_color(muted)
                    .child(body_view(body, ("reasoning-text", key as usize), cx).text_color(muted))
                    .probe(),
            )
        })
        .probe()
        .into_any_element()
}

fn tool_row(
    row: &Row,
    name: &SharedString,
    summary: &SharedString,
    done: bool,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    div()
        .id(("tool", row.key as usize))
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w_0()
        .text_size(text::SM.size)
        .line_height(text::SM.line_height)
        .text_color(palette.muted_foreground.hsla())
        .child(
            icon(if done {
                IconName::Wrench
            } else {
                IconName::LoaderCircle
            })
            .with_size(KitSize::Size(px(14.))),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(palette.secondary_label.hsla())
                .child(name.clone()),
        )
        .when(!summary.is_empty(), |d| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(text::XS.size)
                    .child(summary.clone()),
            )
        })
        .probe()
        .into_any_element()
}

fn notice_row(row: &Row, error: bool, text_: &SharedString, cx: &App) -> AnyElement {
    let palette = cx.design_theme().palette;
    let color = if error {
        palette.destructive
    } else {
        palette.muted_foreground
    };
    div()
        .id(("notice", row.key as usize))
        .flex()
        .items_start()
        .gap(px(6.))
        .text_size(text::SM.size)
        .line_height(text::SM.line_height)
        .text_color(color.hsla())
        .when(error, |d| {
            d.child(
                div().pt(px(3.)).child(
                    icon(IconName::CircleAlert)
                        .with_size(KitSize::Size(px(14.)))
                        .text_color(color.hsla()),
                ),
            )
        })
        .child(div().min_w_0().child(text_.clone()))
        .probe()
        .into_any_element()
}
