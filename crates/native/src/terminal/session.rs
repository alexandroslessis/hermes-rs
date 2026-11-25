//! One terminal's plumbing, with no GPUI in it: the [`Pty`] seam (what the backend gives for a
//! shell: the core's `Shell`, or the fixture's scripted one), and the **pump** that feeds the
//! shell's output into the [`Emulator`] off the UI thread and tells the view when to draw.
//!
//! Backpressure, and what happens to 100 MB of output:
//! - The core keeps each shell's output in a bounded broadcast channel (1,024 chunks of at most
//!   16 KiB) and the last 1 MiB as history. The pump drains it as fast as the emulator parses.
//! - The pump parses in slices of at most [`SLICE`] bytes per lock, so the view never waits on the
//!   lock for more than one slice; it takes the lock fairly ([`FairMutex::lock`]) and the pump
//!   unfairly, so a waiting frame goes first.
//! - After each batch the pump sets a flag and wakes the view once (`Notify` keeps one permit):
//!   however many batches land between two frames, the view draws once, the latest state (drop to
//!   latest). The emulator itself consumes every byte.
//! - If the emulator falls more than the channel's 1,024 chunks behind (the channel says
//!   `Lagged`), the skipped output cannot be parsed any more: the pump resets the emulator and
//!   replays the shell's history (its last 1 MiB), counted in [`Shared::resyncs`]. Memory stays
//!   bounded by the scrollback ([`super::emulator::SCROLLBACK`]) and the channel.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alacritty_terminal::sync::FairMutex;
use tokio::sync::{Notify, broadcast};

use super::emulator::Emulator;

/// Bytes parsed per hold of the lock.
pub const SLICE: usize = 64 * 1024;
/// Bytes taken from the channel before the view is woken.
pub const BATCH: usize = 1024 * 1024;

/// What a shell gives on attach: the output kept so far and the live output from then on.
pub struct PtyAttach {
    pub snapshot: Vec<u8>,
    pub output: broadcast::Receiver<Vec<u8>>,
}

/// A shell as the terminal sees it: the core's (`hermes_core::terminal::Shell`) or the fixture's.
/// Calls do not block (writes go to a writer thread in the core).
pub trait Pty: Send + Sync + 'static {
    /// The output so far and from now on.
    fn attach(&self) -> PtyAttach;
    /// Keystrokes, pasted text, replies to the program's questions.
    fn write(&self, bytes: &[u8]);
    /// The new size in cells.
    fn resize(&self, cols: u16, rows: u16);
    /// Ends the shell and everything it started.
    fn close(&self);
    fn has_exited(&self) -> bool;
    /// Resolves when the shell's process ends.
    fn exited(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    /// The program (`pwsh.exe`, `cmd.exe`).
    fn name(&self) -> String;
    /// The shell's process id.
    fn pid(&self) -> Option<u32>;
}

/// A core shell as a [`Pty`].
pub struct CorePty {
    terminals: Arc<hermes_core::terminal::Terminals>,
    shell: Arc<hermes_core::terminal::Shell>,
}

/// Starts a core shell of `cols` × `rows` in `cwd` (a thread's folder, which must exist: the core
/// would otherwise start it in the home folder). Blocking (the pseudo-console and the process).
pub fn spawn_core(
    terminals: &Arc<hermes_core::terminal::Terminals>,
    cwd: &str,
    cols: u16,
    rows: u16,
) -> Result<Arc<dyn Pty>, String> {
    if !std::path::Path::new(cwd).is_dir() {
        return Err(format!("The thread's folder {cwd} does not exist."));
    }
    let shell = terminals
        .spawn(cols.max(2), rows.max(1), Some(cwd))
        .map_err(|e| match e {
            hermes_core::terminal::SpawnError::Capacity => {
                "Too many terminals are open (16); close one first.".to_string()
            }
            hermes_core::terminal::SpawnError::Failed(why) => why,
        })?;
    Ok(Arc::new(CorePty {
        terminals: terminals.clone(),
        shell,
    }))
}

impl Pty for CorePty {
    fn attach(&self) -> PtyAttach {
        let attachment = self.shell.attach();
        PtyAttach {
            snapshot: attachment.snapshot,
            output: attachment.output,
        }
    }

    fn write(&self, bytes: &[u8]) {
        self.shell.write(bytes);
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.shell.resize(cols.max(2), rows.max(1));
    }

    fn close(&self) {
        self.terminals.remove(self.shell.id());
    }

    fn has_exited(&self) -> bool {
        self.shell.has_exited()
    }

    fn exited(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let shell = self.shell.clone();
        Box::pin(async move {
            if !shell.has_exited() {
                shell.exited().await;
            }
        })
    }

    fn name(&self) -> String {
        self.shell.name().to_string()
    }

    fn pid(&self) -> Option<u32> {
        self.shell.pid()
    }
}

/// The state the pump and the view share.
pub struct Shared {
    pub emulator: FairMutex<Emulator>,
    /// Something changed since the view last drew.
    pub dirty: AtomicBool,
    /// Wakes the view (one permit however many batches).
    pub wake: Notify,
    /// The shell ended.
    pub exited: AtomicBool,
    /// Times the emulator fell too far behind and replayed the history.
    pub resyncs: AtomicU64,
    /// Batches the pump handed to the view (wake-ups asked for).
    pub batches: AtomicU64,
}

impl Shared {
    pub fn new(emulator: Emulator) -> Arc<Self> {
        Arc::new(Self {
            emulator: FairMutex::new(emulator),
            dirty: AtomicBool::new(true),
            wake: Notify::new(),
            exited: AtomicBool::new(false),
            resyncs: AtomicU64::new(0),
            batches: AtomicU64::new(0),
        })
    }

    fn changed(&self) {
        self.batches.fetch_add(1, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

/// Parses `bytes` a slice at a time and writes the emulator's answers back to the shell.
fn feed(shared: &Shared, pty: &dyn Pty, bytes: &[u8]) {
    for slice in bytes.chunks(SLICE) {
        let replies = {
            let mut emulator = shared.emulator.lock_unfair();
            emulator.feed(slice);
            emulator.take_replies()
        };
        if !replies.is_empty() {
            pty.write(&replies);
        }
    }
}

/// Feeds the shell's output into the emulator until the shell ends or the channel closes. Runs on
/// a background task (GPUI's background executor in the app; any executor in tests: it needs no
/// runtime of its own).
pub async fn pump(shared: Arc<Shared>, pty: Arc<dyn Pty>) {
    let PtyAttach {
        snapshot,
        mut output,
    } = pty.attach();
    feed(&shared, pty.as_ref(), &snapshot);
    shared.changed();
    let mut exit = pty.exited();
    loop {
        tokio::select! {
            biased;
            received = output.recv() => match received {
                Ok(first) => {
                    let mut batch = first;
                    // Take what is already waiting, up to a batch, so a flood is parsed in large
                    // slices and the view is woken once for it.
                    while batch.len() < BATCH {
                        match output.try_recv() {
                            Ok(more) => batch.extend_from_slice(&more),
                            Err(_) => break,
                        }
                    }
                    feed(&shared, pty.as_ref(), &batch);
                    shared.changed();
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Fell behind the channel: what was skipped cannot be parsed; start again from
                    // the shell's history.
                    shared.resyncs.fetch_add(1, Ordering::Relaxed);
                    let again = pty.attach();
                    output = again.output;
                    {
                        let mut emulator = shared.emulator.lock_unfair();
                        let size = emulator.size();
                        let colors = emulator.colors().clone();
                        *emulator = Emulator::new(size.cols, size.rows, colors);
                    }
                    feed(&shared, pty.as_ref(), &again.snapshot);
                    shared.changed();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            () = &mut exit => {
                // The last output, then the end.
                let mut rest = Vec::new();
                while let Ok(more) = output.try_recv() {
                    rest.extend_from_slice(&more);
                }
                feed(&shared, pty.as_ref(), &rest);
                break;
            }
        }
    }
    shared.exited.store(true, Ordering::Release);
    shared.changed();
}
