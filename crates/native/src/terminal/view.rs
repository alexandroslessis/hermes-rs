//! One terminal on screen: the grid of an [`Emulator`] drawn with GPUI, and the keyboard, mouse,
//! IME and clipboard going back to the shell.
//!
//! - **Drawing.** The view keeps a [`Screen`] (the visible cells, colours resolved) between
//!   frames and brings it up to date under the emulator's lock only when the pump says something
//!   changed, rebuilding only the rows the grid's damage names. A frame paints background runs, the
//!   selection, the cursor (block, beam or underline, hollow when unfocused, blinking when the
//!   program asks), then each row's text: runs of single-width cells shaped as one line with every
//!   glyph forced onto the cell grid (`force_width`), double-width (CJK, emoji) characters on their
//!   own two cells, and box-drawing / block characters as rectangles ([`super::boxdraw`]).
//! - **Size.** The grid follows the element's bounds: cols and rows from the cell size of the
//!   theme's monospace font (Consolas, else Cascadia Mono, as the code blocks), the emulator resized
//!   at once and the shell told after [`RESIZE_DEBOUNCE`] (a drag sends one resize, not fifty).
//! - **Keys.** The view has its own key context, `Terminal` (see [`super::bind`] for what the app
//!   keeps and what reaches the shell). Keys that type text go through the input handler (so the
//!   IME works: its preedit is drawn at the cursor); the others are encoded by
//!   [`super::input::encode_key`]. Typing jumps to the bottom and drops the selection.
//! - **Mouse.** A full-screen program that asked for the mouse gets reports (Shift overrides, as
//!   in every terminal); otherwise drag selects, a double click a word, a triple click a line,
//!   Shift+click extends; the wheel scrolls the scrollback (or sends arrows to a program on the
//!   alternate screen). Ctrl+click opens a link: a web address (`http(s)` only) in the browser, a
//!   file in the Files tab (the app's [`OpenFileInPanel`]); nothing is run. Only links visible in
//!   the text open: a program's OSC 8 hyperlink (whose target may differ from its text) is not
//!   followed.
//! - **Copy / paste.** `Ctrl+Shift+C` copies; `Ctrl+C` copies when there is a selection, else is
//!   ^C; `Ctrl+Shift+V` and `Ctrl+V` paste (bracketed when the program turned it on, with every
//!   ESC taken out of the text so it cannot end the bracket). Programs cannot use the clipboard
//!   (OSC 52 is refused).
//!
//! t3code: `apps/web/src/components/ThreadTerminalDrawer.tsx` (`TerminalViewport`: the surface,
//! its theme from the app's tokens, `Ctrl/Cmd+click` opening links, the font size 12). Reference:
//! `app-terminal-open.png` (the prompt on the black canvas, the bar cursor block).

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui_kit::{
    App, AppContext as _, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, FontFallbacks, FontStyle,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton as GpuiButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render,
    ScrollEvent, SharedString, StrikethroughStyle, Styled as _, Subscription, Task, TextAlign,
    TextRun, UTF16Selection, UnderlineStyle, Window, assets::IconName, canvas, div, fill, font,
    point, prelude::FluentBuilder as _, px, size,
};

use super::boxdraw;
use super::colors::TermColors;
use super::emulator::{Emulator, Screen, attrs};
use super::input::{self, KeyModes, Mods, MouseAction, MouseButton, MouseModes};
use super::links::{self, Link};
use super::session::{self, Pty, Shared};
use super::{
    CloseTerminal, Copy, CopyOrInterrupt, NewTerminal, Paste, ScrollPageDown, ScrollPageUp,
    ScrollToBottom, ScrollToTop,
};
use crate::{
    design::{
        ToastSpec,
        icons::icon,
        tokens::{radius, text},
    },
    dev::ProbeExt as _,
    model::AppModel,
    theme::{DesignTheme as _, Fonts},
    thread::OpenFileInPanel,
};

/// t3code's terminal font size (`DEFAULT_TERMINAL_FONT_SIZE`).
pub const FONT_SIZE: f32 = 12.;
/// Space between the panel's edge and the first column / row.
pub const PAD_X: f32 = 6.;
pub const PAD_Y: f32 = 4.;
/// How long a size must hold before the shell is told.
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
/// The cursor's blink half-period.
pub const BLINK: Duration = Duration::from_millis(530);
/// How long the bell's flash lasts.
pub const BELL_FLASH: Duration = Duration::from_millis(150);
/// The key context of a focused terminal.
pub const CONTEXT: &str = "Terminal";

/// What the bell does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BellStyle {
    /// The terminal flashes briefly.
    Flash,
    None,
}

/// The cell size and the font, for one font size.
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl Metrics {
    /// The theme's monospace font at [`FONT_SIZE`]: the cell is the width of `m` and 1.2 lines.
    pub fn measure(window: &Window, cx: &mut App) -> Self {
        let family = Fonts::install(cx).mono;
        let mut font = font(family);
        font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            "Cascadia Mono".into(),
            "Segoe UI Emoji".into(),
            "Segoe UI Symbol".into(),
            "MS Gothic".into(),
            "Malgun Gothic".into(),
        ]));
        let font_size = px(FONT_SIZE);
        let text = window.text_system();
        let id = text.resolve_font(&font);
        let cell_w = text
            .advance(id, font_size, 'm')
            .map(|s| s.width.as_f32())
            .unwrap_or(FONT_SIZE * 0.55);
        let ascent = text.ascent(id, font_size).as_f32();
        let descent = text.descent(id, font_size).as_f32().abs();
        let cell_h = (ascent + descent).ceil().max((FONT_SIZE * 1.2).round());
        Self {
            font,
            font_size,
            cell_w,
            cell_h,
        }
    }
}

/// Where the grid is on screen (set while the frame is laid out).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLayout {
    pub origin: Point<Pixels>,
    pub cell_w: f32,
    pub cell_h: f32,
    pub cols: usize,
    pub rows: usize,
}

/// Where the shell is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Life {
    Starting,
    Running,
    Failed(String),
    Exited,
}

/// Content a story puts in a terminal once it knows its grid: `(emulator, cols, rows)`.
pub type Script = Box<dyn FnOnce(&mut Emulator, usize, usize)>;

/// What a terminal tells its group.
#[derive(Clone, Debug)]
pub enum TerminalEvent {
    /// The title (OSC 0 / 2) changed.
    Title,
    /// The shell ended (`exit`, or it was closed).
    Exited,
    /// `Ctrl+Shift+T`.
    NewRequested,
    /// `Ctrl+Shift+W`.
    CloseRequested,
    Notice(ToastSpec),
}

/// One terminal.
pub struct TerminalView {
    /// The thread it belongs to.
    session: String,
    /// "Terminal 2": its number in the thread.
    number: usize,
    shared: Arc<Shared>,
    pty: Option<Arc<dyn Pty>>,
    life: Life,
    focus: FocusHandle,
    screen: Rc<Screen>,
    metrics: Option<Metrics>,
    layout: Option<GridLayout>,
    /// The size the shell was last told.
    pty_size: Option<(u16, u16)>,
    resize_task: Option<Task<()>>,
    title: Option<String>,
    shell_name: Option<String>,
    bells_seen: u64,
    bell: BellStyle,
    flash_until: Option<Instant>,
    flash_task: Option<Task<()>>,
    blink_on: bool,
    blink_task: Option<Task<()>>,
    /// The IME's text being composed.
    marked: Option<String>,
    /// The link under the pointer while Ctrl is held: `(row, columns)`.
    hover_link: Option<(usize, Range<usize>)>,
    /// Wheel movement not yet a whole line.
    scroll_rest: f32,
    /// A drag is selecting.
    selecting: bool,
    /// A button held in a program that reports the mouse.
    reported_button: Option<MouseButton>,
    closed: bool,
    /// A story's scripted content, run once the grid size is known.
    script: Option<Script>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// A terminal for thread `session` whose shell the backend starts now (in the background),
    /// `cols` × `rows` until the view knows its size.
    pub fn spawn(
        app: Entity<AppModel>,
        session: String,
        number: usize,
        (cols, rows): (u16, u16),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::bare(session.clone(), number, (cols, rows), window, cx);
        let Some(backend) = app.read(cx).backend() else {
            view.life = Life::Failed("Hermes is still starting.".into());
            return view;
        };
        let spawning =
            cx.background_spawn(async move { backend.terminal_spawn(&session, cols, rows) });
        view.tasks.push(cx.spawn(async move |this, cx| {
            let result = spawning.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(pty) => view.attach(pty, cx),
                Err(why) => {
                    view.life = Life::Failed(why);
                    cx.notify();
                }
            });
        }));
        view
    }

    /// A terminal over `pty` (tests and the gallery).
    pub fn with_pty(
        pty: Arc<dyn Pty>,
        number: usize,
        (cols, rows): (u16, u16),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::bare(String::new(), number, (cols, rows), window, cx);
        view.attach(pty, cx);
        view
    }

    fn bare(
        session: String,
        number: usize,
        (cols, rows): (u16, u16),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let colors = TermColors::of(cx.design_theme());
        let emulator = Emulator::new(usize::from(cols), usize::from(rows), colors);
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_blur(&focus, window, |this, _, cx| this.focus_changed(false, cx)),
        ];
        Self {
            session,
            number,
            shared: Shared::new(emulator),
            pty: None,
            life: Life::Starting,
            focus,
            screen: Rc::new(Screen::default()),
            metrics: None,
            layout: None,
            pty_size: None,
            resize_task: None,
            title: None,
            shell_name: None,
            bells_seen: 0,
            bell: BellStyle::Flash,
            flash_until: None,
            flash_task: None,
            blink_on: true,
            blink_task: None,
            marked: None,
            hover_link: None,
            scroll_rest: 0.,
            selecting: false,
            reported_button: None,
            closed: false,
            script: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Starts feeding the shell's output (the pump on a background task) and drawing it.
    fn attach(&mut self, pty: Arc<dyn Pty>, cx: &mut Context<Self>) {
        if self.closed {
            // Closed while it was starting: end the shell that arrived late.
            cx.background_spawn(async move { pty.close() }).detach();
            return;
        }
        self.shell_name = Some(pty.name());
        self.pty = Some(pty.clone());
        self.life = Life::Running;
        self.tasks
            .push(cx.background_spawn(session::pump(self.shared.clone(), pty.clone())));
        let shared = self.shared.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                shared.wake.notified().await;
                let exited = shared.exited.load(Ordering::Acquire);
                if this.update(cx, |view, cx| view.on_output(cx)).is_err() || exited {
                    break;
                }
            }
        }));
        // The size it was spawned with may already be wrong.
        if let Some(layout) = self.layout {
            self.tell_pty(layout.cols as u16, layout.rows as u16, true, cx);
        }
        cx.notify();
    }

    /// The pump parsed more output: pick up the title, the bell, the end.
    fn on_output(&mut self, cx: &mut Context<Self>) {
        let (title, bells) = {
            let emulator = self.shared.emulator.lock();
            (emulator.title().map(str::to_string), emulator.bells())
        };
        if title != self.title {
            self.title = title;
            cx.emit(TerminalEvent::Title);
        }
        if bells > self.bells_seen {
            self.bells_seen = bells;
            self.ring(cx);
        }
        if self.shared.exited.load(Ordering::Acquire) && self.life != Life::Exited {
            self.life = Life::Exited;
            cx.emit(TerminalEvent::Exited);
        }
        cx.notify();
    }

    // --- what the group and tests read -------------------------------------------------------

    /// The tab's label: the program's title, else "Terminal N".
    pub fn label(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| format!("Terminal {}", self.number))
    }

    pub fn number(&self) -> usize {
        self.number
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn life(&self) -> &Life {
        &self.life
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn pty(&self) -> Option<&Arc<dyn Pty>> {
        self.pty.as_ref()
    }

    pub fn layout(&self) -> Option<GridLayout> {
        self.layout
    }

    pub fn metrics(&self) -> Option<&Metrics> {
        self.metrics.as_ref()
    }

    /// The screen as last drawn.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn is_flashing(&self) -> bool {
        self.flash_until.is_some_and(|t| t > Instant::now())
    }

    /// Runs `script` on the emulator once the grid's size is known (stories: content of the
    /// right size without waiting for a shell).
    pub fn script(&mut self, script: impl FnOnce(&mut Emulator, usize, usize) + 'static) {
        match self.layout {
            Some(l) => script(&mut self.shared.emulator.lock(), l.cols, l.rows),
            None => self.script = Some(Box::new(script)),
        }
    }

    pub fn set_bell(&mut self, bell: BellStyle) {
        self.bell = bell;
    }

    pub fn marked_text(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    pub fn hover_link(&self) -> Option<&(usize, Range<usize>)> {
        self.hover_link.as_ref()
    }

    /// The program's modes now.
    pub fn mode(&self) -> TermMode {
        self.shared.emulator.lock().mode()
    }

    // --- writing to the shell ----------------------------------------------------------------

    fn write(&self, bytes: &[u8]) {
        if let Some(pty) = &self.pty
            && self.life == Life::Running
        {
            pty.write(bytes);
        }
    }

    /// Input from the user: jumps to the bottom, drops the selection, shows the cursor.
    pub fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        {
            let mut emulator = self.shared.emulator.lock();
            emulator.clear_selection();
            if emulator.display_offset() > 0 {
                emulator.scroll(Scroll::Bottom);
            }
        }
        self.write(bytes);
        self.restart_blink(cx);
        cx.notify();
    }

    /// Text typed or pasted (not bracketed): line ends become Enter.
    fn input_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bytes = input::encode_paste(text, false);
        self.send_input(&bytes, cx);
    }

    pub fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        let bytes = input::encode_paste(text, bracketed);
        self.send_input(&bytes, cx);
    }

    /// Copies the selection; `false` when nothing is selected.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.shared.emulator.lock().selection_text();
        match text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy(cx);
    }

    /// `Ctrl+C`: copies (and drops) the selection when there is one, else interrupts.
    fn on_copy_or_interrupt(
        &mut self,
        _: &CopyOrInterrupt,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.copy(cx) {
            self.shared.emulator.lock().clear_selection();
            cx.notify();
        } else {
            self.send_input(&[0x03], cx);
        }
    }

    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste(&text, cx);
        }
    }

    pub fn scroll(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        self.shared.emulator.lock().scroll(scroll);
        cx.notify();
    }

    // --- size ----------------------------------------------------------------------------------

    /// The frame laid the grid out at `layout`: resize the emulator now, the shell soon.
    pub fn set_layout(&mut self, layout: GridLayout, cx: &mut Context<Self>) {
        if self.layout == Some(layout) {
            return;
        }
        let first = self.layout.is_none();
        self.layout = Some(layout);
        {
            let mut emulator = self.shared.emulator.lock();
            emulator.set_cell_size(layout.cell_w.round() as u16, layout.cell_h.round() as u16);
            emulator.resize(layout.cols, layout.rows);
        }
        self.tell_pty(layout.cols as u16, layout.rows as u16, first, cx);
        if let Some(script) = self.script.take() {
            let mut emulator = self.shared.emulator.lock();
            script(&mut emulator, layout.cols, layout.rows);
            self.title = emulator.title().map(str::to_string);
        }
        cx.notify();
    }

    fn tell_pty(&mut self, cols: u16, rows: u16, now: bool, cx: &mut Context<Self>) {
        if self.pty_size == Some((cols, rows)) {
            self.resize_task = None;
            return;
        }
        let Some(pty) = self.pty.clone() else {
            return;
        };
        if now {
            self.pty_size = Some((cols, rows));
            self.resize_task = None;
            pty.resize(cols, rows);
            return;
        }
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            let _ = this.update(cx, |view, _| {
                view.pty_size = Some((cols, rows));
                pty.resize(cols, rows);
            });
        }));
    }

    // --- bell, blink, focus ----------------------------------------------------------------------

    fn ring(&mut self, cx: &mut Context<Self>) {
        if self.bell != BellStyle::Flash {
            return;
        }
        self.flash_until = Some(Instant::now() + BELL_FLASH);
        self.flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BELL_FLASH).await;
            let _ = this.update(cx, |view, cx| {
                view.flash_until = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        if focused {
            self.restart_blink(cx);
        } else {
            self.blink_task = None;
            self.blink_on = true;
            self.marked = None;
        }
        cx.notify();
    }

    /// The cursor shows at once after input, then blinks (if the program asked for a blinking one).
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_on = true;
        let blinking = self.screen.cursor.is_some_and(|c| c.blinking);
        if !blinking {
            self.blink_task = None;
            return;
        }
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                if this
                    .update(cx, |view, cx| {
                        view.blink_on = !view.blink_on;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    // --- closing -----------------------------------------------------------------------------------

    /// Ends the shell and everything it started (in the background: walking the process table takes a moment).
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.resize_task = None;
        self.blink_task = None;
        if let Some(pty) = self.pty.clone() {
            cx.background_spawn(async move { pty.close() }).detach();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    // --- keys ------------------------------------------------------------------------------------

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        if m.platform || m.function {
            return;
        }
        let mods = Mods {
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
        };
        let modes = KeyModes {
            app_cursor: self.mode().contains(TermMode::APP_CURSOR),
        };
        if let Some(bytes) =
            input::encode_key(&keystroke.key, mods, keystroke.key_char.as_deref(), modes)
        {
            self.send_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    // --- mouse ------------------------------------------------------------------------------------

    /// The cell under window point `position`, and which half of it.
    pub fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let layout = self.layout?;
        let x = (position.x - layout.origin.x).as_f32() / layout.cell_w;
        let y = (position.y - layout.origin.y).as_f32() / layout.cell_h;
        let col = (x.max(0.) as usize).min(layout.cols.saturating_sub(1));
        let row = (y.max(0.) as usize).min(layout.rows.saturating_sub(1));
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some((col, row, side))
    }

    fn mouse_modes(&self) -> MouseModes {
        let mode = self.mode();
        MouseModes {
            click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    fn report(
        &mut self,
        button: MouseButton,
        action: MouseAction,
        position: Point<Pixels>,
        mods: Mods,
    ) -> bool {
        let modes = self.mouse_modes();
        if !modes.any() || mods.shift {
            return false;
        }
        if let Some((col, row, _)) = self.cell_at(position)
            && let Some(bytes) = input::encode_mouse(button, action, col, row, mods, modes)
        {
            self.write(&bytes);
        }
        true
    }

    fn mods_of(m: gpui_kit::Modifiers) -> Mods {
        Mods {
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
        }
    }

    /// The link at visible cell (`col`, `row`): a web address or a file path found in the row's
    /// text. A program's OSC 8 hyperlink is not followed: its target can differ from the text it
    /// is drawn on (`https://good.example` leading to another site), so only what is shown opens.
    pub fn link_at(&self, col: usize, row: usize) -> Option<(Link, Range<usize>)> {
        let emulator = self.shared.emulator.lock();
        let (text, columns) = emulator.line_text(row);
        let at = columns.iter().position(|c| *c >= col)?;
        let (link, chars) = links::link_at(&text, at)?;
        let first = *columns.get(chars.start)?;
        let last = columns
            .get(chars.end.saturating_sub(1))
            .copied()
            .unwrap_or(first);
        Some((link, first..last + 1))
    }

    /// Opens `link`: a web address in the browser, a file in the Files tab. Never runs anything.
    pub fn open_link(&mut self, link: &Link, window: &mut Window, cx: &mut Context<Self>) {
        match link {
            Link::Url(url) if url.starts_with("https://") || url.starts_with("http://") => {
                cx.open_url(url);
            }
            Link::Url(_) => {}
            Link::File { path, line } => {
                window.dispatch_action(
                    Box::new(OpenFileInPanel {
                        path: path.clone(),
                        line: *line,
                    }),
                    cx,
                );
            }
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let mods = Self::mods_of(event.modifiers);
        let button = match event.button {
            GpuiButton::Left => MouseButton::Left,
            GpuiButton::Middle => MouseButton::Middle,
            GpuiButton::Right => MouseButton::Right,
            _ => return,
        };
        let Some((col, row, side)) = self.cell_at(event.position) else {
            return;
        };
        if button == MouseButton::Left && mods.ctrl && !mods.shift {
            if let Some((link, _)) = self.link_at(col, row) {
                self.open_link(&link, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if self.report(button, MouseAction::Press, event.position, mods) {
            self.reported_button = Some(button);
            cx.stop_propagation();
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        {
            let mut emulator = self.shared.emulator.lock();
            let point = emulator.point_at(col, row);
            if mods.shift && emulator.has_anchor() {
                emulator.extend_selection(point, side);
            } else {
                let kind = match event.click_count {
                    2 => SelectionType::Semantic,
                    n if n >= 3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                emulator.start_selection(kind, point, side);
            }
        }
        self.selecting = true;
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mods = Self::mods_of(event.modifiers);
        // The link under the pointer while Ctrl is held.
        let hover = if mods.ctrl {
            self.cell_at(event.position)
                .and_then(|(col, row, _)| self.link_at(col, row).map(|(_, cols)| (row, cols)))
        } else {
            None
        };
        if hover != self.hover_link {
            self.hover_link = hover;
            cx.notify();
        }
        match event.pressed_button {
            Some(GpuiButton::Left) if self.selecting => {
                if let Some((col, row, side)) = self.cell_at(event.position) {
                    let mut emulator = self.shared.emulator.lock();
                    let point = emulator.point_at(col, row);
                    emulator.extend_selection(point, side);
                    drop(emulator);
                    cx.notify();
                }
            }
            pressed => {
                let held = self
                    .reported_button
                    .filter(|_| pressed.is_some())
                    .unwrap_or(MouseButton::None);
                if self.mouse_modes().any() && !mods.shift {
                    self.report(held, MouseAction::Motion, event.position, mods);
                }
            }
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(button) = self.reported_button.take() {
            let mods = Self::mods_of(event.modifiers);
            self.report(button, MouseAction::Release, event.position, mods);
        }
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else {
            return;
        };
        let delta = event.delta.pixel_delta(px(layout.cell_h)).y.as_f32() / layout.cell_h;
        let total = delta + self.scroll_rest;
        let lines = total.trunc() as i32;
        self.scroll_rest = total.fract();
        if lines == 0 {
            return;
        }
        let mods = Self::mods_of(event.modifiers);
        let mode = self.mode();
        let button = if lines > 0 {
            MouseButton::WheelUp
        } else {
            MouseButton::WheelDown
        };
        if self.mouse_modes().any() && !mods.shift {
            for _ in 0..lines.unsigned_abs() {
                self.report(button, MouseAction::Press, event.position, mods);
            }
        } else if mode.contains(TermMode::ALT_SCREEN)
            && mode.contains(TermMode::ALTERNATE_SCROLL)
            && !mods.shift
        {
            let key = if lines > 0 { "up" } else { "down" };
            let modes = KeyModes {
                app_cursor: mode.contains(TermMode::APP_CURSOR),
            };
            if let Some(bytes) = input::encode_key(key, Mods::default(), None, modes) {
                self.write(&bytes.repeat(lines.unsigned_abs() as usize));
            }
        } else {
            self.shared.emulator.lock().scroll(Scroll::Delta(lines));
        }
        cx.stop_propagation();
        cx.notify();
    }

    // --- drawing ----------------------------------------------------------------------------------

    /// Brings the kept screen up to date with the emulator (only when something changed).
    fn sync_screen(&mut self, colors: &TermColors) {
        let mut emulator = self.shared.emulator.lock();
        emulator.set_colors(colors.clone());
        emulator.check_sync_timeout();
        let changed = self.shared.dirty.swap(false, Ordering::AcqRel)
            || emulator.generation() != self.screen.generation;
        if changed {
            emulator.update_screen(Rc::make_mut(&mut self.screen));
        }
    }

    fn status_overlay(&self, cx: &App) -> Option<gpui_kit::AnyElement> {
        let palette = cx.design_theme().palette;
        let message = match &self.life {
            Life::Failed(why) => format!("Could not start a terminal: {why}"),
            _ => return None,
        };
        Some(
            div()
                .id("terminal-status")
                .absolute()
                .top(px(PAD_Y))
                .left(px(PAD_X))
                .right(px(PAD_X))
                .text_size(text::XS.size)
                .text_color(palette.destructive.hsla())
                .child(message)
                .probe()
                .into_any_element(),
        )
    }

    fn scrolled_indicator(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let below = self.screen.display_offset;
        if below == 0 {
            return None;
        }
        let palette = cx.design_theme().palette;
        Some(
            div()
                .id("terminal-scrolled")
                .absolute()
                .right(px(12.))
                .bottom(px(8.))
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(22.))
                .px(px(8.))
                .rounded(radius::MD)
                .border_1()
                .border_color(palette.border.hsla())
                .bg(palette.popover.hsla())
                .text_size(text::XS.size)
                .text_color(palette.foreground.hsla())
                .cursor_pointer()
                .hover(move |s| s.bg(palette.accent.hsla()))
                .on_mouse_down(
                    GpuiButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.scroll(Scroll::Bottom, cx);
                    }),
                )
                .child(icon(IconName::ArrowDown).size(px(12.)))
                .child(if below == 1 {
                    "1 line below".to_string()
                } else {
                    format!("{below} lines below")
                })
                .probe()
                .into_any_element(),
        )
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // A terminal that goes away without being closed still ends its shell (off this thread).
        if !self.closed
            && let Some(pty) = self.pty.take()
        {
            std::thread::spawn(move || pty.close());
        }
    }
}

/// Everything one frame paints, taken out of the view so the paint closure owns it.
struct Frame {
    screen: Rc<Screen>,
    metrics: Metrics,
    colors: TermColors,
    focused: bool,
    blink_on: bool,
    marked: Option<String>,
    hover_link: Option<(usize, Range<usize>)>,
    flash: bool,
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = TermColors::of(cx.design_theme());
        if self.metrics.is_none() {
            self.metrics = Some(Metrics::measure(window, cx));
        }
        let metrics = self.metrics.clone().expect("measured above");
        self.sync_screen(&colors);
        let frame = Frame {
            screen: self.screen.clone(),
            metrics: metrics.clone(),
            colors: colors.clone(),
            focused: self.focus.is_focused(window),
            blink_on: self.blink_on,
            marked: self.marked.clone(),
            hover_link: self.hover_link.clone(),
            flash: self.is_flashing(),
        };
        let view = cx.entity();
        let focus = self.focus.clone();
        let grid = canvas(
            {
                let view = view.clone();
                let metrics = metrics.clone();
                move |bounds: Bounds<Pixels>, _, cx: &mut App| {
                    let cols =
                        (((bounds.size.width.as_f32() - 2. * PAD_X) / metrics.cell_w).floor()
                            as usize)
                            .max(2);
                    let rows = (((bounds.size.height.as_f32() - 2. * PAD_Y) / metrics.cell_h)
                        .floor() as usize)
                        .max(1);
                    let layout = GridLayout {
                        origin: bounds.origin + point(px(PAD_X), px(PAD_Y)),
                        cell_w: metrics.cell_w,
                        cell_h: metrics.cell_h,
                        cols,
                        rows,
                    };
                    view.update(cx, |v, cx| v.set_layout(layout, cx));
                    layout
                }
            },
            move |bounds, layout, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
                paint(&frame, layout, window, cx);
            },
        )
        .size_full();
        let has_link = self.hover_link.is_some();
        div()
            .id(SharedString::from(format!("terminal-{}", self.number)))
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors.background.hsla())
            .when(has_link, |d| d.cursor_pointer())
            .when(!has_link, |d| d.cursor_text())
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_copy_or_interrupt))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| this.scroll(Scroll::PageUp, cx)))
            .on_action(
                cx.listener(|this, _: &ScrollPageDown, _, cx| this.scroll(Scroll::PageDown, cx)),
            )
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| this.scroll(Scroll::Top, cx)))
            .on_action(
                cx.listener(|this, _: &ScrollToBottom, _, cx| this.scroll(Scroll::Bottom, cx)),
            )
            .on_action(
                cx.listener(|_, _: &NewTerminal, _, cx| cx.emit(TerminalEvent::NewRequested)),
            )
            .on_action(
                cx.listener(|_, _: &CloseTerminal, _, cx| cx.emit(TerminalEvent::CloseRequested)),
            )
            .on_mouse_down(GpuiButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(GpuiButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(GpuiButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(GpuiButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(GpuiButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(GpuiButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(GpuiButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .child(grid)
            .children(self.status_overlay(cx))
            .children(self.scrolled_indicator(cx))
            .probe()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let marked: Vec<u16> = self
            .marked
            .as_deref()
            .unwrap_or("")
            .encode_utf16()
            .collect();
        let end = range.end.min(marked.len());
        let start = range.start.min(end);
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&marked[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let len = self
            .marked
            .as_deref()
            .map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_deref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if !text.is_empty() {
            self.input_text(text, cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The IME's window goes at the cursor.
        let layout = self.layout?;
        let (col, row) = self.screen.cursor.map_or((0, 0), |c| (c.col, c.row));
        Some(Bounds::new(
            layout.origin
                + point(
                    px(layout.cell_w * col as f32),
                    px(layout.cell_h * row as f32),
                ),
            size(px(layout.cell_w), px(layout.cell_h)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

// --- painting --------------------------------------------------------------------------------------

fn cell_bounds(layout: &GridLayout, col: usize, row: usize, width: usize) -> Bounds<Pixels> {
    Bounds::new(
        layout.origin
            + point(
                px(layout.cell_w * col as f32),
                px(layout.cell_h * row as f32),
            ),
        size(px(layout.cell_w * width as f32), px(layout.cell_h)),
    )
}

fn paint(frame: &Frame, layout: GridLayout, window: &mut Window, cx: &mut App) {
    let screen = &frame.screen;
    let colors = &frame.colors;
    let rows = screen.rows.min(layout.rows.max(screen.rows));
    // Backgrounds that are not the canvas's.
    for (row, cells) in screen.lines.iter().enumerate().take(rows) {
        let mut col = 0;
        while col < cells.len() {
            let bg = cells[col].bg;
            if bg == colors.background {
                col += 1;
                continue;
            }
            let start = col;
            while col < cells.len() && cells[col].bg == bg {
                col += 1;
            }
            window.paint_quad(fill(
                cell_bounds(&layout, start, row, col - start),
                bg.hsla(),
            ));
        }
    }
    // The selection.
    for &(row, first, last) in &screen.selection {
        window.paint_quad(fill(
            cell_bounds(&layout, first, row, last + 1 - first),
            colors.selection.hsla(),
        ));
    }
    // The cursor (under the text: a block's character is drawn in the canvas colour on it).
    let cursor = screen
        .cursor
        .filter(|c| !c.blinking || frame.blink_on || !frame.focused)
        .filter(|_| frame.marked.is_none());
    let mut inverted: Option<(usize, usize)> = None;
    if let Some(c) = cursor {
        let width = if c.wide { 2 } else { 1 };
        let cell = cell_bounds(&layout, c.col, c.row, width);
        let color = colors.cursor.hsla();
        match (c.shape, frame.focused) {
            (CursorShape::Block, true) => {
                window.paint_quad(fill(cell, color));
                inverted = Some((c.row, c.col));
            }
            (CursorShape::Underline, _) => {
                let h = px(2.);
                window.paint_quad(fill(
                    Bounds::new(
                        point(cell.origin.x, cell.origin.y + cell.size.height - h),
                        size(cell.size.width, h),
                    ),
                    color,
                ));
            }
            (CursorShape::Beam, _) => {
                window.paint_quad(fill(
                    Bounds::new(cell.origin, size(px(2.), cell.size.height)),
                    color,
                ));
            }
            _ => {
                // Unfocused block (or a hollow one): an outline.
                let t = px(1.);
                let (o, s) = (cell.origin, cell.size);
                for b in [
                    Bounds::new(o, size(s.width, t)),
                    Bounds::new(point(o.x, o.y + s.height - t), size(s.width, t)),
                    Bounds::new(o, size(t, s.height)),
                    Bounds::new(point(o.x + s.width - t, o.y), size(t, s.height)),
                ] {
                    window.paint_quad(fill(b, color));
                }
            }
        }
    }
    // Text.
    let metrics = &frame.metrics;
    let line_px = (metrics.font_size.as_f32() / 12.).round().max(1.);
    for (row, cells) in screen.lines.iter().enumerate().take(rows) {
        paint_row(frame, &layout, row, cells, inverted, line_px, window, cx);
    }
    // The Ctrl-hovered link.
    if let Some((row, cols)) = &frame.hover_link {
        let cell = cell_bounds(&layout, cols.start, *row, cols.len());
        let color = screen
            .lines
            .get(*row)
            .and_then(|r| r.get(cols.start))
            .map_or(colors.foreground, |g| g.fg);
        window.paint_quad(fill(
            Bounds::new(
                point(cell.origin.x, cell.origin.y + cell.size.height - px(1.)),
                size(cell.size.width, px(1.)),
            ),
            color.hsla(),
        ));
    }
    // The IME's text being composed, at the cursor.
    if let (Some(marked), Some(c)) = (&frame.marked, screen.cursor) {
        let run = TextRun {
            len: marked.len(),
            font: metrics.font.clone(),
            color: colors.foreground.hsla(),
            background_color: Some(colors.background.hsla()),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(colors.foreground.hsla()),
                wavy: false,
            }),
            strikethrough: None,
        };
        let shaped =
            window
                .text_system()
                .shape_line(marked.clone().into(), metrics.font_size, &[run], None);
        let origin = cell_bounds(&layout, c.col, c.row, 1).origin;
        let _ = shaped.paint(origin, px(layout.cell_h), TextAlign::Left, None, window, cx);
    }
    if frame.flash {
        let bounds = Bounds::new(
            layout.origin - point(px(PAD_X), px(PAD_Y)),
            size(
                px(layout.cell_w * layout.cols as f32 + 2. * PAD_X),
                px(layout.cell_h * layout.rows as f32 + 2. * PAD_Y),
            ),
        );
        window.paint_quad(fill(bounds, colors.foreground.with_alpha(28).hsla()));
    }
}

/// The style a run of text is shaped with.
#[derive(Clone, PartialEq)]
struct RunStyle {
    bold: bool,
    italic: bool,
    color: crate::design::Color,
    underline: Option<(bool, crate::design::Color)>,
    strike: bool,
}

#[allow(clippy::too_many_arguments)]
fn paint_row(
    frame: &Frame,
    layout: &GridLayout,
    row: usize,
    cells: &[super::emulator::Glyph],
    inverted: Option<(usize, usize)>,
    line_px: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let metrics = &frame.metrics;
    let colors = &frame.colors;
    // A row of plain blanks draws nothing.
    if cells.iter().all(|g| {
        g.c == ' '
            && g.extra.is_none()
            && g.attrs
                & (attrs::UNDERLINE | attrs::STRIKE | attrs::DOUBLE_UNDERLINE | attrs::UNDERCURL)
                == 0
    }) {
        return;
    }
    let style_of = |col: usize, g: &super::emulator::Glyph| RunStyle {
        bold: g.has(attrs::BOLD),
        italic: g.has(attrs::ITALIC),
        color: if inverted == Some((row, col)) {
            g.bg
        } else {
            g.fg
        },
        underline: (g.has(attrs::UNDERLINE)
            || g.has(attrs::DOUBLE_UNDERLINE)
            || g.has(attrs::UNDERCURL))
        .then(|| (g.has(attrs::UNDERCURL), g.fg)),
        strike: g.has(attrs::STRIKE),
    };
    let font_of = |style: &RunStyle| {
        let mut f = metrics.font.clone();
        if style.bold {
            f.weight = FontWeight::BOLD;
        }
        if style.italic {
            f.style = FontStyle::Italic;
        }
        f
    };
    let run_of = |style: &RunStyle, len: usize| TextRun {
        len,
        font: font_of(style),
        color: style.color.hsla(),
        background_color: None,
        underline: style.underline.map(|(wavy, color)| UnderlineStyle {
            thickness: px(line_px),
            color: Some(color.hsla()),
            wavy,
        }),
        strikethrough: style.strike.then(|| StrikethroughStyle {
            thickness: px(line_px),
            color: Some(style.color.hsla()),
        }),
    };
    // A segment of single-width cells: shaped as one line, every glyph on its cell.
    let mut start: Option<usize> = None;
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<(RunStyle, usize)> = None;
    let flush = |start: &mut Option<usize>,
                 text: &mut String,
                 runs: &mut Vec<TextRun>,
                 current: &mut Option<(RunStyle, usize)>,
                 window: &mut Window,
                 cx: &mut App| {
        if let Some((style, len)) = current.take() {
            runs.push(run_of(&style, len));
        }
        if let Some(col) = start.take()
            && !text.trim_end().is_empty()
        {
            let shaped = window.text_system().shape_line(
                std::mem::take(text).into(),
                metrics.font_size,
                runs,
                Some(px(layout.cell_w)),
            );
            let origin = cell_bounds(layout, col, row, 1).origin;
            let _ = shaped.paint(origin, px(layout.cell_h), TextAlign::Left, None, window, cx);
        }
        text.clear();
        runs.clear();
    };
    for (col, g) in cells.iter().enumerate() {
        if g.has(attrs::SPACER) {
            continue;
        }
        let drawn = boxdraw::shapes(g.c, layout.cell_w, layout.cell_h, line_px);
        if g.has(attrs::WIDE) || drawn.is_some() {
            flush(&mut start, &mut text, &mut runs, &mut current, window, cx);
            let color = if inverted == Some((row, col)) {
                g.bg
            } else {
                g.fg
            };
            if let Some(rects) = drawn {
                let cell = cell_bounds(layout, col, row, 1);
                for r in rects {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(
                                cell.origin.x + px(r.x * layout.cell_w),
                                cell.origin.y + px(r.y * layout.cell_h),
                            ),
                            size(
                                px((r.w * layout.cell_w).max(1.)),
                                px((r.h * layout.cell_h).max(1.)),
                            ),
                        ),
                        color.with_alpha((r.alpha * 255.) as u8).hsla(),
                    ));
                }
            } else {
                // A double-width character, centred on its two cells.
                let mut s = g.c.to_string();
                if let Some(extra) = &g.extra {
                    s.push_str(extra);
                }
                let style = style_of(col, g);
                let run = run_of(&RunStyle { color, ..style }, s.len());
                let shaped =
                    window
                        .text_system()
                        .shape_line(s.into(), metrics.font_size, &[run], None);
                let cell = cell_bounds(layout, col, row, 2);
                let dx = ((cell.size.width - shaped.width) / 2.).max(px(0.));
                let _ = shaped.paint(
                    point(cell.origin.x + dx, cell.origin.y),
                    px(layout.cell_h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            continue;
        }
        if start.is_none() {
            start = Some(col);
        }
        let style = style_of(col, g);
        let before = text.len();
        text.push(g.c);
        if let Some(extra) = &g.extra {
            text.push_str(extra);
        }
        let len = text.len() - before;
        match current.as_mut() {
            Some((s, n)) if *s == style => *n += len,
            _ => {
                if let Some((s, n)) = current.take() {
                    runs.push(run_of(&s, n));
                }
                current = Some((style, len));
            }
        }
    }
    flush(&mut start, &mut text, &mut runs, &mut current, window, cx);
    let _ = colors;
}
