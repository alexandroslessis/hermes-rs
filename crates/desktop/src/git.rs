//! The coding rail's git reads (Hermes's `hermes_cli/web_git.py`, which serves
//! `/api/git/status` and `/api/git/review/rev-parse` to a browser): the working tree's branch and
//! changes, and a ref's commit. Each runs system `git` without a window, a prompt or stdin; a folder
//! that isn't a repository reads as `None`.

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The changed-file list's cap (Hermes's `files[:200]`).
const FILE_CAP: usize = 200;
/// Untracked files whose lines are counted (`_UNTRACKED_SCAN_CAP`), and the size past which one
/// counts as 0 (`_UNTRACKED_LINE_MAX_BYTES`).
const UNTRACKED_SCAN_CAP: usize = 500;
const UNTRACKED_LINE_MAX_BYTES: u64 = 1024 * 1024;

/// `(succeeded, stdout)` of `git args` in `cwd`.
pub(crate) fn git(cwd: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        Err(_) => (false, String::new()),
    }
}

pub(crate) fn git_line(cwd: &Path, args: &[&str]) -> String {
    let (ok, out) = git(cwd, args);
    if ok {
        out.trim().to_string()
    } else {
        String::new()
    }
}

/// `git rev-parse <ref>` (HEAD by default): the commit, or `None` outside a repository.
pub fn rev_parse(cwd: &Path, reference: Option<&str>) -> Option<String> {
    Some(git_line(cwd, &["rev-parse", reference.unwrap_or("HEAD")])).filter(|sha| !sha.is_empty())
}

/// `old => new` and `dir/{old => new}/f` name the new path (`resolve_rename_path`).
pub(crate) fn renamed_to(raw: &str) -> String {
    let path = raw.trim();
    if !path.contains(" => ") {
        return path.to_string();
    }
    if let Some((head, tail)) = path.split_once('{') {
        if let Some((inner, suffix)) = tail.split_once('}') {
            let to = inner.split_once(" => ").map_or(inner, |(_, to)| to);
            return format!("{head}{to}{suffix}").replace("//", "/");
        }
    }
    path.rsplit(" => ")
        .next()
        .unwrap_or(path)
        .trim()
        .to_string()
}

/// One changed path of `git status --porcelain=v2 -z` as `(tag, xy, path)` (`_walk_entries`).
pub(crate) fn entries(raw: &str) -> Vec<(char, String, String)> {
    let mut out = Vec::new();
    let mut records = raw.split('\0');
    while let Some(record) = records.next() {
        let Some(tag) = record.chars().next() else {
            continue;
        };
        let fields: Vec<&str> = record.split(' ').collect();
        match tag {
            '?' => out.push(('?', "??".into(), record[2.min(record.len())..].to_string())),
            'u' => out.push((
                'u',
                fields.get(1).copied().unwrap_or("").into(),
                record.splitn(11, ' ').last().unwrap_or("").into(),
            )),
            '1' | '2' => {
                let path = record
                    .splitn(if tag == '1' { 9 } else { 10 }, ' ')
                    .last()
                    .unwrap_or("");
                if tag == '2' {
                    // A rename/copy's origin path is the next record.
                    records.next();
                }
                out.push((
                    tag,
                    fields.get(1).copied().unwrap_or("").into(),
                    renamed_to(path),
                ));
            }
            _ => {}
        }
    }
    out
}

/// `git diff --numstat` as `(added, removed)` sums; binary files count 0.
fn numstat_totals(cwd: &Path, args: &[&str]) -> (u64, u64) {
    let mut command = vec!["diff", "--numstat"];
    command.extend_from_slice(args);
    let (ok, out) = git(cwd, &command);
    if !ok {
        return (0, 0);
    }
    out.lines().fold((0, 0), |(added, removed), line| {
        let mut parts = line.split('\t');
        let count = |part: Option<&str>| part.and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
        (added + count(parts.next()), removed + count(parts.next()))
    })
}

/// An untracked file's line count (its `+N`); binary or oversized files count 0.
pub(crate) fn untracked_lines(cwd: &Path, relative: &str) -> u64 {
    let path = cwd.join(relative);
    let Ok(meta) = std::fs::metadata(&path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > UNTRACKED_LINE_MAX_BYTES {
        return 0;
    }
    let Ok(data) = std::fs::read(&path) else {
        return 0;
    };
    if data.contains(&0) {
        return 0;
    }
    let lines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    if !data.is_empty() && !data.ends_with(b"\n") {
        lines + 1
    } else {
        lines
    }
}

/// The repository's trunk ("main", "master"…), preferring origin's default branch.
pub(crate) fn default_branch(cwd: &Path) -> Option<String> {
    let head = git_line(cwd, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    if !head.is_empty() && head != "origin/HEAD" {
        return head.split_once('/').map(|(_, name)| name.to_string());
    }
    [
        "refs/heads/main",
        "refs/heads/master",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ]
    .into_iter()
    .find(|reference| git(cwd, &["rev-parse", "--verify", "--quiet", reference]).0)
    .and_then(|reference| reference.rsplit('/').next().map(str::to_string))
}

/// The coding rail's status (`repo_status`, `HermesRepoStatus`); `None` outside a repository.
pub fn repo_status(cwd: &Path) -> Option<Value> {
    if !cwd.is_dir() {
        return None;
    }
    let (ok, raw) = git(cwd, &["status", "--porcelain=v2", "--branch", "-z"]);
    if !ok {
        return None;
    }
    let (mut branch, mut detached, mut ahead, mut behind) = (None, false, 0i64, 0i64);
    for record in raw.split('\0') {
        if let Some(head) = record.strip_prefix("# branch.head ") {
            detached = head == "(detached)";
            branch = (!detached).then(|| head.to_string());
        } else if let Some(counts) = record.strip_prefix("# branch.ab ") {
            for token in counts.split_whitespace() {
                if let Some(n) = token.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = token.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        }
    }
    let files: Vec<Value> = entries(&raw)
        .into_iter()
        .map(|(tag, xy, path)| {
            let mut codes = xy.chars();
            let (x, y) = (codes.next().unwrap_or('.'), codes.next().unwrap_or('.'));
            let tracked = tag == '1' || tag == '2';
            json!({
                "path": path,
                "staged": tracked && x != '.' && x != '?',
                "unstaged": tag == '?' || (tracked && y != '.' && y != '?'),
                "untracked": tag == '?',
                "conflicted": tag == 'u',
            })
        })
        .collect();
    let count = |flag: &str| files.iter().filter(|f| f[flag] == true).count();
    let (mut added, removed) = numstat_totals(cwd, &["HEAD"]);
    // `git diff HEAD` leaves untracked files out: a new-file-only change would read +0.
    added += files
        .iter()
        .take(UNTRACKED_SCAN_CAP)
        .filter(|f| f["untracked"] == true)
        .map(|f| untracked_lines(cwd, f["path"].as_str().unwrap_or_default()))
        .sum::<u64>();
    Some(json!({
        "branch": branch,
        "defaultBranch": default_branch(cwd),
        "detached": detached,
        "ahead": ahead,
        "behind": behind,
        "staged": count("staged"),
        "unstaged": count("unstaged"),
        "untracked": count("untracked"),
        "conflicted": count("conflicted"),
        "changed": files.len(),
        "added": added,
        "removed": removed,
        "files": files.into_iter().take(FILE_CAP).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_records_name_each_changed_path() {
        let raw = "# branch.head main\0# branch.ab +2 -1\0\
                   1 .M N... 100644 100644 100644 aaa bbb src/main.rs\0\
                   2 R. N... 100644 100644 100644 aaa bbb R100 new name.rs\0old.rs\0\
                   ? notes.txt\0";
        let found = entries(raw);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0], ('1', ".M".to_string(), "src/main.rs".to_string()));
        // A rename's origin record is skipped; spaces in a path survive.
        assert_eq!(found[1].2, "new name.rs");
        assert_eq!(found[2], ('?', "??".to_string(), "notes.txt".to_string()));
        assert_eq!(renamed_to("dir/{a.rs => b.rs}"), "dir/b.rs");
    }

    #[test]
    fn this_repository_reports_a_branch() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let status = repo_status(repo).expect("the crate lives in a git repository");
        assert!(status["branch"].is_string() || status["detached"] == true);
        assert!(rev_parse(repo, None).is_some_and(|sha| sha.len() >= 40));
        assert!(repo_status(&std::env::temp_dir().join("hermes-no-such-dir")).is_none());
    }
}
