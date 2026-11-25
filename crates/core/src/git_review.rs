//! The review pane and the coding rail's git actions: changed files per scope, a file's diff,
//! stage / unstage / revert, commit (and push), the commit-message context, branches and
//! worktrees, and the GitHub ship flow through `gh`. Reads answer empty outside a repository;
//! a failed change is an `Err` with git's own message, which the page shows.
//!
//! Hermes: `hermes_cli/web_git.py` (all of it but `repo_status`, which is `git.rs`), served by
//! `hermes_cli/web_routers/git.py` (the routes and their JSON: `crates/desktop/src/git_review.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::files::is_sensitive;
use crate::git::{
    GIT_TIMEOUT, Ran, entries, git, git_line, renamed_to, run_limited, untracked_lines,
};

/// The commit-message context's diff cap (`_COMMIT_CONTEXT_DIFF_MAX_CHARS`) and untracked list
/// cap (`_COMMIT_CONTEXT_UNTRACKED_MAX`).
const CONTEXT_DIFF_MAX_CHARS: usize = 120_000;
const CONTEXT_UNTRACKED_MAX: usize = 80;
/// How much of `gh`'s error rides into the message (`_GH_ERR_TAIL_CHARS`).
const GH_ERROR_TAIL: usize = 400;

/// A change: `Err` with git's (or gh's) message, which the page shows.
type Outcome<T = ()> = Result<T, String>;

/// A changed file in the review pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
    /// `M`, `A`, `D`, `R`, `?` (untracked), `U` (conflicted)...
    pub status: String,
    pub staged: bool,
}

/// The review pane's changed files, and the commit they compare against (branch and last-turn
/// scopes).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewList {
    pub files: Vec<ReviewFile>,
    pub base: Option<String>,
}

/// What a commit message is drafted from: the diff that will commit, the recent subjects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitContext {
    pub diff: String,
    pub recent: String,
}

/// Whether `gh` is installed and signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhAuth {
    pub available: bool,
    pub authenticated: bool,
}

/// This branch's pull request, as `gh pr view` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPullRequest {
    pub url: String,
    pub state: Option<String>,
    pub number: Option<u64>,
}

/// The ship button's state: `gh` ready, and this branch's pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipInfo {
    pub gh_ready: bool,
    pub pr: Option<BranchPullRequest>,
}

/// A pull request on one of the asked-for branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub branch: String,
    pub draft: bool,
    pub number: u64,
    /// Lower case (`open`, `merged`, `closed`).
    pub state: String,
    pub title: String,
    pub url: String,
}

/// The pull requests found, and whether `gh` could be asked at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequests {
    pub gh_ready: bool,
    pub prs: Vec<PullRequest>,
}

/// One worktree of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub detached: bool,
    pub locked: bool,
}

/// What a new worktree asks for (all optional; the page's fields, trimmed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeRequest {
    /// Its folder's name (`work-<hex>` when empty).
    pub name: String,
    /// Its branch (`hermes/<name>` when empty).
    pub branch: String,
    /// What to branch from.
    pub base: String,
    /// Check out this existing branch instead.
    pub existing_branch: String,
}

/// A worktree made (or the main tree switched, for the trunk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedWorktree {
    pub path: String,
    pub branch: String,
    pub repo_root: String,
}

/// A branch to switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub checked_out: bool,
    pub is_default: bool,
    pub is_remote: bool,
    /// The worktree it is checked out in.
    pub worktree_path: Option<String>,
}

/// A branch to branch from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBranch {
    pub name: String,
    pub is_remote: bool,
    pub is_default: bool,
}

/// How long `gh` may take (`gh pr create` talks to GitHub); `git` has [`GIT_TIMEOUT`].
const GH_TIMEOUT: Duration = Duration::from_secs(120);

/// `(succeeded, stdout, stderr)` of `program args` in `cwd`: no window, no stdin, no prompts (a
/// credential prompt could never be answered from the page). A child that runs past its limit is
/// killed and answers as a failure whose stderr says so.
fn run(program: &str, cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GH_PROMPT_DISABLED", "1");
    let limit = if program == "git" {
        GIT_TIMEOUT
    } else {
        GH_TIMEOUT
    };
    match run_limited(&mut command, limit) {
        Ran::Exited(ok, out, err) => (ok, out, err),
        Ran::TimedOut => (
            false,
            String::new(),
            format!(
                "{program} {} did not answer within {} s and was stopped",
                args.first().copied().unwrap_or(""),
                limit.as_secs()
            ),
        ),
        Ran::NotStarted(reason) => (false, String::new(), reason),
    }
}

/// A git change: `Err` with git's message when it fails (`_git_ok`).
fn git_ok(cwd: &Path, args: &[&str]) -> Result<(), String> {
    let (ok, out, err) = run("git", cwd, args);
    if ok {
        return Ok(());
    }
    // (Some refusals, such as "nothing to commit", are on stdout.)
    Err([err.trim(), out.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("git {} failed", args.join(" "))))
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let (ok, out) = git(cwd, args);
    if ok { out } else { String::new() }
}

fn status_z(cwd: &Path) -> (bool, String) {
    git(cwd, &["status", "--porcelain=v2", "-z"])
}

/// A tracked entry with its index (staged) code set (`_entry_staged`).
fn staged(tag: char, xy: &str) -> bool {
    matches!(tag, '1' | '2') && !matches!(xy.chars().next(), Some('.' | '?') | None)
}

fn has_staged(raw: &str) -> bool {
    entries(raw).iter().any(|(tag, xy, _)| staged(*tag, xy))
}

/// The review row's letter (`_status_letter`): `?`, `U`, else the index code, else the
/// worktree code (`M` when neither says).
fn status_letter(tag: char, xy: &str) -> String {
    if matches!(tag, '?' | 'u') {
        return tag.to_ascii_uppercase().to_string();
    }
    let mut codes = xy.chars();
    let (x, y) = (codes.next().unwrap_or('.'), codes.next().unwrap_or('.'));
    let code = if x != '.' { x } else { y };
    (if code == '.' { 'M' } else { code })
        .to_ascii_uppercase()
        .to_string()
}

/// `git diff --numstat args` by path: `(added, removed)`, binary files 0 (`_numstat`).
fn numstat(cwd: &Path, args: &[&str]) -> Vec<(String, (u64, u64))> {
    let mut command = vec!["diff", "--numstat"];
    command.extend_from_slice(args);
    git_out(cwd, &command)
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            let count = |p: &str| p.parse::<u64>().unwrap_or(0);
            (parts.len() == 3).then(|| (renamed_to(parts[2]), (count(parts[0]), count(parts[1]))))
        })
        .collect()
}

fn lookup(counts: &[(String, (u64, u64))], path: &str) -> (u64, u64) {
    counts
        .iter()
        .find(|(p, _)| p == path)
        .map_or((0, 0), |(_, c)| *c)
}

/// The merge-base with the remote default branch, for "all branch changes" (`_branch_base`).
fn branch_base(cwd: &Path) -> Option<String> {
    let head = git_line(cwd, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    let mut candidates: Vec<String> = Vec::new();
    if !head.is_empty() {
        candidates.push(head);
    }
    candidates.extend(["origin/main", "origin/master", "main", "master"].map(String::from));
    candidates
        .iter()
        .map(|reference| git_line(cwd, &["merge-base", "HEAD", reference]))
        .find(|base| !base.is_empty())
}

/// The changed files of a scope: `uncommitted` (the working tree), `branch` (since the merge-base
/// with the default branch) or `lastTurn` (since `base_ref`, plus untracked files).
pub fn review_list(cwd: &Path, scope: &str, base_ref: Option<&str>) -> ReviewList {
    let empty = ReviewList::default();
    if !cwd.is_dir() {
        return empty;
    }
    let file = |path: String, added, removed, status: &str, staged| ReviewFile {
        path,
        added,
        removed,
        status: status.to_string(),
        staged,
    };
    let (files, base) = if matches!(scope, "branch" | "lastTurn") {
        let base = if scope == "branch" {
            branch_base(cwd)
        } else {
            base_ref.map(str::to_string)
        };
        let Some(base) = base.filter(|b| !b.is_empty()) else {
            return empty;
        };
        let range = if scope == "branch" {
            format!("{base}...HEAD")
        } else {
            base.clone()
        };
        let mut files: Vec<ReviewFile> = numstat(cwd, &[&range])
            .into_iter()
            .map(|(path, (a, r))| file(path, a, r, "M", false))
            .collect();
        if scope == "lastTurn" {
            let seen: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
            for (tag, _, path) in entries(&status_z(cwd).1) {
                if tag == '?' && !seen.contains(&path) {
                    files.push(file(path, 0, 0, "?", false));
                }
            }
        }
        (files, Some(base))
    } else {
        let (ok, raw) = status_z(cwd);
        if !ok {
            return empty;
        }
        let (cached, worktree) = (numstat(cwd, &["--cached"]), numstat(cwd, &[]));
        let files = entries(&raw)
            .into_iter()
            .map(|(tag, xy, path)| {
                let (sa, sr) = lookup(&cached, &path);
                let (ua, ur) = lookup(&worktree, &path);
                let (status, staged) = (status_letter(tag, &xy), staged(tag, &xy));
                file(path, sa + ua, sr + ur, &status, staged)
            })
            .collect();
        (files, None)
    };
    let mut files = files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in &mut files {
        if file.status == "?" && file.added == 0 && file.removed == 0 {
            file.added = untracked_lines(cwd, &file.path);
        }
    }
    ReviewList { files, base }
}

/// An untracked file as an all-added diff (`_all_add_diff`; `--no-index` exits 1 by design).
fn all_added(cwd: &Path, file: &Path) -> String {
    if !cwd.join(file).is_file() {
        return String::new();
    }
    // Named relative to the repository folder, as git names tracked files.
    let file = file
        .strip_prefix(cwd)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/");
    run(
        "git",
        cwd,
        // Plain text whatever the user's config says (`color.ui = always` would put escape
        // codes in it; an external diff tool would run).
        &[
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "--",
            "/dev/null",
            &file,
        ],
    )
    .1
}

/// One file's diff, refusing a path that names a folder (`_single_file_diff`).
fn single_file_diff(cwd: &Path, args: &[&str], file: &Path) -> Result<String, String> {
    let literal = format!(":(literal){}", file.display());
    let mut names_args = vec!["diff"];
    names_args.extend_from_slice(args);
    names_args.extend(["--no-renames", "--name-only", "-z", "--", &literal]);
    let names = git_out(cwd, &names_args);
    if names.is_empty() {
        return Ok(String::new());
    }
    let root = PathBuf::from(git_line(cwd, &["rev-parse", "--show-toplevel"]));
    let target = normalized(&cwd.join(file));
    if names
        .split('\0')
        .filter(|n| !n.is_empty())
        .any(|name| normalized(&root.join(name)) != target)
    {
        return Err("Expected a single file, not a directory".into());
    }
    let mut diff_args = vec!["diff"];
    diff_args.extend_from_slice(args);
    diff_args.extend(["--no-renames", "--", &literal]);
    Ok(git_out(cwd, &diff_args))
}

/// A path compared case-insensitively with one separator (Windows), through its real location
/// when it exists.
fn normalized(path: &Path) -> String {
    let path = dunce_canonical(path);
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn dunce_canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|p| {
            let text = p.to_string_lossy().to_string();
            PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        })
        .unwrap_or_else(|_| path.components().collect())
}

/// A changed file's diff in a scope (`staged`: the index's, for the uncommitted scope).
pub fn review_diff(
    cwd: &Path,
    file: &Path,
    scope: &str,
    base_ref: Option<&str>,
    cached: bool,
) -> Outcome<String> {
    if !cwd.is_dir() {
        return Ok(String::new());
    }
    let diff = match scope {
        "branch" => match branch_base(cwd) {
            Some(base) => single_file_diff(cwd, &[&format!("{base}...HEAD")], file)?,
            None => String::new(),
        },
        "lastTurn" => match base_ref.filter(|b| !b.is_empty()) {
            Some(base) => single_file_diff(cwd, &[base], file)?,
            None => String::new(),
        },
        _ if cached => single_file_diff(cwd, &["--cached"], file)?,
        _ => {
            let worktree = single_file_diff(cwd, &[], file)?;
            if worktree.trim().is_empty() {
                all_added(cwd, file)
            } else {
                worktree
            }
        }
    };
    Ok(diff)
}

/// The empty tree: what a repository without a first commit is compared with.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// How many untracked files [`review_patch`] writes out as all-added diffs, and the size past
/// which one is left out (`review_list` still names it).
const PATCH_UNTRACKED_MAX: usize = 100;
const PATCH_UNTRACKED_BYTES: u64 = 1024 * 1024;

/// A scope's whole patch ([`review_patch`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewPatch {
    /// One unified, Git-style diff of every changed file, renames detected, untracked files as
    /// all-added files. Empty outside a repository or when nothing changed.
    pub patch: String,
    /// Untracked files left out (past the count or size caps).
    pub untracked_skipped: usize,
}

/// Every changed file of a scope as one patch, in one `git diff` (plus one `--no-index` diff per
/// untracked file, capped): `uncommitted` (the working tree against `HEAD`, or the empty tree in a
/// repository with no commit), `lastTurn` (against `base_ref`, which a front end took at the
/// turn's start) or `branch` (since the merge-base with the default branch). `Err` when `git`
/// failed to answer (it ran past [`GIT_TIMEOUT`] and was killed) or the base is no longer there.
/// Untracked files are not part of `branch`.
pub fn review_patch(cwd: &Path, scope: &str, base_ref: Option<&str>) -> Outcome<ReviewPatch> {
    if !cwd.is_dir() {
        return Ok(ReviewPatch::default());
    }
    let (inside, top) = crate::git::git_timed(cwd, &["rev-parse", "--show-toplevel"], GIT_TIMEOUT)?;
    if !inside || top.trim().is_empty() {
        return Ok(ReviewPatch::default());
    }
    // Everything from the repository's top: `git diff` names files from there whatever the
    // folder, and the untracked list must too (from a subfolder `ls-files` would name them
    // relative to it, and only those under it), so a path means one file for every caller.
    let top = PathBuf::from(top.trim().replace('/', "\\"));
    let cwd = top.as_path();
    let ask = |args: &[&str]| crate::git::git_timed(cwd, args, GIT_TIMEOUT);
    let target = match scope {
        "branch" => match branch_base(cwd) {
            Some(base) => format!("{base}...HEAD"),
            None => return Ok(ReviewPatch::default()),
        },
        "lastTurn" => match base_ref.filter(|b| !b.is_empty()) {
            Some(base) => base.to_string(),
            None => return Ok(ReviewPatch::default()),
        },
        _ => {
            if ask(&["rev-parse", "--verify", "--quiet", "HEAD"])?.0 {
                "HEAD".to_string()
            } else {
                EMPTY_TREE.to_string()
            }
        }
    };
    let (ok, mut patch) = ask(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "-M",
        &target,
        "--",
    ])?;
    if !ok {
        return Err(format!("git could not compare with {target}"));
    }
    let mut skipped = 0;
    if scope != "branch" {
        let (_, listed) = ask(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        for (at, relative) in listed.split('\0').filter(|n| !n.is_empty()).enumerate() {
            let big = std::fs::metadata(cwd.join(relative))
                .map(|m| m.len() > PATCH_UNTRACKED_BYTES)
                .unwrap_or(true);
            if at >= PATCH_UNTRACKED_MAX || big {
                skipped += 1;
                continue;
            }
            let added = all_added(cwd, Path::new(relative));
            if !added.is_empty() {
                if !patch.is_empty() && !patch.ends_with('\n') {
                    patch.push('\n');
                }
                patch.push_str(&added);
            }
        }
    }
    Ok(ReviewPatch {
        patch,
        untracked_skipped: skipped,
    })
}

/// The working tree against HEAD for the preview's diff view; an
/// all-added diff only for a genuinely untracked file (`file_diff_vs_head`).
pub fn file_diff(cwd: &Path, file: &Path) -> Outcome<String> {
    if !cwd.is_dir() {
        return Ok(String::new());
    }
    let head = single_file_diff(cwd, &["HEAD"], file)?;
    if !head.trim().is_empty() {
        return Ok(head);
    }
    let literal = format!(":(literal){}", file.display());
    let status = git_out(cwd, &["status", "--porcelain", "--", &literal]);
    Ok(if status.trim_start().starts_with("??") {
        all_added(cwd, file)
    } else {
        String::new()
    })
}

/// Stages one file, or everything.
pub fn stage(cwd: &Path, file: Option<&str>) -> Outcome {
    match file {
        Some(file) => git_ok(cwd, &["add", "--", file])?,
        None => git_ok(cwd, &["add", "-A"])?,
    }
    Ok(())
}

/// Unstages one file, or everything.
pub fn unstage(cwd: &Path, file: Option<&str>) -> Outcome {
    match file {
        Some(file) => git_ok(cwd, &["reset", "-q", "HEAD", "--", file])?,
        None => git_ok(cwd, &["reset", "-q", "HEAD"])?,
    }
    Ok(())
}

/// One file (or everything) back to the committed state (tracked files restored,
/// untracked ones removed).
pub fn revert(cwd: &Path, file: Option<&str>) -> Outcome {
    let target = file.unwrap_or(".");
    run("git", cwd, &["checkout", "HEAD", "--", target]);
    run("git", cwd, &["clean", "-fd", "--", target]);
    Ok(())
}

fn push_branch(cwd: &Path) -> Result<(), String> {
    if !git_line(
        cwd,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_empty()
    {
        return git_ok(cwd, &["push"]);
    }
    let branch = git_line(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if !branch.is_empty() && branch != "HEAD" {
        git_ok(cwd, &["push", "-u", "origin", &branch])?;
    }
    Ok(())
}

/// Commits (and pushes when `push`): everything is staged first when nothing is.
pub fn commit(cwd: &Path, message: &str, push: bool) -> Outcome {
    if message.trim().is_empty() {
        return Err("A commit needs a message.".into());
    }
    if !has_staged(&status_z(cwd).1) {
        git_ok(cwd, &["add", "-A"])?;
    }
    git_ok(cwd, &["commit", "-m", message])?;
    if push {
        push_branch(cwd)?;
    }
    Ok(())
}

/// Pushes the branch (setting its upstream the first time).
pub fn push(cwd: &Path) -> Outcome {
    push_branch(cwd)?;
    Ok(())
}

/// The diff that will commit and the recent subjects, for
/// drafting a message. Credential files are left out of the diff.
pub fn commit_context(cwd: &Path) -> CommitContext {
    let (ok, raw) = if cwd.is_dir() {
        status_z(cwd)
    } else {
        (false, String::new())
    };
    let repo = PathBuf::from(git_line(cwd, &["rev-parse", "--show-toplevel"]));
    if !ok || repo.as_os_str().is_empty() {
        return CommitContext::default();
    }
    let base: &[&str] = if has_staged(&raw) {
        &["diff", "--cached"]
    } else {
        &["diff", "HEAD"]
    };
    let mut names_args = base.to_vec();
    names_args.extend(["--no-renames", "--name-only", "-z"]);
    let allowed: Vec<String> = git_out(&repo, &names_args)
        .split('\0')
        .filter(|n| !n.is_empty() && !is_sensitive(&repo.join(n)))
        .map(|n| format!(":(literal){n}"))
        .collect();
    let mut diff = if allowed.is_empty() {
        String::new()
    } else {
        let mut args = base.to_vec();
        args.extend(["--no-renames", "--"]);
        args.extend(allowed.iter().map(String::as_str));
        git_out(&repo, &args)
    };
    if diff.len() > CONTEXT_DIFF_MAX_CHARS {
        let mut cut = CONTEXT_DIFF_MAX_CHARS;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        let omitted = diff.len() - cut;
        diff.truncate(cut);
        diff.push_str(&format!("\n# diff truncated: {omitted} chars omitted\n"));
    }
    let untracked: Vec<String> = entries(&raw)
        .into_iter()
        .filter(|(tag, _, path)| *tag == '?' && !is_sensitive(&repo.join(path)))
        .map(|(_, _, path)| path)
        .collect();
    if !untracked.is_empty() {
        diff.push_str("\n# New (untracked) files:\n");
        for path in untracked.iter().take(CONTEXT_UNTRACKED_MAX) {
            diff.push_str(&format!("#   {path}\n"));
        }
        if untracked.len() > CONTEXT_UNTRACKED_MAX {
            diff.push_str(&format!(
                "#   ... {} more omitted\n",
                untracked.len() - CONTEXT_UNTRACKED_MAX
            ));
        }
    }
    CommitContext {
        diff,
        recent: git_line(cwd, &["log", "-n", "10", "--pretty=format:%s"]),
    }
}

// ── the ship flow (gh) ────────────────────────────────────────────────────────────────────────

fn gh_json(cwd: &Path, args: &[&str]) -> Option<Value> {
    let (ok, out, _) = run("gh", cwd, args);
    if ok {
        serde_json::from_str(&out).ok()
    } else {
        None
    }
}

/// Whether `gh` is installed and signed in.
pub fn gh_auth() -> GhAuth {
    let cwd = std::env::temp_dir();
    let (ok, _, err) = run("gh", &cwd, &["auth", "status"]);
    let available = ok || !err.contains("program not found") && !err.contains("cannot find");
    GhAuth {
        available,
        authenticated: ok,
    }
}

/// `gh` ready, and this branch's pull request.
pub fn ship_info(cwd: &Path) -> ShipInfo {
    if !cwd.is_dir() || !run("gh", cwd, &["auth", "status"]).0 {
        return ShipInfo {
            gh_ready: false,
            pr: None,
        };
    }
    let pr = gh_json(cwd, &["pr", "view", "--json", "url,state,number"]).and_then(|pr| {
        Some(BranchPullRequest {
            url: pr["url"].as_str()?.to_string(),
            state: pr["state"].as_str().map(str::to_string),
            number: pr["number"].as_u64(),
        })
    });
    ShipInfo { gh_ready: true, pr }
}

const PR_FIELDS: &str = "number state isDraft isCrossRepository title url headRefName";

/// The pull requests on those branches
/// (own-repository ones), asked per branch through GraphQL (`review_pr_list`).
pub fn pr_list(cwd: &Path, branches: &[String], numbers: &[u64]) -> PullRequests {
    let not_ready = PullRequests {
        gh_ready: false,
        prs: Vec::new(),
    };
    let mut wanted: Vec<&String> = Vec::new();
    for branch in branches.iter().filter(|b| !b.is_empty()) {
        if !wanted.contains(&branch) && wanted.len() < 300 {
            wanted.push(branch);
        }
    }
    let numbers: Vec<u64> = numbers
        .iter()
        .copied()
        .filter(|n| *n > 0)
        .take(300)
        .collect();
    if !cwd.is_dir() || (wanted.is_empty() && numbers.is_empty()) {
        return not_ready;
    }
    let (ok, out, _) = run(
        "gh",
        cwd,
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
    );
    let Some((owner, name)) = out.trim().split_once('/').filter(|_| ok) else {
        return not_ready;
    };
    let quote = |s: &str| Value::String(s.to_string()).to_string();
    let mut prs = Vec::new();
    let mut chunks: Vec<(Vec<&String>, Vec<u64>)> = wanted
        .chunks(50)
        .map(|c| (c.to_vec(), Vec::new()))
        .collect();
    chunks.extend(numbers.chunks(50).map(|c| (Vec::new(), c.to_vec())));
    for (branch_chunk, number_chunk) in chunks {
        let mut fields: Vec<String> = branch_chunk
            .iter()
            .enumerate()
            .map(|(i, b)| {
                format!(
                    "b{i}: pullRequests(headRefName: {}, first: 5, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ {PR_FIELDS} }} }}",
                    quote(b)
                )
            })
            .collect();
        fields.extend(
            number_chunk
                .iter()
                .enumerate()
                .map(|(i, n)| format!("n{i}: pullRequest(number: {n}) {{ {PR_FIELDS} }}")),
        );
        let query = format!(
            "query={{ repository(owner: {}, name: {}) {{\n{}\n}} }}",
            quote(owner),
            quote(name),
            fields.join("\n")
        );
        let Some(data) = gh_json(cwd, &["api", "graphql", "-f", &query]) else {
            continue;
        };
        let Some(repository) = data["data"]["repository"].as_object() else {
            continue;
        };
        for (key, field) in repository {
            // By number: ours by construction. By branch: only own-repository nodes (a fork's
            // `main` would badge a trunk session with a stranger's PR).
            let pr = if key.starts_with('n') {
                Some(field.clone()).filter(|f| !f.is_null())
            } else {
                field["nodes"].as_array().and_then(|nodes| {
                    nodes
                        .iter()
                        .find(|n| n["isCrossRepository"] != true)
                        .cloned()
                })
            };
            if let Some(pr) = pr
                && let Some(branch) = pr["headRefName"].as_str()
            {
                prs.push(PullRequest {
                    branch: branch.to_string(),
                    draft: pr["isDraft"] == true,
                    number: pr["number"].as_u64().unwrap_or(0),
                    state: pr["state"].as_str().unwrap_or_default().to_lowercase(),
                    title: pr["title"].as_str().unwrap_or_default().to_string(),
                    url: pr["url"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
    }
    PullRequests {
        gh_ready: true,
        prs,
    }
}

/// Pushes, then `gh pr create --fill`: the new pull request's URL.
pub fn create_pr(cwd: &Path) -> Outcome<String> {
    let _ = push_branch(cwd);
    let (ok, out, err) = run("gh", cwd, &["pr", "create", "--fill"]);
    if !ok {
        let err = err.trim();
        let tail = &err[err.len().saturating_sub(GH_ERROR_TAIL)..];
        let tail = if tail.is_empty() {
            "is gh installed and authenticated?"
        } else {
            tail
        };
        return Err(format!("gh pr create failed: {tail}"));
    }
    let url = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    Ok(url.to_string())
}

// ── worktrees and branches ────────────────────────────────────────────────────────────────────

/// The repository's worktrees, the main one first.
pub fn worktrees(cwd: &Path) -> Vec<Worktree> {
    let mut trees: Vec<Worktree> = Vec::new();
    for line in git_out(cwd, &["worktree", "list", "--porcelain"]).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            trees.push(Worktree {
                path: path.trim().to_string(),
                branch: None,
                is_main: trees.is_empty(),
                detached: false,
                locked: false,
            });
        } else if let Some(tree) = trees.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                tree.branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
            } else if line == "detached" {
                tree.detached = true;
            } else if line.starts_with("locked") {
                tree.locked = true;
            }
        }
    }
    trees
}

fn main_root(cwd: &Path) -> PathBuf {
    worktrees(cwd)
        .first()
        .map(|t| PathBuf::from(&t.path))
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// A branch name git accepts, from what was typed (`_sanitize_branch`).
fn sanitize_branch(name: &str) -> String {
    let mut value: String = name.split_whitespace().collect::<Vec<_>>().join("-");
    value.retain(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'));
    for (from, to) in [("--", "-"), ("//", "/"), ("..", ".")] {
        while value.contains(from) {
            value = value.replace(from, to);
        }
    }
    value.trim_matches(['-', '.', '/']).to_string()
}

fn slugify(name: &str) -> String {
    let lowered = name.trim().to_lowercase();
    let mut slug = String::new();
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "work".into() } else { slug }
}

fn unique_dir(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }
    (2..)
        .map(|n| PathBuf::from(format!("{}-{n}", base.display())))
        .find(|p| !p.exists())
        .expect("a free name")
}

/// The trunk to branch from: origin's default, `init.defaultBranch`, else main or master.
fn trunk(cwd: &Path) -> String {
    let origin = git_line(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    );
    if !origin.is_empty() {
        return origin.trim_start_matches("origin/").to_string();
    }
    // The configured default counts only when the branch exists here (Hermes takes it as is, so
    // a global `master` would hide a repository's `main`).
    let configured = git_line(cwd, &["config", "--get", "init.defaultBranch"]);
    if !configured.is_empty()
        && git(
            cwd,
            &["show-ref", "--verify", &format!("refs/heads/{configured}")],
        )
        .0
    {
        return configured;
    }
    ["main", "master"]
        .into_iter()
        .find(|b| git(cwd, &["show-ref", "--verify", &format!("refs/heads/{b}")]).0)
        .unwrap_or_default()
        .to_string()
}

/// A new project folder may not be a repository, or have no commit to branch from: make it one
/// with a root commit, so worktrees work (`_ensure_repo`).
fn ensure_repo(cwd: &Path) -> Result<(), String> {
    let needs_root = if git_line(cwd, &["rev-parse", "--is-inside-work-tree"]) != "true" {
        git_ok(cwd, &["init"])?;
        true
    } else {
        !git(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]).0
    };
    if needs_root {
        git_ok(
            cwd,
            &[
                "-c",
                "user.email=hermes@localhost",
                "-c",
                "user.name=Hermes",
                "commit",
                "--allow-empty",
                "-m",
                "Initial commit",
            ],
        )?;
    }
    Ok(())
}

/// A new worktree under the repository's `.worktrees` (or the main tree switched, for the trunk).
pub fn worktree_add(cwd: &Path, request: &WorktreeRequest) -> Outcome<AddedWorktree> {
    ensure_repo(cwd)?;
    let root = main_root(cwd);
    let added = |path: String, branch: String| AddedWorktree {
        path,
        branch,
        repo_root: root.display().to_string(),
    };
    let existing = request.existing_branch.trim().to_string();
    if !existing.is_empty() {
        let requested = sanitize_branch(&existing);
        if requested.is_empty() {
            return Err("Branch name is required.".into());
        }
        // "origin/feature" is a remote-tracking ref: check it out as a local branch tracking it.
        let remote = requested
            .split_once('/')
            .filter(|_| {
                git(
                    &root,
                    &[
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/remotes/{requested}"),
                    ],
                )
                .0
            })
            .map(|(remote, branch)| (remote.to_string(), branch.to_string()));
        let local = remote
            .as_ref()
            .map_or(requested.clone(), |(_, branch)| branch.clone());
        if remote.is_none() && local == trunk(&root) {
            git_ok(&root, &["switch", &local])?;
            return Ok(added(root.display().to_string(), local));
        }
        let target = unique_dir(root.join(".worktrees").join(slugify(&local)));
        let target_text = target.display().to_string().replace('\\', "/");
        if let Some((remote, branch)) = &remote {
            let _ = run("git", &root, &["fetch", remote, branch]);
            if !run(
                "git",
                &root,
                &[
                    "worktree",
                    "add",
                    "--track",
                    "-b",
                    &local,
                    &target_text,
                    &requested,
                ],
            )
            .0
            {
                git_ok(
                    &root,
                    &["worktree", "add", "-b", &local, &target_text, &requested],
                )?;
            }
        } else {
            git_ok(&root, &["worktree", "add", &target_text, &local])?;
        }
        return Ok(added(target_text, local));
    }
    let name = request.name.trim().to_string();
    // (Unnamed: `work-<8 hex>`, as Hermes's `work-{os.urandom(4).hex()}`.)
    let slug = if name.is_empty() {
        format!("work-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    } else {
        slugify(&name)
    };
    let branch = Some(sanitize_branch(request.branch.trim()))
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| format!("hermes/{slug}"));
    let target = unique_dir(root.join(".worktrees").join(&slug));
    let target_text = target.display().to_string().replace('\\', "/");
    let base = request.base.trim().to_string();
    let mut args = vec!["worktree", "add", "-b", &branch, &target_text];
    if !base.is_empty() {
        if let Some(remote_branch) = base.strip_prefix("origin/")
            && git(&root, &["check-ref-format", "--branch", remote_branch]).0
        {
            let _ = run("git", &root, &["fetch", "origin", remote_branch]);
        }
        if base.starts_with("origin/") {
            // A standalone local branch, not one tracking the base.
            args.push("--no-track");
        }
        args.push(&base);
    }
    let (ok, _, err) = run("git", &root, &args);
    if !ok {
        if !err.to_lowercase().contains("already exists") {
            return Err(Some(err.trim().to_string())
                .filter(|e| !e.is_empty())
                .unwrap_or("git worktree add failed".into()));
        }
        git_ok(&root, &["worktree", "add", &target_text, &branch])?;
    }
    Ok(added(target_text, branch))
}

/// Removes a worktree (`force`: with its changes).
pub fn worktree_remove(cwd: &Path, worktree: &str, force: bool) -> Outcome {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree);
    git_ok(&main_root(cwd), &args)
}

fn ref_names(cwd: &Path, patterns: &[&str], format: &str) -> Vec<String> {
    let format = format!("--format={format}");
    let mut args = vec!["for-each-ref", &format, "--sort=-committerdate"];
    args.extend_from_slice(patterns);
    git_out(cwd, &args)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Local branches first, then remote ones with no local branch yet.
pub fn branches(cwd: &Path) -> Vec<BranchInfo> {
    let locals = ref_names(cwd, &["refs/heads"], "%(refname:short)");
    if locals.is_empty() {
        return Vec::new();
    }
    let trees = worktrees(cwd);
    let checked_out = |name: &str| {
        trees
            .iter()
            .find(|t| t.branch.as_deref() == Some(name))
            .map(|t| t.path.clone())
    };
    let main = trunk(cwd);
    let mut out: Vec<BranchInfo> = locals
        .iter()
        .map(|name| {
            let path = checked_out(name);
            BranchInfo {
                name: name.clone(),
                checked_out: path.is_some(),
                is_default: !main.is_empty() && *name == main,
                is_remote: false,
                worktree_path: path,
            }
        })
        .collect();
    out.extend(
        ref_names(cwd, &["refs/remotes"], "%(refname:short)")
            .into_iter()
            .filter(|name| !name.ends_with("/HEAD"))
            .filter(|name| {
                !locals
                    .iter()
                    .any(|l| Some(l.as_str()) == name.split_once('/').map(|(_, b)| b))
            })
            .map(|name| BranchInfo {
                name,
                checked_out: false,
                is_default: false,
                is_remote: true,
                worktree_path: None,
            }),
    );
    out
}

/// Local and remote branches to branch from; the default flagged.
pub fn base_branches(cwd: &Path) -> Vec<BaseBranch> {
    let names = ref_names(cwd, &["refs/heads", "refs/remotes"], "%(refname:short)");
    if names.is_empty() {
        return Vec::new();
    }
    let origin = git_line(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    );
    let default = if origin.is_empty() {
        trunk(cwd)
    } else {
        origin
    };
    names
        .into_iter()
        .map(|name| BaseBranch {
            is_remote: name.starts_with("origin/"),
            is_default: !default.is_empty() && name == default,
            name,
        })
        .collect()
}

/// Switches the working tree to a branch; the name it took.
pub fn branch_switch(cwd: &Path, branch: &str) -> Outcome<String> {
    let target = sanitize_branch(branch);
    if target.is_empty() {
        return Err("Branch name is required.".into());
    }
    git_ok(cwd, &["switch", &target])?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one commit, and git's identity set for it.
    fn repo() -> hermes_testdir::TestDir {
        let dir = hermes_testdir::tempdir().unwrap();
        let cwd = dir.path();
        git_ok(cwd, &["init", "-q", "-b", "main"]).unwrap();
        git_ok(cwd, &["config", "user.email", "test@example.com"]).unwrap();
        git_ok(cwd, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(cwd.join("a.txt"), "one\n").unwrap();
        git_ok(cwd, &["add", "-A"]).unwrap();
        git_ok(cwd, &["commit", "-q", "-m", "first"]).unwrap();
        dir
    }

    #[test]
    fn changes_are_listed_diffed_staged_and_committed() {
        let dir = repo();
        let cwd = dir.path();
        std::fs::write(cwd.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(cwd.join("new.txt"), "x\ny\n").unwrap();
        let list = review_list(cwd, "uncommitted", None);
        let files = &list.files;
        assert_eq!(files.len(), 2, "{list:?}");
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].added, 1);
        assert_eq!(files[1].status, "?");
        assert_eq!(files[1].added, 2, "an untracked file counts its lines");
        assert_eq!(list.base, None);

        let diff = review_diff(cwd, Path::new("a.txt"), "uncommitted", None, false).unwrap();
        assert!(diff.contains("+two"), "{diff}");
        let added = review_diff(cwd, Path::new("new.txt"), "uncommitted", None, false).unwrap();
        assert!(
            added.contains("+y"),
            "untracked: an all-added diff: {added}"
        );
        assert!(file_diff(cwd, Path::new("a.txt")).unwrap().contains("+two"));

        stage(cwd, Some("a.txt")).unwrap();
        let list = review_list(cwd, "uncommitted", None);
        assert!(list.files[0].staged);
        let cached = review_diff(cwd, Path::new("a.txt"), "uncommitted", None, true).unwrap();
        assert!(cached.contains("+two"));
        unstage(cwd, Some("a.txt")).unwrap();
        assert!(!review_list(cwd, "uncommitted", None).files[0].staged);

        let context = commit_context(cwd);
        assert!(context.diff.contains("+two"));
        assert!(context.diff.contains("#   new.txt"));
        assert_eq!(context.recent, "first");

        // Nothing staged: everything goes in.
        commit(cwd, "second", false).unwrap();
        assert!(review_list(cwd, "uncommitted", None).files.is_empty());
        assert!(commit(cwd, "  ", false).is_err());
    }

    #[test]
    fn revert_restores_and_removes() {
        let dir = repo();
        let cwd = dir.path();
        std::fs::write(cwd.join("a.txt"), "changed\n").unwrap();
        std::fs::write(cwd.join("stray.txt"), "x").unwrap();
        revert(cwd, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("a.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "one\n"
        );
        assert!(!cwd.join("stray.txt").exists());
    }

    #[test]
    fn a_scope_comes_as_one_patch_with_renames_deletes_binaries_and_untracked_files() {
        let dir = repo();
        let cwd = dir.path();
        let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        std::fs::write(cwd.join("old.txt"), &lines).unwrap();
        std::fs::write(cwd.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(cwd.join("logo.bin"), [0u8, 1, 2, 3]).unwrap();
        git_ok(cwd, &["add", "-A"]).unwrap();
        git_ok(cwd, &["commit", "-q", "-m", "more"]).unwrap();
        let base = git_line(cwd, &["rev-parse", "HEAD"]);

        git_ok(cwd, &["mv", "old.txt", "new name.txt"]).unwrap();
        std::fs::write(
            cwd.join("new name.txt"),
            lines.replace("line 3\n", "line three\n"),
        )
        .unwrap();
        std::fs::remove_file(cwd.join("gone.txt")).unwrap();
        std::fs::write(cwd.join("logo.bin"), [0u8, 9, 9, 9]).unwrap();
        std::fs::write(cwd.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(cwd.join("brand new.txt"), "x\ny\n").unwrap();

        let all = review_patch(cwd, "uncommitted", None).unwrap();
        let patch = &all.patch;
        assert_eq!(all.untracked_skipped, 0);
        assert!(patch.contains("rename from old.txt"), "{patch}");
        assert!(patch.contains("rename to new name.txt"), "{patch}");
        assert!(patch.contains("deleted file mode"), "{patch}");
        assert!(patch.contains("Binary files"), "{patch}");
        assert!(patch.contains("+two"), "{patch}");
        assert!(patch.contains("+++ b/brand new.txt"), "untracked: {patch}");
        assert!(
            patch.contains("-line 3") && patch.contains("+line three"),
            "{patch}"
        );

        // Since a commit taken at the turn's start: the same files, nothing committed since.
        let turn = review_patch(cwd, "lastTurn", Some(&base)).unwrap();
        assert_eq!(turn.patch, all.patch);
        // A turn that committed: its commit is in the patch, later edits too.
        git_ok(cwd, &["add", "-A"]).unwrap();
        git_ok(cwd, &["commit", "-q", "-m", "turn"]).unwrap();
        std::fs::write(cwd.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let turn = review_patch(cwd, "lastTurn", Some(&base)).unwrap().patch;
        assert!(
            turn.contains("+three") && turn.contains("rename to new name.txt"),
            "{turn}"
        );
        // The working tree alone is now just a.txt.
        let tree = review_patch(cwd, "uncommitted", None).unwrap().patch;
        assert!(
            tree.contains("+three") && !tree.contains("rename"),
            "{tree}"
        );

        // No base, an unknown base, a folder that is no repository, a missing folder.
        assert_eq!(
            review_patch(cwd, "lastTurn", None).unwrap(),
            ReviewPatch::default()
        );
        assert!(review_patch(cwd, "lastTurn", Some("deadbeef")).is_err());
        let plain = hermes_testdir::tempdir().unwrap();
        assert_eq!(
            review_patch(plain.path(), "uncommitted", None).unwrap(),
            ReviewPatch::default()
        );
        assert_eq!(
            review_patch(&cwd.join("nope"), "uncommitted", None).unwrap(),
            ReviewPatch::default()
        );
    }

    #[test]
    fn a_repository_with_no_commit_compares_with_the_empty_tree() {
        let dir = hermes_testdir::tempdir().unwrap();
        let cwd = dir.path();
        git_ok(cwd, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(cwd.join("staged.txt"), "s\n").unwrap();
        git_ok(cwd, &["add", "staged.txt"]).unwrap();
        std::fs::write(cwd.join("loose.txt"), "l\n").unwrap();
        let patch = review_patch(cwd, "uncommitted", None).unwrap().patch;
        assert!(
            patch.contains("+++ b/staged.txt") && patch.contains("+++ b/loose.txt"),
            "{patch}"
        );
    }

    #[test]
    fn from_a_subfolder_every_path_is_named_from_the_repository_top() {
        let dir = repo();
        let cwd = dir.path();
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("sub").join("loose.txt"), "l\n").unwrap();
        std::fs::write(cwd.join("top-loose.txt"), "t\n").unwrap();
        std::fs::write(cwd.join("a.txt"), "changed\n").unwrap();
        let patch = review_patch(&cwd.join("sub"), "uncommitted", None)
            .unwrap()
            .patch;
        // Untracked files as `git diff` names tracked ones: from the top, the whole repository.
        assert!(patch.contains("+++ b/sub/loose.txt"), "{patch}");
        assert!(patch.contains("+++ b/top-loose.txt"), "{patch}");
        assert!(patch.contains("+++ b/a.txt"), "{patch}");
        assert!(!patch.contains("+++ b/loose.txt"), "{patch}");
    }

    #[test]
    fn untracked_files_past_the_caps_are_counted_not_written() {
        let dir = repo();
        let cwd = dir.path();
        for n in 0..(PATCH_UNTRACKED_MAX + 3) {
            std::fs::write(cwd.join(format!("f{n:03}.txt")), "x\n").unwrap();
        }
        std::fs::write(
            cwd.join("huge.log"),
            vec![b'a'; PATCH_UNTRACKED_BYTES as usize + 1],
        )
        .unwrap();
        let out = review_patch(cwd, "uncommitted", None).unwrap();
        assert_eq!(
            out.untracked_skipped, 4,
            "three past the count, one too big"
        );
        assert_eq!(out.patch.matches("diff --git").count(), PATCH_UNTRACKED_MAX);
    }

    #[test]
    fn a_diff_of_a_folder_is_refused() {
        let dir = repo();
        let cwd = dir.path();
        std::fs::create_dir(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("sub/x.txt"), "1\n").unwrap();
        git_ok(cwd, &["add", "-A"]).unwrap();
        git_ok(cwd, &["commit", "-q", "-m", "sub"]).unwrap();
        std::fs::write(cwd.join("sub/x.txt"), "2\n").unwrap();
        assert!(review_diff(cwd, Path::new("sub"), "uncommitted", None, false).is_err());
    }

    #[test]
    fn branches_and_worktrees() {
        let dir = repo();
        let cwd = dir.path();
        git_ok(cwd, &["branch", "feature"]).unwrap();
        let list = branches(cwd);
        assert_eq!(list.len(), 2);
        let main = list.iter().find(|b| b.name == "main").unwrap();
        assert!(main.is_default && main.checked_out);
        assert!(base_branches(cwd).iter().any(|b| b.name == "feature"));

        let request = WorktreeRequest {
            name: "My Task".into(),
            ..WorktreeRequest::default()
        };
        let added = worktree_add(cwd, &request).unwrap();
        assert_eq!(added.branch, "hermes/my-task");
        assert!(Path::new(&added.path).is_dir());
        assert_eq!(worktrees(cwd).len(), 2);
        worktree_remove(cwd, &added.path, true).unwrap();
        assert_eq!(worktrees(cwd).len(), 1);

        assert_eq!(branch_switch(cwd, "feature").unwrap(), "feature");
        assert_eq!(
            git_line(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "feature"
        );
        assert!(branch_switch(cwd, "  ").is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(sanitize_branch(" my  new branch!! "), "my-new-branch");
        assert_eq!(sanitize_branch("a//b..c"), "a/b.c");
        assert_eq!(slugify("Fix: the THING"), "fix-the-thing");
        assert_eq!(slugify("!!!"), "work");
        assert_eq!(status_letter('1', ".M"), "M");
        assert_eq!(status_letter('1', "A."), "A");
        assert_eq!(status_letter('?', "??"), "?");
    }

    #[test]
    fn a_folder_that_is_not_a_repository_reads_empty() {
        let dir = hermes_testdir::tempdir().unwrap();
        assert!(
            review_list(dir.path(), "uncommitted", None)
                .files
                .is_empty()
        );
        assert!(branches(dir.path()).is_empty());
        assert_eq!(commit_context(dir.path()).diff, "");
    }
}
