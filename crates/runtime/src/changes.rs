//! The review pane's data: what one session changed, per file. Only the files its own
//! `write_file` / `patch` calls touched count, so sessions sharing a folder each see their own
//! edits. With checkpoints on, a file is compared with the first checkpoint taken after the session
//! started (and can be put back); otherwise with git's HEAD.

use std::path::{Path, PathBuf};

use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_tools::checkpoints::Checkpoints;

use crate::commands::git;

const MAX_UNTRACKED_FILES: usize = 50;

/// A session: the files its tool calls wrote and when it started.
pub struct SessionScope {
    /// Absolute paths.
    pub files: Vec<PathBuf>,
    pub started_ms: i64,
}

/// The files the session's `write_file` and `patch` calls wrote, as absolute paths.
pub fn session_files(messages: &[Message], cwd: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut add = |path: &str| {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        let path = PathBuf::from(path.replace('/', "\\"));
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        if !files.iter().any(|f| same_path(f, &path)) {
            files.push(path);
        }
    };
    for message in messages {
        for content in &message.content {
            let MessageContent::ToolRequest(request) = content else {
                continue;
            };
            let Ok(call) = &request.tool_call else {
                continue;
            };
            if !matches!(call.name.as_ref(), "write_file" | "patch") {
                continue;
            }
            let Some(args) = &call.arguments else {
                continue;
            };
            if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                add(path);
            }
            // A multi-file patch names its files inside the patch text.
            if let Some(patch) = args.get("patch").and_then(|p| p.as_str()) {
                for line in patch.lines() {
                    for marker in ["*** Update File:", "*** Add File:", "*** Delete File:"] {
                        if let Some(path) = line.trim().strip_prefix(marker) {
                            add(path);
                        }
                    }
                }
            }
        }
    }
    files
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// A path as a git pathspec.
fn pathspec(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The session's baseline: the oldest checkpoint taken after it started.
fn baseline(checkpoints: &Checkpoints, workdir: &Path, started_ms: i64) -> Option<String> {
    checkpoints
        .list(workdir)
        .into_iter()
        .filter(|c| {
            chrono::DateTime::parse_from_rfc3339(&c.timestamp)
                .is_ok_and(|t| t.timestamp_millis() >= started_ms - 1000)
        })
        .last()
        .map(|c| c.hash)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesSource {
    /// Since the session's first checkpoint; files can be reverted.
    Session,
    /// `git diff HEAD` plus untracked files.
    Git,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub created: bool,
    pub deleted: bool,
    /// The hunks (from the first `@@`), or git's one-line note for a binary file.
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub source: Option<ChangesSource>,
    pub files: Vec<FileChange>,
    /// Why there's nothing to show (not a repository, …), or a problem getting the diff.
    pub note: Option<String>,
}

/// What the session changed: from its first checkpoint when checkpoints are on and one exists,
/// else from git.
pub fn changes(
    checkpoints: &Checkpoints,
    cwd: &Path,
    home: &Path,
    scope: &SessionScope,
) -> Changes {
    if scope.files.is_empty() {
        return Changes {
            source: None,
            files: Vec::new(),
            note: Some("This session hasn't edited any files yet.".into()),
        };
    }
    let specs: Vec<String> = scope.files.iter().map(|f| pathspec(f)).collect();
    if checkpoints.enabled() {
        let workdir = checkpoints.working_dir_for(cwd, home);
        if let Some(hash) = baseline(checkpoints, &workdir, scope.started_ms) {
            return match checkpoints.diff_paths(&workdir, &hash, &specs) {
                Ok(diff) => Changes {
                    source: Some(ChangesSource::Session),
                    files: parse_unified(&diff.diff),
                    note: None,
                },
                Err(error) => Changes {
                    source: Some(ChangesSource::Session),
                    files: Vec::new(),
                    note: Some(error),
                },
            };
        }
        // No checkpoint from this session: git can still say what's changed.
    }
    match git(&["rev-parse", "--is-inside-work-tree"], cwd) {
        None => return note("git is not installed or not on PATH."),
        Some((false, _)) => {
            return note(if checkpoints.enabled() {
                "Not a git repository, and this session took no checkpoint (they start with its first edit after checkpoints were turned on)."
            } else {
                "Not a git repository. Turn on checkpoints (Settings → Safety) to review the agent's edits anywhere."
            });
        }
        Some((true, _)) => {}
    }
    let with_specs = |args: &[&str]| -> Vec<String> {
        let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        all.push("--".into());
        all.extend(specs.iter().cloned());
        all
    };
    let base = ["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    // Against HEAD (staged and unstaged); a repository without commits has no HEAD.
    let mut body = git(&with_specs(&[&base[..], &["HEAD"]].concat()), cwd)
        .filter(|(ok, _)| *ok)
        .or_else(|| git(&with_specs(&base), cwd))
        .map(|(_, out)| out)
        .unwrap_or_default();
    let untracked: Vec<String> = git(
        &with_specs(&["ls-files", "--others", "--exclude-standard"]),
        cwd,
    )
    .map(|(_, out)| {
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default();
    for rel in untracked.iter().take(MAX_UNTRACKED_FILES) {
        if let Some((_, out)) = git(
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--no-index",
                "--",
                "NUL",
                rel,
            ],
            cwd,
        ) {
            body.push('\n');
            body.push_str(&out);
        }
    }
    Changes {
        source: Some(ChangesSource::Git),
        files: parse_unified(&body),
        note: None,
    }
}

fn note(text: &str) -> Changes {
    Changes {
        source: None,
        files: Vec::new(),
        note: Some(text.to_string()),
    }
}

/// Splits `git diff` output into files.
pub fn parse_unified(diff: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    let mut current: Option<(FileChange, bool)> = None;
    let strip = |path: &str| {
        let path = path.trim().trim_matches('"');
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_string()
    };
    let null = |path: &str| matches!(path.trim(), "/dev/null" | "NUL" | "nul");
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take().map(|(file, _)| file));
            // `a/x b/x`: the new name, until `+++` says otherwise.
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_else(|| strip(rest));
            current = Some((
                FileChange {
                    path,
                    added: 0,
                    removed: 0,
                    created: false,
                    deleted: false,
                    patch: String::new(),
                },
                false,
            ));
            continue;
        }
        let Some((file, in_hunks)) = current.as_mut() else {
            continue;
        };
        if *in_hunks {
            match line.chars().next() {
                Some('+') => file.added += 1,
                Some('-') => file.removed += 1,
                _ => {}
            }
            file.patch.push_str(line);
            file.patch.push('\n');
        } else if line.starts_with("@@") {
            *in_hunks = true;
            file.patch.push_str(line);
            file.patch.push('\n');
        } else if let Some(path) = line.strip_prefix("--- ") {
            file.created |= null(path);
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if null(path) {
                file.deleted = true;
            } else {
                file.path = strip(path);
            }
        } else if line.starts_with("new file mode") {
            file.created = true;
        } else if line.starts_with("deleted file mode") {
            file.deleted = true;
        } else if line.starts_with("Binary files") {
            file.patch = line.to_string();
        }
    }
    files.extend(current.map(|(file, _)| file));
    files.retain(|file| !file.patch.is_empty() || file.created || file.deleted);
    files
}

/// Puts one file back the way it was at the session's first checkpoint (a snapshot is taken
/// first, so `/rollback` can undo it).
pub fn revert_file(
    checkpoints: &Checkpoints,
    cwd: &Path,
    home: &Path,
    file: &str,
    started_ms: i64,
) -> Result<(), String> {
    let workdir = checkpoints.working_dir_for(cwd, home);
    let hash = baseline(checkpoints, &workdir, started_ms)
        .ok_or("This session has no checkpoint to revert to")?;
    checkpoints.revert_file(&workdir, &hash, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_owns_the_files_its_tools_wrote() {
        use rmcp::model::CallToolRequestParams;
        let call = |id: &str, name: &str, args: serde_json::Value| {
            let params = CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().cloned().unwrap_or_default());
            Message::assistant().with_tool_request(id, Ok(params))
        };
        let messages = vec![
            call("1", "read_file", serde_json::json!({"path": "a.txt"})),
            call(
                "2",
                "write_file",
                serde_json::json!({"path": "src/new.rs", "content": "x"}),
            ),
            call("3", "patch", serde_json::json!({"path": "src/new.rs"})),
            call(
                "4",
                "patch",
                serde_json::json!({"mode": "patch", "patch": "*** Begin Patch\n*** Update File: b.py\n@@\n-x\n+y\n*** End Patch"}),
            ),
        ];
        let cwd = Path::new(r"C:\proj");
        let files = session_files(&messages, cwd);
        assert_eq!(
            files,
            vec![
                PathBuf::from(r"C:\proj\src\new.rs"),
                PathBuf::from(r"C:\proj\b.py")
            ]
        );
        let none = changes(
            &Checkpoints::new(PathBuf::from(r"C:\nonexistent"), false),
            cwd,
            Path::new(r"C:\home"),
            &SessionScope {
                files: Vec::new(),
                started_ms: 0,
            },
        );
        assert!(none.note.unwrap().contains("hasn't edited"));
    }

    #[test]
    fn unified_diffs_split_per_file() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index 1..2 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-fn main() {}
+fn main() { run(); }
 // end
diff --git a/NUL b/notes.txt
new file mode 100644
--- /dev/null
+++ b/notes.txt
@@ -0,0 +1 @@
+hello
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";
        let files = parse_unified(diff);
        assert_eq!(files.len(), 4, "{files:#?}");
        assert_eq!(
            (files[0].path.as_str(), files[0].added, files[0].removed),
            ("src/main.rs", 1, 1)
        );
        assert!(files[0].patch.starts_with("@@ -1,2"));
        assert!(files[1].created && files[1].path == "notes.txt" && files[1].added == 1);
        assert!(files[2].deleted && files[2].path == "old.txt" && files[2].removed == 1);
        assert!(files[3].patch.starts_with("Binary files"));
    }
}
