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
//! CP5.2 adds, around the same input: the `@` and `/` menus ([`super::suggest`]: the drawer above
//! the card, mentions as atomic chips in the text), the strip of attachments above the text
//! ([`super::strip`]; paste, drop and the paperclip all end in the model's `add_attachments`),
//! and the commands of the app (`/new`, `/model`, ...), which the view does itself or hands to the
//! frame as [`ComposerViewEvent::App`]. t3code: `ComposerCommandMenu.tsx`,
//! `ComposerAttachmentStrip.tsx`, `useComposerAttachmentIntake.ts` (paste and drop).
//!
//! Not copied: the speech button and the context row under the card (the checkout, the branch;
//! the branch is in the header), the stash menu. The context meter is a chip in the footer:
//! t3code shows it in the thread details card.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardEntry, Context, ElementId, Entity,
    EntityInputHandler as _, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight,
    ImageFormat, InteractiveElement as _, IntoElement, MouseDownEvent, ParentElement as _, Pixels,
    Point, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window,
    assets::IconName,
    base,
    component::{
        Icon, Sizable as _, Size as KitSize,
        input::{
            Enter, Escape, IndentInline, Input, InputContent, InputEvent, InputState, MoveDown,
            MoveUp, Paste, Textarea, TextareaState,
        },
    },
    div,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::ask::Nav;
use super::data::{
    EFFORTS, MODELS_SHOWN, Mode, context_label, effort_label, sendable, visible_queue,
};
use super::mention::{self, Trigger};
use super::model::{Choices, ComposerEvent, ComposerModel};
use super::strip;
use super::suggest::{self, Key};
use crate::design::{
    Button,
    button::{Size, Variant, icon_button},
    icons::icon,
    toast::{Kind, ToastSpec},
    tokens::{Palette, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::model::backend::AttachmentSource;
use crate::theme::DesignTheme as _;

/// The most rows the input shows before it scrolls (t3code's `max-h-52`, 208 px, at 14 px text
/// with a 1.625 line height).
pub const MAX_ROWS: usize = 9;
/// The fewest rows (`min-h-[calc(2lh + ...)]`).
pub const MIN_ROWS: usize = 2;
/// The composer's widest (the timeline's content lane, `--chat-content-max-width`).
pub const LANE: f32 = crate::thread::view::LANE;
/// What the input says while it is empty: t3code's "Ask anything. @ files, $ skills, / commands"
/// without the `$` part (skills are commands here: `/name`).
pub const PLACEHOLDER: &str = "Ask anything. @ files, / commands";
/// The file dialog of the paperclip, behind a seam so a test answers without a window: it gives
/// the paths picked (`None`: cancelled).
pub type FilePicker = Rc<dyn Fn(&mut App) -> Task<Option<Vec<PathBuf>>>>;
/// The narrowest the model chip gets: its icon (the name is shown from [`LABEL_MIN`] of room).
pub const MODEL_FLOOR: f32 = 32.;
/// The narrowest the mode chip gets: its icon alone.
pub const MODE_FLOOR: f32 = 32.;
/// The least room a footer chip's label is shown in (about four letters and the ellipsis);
/// with less, the chip is its icon.
pub const LABEL_MIN: f32 = 36.;
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
    /// An app command the user typed (`/new`, `/settings`, `/terminal`): the frame's command id
    /// (`keymap::COMMANDS`).
    App(&'static str),
}

/// The composer of one thread (or of the new-thread screen).
pub struct ComposerView {
    pub(super) model: Entity<ComposerModel>,
    /// The draft's key (the thread's id, or `new:<project>`).
    key: String,
    pub(super) input: Entity<TextareaState>,
    search: Entity<InputState>,
    focus: FocusHandle,
    pub(super) menu: Option<Menu>,
    /// The menu that closed itself on a mouse-down outside it, and where that press went down:
    /// the click that the same press makes on the menu's chip must not open it again.
    closed_by_outside: Option<(Menu, Point<Pixels>)>,
    /// The mode whose description the menu shows (hovered), if not the current one.
    preview: Option<Mode>,
    /// The text the input held before a queued prompt was loaded into it to be edited.
    stash: Option<String>,
    /// What the caret asks a menu for now (`@` entries, the commands, a command's arguments).
    pub(super) trigger: Option<Trigger>,
    /// The trigger Escape put away: the menu stays closed until the trigger changes.
    pub(super) dismissed: Option<(u8, usize)>,
    /// The highlighted row of the drawer.
    pub(super) highlight: usize,
    pub(super) list_scroll: ScrollHandle,
    /// A paste is on its way into the input: its references become chips when its change lands
    /// (a paste that changes nothing leaves this set until the next edit, which then makes chips
    /// of the references in the text: harmless).
    pasting: bool,
    /// Where the keyboard is in the request the thread waits on (CP5.3, [`super::panel`]).
    pub(super) nav: Nav,
    /// The request `nav` belongs to.
    pub(super) ask_for: Option<String>,
    /// When it appeared (the executor's clock): its keys answer nothing for
    /// [`super::panel::KEY_GUARD`].
    pub(super) ask_shown_at: Option<std::time::Instant>,
    /// The panel's focus: its keys work while it has it.
    pub(super) ask_focus: FocusHandle,
    /// A question's free-text answer.
    pub(super) answer_input: Entity<TextareaState>,
    /// What the input says while it is empty (it changes while the agent waits).
    pub(super) input_hint: &'static str,
    file_picker: FilePicker,
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
                .placeholder(PLACEHOLDER)
        });
        if let Some(draft) = draft.filter(|d| !d.is_empty()) {
            // A draft is plain text; the references in it are chips again.
            input.update(cx, |input, cx| {
                input.set_value(content_of(&draft), window, cx)
            });
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        let answer_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .submit_on_enter(true)
                .placeholder(super::panel::FIELD_HINT)
        });
        let subscriptions = vec![
            // A request arriving or closing moves the keyboard (the panel takes it, or gives it back).
            cx.observe_in(&model, window, |this, _, window, cx| {
                this.sync_ask(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&answer_input, window, Self::on_answer_event),
            cx.observe(&answer_input, |_, _, cx| cx.notify()),
            cx.subscribe_in(&model, window, Self::on_model_event),
            cx.subscribe_in(&input, window, Self::on_input_event),
            // The caret moving is no change of the text, but it moves the trigger.
            cx.observe(&input, |this, _, cx| this.sync_trigger(cx)),
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
            trigger: None,
            dismissed: None,
            highlight: 0,
            list_scroll: ScrollHandle::new(),
            pasting: false,
            nav: Nav::default(),
            ask_for: None,
            ask_shown_at: None,
            ask_focus: cx.focus_handle(),
            answer_input,
            input_hint: PLACEHOLDER,
            file_picker: real_file_picker(),
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

    /// What the input says while it is empty.
    pub fn input_hint(&self) -> &str {
        self.input_hint
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
            input.set_value(content_of(text), window, cx)
        });
        // `set_value` is not a change the input announces: the draft follows it here.
        cx.emit(ComposerViewEvent::Draft {
            key: self.key.clone(),
            text: text.to_string(),
        });
        cx.notify();
    }

    /// The input changed by a call, not a key: the draft and the menus follow.
    pub(super) fn edited(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        cx.emit(ComposerViewEvent::Draft {
            key: self.key.clone(),
            text,
        });
        self.sync_trigger(cx);
        cx.notify();
    }

    /// Makes the references in the text chips again (after a paste).
    fn retokenise(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        self.input.update(cx, |input, cx| {
            let _ = input.set_tokens(content_of(&text), cx);
        });
    }

    /// Puts the caret at the end of the text and reads what it asks for (stories: a menu open
    /// over a draft).
    pub fn caret_to_end(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            let end = input.value().len();
            input.set_selected_range(end..end, cx);
        });
        self.sync_trigger(cx);
    }

    /// Replaces the file dialog (a test's).
    pub fn set_file_picker(&mut self, picker: FilePicker) {
        self.file_picker = picker;
    }

    // --- attachments --------------------------------------------------------------------------

    /// The paperclip: asks for files, which are attached once chosen.
    pub fn pick_files(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let picked = (self.file_picker)(cx);
        cx.spawn(async move |this, cx| {
            if let Some(paths) = picked.await {
                let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
            }
        })
        .detach();
    }

    /// Attaches files (dropped, picked or pasted from Explorer).
    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let sources = paths.into_iter().map(AttachmentSource::Path).collect();
        self.model
            .update(cx, |m, cx| m.add_attachments(sources, cx));
    }

    /// What Ctrl+V finds on the clipboard: an image or files with no text are attached (the
    /// paste is taken); anything else is the input's own paste, and the references in it become
    /// chips when it lands. True when the paste was taken.
    pub fn paste_attachments(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            self.pasting = true;
            return false;
        };
        // A spreadsheet cell puts a picture beside its text: the text is what was meant.
        if item.text().is_some_and(|t| !t.trim().is_empty()) {
            self.pasting = true;
            return false;
        }
        let mut sources = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let mime = match image.format {
                        ImageFormat::Jpeg => "image/jpeg",
                        ImageFormat::Gif => "image/gif",
                        ImageFormat::Webp => "image/webp",
                        ImageFormat::Bmp => "image/bmp",
                        ImageFormat::Tiff => "image/tiff",
                        _ => "image/png",
                    };
                    sources.push(AttachmentSource::Bytes {
                        data: std::sync::Arc::new(image.bytes.clone()),
                        name: super::attach::pasted_name(mime),
                    });
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    sources.extend(paths.paths().iter().cloned().map(AttachmentSource::Path))
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if sources.is_empty() {
            self.pasting = true;
            return false;
        }
        self.model
            .update(cx, |m, cx| m.add_attachments(sources, cx));
        true
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
                if std::mem::take(&mut self.pasting) {
                    self.retokenise(cx);
                }
                let text = input.read(cx).value().to_string();
                cx.emit(ComposerViewEvent::Draft {
                    key: self.key.clone(),
                    text,
                });
                self.sync_trigger(cx);
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

    fn on_answer_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { secondary, shift } => {
                if *shift && !*secondary {
                    return;
                }
                let composing = input.update(cx, |input, cx| {
                    input.marked_text_range(window, cx).is_some()
                });
                if composing {
                    return;
                }
                let current = self.model.read(cx).current_ask().map(|r| r.prompt_id.clone());
                if let Some(id) = current {
                    self.send_field(&id, window, cx);
                }
            }
            InputEvent::Focus | InputEvent::Blur => {}
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
            ComposerEvent::Notice { title, why } => {
                cx.emit(ComposerViewEvent::Notice(
                    ToastSpec::new(Kind::Info, title.clone()).description(why.clone()),
                ));
            }
            ComposerEvent::Answer { command, text } => {
                cx.emit(ComposerViewEvent::Notice(
                    ToastSpec::new(Kind::Info, command.clone()).description(text.clone()),
                ));
            }
            ComposerEvent::App(command) => match command.workspace_id() {
                Some(id) => cx.emit(ComposerViewEvent::App(id)),
                // The model picker is this composer's own.
                None => {
                    if self.menu != Some(Menu::Model) {
                        self.toggle_menu(Menu::Model, window, cx);
                    }
                }
            },
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
        if let Some(trigger) = &self.trigger {
            self.dismissed = Some(trigger.key());
        }
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
        let (has_attachments, preparing) = {
            let model = self.model.read(cx);
            (!model.attachments().is_empty(), model.preparing() > 0)
        };
        // Attachments alone can be sent; not while one is still being read.
        let has_text = (sendable(&self.input.read(cx).value()) || has_attachments) && !preparing;
        let strip = self.attachment_strip(&palette, cx);
        let drawer = self.suggest_drawer(&palette, window, cx);
        let ask_panel = self.ask_panel(&palette, dark, cx);

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
            .child(self.attach_button(&palette, cx))
            .child(self.mode_menu(&controls, &palette, cx))
            .child(div().flex_1().min_w(px(4.)))
            .when_some(controls.context, |row, (used, max)| {
                row.child(context_meter(used, max, &palette))
            })
            .child(self.model_menu(&controls, &palette, window, cx))
            .child(div().ml(px(4.)).flex_shrink_0().child(primary))
            .probe();

        let ring = palette.ring.hsla();
        let dropped = cx.entity();
        let card = div()
            .id("composer")
            .relative()
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(ring))
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                let paths = paths.paths().to_vec();
                dropped.update(cx, |this, cx| this.attach_paths(paths, cx));
            })
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
                    .children(strip)
                    .child(
                        div()
                            .id("composer-input")
                            .w_full()
                            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                                if this.suggest_key(Key::Up, window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                                if this.suggest_key(Key::Down, window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, enter: &Enter, window, cx| {
                                // Enter picks; Shift+Enter and Ctrl+Enter are the input's.
                                if !enter.secondary
                                    && !enter.shift
                                    && this.suggest_key(Key::Enter, window, cx)
                                {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                                if this.suggest_key(Key::Tab, window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                                if this.suggest_key(Key::Escape, window, cx) || this.escape_ask(window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                                if this.paste_attachments(cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                Textarea::new(&self.input)
                                    .token(strip::chip_element)
                                    .appearance(false)
                                    .bordered(false),
                            )
                            .probe(),
                    ),
            )
            .child(footer)
            .children(drawer)
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
                    .children(ask_panel)
                    .child(card),
            )
            .probe()
    }
}

impl ComposerView {
    /// The paperclip at the left of the footer.
    fn attach_button(&self, _palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        icon_button(
            "attach-button",
            IconName::Paperclip,
            "Attach files or images",
        )
        .variant(Variant::GhostMuted)
        .size(Size::Sm)
        .on_click(move |_, window, cx| view.update(cx, |this, cx| this.pick_files(window, cx)))
        .into_any_element()
    }

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
                view.update(cx, |this, cx| {
                    this.chip_clicked(Menu::Mode, event, window, cx)
                })
            },
        );
        div()
            .id("mode-menu")
            .relative()
            .flex()
            // In a narrow footer the model chip gives way first (to its floor); only in a
            // footer narrower still does the mode's name, down to its icon (the tooltip names
            // it), so the send button never leaves the card.
            .min_w(px(MODE_FLOOR))
            .flex_shrink(0.001)
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
                view.update(cx, |this, cx| {
                    this.chip_clicked(Menu::Model, event, window, cx)
                })
            },
        );
        div()
            .id("model-menu")
            .relative()
            .flex()
            // It shrinks first (the long names are models'): its name is cut, then, with less
            // than `LABEL_MIN` for it, hidden (the icon stays; the tooltip names the model);
            // only then does the mode chip shrink.
            .min_w(px(MODEL_FLOOR))
            .flex_shrink(64.)
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
/// send button out of the card). With less than [`LABEL_MIN`] for the label, the label goes and
/// the chip is its icon (the tooltip names it), so a narrow footer never shows a lone `o...`.
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
            // The label's room: it wraps, with an empty first item, so a label that needs more
            // than the room has falls to a second line, which the 28 px clip.
            div()
                .flex_grow(1.)
                .min_w_0()
                .h(px(28.))
                .overflow_hidden()
                .flex()
                .flex_wrap()
                .content_start()
                .child(div().w(px(0.)).h(px(28.)))
                .child(
                    div()
                        .id("label")
                        .flex_basis(relative(1.))
                        .min_w(px(LABEL_MIN))
                        .h(px(28.))
                        .line_height(px(28.))
                        .truncate()
                        .child(label.clone())
                        .probe_text(label),
                ),
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

/// The text with the references in it as chips: what a draft, a restored prompt or a paste loads.
pub fn content_of(text: &str) -> InputContent {
    let mut content = InputContent::new(text.to_string());
    for (range, chip) in mention::reference_chips(text) {
        if let Ok(next) = content.clone().with_token(range, suggest::token_of(&chip)) {
            content = next;
        }
    }
    content
}

/// The paperclip's dialog: the system's file picker, several files.
fn real_file_picker() -> FilePicker {
    Rc::new(|cx: &mut App| {
        let asked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |_| asked.await.ok().and_then(|r| r.ok()).flatten())
    })
}
