//! The terminal (Phase 6, CP6.3): the core's shells (`hermes_core::terminal`, through the
//! backend seam [`session::Pty`]) emulated by `alacritty_terminal` ([`emulator`]) and drawn by our
//! own GPUI view ([`view`]), as t3code's bottom drawer (`Ctrl+J`) and as a right-panel tab, several
//! per thread ([`group`]).
//!
//! | Module | What |
//! |---|---|
//! | [`emulator`] | The grid, parser, scrollback, modes, selection; the screen the view draws (no GPUI) |
//! | [`colors`] | Theme tokens and ANSI palettes (no GPUI) |
//! | [`input`] | Keys, paste and mouse reports as bytes (no GPUI) |
//! | [`links`] | Web addresses and `path:line` in a row's text (no GPUI) |
//! | [`boxdraw`] | Box-drawing and block characters as rectangles (no GPUI) |
//! | [`session`] | The [`session::Pty`] seam and the pump: output into the emulator off the UI thread |
//! | [`fixture`] | The fixture's scripted shell and the recorded byte streams |
//! | [`view`] | One terminal on screen: painting, keys, mouse, IME, clipboard, resize |
//! | [`group`] | A thread's terminals in one surface (drawer or panel), and the hub of all of them |
//!
//! **Keys while a terminal has the focus** ([`bind`]): the terminal's key context, `Terminal`,
//! lets every app shortcut through to the shell **except** the ones in [`RESERVED_IN_TERMINAL`]:
//! `Ctrl+J` (the drawer, as t3code: its `terminal.toggle` has no `!terminalFocus`), `Ctrl+Alt+B`
//! (the right panel, idem), `F6` / `Shift+F6` (leave the terminal by keyboard, t3code's focus
//! cycling) and `Ctrl+Shift+P` (all actions; Windows Terminal's palette key). So `Ctrl+K`,
//! `Ctrl+P`, `Ctrl+B`, `Ctrl+N`, `Ctrl+,`, `Tab` and `Shift+Tab` reach the shell (readline,
//! PSReadLine, vim, tmux use them). The terminal's own keys follow Windows Terminal:
//! `Ctrl+Shift+C` / `Ctrl+Shift+V` copy and paste, `Ctrl+C` copies only when something is
//! selected (else ^C), `Ctrl+V` pastes, `Shift+PageUp/PageDown` scroll a page,
//! `Ctrl+Shift+Home/End` jump to the top or bottom, `Ctrl+Shift+T` a new terminal,
//! `Ctrl+Shift+W` closes this one.
//!
//! t3code: `apps/web/src/components/ThreadTerminalDrawer.tsx`, `components/terminal/*`,
//! `packages/shared/src/keybindings.ts` (`terminalFocus`). Reference: `app-terminal-open.png`.

pub mod boxdraw;
pub mod colors;
pub mod emulator;
pub mod fixture;
pub mod group;
pub mod input;
pub mod links;
pub mod session;
pub mod view;

#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod view_tests;

use gpui_kit::{App, KeyBinding, NoAction};

gpui_kit::actions!(
    terminal,
    [
        Copy,
        CopyOrInterrupt,
        Paste,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
        NewTerminal,
        CloseTerminal,
    ]
);

/// App shortcuts that keep working while a terminal has the focus (everything else bound
/// globally reaches the shell).
pub const RESERVED_IN_TERMINAL: &[&str] = &["ctrl-j", "ctrl-alt-b", "f6", "shift-f6", "ctrl-shift-p"];

/// Keys bound outside the app's command table that the terminal takes back for the shell: GPUI
/// Kit's root focus cycling (`Tab`, `Shift+Tab`) and its inspector (`Ctrl+Shift+I`).
const KIT_KEYS: &[&str] = &["tab", "shift-tab", "ctrl-shift-i"];

/// The app's global shortcuts that reach the shell while a terminal has the focus.
pub fn passed_through() -> Vec<&'static str> {
    crate::keymap::COMMANDS
        .iter()
        .filter_map(|c| c.keys)
        .filter(|k| !RESERVED_IN_TERMINAL.contains(k))
        .chain(KIT_KEYS.iter().copied())
        .collect()
}

/// Registers the terminal's keys. Call **after** [`crate::keymap::bind`] and `gpui_kit::init`: a
/// global binding and one in the `Terminal` context weigh the same when the terminal is the
/// focused element, and the later one wins.
pub fn bind(cx: &mut App) {
    let context = Some(view::CONTEXT);
    let mut bindings: Vec<KeyBinding> = passed_through()
        .into_iter()
        .map(|keys| KeyBinding::new(keys, NoAction {}, context))
        .collect();
    bindings.extend([
        KeyBinding::new("ctrl-shift-c", Copy, context),
        KeyBinding::new("ctrl-c", CopyOrInterrupt, context),
        KeyBinding::new("ctrl-shift-v", Paste, context),
        KeyBinding::new("ctrl-v", Paste, context),
        KeyBinding::new("shift-pageup", ScrollPageUp, context),
        KeyBinding::new("shift-pagedown", ScrollPageDown, context),
        KeyBinding::new("ctrl-shift-home", ScrollToTop, context),
        KeyBinding::new("ctrl-shift-end", ScrollToBottom, context),
        KeyBinding::new("ctrl-shift-t", NewTerminal, context),
        KeyBinding::new("ctrl-shift-w", CloseTerminal, context),
    ]);
    cx.bind_keys(bindings);
}
