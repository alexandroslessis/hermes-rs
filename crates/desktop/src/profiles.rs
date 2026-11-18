//! The profiles window's SOUL.md editor, as Hermes's routes answer it
//! (`/api/profiles/{name}/soul`): 404 `Profile '<name>' does not exist.` for any profile but
//! "default". The logic is `hermes_core::profiles`.
//!
//! Hermes: `hermes_cli/web_routers/profiles.py` (`get_profile_soul`, `update_profile_soul`).

use std::path::Path;

use hermes_core::profiles::{self, ProfileError};
use serde_json::{Value, json};

/// A refused or failed profile call as Hermes's answer: 404 for another profile's name, 500 for
/// a file that could not be read or written.
fn failure(error: ProfileError) -> (u16, Value) {
    let status = match error {
        ProfileError::NoSuchProfile(_) => 404,
        ProfileError::Io(_) => 500,
    };
    (status, json!({"detail": error.to_string()}))
}

/// `GET /api/profiles/{name}/soul`.
pub fn soul(data_dir: &Path, name: &str) -> (u16, Value) {
    match profiles::soul(data_dir, name) {
        Ok(soul) => (200, json!({"content": soul.content, "exists": soul.exists})),
        Err(error) => failure(error),
    }
}

/// `PUT /api/profiles/{name}/soul` with `{"content": …}`.
pub fn save_soul(data_dir: &Path, name: &str, content: &str) -> (u16, Value) {
    match profiles::save_soul(data_dir, name, content) {
        Ok(()) => (200, json!({"ok": true})),
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soul_routes_answer_with_hermes_shapes() {
        let data = hermes_testdir::tempdir().unwrap();
        let (status, body) = soul(data.path(), "default");
        assert_eq!(status, 200);
        assert_eq!(body, json!({"content": "", "exists": false}));
        let (status, body) = save_soul(data.path(), "default", "You are blunt.");
        assert_eq!((status, body), (200, json!({"ok": true})));
        let (_, body) = soul(data.path(), "default");
        assert_eq!(body, json!({"content": "You are blunt.", "exists": true}));
    }

    #[test]
    fn another_profile_is_hermes_404() {
        let data = hermes_testdir::tempdir().unwrap();
        for name in ["second", "../data"] {
            let message = format!("Profile '{name}' does not exist.");
            let (status, body) = soul(data.path(), name);
            assert_eq!((status, body), (404, json!({"detail": message})));
            let (status, body) = save_soul(data.path(), name, "x");
            assert_eq!((status, body), (404, json!({"detail": message})));
        }
    }
}
