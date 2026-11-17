//! Temporary folders for tests that are really removed, also on Windows.
//!
//! Why this exists (2026-11-17): `tempfile::TempDir` removes its folder with one
//! `remove_dir_all` in `Drop` and ignores the result. On Windows that call stops at the first file
//! that is still open (a SQLite database held by a runtime or a thread that has not finished), so
//! the whole folder stayed behind, with a full copy of the bundled skills (about 4 MB) in each.
//! One run of the `turns` suite left ~340 such `.tmpXXXX` folders (1.3 GB) in `%TEMP%`; a few
//! hundred runs filled a 300 GB drive. See docs/PITFALLS.md ("Test folders that do not go away").
//!
//! [`TestDir`] is a drop-in for `tempfile::TempDir` in tests:
//! * on drop it deletes everything it can (a locked file does not stop the rest), then retries for
//!   up to about a second and a half while whatever held the file lets go;
//! * what still cannot be deleted is reported on stderr with the folder's name, so a leak is seen
//!   in the test output instead of silently filling the disk;
//! * its name is `tempfile`'s default (`.tmpXXXX`) on purpose: some tests read the workspace
//!   folder's name as words (the local skill retrieval), and a prefix such as `hermes-test-`
//!   made "hello" attach the `test-driven-development` skill. `scripts/test.ps1` keeps every test
//!   run's folders in one place and sweeps them instead.
//!
//! Declare a `TestDir` field LAST in a fixture struct (fields drop in declaration order, so the
//! runtime or store that holds files open is dropped first).

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a drop keeps retrying a folder with a file still open.
const RETRY_FOR: Duration = Duration::from_millis(1500);

/// A temporary folder removed on drop (see the module doc).
#[derive(Debug)]
pub struct TestDir {
    path: Option<PathBuf>,
}

/// Like `tempfile::tempdir()`.
pub fn tempdir() -> io::Result<TestDir> {
    TestDir::new()
}

impl TestDir {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            path: Some(tempfile::tempdir()?.keep()),
        })
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("TestDir used after keep")
    }

    /// Keeps the folder (the caller removes it), like `tempfile::TempDir::keep`.
    pub fn keep(mut self) -> PathBuf {
        self.path.take().expect("TestDir used after keep")
    }

    /// `tempfile::TempDir::into_path`.
    pub fn into_path(self) -> PathBuf {
        self.keep()
    }

    /// Removes the folder now and says whether it is gone.
    pub fn close(mut self) -> io::Result<()> {
        let path = self.path.take().expect("TestDir used after keep");
        remove(&path)
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) = remove(&path) {
            // Not a panic (a drop during unwinding would abort the test binary), but loud.
            eprintln!(
                "hermes_testdir: could not remove {} ({error}); a file in it is still open. \
                 Declare the TestDir after whatever holds files open, so it drops last.",
                path.display()
            );
        }
    }
}

/// Removes `path`, deleting everything that can be deleted on each attempt and retrying for a
/// short while for what could not.
pub fn remove(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(_) if !path.exists() => return Ok(()),
            Err(error) => {
                // One locked file stops `remove_dir_all` early: take out all the rest ourselves.
                sweep(path);
                if !path.exists() {
                    return Ok(());
                }
                if started.elapsed() >= RETRY_FOR {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Deletes every file and folder under `path` that can be deleted, then `path` itself if empty.
fn sweep(path: &Path) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            sweep(&child);
        } else {
            if let Ok(meta) = child.metadata() {
                let mut permissions = meta.permissions();
                if permissions.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    let _ = std::fs::set_permissions(&child, permissions);
                }
            }
            let _ = std::fs::remove_file(&child);
        }
    }
    let _ = std::fs::remove_dir(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_folder_is_removed_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(path.join("a/b")).unwrap();
        std::fs::write(path.join("a/b/f.txt"), "x").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    /// Opens a file the way SQLite does on Windows: other handles may read and write it but not
    /// delete it. (`std::fs::File::create` also allows deleting, which would not reproduce the
    /// leak.)
    #[cfg(windows)]
    fn open_like_sqlite(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap()
    }

    #[test]
    fn a_read_only_file_does_not_keep_the_folder() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let file = path.join("ro.txt");
        std::fs::write(&file, "x").unwrap();
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions).unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    /// The case that filled the disk: a file held open when the folder is dropped. Everything
    /// else goes at once, and the folder goes as soon as the file is closed.
    #[test]
    #[cfg(windows)]
    fn an_open_file_costs_only_itself_and_the_folder_goes_once_it_is_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(path.join("skills")).unwrap();
        std::fs::write(path.join("skills/big.md"), "x".repeat(10_000)).unwrap();
        let open = path.join("a-database.db");
        let mut held = open_like_sqlite(&open);
        held.write_all(b"data").unwrap();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        drop(dir);
        closer.join().unwrap();
        assert!(
            !path.exists(),
            "the folder should be gone after the file closed"
        );
    }

    #[test]
    #[cfg(windows)]
    fn a_file_that_stays_open_leaves_only_that_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(path.join("skills")).unwrap();
        std::fs::write(path.join("skills/big.md"), "x".repeat(10_000)).unwrap();
        let open = path.join("a-database.db");
        let held = open_like_sqlite(&open);
        let result = dir.close();
        assert!(result.is_err(), "a held file cannot be removed");
        assert!(
            !path.join("skills").exists(),
            "the rest must still be deleted"
        );
        drop(held);
        std::fs::remove_dir_all(&path).unwrap();
    }
}
