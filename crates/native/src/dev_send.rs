//! `--dev-send <prompt>` (DEV ONLY, with `--dev`): the app drives one turn by itself, for the
//! Phase 4 live check (`scripts/perf/native-thread-check.ps1`): once the list is read it opens the
//! first listed thread, sends the prompt as the dev strip would (`ThreadModel::submit`, so
//! `Backend::submit` = `Core::submit`), waits for the turn to stream and end, prints what the thread
//! shows on stderr (`dev-send: reply <n> chars: <head>`), writes the `--dump` if one was asked
//! for, reopens the thread from the store and prints the stored reply the same way
//! (`dev-send: stored <n> chars: <head>`), then quits. Every stage has a deadline; a stage that
//! does not finish prints `dev-send: failed: <why>` and quits. No pointer or keyboard is used: the
//! user's input is never touched.
//!
//! Hermes: none (a test driver of our own).

use std::time::{Duration, Instant};

use gpui_kit::{AnyWindowHandle, App, Entity};

use crate::model::{AppModel, Phase, Selection};
use crate::thread::ThreadModel;
use crate::thread::model::{RowKind, ThreadPhase};

/// How long the whole drive may take.
const DEADLINE: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_millis(100);

/// The newest assistant reply of `thread`, as the copy button would give it.
fn last_reply(thread: &Entity<ThreadModel>, cx: &App) -> Option<String> {
    let thread = thread.read(cx);
    let key = thread
        .rows()
        .iter()
        .rev()
        .find(|r| matches!(r.kind, RowKind::Assistant { .. }))?
        .key;
    thread.copy_text(key, cx)
}

fn head(text: &str) -> String {
    text.chars()
        .take(80)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Drives one turn in the app's window (see the module docs).
pub fn start(
    model: Entity<AppModel>,
    window: AnyWindowHandle,
    prompt: String,
    dump: Option<std::path::PathBuf>,
    cx: &mut App,
) {
    #[cfg(not(feature = "dev"))]
    let _ = window;
    cx.spawn(async move |cx| {
        let started = Instant::now();
        let tick = || cx.background_executor().timer(TICK);
        let expired = || started.elapsed() > DEADLINE;
        let fail = |why: &str| eprintln!("dev-send: failed: {why}");

        // 1. The list is read: open the first thread.
        let session = loop {
            let ready = cx.update(|cx| {
                let m = model.read(cx);
                match m.phase() {
                    Phase::Ready => m.snapshot().threads.first().map(|t| Ok(t.id.clone())),
                    Phase::Failed(why) => Some(Err(why.clone())),
                    Phase::Starting => None,
                }
            });
            match ready {
                Some(Ok(id)) => break id,
                Some(Err(why)) => {
                    fail(&format!("the core did not start: {why}"));
                    return quit(cx);
                }
                None if expired() => {
                    fail("no thread was listed");
                    return quit(cx);
                }
                None => tick().await,
            }
        };
        cx.update(|cx| model.update(cx, |m, cx| m.open(&session, cx)));

        // 2. The thread's history is read: send the prompt.
        let thread = loop {
            let ready = cx.update(|cx| {
                let m = model.read(cx);
                let open = matches!(m.selected(), Selection::Thread(id) if *id == session);
                m.thread()
                    .filter(|t| open && *t.read(cx).phase() == ThreadPhase::Ready)
                    .cloned()
            });
            match ready {
                Some(thread) => break thread,
                None if expired() => {
                    fail("the thread did not open");
                    return quit(cx);
                }
                None => tick().await,
            }
        };
        eprintln!("dev-send: sending to {session}");
        cx.update(|cx| thread.update(cx, |t, cx| t.submit(&prompt, cx)));

        // 3. The turn streams, then ends.
        let mut seen_live = false;
        loop {
            if cx.update(|cx| thread.read(cx).is_live()) {
                seen_live = true;
            } else if seen_live {
                break;
            }
            if expired() {
                fail(if seen_live {
                    "the turn did not end"
                } else {
                    "no turn started"
                });
                return quit(cx);
            }
            tick().await;
        }
        let shown = cx.update(|cx| last_reply(&thread, cx));
        match &shown {
            Some(text) => eprintln!("dev-send: reply {} chars: {}", text.len(), head(text)),
            None => {
                fail("the turn ended without a reply");
                return quit(cx);
            }
        }
        #[cfg(feature = "dev")]
        if let Some(path) = dump.clone() {
            cx.update(|cx| crate::dev::dump_after(window, Duration::ZERO, path, cx));
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
        }
        #[cfg(not(feature = "dev"))]
        let _ = &dump;

        // 4. The same thread read back from the store.
        cx.update(|cx| thread.update(cx, |t, cx| t.restart(cx)));
        let mut stored = None;
        while !expired() {
            let read = cx.update(|cx| {
                (*thread.read(cx).phase() == ThreadPhase::Ready)
                    .then(|| last_reply(&thread, cx))
                    .flatten()
            });
            if let Some(text) = read {
                stored = Some(text);
                break;
            }
            tick().await;
        }
        match stored {
            Some(text) => eprintln!("dev-send: stored {} chars: {}", text.len(), head(&text)),
            None => fail("the reply was not read back from the store"),
        }
        quit(cx)
    })
    .detach();
}

fn quit(cx: &mut gpui_kit::AsyncApp) {
    cx.update(|cx| cx.quit());
}
