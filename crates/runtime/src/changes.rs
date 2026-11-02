//! The review pane's data: what changed in a session's folder, per file. With checkpoints on it's
//! everything since Hermes's first recorded edit (and a file can be put back); otherwise it's
//! git's view of the working tree (staged and unstaged, new files included).

use std::path::Path;

use hermes_tools::checkpoints::Checkpoints;

use crate::commands::git;

const MAX_UNTRACKED_FILES: usize = 50;

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

/// The changes in `cwd`, from checkpoints when they're on and exist, else from git.
pub fn changes(checkpoints: &Checkpoints, cwd: &Path, home: &Path) -> Changes {
    if checkpoints.enabled() {
        let workdir = checkpoints.working_dir_for(cwd, home);
        match checkpoints.session_diff(&workdir) {
            Some(Ok(diff)) => {
                return Changes {
                    source: Some(ChangesSource::Session),
                    files: parse_unified(&diff.diff),
                    note: None,
                };
            }
            Some(Err(error)) => {
                return Changes {
                    source: Some(ChangesSource::Session),
                    files: Vec::new(),
                    note: Some(error),
                };
            }
            // No checkpoint yet: git can still say what's changed.
            None => {}
        }
    }
    match git(&["rev-parse", "--is-inside-work-tree"], cwd) {
        None => return note("git is not installed or not on PATH."),
        Some((false, _)) => {
            return note(if checkpoints.enabled() {
                "Not a git repository, and no checkpoints here yet: they start with the agent's first edit in this folder."
            } else {
                "Not a git repository. Turn on checkpoints (Settings → Safety) to review the agent's edits anywhere."
            });
        }
        Some((true, _)) => {}
    }
    let base = ["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    // Against HEAD (staged and unstaged); a repository without commits has no HEAD.
    let mut body = git(&[&base[..], &["HEAD"]].concat(), cwd)
        .filter(|(ok, _)| *ok)
        .or_else(|| git(&base, cwd))
        .map(|(_, out)| out)
        .unwrap_or_default();
    let untracked: Vec<String> = git(&["ls-files", "--others", "--exclude-standard"], cwd)
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
) -> Result<(), String> {
    let workdir = checkpoints.working_dir_for(cwd, home);
    let oldest = checkpoints
        .list(&workdir)
        .pop()
        .ok_or("No checkpoints exist for this folder")?;
    checkpoints.revert_file(&workdir, &oldest.hash, file)
}

#[cfg(test)]
mod tests {
    use super::*;

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
