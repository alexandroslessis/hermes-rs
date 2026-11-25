//! The fixture's shell: a scripted [`Pty`] for `hermes-native --fixture`, the gallery, the
//! snapshots and the tests. It prints a prompt, echoes what is typed (Backspace and Ctrl+C work),
//! and answers a few commands with recorded byte streams: `colors` (SGR 16 / 256 / true colour,
//! every attribute, wide and emoji characters, box drawing), `tui` (a full-screen program: the
//! alternate screen, cursor addressing, mouse reporting; `q` leaves it), `lines <n>`, `title <t>`,
//! `bell`, `exit`. It records every write and resize. Nothing runs: there is no process.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, broadcast};

use super::session::{Pty, PtyAttach};

/// The prompt the fixture's shell prints.
pub const PROMPT: &str = "PS C:\\fixture\\halo> ";

#[derive(Default)]
struct Typing {
    line: String,
    /// Inside an escape sequence the program sent us (a reply, a paste bracket, an arrow key).
    escape: Option<Vec<u8>>,
    /// The `tui` screen is up (keys go to it).
    tui: bool,
}

/// A scripted shell.
pub struct FixturePty {
    tx: broadcast::Sender<Vec<u8>>,
    history: Mutex<Vec<u8>>,
    writes: Mutex<Vec<Vec<u8>>>,
    resizes: Mutex<Vec<(u16, u16)>>,
    typing: Mutex<Typing>,
    exited: AtomicBool,
    closed: AtomicBool,
    exit: Arc<Notify>,
    /// Echo and answer commands (the app's `--fixture`); off, it only records.
    interactive: bool,
    size: Mutex<(u16, u16)>,
}

impl FixturePty {
    /// A shell that echoes and answers commands, starting with `greeting` then the prompt.
    pub fn interactive(cols: u16, rows: u16) -> Arc<Self> {
        let pty = Self::make(true, cols, rows);
        pty.emit(
            b"Hermes fixture shell. Try: colors, tui, lines 200, title <text>, bell, exit\r\n",
        );
        pty.emit(PROMPT.as_bytes());
        pty
    }

    /// A shell that only records what it is sent; output comes from [`FixturePty::emit`].
    pub fn scripted(cols: u16, rows: u16) -> Arc<Self> {
        Self::make(false, cols, rows)
    }

    fn make(interactive: bool, cols: u16, rows: u16) -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(1024).0,
            history: Mutex::default(),
            writes: Mutex::default(),
            resizes: Mutex::default(),
            typing: Mutex::default(),
            exited: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            exit: Arc::new(Notify::new()),
            interactive,
            size: Mutex::new((cols, rows)),
        })
    }

    /// Output, as the program printed it.
    pub fn emit(&self, bytes: &[u8]) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        history.extend_from_slice(bytes);
        if history.len() > 1 << 20 {
            let over = history.len() - (1 << 20);
            history.drain(..over);
        }
        let _ = self.tx.send(bytes.to_vec());
    }

    /// Chunks sent and not yet read by every receiver (the channel's fill).
    pub fn pending(&self) -> usize {
        self.tx.len()
    }

    /// How many attachments listen.
    pub fn receivers(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Everything written to the shell, joined.
    pub fn written(&self) -> Vec<u8> {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .concat()
    }

    /// Forgets what was written (a test's next step).
    pub fn clear_written(&self) {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The resizes asked for, in order.
    pub fn resizes(&self) -> Vec<(u16, u16)> {
        self.resizes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Ends the "process" (as `exit` does).
    pub fn end(&self) {
        self.exited.store(true, Ordering::SeqCst);
        self.exit.notify_one();
    }

    fn typed(&self, data: &[u8]) {
        let mut out: Vec<u8> = Vec::new();
        let mut typing = self.typing.lock().unwrap_or_else(|e| e.into_inner());
        for &byte in data {
            if let Some(seq) = typing.escape.as_mut() {
                seq.push(byte);
                // `ESC [ ... final` or `ESC O x`, or a two-byte escape.
                let done = match seq.as_slice() {
                    [b'['] | [b'O'] => false,
                    [b'[', .., last] => (0x40..=0x7e).contains(last),
                    _ => true,
                };
                if done {
                    typing.escape = None;
                }
                continue;
            }
            if typing.tui {
                if byte == b'q' {
                    typing.tui = false;
                    out.extend_from_slice(b"\x1b[?1000l\x1b[?1006l\x1b[?1049l");
                    out.extend_from_slice(PROMPT.as_bytes());
                }
                continue;
            }
            match byte {
                0x1b => typing.escape = Some(Vec::new()),
                b'\r' => {
                    let line = std::mem::take(&mut typing.line);
                    out.extend_from_slice(b"\r\n");
                    let (cols, rows) = *self.size.lock().unwrap_or_else(|e| e.into_inner());
                    match run(line.trim(), cols, rows) {
                        Reply::Text(text) => {
                            out.extend_from_slice(&text);
                            out.extend_from_slice(PROMPT.as_bytes());
                        }
                        Reply::Tui(screen) => {
                            typing.tui = true;
                            out.extend_from_slice(&screen);
                        }
                        Reply::Exit => {
                            drop(typing);
                            self.emit(&out);
                            self.end();
                            return;
                        }
                    }
                }
                0x7f | 0x08 => {
                    if typing.line.pop().is_some() {
                        out.extend_from_slice(b"\x08 \x08");
                    }
                }
                0x03 => {
                    typing.line.clear();
                    out.extend_from_slice(b"^C\r\n");
                    out.extend_from_slice(PROMPT.as_bytes());
                }
                b if b >= 0x20 => {
                    typing.line.push(b as char);
                    out.push(b);
                }
                _ => {}
            }
        }
        drop(typing);
        if !out.is_empty() {
            self.emit(&out);
        }
    }
}

enum Reply {
    Text(Vec<u8>),
    Tui(Vec<u8>),
    Exit,
}

fn run(command: &str, cols: u16, rows: u16) -> Reply {
    let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
    match name {
        "" => Reply::Text(Vec::new()),
        "colors" | "colours" => Reply::Text(colours_demo()),
        "tui" => Reply::Tui(tui_demo(usize::from(cols), usize::from(rows))),
        "lines" => {
            let n: usize = rest.trim().parse().unwrap_or(100).min(1_000_000);
            Reply::Text(numbered_lines(n))
        }
        "title" => Reply::Text(format!("\x1b]0;{rest}\x07").into_bytes()),
        "bell" => Reply::Text(b"\x07".to_vec()),
        "exit" => Reply::Exit,
        "help" => Reply::Text(b"colors, tui, lines <n>, title <text>, bell, exit\r\n".to_vec()),
        other => Reply::Text(
            format!("fixture: '{other}' is not a command here (try help)\r\n").into_bytes(),
        ),
    }
}

impl Pty for FixturePty {
    fn attach(&self) -> PtyAttach {
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        PtyAttach {
            snapshot: history.clone(),
            output: self.tx.subscribe(),
        }
    }

    fn write(&self, bytes: &[u8]) {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(bytes.to_vec());
        if self.interactive && !self.exited.load(Ordering::SeqCst) {
            self.typed(bytes);
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = (cols, rows);
        self.resizes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((cols, rows));
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.end();
    }

    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn exited(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let exit = self.exit.clone();
        let done = self.exited.load(Ordering::SeqCst);
        Box::pin(async move {
            if !done {
                exit.notified().await;
            }
        })
    }

    fn name(&self) -> String {
        "fixture".into()
    }

    fn pid(&self) -> Option<u32> {
        None
    }
}

// --- recorded streams ---------------------------------------------------------------------------

/// `n` numbered lines (`line 1` ...), CRLF-ended.
pub fn numbered_lines(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 12);
    for i in 1..=n {
        out.extend_from_slice(format!("line {i}\r\n").as_bytes());
    }
    out
}

/// What `colors` prints: the 16 colours as foreground and background, a 256-colour strip, a
/// true-colour ramp, every attribute, wide and emoji characters, and a box-drawn table.
pub fn colours_demo() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("\x1b[1m16 colours\x1b[0m\r\n");
    for row in [0u8, 8] {
        for i in row..row + 8 {
            let fg = if i < 8 { 30 + i } else { 90 + i - 8 };
            s.push_str(&format!("\x1b[{fg}m{i:>3} \x1b[0m"));
        }
        s.push_str("  ");
        for i in row..row + 8 {
            let bg = if i < 8 { 40 + i } else { 100 + i - 8 };
            s.push_str(&format!("\x1b[{bg}m   \x1b[0m"));
        }
        s.push_str("\r\n");
    }
    s.push_str("\x1b[1m256\x1b[0m ");
    for i in (16..232).step_by(6) {
        s.push_str(&format!("\x1b[48;5;{i}m \x1b[0m"));
    }
    s.push_str("\r\n\x1b[1mRGB\x1b[0m ");
    for i in 0..36u32 {
        let r = 255 - i * 7;
        let b = i * 7;
        s.push_str(&format!("\x1b[48;2;{r};64;{b}m \x1b[0m"));
    }
    s.push_str("\r\n");
    s.push_str(
        "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \
         \x1b[7minverse\x1b[0m \x1b[9mstrike\x1b[0m \x1b[4:3mcurly\x1b[0m \
         \x1b[38;2;255;136;0mtrue\x1b[0m \x1b[38;5;45m256\x1b[0m\r\n",
    );
    s.push_str("wide: 漢字かな 한글  emoji: 🚀✨ ok\r\n");
    s.push_str("┌──────┬──────┐\r\n│ name │ size │\r\n├──────┼──────┤\r\n│ a.rs │  12K │\r\n└──────┴──────┘\r\n");
    s.push_str("blocks: █▓▒░ ▁▂▃▄▅▆▇█\r\n");
    s.into_bytes()
}

/// What `tui` draws: the alternate screen with a frame of the terminal's size, a title, a list, a
/// status bar in inverse at the bottom, the cursor parked, and mouse reporting on (SGR).
pub fn tui_demo(cols: usize, rows: usize) -> Vec<u8> {
    let cols = cols.clamp(20, 400);
    let rows = rows.clamp(6, 200);
    let mut s = String::from("\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[H\x1b[2J");
    // The frame.
    s.push_str(&format!("\x1b[1;1H\x1b[36m┌{}┐", "─".repeat(cols - 2)));
    for row in 2..rows - 1 {
        s.push_str(&format!("\x1b[{row};1H│\x1b[{row};{cols}H│"));
    }
    s.push_str(&format!(
        "\x1b[{};1H└{}┘\x1b[0m",
        rows - 1,
        "─".repeat(cols - 2)
    ));
    s.push_str("\x1b[1;3H\x1b[1m htop-ish \x1b[0m");
    for (i, (name, cpu)) in [("hermes-native", 12), ("pwsh", 3), ("git", 0)]
        .iter()
        .enumerate()
    {
        let bar = "|".repeat(cpu / 2 + 1);
        s.push_str(&format!(
            "\x1b[{};3H{name:<16}\x1b[32m{bar}\x1b[0m {cpu:>3}%",
            i + 3
        ));
    }
    s.push_str(&format!(
        "\x1b[{rows};1H\x1b[7m{:<width$}\x1b[0m",
        " q quit  F1 help",
        width = cols
    ));
    s.push_str("\x1b[3;2H");
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_shell_echoes_and_answers() {
        let pty = FixturePty::interactive(80, 24);
        let mut attach = pty.attach();
        assert!(String::from_utf8_lossy(&attach.snapshot).ends_with(PROMPT));
        pty.write(b"helx\x7fp\r");
        let mut got = Vec::new();
        while let Ok(chunk) = attach.output.try_recv() {
            got.extend(chunk);
        }
        let text = String::from_utf8_lossy(&got);
        assert!(text.contains("colors, tui"), "{text:?}");
        assert!(text.ends_with(PROMPT));
        // A reply to the program's question is not typed text.
        pty.write(b"\x1b[1;1R");
        pty.write(b"exit\r");
        assert!(pty.has_exited());
        assert_eq!(pty.written(), b"helx\x7fp\r\x1b[1;1Rexit\r");
    }
}
