//! The profiles window's SOUL.md editor (the profile rail's "Edit SOUL.md…" too): reads and
//! writes the default profile's persona file. Hermes profiles are separate data folders
//! (HERMES_HOME per profile, switched by relaunching the backend); this backend runs one profile
//! ("default") whose home IS the app's data folder, so the routes answer for it and refuse any
//! other name with Hermes's "does not exist" 404. The runtime reads this file as the agent's
//! identity (slot #1 of the system prompt), so an edit takes effect from the next turn — no
//! restart, as Hermes's scaffold says.
//!
//! Hermes: `hermes_cli/web_routers/profiles.py` (`get_profile_soul`, `update_profile_soul`),
//! `hermes_cli/web_server_profiles.py` (`_resolve_profile_dir`); the multi-profile routes
//! (create, rename, delete, export, import, setup-command) aren't ported: there is no second
//! profile to serve and no relaunch to switch to one.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The one profile this backend runs (`rest.rs` `profile()`).
const DEFAULT_PROFILE: &str = "default";

/// The persona file of `name`: the data folder's SOUL.md for "default"; another name is a
/// profile that does not exist (Hermes's `_resolve_profile_dir` 404).
fn soul_path(data_dir: &Path, name: &str) -> Result<PathBuf, (u16, Value)> {
    if name != DEFAULT_PROFILE {
        return Err((
            404,
            json!({"detail": format!("Profile '{name}' does not exist.")}),
        ));
    }
    Ok(data_dir.join("SOUL.md"))
}

/// `GET /api/profiles/{name}/soul`: the persona file, `exists: false` when it was never written
/// (the runtime's built-in identity serves sessions with no SOUL.md, as Hermes's
/// `DEFAULT_AGENT_IDENTITY` does). A UTF-8 BOM is stripped, as Hermes's `utf-8-sig` read is.
pub fn soul(data_dir: &Path, name: &str) -> (u16, Value) {
    match soul_path(data_dir, name) {
        Err(answer) => answer,
        Ok(path) => match std::fs::read_to_string(&path) {
            Ok(content) => (
                200,
                json!({
                    "content": content.strip_prefix('\u{feff}').unwrap_or(&content),
                    "exists": true,
                }),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (200, json!({"content": "", "exists": false}))
            }
            Err(e) => (
                500,
                json!({"detail": format!("Could not read SOUL.md: {e}")}),
            ),
        },
    }
}

/// `PUT /api/profiles/{name}/soul` with `{"content": …}`: the editor's save, written to a
/// sibling and renamed over so an interrupted save can't truncate the file (Hermes's
/// `atomic_write_text`): the paired GET reports an unreadable file as "never set", which would
/// make the editor's next save persist an empty document.
pub fn save_soul(data_dir: &Path, name: &str, content: &str) -> (u16, Value) {
    match soul_path(data_dir, name) {
        Err(answer) => answer,
        Ok(path) => {
            let temp = path.with_file_name(format!(".SOUL.md.hermes-tmp-{}", std::process::id()));
            let written =
                std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, &path));
            if let Err(e) = written {
                let _ = std::fs::remove_file(&temp);
                return (
                    500,
                    json!({"detail": format!("Could not write SOUL.md: {e}")}),
                );
            }
            (200, json!({"ok": true}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_profile_reads_and_writes_the_soul() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("SOUL.md"), "You are terse.").unwrap();

        let (status, body) = soul(data.path(), "default");
        assert_eq!(status, 200);
        assert_eq!(body["content"], "You are terse.");
        assert_eq!(body["exists"], true);

        let (status, body) = save_soul(data.path(), "default", "You are blunt.");
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        let (status, body) = soul(data.path(), "default");
        assert_eq!(status, 200);
        assert_eq!(body["content"], "You are blunt.");

        // The file the runtime reads as the identity is the same one.
        assert_eq!(
            std::fs::read_to_string(data.path().join("SOUL.md")).unwrap(),
            "You are blunt."
        );
    }

    #[test]
    fn a_never_written_soul_reads_as_missing() {
        let data = tempfile::tempdir().unwrap();
        let (status, body) = soul(data.path(), "default");
        assert_eq!(status, 200);
        assert_eq!(body["content"], "");
        assert_eq!(body["exists"], false);
    }

    #[test]
    fn saving_creates_a_missing_soul() {
        let data = tempfile::tempdir().unwrap();
        let (status, body) = save_soul(data.path(), "default", "First write.");
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert_eq!(soul(data.path(), "default").1["content"], "First write.");
    }

    #[test]
    fn a_utf8_bom_is_stripped_from_the_read() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("SOUL.md"), "\u{feff}Bom persona.").unwrap();
        let (_, body) = soul(data.path(), "default");
        assert_eq!(body["content"], "Bom persona.");
    }

    #[test]
    fn another_profile_name_is_refused_like_hermes() {
        let data = tempfile::tempdir().unwrap();
        for name in ["second", "Default", "../data", "default/x"] {
            let (status, body) = soul(data.path(), name);
            assert_eq!(status, 404, "GET {name}");
            assert_eq!(
                body["detail"],
                format!("Profile '{name}' does not exist."),
                "GET {name}"
            );
            let (status, body) = save_soul(data.path(), name, "x");
            assert_eq!(status, 404, "PUT {name}");
            assert_eq!(
                body["detail"],
                format!("Profile '{name}' does not exist."),
                "PUT {name}"
            );
        }
        // The refusal wrote nothing.
        assert!(!data.path().join("SOUL.md").exists());
    }
}
