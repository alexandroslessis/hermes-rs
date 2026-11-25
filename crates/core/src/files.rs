//! The side panel's file preview and spot editor: reading a file as text, saving an edit, making a
//! folder, the git root of a path, and resolving the paths the page names (relative to the
//! session's folder, `~`, `file:` URLs).
//!
//! Hermes: `hermes_cli/web_routers/files.py` (`/api/fs/read-text`, `/api/fs/write-text`,
//! `/api/files/mkdir`, `/api/fs/git-root`, the sensitive-path guard and preview constants).
//! The byte routes (`/api/fs/download`, `/api/files/stream`) are in
//! `crates/desktop/src/server.rs`; the routes' JSON is `crates/desktop/src/files.rs`.

use std::path::{Path, PathBuf};

/// Larger files aren't previewed at all (`_FS_TEXT_SOURCE_MAX_BYTES`).
const SOURCE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The preview shows this much (`_FS_TEXT_PREVIEW_MAX_BYTES`); the editor opens only files that fit.
const PREVIEW_MAX_BYTES: u64 = 512 * 1024;
/// A save larger than this is refused (`_FS_TEXT_WRITE_MAX_BYTES`).
const WRITE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Why a file could not be read, written or made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// The request names the wrong thing (a folder, a missing parent, not a regular file).
    Invalid(String),
    /// Credentials, or a file the user may not read or write.
    Forbidden(String),
    NotFound(String),
    /// A file is in the way of a folder.
    Conflict(String),
    TooLarge(String),
    /// The disk refused.
    Failed(String),
}

impl FileError {
    /// The sentence for the user.
    pub fn message(&self) -> &str {
        match self {
            FileError::Invalid(text)
            | FileError::Forbidden(text)
            | FileError::NotFound(text)
            | FileError::Conflict(text)
            | FileError::TooLarge(text)
            | FileError::Failed(text) => text,
        }
    }
}

/// Decodes `%XX` escapes and `+` (a URL path or query part).
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
            let decoded = crate::files::percent_decode(rest);
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

/// Credential folders a project browser never enters, wherever they are (Hermes's
/// `HOME_CREDENTIAL_DIRS`, as the runtime's `@file:` references refuse them under home).
const CREDENTIAL_DIRS: [&str; 6] = [".ssh", ".aws", ".gnupg", ".kube", ".docker", ".azure"];

/// What a front end that browses a project (the native app's Files tab and finder) never reads or
/// opens: stricter than [`is_sensitive`] (Hermes's preview guard, which the page's routes keep
/// as Hermes has it). Also the runtime's secret files (`hermes_tools::redact::is_secret_file`:
/// private keys, `*.pem`, `*.key`, `.netrc`, `.npmrc`, shell profiles...), anything inside a
/// credential folder (`.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.azure`, `.config/gh`,
/// `.config/gcloud`), and git's own folder (`.git/config` can hold a token in a remote URL).
/// Names compare without case, as Windows does.
pub fn is_private(path: &Path) -> bool {
    if is_sensitive(path) || hermes_tools::redact::is_secret_file(path) {
        return true;
    }
    let parts: Vec<String> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    parts.iter().enumerate().any(|(n, part)| {
        part == ".git"
            || CREDENTIAL_DIRS.contains(&part.as_str())
            || (part == ".config"
                && parts
                    .get(n + 1)
                    .is_some_and(|next| next == "gh" || next == "gcloud"))
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

/// A regular, non-sensitive file to read: its size.
pub fn readable(target: &Path) -> Result<u64, FileError> {
    if is_sensitive(target) {
        return Err(FileError::Forbidden(
            "That file holds credentials and isn't shown.".into(),
        ));
    }
    match std::fs::metadata(target) {
        Ok(meta) if meta.is_file() => Ok(meta.len()),
        Ok(_) => Err(FileError::Invalid("Not a file".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(FileError::NotFound("File not found".into()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(FileError::Forbidden("File is not readable".into()))
        }
        Err(e) => Err(FileError::Invalid(e.to_string())),
    }
}

/// A file's preview: its first 512 KB as text, and what it is.
#[derive(Debug, Clone, PartialEq)]
pub struct TextPreview {
    pub path: PathBuf,
    pub text: String,
    /// It looks binary (a NUL, or many control characters near the start).
    pub binary: bool,
    pub byte_size: u64,
    pub language: &'static str,
    pub mime: &'static str,
    /// The file is longer than the preview.
    pub truncated: bool,
}

/// The file's text for the preview (the first 512 KB), whether it looks binary, its language and
/// type.
pub fn read_text(target: &Path) -> Result<TextPreview, FileError> {
    let size = readable(target)?;
    if size > SOURCE_MAX_BYTES {
        return Err(FileError::TooLarge("File too large".into()));
    }
    let data = std::fs::File::open(target)
        .and_then(|file| {
            use std::io::Read as _;
            let mut data = Vec::new();
            file.take(PREVIEW_MAX_BYTES).read_to_end(&mut data)?;
            Ok(data)
        })
        .map_err(|e| FileError::Failed(format!("Could not read the file: {e}")))?;
    Ok(TextPreview {
        path: target.to_path_buf(),
        binary: looks_binary(&data[..data.len().min(4096)]),
        byte_size: size,
        language: language(target),
        mime: mime(target),
        text: String::from_utf8_lossy(&data).into_owned(),
        truncated: size > PREVIEW_MAX_BYTES,
    })
}

/// The spot editor's save: only a regular file (or a new one in an existing folder), written to
/// a sibling and renamed over, so a crash can't truncate it. The bytes written.
pub fn write_text(target: &Path, content: &str) -> Result<usize, FileError> {
    if content.len() > WRITE_MAX_BYTES {
        return Err(FileError::TooLarge("Content too large".into()));
    }
    match std::fs::metadata(target) {
        Ok(meta) if meta.is_dir() => {
            return Err(FileError::Invalid("Path points to a directory".into()));
        }
        Ok(meta) if !meta.is_file() => {
            return Err(FileError::Invalid(
                "Only regular files can be written".into(),
            ));
        }
        Ok(meta) if meta.permissions().readonly() => {
            return Err(FileError::Forbidden("File is not writable".into()));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(FileError::Invalid(e.to_string())),
    }
    if !target.parent().is_some_and(Path::is_dir) {
        return Err(FileError::Invalid("Parent directory does not exist".into()));
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let temp = target.with_file_name(format!(".{name}.hermes-tmp-{}", std::process::id()));
    let written = std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                FileError::Forbidden("File is not writable".into())
            }
            _ => FileError::Failed(format!("Could not write file: {e}")),
        });
    }
    Ok(content.len())
}

/// A folder (and its parents), for the folder picker.
pub fn mkdir(target: &Path) -> Result<(), FileError> {
    if target.exists() && !target.is_dir() {
        return Err(FileError::Conflict(
            "A file already exists at that path".into(),
        ));
    }
    std::fs::create_dir_all(target)
        .map_err(|e| FileError::Failed(format!("Could not create directory: {e}")))
}

/// The repository folder holding `target` (the nearest with a `.git`), if any.
pub fn git_root(target: &Path) -> Option<PathBuf> {
    let start = if target.is_dir() {
        target
    } else {
        target.parent().unwrap_or(target)
    };
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
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
        assert_eq!(percent_decode("a+b%2Fc%zz"), "a b/c%zz");
    }

    #[test]
    fn credentials_are_not_previewed() {
        assert!(is_sensitive(Path::new("C:/repo/.env")));
        assert!(is_sensitive(Path::new("C:/repo/.env.local")));
        assert!(is_sensitive(Path::new("C:/home/mcp-tokens/server.json")));
        assert!(!is_sensitive(Path::new("C:/repo/src/env.rs")));
        let dir = hermes_testdir::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "KEY=1").unwrap();
        assert!(matches!(read_text(&env), Err(FileError::Forbidden(_))));
    }

    #[test]
    fn a_project_browser_also_keeps_out_keys_credential_folders_and_git_internals() {
        for private in [
            "C:/repo/.env",
            "C:/repo/deploy/id_rsa",
            "C:/repo/certs/server.PEM",
            "C:/repo/tls/site.key",
            "C:/Users/me/.netrc",
            "C:/Users/me/.npmrc",
            "C:/Users/me/.SSH/config",
            "C:/Users/me/.aws/credentials",
            "C:/Users/me/.docker/config.json",
            "C:/Users/me/.config/gh/hosts.yml",
            "C:/Users/me/.config/gcloud/application_default_credentials.json",
            "C:/repo/.git/config",
            "C:/repo/.GIT/hooks/pre-commit",
            "C:/repo/mcp-tokens/server.json",
        ] {
            assert!(is_private(Path::new(private)), "{private}");
        }
        for public in [
            "C:/repo/src/env.rs",
            "C:/repo/.gitignore",
            "C:/repo/.github/workflows/ci.yml",
            "C:/repo/docs/ssh.md",
            "C:/repo/.config/app.toml",
            "C:/repo/keys.rs",
        ] {
            assert!(!is_private(Path::new(public)), "{public}");
        }
    }

    #[test]
    fn a_file_is_read_edited_and_saved() {
        let dir = hermes_testdir::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let read = read_text(&file).unwrap();
        assert_eq!(read.language, "rust");
        assert!(!read.binary);
        assert_eq!(read.text, "fn main() {}\n");
        assert!(write_text(&file, "fn main() { println!(); }\n").is_ok());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn main() { println!(); }\n"
        );
        // A new file in an existing folder, yes; in a missing one, no; a folder, never.
        assert_eq!(write_text(&dir.path().join("new.txt"), "x"), Ok(1));
        assert!(matches!(
            write_text(&dir.path().join("missing/new.txt"), "x"),
            Err(FileError::Invalid(_))
        ));
        assert!(matches!(
            write_text(dir.path(), "x"),
            Err(FileError::Invalid(_))
        ));
        // No temporary files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Binary content is flagged.
        let blob = dir.path().join("blob.bin");
        std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
        assert!(read_text(&blob).unwrap().binary);
    }

    #[test]
    fn folders_and_git_roots() {
        let dir = hermes_testdir::tempdir().unwrap();
        let made = dir.path().join("a/b");
        assert_eq!(mkdir(&made), Ok(()));
        assert!(made.is_dir());
        assert_eq!(git_root(&made), None);
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(git_root(&made).as_deref(), Some(dir.path()));
        let file = dir.path().join("f");
        std::fs::write(&file, "").unwrap();
        assert!(matches!(mkdir(&file), Err(FileError::Conflict(_))));
    }
}
