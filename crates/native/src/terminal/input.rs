//! What the terminal sends the program, with no GPUI in it: keys as xterm's sequences (the ones
//! Windows Terminal sends a pseudo-console), pasted text (bracketed when the program asked), and
//! mouse reports for full-screen programs (SGR or the old X10 form).
//!
//! A key that types text (a letter, AltGr's characters, the IME's output) is not encoded here: it
//! reaches the view's input handler and is written as the text it is ([`encode_key`] answers
//! `None` for it).

/// The modifiers of a key or a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2 alt + 4 ctrl (`CSI 1;5A` is Ctrl+Up).
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

/// The modes of the program that change what keys send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: arrows as `ESC O A`.
    pub app_cursor: bool,
}

/// The bytes for key `key` (GPUI's names: `enter`, `left`, `f5`, `a`) with `mods`; `key_char` is
/// the character the key types, if any. `None`: the key types text (the input handler writes it)
/// or means nothing to a terminal.
pub fn encode_key(
    key: &str,
    mods: Mods,
    key_char: Option<&str>,
    modes: KeyModes,
) -> Option<Vec<u8>> {
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    // Keys with a sequence of their own.
    let cursor = |letter: char| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{}{letter}", mods.param()).into_bytes()
        } else if modes.app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[{n};{}~", mods.param()).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt_prefixed = |bytes: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };
    let bytes = match key {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" | "f2" | "f3" | "f4" => {
            let letter = (b'P' + key.as_bytes()[1] - b'1') as char;
            if mods.any() {
                format!("\x1b[1;{}{letter}", mods.param()).into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "enter" => alt_prefixed(b"\r"),
        "escape" => alt_prefixed(b"\x1b"),
        "tab" if mods.shift => csi("Z"),
        "tab" => alt_prefixed(b"\t"),
        // Windows Terminal's choice: DEL for Backspace, BS for Ctrl+Backspace.
        "backspace" if mods.ctrl => alt_prefixed(b"\x08"),
        "backspace" => alt_prefixed(b"\x7f"),
        "space" if mods.ctrl => alt_prefixed(b"\x00"),
        "space" if mods.alt => b"\x1b ".to_vec(),
        "space" => return None,
        _ => {
            // AltGr is Ctrl+Alt on Windows: the character it makes is text.
            if mods.ctrl && mods.alt && key_char.is_some_and(|c| !c.is_empty()) {
                return None;
            }
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if mods.ctrl {
                let control = control_byte(c)?;
                return Some(alt_prefixed(&[control]));
            }
            if mods.alt {
                // Meta: ESC then the character the key types (with Shift's case).
                let typed = key_char.map(str::to_string).unwrap_or_else(|| {
                    if mods.shift {
                        c.to_uppercase().collect()
                    } else {
                        c.to_string()
                    }
                });
                let mut out = vec![0x1b];
                out.extend_from_slice(typed.as_bytes());
                return Some(out);
            }
            return None;
        }
    };
    Some(bytes)
}

/// The control character Ctrl+`c` sends (`Ctrl+A` is 1, `Ctrl+[` is ESC, `Ctrl+/` is 0x1f).
fn control_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '-' | '7' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Pasted (or IME) text as the program should get it: line ends as Enter (`\r`, which is what a
/// pressed Enter sends), and inside `ESC[200~ ... ESC[201~` when the program turned bracketed
/// paste on. The text cannot end the bracket early, so pasted text never runs as typed commands
/// in a program that brackets: every ESC (and the 8-bit CSI, U+009B) is removed from it, which
/// removes any `ESC[201~` however it is nested (removing the marker alone would let
/// `ESC[20ESC[201~1~` close up into a fresh one).
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let inner: String = normalized
            .chars()
            .filter(|c| !matches!(c, '\x1b' | '\u{9b}'))
            .collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(inner.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

/// A mouse button as a program sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// No button (motion with none held).
    None,
    WheelUp,
    WheelDown,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// How the program asked for mouse reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModes {
    /// Presses and releases (1000).
    pub click: bool,
    /// Motion while a button is held (1002).
    pub drag: bool,
    /// All motion (1003).
    pub motion: bool,
    /// SGR encoding (1006).
    pub sgr: bool,
}

impl MouseModes {
    /// Whether the program wants the mouse at all.
    pub fn any(self) -> bool {
        self.click || self.drag || self.motion
    }
}

/// The report for a mouse event at cell (`col`, `row`) (0-based), or `None` when the program's
/// modes do not ask for it.
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
    modes: MouseModes,
) -> Option<Vec<u8>> {
    if !modes.any() {
        return None;
    }
    if action == MouseAction::Motion {
        let held = button != MouseButton::None;
        if !(modes.motion || (modes.drag && held)) {
            return None;
        }
    }
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    let (x, y) = (col + 1, row + 1);
    if modes.sgr {
        let end = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
    } else {
        // X10: a release is button 3; coordinates past 223 cannot be said.
        if action == MouseAction::Release
            && !matches!(button, MouseButton::WheelUp | MouseButton::WheelDown)
        {
            code = (code & !3) | 3;
        }
        if x > 223 || y > 223 {
            return None;
        }
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (32 + code) as u8,
            (32 + x) as u8,
            (32 + y) as u8,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str, mods: Mods) -> Option<Vec<u8>> {
        encode_key(k, mods, None, KeyModes::default())
    }
    const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: false,
    };
    const ALT: Mods = Mods {
        ctrl: false,
        alt: true,
        shift: false,
    };
    const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
    };

    #[test]
    fn keys_send_xterm_sequences() {
        assert_eq!(key("enter", NONE).unwrap(), b"\r");
        assert_eq!(key("backspace", NONE).unwrap(), b"\x7f");
        assert_eq!(key("backspace", CTRL).unwrap(), b"\x08");
        assert_eq!(key("tab", NONE).unwrap(), b"\t");
        assert_eq!(key("tab", SHIFT).unwrap(), b"\x1b[Z");
        assert_eq!(key("escape", NONE).unwrap(), b"\x1b");
        assert_eq!(key("up", NONE).unwrap(), b"\x1b[A");
        assert_eq!(key("left", CTRL).unwrap(), b"\x1b[1;5D");
        assert_eq!(key("right", SHIFT).unwrap(), b"\x1b[1;2C");
        assert_eq!(
            encode_key("up", NONE, None, KeyModes { app_cursor: true }).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(key("delete", NONE).unwrap(), b"\x1b[3~");
        assert_eq!(key("pageup", NONE).unwrap(), b"\x1b[5~");
        assert_eq!(key("pagedown", CTRL).unwrap(), b"\x1b[6;5~");
        assert_eq!(key("f1", NONE).unwrap(), b"\x1bOP");
        assert_eq!(key("f4", NONE).unwrap(), b"\x1bOS");
        assert_eq!(key("f5", NONE).unwrap(), b"\x1b[15~");
        assert_eq!(key("f12", NONE).unwrap(), b"\x1b[24~");
        assert_eq!(key("home", NONE).unwrap(), b"\x1b[H");
        assert_eq!(key("end", NONE).unwrap(), b"\x1b[F");
    }

    #[test]
    fn ctrl_and_alt_with_a_letter() {
        assert_eq!(key("c", CTRL).unwrap(), [3]);
        assert_eq!(key("p", CTRL).unwrap(), [0x10]);
        assert_eq!(key("k", CTRL).unwrap(), [0x0b]);
        assert_eq!(key("[", CTRL).unwrap(), [0x1b]);
        assert_eq!(key("space", CTRL).unwrap(), [0]);
        assert_eq!(key("/", CTRL).unwrap(), [0x1f]);
        assert_eq!(key("b", ALT).unwrap(), b"\x1bb");
        assert_eq!(
            encode_key(
                "b",
                Mods {
                    alt: true,
                    shift: true,
                    ctrl: false
                },
                Some("B"),
                KeyModes::default()
            )
            .unwrap(),
            b"\x1bB"
        );
        // Plain letters, and AltGr's characters, are text for the input handler.
        assert_eq!(key("a", NONE), None);
        assert_eq!(encode_key("a", SHIFT, Some("A"), KeyModes::default()), None);
        assert_eq!(
            encode_key(
                "q",
                Mods {
                    ctrl: true,
                    alt: true,
                    shift: false
                },
                Some("@"),
                KeyModes::default()
            ),
            None
        );
        assert_eq!(key("space", NONE), None);
    }

    #[test]
    fn a_paste_is_bracketed_when_asked_and_cannot_close_the_bracket() {
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(encode_paste("ls\n", true), b"\x1b[200~ls\r\x1b[201~");
        assert_eq!(
            encode_paste("x\x1b[201~rm -rf /\n", true),
            b"\x1b[200~x[201~rm -rf /\r\x1b[201~"
        );
        // Nested markers cannot close up into one once the inner one is removed.
        let nested = encode_paste("a\x1b[20\x1b[201~1~\necho owned\n", true);
        let inner = &nested[6..nested.len() - 6];
        assert!(!inner.contains(&0x1b), "{:?}", String::from_utf8_lossy(&nested));
        assert_eq!(nested.windows(6).filter(|w| w == b"\x1b[201~").count(), 1);
        assert!(!encode_paste("x\u{9b}201~y", true)[6..].starts_with("x\u{9b}".as_bytes()));
    }

    #[test]
    fn mouse_reports_follow_the_programs_modes() {
        let sgr = MouseModes {
            click: true,
            drag: true,
            motion: false,
            sgr: true,
        };
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Press, 4, 2, NONE, sgr).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Release, 4, 2, NONE, sgr).unwrap(),
            b"\x1b[<0;5;3m"
        );
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Motion, 5, 2, NONE, sgr).unwrap(),
            b"\x1b[<32;6;3M"
        );
        // Motion with no button only in 1003.
        assert_eq!(
            encode_mouse(MouseButton::None, MouseAction::Motion, 5, 2, NONE, sgr),
            None
        );
        assert_eq!(
            encode_mouse(MouseButton::WheelUp, MouseAction::Press, 0, 0, CTRL, sgr).unwrap(),
            b"\x1b[<80;1;1M"
        );
        let x10 = MouseModes {
            click: true,
            ..MouseModes::default()
        };
        assert_eq!(
            encode_mouse(MouseButton::Right, MouseAction::Press, 0, 0, NONE, x10).unwrap(),
            [0x1b, b'[', b'M', 34, 33, 33]
        );
        assert_eq!(
            encode_mouse(MouseButton::Right, MouseAction::Release, 0, 0, NONE, x10).unwrap(),
            [0x1b, b'[', b'M', 35, 33, 33]
        );
        assert_eq!(
            encode_mouse(
                MouseButton::Left,
                MouseAction::Press,
                0,
                0,
                NONE,
                MouseModes::default()
            ),
            None
        );
    }
}
