//! "Hermes is already open": one app at a time on a data folder. The desktop app and the native
//! app read and write the same folder (`sessions.db`, the side files, the memory and skills), and
//! each keeps per-session state in memory (queues, effort picks, the "always allowed" list): two of
//! them at once would overwrite each other's side files and run two turns' worth of state for one
//! thread. Whichever starts first holds `<data>/app.lock` open with no sharing; the second fails to
//! open it and tells the user. The operating system closes the file when the process dies, so a
//! crash never leaves the folder locked (the file itself may remain, unheld; see `turnlock.rs` for
//! the same pattern per thread, and PITFALLS "Two windows": a pid file would stay locked).
//!
//! The ACP agent (`hermes-acp.exe`) does not take it: an editor runs it beside the desktop app
//! today, on the same folder, and the per-thread `TurnLock` keeps the two from running one thread
//! at once.
//!
//! Hermes: none (Hermes's desktop app is one Electron process per profile; this is ours).

use std::fs::File;
use std::path::{Path, PathBuf};

/// The lock file's name in the data folder.
pub const FILE: &str = "app.lock";

/// What the second app tells the user before it exits.
pub const ALREADY_OPEN: &str = "Hermes is already open. Switch to its window, or close it before \
                                starting Hermes again.";

/// Held for the life of the app; dropping it frees the data folder.
pub struct AppLock {
    file: Option<File>,
    path: PathBuf,
}

/// Why the lock wasn't taken.
#[derive(Debug, PartialEq, Eq)]
pub enum LockError {
    /// Another app holds the folder: show [`ALREADY_OPEN`] and exit.
    AlreadyOpen,
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyOpen => f.write_str(ALREADY_OPEN),
        }
    }
}

impl std::error::Error for LockError {}

impl AppLock {
    /// Takes the data folder for this app. A lock that can't be made at all (no folder, a
    /// read-only disk) is not worth refusing to start for: the app runs unlocked and says so in
    /// the log.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        let path = data_dir.join(FILE);
        if std::fs::create_dir_all(data_dir).is_err() {
            return Ok(Self { file: None, path });
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // No sharing: a second open fails while this handle lives.
            options.share_mode(0);
        }
        match options.open(&path) {
            Ok(file) => Ok(Self {
                file: Some(file),
                path,
            }),
            // ERROR_SHARING_VIOLATION: another process holds it.
            Err(error) if error.raw_os_error() == Some(32) => Err(LockError::AlreadyOpen),
            Err(error) => {
                tracing::warn!(
                    "the app lock {} could not be taken: {error}",
                    path.display()
                );
                Ok(Self { file: None, path })
            }
        }
    }

    /// Whether the folder is really held (false when the file couldn't be made).
    pub fn is_held(&self) -> bool {
        self.file.is_some()
    }
}

impl Drop for AppLock {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_second_app_is_refused_until_the_first_exits() {
        let dir = hermes_testdir::tempdir().unwrap();
        let first = AppLock::acquire(dir.path()).unwrap();
        assert!(first.is_held());
        let refused = AppLock::acquire(dir.path()).err().unwrap();
        assert_eq!(refused, LockError::AlreadyOpen);
        assert!(refused.to_string().contains("Hermes is already open"));
        drop(first);
        assert!(AppLock::acquire(dir.path()).unwrap().is_held());
    }

    #[test]
    fn another_data_folder_is_not_held_back() {
        let (a, b) = (
            hermes_testdir::tempdir().unwrap(),
            hermes_testdir::tempdir().unwrap(),
        );
        let _first = AppLock::acquire(a.path()).unwrap();
        assert!(AppLock::acquire(b.path()).unwrap().is_held());
    }

    #[test]
    fn a_lock_left_by_a_crash_does_not_hold_the_folder() {
        // The file stays behind when a process dies; nobody holds it open.
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), b"").unwrap();
        assert!(AppLock::acquire(dir.path()).unwrap().is_held());
    }
}
