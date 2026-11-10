//! The side panel's terminal: your own shell on this machine (PowerShell), in a pseudo-console,
//! over a WebSocket (`/api/host-terminal`). A shell outlives its socket: reloading the page or
//! switching tabs reattaches to it, with its recent output replayed. Closing the tab ends it;
//! one left alone ends after 15 minutes.
//!
//! The protocol (Hermes's `hermes_cli/web_host_terminal*.py` and `pty_session.py`; the page's
//! side is `apps/desktop/src/lib/browser-terminal.ts`):
//! - the page dials `?cols=&rows=&cwd=` for a new shell, `?attach=<id>` to reattach, or
//!   `?attach=<id>&action=close` to end one;
//! - the server sends a text frame `\0HERMES_TERMINAL_META:{json}` (shell, cwd, terminalId,
//!   truncated…), then one binary frame with the retained output, then the live output;
//! - the page sends keystrokes as text, and `\x1b[RESIZE:<cols>;<rows>]` to resize;
//! - a shell that exits closes the socket with 4410 "Process exited".

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::json;
use tokio::sync::{Notify, broadcast};

const META: &str = "\0HERMES_TERMINAL_META:";
/// The output kept for a reattach (the page caps a snapshot at 1 MiB).
const HISTORY_BYTES: usize = 1024 * 1024;
/// A shell with no page attached ends after this (`RETENTION_SECONDS`).
const RETENTION: Duration = Duration::from_secs(15 * 60);
/// At most this many shells (`_MAX_SHELLS`).
const MAX_SHELLS: usize = 16;
pub const EXITED_REASON: &str = "Process exited";

/// The last `HISTORY_BYTES` of output, and whether older output was dropped.
#[derive(Default)]
struct History {
    bytes: Vec<u8>,
    truncated: bool,
}

impl History {
    fn append(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > HISTORY_BYTES {
            let overflow = self.bytes.len() - HISTORY_BYTES;
            self.bytes.drain(..overflow);
            self.truncated = true;
        }
    }
}

struct Shell {
    id: String,
    name: String,
    cwd: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Keystrokes go to a writer thread (a blocked pipe must not stall the socket).
    input: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Output, and the live feed: appended and sent under one lock, so an attach's snapshot
    /// and its subscription never miss or repeat a chunk.
    history: Mutex<History>,
    live: broadcast::Sender<Vec<u8>>,
    exited: AtomicBool,
    exit: Notify,
    /// Which attachment is current: a newer one supersedes the old (4409).
    viewer: AtomicU64,
    /// When the last page detached (`None` while one is attached).
    idle_since: Mutex<Option<Instant>>,
}

impl Shell {
    fn kill(&self) {
        if let Ok(mut killer) = self.killer.lock() {
            let _ = killer.kill();
        }
    }
}

#[derive(Default)]
pub struct Terminals {
    shells: Mutex<HashMap<String, Arc<Shell>>>,
}

/// The shell to run: PowerShell 7, Windows PowerShell, `COMSPEC`, else cmd (Hermes's
/// `shell_spec`): `(argv, name)`.
fn shell_spec() -> (Vec<String>, String) {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let windows_powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let ladder = [
        on_path("pwsh.exe"),
        Some(windows_powershell).filter(|p| p.is_file()),
        on_path("powershell.exe"),
        std::env::var("COMSPEC")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file()),
    ];
    let command = ladder
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| PathBuf::from("cmd.exe"));
    let name = command
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut argv = vec![command.display().to_string()];
    if name.starts_with("pwsh") || name.starts_with("powershell") {
        argv.push("-NoLogo".into());
    }
    (argv, name)
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// The folder a shell starts in: the one asked for (a file's folder for a file), else home.
fn safe_cwd(requested: Option<&str>) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
    let Some(requested) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return home;
    };
    let candidate = crate::files::resolve(requested, None);
    if candidate.is_dir() {
        candidate
    } else if candidate.is_file() {
        candidate.parent().map_or(home, Path::to_path_buf)
    } else {
        home
    }
}

/// A `cols`/`rows` query value, kept within 2..=max.
pub fn dimension(raw: Option<&str>, default: u16, max: u16) -> u16 {
    raw.and_then(|r| r.parse::<u16>().ok())
        .unwrap_or(default)
        .clamp(2, max)
}

impl Terminals {
    /// Ends shells nobody has looked at within the retention window.
    fn reap_idle(&self) {
        let mut shells = self.shells.lock().unwrap_or_else(|e| e.into_inner());
        shells.retain(|_, shell| {
            let stale = shell
                .idle_since
                .lock()
                .map(|since| since.is_some_and(|t| t.elapsed() > RETENTION))
                .unwrap_or(false);
            let gone = stale || shell.exited.load(Ordering::SeqCst);
            if gone {
                shell.kill();
            }
            !gone
        });
    }

    fn spawn(&self, cols: u16, rows: u16, cwd: Option<&str>) -> Result<Arc<Shell>, String> {
        self.reap_idle();
        if self.shells.lock().map(|s| s.len()).unwrap_or(0) >= MAX_SHELLS {
            return Err("capacity".into());
        }
        let pty = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Could not open a terminal: {e}"))?;
        let (argv, name) = shell_spec();
        let cwd = safe_cwd(cwd);
        let mut command = CommandBuilder::new(&argv[0]);
        command.args(&argv[1..]);
        command.cwd(&cwd);
        for key in ["NO_COLOR", "FORCE_COLOR", "COLORFGBG"] {
            command.env_remove(key);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Hermes");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        command.env("HERMES_DESKTOP_TERMINAL", "1");
        let mut child = pty
            .slave
            .spawn_command(command)
            .map_err(|e| format!("Could not start {name}: {e}"))?;
        drop(pty.slave);
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| format!("Could not read the terminal: {e}"))?;
        let mut writer = pty
            .master
            .take_writer()
            .map_err(|e| format!("Could not write to the terminal: {e}"))?;
        let (input, keystrokes) = std::sync::mpsc::channel::<Vec<u8>>();
        let shell = Arc::new(Shell {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            cwd: cwd.display().to_string(),
            master: Mutex::new(pty.master),
            input,
            killer: Mutex::new(child.clone_killer()),
            history: Mutex::new(History::default()),
            live: broadcast::channel(1024).0,
            exited: AtomicBool::new(false),
            exit: Notify::new(),
            viewer: AtomicU64::new(0),
            idle_since: Mutex::new(None),
        });
        std::thread::spawn(move || {
            for data in keystrokes {
                if writer
                    .write_all(&data)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let output = shell.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut history = output.history.lock().unwrap_or_else(|e| e.into_inner());
                history.append(&buffer[..n]);
                let _ = output.live.send(buffer[..n].to_vec());
            }
        });
        // (The pseudo-console keeps its output pipe open after the shell exits: the exit is
        // the process's own.)
        let waiter = shell.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            waiter.exited.store(true, Ordering::SeqCst);
            waiter.exit.notify_waiters();
            waiter.exit.notify_one();
        });
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(shell.id.clone(), shell.clone());
        Ok(shell)
    }

    fn remove(&self, id: &str) {
        if let Some(shell) = self
            .shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            shell.kill();
        }
    }

    /// Ends every shell (the app is closing).
    pub fn close_all(&self) {
        for (_, shell) in self
            .shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            shell.kill();
        }
    }

    /// Serves one `/api/host-terminal` socket.
    pub async fn serve(
        self: Arc<Self>,
        mut socket: WebSocket,
        attach: Option<String>,
        action: Option<String>,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
    ) {
        let close = |code: u16, reason: &str| {
            Message::Close(Some(CloseFrame {
                code,
                reason: reason.to_string().into(),
            }))
        };
        if action.as_deref() == Some("close") {
            if let Some(id) = &attach {
                self.remove(id);
                let meta = format!("{META}{}", json!({"terminalId": id, "closed": true}));
                let _ = socket.send(Message::Text(meta.into())).await;
            }
            let _ = socket.send(close(1000, "")).await;
            return;
        }
        let existing = attach
            .as_ref()
            .and_then(|id| self.shells.lock().ok().and_then(|s| s.get(id).cloned()));
        let shell = match (&attach, existing) {
            (Some(_), Some(shell)) if shell.exited.load(Ordering::SeqCst) => {
                self.remove(&shell.id);
                let _ = socket.send(close(4410, EXITED_REASON)).await;
                return;
            }
            (Some(_), Some(shell)) => shell,
            (Some(_), None) => {
                let _ = socket
                    .send(close(
                        4410,
                        "Terminal or profile expired; open a new terminal",
                    ))
                    .await;
                return;
            }
            (None, _) => {
                let terminals = self.clone();
                let spawned = tokio::task::spawn_blocking(move || {
                    terminals.spawn(cols, rows, cwd.as_deref())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                match spawned {
                    Ok(shell) => shell,
                    Err(error) if error == "capacity" => {
                        let _ = socket
                            .send(close(
                                1013,
                                "Terminal capacity reached; close a terminal and retry",
                            ))
                            .await;
                        return;
                    }
                    Err(error) => {
                        tracing::warn!("host terminal: {error}");
                        let _ = socket.send(close(1011, &error)).await;
                        return;
                    }
                }
            }
        };
        let me = shell.viewer.fetch_add(1, Ordering::SeqCst) + 1;
        *shell.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // The snapshot and the live feed from the same instant.
        let (snapshot, truncated, mut live) = {
            let history = shell.history.lock().unwrap_or_else(|e| e.into_inner());
            (
                history.bytes.clone(),
                history.truncated,
                shell.live.subscribe(),
            )
        };
        let meta = json!({
            "shell": shell.name, "terminalId": shell.id, "cwd": shell.cwd,
            "reconnected": attach.is_some(), "retentionSeconds": RETENTION.as_secs(),
            "truncated": truncated,
        });
        let opened = socket
            .send(Message::Text(format!("{META}{meta}").into()))
            .await
            .is_ok()
            && socket.send(Message::Binary(snapshot.into())).await.is_ok();
        if opened {
            self.pump(&shell, &mut socket, &mut live, me).await;
        }
        if shell.viewer.load(Ordering::SeqCst) == me {
            *shell.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
    }

    async fn pump(
        &self,
        shell: &Arc<Shell>,
        socket: &mut WebSocket,
        live: &mut broadcast::Receiver<Vec<u8>>,
        me: u64,
    ) {
        let close = |code: u16, reason: &str| {
            Message::Close(Some(CloseFrame {
                code,
                reason: reason.to_string().into(),
            }))
        };
        loop {
            if shell.viewer.load(Ordering::SeqCst) != me {
                let _ = socket.send(close(4409, "Terminal opened elsewhere")).await;
                return;
            }
            if shell.exited.load(Ordering::SeqCst) {
                // What the shell printed last, then the close.
                while let Ok(data) = live.try_recv() {
                    let _ = socket.send(Message::Binary(data.into())).await;
                }
                self.remove(&shell.id);
                let _ = socket.send(close(4410, EXITED_REASON)).await;
                return;
            }
            tokio::select! {
                output = live.recv() => match output {
                    Ok(data) => {
                        if socket.send(Message::Binary(data.into())).await.is_err() {
                            return;
                        }
                    }
                    // A viewer that fell behind skips what it missed.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => self.input(shell, text.as_bytes()),
                    Some(Ok(Message::Binary(bytes))) => self.input(shell, &bytes),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => {}
                },
                () = shell.exit.notified() => {}
                // (Checks the viewer and the exit now and then without a message.)
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    /// A frame from the page: a resize, else keystrokes.
    fn input(&self, shell: &Shell, data: &[u8]) {
        if let Some((cols, rows)) = parse_resize(data) {
            if let Ok(master) = shell.master.lock() {
                let _ = master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            return;
        }
        let _ = shell.input.send(data.to_vec());
    }
}

/// `\x1b[RESIZE:<cols>;<rows>]` → `(cols, rows)`.
fn parse_resize(data: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(data).ok()?;
    let (cols, rows) = text
        .strip_prefix("\x1b[RESIZE:")?
        .strip_suffix(']')?
        .split_once(';')?;
    Some((
        cols.trim().parse::<u16>().ok()?.clamp(2, 1000),
        rows.trim().parse::<u16>().ok()?.clamp(2, 500),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_frames_are_told_from_keystrokes() {
        assert_eq!(parse_resize(b"\x1b[RESIZE:120;30]"), Some((120, 30)));
        assert_eq!(parse_resize(b"\x1b[RESIZE:1;1]"), Some((2, 2)));
        assert_eq!(parse_resize(b"ls\r"), None);
        assert_eq!(parse_resize(b"\x1b[RESIZE:x;30]"), None);
        assert_eq!(dimension(Some("9999"), 80, 1000), 1000);
        assert_eq!(dimension(None, 80, 1000), 80);
    }

    #[test]
    fn history_keeps_the_newest_output() {
        let mut history = History::default();
        history.append(&vec![b'a'; HISTORY_BYTES]);
        assert!(!history.truncated);
        history.append(b"tail");
        assert!(history.truncated);
        assert_eq!(history.bytes.len(), HISTORY_BYTES);
        assert!(history.bytes.ends_with(b"tail"));
    }

    #[test]
    fn the_shell_is_powershell_or_cmd() {
        let (argv, name) = shell_spec();
        assert!(
            Path::new(&argv[0]).is_file() || argv[0] == "cmd.exe",
            "{argv:?}"
        );
        assert!(
            name.starts_with("pwsh") || name.starts_with("powershell") || name.starts_with("cmd")
        );
        assert_eq!(
            safe_cwd(Some("C:/no/such/folder")),
            dirs::home_dir().unwrap()
        );
    }

    #[test]
    fn a_shell_runs_echoes_and_exits() {
        let terminals = Terminals::default();
        let shell = terminals.spawn(80, 24, None).expect("a shell starts");
        // The pseudo-console asks where the cursor is (`ESC[6n`) and waits for the answer, which
        // the page's xterm.js gives; answer as it would.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !shell
            .history
            .lock()
            .unwrap()
            .bytes
            .windows(4)
            .any(|w| w == b"\x1b[6n")
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        shell.input.send(b"\x1b[1;1R".to_vec()).unwrap();
        shell
            .input
            .send(b"echo hermes-terminal-check\r".to_vec())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let seen = String::from_utf8_lossy(&shell.history.lock().unwrap().bytes)
                .contains("hermes-terminal-check\r\n");
            if seen {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let seen = String::from_utf8_lossy(&shell.history.lock().unwrap().bytes).to_string();
        assert!(seen.contains("hermes-terminal-check"), "{seen:?}");
        shell.input.send(b"exit\r".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !shell.exited.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(shell.exited.load(Ordering::SeqCst), "exit ends the shell");
        terminals.close_all();
    }
}
