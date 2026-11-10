//! The side panel's file preview and spot editor: reading a file as text, saving an edit, making a
//! folder, the git root of a path, and resolving the paths the page names (relative to the
//! session's folder, `~`, `file:` URLs).
//!
//! Hermes: `hermes_cli/web_routers/files.py` (`/api/fs/read-text`, `/api/fs/write-text`,
//! `/api/files/mkdir`, `/api/fs/git-root`, the sensitive-path guard and preview constants). The
//! byte routes (`/api/fs/download`, `/api/files/stream`) are in `server.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Larger files aren't previewed at all (`_FS_TEXT_SOURCE_MAX_BYTES`).
const SOURCE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The preview shows this much (`_FS_TEXT_PREVIEW_MAX_BYTES`); the editor opens only files that fit.
const PREVIEW_MAX_BYTES: u64 = 512 * 1024;
/// A save larger than this is refused (`_FS_TEXT_WRITE_MAX_BYTES`).
const WRITE_MAX_BYTES: usize = 8 * 1024 * 1024;

type Answer = (u16, Value);

fn error(status: u16, detail: impl Into<String>) -> Answer {
    (status, json!({"detail": detail.into()}))
}

/// The file a page path names: a `file:` URL, `~/…`, an absolute path, or a path relative to the
/// session's folder (`cwd`).
pub fn resolve(path: &str, cwd: Option<&Path>) -> PathBuf {
    let path = path.trim();
    let path = match path
        .strip_prefix("file://")
        .or_else(|| path.strip_prefix("file:"))
    {
        // `file:///C:/x` → `C:/x`; `file:///home/x` stays rooted.
        Some(rest) => {
            let decoded = crate::rest::percent_decode(rest);
            let bytes = decoded.as_bytes();
            if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':' {
                decoded[1..].to_string()
            } else {
                decoded
            }
        }
        None => path.to_string(),
    };
    if let Some(rest) = path.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with(['/', '\\']) {
            return dirs::home_dir()
                .unwrap_or_default()
                .join(rest.trim_start_matches(['/', '\\']));
        }
    }
    let candidate = PathBuf::from(&path);
    let joined = match cwd {
        Some(cwd) if candidate.is_relative() => cwd.join(candidate),
        _ => candidate,
    };
    // One separator throughout (a session folder kept as `C:/…` joined with `src\…`).
    joined.components().collect()
}

/// Credential files and folders the preview never reads or serves (`_is_sensitive_path`).
pub fn is_sensitive(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let sensitive_name = name == ".env"
        || name.starts_with(".env.")
        || name == ".envrc"
        || [
            "auth.json",
            "auth.lock",
            "credentials",
            ".anthropic_oauth.json",
            "google_token.json",
            "google_oauth_pending.json",
            "google_oauth.json",
            "webhook_subscriptions.json",
            ".git-credentials",
        ]
        .contains(&name.as_str());
    sensitive_name
        || path.components().any(|part| {
            let part = part.as_os_str().to_string_lossy().to_lowercase();
            part == "mcp-tokens" || part == "pairing"
        })
}

/// `_FS_PREVIEW_LANGUAGE_BY_EXT`.
fn language(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "c" | "h" => "c",
        "conf" => "ini",
        "cpp" | "hpp" => "cpp",
        "css" => "css",
        "csv" => "csv",
        "go" => "go",
        "graphql" => "graphql",
        "html" => "html",
        "java" => "java",
        "js" | "mjs" => "javascript",
        "json" => "json",
        "jsx" => "jsx",
        "kt" => "kotlin",
        "lua" => "lua",
        "md" => "markdown",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "sh" | "zsh" => "shell",
        "sql" => "sql",
        "svg" | "xml" => "xml",
        "toml" => "toml",
        "ts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        _ => "text",
    }
}

/// `_fs_mime_type`: the media types the preview plays or shows, else a guess by extension.
pub fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "avi" => "video/x-msvideo",
        "bmp" => "image/bmp",
        "flac" => "audio/flac",
        "gif" => "image/gif",
        "jpeg" | "jpg" => "image/jpeg",
        "m4a" => "audio/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "ogg" => "audio/ogg",
        "opus" => "audio/ogg; codecs=opus",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "md" | "txt" | "log" | "csv" | "rs" | "py" | "ts" | "tsx" | "toml" | "yaml" | "yml" => {
            "text/plain"
        }
        _ => "application/octet-stream",
    }
}

/// `_fs_looks_binary`: a NUL, or more than 12% control characters.
fn looks_binary(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    if data.contains(&0) {
        return true;
    }
    let suspicious = data
        .iter()
        .filter(|b| **b < 32 && !matches!(b, 9 | 10 | 13))
        .count();
    suspicious as f64 / data.len() as f64 > 0.12
}

/// A regular, non-sensitive file to read: its path and size, or the error answer.
pub fn readable(target: &Path) -> Result<u64, Answer> {
    if is_sensitive(target) {
        return Err(error(403, "That file holds credentials and isn't shown."));
    }
    match std::fs::metadata(target) {
        Ok(meta) if meta.is_file() => Ok(meta.len()),
        Ok(_) => Err(error(400, "Not a file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(error(404, "File not found")),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(error(403, "File is not readable"))
        }
        Err(e) => Err(error(400, e.to_string())),
    }
}

/// `GET /api/fs/read-text`: the file's text for the preview (the first 512 KB), whether it looks
/// binary, its language and type.
pub fn read_text(target: &Path) -> Answer {
    let size = match readable(target) {
        Ok(size) => size,
        Err(answer) => return answer,
    };
    if size > SOURCE_MAX_BYTES {
        return error(413, "File too large");
    }
    let data = match std::fs::File::open(target).and_then(|file| {
        use std::io::Read as _;
        let mut data = Vec::new();
        file.take(PREVIEW_MAX_BYTES).read_to_end(&mut data)?;
        Ok(data)
    }) {
        Ok(data) => data,
        Err(e) => return error(500, format!("Could not read the file: {e}")),
    };
    (
        200,
        json!({
            "binary": looks_binary(&data[..data.len().min(4096)]),
            "byteSize": size,
            "language": language(target),
            "mimeType": mime(target),
            "path": target.display().to_string(),
            "text": String::from_utf8_lossy(&data),
            "truncated": size > PREVIEW_MAX_BYTES,
        }),
    )
}

/// `POST /api/fs/write-text`: the spot editor's save. Only a regular file (or a new one in an
/// existing folder), written to a sibling and renamed over, so a crash can't truncate it.
pub fn write_text(target: &Path, content: &str) -> Answer {
    if content.len() > WRITE_MAX_BYTES {
        return error(413, "Content too large");
    }
    match std::fs::metadata(target) {
        Ok(meta) if meta.is_dir() => return error(400, "Path points to a directory"),
        Ok(meta) if !meta.is_file() => return error(400, "Only regular files can be written"),
        Ok(meta) if meta.permissions().readonly() => return error(403, "File is not writable"),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return error(400, e.to_string()),
    }
    if !target.parent().is_some_and(Path::is_dir) {
        return error(400, "Parent directory does not exist");
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let temp = target.with_file_name(format!(".{name}.hermes-tmp-{}", std::process::id()));
    let written = std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return match e.kind() {
            std::io::ErrorKind::PermissionDenied => error(403, "File is not writable"),
            _ => error(500, format!("Could not write file: {e}")),
        };
    }
    (
        200,
        json!({"ok": true, "path": target.display().to_string(), "byteSize": content.len()}),
    )
}

/// `POST /api/files/mkdir`: a folder (and its parents), for the web folder picker.
pub fn mkdir(target: &Path) -> Answer {
    if target.exists() && !target.is_dir() {
        return error(409, "A file already exists at that path");
    }
    match std::fs::create_dir_all(target) {
        Ok(()) => (
            200,
            json!({"ok": true, "path": target.display().to_string().replace('\\', "/"),
                   "entry": {"name": target.file_name().map(|n| n.to_string_lossy().to_string()),
                             "path": target.display().to_string(), "isDirectory": true}}),
        ),
        Err(e) => error(500, format!("Could not create directory: {e}")),
    }
}

/// `GET /api/fs/git-root`: the repository folder holding `target`, or null.
pub fn git_root(target: &Path) -> Value {
    let start = if target.is_dir() {
        target
    } else {
        target.parent().unwrap_or(target)
    };
    let root = start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(|dir| dir.display().to_string());
    json!({"root": root})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_against_the_session_folder() {
        let cwd = Path::new("C:/work/repo");
        assert_eq!(
            resolve("src/main.rs", Some(cwd)),
            PathBuf::from(r"C:\work\repo\src\main.rs")
        );
        assert_eq!(
            resolve("D:/other/x.txt", Some(cwd)),
            PathBuf::from(r"D:\other\x.txt")
        );
        assert_eq!(
            resolve("file:///C:/a%20b/x.md", None),
            PathBuf::from(r"C:\a b\x.md")
        );
        assert_eq!(
            resolve("~/notes.md", None),
            dirs::home_dir().unwrap().join("notes.md")
        );
    }

    #[test]
    fn credentials_are_not_previewed() {
        assert!(is_sensitive(Path::new("C:/repo/.env")));
        assert!(is_sensitive(Path::new("C:/repo/.env.local")));
        assert!(is_sensitive(Path::new("C:/home/mcp-tokens/server.json")));
        assert!(!is_sensitive(Path::new("C:/repo/src/env.rs")));
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "KEY=1").unwrap();
        assert_eq!(read_text(&env).0, 403);
    }

    #[test]
    fn a_file_is_read_edited_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let (status, read) = read_text(&file);
        assert_eq!(status, 200);
        assert_eq!(read["language"], "rust");
        assert_eq!(read["binary"], false);
        assert_eq!(read["text"], "fn main() {}\n");
        assert_eq!(write_text(&file, "fn main() { println!(); }\n").0, 200);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn main() { println!(); }\n"
        );
        // A new file in an existing folder, yes; in a missing one, no; a folder, never.
        assert_eq!(write_text(&dir.path().join("new.txt"), "x").0, 200);
        assert_eq!(write_text(&dir.path().join("missing/new.txt"), "x").0, 400);
        assert_eq!(write_text(dir.path(), "x").0, 400);
        // No temporary files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Binary content is flagged.
        let blob = dir.path().join("blob.bin");
        std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
        assert_eq!(read_text(&blob).1["binary"], true);
    }

    #[test]
    fn folders_and_git_roots() {
        let dir = tempfile::tempdir().unwrap();
        let made = dir.path().join("a/b");
        assert_eq!(mkdir(&made).0, 200);
        assert!(made.is_dir());
        assert_eq!(git_root(&made)["root"], Value::Null);
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(git_root(&made)["root"], dir.path().display().to_string());
    }
}
