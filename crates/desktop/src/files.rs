//! The side panel's file preview and spot editor in Hermes's JSON: `/api/fs/read-text`,
//! `/api/fs/write-text`, `/api/files/mkdir`, `/api/fs/git-root`, each answering a status and a
//! body (`{"detail"}` on a refusal). Reading, writing and resolving the files is
//! `hermes_core::files`.
//!
//! Hermes: `hermes_cli/web_routers/files.py`.

use std::path::Path;

use hermes_core::files::FileError;
use serde_json::{Value, json};

pub use hermes_core::files::{is_sensitive, mime, resolve};

type Answer = (u16, Value);

/// A refusal as the route's status and `{"detail"}`.
pub fn refused(error: FileError) -> Answer {
    let status = match &error {
        FileError::Invalid(_) => 400,
        FileError::Forbidden(_) => 403,
        FileError::NotFound(_) => 404,
        FileError::Conflict(_) => 409,
        FileError::TooLarge(_) => 413,
        FileError::Failed(_) => 500,
    };
    (status, json!({"detail": error.message()}))
}

/// A regular, non-sensitive file to read: its size, or the error answer.
pub fn readable(target: &Path) -> Result<u64, Answer> {
    hermes_core::files::readable(target).map_err(refused)
}

/// `GET /api/fs/read-text`: the file's text for the preview (the first 512 KB), whether it looks
/// binary, its language and type.
pub fn read_text(target: &Path) -> Answer {
    match hermes_core::files::read_text(target) {
        Ok(preview) => (
            200,
            json!({
                "binary": preview.binary,
                "byteSize": preview.byte_size,
                "language": preview.language,
                "mimeType": preview.mime,
                "path": preview.path.display().to_string(),
                "text": preview.text,
                "truncated": preview.truncated,
            }),
        ),
        Err(error) => refused(error),
    }
}

/// `POST /api/fs/write-text`: the spot editor's save.
pub fn write_text(target: &Path, content: &str) -> Answer {
    match hermes_core::files::write_text(target, content) {
        Ok(size) => (
            200,
            json!({"ok": true, "path": target.display().to_string(), "byteSize": size}),
        ),
        Err(error) => refused(error),
    }
}

/// `POST /api/files/mkdir`: a folder (and its parents), for the web folder picker.
pub fn mkdir(target: &Path) -> Answer {
    match hermes_core::files::mkdir(target) {
        Ok(()) => (
            200,
            json!({"ok": true, "path": target.display().to_string().replace('\\', "/"),
                   "entry": {"name": target.file_name().map(|n| n.to_string_lossy().to_string()),
                             "path": target.display().to_string(), "isDirectory": true}}),
        ),
        Err(error) => refused(error),
    }
}

/// `GET /api/fs/git-root`: the repository folder holding `target`, or null.
pub fn git_root(target: &Path) -> Value {
    json!({"root": hermes_core::files::git_root(target).map(|root| root.display().to_string())})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routes_answer_hermes_shapes() {
        let dir = hermes_testdir::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let (status, read) = read_text(&file);
        assert_eq!(status, 200);
        assert_eq!(read["language"], "rust");
        assert_eq!(read["mimeType"], "text/plain");
        assert_eq!(read["byteSize"], 13);
        assert_eq!(read["truncated"], false);
        let env = dir.path().join(".env");
        std::fs::write(&env, "KEY=1").unwrap();
        let (status, refused) = read_text(&env);
        assert_eq!(status, 403);
        assert_eq!(
            refused["detail"],
            "That file holds credentials and isn't shown."
        );
        assert_eq!(read_text(&dir.path().join("none")).0, 404);
        let (status, saved) = write_text(&file, "x");
        assert_eq!((status, saved["byteSize"].clone()), (200, json!(1)));
        assert_eq!(write_text(&dir.path().join("missing/new.txt"), "x").0, 400);
        let made = dir.path().join("a/b");
        let (status, body) = mkdir(&made);
        assert_eq!(status, 200);
        assert_eq!(body["entry"]["name"], "b");
        assert_eq!(body["entry"]["isDirectory"], true);
        assert_eq!(mkdir(&file).0, 409);
        assert_eq!(git_root(&made)["root"], Value::Null);
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(git_root(&made)["root"], dir.path().display().to_string());
    }
}
