//! Hermes's `@` references (`agent/context_references.py`, MIT): `@file:path[:start-end]`,
//! `@folder:path`, `@diff`, `@staged`, `@git:N` and `@url:` in a message attach that context
//! under it. The tokens stay where the user typed them; the attached blocks follow
//! "--- Attached Context ---", any problems "--- Context Warnings ---".

use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::hints::{canonical, is_under};

/// The operation name a message with expanded references is noted under (`typed`: what the
/// user wrote, which the chat shows).
pub const NOTE: &str = "references";

/// What the user typed, for a message whose references were expanded.
pub fn typed_text(message: &hermes_llm::conversation::message::Message) -> Option<&str> {
    message
        .metadata
        .operation_note(NOTE, "typed")
        .and_then(serde_json::Value::as_str)
}
const CHARS_PER_TOKEN: usize = 4;
/// At most this many references are expanded per message (`_MAX_EXPANDED_REFERENCES`).
const MAX_EXPANDED_REFERENCES: usize = 16;
/// Folder listings stop counting lines past this size (`_LINE_COUNT_MAX_BYTES`).
const LINE_COUNT_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// `git` output ceiling (`_MAX_QUIET_OUTPUT_BYTES`).
const MAX_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const FOLDER_LISTING_LIMIT: usize = 200;
const DEFAULT_CONTEXT_LENGTH: usize = 128_000;
/// Credential folders under home that are never attached (`HOME_CREDENTIAL_DIRS`).
const HOME_CREDENTIAL_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".config/gh",
    ".config/gcloud",
];
/// Single sensitive files under home (`_SENSITIVE_HOME_FILES`).
const SENSITIVE_HOME_FILES: &[&str] = &[
    ".bashrc",
    ".zshrc",
    ".profile",
    ".bash_profile",
    ".zprofile",
    ".netrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
];
const FENCE_LANGUAGES: &[(&str, &str)] = &[
    ("py", "python"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("tsx", "tsx"),
    ("jsx", "jsx"),
    ("json", "json"),
    ("md", "markdown"),
    ("sh", "bash"),
    ("yml", "yaml"),
    ("yaml", "yaml"),
    ("toml", "toml"),
    ("rs", "rust"),
    ("ps1", "powershell"),
    ("cs", "csharp"),
    ("cpp", "cpp"),
    ("c", "c"),
    ("h", "c"),
    ("hpp", "cpp"),
    ("go", "go"),
    ("java", "java"),
    ("html", "html"),
    ("css", "css"),
    ("sql", "sql"),
];
/// Extensions that are binary whatever their first bytes say (Hermes asks `mimetypes`).
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "tif", "tiff", "pdf", "zip", "gz", "tgz",
    "7z", "rar", "tar", "exe", "dll", "so", "dylib", "bin", "o", "obj", "lib", "a", "pdb", "class",
    "jar", "wasm", "mp3", "mp4", "wav", "ogg", "flac", "mov", "avi", "mkv", "woff", "woff2", "ttf",
    "otf", "sqlite", "db", "docx", "xlsx", "pptx",
];

const QUOTED: &str = r#"(?:`[^`\n]+`|"[^"\n]+"|'[^'\n]+')"#;

/// `REFERENCE_PATTERN`.
static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?<![\w/])@(?:(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>{QUOTED}(?::\d+(?:-\d+)?)?|\S+))"
    ))
    .expect("reference pattern")
});

/// `_FILE_VALUE_PATTERN`: a quoted or bare path with an optional `:start[-end]`.
static FILE_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^(?:(?P<quote>`|"|')(?P<qpath>.+?)(?P=quote)|(?P<path>.+?))(?::(?P<start>\d+)(?:-(?P<end>\d+))?)?$"#,
    )
    .expect("file value pattern")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The token as typed, e.g. `@file:src/main.rs:10-20`.
    pub raw: String,
    pub kind: String,
    pub target: String,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Expanded {
    /// What the model reads: the message, then warnings and attached context.
    pub message: String,
    pub warnings: Vec<String>,
    pub injected_tokens: usize,
    /// Over half the context window: the message isn't sent.
    pub blocked: bool,
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// `_strip_trailing_punctuation`: "(see @file:x.rs)." keeps neither the ")" nor the ".".
fn strip_trailing_punctuation(value: &str) -> &str {
    let mut stripped = value.trim_end_matches([',', '.', ';', '!', '?']);
    loop {
        let Some(last) = stripped.chars().last() else {
            break;
        };
        let opener = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        if stripped.matches(last).count() > stripped.matches(opener).count() {
            stripped = &stripped[..stripped.len() - 1];
        } else {
            break;
        }
    }
    stripped
}

fn strip_wrappers(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && bytes[0] == bytes[bytes.len() - 1]
        && matches!(bytes[0], b'`' | b'"' | b'\'')
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// `parse_context_references`.
pub fn parse(message: &str) -> Vec<Reference> {
    REFERENCE
        .captures_iter(message)
        .filter_map(Result::ok)
        .map(|captures| {
            let whole = captures.get(0).map_or("", |m| m.as_str());
            if let Some(simple) = captures.name("simple") {
                return Reference {
                    raw: whole.to_string(),
                    kind: simple.as_str().to_string(),
                    target: String::new(),
                    line_start: None,
                    line_end: None,
                };
            }
            let kind = captures.name("kind").map_or("", |m| m.as_str()).to_string();
            let raw_value = captures.name("value").map_or("", |m| m.as_str());
            let value = strip_trailing_punctuation(raw_value);
            let raw = whole[..whole.len() - (raw_value.len() - value.len())].to_string();
            let (target, line_start, line_end) = if kind == "file" {
                parse_file_value(value)
            } else {
                (strip_wrappers(value).to_string(), None, None)
            };
            Reference {
                raw,
                kind,
                target,
                line_start,
                line_end,
            }
        })
        .collect()
}

fn parse_file_value(value: &str) -> (String, Option<usize>, Option<usize>) {
    let Ok(Some(captures)) = FILE_VALUE.captures(value) else {
        return (strip_wrappers(value).to_string(), None, None);
    };
    let Some(start) = captures.name("start").and_then(|m| m.as_str().parse().ok()) else {
        return (strip_wrappers(value).to_string(), None, None);
    };
    let path = captures
        .name("qpath")
        .or_else(|| captures.name("path"))
        .map_or("", |m| m.as_str());
    let end = captures
        .name("end")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(start);
    (path.to_string(), Some(start), Some(end))
}

/// `format_reference_value`: quotes a path so the pattern reads it back whole.
pub fn format_value(value: &str) -> String {
    if !value.contains(|c: char| c.is_whitespace() || "()[]{}<>\"'`".contains(c)) {
        return value.to_string();
    }
    ["`", "\"", "'"]
        .iter()
        .find(|quote| !value.contains(**quote))
        .map_or_else(
            || value.to_string(),
            |quote| format!("{quote}{value}{quote}"),
        )
}

/// `preprocess_context_references`: expands the references in `message`. Paths must stay under
/// `cwd` (Hermes's default allowed root). `None` when the message has no references.
pub fn expand(
    message: &str,
    cwd: &Path,
    home: &Path,
    context_length: Option<usize>,
) -> Option<Expanded> {
    let references = parse(message);
    if references.is_empty() {
        return None;
    }
    let context_length = context_length
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_CONTEXT_LENGTH);
    let hard_limit = (context_length / 2).max(1);
    let soft_limit = (context_length / 4).max(1);
    let cwd = canonical(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut warnings = Vec::new();
    let mut blocks = Vec::new();
    for reference in references.iter().take(MAX_EXPANDED_REFERENCES) {
        match expand_one(reference, &cwd, home, hard_limit) {
            Ok(block) => blocks.push(block),
            Err(warning) => warnings.push(warning),
        }
    }
    warnings.extend(references.iter().skip(MAX_EXPANDED_REFERENCES).map(|r| {
        format!(
            "{}: not expanded (maximum {MAX_EXPANDED_REFERENCES} @-references per message)",
            r.raw
        )
    }));
    let injected_tokens = blocks.iter().map(|b| estimate_tokens(b)).sum();
    if injected_tokens > hard_limit {
        warnings.push(format!(
            "@ context injection refused: {injected_tokens} tokens exceeds the 50% hard limit ({hard_limit})."
        ));
        return Some(Expanded {
            message: message.to_string(),
            warnings,
            injected_tokens,
            blocked: true,
        });
    }
    if injected_tokens > soft_limit {
        warnings.push(format!(
            "@ context injection warning: {injected_tokens} tokens exceeds the 25% soft limit ({soft_limit})."
        ));
    }
    let mut out = message.to_string();
    if !warnings.is_empty() {
        out.push_str("\n\n--- Context Warnings ---\n");
        out.push_str(
            &warnings
                .iter()
                .map(|w| format!("- {w}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if !blocks.is_empty() {
        out.push_str("\n\n--- Attached Context ---\n\n");
        out.push_str(&blocks.join("\n\n"));
    }
    Some(Expanded {
        message: out.trim().to_string(),
        warnings,
        injected_tokens,
        blocked: false,
    })
}

/// One reference's attached block, or the warning saying why it has none.
fn expand_one(
    reference: &Reference,
    cwd: &Path,
    home: &Path,
    max_inline_tokens: usize,
) -> Result<String, String> {
    let raw = &reference.raw;
    match reference.kind.as_str() {
        "file" | "folder" => expand_path(reference, cwd, home, max_inline_tokens)
            .map_err(|error| format!("{raw}: {error}")),
        "diff" => git(raw, cwd, &["diff"]),
        "staged" => git(raw, cwd, &["diff", "--staged"]),
        "git" => {
            let count = reference.target.parse::<u32>().unwrap_or(1).clamp(1, 10);
            git(raw, cwd, &["log", &format!("-{count}"), "-p"])
        }
        "url" => Err(format!(
            "{raw}: web pages can't be attached yet (web extraction isn't available)"
        )),
        _ => Err(format!("{raw}: unsupported reference type")),
    }
}

/// `_resolve_path` + `_ensure_reference_path_allowed`.
fn resolve(target: &str, cwd: &Path, home: &Path) -> Result<PathBuf, String> {
    if target.starts_with(r"\\?\") || target.starts_with(r"\\.\") {
        return Err("path uses a Windows NT/device namespace prefix and cannot be attached".into());
    }
    let path = crate::paths::resolve(target, cwd, home);
    let path = canonical(&path).unwrap_or(path);
    if !is_under(&path, cwd) {
        return Err("path is outside the allowed workspace".into());
    }
    let home = canonical(home).unwrap_or_else(|| home.to_path_buf());
    let sensitive = SENSITIVE_HOME_FILES.iter().any(|file| {
        crate::display_path(&path).eq_ignore_ascii_case(&crate::display_path(&home.join(file)))
    }) || HOME_CREDENTIAL_DIRS
        .iter()
        .any(|dir| is_under(&path, &home.join(dir)))
        || crate::redact::is_secret_file(&path);
    if sensitive {
        return Err("path is a sensitive credential file and cannot be attached".into());
    }
    Ok(path)
}

fn expand_path(
    reference: &Reference,
    cwd: &Path,
    home: &Path,
    max_inline_tokens: usize,
) -> Result<String, String> {
    let folder = reference.kind == "folder";
    let path = resolve(&reference.target, cwd, home)?;
    if !path.exists() {
        return Err(format!("{} not found", reference.kind));
    }
    if folder != path.is_dir() {
        return Err(format!("path is not a {}", reference.kind));
    }
    if folder {
        let listing = folder_listing(&path, cwd);
        return Ok(format!(
            "📁 {} ({} tokens)\n{listing}",
            reference.raw,
            estimate_tokens(&listing)
        ));
    }
    if is_binary(&path) {
        let kind = path
            .extension()
            .map(|e| format!(".{} file", e.to_string_lossy()))
            .unwrap_or_else(|| "binary file".into());
        return Ok(on_disk_block(
            reference,
            &path,
            &kind,
            "binary file, not inlined as text.",
            "Use your tools to work with it (read or convert it, extract its text, or view/render it as needed); do not tell the user the file type is unsupported.",
        ));
    }
    let char_budget = max_inline_tokens * CHARS_PER_TOKEN;
    let text = match reference.line_start {
        Some(start) => {
            let end = reference.line_end.unwrap_or(start).max(start);
            let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut text = String::new();
            for line in std::io::BufReader::new(file)
                .lines()
                .skip(start.saturating_sub(1))
                .take(end - start + 1)
            {
                text.push_str(&line.map_err(|e| e.to_string())?);
                text.push('\n');
                if text.len() > char_budget {
                    return Ok(oversized_block(
                        reference,
                        &path,
                        text.len() / CHARS_PER_TOKEN,
                    ));
                }
            }
            text
        }
        None => {
            let size = std::fs::metadata(&path).map_or(0, |m| m.len()) as usize;
            if size > char_budget {
                return Ok(oversized_block(reference, &path, size / CHARS_PER_TOKEN));
            }
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            text.trim_start_matches('\u{feff}').to_string()
        }
    };
    let tokens = estimate_tokens(&text);
    if tokens > max_inline_tokens {
        return Ok(oversized_block(reference, &path, tokens));
    }
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let language = FENCE_LANGUAGES
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map_or("", |(_, lang)| lang);
    // Hermes fences the text as is; a trailing newline would leave a blank line before the fence.
    let text = text.strip_suffix('\n').unwrap_or(&text);
    Ok(format!(
        "📄 {} ({tokens} tokens)\n```{language}\n{text}\n```",
        reference.raw
    ))
}

fn is_binary(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if BINARY_EXTENSIONS.contains(&extension.as_str()) {
        return true;
    }
    let mut head = [0u8; 4096];
    std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .is_ok_and(|n| head[..n].contains(&0))
}

fn format_bytes(size: u64) -> String {
    match size {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// `_on_disk_reference_block`: not inlined, but on disk where the agent's tools run.
fn on_disk_block(
    reference: &Reference,
    path: &Path,
    descriptor: &str,
    reason: &str,
    guidance: &str,
) -> String {
    let size =
        std::fs::metadata(path).map_or_else(|_| "unknown size".into(), |m| format_bytes(m.len()));
    format!(
        "📎 {} ({descriptor}, {size}) — {reason} It is available on disk at `{}`. {guidance}",
        reference.raw,
        crate::display_path(path)
    )
}

fn oversized_block(reference: &Reference, path: &Path, tokens: usize) -> String {
    on_disk_block(
        reference,
        path,
        &format!("text file, approximately {tokens} tokens"),
        "too large to inline safely.",
        "Use read_file with a narrow line range, search_files, or terminal/code tools to inspect only the relevant parts; do not load the entire file into context.",
    )
}

/// `_build_folder_listing`: the folder's files (ignore files honored), nested by depth.
fn folder_listing(path: &Path, cwd: &Path) -> String {
    let shown = path
        .strip_prefix(cwd)
        .map(crate::display_path)
        .unwrap_or_else(|_| crate::display_path(path));
    let mut lines = vec![if shown.is_empty() {
        "./".to_string()
    } else {
        format!("{shown}/")
    }];
    let mut entries: Vec<PathBuf> = ignore::WalkBuilder::new(path)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() > 0)
        .map(|entry| entry.into_path())
        .take(FOLDER_LISTING_LIMIT)
        .collect();
    let truncated = entries.len() >= FOLDER_LISTING_LIMIT;
    entries.sort();
    for entry in &entries {
        let depth = entry
            .strip_prefix(path)
            .map_or(1, |relative| relative.components().count());
        let indent = "  ".repeat(depth.saturating_sub(1));
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if entry.is_dir() {
            lines.push(format!("{indent}- {name}/"));
        } else {
            lines.push(format!("{indent}- {name} ({})", file_metadata(entry)));
        }
    }
    if truncated {
        lines.push("- ...".into());
    }
    lines.join("\n")
}

/// `_file_metadata`: line count for text files, bytes otherwise.
fn file_metadata(path: &Path) -> String {
    let Ok(size) = std::fs::metadata(path).map(|m| m.len()) else {
        return "unknown size".into();
    };
    if size > LINE_COUNT_MAX_BYTES || is_binary(path) {
        return format!("{size} bytes");
    }
    match std::fs::read(path) {
        Ok(bytes) => format!(
            "{} lines",
            bytes.iter().filter(|&&b| b == b'\n').count() + 1
        ),
        Err(_) => format!("{size} bytes"),
    }
}

/// `_expand_git_reference`: git with repo config that could run code switched off (no external
/// diff, textconv, fsmonitor, hooks or pager), no prompts, no console window, bounded output.
fn git(raw: &str, cwd: &Path, args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt as _;
    let label = format!("git {}", args.join(" "));
    let mut command = std::process::Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=NUL",
            "-c",
            "core.pager=cat",
        ])
        .args(args.iter().take(1))
        .args(["--no-ext-diff", "--no-textconv", "--no-color"])
        .args(args.iter().skip(1))
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x0800_0000);
    let mut child = command.spawn().map_err(|error| format!("{raw}: {error}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_GIT_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut out);
        out
    });
    let err_reader = std::thread::spawn(move || {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err);
        err
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let Some(status) = status else {
        return Err(format!("{raw}: git command timed out (30s)"));
    };
    if !status.success() || out.len() > MAX_GIT_OUTPUT_BYTES {
        let message = err.trim();
        return Err(format!(
            "{raw}: {}",
            if message.is_empty() {
                "git command failed"
            } else {
                message
            }
        ));
    }
    let text = String::from_utf8_lossy(&out);
    let content = match text.trim() {
        "" => "(no output)",
        content => content,
    };
    Ok(format!(
        "🧾 {label} ({} tokens)\n```diff\n{content}\n```",
        estimate_tokens(content)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hermes_reference_forms() {
        let refs =
            parse("see @file:src/main.rs:10-20, @folder:`my dir` and @diff (also @file:a.rs).");
        assert_eq!(refs.len(), 4);
        assert_eq!(refs[0].raw, "@file:src/main.rs:10-20");
        assert_eq!(
            (
                refs[0].target.as_str(),
                refs[0].line_start,
                refs[0].line_end
            ),
            ("src/main.rs", Some(10), Some(20))
        );
        assert_eq!(refs[1].target, "my dir");
        assert_eq!(refs[2].kind, "diff");
        assert_eq!(refs[3].raw, "@file:a.rs");
        assert_eq!(refs[3].target, "a.rs");
        assert!(parse("mail me at a@file:x").is_empty());
        assert_eq!(format_value("my dir/a.rs"), "`my dir/a.rs`");
    }

    #[test]
    fn files_folders_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join(".env"), "KEY=x").unwrap();
        let expanded = expand(
            "fix @file:src/lib.rs:2 in @folder:src, not @file:.env or @file:../x.rs",
            dir.path(),
            home.path(),
            None,
        )
        .unwrap();
        assert!(!expanded.blocked);
        let message = &expanded.message;
        assert!(message.starts_with("fix @file:src/lib.rs:2 in @folder:src"));
        assert!(
            message.contains("📄 @file:src/lib.rs:2 (1 tokens)\n```rust\ntwo\n```"),
            "{message}"
        );
        assert!(message.contains("📁 @folder:src"));
        assert!(message.contains("- lib.rs (4 lines)"));
        assert!(message.contains("- @file:.env: path is a sensitive credential file"));
        assert!(message.contains("- @file:../x.rs: path is outside the allowed workspace"));
    }

    #[test]
    fn too_much_context_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "x".repeat(4_000)).unwrap();
        // Over half of a 1k window: the file goes on disk as a pointer.
        let expanded = expand("@file:big.txt", dir.path(), dir.path(), Some(1_000)).unwrap();
        assert!(
            expanded
                .message
                .contains("📎 @file:big.txt (text file, approximately 1000 tokens")
        );
        assert!(expand("no references", dir.path(), dir.path(), None).is_none());
    }
}

/// Files and folders under `cwd` for `@file:` / `@folder:` completion, relative with `/`
/// (ignore files honored, dependency and build trees skipped), at most `limit`.
pub fn workspace_entries(cwd: &Path, limit: usize) -> Vec<(String, bool)> {
    ignore::WalkBuilder::new(cwd)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            !crate::hints::EXCLUDED_DIR_NAMES.contains(&name.as_str())
        })
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() > 0)
        .take(limit)
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(cwd).ok()?;
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            Some((crate::display_path(relative), is_dir))
        })
        .collect()
}
