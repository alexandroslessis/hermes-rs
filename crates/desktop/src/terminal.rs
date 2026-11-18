//! The side panel's terminal over a WebSocket (`/api/host-terminal`): the core's shells
//! (`hermes_core::terminal`) in Hermes's socket protocol. A shell outlives its socket: reloading
//! the page or switching tabs reattaches to it, with its recent output replayed. Closing the tab
//! ends it; one left alone ends after 15 minutes.
//!
//! The protocol (Hermes's `hermes_cli/web_host_terminal*.py` and `pty_session.py`; the page's
//! side is `apps/desktop/src/lib/browser-terminal.ts`):
//! - the page dials `?cols=&rows=&cwd=` for a new shell, `?attach=<id>` to reattach, or
//!   `?attach=<id>&action=close` to end one;
//! - the server sends a text frame `\0HERMES_TERMINAL_META:{json}` (shell, cwd, terminalId,
//!   truncated…), then one binary frame with the retained output, then the live output;
//! - the page sends keystrokes as text, and `\x1b[RESIZE:<cols>;<rows>]` to resize;
//! - a shell that exits closes the socket with 4410 "Process exited".

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use hermes_core::terminal::{RETENTION, Shell, SpawnError};
use serde_json::json;
use tokio::sync::broadcast;

pub use hermes_core::terminal::Terminals;

const META: &str = "\0HERMES_TERMINAL_META:";
pub const EXITED_REASON: &str = "Process exited";

/// A `cols`/`rows` query value, kept within 2..=max.
pub fn dimension(raw: Option<&str>, default: u16, max: u16) -> u16 {
    raw.and_then(|r| r.parse::<u16>().ok())
        .unwrap_or(default)
        .clamp(2, max)
}

fn close(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.to_string().into(),
    }))
}

/// Serves one `/api/host-terminal` socket.
pub async fn serve(
    terminals: Arc<Terminals>,
    mut socket: WebSocket,
    attach: Option<String>,
    action: Option<String>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) {
    if action.as_deref() == Some("close") {
        if let Some(id) = &attach {
            terminals.remove(id);
            let meta = format!("{META}{}", json!({"terminalId": id, "closed": true}));
            let _ = socket.send(Message::Text(meta.into())).await;
        }
        let _ = socket.send(close(1000, "")).await;
        return;
    }
    let existing = attach.as_ref().and_then(|id| terminals.get(id));
    let shell = match (&attach, existing) {
        (Some(_), Some(shell)) if shell.has_exited() => {
            terminals.remove(shell.id());
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
            let spawner = terminals.clone();
            let spawned =
                tokio::task::spawn_blocking(move || spawner.spawn(cols, rows, cwd.as_deref()))
                    .await
                    .unwrap_or_else(|e| Err(SpawnError::Failed(e.to_string())));
            match spawned {
                Ok(shell) => shell,
                Err(SpawnError::Capacity) => {
                    let _ = socket
                        .send(close(
                            1013,
                            "Terminal capacity reached; close a terminal and retry",
                        ))
                        .await;
                    return;
                }
                Err(SpawnError::Failed(error)) => {
                    tracing::warn!("host terminal: {error}");
                    let _ = socket.send(close(1011, &error)).await;
                    return;
                }
            }
        }
    };
    // The snapshot and the live feed from the same instant.
    let mut attached = shell.attach();
    let meta = json!({
        "shell": shell.name(), "terminalId": shell.id(), "cwd": shell.cwd(),
        "reconnected": attach.is_some(), "retentionSeconds": RETENTION.as_secs(),
        "truncated": attached.truncated,
    });
    let snapshot = std::mem::take(&mut attached.snapshot);
    let opened = socket
        .send(Message::Text(format!("{META}{meta}").into()))
        .await
        .is_ok()
        && socket.send(Message::Binary(snapshot.into())).await.is_ok();
    if opened {
        pump(
            &terminals,
            &shell,
            &mut socket,
            &mut attached.output,
            attached.viewer,
        )
        .await;
    }
    shell.detach(attached.viewer);
}

async fn pump(
    terminals: &Terminals,
    shell: &Arc<Shell>,
    socket: &mut WebSocket,
    live: &mut broadcast::Receiver<Vec<u8>>,
    me: u64,
) {
    loop {
        if !shell.is_current(me) {
            let _ = socket.send(close(4409, "Terminal opened elsewhere")).await;
            return;
        }
        if shell.has_exited() {
            // What the shell printed last, then the close.
            while let Ok(data) = live.try_recv() {
                let _ = socket.send(Message::Binary(data.into())).await;
            }
            terminals.remove(shell.id());
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
                Some(Ok(Message::Text(text))) => input(shell, text.as_bytes()),
                Some(Ok(Message::Binary(bytes))) => input(shell, &bytes),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
            () = shell.exited() => {}
            // (Checks the viewer and the exit now and then without a message.)
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

/// A frame from the page: a resize, else keystrokes.
fn input(shell: &Shell, data: &[u8]) {
    match parse_resize(data) {
        Some((cols, rows)) => shell.resize(cols, rows),
        None => shell.write(data),
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
}
