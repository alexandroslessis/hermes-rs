//! The terminal in the app's frame, headless with real text, over the fixture's scripted shell
//! ([`super::fixture::FixturePty`]): the drawer and the right panel's Terminal tab at three window
//! sizes (where the grid, the toolbar and the list sit, the cell grid's alignment, nothing
//! overflowing), keys (what reaches the shell and what the app keeps), Ctrl+C, paste (bracketed
//! when asked), mouse selection and copy, mouse reports in a full-screen program, the wheel and the
//! "lines below" indicator, resize (debounced to the shell), Ctrl+click on a file path, several
//! terminals with titles, a terminal surviving a thread switch and a hidden drawer, closing, the
//! "+" chooser, the bell, the IME's composition, and a failed start.

use std::sync::Arc;
use std::time::Duration;

use alacritty_terminal::term::TermMode;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, ClipboardItem, Entity, EntityInputHandler as _,
    Focusable as _, HeadlessAppContext, Keystroke, Modifiers, MouseButton, PlatformInput,
    ScrollDelta, ScrollEvent, point, px, size, test::TestEventExt as _,
};

use super::fixture::{FixturePty, PROMPT, numbered_lines, tui_demo};
use super::group::{Surface, TerminalGroup};
use super::view::{self, PAD_X, PAD_Y, RESIZE_DEBOUNCE, TerminalView};
use crate::model::{
    AppModel, FixtureBackend,
    fixture::{self as app_fixture, NOW_MS},
};
use crate::panel::Tab;
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::ui_state::UiState;
use crate::workspace::Workspace;

const SMALL: (f32, f32) = (640., 480.);
const MEDIUM: (f32, f32) = (1100., 700.);
const LARGE: (f32, f32) = (1920., 1080.);

struct Rig {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    backend: Arc<FixtureBackend>,
    app: Entity<AppModel>,
    workspace: Entity<Workspace>,
}

/// The frame over the fixture, thread `t1` open, the drawer open (its first terminal started),
/// the sidebar and the right panel closed.
fn rig(window: (f32, f32)) -> Rig {
    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        crate::keymap::bind(cx);
        super::bind(cx);
    });
    let snapshot = app_fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    let app = {
        let backend = backend.clone();
        cx.update(|cx| {
            cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx).with_clock(|| NOW_MS))
        })
    };
    let root = app.clone();
    let mut state = UiState::default();
    state.sidebar.open = false;
    state.right_panel.open = false;
    state.drawer.open = false;
    let handle = open_window(
        &mut cx,
        size(px(window.0), px(window.1)),
        move |window, cx| cx.new(|cx| Workspace::with_model(state, None, root, window, cx)),
    );
    let workspace = cx
        .update_window(handle.into(), |root, _, _| {
            root.downcast::<Workspace>().expect("the workspace")
        })
        .unwrap();
    let mut rig = Rig {
        cx,
        window: handle.into(),
        backend,
        app,
        workspace,
    };
    rig.open("t1");
    rig.toggle_drawer();
    rig
}

impl Rig {
    fn settle(&mut self) {
        for _ in 0..4 {
            self.cx.run_until_parked();
            draw(&mut self.cx, self.window);
        }
    }

    fn layout(&mut self) -> Layout {
        layout(&mut self.cx, self.window)
    }

    fn open(&mut self, id: &str) {
        let app = self.app.clone();
        self.cx.update(|cx| app.update(cx, |m, cx| m.open(id, cx)));
        self.settle();
    }

    fn toggle_drawer(&mut self) {
        let ws = self.workspace.clone();
        self.cx
            .update(|cx| ws.update(cx, |w, cx| w.toggle_drawer(cx)));
        self.settle();
    }

    fn group(&mut self, thread: &str) -> Entity<TerminalGroup> {
        let ws = self.workspace.clone();
        self.cx.update(|cx| {
            ws.read(cx)
                .drawer_terminals()
                .read(cx)
                .existing(thread)
                .cloned()
                .expect("the thread has a drawer group")
        })
    }

    fn terminals(&mut self, thread: &str) -> Vec<Entity<TerminalView>> {
        let group = self.group(thread);
        self.cx.update(|cx| group.read(cx).terminals().to_vec())
    }

    fn terminal(&mut self) -> Entity<TerminalView> {
        let group = self.group("t1");
        self.cx
            .update(|cx| group.read(cx).active().cloned().expect("a terminal"))
    }

    fn pty(&mut self) -> Arc<FixturePty> {
        self.backend.ptys().last().cloned().expect("a shell")
    }

    fn view<R>(&mut self, f: impl FnOnce(&TerminalView) -> R) -> R {
        let t = self.terminal();
        self.cx.update(|cx| f(t.read(cx)))
    }

    fn screen_text(&mut self) -> String {
        self.settle();
        self.view(|v| v.screen().text())
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.pty().emit(bytes);
        self.settle();
    }

    fn with_window(&mut self, f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App)) {
        self.cx
            .update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap();
    }

    fn focus_terminal(&mut self) {
        let t = self.terminal();
        self.with_window(|window, cx| {
            let handle = t.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
        self.settle();
    }

    fn key(&mut self, keys: &'static str) {
        let keystroke = Keystroke::parse(keys).expect("a keystroke");
        self.with_window(move |window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        });
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' {
                "space".to_string()
            } else {
                c.to_string()
            };
            self.key(Box::leak(key.into_boxed_str()));
        }
    }

    /// The window point at the middle of visible cell (`col`, `row`).
    fn cell_point(&mut self, col: usize, row: usize) -> gpui_kit::Point<gpui_kit::Pixels> {
        self.cell_point_at(col, row, 0.5)
    }

    /// A point in cell (`col`, `row`), `fx` of the way across it (under 0.5: its left half).
    fn cell_point_at(
        &mut self,
        col: usize,
        row: usize,
        fx: f32,
    ) -> gpui_kit::Point<gpui_kit::Pixels> {
        let l = self.view(|v| v.layout().expect("laid out"));
        l.origin
            + point(
                px(l.cell_w * (col as f32 + fx)),
                px(l.cell_h * (row as f32 + 0.5)),
            )
    }

    fn mouse(
        &mut self,
        at: gpui_kit::Point<gpui_kit::Pixels>,
        f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App, gpui_kit::Point<gpui_kit::Pixels>),
    ) {
        self.with_window(move |window, cx| f(window, cx, at));
        self.settle();
    }

    fn click_cell(&mut self, col: usize, row: usize, modifiers: Modifiers, count: usize) {
        let at = self.cell_point_at(col, row, 0.25);
        self.with_window(move |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(gpui_kit::MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count: count,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(gpui_kit::MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count: count,
                }),
                cx,
            );
        });
        self.settle();
    }

    fn drag(&mut self, from: (usize, usize), to: (usize, usize)) {
        let (a, b) = (
            self.cell_point_at(from.0, from.1, 0.25),
            self.cell_point_at(to.0, to.1, 0.75),
        );
        self.with_window(move |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseDown(gpui_kit::MouseDownEvent {
                    button: MouseButton::Left,
                    position: a,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
                    position: b,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(gpui_kit::MouseUpEvent {
                    button: MouseButton::Left,
                    position: b,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        });
        self.settle();
    }

    fn wheel(&mut self, lines: f32, modifiers: Modifiers) {
        let at = self.cell_point(2, 2);
        self.with_window(move |window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: at,
                    delta: ScrollDelta::Lines(point(0., lines)),
                    modifiers,
                    ..Default::default()
                }),
                cx,
            );
        });
        self.settle();
    }

    fn click(&mut self, name: &str) {
        let at = self.layout().bounds(name).center();
        self.mouse(at, |window, cx, at| {
            window.pointer_move(at, None, cx);
            window.pointer_down(at, MouseButton::Left, cx);
            window.pointer_up(at, MouseButton::Left, cx);
        });
    }

    fn written(&mut self) -> Vec<u8> {
        self.pty().written()
    }

    fn clear_written(&mut self) {
        self.pty().clear_written();
    }

    fn clipboard(&mut self) -> Option<String> {
        self.cx
            .update(|cx| cx.read_from_clipboard().and_then(|i| i.text()))
    }
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

// --- layout -----------------------------------------------------------------------------------------

#[test]
fn the_drawer_shows_the_threads_terminal_aligned_to_its_cell_grid_at_three_sizes() {
    for size in [SMALL, MEDIUM, LARGE] {
        let mut rig = rig(size);
        let ctx = format!("{size:?}");
        assert_eq!(rig.backend.ptys().len(), 1, "{ctx}: one shell started");
        assert!(rig.screen_text().ends_with(PROMPT.trim_end()), "{ctx}");
        let l = rig.layout();
        let mut check = l.check();
        check
            .inside("terminal-group-drawer", "drawer")
            .inside("terminal-1", "drawer")
            .inside("terminal-toolbar", "terminal-1")
            .no_overflow("drawer")
            .no_overflow("terminal-toolbar");
        check.assert();
        // The toolbar at the top right (t3code's `right-2 top-2`).
        let (term, bar) = (l.bounds("terminal-1"), l.bounds("terminal-toolbar"));
        assert!((term.right() - bar.right()).as_f32() - 8. < 1.5, "{ctx}");
        assert!((bar.top() - term.top()).as_f32() - 8. < 1.5, "{ctx}");
        // The grid: whole cells inside the terminal, from its padded corner.
        let grid = rig.view(|v| v.layout().unwrap());
        let metrics = rig.view(|v| v.metrics().cloned().unwrap());
        assert!(
            metrics.cell_w > 4. && metrics.cell_w < 12.,
            "{ctx}: {}",
            metrics.cell_w
        );
        assert_eq!(
            metrics.cell_h,
            metrics.cell_h.round(),
            "{ctx}: whole-pixel rows"
        );
        assert_eq!(
            grid.origin,
            term.origin + point(px(PAD_X), px(PAD_Y)),
            "{ctx}"
        );
        assert_eq!(
            grid.cols,
            ((term.size.width.as_f32() - 2. * PAD_X) / grid.cell_w).floor() as usize,
            "{ctx}"
        );
        assert_eq!(
            grid.rows,
            ((term.size.height.as_f32() - 2. * PAD_Y) / grid.cell_h).floor() as usize,
            "{ctx}"
        );
        assert!(grid.cols as f32 * grid.cell_w <= term.size.width.as_f32() - PAD_X);
        // The emulator and the shell have that size.
        let emulated = rig.view(|v| v.shared().emulator.lock().size());
        assert_eq!(
            (emulated.cols, emulated.rows),
            (grid.cols, grid.rows),
            "{ctx}"
        );
        assert_eq!(
            rig.pty().resizes().last().copied(),
            Some((grid.cols as u16, grid.rows as u16)),
            "{ctx}: the first size is told at once"
        );
    }
}

#[test]
fn a_drawer_border_drag_resizes_the_grid_and_the_shell_after_a_pause() {
    let mut rig = rig(MEDIUM);
    let before = rig.view(|v| v.layout().unwrap());
    let ws = rig.workspace.clone();
    rig.cx.update(|cx| {
        ws.update(cx, |w, cx| {
            w.drag_border(
                crate::workspace::Pane::Drawer,
                point(px(500.), px(MEDIUM.1 - 380.)),
                size(px(MEDIUM.0), px(MEDIUM.1)),
                cx,
            )
        })
    });
    rig.settle();
    let after = rig.view(|v| v.layout().unwrap());
    assert!(
        after.rows > before.rows,
        "{} -> {}",
        before.rows,
        after.rows
    );
    let emulated = rig.view(|v| v.shared().emulator.lock().size());
    assert_eq!(emulated.rows, after.rows, "the emulator follows at once");
    // The shell is told once the size has held for the debounce.
    let told = rig.pty().resizes().len();
    rig.cx.advance_clock(RESIZE_DEBOUNCE / 2);
    rig.settle();
    assert_eq!(rig.pty().resizes().len(), told, "not yet");
    rig.cx.advance_clock(RESIZE_DEBOUNCE);
    rig.settle();
    assert_eq!(
        rig.pty().resizes().last().copied(),
        Some((after.cols as u16, after.rows as u16))
    );
    // A full-screen program draws at the new size.
    rig.emit(&tui_demo(after.cols, after.rows));
    let s = rig.view(|v| v.screen().clone());
    assert_eq!(s.lines[after.rows - 2][0].c, '└');
    assert_eq!(s.lines[0][after.cols - 1].c, '┐');
}

// --- keys ------------------------------------------------------------------------------------------

#[test]
fn keys_reach_the_shell_and_app_shortcuts_pass_through_except_the_reserved_ones() {
    let mut rig = rig(MEDIUM);
    rig.focus_terminal();
    rig.clear_written();
    rig.type_text("ls");
    rig.key("enter");
    assert_eq!(rig.written(), b"ls\r");
    rig.clear_written();
    // App shortcuts the shell gets: Ctrl+P (the file finder), Ctrl+K (the palette), Ctrl+B
    // (the sidebar), Ctrl+N, Tab and Shift+Tab (GPUI Kit's focus cycling), arrows, Alt+B.
    for key in [
        "ctrl-p",
        "ctrl-k",
        "ctrl-b",
        "ctrl-n",
        "tab",
        "shift-tab",
        "up",
        "alt-b",
        "ctrl-left",
        "escape",
        "backspace",
    ] {
        rig.key(key);
    }
    assert_eq!(
        rig.written(),
        b"\x10\x0b\x02\x0e\t\x1b[Z\x1b[A\x1bb\x1b[1;5D\x1b\x7f"
    );
    let ws = rig.workspace.clone();
    let (palette, finder, sidebar) = rig.cx.update(|cx| {
        let w = ws.read(cx);
        (
            w.palette_open(),
            w.finder().is_some(),
            w.state().sidebar.open,
        )
    });
    assert!(
        !palette && !finder && !sidebar,
        "none of the app's actions ran"
    );
    // Application cursor keys when the program asks.
    rig.emit(b"\x1b[?1h");
    rig.clear_written();
    rig.key("up");
    assert_eq!(rig.written(), b"\x1bOA");
    // Reserved: Ctrl+J closes the drawer (and the shell gets nothing).
    rig.clear_written();
    rig.key("ctrl-j");
    assert!(rig.written().is_empty());
    let open = rig.cx.update(|cx| ws.read(cx).state().drawer.open);
    assert!(!open, "Ctrl+J toggled the drawer");
    // The shell keeps running behind the closed drawer.
    assert!(!rig.pty().is_closed());
}

#[test]
fn the_reserved_and_passed_through_sets_cover_every_global_shortcut() {
    let through = super::passed_through();
    for command in crate::keymap::COMMANDS {
        let Some(keys) = command.keys else { continue };
        let reserved = super::RESERVED_IN_TERMINAL.contains(&keys);
        assert!(
            reserved != through.contains(&keys),
            "{keys} is either reserved or passed through"
        );
    }
    for keys in ["ctrl-k", "ctrl-p", "ctrl-b", "ctrl-n", "tab", "shift-tab"] {
        assert!(through.contains(&keys), "{keys}");
    }
    for keys in ["ctrl-j", "ctrl-alt-b", "f6", "shift-f6", "ctrl-shift-p"] {
        assert!(super::RESERVED_IN_TERMINAL.contains(&keys));
    }
}

#[test]
fn ctrl_c_interrupts_unless_something_is_selected_then_copies() {
    let mut rig = rig(MEDIUM);
    rig.emit(b"\r\nhello world\r\n");
    rig.focus_terminal();
    rig.clear_written();
    rig.key("ctrl-c");
    assert_eq!(rig.written(), [0x03]);
    // Drag over "hello" (row 2, the first line after the greeting and the echoed prompt).
    let row = rig.view(|v| {
        (0..v.screen().rows)
            .find(|&r| v.screen().row_text(r).starts_with("hello"))
            .unwrap()
    });
    rig.drag((0, row), (4, row));
    rig.clear_written();
    rig.key("ctrl-c");
    assert!(
        rig.written().is_empty(),
        "a selection is copied, not interrupted"
    );
    assert_eq!(rig.clipboard().as_deref(), Some("hello"));
    // The selection went with the copy: the next Ctrl+C interrupts again.
    rig.key("ctrl-c");
    assert_eq!(rig.written(), [0x03]);
    // Ctrl+Shift+C copies and keeps the selection; a double click takes a word, a triple a line.
    rig.click_cell(8, row, Modifiers::default(), 2);
    rig.key("ctrl-shift-c");
    assert_eq!(rig.clipboard().as_deref(), Some("world"));
    rig.click_cell(1, row, Modifiers::default(), 3);
    rig.key("ctrl-shift-c");
    assert_eq!(rig.clipboard().as_deref(), Some("hello world\n"));
    // Shift+click extends from where the selection began.
    rig.click_cell(0, row, Modifiers::default(), 1);
    rig.click_cell(3, row, shift(), 1);
    rig.key("ctrl-shift-c");
    assert_eq!(rig.clipboard().as_deref(), Some("hel"));
    // Typing drops the selection.
    rig.type_text("x");
    assert!(!rig.view(|v| v.shared().emulator.lock().has_selection()));
}

#[test]
fn paste_writes_the_clipboard_bracketed_when_the_program_asks() {
    let mut rig = rig(MEDIUM);
    rig.focus_terminal();
    rig.cx
        .update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("echo a\necho b".into())));
    rig.clear_written();
    rig.key("ctrl-v");
    assert_eq!(rig.written(), b"echo a\recho b");
    rig.emit(b"\x1b[?2004h");
    assert!(rig.view(|v| v.mode().contains(TermMode::BRACKETED_PASTE)));
    rig.clear_written();
    rig.key("ctrl-shift-v");
    assert_eq!(rig.written(), b"\x1b[200~echo a\recho b\x1b[201~");
}

#[test]
fn the_ime_composes_at_the_cursor_and_commits_text() {
    let mut rig = rig(MEDIUM);
    rig.focus_terminal();
    rig.clear_written();
    let t = rig.terminal();
    rig.with_window(|window, cx| {
        t.update(cx, |v, cx| {
            v.replace_and_mark_text_in_range(None, "かな", None, window, cx)
        });
    });
    rig.settle();
    assert_eq!(
        rig.view(|v| v.marked_text().map(str::to_string)).as_deref(),
        Some("かな")
    );
    assert!(rig.written().is_empty(), "nothing is sent while composing");
    // The candidate window goes at the cursor.
    let cursor = rig.view(|v| v.screen().cursor.unwrap());
    let at = rig.with_bounds(&t);
    let expected = rig.cell_point(cursor.col, cursor.row);
    assert!(at.contains(&expected), "{at:?} {expected:?}");
    rig.with_window(|window, cx| {
        t.update(cx, |v, cx| {
            v.replace_text_in_range(None, "仮名", window, cx)
        });
    });
    rig.settle();
    assert_eq!(rig.written(), "仮名".as_bytes());
    assert!(rig.view(|v| v.marked_text().is_none()));
}

impl Rig {
    fn with_bounds(&mut self, t: &Entity<TerminalView>) -> gpui_kit::Bounds<gpui_kit::Pixels> {
        let t = t.clone();
        let mut out = None;
        self.with_window(|window, cx| {
            out = t.update(cx, |v, cx| {
                v.bounds_for_range(0..0, gpui_kit::Bounds::default(), window, cx)
            });
        });
        out.expect("bounds for the IME")
    }
}

// --- mouse and scrolling -----------------------------------------------------------------------

#[test]
fn a_full_screen_program_gets_the_mouse_and_shift_selects_instead() {
    let mut rig = rig(MEDIUM);
    let grid = rig.view(|v| v.layout().unwrap());
    let _ = grid;
    rig.focus_terminal();
    rig.type_text("tui");
    rig.key("enter");
    assert!(rig.view(|v| {
        v.mode()
            .contains(TermMode::ALT_SCREEN | TermMode::SGR_MOUSE)
    }));
    rig.clear_written();
    rig.click_cell(4, 2, Modifiers::default(), 1);
    assert_eq!(rig.written(), b"\x1b[<0;5;3M\x1b[<0;5;3m");
    // The wheel goes to the program too.
    rig.clear_written();
    rig.wheel(1., Modifiers::default());
    assert_eq!(rig.written(), b"\x1b[<64;3;3M");
    // Shift overrides: a selection, nothing sent.
    rig.clear_written();
    rig.click_cell(4, 2, shift(), 2);
    assert!(rig.written().is_empty());
    assert!(rig.view(|v| v.shared().emulator.lock().has_selection()));
    // `q` leaves the program.
    rig.focus_terminal();
    rig.key("q");
    assert!(!rig.view(|v| v.mode().contains(TermMode::ALT_SCREEN)));
}

#[test]
fn the_wheel_and_page_keys_scroll_back_input_jumps_to_the_bottom() {
    let mut rig = rig(MEDIUM);
    rig.emit(&numbered_lines(500));
    assert!(
        rig.layout().get("terminal-scrolled").is_err(),
        "at the bottom: no indicator"
    );
    rig.wheel(3., Modifiers::default());
    assert_eq!(rig.view(|v| v.screen().display_offset), 3);
    let l = rig.layout();
    l.check().inside("terminal-scrolled", "terminal-1").assert();
    rig.focus_terminal();
    rig.key("shift-pageup");
    let rows = rig.view(|v| v.layout().unwrap().rows);
    assert_eq!(rig.view(|v| v.screen().display_offset), 3 + rows);
    rig.key("ctrl-shift-home");
    assert_eq!(
        rig.view(|v| v.screen().display_offset),
        rig.view(|v| v.screen().history)
    );
    // Output while scrolled up does not move the view (the indicator counts the lines below).
    rig.key("ctrl-shift-end");
    rig.wheel(5., Modifiers::default());
    rig.emit(b"more\r\n");
    assert!(rig.view(|v| v.screen().display_offset) >= 5);
    // Typing jumps to the bottom.
    rig.type_text("x");
    assert_eq!(rig.view(|v| v.screen().display_offset), 0);
    // The indicator is a button to the bottom too.
    rig.wheel(2., Modifiers::default());
    rig.click("terminal-scrolled");
    assert_eq!(rig.view(|v| v.screen().display_offset), 0);
}

#[test]
fn ctrl_click_on_a_file_path_opens_it_in_the_files_tab() {
    let mut rig = rig(MEDIUM);
    rig.emit(b"\r\nerror: src/main.rs:5:1 oops\r\n");
    let row = rig.view(|v| {
        (0..v.screen().rows)
            .find(|&r| v.screen().row_text(r).starts_with("error"))
            .unwrap()
    });
    // A plain click selects; Ctrl+click on the path opens it.
    rig.click_cell(10, row, ctrl(), 1);
    let ws = rig.workspace.clone();
    let (open, tab) = rig.cx.update(|cx| {
        let w = ws.read(cx);
        (
            w.state().right_panel.open,
            w.right_panel().read(cx).tabs().active(),
        )
    });
    assert!(open, "the right panel opened");
    assert_eq!(tab, Tab::Files);
    let right = rig.cx.update(|cx| ws.read(cx).right_panel().clone());
    let shown = rig.cx.update(|cx| {
        let files = right.read(cx).files().clone();
        let view = files.read(cx).view().clone();
        view.read(cx).shown().map(str::to_string)
    });
    assert_eq!(shown.as_deref(), Some("src/main.rs"));
    // Ctrl held over a link marks it.
    let at = rig.cell_point(10, row);
    rig.mouse(at, |window, cx, at| {
        window.dispatch_event(
            PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: ctrl(),
            }),
            cx,
        );
    });
    let hover = rig.view(|v| v.hover_link().cloned());
    assert_eq!(hover, Some((row, 7..22)));
}

// --- several terminals, threads, closing -----------------------------------------------------------

#[test]
fn several_terminals_have_a_list_titles_and_survive_thread_switches() {
    let mut rig = rig(MEDIUM);
    rig.click("terminal-new");
    let terminals = rig.terminals("t1");
    assert_eq!(terminals.len(), 2);
    assert_eq!(rig.backend.ptys().len(), 2);
    let l = rig.layout();
    let mut check = l.check();
    check
        .inside("terminal-list", "drawer")
        .same_height(&["terminal-row:0", "terminal-row:1"])
        .no_overflow("terminal-list")
        .width_within("terminal-list", 143., 145.);
    check.assert();
    assert!(
        l.get("terminal-toolbar").is_err(),
        "the list has the buttons now"
    );
    assert_eq!(l.bounds("terminal-row:0").size.height.as_f32(), 24.);
    assert_eq!(l.bounds("terminal-list-header").size.height.as_f32(), 22.);
    // A program's title names its row.
    rig.backend.ptys()[1].emit(b"\x1b]0;cargo watch\x07");
    rig.settle();
    let labels: Vec<String> = rig
        .cx
        .update(|cx| terminals.iter().map(|t| t.read(cx).label()).collect());
    assert_eq!(labels, ["Terminal 1", "cargo watch"]);
    // Click the first row: it is shown.
    rig.click("terminal-row:0");
    let group = rig.group("t1");
    let active = rig.cx.update(|cx| rig_group_active(&group, cx));
    assert_eq!(active, 0);
    // Another thread has its own terminal; coming back finds the same two, still running.
    rig.open("t2");
    assert_eq!(rig.terminals("t2").len(), 1);
    assert_eq!(rig.backend.ptys().len(), 3);
    rig.open("t1");
    let again = rig.terminals("t1");
    assert_eq!(
        again.iter().map(|t| t.entity_id()).collect::<Vec<_>>(),
        terminals.iter().map(|t| t.entity_id()).collect::<Vec<_>>()
    );
    // Output that arrived while hidden is there.
    rig.toggle_drawer();
    rig.backend.ptys()[0].emit(b"\r\nwhile hidden\r\n");
    rig.settle();
    rig.toggle_drawer();
    assert!(rig.screen_text().contains("while hidden"));
    assert!(rig.backend.ptys().iter().all(|p| !p.is_closed()));
    // Closing a row ends its shell; `exit` in the other removes it too.
    rig.click("terminal-row-close:1");
    assert!(rig.backend.ptys()[1].is_closed());
    assert_eq!(rig.terminals("t1").len(), 1);
    rig.focus_terminal();
    rig.type_text("exit");
    rig.key("enter");
    assert!(rig.terminals("t1").is_empty());
    assert!(
        rig.layout().get("terminal-empty").is_ok(),
        "t3code's empty state"
    );
    rig.click("terminal-empty-new");
    assert_eq!(rig.terminals("t1").len(), 1);
}

fn rig_group_active(group: &Entity<TerminalGroup>, cx: &gpui_kit::App) -> usize {
    group.read(cx).active_index()
}

#[test]
fn ctrl_shift_t_and_ctrl_shift_w_make_and_close_terminals() {
    let mut rig = rig(MEDIUM);
    rig.focus_terminal();
    rig.key("ctrl-shift-t");
    assert_eq!(rig.terminals("t1").len(), 2);
    rig.focus_terminal();
    rig.key("ctrl-shift-w");
    assert_eq!(rig.terminals("t1").len(), 1);
    assert!(rig.backend.ptys()[1].is_closed());
    assert!(!rig.backend.ptys()[0].is_closed());
}

#[test]
fn the_bell_flashes_briefly() {
    let mut rig = rig(MEDIUM);
    rig.emit(b"\x07");
    assert!(rig.view(|v| v.is_flashing()));
    rig.cx.advance_clock(view::BELL_FLASH * 2);
    rig.settle();
    assert!(!rig.view(|v| v.is_flashing()));
}

#[test]
fn a_shell_that_does_not_start_says_why() {
    let mut cx_rig = rig(MEDIUM);
    cx_rig.backend.fail_terminals(Some("no pseudo-console"));
    cx_rig.click("terminal-new");
    let l = cx_rig.layout();
    assert!(l.get("terminal-status").is_ok());
    let life = cx_rig.view(|v| v.life().clone());
    assert_eq!(life, view::Life::Failed("no pseudo-console".into()));
}

// --- the right panel's Terminal tab and the "+" chooser ----------------------------------------------

#[test]
fn the_plus_chooser_opens_a_terminal_tab_with_its_own_terminals() {
    for size in [SMALL, MEDIUM, LARGE] {
        let mut rig = rig(size);
        let ctx = format!("{size:?}");
        let ws = rig.workspace.clone();
        rig.cx
            .update(|cx| ws.update(cx, |w, cx| w.toggle_right_panel(cx)));
        rig.settle();
        rig.click("panel-tab-new");
        let l = rig.layout();
        let mut check = l.check();
        check
            .inside("surface-chooser", "right-panel")
            .same_height(&["surface-terminal", "surface-files", "surface-diff"])
            .no_overflow("right-panel")
            .no_overflow("panel-tabs");
        check.assert();
        assert_eq!(
            l.bounds("surface-terminal").size.height.as_f32(),
            32.,
            "{ctx}"
        );
        rig.click("surface-terminal");
        let right = rig.cx.update(|cx| ws.read(cx).right_panel().clone());
        let tab = rig.cx.update(|cx| right.read(cx).tabs().active());
        assert_eq!(tab, Tab::Terminal, "{ctx}");
        // Its own shell, not the drawer's.
        assert_eq!(rig.backend.ptys().len(), 2, "{ctx}");
        let l = rig.layout();
        let mut check = l.check();
        check
            .inside("panel-terminal", "right-panel")
            .inside("terminal-group-panel", "panel-terminal")
            .same_row("panel-tab-terminal", "panel-tab-diff")
            .no_overflow("right-panel");
        check.assert();
        // The tab is labelled by its terminal; a title renames it.
        rig.backend.ptys()[1].emit(b"\x1b]2;htop\x07");
        rig.settle();
        let group = rig.cx.update(|cx| {
            right
                .read(cx)
                .terminals()
                .read(cx)
                .existing("t1")
                .cloned()
                .unwrap()
        });
        assert_eq!(
            rig.cx.update(|cx| group.read(cx).label(cx)).as_deref(),
            Some("htop"),
            "{ctx}"
        );
        // Closing the tab keeps the shell; the chooser's letter brings the tab back.
        rig.click("panel-tab-terminal-close");
        let open: Vec<Tab> = rig.cx.update(|cx| right.read(cx).tabs().open().to_vec());
        assert_eq!(open, [Tab::Diff, Tab::Files], "{ctx}");
        assert!(!rig.backend.ptys()[1].is_closed());
        rig.click("panel-tab-new");
        rig.key("t");
        let tab = rig.cx.update(|cx| right.read(cx).tabs().active());
        assert_eq!(tab, Tab::Terminal, "{ctx}: T in the chooser");
        assert_eq!(
            rig.backend.ptys().len(),
            2,
            "{ctx}: the same terminal, no new shell"
        );
        rig.click("panel-tab-new");
        rig.key("f");
        let tab = rig.cx.update(|cx| right.read(cx).tabs().active());
        assert_eq!(tab, Tab::Files, "{ctx}: F in the chooser");
    }
}

#[test]
fn quitting_ends_every_shell() {
    let mut rig = rig(MEDIUM);
    rig.click("terminal-new");
    let backend = rig.backend.clone();
    crate::model::Backend::terminals_close_all(&*backend);
    assert!(backend.ptys().iter().all(|p| p.is_closed()));
    let _ = Surface::Drawer;
}

#[test]
fn a_terminal_view_drops_its_shell_when_it_goes_away() {
    let mut cx = headless_app();
    cx.update(|cx| crate::theme::apply_theme(ThemeChoice::T3Dark, cx));
    let pty = FixturePty::interactive(80, 24);
    {
        let pty = pty.clone();
        let handle = open_window(&mut cx, size(px(600.), px(300.)), move |window, cx| {
            cx.new(|cx| TerminalView::with_pty(pty, 1, (80, 24), window, cx))
        });
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
    }
    cx.run_until_parked();
    // The drop ends it on a thread of its own.
    for _ in 0..200 {
        if pty.is_closed() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pty.is_closed());
}
