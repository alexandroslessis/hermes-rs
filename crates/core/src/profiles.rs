//! The profiles window's SOUL.md editor (the profile rail's "Edit SOUL.md…" too): reads and
//! writes the default profile's persona file. Hermes profiles are separate data folders
//! (HERMES_HOME per profile, switched by relaunching the backend); this backend runs one profile
//! ("default") whose home IS the app's data folder, so the calls answer for it and refuse any
//! other name. The runtime reads this file as the agent's identity (slot #1 of the system
//! prompt), so an edit takes effect from the next turn — no restart, as Hermes's scaffold says.
//!
//! Hermes: `hermes_cli/web_routers/profiles.py` (`get_profile_soul`, `update_profile_soul`),
//! `hermes_cli/web_server_profiles.py` (`_resolve_profile_dir`); the multi-profile routes
//! (create, rename, delete, export, import, setup-command) aren't ported: there is no second
//! profile to serve and no relaunch to switch to one.
//!
//! The page's JSON and its 404 / 500 answers are in `crates/desktop/src/profiles.rs`.

use std::path::{Path, PathBuf};

/// The one profile this backend runs.
pub const DEFAULT_PROFILE: &str = "default";

/// Why a profile call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// No profile of that name (Hermes's `_resolve_profile_dir` 404).
    NoSuchProfile(String),
    /// The file could not be read or written; the text names why.
    Io(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::NoSuchProfile(name) => write!(f, "Profile '{name}' does not exist."),
            ProfileError::Io(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for ProfileError {}

/// A profile's persona file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Soul {
    pub content: String,
    /// `false` when it was never written.
    pub exists: bool,
}

/// The persona file of `name`: the data folder's SOUL.md for "default"; another name is a
/// profile that does not exist.
fn soul_path(data_dir: &Path, name: &str) -> Result<PathBuf, ProfileError> {
    if name != DEFAULT_PROFILE {
        return Err(ProfileError::NoSuchProfile(name.to_string()));
    }
    Ok(data_dir.join("SOUL.md"))
}

/// The persona file, `exists: false` when it was never written (the runtime's built-in identity
/// serves sessions with no SOUL.md, as Hermes's `DEFAULT_AGENT_IDENTITY` does). A UTF-8 BOM is
/// stripped, as Hermes's `utf-8-sig` read is.
pub fn soul(data_dir: &Path, name: &str) -> Result<Soul, ProfileError> {
    let path = soul_path(data_dir, name)?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Soul {
            content: content
                .strip_prefix('\u{feff}')
                .unwrap_or(&content)
                .to_string(),
            exists: true,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Soul {
            content: String::new(),
            exists: false,
        }),
        Err(e) => Err(ProfileError::Io(format!("Could not read SOUL.md: {e}"))),
    }
}

/// The editor's save, written to a sibling and renamed over so an interrupted save can't truncate
/// the file (Hermes's `atomic_write_text`): the paired read reports an unreadable file as "never
/// set", which would make the editor's next save persist an empty document.
pub fn save_soul(data_dir: &Path, name: &str, content: &str) -> Result<(), ProfileError> {
    let path = soul_path(data_dir, name)?;
    let temp = path.with_file_name(format!(".SOUL.md.hermes-tmp-{}", std::process::id()));
    let written = std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(ProfileError::Io(format!("Could not write SOUL.md: {e}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_profile_reads_and_writes_the_soul() {
        let data = hermes_testdir::tempdir().unwrap();
        std::fs::write(data.path().join("SOUL.md"), "You are terse.").unwrap();

        let read = soul(data.path(), "default").unwrap();
        assert_eq!(read.content, "You are terse.");
        assert!(read.exists);

        save_soul(data.path(), "default", "You are blunt.").unwrap();
        assert_eq!(
            soul(data.path(), "default").unwrap().content,
            "You are blunt."
        );

        // The file the runtime reads as the identity is the same one.
        assert_eq!(
            std::fs::read_to_string(data.path().join("SOUL.md")).unwrap(),
            "You are blunt."
        );
    }

    #[test]
    fn a_never_written_soul_reads_as_missing() {
        let data = hermes_testdir::tempdir().unwrap();
        let read = soul(data.path(), "default").unwrap();
        assert_eq!((read.content.as_str(), read.exists), ("", false));
    }

    #[test]
    fn saving_creates_a_missing_soul() {
        let data = hermes_testdir::tempdir().unwrap();
        save_soul(data.path(), "default", "First write.").unwrap();
        assert_eq!(
            soul(data.path(), "default").unwrap().content,
            "First write."
        );
    }

    #[test]
    fn a_utf8_bom_is_stripped_from_the_read() {
        let data = hermes_testdir::tempdir().unwrap();
        std::fs::write(data.path().join("SOUL.md"), "\u{feff}Bom persona.").unwrap();
        assert_eq!(
            soul(data.path(), "default").unwrap().content,
            "Bom persona."
        );
    }

    #[test]
    fn another_profile_name_is_refused() {
        let data = hermes_testdir::tempdir().unwrap();
        for name in ["second", "Default", "../data", "default/x"] {
            let expected = ProfileError::NoSuchProfile(name.to_string());
            assert_eq!(soul(data.path(), name).unwrap_err(), expected, "GET {name}");
            assert_eq!(
                save_soul(data.path(), name, "x").unwrap_err(),
                expected,
                "PUT {name}"
            );
        }
        // The refusal wrote nothing.
        assert!(!data.path().join("SOUL.md").exists());
    }
}
