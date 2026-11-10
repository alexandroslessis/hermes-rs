//! "A turn is running in this thread", across processes. The desktop app and an editor's
//! `hermes-acp.exe` are separate processes on one data folder: two turns on one thread would
//! interleave their messages (rows are keyed by position, so one would overwrite the other's).
//! While a turn runs, its process holds `<data>/running/<session id>.lock` open with no sharing;
//! a second process fails to open it and refuses to start its turn. The operating system closes
//! the file when the process dies, so a crash leaves no stuck lock (the file itself may remain,
//! unheld).

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Held while a turn runs; dropping it frees the thread.
pub struct TurnLock {
    file: Option<File>,
    path: PathBuf,
}

impl TurnLock {
    /// Takes the thread's lock. `Err` (with a message for the user) when another process holds it;
    /// a lock that can't be made at all (no folder, a full disk) is not worth refusing a turn for.
    pub fn acquire(data_dir: &Path, session_id: &str) -> Result<Self> {
        let dir = data_dir.join("running");
        let name: String = session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("{name}.lock"));
        if std::fs::create_dir_all(&dir).is_err() {
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
            // ERROR_SHARING_VIOLATION: held by another process.
            Err(error) if error.raw_os_error() == Some(32) => anyhow::bail!(
                "This thread is running a turn in another Hermes window (the desktop app or an \
                 editor). Wait for it to finish, then send your message again."
            ),
            Err(error) => {
                tracing::warn!(
                    "the turn lock {} could not be taken: {error}",
                    path.display()
                );
                Ok(Self { file: None, path })
            }
        }
    }
}

impl Drop for TurnLock {
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
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let first = TurnLock::acquire(dir.path(), "ses_1").unwrap();
        let refused = TurnLock::acquire(dir.path(), "ses_1").err().unwrap();
        assert!(
            refused.to_string().contains("another Hermes window"),
            "{refused}"
        );
        // Another thread is not held back.
        let _other = TurnLock::acquire(dir.path(), "ses_2").unwrap();
        drop(first);
        TurnLock::acquire(dir.path(), "ses_1").unwrap();
    }
}
