//! The composer: the card under the timeline where the user writes. A multiline input that grows
//! to a maximum height and then scrolls (Enter sends, Shift+Enter is a new line, Ctrl+Enter steers
//! a running turn), a footer with the permission mode, how full the context is, the model and
//! effort, and the round send button (Stop while a turn runs and there is nothing to send), and,
//! above the card, the prompts queued behind a running turn, each removable (and "Send now",
//! "Edit"). The draft is kept per thread: this view tells the workspace what was typed
//! ([`ComposerViewEvent::Draft`]) and starts from what was kept.
//!
//! The view reads its [`ComposerModel`] and asks it to do things; it never calls the core. A send
//! that fails leaves the text in the input and shows a toast; the input is cleared only when the
//! model says the send went through.
//!
//! t3code: `apps/web/src/components/chat/ChatComposer.tsx` (the card: `rounded-2xl`, body
//! `px-4 pt-4 pb-2`, the footer row), `ComposerSurface.tsx` (the surface and its outline),
//! `ComposerPrimaryActions.tsx` (the round `size-8` send, queue and stop buttons, tooltips),
//! `ComposerFooterControls.tsx` (the permissions menu: a row of icon segments, the shown mode's
//! name and description below), `ComposerModelEffort.tsx` and `ModelPickerContent.tsx` (the
//! picker: a search box and the models on the left, "Reasoning effort" on the right),
//! `QueuedRunsControl.tsx` (the queued messages above the composer: first two of a long queue,
//! "Show all N queued", send now, edit, remove), `ComposerPromptEditorTiptap.tsx` (`max-h-52`,
//! two lines at least), `DraftHeroHeadline.tsx` (the new-thread screen). References:
//! `app-thread-dark.png` and `app-empty-dark.png` (the card, the footer, the send button),
//! `composer-model-picker.png` (the picker) and `composer-model-picker-permission.png` (the
//! permissions menu).
//!
//! Not copied: the speech button, the attach button and the context row under the card (the
//! checkout, the branch; attachments are CP5.2, the branch is in the header), the stash menu.
//! The context meter is a chip in the footer: t3code shows it in the thread details card.

use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity,
    EntityInputHandler as _, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, MouseDownEvent, ParentElement as _, Pixels, Point,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    assets::IconName,
    base,
    component::{
        Icon, Sizable as _, Size as KitSize,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
    },
    div,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::data::{
    EFFORTS, MODELS_SHOWN, Mode, context_label, effort_label, sendable, visible_queue,
};
use super::model::{Choices, ComposerEvent, ComposerModel};
use crate::design::{
    Button,
    button::{Size, Variant, icon_button},
    icons::icon,
    toast::{Kind, ToastSpec},
    tokens::{Palette, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The most rows the input shows before it scrolls (t3code's `max-h-52`, 208 px, at 14 px text
/// with a 1.625 line height).
pub const MAX_ROWS: usize = 9;
/// The fewest rows (`min-h-[calc(2lh + ...)]`).
pub const MIN_ROWS: usize = 2;
/// The composer's widest (the timeline's content lane, `--chat-content-max-width`).
pub const LANE: f32 = crate::thread::view::LANE;
/// The model picker's size.
const PICKER_WIDTH: f32 = 560.;
const PICKER_HEIGHT: f32 = 340.;

gpui_kit::actions!(
    hermes_composer,
    [
        /// Escape in an open composer menu: close it.
        CloseMenu
    ]
);

/// The key context of an open menu (active while the focus is inside it).
pub const MENU_CONTEXT: &str = "ComposerMenu";

/// Registers the composer's key binding.
pub fn bind(cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "escape",
        CloseMenu,
        Some(MENU_CONTEXT),
    )]);
}

/// Which menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Model,
    Mode,
}

/// What the composer tells the frame around it.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerViewEvent {
    /// The input's text changed: keep it as the draft of `key` (empty: forget it).
    Draft { key: String, text: String },
    /// A notice to show as a toast.
    Notice(ToastSpec),
}

/// The composer of one thread (or of the new-thread screen).
pub struct ComposerView {
    model: Entity<ComposerModel>,
    /// The draft's key (the thread's id, or `new:<project>`).
    key: String,
    input: Entity<TextareaState>,
    search: Entity<InputState>,
    focus: FocusHandle,
    menu: Option<Menu>,
    /// The menu that closed itself on a mouse-down outside it, and where that press went down:
    /// the click that the same press makes on the menu's chip must not open it again.
    closed_by_outside: Option<(Menu, Point<Pixels>)>,
    /// The mode whose description the menu shows (hovered), if not the current one.
    preview: Option<Mode>,
    /// The text the input held before a queued prompt was loaded into it to be edited.
    stash: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerViewEvent> for ComposerView {}

impl Focusable for ComposerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ComposerView {
    /// The composer over `model`, starting with `draft` in the input.
    pub fn new(
        model: Entity<ComposerModel>,
        key: String,
        draft: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(MIN_ROWS, MAX_ROWS)
                .submit_on_enter(true)
                .placeholder("Ask anything...")
        });
        if let Some(draft) = draft.filter(|d| !d.is_empty()) {
            input.update(cx, |input, cx| input.set_value(draft, window, cx));
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe_in(&model, window, Self::on_model_event),
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.subscribe_in(&search, window, Self::on_search_event),
        ];
        let view = Self {
            model,
            key,
            input,
            search,
            focus: cx.focus_handle(),
            menu: None,
            closed_by_outside: None,
            preview: None,
            stash: None,
            _subscriptions: subscriptions,
        };
        view.focus_input(window, cx);
        view
    }

    /// The model.
    pub fn model(&self) -> &Entity<ComposerModel> {
        &self.model
    }

    /// The input's state.
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    /// The model picker's search field.
    pub fn search(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The draft key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The open menu.
    pub fn open_menu(&self) -> Option<Menu> {
        self.menu
    }

    /// What the input holds.
    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Puts the cursor in the input.
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Replaces what the input holds (without telling anyone but the draft).
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        // `set_value` is not a change the input announces: the draft follows it here.
        cx.emit(ComposerViewEvent::Draft {
            key: self.key.clone(),
            text: text.to_string(),
        });
        cx.notify();
    }

    // --- events -------------------------------------------------------------------------------

    fn on_input_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                cx.emit(ComposerViewEvent::Draft {
                    key: self.key.clone(),
                    text,
                });
                cx.notify();
            }
            InputEvent::PressEnter { secondary, shift } => {
                // Shift+Enter is a new line (the input inserted it); an Enter that ends an IME
                // composition belongs to the IME, not to us.
                if *shift && !*secondary {
                    return;
                }
                let composing = input.update(cx, |input, cx| {
                    input.marked_text_range(window, cx).is_some()
                });
                if composing {
                    return;
                }
                self.submit(*secondary, window, cx);
            }
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    fn on_search_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => cx.notify(),
            // Enter takes the first match, as t3code's picker does.
            InputEvent::PressEnter { .. } => {
                let first = self.matches(cx).first().cloned();
                if let Some((provider, model)) = first {
                    self.pick_model(&provider, &model, window, cx);
                }
            }
            _ => {}
        }
    }

    fn on_model_event(
        &mut self,
        _: &Entity<ComposerModel>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::Sent { text } => {
                // Cleared unless the user already wrote something else meanwhile.
                if self.text(cx) == *text {
                    self.set_text("", window, cx);
                }
                self.restore_stash(window, cx);
            }
            ComposerEvent::Failed { text, why } => {
                if self.text(cx).is_empty() {
                    self.set_text(text, window, cx);
                }
                cx.emit(ComposerViewEvent::Notice(
                    ToastSpec::new(Kind::Error, "Could not send the message")
                        .description(why.clone()),
                ));
            }
            ComposerEvent::Restore { text, why } => {
                let current = self.text(cx);
                let restored = if current.trim().is_empty() {
                    text.clone()
                } else {
                    format!("{current}\n\n{text}")
                };
                self.set_text(&restored, window, cx);
                cx.emit(ComposerViewEvent::Notice(
                    ToastSpec::new(Kind::Warning, "A queued message could not start")
                        .description(format!("It is back in the composer. {why}")),
                ));
            }
            ComposerEvent::Edit { text } => {
                if self.stash.is_none() {
                    self.stash = Some(self.text(cx));
                }
                self.set_text(text, window, cx);
                self.focus_input(window, cx);
            }
            ComposerEvent::EditEnded => {
                // Sent as a replacement: `Sent` clears the input; cancelled: give back the draft.
                let stash = self.stash.take();
                if let Some(stash) = stash {
                    self.set_text(&stash, window, cx);
                }
            }
            ComposerEvent::Problem { title, why } => {
                cx.emit(ComposerViewEvent::Notice(
                    ToastSpec::new(Kind::Error, title.clone()).description(why.clone()),
                ));
            }
            ComposerEvent::Created { .. } => {
                // The text moves to the new thread's first send: this draft is done.
                cx.emit(ComposerViewEvent::Draft {
                    key: self.key.clone(),
                    text: String::new(),
                });
            }
        }
    }

    /// After an edit was sent or cancelled: the draft that was in the input comes back.
    fn restore_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(stash) = self.stash.take() {
            self.set_text(&stash, window, cx);
        }
    }

    /// Enter (or Ctrl+Enter for `steer`) or the send button.
    pub fn submit(&mut self, steer: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let text = self.text(cx);
        self.model.update(cx, |m, cx| m.send(&text, steer, cx));
    }

    /// The stop button.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.stop(cx));
    }

    // --- the menus ----------------------------------------------------------------------------

    /// A click on a menu's chip: opens the menu, or closes it when it is open. The open menu
    /// closes itself when the mouse goes down outside it (on the chip too, in the capture phase,
    /// before the chip sees the press): the click of that same press, recognised by where it went
    /// down, leaves the menu closed instead of opening it again.
    fn chip_clicked(
        &mut self,
        menu: Menu,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let down = match event {
            ClickEvent::Mouse(click) => Some(click.down.position),
            _ => None,
        };
        let closed_by_this_press = self
            .closed_by_outside
            .take()
            .is_some_and(|(closed, at)| closed == menu && Some(at) == down);
        if closed_by_this_press {
            return;
        }
        self.toggle_menu(menu, window, cx);
    }

    /// Opens `menu`, or closes it when it is open.
    pub fn toggle_menu(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu == Some(menu) {
            self.close_menu(window, cx);
            return;
        }
        self.menu = Some(menu);
        self.preview = None;
        match menu {
            Menu::Model => {
                self.model.update(cx, |m, cx| m.load_choices(cx));
                self.search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                self.search
                    .update(cx, |search, cx| search.focus(window, cx));
            }
            Menu::Mode => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    /// Closes the open menu and puts the cursor back in the input.
    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.preview = None;
            self.focus_input(window, cx);
            cx.notify();
        }
    }

    fn close_by_outside_click(&mut self, down: &MouseDownEvent, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu.take() {
            self.closed_by_outside = Some((menu, down.position));
            self.preview = None;
            cx.notify();
        }
    }

    /// The models matching the search, as `(provider, model)`.
    fn matches(&self, cx: &App) -> Vec<(String, String)> {
        let model = self.model.read(cx);
        let Choices::Ready(choices) = model.choices() else {
            return Vec::new();
        };
        let controls = model.controls();
        let query = self.search.read(cx).value().to_string();
        choices
            .filter(&query, Some((&controls.provider, &controls.model)))
            .into_iter()
            .take(MODELS_SHOWN)
            .map(|e| (e.provider.clone(), e.model.clone()))
            .collect()
    }

    /// Picks a model; the picker closes.
    pub fn pick_model(
        &mut self,
        provider: &str,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model
            .update(cx, |m, cx| m.pick_model(provider, model, cx));
        self.close_menu(window, cx);
    }

    /// Picks an effort (the picker stays open: t3code's lists effort beside the models).
    pub fn pick_effort(&mut self, effort: &str, cx: &mut Context<Self>) {
        self.model
            .update(cx, |m, cx| m.pick_effort(Some(effort), cx));
    }

    /// Picks a mode; the menu closes.
    pub fn pick_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.pick_mode(mode, cx));
        self.close_menu(window, cx);
    }
}

fn mode_icon(mode: Mode) -> IconName {
    match mode {
        Mode::Manual => IconName::Lock,
        Mode::Plan => IconName::ClipboardList,
        Mode::AcceptEdits => IconName::Pencil,
        Mode::Auto => IconName::ShieldCheck,
        Mode::Yolo => IconName::ShieldOff,
    }
}

// --- drawing --------------------------------------------------------------------------------------

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (palette, dark) = {
            let theme = cx.design_theme();
            (theme.palette, theme.appearance.is_dark())
        };
        let (controls, running, sending, editing, queue, show_all) = {
            let model = self.model.read(cx);
            (
                model.controls().clone(),
                model.running(),
                model.sending(),
                model.editing().map(str::to_string),
                model.queue().into_iter().cloned().collect::<Vec<_>>(),
                model.show_all_queued(),
            )
        };
        let has_text = sendable(&self.input.read(cx).value());

        // The surface and its 1 px outline (`--chat-composer-outline`: white at 5% in the dark,
        // black at 8% in the light; the theme's foreground at those strengths, so another theme's
        // outline follows its text colour). On the dark canvas it is #1d1d1d around #111111,
        // t3code's #1a1a1a around #0e0e0e (`app-empty-dark.png`): the same step.
        let outline = palette
            .foreground
            .hsla()
            .opacity(if dark { 0.05 } else { 0.08 });
        let surface = palette.surface_raised.over(palette.background).hsla();

        let primary =
            self.primary_button(running, has_text, sending, editing.is_some(), &palette, cx);

        let footer = div()
            .id("composer-footer")
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(12.))
            .pb(px(12.))
            .min_w_0()
            .child(self.mode_menu(&controls, &palette, cx))
            .child(div().flex_1().min_w(px(4.)))
            .when_some(controls.context, |row, (used, max)| {
                row.child(context_meter(used, max, &palette))
            })
            .child(self.model_menu(&controls, &palette, window, cx))
            .child(div().ml(px(4.)).flex_shrink_0().child(primary))
            .probe();

        let card = div()
            .id("composer")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .rounded(radius::XL2)
            .border_1()
            .border_color(outline)
            .bg(surface)
            .child(
                div()
                    .id("composer-body")
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(8.))
                    .text_size(text::SM.size)
                    .text_color(palette.foreground.hsla())
                    .child(
                        div()
                            .id("composer-input")
                            .w_full()
                            .child(Textarea::new(&self.input).appearance(false).bordered(false))
                            .probe(),
                    ),
            )
            .child(footer)
            .probe();

        div()
            .id("composer-area")
            .flex()
            .flex_col()
            .items_center()
            .w_full()
            .px(px(16.))
            .pb(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(LANE))
                    .when(!queue.is_empty(), |d| {
                        d.child(self.queue_strip(
                            &queue,
                            show_all,
                            editing.as_deref(),
                            &palette,
                            cx,
                        ))
                    })
                    .child(card),
            )
            .probe()
    }
}

impl ComposerView {
    /// The send, queue and stop button (t3code's round `size-8` button).
    fn primary_button(
        &self,
        running: bool,
        has_text: bool,
        sending: bool,
        editing: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Stop when a turn runs and there is nothing to send; else send (queue while running).
        let stop = running && !has_text && !editing;
        let (id, glyph, tip, disabled): (&'static str, IconName, &str, bool) = if stop {
            ("composer-stop", IconName::Square, "Stop generating", false)
        } else if editing {
            (
                "composer-send",
                IconName::Check,
                "Update the queued message",
                !has_text || sending,
            )
        } else if running {
            (
                "composer-send",
                IconName::ListPlus,
                "Queue message (Enter). Ctrl+Enter sends it into the running turn.",
                !has_text || sending,
            )
        } else {
            (
                "composer-send",
                IconName::ArrowUp,
                "Send message",
                !has_text || sending,
            )
        };
        let (bg, fg, hover) = if disabled {
            (
                palette.foreground.hsla().opacity(0.12),
                palette.muted_foreground.hsla(),
                palette.foreground.hsla().opacity(0.12),
            )
        } else {
            (
                palette.message_action.hsla(),
                palette.message_action_foreground.hsla(),
                palette.message_action_hover.hsla(),
            )
        };
        let view = cx.entity();
        let tip: SharedString = tip.to_string().into();
        let tip_view = tip.clone();
        div()
            .child(
                base::Button::new(id)
                    .tab_stop(true)
                    .disabled(disabled)
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .w(px(32.))
                    .h(px(32.))
                    .rounded_full()
                    .bg(bg)
                    .text_color(fg)
                    .when(!disabled, |b| b.hover(move |s| s.bg(hover)))
                    .accessibility_label(tip)
                    .tooltip(move |window, cx| {
                        crate::design::tooltip::view(tip_view.clone(), None, window, cx)
                    })
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            if stop {
                                this.stop(cx)
                            } else {
                                this.submit(false, window, cx)
                            }
                        });
                    })
                    .child(
                        Icon::new(glyph)
                            .with_size(KitSize::Size(px(16.)))
                            .text_color(fg),
                    ),
            )
            .into_any_element()
    }

    // --- the queue ----------------------------------------------------------------------------

    fn queue_strip(
        &self,
        queue: &[super::data::QueuedItem],
        expanded: bool,
        editing: Option<&str>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = visible_queue(queue, expanded);
        let rows = shown.iter().enumerate().map(|(ix, item)| {
            let (model, view) = (self.model.clone(), cx.entity());
            let id = item.id.clone();
            let is_editing = editing == Some(item.id.as_str());
            let bubble = div()
                .id(ElementId::Name(format!("queued-text-{ix}").into()))
                .max_w(relative(0.8))
                .px(px(12.))
                .py(px(8.))
                .rounded(radius::XL2)
                .border_1()
                .border_dashed()
                .border_color(palette.border.hsla())
                .bg(palette.message_surface.hsla())
                .text_color(palette.message_foreground.hsla())
                .text_size(text::SM.size)
                .line_height(text::SM.line_height)
                .line_clamp(2)
                .child(SharedString::from(item.text.clone()))
                .probe();
            let controls: AnyElement = if is_editing {
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(text::XS.size)
                            .text_color(palette.foreground.hsla())
                            .child("Editing in the composer"),
                    )
                    .child({
                        let model = model;
                        Button::new(
                            ElementId::Name(format!("queued-cancel-{ix}").into()),
                            "Cancel",
                        )
                        .variant(Variant::Ghost)
                        .size(Size::Xs)
                        .on_click(move |_, _, cx| model.update(cx, |m, cx| m.cancel_edit(cx)))
                    })
                    .into_any_element()
            } else {
                let (send_id, edit_id, remove_id) = (id.clone(), id.clone(), id);
                let (m1, m2, m3) = (model.clone(), model.clone(), model);
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        Button::new(
                            ElementId::Name(format!("queued-send-{ix}").into()),
                            "Send now",
                        )
                        .icon(IconName::ArrowUp)
                        .variant(Variant::GhostMuted)
                        .size(Size::Xs)
                        .tooltip("Send into the current turn")
                        .on_click(move |_, _, cx| {
                            m1.update(cx, |m, cx| m.steer_queued(&send_id, cx))
                        }),
                    )
                    .child(
                        icon_button(
                            ElementId::Name(format!("queued-edit-{ix}").into()),
                            IconName::Pencil,
                            "Edit in the composer",
                        )
                        .variant(Variant::GhostMuted)
                        .size(Size::Xs)
                        .on_click(move |_, _, cx| {
                            m2.update(cx, |m, cx| m.begin_edit(&edit_id, cx))
                        }),
                    )
                    .child(
                        icon_button(
                            ElementId::Name(format!("queued-remove-{ix}").into()),
                            IconName::Close,
                            "Remove from queue",
                        )
                        .variant(Variant::GhostMuted)
                        .size(Size::Xs)
                        .on_click(move |_, _, cx| {
                            m3.update(cx, |m, cx| m.remove_queued(&remove_id, cx))
                        }),
                    )
                    .into_any_element()
            };
            let _ = view;
            div()
                .id(ElementId::Name(format!("queued-{ix}").into()))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .w_full()
                .child(controls)
                .child(bubble)
                .probe()
        });
        let toggle = (queue.len() > super::data::QUEUE_VISIBLE_ROWS).then(|| {
            let model = self.model.clone();
            let label = if expanded {
                "Show fewer".to_string()
            } else {
                format!("Show all {} queued", queue.len())
            };
            div().flex().justify_end().child(
                Button::new("queue-toggle", label)
                    .icon(if expanded {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .variant(Variant::GhostMuted)
                    .size(Size::Xs)
                    .on_click(move |_, _, cx| {
                        model.update(cx, |m, cx| m.toggle_show_all_queued(cx))
                    }),
            )
        });
        div()
            .id("composer-queue")
            .flex()
            .flex_col()
            .items_end()
            .gap(px(8.))
            .pb(px(12.))
            .children(rows)
            .children(toggle)
            .probe()
            .into_any_element()
    }

    // --- the mode menu ------------------------------------------------------------------------

    fn mode_menu(
        &self,
        controls: &super::data::Controls,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu == Some(Menu::Mode);
        let view = cx.entity();
        let chip = chip(
            "mode-chip",
            mode_icon(controls.mode),
            controls.mode.label().into(),
            open,
            format!(
                "Permissions: {}. {}",
                controls.mode.label(),
                controls.mode.description()
            ),
            palette,
            move |event, window, cx| {
                view.update(cx, |this, cx| this.chip_clicked(Menu::Mode, event, window, cx))
            },
        );
        div()
            .id("mode-menu")
            .relative()
            .flex()
            .flex_shrink_0()
            .child(chip)
            .when(open, |d| {
                d.child(self.mode_panel(controls.mode, palette, cx))
            })
            .into_any_element()
    }

    fn mode_panel(&self, current: Mode, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.preview.unwrap_or(current);
        let segments = Mode::ALL.map(|mode| {
            let view = cx.entity();
            let hover = cx.entity();
            let selected = mode == current;
            div()
                .id(ElementId::Name(format!("mode-option-{}", mode.id()).into()))
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .h(px(24.))
                .cursor_pointer()
                .rounded(radius::MD)
                .text_color(if selected {
                    palette.foreground.hsla()
                } else {
                    palette.muted_foreground.hsla()
                })
                .when(selected, |d| d.bg(palette.input.hsla().opacity(0.72)))
                .when(!selected, |d| {
                    d.hover(|s| {
                        s.bg(palette.accent.hsla())
                            .text_color(palette.foreground.hsla())
                    })
                })
                .on_hover(move |hovered, _, cx| {
                    let hovered = *hovered;
                    hover.update(cx, |this, cx| {
                        let next = hovered.then_some(mode);
                        if this.preview != next {
                            this.preview = next;
                            cx.notify();
                        }
                    });
                })
                .on_click(move |_, window, cx| {
                    view.update(cx, |this, cx| this.pick_mode(mode, window, cx))
                })
                .child(icon(mode_icon(mode)).with_size(KitSize::Size(px(14.))))
                .probe()
        });
        let out = cx.entity();
        div()
            .id("mode-panel")
            .key_context(MENU_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseMenu, window, cx| this.close_menu(window, cx)))
            .on_mouse_down_out(move |down: &MouseDownEvent, _, cx| {
                out.update(cx, |this, cx| this.close_by_outside_click(down, cx))
            })
            .occlude()
            .absolute()
            .bottom(relative(1.))
            .left_0()
            .mb(px(8.))
            .w(px(288.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(8.))
            .py(px(6.))
            .rounded(radius::XL)
            .border_1()
            // t3code's popovers are `dropdown-glass` (the panel tokens), not `--border`.
            .border_color(palette.panel_edge.hsla())
            .bg(palette.panel_surface.over(palette.background).hsla())
            .text_color(palette.popover_foreground.hsla())
            .child(
                div()
                    .text_size(text::XS.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.muted_foreground.hsla())
                    .child("Permissions"),
            )
            .child(
                div()
                    .id("mode-options")
                    .flex()
                    .p(px(2.))
                    .rounded(radius::LG)
                    .bg(palette.input.hsla().opacity(0.4))
                    .children(segments),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .id("mode-name")
                            .text_size(text::SM.size)
                            .text_color(if shown == current {
                                palette.foreground.hsla()
                            } else {
                                palette.muted_foreground.hsla()
                            })
                            .child(shown.label())
                            .probe_text(shown.label()),
                    )
                    .child(
                        div()
                            .id("mode-description")
                            .min_h(px(32.))
                            .text_size(text::XS.size)
                            .line_height(px(16.))
                            .text_color(palette.muted_foreground.hsla())
                            .child(shown.description()),
                    ),
            )
            .probe()
            .into_any_element()
    }

    // --- the model menu -----------------------------------------------------------------------

    fn model_menu(
        &self,
        controls: &super::data::Controls,
        palette: &Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu == Some(Menu::Model);
        let view = cx.entity();
        let label = format!("{} {}", controls.model_label(), controls.effort_text());
        let chip = chip(
            "model-chip",
            IconName::Brain,
            label.clone().into(),
            open,
            if controls.provider_name.is_empty() {
                format!("{label}: model and reasoning effort")
            } else {
                format!(
                    "{}, {label}: model and reasoning effort",
                    controls.provider_name
                )
            },
            palette,
            move |event, window, cx| {
                view.update(cx, |this, cx| this.chip_clicked(Menu::Model, event, window, cx))
            },
        );
        div()
            .id("model-menu")
            .relative()
            .flex()
            .min_w(px(96.))
            .flex_shrink(1.)
            .child(chip)
            .when(open, |d| d.child(self.model_panel(controls, palette, cx)))
            .into_any_element()
    }

    fn model_panel(
        &self,
        controls: &super::data::Controls,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let matches = self.matches(cx);
        let status = match self.model.read(cx).choices() {
            Choices::Idle | Choices::Loading => {
                Some("Asking the endpoints for their models...".to_string())
            }
            Choices::Failed(why) => Some(format!("Could not list the models: {why}")),
            Choices::Ready(_) if matches.is_empty() => Some("No model matches.".to_string()),
            Choices::Ready(_) => None,
        };
        let current = (controls.provider.clone(), controls.model.clone());
        let rows = matches
            .into_iter()
            .enumerate()
            .map(|(ix, (provider, model))| {
                let selected = (provider.clone(), model.clone()) == current;
                let view = cx.entity();
                let (pick_provider, pick_model) = (provider.clone(), model.clone());
                div()
                    .id(ElementId::Name(format!("model-option-{ix}").into()))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(12.))
                    .flex_shrink_0()
                    .rounded(radius::MD)
                    .cursor_pointer()
                    .text_size(text::SM.size)
                    .text_color(palette.foreground.hsla())
                    .hover(|s| s.bg(palette.accent.hsla()))
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.pick_model(&pick_provider, &pick_model, window, cx)
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(SharedString::from(model)),
                    )
                    .child(
                        div()
                            .text_size(text::XXS2.size)
                            .text_color(palette.muted_foreground.hsla())
                            .child(SharedString::from(provider)),
                    )
                    .when(selected, |d| {
                        d.child(icon(IconName::Check).with_size(KitSize::Size(px(14.))))
                    })
                    .probe()
            });
        let picked = if controls.effort.is_empty() {
            super::data::DEFAULT_EFFORT
        } else {
            controls.effort.as_str()
        };
        let efforts = EFFORTS.map(|(id, label)| {
            let view = cx.entity();
            let selected = id == picked;
            div()
                .id(ElementId::Name(format!("effort-{id}").into()))
                .flex()
                .items_center()
                .justify_between()
                .h(px(32.))
                .px(px(12.))
                .rounded(radius::MD)
                .cursor_pointer()
                .text_size(text::SM.size)
                .text_color(palette.foreground.hsla())
                .hover(|s| s.bg(palette.accent.hsla()))
                .on_click(move |_, _, cx| view.update(cx, |this, cx| this.pick_effort(id, cx)))
                .child(label)
                .when(selected, |d| {
                    d.child(icon(IconName::Check).with_size(KitSize::Size(px(14.))))
                })
                .probe()
        });
        let out = cx.entity();
        div()
            .id("model-panel")
            .key_context(MENU_CONTEXT)
            .on_action(cx.listener(|this, _: &CloseMenu, window, cx| this.close_menu(window, cx)))
            .on_mouse_down_out(move |down: &MouseDownEvent, _, cx| {
                out.update(cx, |this, cx| this.close_by_outside_click(down, cx))
            })
            .occlude()
            .absolute()
            .bottom(relative(1.))
            .right_0()
            .mb(px(8.))
            .flex()
            .w(px(PICKER_WIDTH))
            .h(px(PICKER_HEIGHT))
            .overflow_hidden()
            .rounded(radius::XL)
            .border_1()
            // t3code's popovers are `dropdown-glass` (the panel tokens), not `--border`.
            .border_color(palette.panel_edge.hsla())
            .bg(palette.panel_surface.over(palette.background).hsla())
            .text_color(palette.popover_foreground.hsla())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("model-search")
                            .px(px(8.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(palette.border.hsla())
                            .child(Input::new(&self.search).appearance(false).bordered(false))
                            .probe(),
                    )
                    .child(
                        div()
                            .id("model-list")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .p(px(4.))
                            .overflow_y_scroll()
                            .children(rows)
                            .when_some(status, |d, status| {
                                d.child(
                                    div()
                                        .id("model-status")
                                        .px(px(12.))
                                        .py(px(8.))
                                        .text_size(text::XS.size)
                                        .text_color(palette.muted_foreground.hsla())
                                        .child(status),
                                )
                            })
                            .probe(),
                    ),
            )
            .child(
                div()
                    .id("effort-list")
                    .flex()
                    .flex_col()
                    .w(px(180.))
                    .flex_shrink_0()
                    .p(px(4.))
                    .border_l_1()
                    .border_color(palette.border.hsla())
                    .child(
                        div()
                            .px(px(12.))
                            .py(px(8.))
                            .text_size(text::XS.size)
                            .text_color(palette.muted_foreground.hsla())
                            .child(format!("Reasoning effort: {}", effort_label(picked))),
                    )
                    .children(efforts)
                    .probe(),
            )
            .probe()
            .into_any_element()
    }
}

/// The new-thread screen (`DraftHeroHeadline.tsx`): the headline, what `extras` adds under it (the
/// way out of the project) and the composer, centred together in the main area.
pub fn hero(
    palette: &Palette,
    headline: String,
    extras: Option<AnyElement>,
    composer: Option<Entity<ComposerView>>,
) -> AnyElement {
    div()
        .id("new-thread-screen")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .size_full()
        .child(
            div()
                .id("new-thread-headline")
                .px(px(16.))
                .pb(px(8.))
                .text_size(text::XL2.size)
                .line_height(text::XL2.line_height)
                .text_color(palette.foreground.hsla())
                .child(headline)
                .probe(),
        )
        .children(extras)
        .child(div().h(px(8.)))
        .children(composer)
        .probe()
        .into_any_element()
}

/// A footer chip (t3code's `ComposerControl`, 28 px, muted until hovered): an icon and a label
/// that is cut with an ellipsis when the footer is narrow (a long model name must not push the
/// send button out of the card).
fn chip(
    id: &'static str,
    glyph: IconName,
    label: SharedString,
    selected: bool,
    tooltip: String,
    palette: &Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let (fg, hover_bg, hover_fg) = (
        palette.muted_foreground.hsla(),
        palette.accent.hsla(),
        palette.foreground.hsla(),
    );
    let tip: SharedString = tooltip.into();
    base::Button::new(id)
        .tab_stop(true)
        .selected(selected)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(8.))
        .min_w_0()
        .w_full()
        .overflow_hidden()
        .rounded(radius::MD)
        .text_size(text::SM.size)
        .text_color(fg)
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .styles(move |s| s.selected(move |s| s.bg(hover_bg).text_color(hover_fg)))
        .accessibility_label(tip.clone())
        .tooltip(move |window, cx| crate::design::tooltip::view(tip.clone(), None, window, cx))
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .child(
            div()
                .flex_shrink_0()
                .child(icon(glyph).with_size(KitSize::Size(px(16.)))),
        )
        .child(
            div()
                .id("label")
                .min_w_0()
                .truncate()
                .child(label.clone())
                .probe_text(label),
        )
        .into_any_element()
}

/// How full the context is: a thin bar and the percentage (t3code's thread details show
/// `18k / 200k` and a bar; the tooltip has the numbers).
fn context_meter(used: usize, max: usize, palette: &Palette) -> AnyElement {
    let ratio = if max == 0 {
        0.
    } else {
        (used as f32 / max as f32).clamp(0., 1.)
    };
    let tip: SharedString = context_label(used, max).into();
    let fill = if ratio >= 0.9 {
        palette.warning.hsla()
    } else {
        palette.muted_foreground.hsla()
    };
    div()
        .id("context-meter")
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .h(px(28.))
        .text_size(text::XXS2.size)
        .text_color(palette.muted_foreground.hsla())
        .tooltip(move |window, cx| crate::design::tooltip::view(tip.clone(), None, window, cx))
        .child(
            div()
                .id("context-bar")
                .w(px(36.))
                .h(px(4.))
                .rounded_full()
                .bg(palette.muted_foreground.hsla().opacity(0.25))
                .child(
                    div()
                        .h_full()
                        .w(relative(ratio.max(0.02)))
                        .rounded_full()
                        .bg(fill),
                ),
        )
        .probe()
        .into_any_element()
}
