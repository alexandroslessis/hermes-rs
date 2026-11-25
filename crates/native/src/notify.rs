//! Telling the user that a thread waits for them (Phase 5, CP5.3) when they may not be looking: a
//! flash of the taskbar button and a Windows notification, once per wait, never for a request that
//! was settled meanwhile. What decides is plain logic ([`Policy`], [`Attention`]), tested with no
//! window; what does it is behind [`Notifier`], so the tests record instead of flashing the
//! tester's taskbar.
//!
//! The rules (the CP5.3 brief; t3code's `ThreadNotificationCoordinator.tsx` is the same idea):
//! - Only when the window is not the one the user is in, or the thread that waits is not the open
//!   one: with the thread open in the focused window the panel is the notice.
//! - One notice per thread per wait: a thread's second request while it already announced the
//!   first adds nothing (the notice says "and 1 more" only if it is still unsent when they pile
//!   up); the next wait, after the thread was answered or settled, announces again.
//! - The frame holds a request back for [`HOLD`] before announcing, so a request answered at once
//!   (by a rule, by another front end) never makes a sound, and two arriving together are one.
//! - `notifications` in `native-ui.json` (default on) turns all of it off; the settings screen
//!   that sets it is Phase 7.
//!
//! The real notifier ([`OsNotifier`], Windows): `FlashWindowEx` with `FLASHW_ALL |
//! FLASHW_TIMERNOFG` (the taskbar button flashes until the window is brought to the front, and
//! Windows stops it then) from the `windows` crate already in `Cargo.lock`, and a toast through
//! `tauri-winrt-notification`, also already in `Cargo.lock` (the Tauri app's notification stack).
//! A toast needs an application id the shell knows: until the installer registers ours
//! (Phase 8) it is Windows PowerShell's, so the toast is headed with that name; its own title
//! therefore starts "Hermes:" and its text never holds the command, the path or the question
//! (a lock screen shows it; a command line may carry a token). Clicking the
//! toast sends the thread's id on a channel the frame reads, and the frame opens the thread and
//! brings the window to the front.
//!
//! t3code: `apps/web/src/components/ThreadNotificationCoordinator.tsx` (when a thread's status
//! becomes approval or input while the window is hidden). Hermes: none.

use std::collections::HashSet;
use std::time::Duration;

use super::thread::composer::ask::Asks;

/// How long a request waits before the frame announces it.
pub const HOLD: Duration = Duration::from_millis(400);

/// What a notice says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attention {
    /// The thread that waits.
    pub session: String,
    /// The thread's title (the notice's heading).
    pub title: String,
    /// What it waits for ("Approve a command: ...").
    pub body: String,
}

impl Attention {
    /// The notice for `session`'s waiting requests, or `None` when it has none (settled).
    pub fn of(asks: &Asks, session: &str, title: &str) -> Option<Self> {
        let first = asks.first(session)?;
        let more = asks.count(session) - 1;
        let mut body = first.summary();
        if more > 0 {
            body.push_str(&format!(" (and {more} more)"));
        }
        Some(Self {
            session: session.to_string(),
            title: if title.trim().is_empty() {
                "A thread".to_string()
            } else {
                title.trim().to_string()
            },
            body,
        })
    }

    /// The toast's heading: says it is Hermes (the toast is shown under Windows PowerShell's
    /// name until Phase 8 registers ours, so it must not read as a shell's or the system's
    /// prompt), then the thread's title, cut short.
    pub fn heading(&self) -> String {
        format!("Hermes: {}", super::thread::composer::ask::one_line(&self.title, 60))
    }

    /// The toast's text: what waits (never the command itself, see [`Request::summary`]) and
    /// that the answer is given in the app.
    ///
    /// [`Request::summary`]: crate::thread::composer::ask::Request::summary
    pub fn text(&self) -> String {
        format!("{}. Open Hermes to answer.", self.body)
    }
}

/// Which waits were already announced, and whether announcing is on.
#[derive(Clone, Debug)]
pub struct Policy {
    enabled: bool,
    announced: HashSet<String>,
}

impl Policy {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            announced: HashSet::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether to announce `session`'s wait now: `open` is the thread open in the window,
    /// `window_active` whether the window is the one in front. Says yes once per wait.
    pub fn should_announce(
        &mut self,
        session: &str,
        open: Option<&str>,
        window_active: bool,
    ) -> bool {
        if !self.enabled || self.announced.contains(session) {
            return false;
        }
        // The thread open in the window in front: the panel is the notice.
        if window_active && open == Some(session) {
            return false;
        }
        self.announced.insert(session.to_string());
        true
    }

    /// `session` waits no more: its next wait is announced again.
    pub fn resumed(&mut self, session: &str) {
        self.announced.remove(session);
    }

    /// Whether the wait was announced (tests).
    pub fn announced(&self, session: &str) -> bool {
        self.announced.contains(session)
    }
}

/// How the user is told.
pub trait Notifier {
    /// Flashes the window's taskbar button until the window is brought to the front.
    fn flash(&self, window: &gpui_kit::Window);
    /// Shows a system notification; a click on it sends the thread's id to the frame.
    fn toast(&self, attention: &Attention);
}

/// The Windows notifier.
#[cfg(windows)]
pub struct OsNotifier {
    clicked: std::sync::mpsc::Sender<String>,
}

#[cfg(windows)]
impl OsNotifier {
    /// A notifier whose toasts send the clicked thread's id on `clicked`.
    pub fn new(clicked: std::sync::mpsc::Sender<String>) -> Self {
        Self { clicked }
    }
}

#[cfg(windows)]
impl Notifier for OsNotifier {
    fn flash(&self, window: &gpui_kit::Window) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx,
        };
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(win32.hwnd.get() as *mut std::ffi::c_void),
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 3,
            dwTimeout: 0,
        };
        // SAFETY: `info` is a fully initialised FLASHWINFO for a window of this process.
        unsafe {
            let _ = FlashWindowEx(&info);
        }
    }

    fn toast(&self, attention: &Attention) {
        use tauri_winrt_notification::Toast;
        let session = attention.session.clone();
        let clicked = self.clicked.clone();
        let (heading, text) = (attention.heading(), attention.text());
        // WinRT work (COM, the toast's XML, the notifier) is kept off the UI thread. The texts go
        // in through the XML DOM's `SetInnerText` (the crate's `create_template`), so a `<` or an
        // `&` in a title is text, never markup.
        let spawned = std::thread::Builder::new()
            .name("hermes-toast".into())
            .spawn(move || {
                let shown = Toast::new(Toast::POWERSHELL_APP_ID)
                    .title(&heading)
                    .text1(&text)
                    .on_activated(move |_| {
                        let _ = clicked.send(session.clone());
                        Ok(())
                    })
                    .show();
                if let Err(error) = shown {
                    tracing::warn!("could not show the notification: {error}");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("could not show the notification: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::composer::ask::Request;
    use hermes_core::CoreEvent;

    fn asks_with(count: usize) -> Asks {
        let mut asks = Asks::default();
        for n in 0..count {
            asks.push(
                Request::from_event(&CoreEvent::ApprovalRequested {
                    session_id: "s1".into(),
                    prompt_id: format!("p{n}"),
                    command: format!("rm -rf build{n}"),
                    description: "recursive delete".into(),
                    choices: vec!["once", "deny"],
                })
                .unwrap(),
            );
        }
        asks
    }

    #[test]
    fn the_notice_says_what_waits_and_how_many() {
        assert_eq!(Attention::of(&asks_with(0), "s1", "T"), None);
        let one = Attention::of(&asks_with(1), "s1", " My thread ").unwrap();
        assert_eq!(one.title, "My thread");
        assert_eq!(one.body, "A command needs your approval");
        assert!(!one.text().contains("rm -rf"), "the command never goes in a toast");
        assert_eq!(one.heading(), "Hermes: My thread");
        assert!(one.text().contains("Hermes"));
        let three = Attention::of(&asks_with(3), "s1", "").unwrap();
        assert_eq!(three.title, "A thread");
        assert!(three.body.ends_with("(and 2 more)"), "{}", three.body);
        assert_eq!(Attention::of(&asks_with(1), "s2", "T"), None, "another thread");
    }

    #[test]
    fn announces_when_the_window_is_not_in_front_or_the_thread_is_not_open() {
        let mut policy = Policy::new(true);
        // The open thread in the window in front: no.
        assert!(!policy.should_announce("s1", Some("s1"), true));
        assert!(!policy.announced("s1"));
        // The window is behind: yes (and once).
        assert!(policy.should_announce("s1", Some("s1"), false));
        assert!(!policy.should_announce("s1", Some("s1"), false), "once per wait");
        // Another thread than the open one: yes, whatever the window.
        assert!(policy.should_announce("s2", Some("s1"), true));
        assert!(policy.should_announce("s3", None, true));
    }

    #[test]
    fn a_new_wait_after_the_old_one_ended_is_announced_again() {
        let mut policy = Policy::new(true);
        assert!(policy.should_announce("s1", None, false));
        assert!(!policy.should_announce("s1", None, false));
        policy.resumed("s1");
        assert!(policy.should_announce("s1", None, false));
    }

    #[test]
    fn switched_off_it_never_announces() {
        let mut policy = Policy::new(false);
        assert!(!policy.should_announce("s1", None, false));
        policy.set_enabled(true);
        assert!(policy.should_announce("s1", None, false));
        policy.set_enabled(false);
        policy.resumed("s1");
        assert!(!policy.should_announce("s1", None, false));
    }
}
