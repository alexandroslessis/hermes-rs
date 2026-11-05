//! The review pane and the coding rail's git actions: changed files per scope, a file's diff,
//! stage / unstage / revert, commit (and push), the commit-message context, branches and
//! worktrees, and the GitHub ship flow through `gh`. Reads answer empty outside a repository;
//! a failed change is an `Err` with git's own message, which the page shows.
//!
//! Hermes: `hermes_cli/web_git.py` (all of it but `repo_status`, which is `git.rs`), served by
//! `hermes_cli/web_routers/git.py`.

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::git::{CREATE_NO_WINDOW, entries, git, git_line, renamed_to, untracked_lines};

/// The commit-message context's diff cap (`_COMMIT_CONTEXT_DIFF_MAX_CHARS`) and untracked list
/// cap (`_COMMIT_CONTEXT_UNTRACKED_MAX`).
const CONTEXT_DIFF_MAX_CHARS: usize = 120_000;
const CONTEXT_UNTRACKED_MAX: usize = 80;
/// How much of `gh`'s error rides into the message (`_GH_ERR_TAIL_CHARS`).
const GH_ERROR_TAIL: usize = 400;

type Outcome = Result<Value, String>;

/// `(succeeded, stdout, stderr)` of `program args` in `cwd`: no window, no stdin, no prompts (a
/// credential prompt could never be answered from the page).
fn run(program: &str, cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
        Err(error) => (false, String::new(), error.to_string()),
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
    (if code == '.' { 'M' } else { code }).to_ascii_uppercase().to_string()
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
    counts.iter().find(|(p, _)| p == path).map_or((0, 0), |(_, c)| *c)
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

/// `GET /api/git/review/list` `?scope=uncommitted|branch|lastTurn&base=`: the changed files.
pub fn review_list(cwd: &Path, scope: &str, base_ref: Option<&str>) -> Value {
    let empty = json!({"files": [], "base": null});
    if !cwd.is_dir() {
        return empty;
    }
    let (files, base) = if matches!(scope, "branch" | "lastTurn") {
        let base = if scope == "branch" { branch_base(cwd) } else { base_ref.map(str::to_string) };
        let Some(base) = base.filter(|b| !b.is_empty()) else {
            return empty;
        };
        let range = if scope == "branch" { format!("{base}...HEAD") } else { base.clone() };
        let mut files: Vec<Value> = numstat(cwd, &[&range])
            .into_iter()
            .map(|(path, (a, r))| json!({"path": path, "added": a, "removed": r, "status": "M", "staged": false}))
            .collect();
        if scope == "lastTurn" {
            let seen: Vec<String> = files.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect();
            for (tag, _, path) in entries(&status_z(cwd).1) {
                if tag == '?' && !seen.contains(&path) {
                    files.push(json!({"path": path, "added": 0, "removed": 0, "status": "?", "staged": false}));
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
                json!({"path": path, "added": sa + ua, "removed": sr + ur,
                       "status": status_letter(tag, &xy), "staged": staged(tag, &xy)})
            })
            .collect();
        (files, None)
    };
    let mut files = files;
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    for file in &mut files {
        if file["status"] == "?" && file["added"] == 0 && file["removed"] == 0 {
            file["added"] = json!(untracked_lines(cwd, file["path"].as_str().unwrap_or_default()));
        }
    }
    json!({"files": files, "base": base})
}

/// An untracked file as an all-added diff (`_all_add_diff`; `--no-index` exits 1 by design).
fn all_added(cwd: &Path, file: &Path) -> String {
    if !cwd.join(file).is_file() {
        return String::new();
    }
    // Named relative to the repository folder, as git names tracked files.
    let file = file.strip_prefix(cwd).unwrap_or(file).to_string_lossy().replace('\\', "/");
    run("git", cwd, &["diff", "--no-index", "--", "/dev/null", &file]).1
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

/// `GET /api/git/review/diff` `?file=&scope=&base=&staged=`: a changed file's diff.
pub fn review_diff(cwd: &Path, file: &Path, scope: &str, base_ref: Option<&str>, cached: bool) -> Outcome {
    if !cwd.is_dir() {
        return Ok(json!(""));
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
            if worktree.trim().is_empty() { all_added(cwd, file) } else { worktree }
        }
    };
    Ok(json!(diff))
}

/// `GET /api/git/file-diff`: the working tree against HEAD for the preview's diff view; an
/// all-added diff only for a genuinely untracked file (`file_diff_vs_head`).
pub fn file_diff(cwd: &Path, file: &Path) -> Outcome {
    if !cwd.is_dir() {
        return Ok(json!(""));
    }
    let head = single_file_diff(cwd, &["HEAD"], file)?;
    if !head.trim().is_empty() {
        return Ok(json!(head));
    }
    let literal = format!(":(literal){}", file.display());
    let status = git_out(cwd, &["status", "--porcelain", "--", &literal]);
    Ok(json!(if status.trim_start().starts_with("??") { all_added(cwd, file) } else { String::new() }))
}

/// `POST /api/git/review/stage` `{file?}`: one file, or everything.
pub fn stage(cwd: &Path, file: Option<&str>) -> Outcome {
    match file {
        Some(file) => git_ok(cwd, &["add", "--", file])?,
        None => git_ok(cwd, &["add", "-A"])?,
    }
    Ok(json!({"ok": true}))
}

/// `POST /api/git/review/unstage` `{file?}`.
pub fn unstage(cwd: &Path, file: Option<&str>) -> Outcome {
    match file {
        Some(file) => git_ok(cwd, &["reset", "-q", "HEAD", "--", file])?,
        None => git_ok(cwd, &["reset", "-q", "HEAD"])?,
    }
    Ok(json!({"ok": true}))
}

/// `POST /api/git/review/revert` `{file?}`: back to the committed state (tracked files restored,
/// untracked ones removed).
pub fn revert(cwd: &Path, file: Option<&str>) -> Outcome {
    let target = file.unwrap_or(".");
    run("git", cwd, &["checkout", "HEAD", "--", target]);
    run("git", cwd, &["clean", "-fd", "--", target]);
    Ok(json!({"ok": true}))
}

fn push_branch(cwd: &Path) -> Result<(), String> {
    if !git_line(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_empty() {
        return git_ok(cwd, &["push"]);
    }
    let branch = git_line(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if !branch.is_empty() && branch != "HEAD" {
        git_ok(cwd, &["push", "-u", "origin", &branch])?;
    }
    Ok(())
}

/// `POST /api/git/review/commit` `{message, push}`: everything is staged first when nothing is.
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
    Ok(json!({"ok": true}))
}

/// `POST /api/git/review/push`.
pub fn push(cwd: &Path) -> Outcome {
    push_branch(cwd)?;
    Ok(json!({"ok": true}))
}

/// `GET /api/git/review/commit-context`: the diff that will commit and the recent subjects, for
/// drafting a message. Credential files are left out of the diff.
pub fn commit_context(cwd: &Path) -> Value {
    let (ok, raw) = if cwd.is_dir() { status_z(cwd) } else { (false, String::new()) };
    let repo = PathBuf::from(git_line(cwd, &["rev-parse", "--show-toplevel"]));
    if !ok || repo.as_os_str().is_empty() {
        return json!({"diff": "", "recent": ""});
    }
    let base: &[&str] = if has_staged(&raw) { &["diff", "--cached"] } else { &["diff", "HEAD"] };
    let mut names_args = base.to_vec();
    names_args.extend(["--no-renames", "--name-only", "-z"]);
    let allowed: Vec<String> = git_out(&repo, &names_args)
        .split('\0')
        .filter(|n| !n.is_empty() && !crate::files::is_sensitive(&repo.join(n)))
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
        .filter(|(tag, _, path)| *tag == '?' && !crate::files::is_sensitive(&repo.join(path)))
        .map(|(_, _, path)| path)
        .collect();
    if !untracked.is_empty() {
        diff.push_str("\n# New (untracked) files:\n");
        for path in untracked.iter().take(CONTEXT_UNTRACKED_MAX) {
            diff.push_str(&format!("#   {path}\n"));
        }
        if untracked.len() > CONTEXT_UNTRACKED_MAX {
            diff.push_str(&format!("#   ... {} more omitted\n", untracked.len() - CONTEXT_UNTRACKED_MAX));
        }
    }
    json!({"diff": diff, "recent": git_line(cwd, &["log", "-n", "10", "--pretty=format:%s"])})
}

// ── the ship flow (gh) ────────────────────────────────────────────────────────────────────────

fn gh_json(cwd: &Path, args: &[&str]) -> Option<Value> {
    let (ok, out, _) = run("gh", cwd, args);
    if ok { serde_json::from_str(&out).ok() } else { None }
}

/// `GET /api/git/gh-auth`: whether `gh` is installed and signed in.
pub fn gh_auth() -> Value {
    let cwd = std::env::temp_dir();
    let (ok, _, err) = run("gh", &cwd, &["auth", "status"]);
    let available = ok || !err.contains("program not found") && !err.contains("cannot find");
    json!({"available": available, "authenticated": ok})
}

/// `GET /api/git/review/ship-info`: `gh` ready, and this branch's pull request.
pub fn ship_info(cwd: &Path) -> Value {
    if !cwd.is_dir() || !run("gh", cwd, &["auth", "status"]).0 {
        return json!({"ghReady": false, "pr": null});
    }
    let pr = gh_json(cwd, &["pr", "view", "--json", "url,state,number"])
        .filter(|pr| pr["url"].is_string())
        .map(|pr| json!({"url": pr["url"], "state": pr["state"], "number": pr["number"]}));
    json!({"ghReady": true, "pr": pr})
}

const PR_FIELDS: &str = "number state isDraft isCrossRepository title url headRefName";

/// `POST /api/git/review/pr-list` `{branches, numbers}`: the pull requests on those branches
/// (own-repository ones), asked per branch through GraphQL (`review_pr_list`).
pub fn pr_list(cwd: &Path, branches: &[String], numbers: &[u64]) -> Value {
    let not_ready = json!({"ghReady": false, "prs": []});
    let mut wanted: Vec<&String> = Vec::new();
    for branch in branches.iter().filter(|b| !b.is_empty()) {
        if !wanted.contains(&branch) && wanted.len() < 300 {
            wanted.push(branch);
        }
    }
    let numbers: Vec<u64> = numbers.iter().copied().filter(|n| *n > 0).take(300).collect();
    if !cwd.is_dir() || (wanted.is_empty() && numbers.is_empty()) {
        return not_ready;
    }
    let (ok, out, _) = run("gh", cwd, &["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"]);
    let Some((owner, name)) = out.trim().split_once('/').filter(|_| ok) else {
        return not_ready;
    };
    let quote = |s: &str| Value::String(s.to_string()).to_string();
    let mut prs = Vec::new();
    let mut chunks: Vec<(Vec<&String>, Vec<u64>)> = wanted.chunks(50).map(|c| (c.to_vec(), Vec::new())).collect();
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
        fields.extend(number_chunk.iter().enumerate().map(|(i, n)| format!("n{i}: pullRequest(number: {n}) {{ {PR_FIELDS} }}")));
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
                field["nodes"]
                    .as_array()
                    .and_then(|nodes| nodes.iter().find(|n| n["isCrossRepository"] != true).cloned())
            };
            if let Some(pr) = pr.filter(|pr| pr["headRefName"].is_string()) {
                prs.push(json!({
                    "branch": pr["headRefName"],
                    "draft": pr["isDraft"] == true,
                    "number": pr["number"].as_u64().unwrap_or(0),
                    "state": pr["state"].as_str().unwrap_or_default().to_lowercase(),
                    "title": pr["title"].as_str().unwrap_or_default(),
                    "url": pr["url"].as_str().unwrap_or_default(),
                }));
            }
        }
    }
    json!({"ghReady": true, "prs": prs})
}

/// `POST /api/git/review/create-pr`: push, then `gh pr create --fill`.
pub fn create_pr(cwd: &Path) -> Outcome {
    let _ = push_branch(cwd);
    let (ok, out, err) = run("gh", cwd, &["pr", "create", "--fill"]);
    if !ok {
        let err = err.trim();
        let tail = &err[err.len().saturating_sub(GH_ERROR_TAIL)..];
        let tail = if tail.is_empty() { "is gh installed and authenticated?" } else { tail };
        return Err(format!("gh pr create failed: {tail}"));
    }
    let url = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    Ok(json!({"url": url}))
}

// ── worktrees and branches ────────────────────────────────────────────────────────────────────

/// `GET /api/git/worktrees`: one entry per tree, the main one first.
pub fn worktrees(cwd: &Path) -> Vec<Value> {
    let mut trees: Vec<Value> = Vec::new();
    for line in git_out(cwd, &["worktree", "list", "--porcelain"]).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            trees.push(json!({"path": path.trim(), "branch": null, "isMain": trees.is_empty(),
                              "detached": false, "locked": false}));
        } else if let Some(tree) = trees.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                tree["branch"] = json!(branch.trim().trim_start_matches("refs/heads/"));
            } else if line == "detached" {
                tree["detached"] = json!(true);
            } else if line.starts_with("locked") {
                tree["locked"] = json!(true);
            }
        }
    }
    trees
}

fn main_root(cwd: &Path) -> PathBuf {
    worktrees(cwd)
        .first()
        .and_then(|t| t["path"].as_str().map(PathBuf::from))
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
    let origin = git_line(cwd, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]);
    if !origin.is_empty() {
        return origin.trim_start_matches("origin/").to_string();
    }
    // The configured default counts only when the branch exists here (Hermes takes it as is, so
    // a global `master` would hide a repository's `main`).
    let configured = git_line(cwd, &["config", "--get", "init.defaultBranch"]);
    if !configured.is_empty()
        && git(cwd, &["show-ref", "--verify", &format!("refs/heads/{configured}")]).0
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
        git_ok(cwd, &["-c", "user.email=hermes@localhost", "-c", "user.name=Hermes",
                      "commit", "--allow-empty", "-m", "Initial commit"])?;
    }
    Ok(())
}

/// `POST /api/git/worktree/add` `{name?, branch?, base?, existingBranch?}`: a new worktree under
/// the repository's `.worktrees` (or the main tree switched, for the trunk).
pub fn worktree_add(cwd: &Path, options: &Value) -> Outcome {
    ensure_repo(cwd)?;
    let root = main_root(cwd);
    let text = |key: &str| options[key].as_str().unwrap_or_default().trim().to_string();
    let existing = text("existingBranch");
    if !existing.is_empty() {
        let requested = sanitize_branch(&existing);
        if requested.is_empty() {
            return Err("Branch name is required.".into());
        }
        // "origin/feature" is a remote-tracking ref: check it out as a local branch tracking it.
        let remote = requested
            .split_once('/')
            .filter(|_| git(&root, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{requested}")]).0)
            .map(|(remote, branch)| (remote.to_string(), branch.to_string()));
        let local = remote.as_ref().map_or(requested.clone(), |(_, branch)| branch.clone());
        if remote.is_none() && local == trunk(&root) {
            git_ok(&root, &["switch", &local])?;
            return Ok(json!({"path": root.display().to_string(), "branch": local, "repoRoot": root.display().to_string()}));
        }
        let target = unique_dir(root.join(".worktrees").join(slugify(&local)));
        let target_text = target.display().to_string().replace('\\', "/");
        if let Some((remote, branch)) = &remote {
            let _ = run("git", &root, &["fetch", remote, branch]);
            if !run("git", &root, &["worktree", "add", "--track", "-b", &local, &target_text, &requested]).0 {
                git_ok(&root, &["worktree", "add", "-b", &local, &target_text, &requested])?;
            }
        } else {
            git_ok(&root, &["worktree", "add", &target_text, &local])?;
        }
        return Ok(json!({"path": target_text, "branch": local, "repoRoot": root.display().to_string()}));
    }
    let name = text("name");
    // (Unnamed: `work-<8 hex>`, as Hermes's `work-{os.urandom(4).hex()}`.)
    let slug = if name.is_empty() {
        format!("work-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    } else {
        slugify(&name)
    };
    let branch = Some(sanitize_branch(&text("branch")))
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| format!("hermes/{slug}"));
    let target = unique_dir(root.join(".worktrees").join(&slug));
    let target_text = target.display().to_string().replace('\\', "/");
    let base = text("base");
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
            return Err(Some(err.trim().to_string()).filter(|e| !e.is_empty()).unwrap_or("git worktree add failed".into()));
        }
        git_ok(&root, &["worktree", "add", &target_text, &branch])?;
    }
    Ok(json!({"path": target_text, "branch": branch, "repoRoot": root.display().to_string()}))
}

/// `POST /api/git/worktree/remove` `{worktreePath, force}`.
pub fn worktree_remove(cwd: &Path, worktree: &str, force: bool) -> Outcome {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree);
    git_ok(&main_root(cwd), &args)?;
    Ok(json!({"removed": worktree}))
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

/// `GET /api/git/branches`: local branches first, then remote ones with no local branch yet.
pub fn branches(cwd: &Path) -> Vec<Value> {
    let locals = ref_names(cwd, &["refs/heads"], "%(refname:short)");
    if locals.is_empty() {
        return Vec::new();
    }
    let trees = worktrees(cwd);
    let checked_out = |name: &str| {
        trees
            .iter()
            .find(|t| t["branch"] == name)
            .and_then(|t| t["path"].as_str().map(str::to_string))
    };
    let main = trunk(cwd);
    let mut out: Vec<Value> = locals
        .iter()
        .map(|name| {
            let path = checked_out(name);
            json!({"name": name, "checkedOut": path.is_some(), "isDefault": !main.is_empty() && *name == main,
                   "isRemote": false, "worktreePath": path})
        })
        .collect();
    out.extend(
        ref_names(cwd, &["refs/remotes"], "%(refname:short)")
            .into_iter()
            .filter(|name| !name.ends_with("/HEAD"))
            .filter(|name| !locals.iter().any(|l| Some(l.as_str()) == name.split_once('/').map(|(_, b)| b)))
            .map(|name| json!({"name": name, "checkedOut": false, "isDefault": false, "isRemote": true, "worktreePath": null})),
    );
    out
}

/// `GET /api/git/base-branches`: local and remote branches to branch from; the default flagged.
pub fn base_branches(cwd: &Path) -> Vec<Value> {
    let names = ref_names(cwd, &["refs/heads", "refs/remotes"], "%(refname:short)");
    if names.is_empty() {
        return Vec::new();
    }
    let origin = git_line(cwd, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]);
    let default = if origin.is_empty() { trunk(cwd) } else { origin };
    names
        .into_iter()
        .map(|name| json!({"name": name, "isRemote": name.starts_with("origin/"), "isDefault": !default.is_empty() && name == default}))
        .collect()
}

/// `POST /api/git/branch/switch` `{branch}`.
pub fn branch_switch(cwd: &Path, branch: &str) -> Outcome {
    let target = sanitize_branch(branch);
    if target.is_empty() {
        return Err("Branch name is required.".into());
    }
    git_ok(cwd, &["switch", &target])?;
    Ok(json!({"branch": target}))
}

/// The `/api/git/*` routes (`hermes_cli/web_routers/git.py`): `(status, body)`, `None` for one
/// this backend doesn't serve. A failed change answers 400 with git's message. Blocking: call it
/// off the async workers.
pub fn route(method: &str, path: &str, query: Option<&str>, body: &[u8]) -> Option<(u16, Value)> {
    let route = path.strip_prefix("/api/git/")?;
    let param = |name: &str| crate::rest::query_param(query, name).filter(|v| !v.is_empty());
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let field = |name: &str| body[name].as_str().filter(|v| !v.is_empty()).map(str::to_string);
    let folder = PathBuf::from(if method == "GET" { param("path") } else { field("path") }.unwrap_or_default());
    // A file named by the page, against the repository folder.
    let file = |name: Option<String>| crate::files::resolve(&name.unwrap_or_default(), Some(&folder));
    let outcome: Outcome = match (method, route) {
        ("GET", "status") => Ok(crate::git::repo_status(&folder).unwrap_or(Value::Null)),
        ("GET", "review/rev-parse") => Ok(json!({"sha": crate::git::rev_parse(&folder, param("ref").as_deref())})),
        ("GET", "gh-auth") => Ok(gh_auth()),
        ("GET", "worktrees") => Ok(json!({"worktrees": worktrees(&folder)})),
        ("GET", "branches") => Ok(json!({"branches": branches(&folder)})),
        ("GET", "base-branches") => Ok(json!({"branches": base_branches(&folder)})),
        ("GET", "review/list") => Ok(review_list(
            &folder,
            param("scope").as_deref().unwrap_or("uncommitted"),
            param("base").as_deref(),
        )),
        ("GET", "review/diff") => {
            let target = file(param("file"));
            if crate::files::is_sensitive(&target) {
                return Some((403, json!({"detail": "That file holds credentials and isn't shown."})));
            }
            review_diff(
                &folder,
                &target,
                param("scope").as_deref().unwrap_or("uncommitted"),
                param("base").as_deref(),
                param("staged").as_deref() == Some("true"),
            )
            .map(|diff| json!({"diff": diff}))
        }
        ("GET", "file-diff") => {
            let target = file(param("file"));
            if crate::files::is_sensitive(&target) {
                return Some((403, json!({"detail": "That file holds credentials and isn't shown."})));
            }
            file_diff(&folder, &target).map(|diff| json!({"diff": diff}))
        }
        ("GET", "review/commit-context") => Ok(commit_context(&folder)),
        ("GET", "review/ship-info") => Ok(ship_info(&folder)),
        ("POST", "review/stage") => stage(&folder, field("file").as_deref()),
        ("POST", "review/unstage") => unstage(&folder, field("file").as_deref()),
        ("POST", "review/revert") => revert(&folder, field("file").as_deref()),
        ("POST", "review/commit") => commit(
            &folder,
            body["message"].as_str().unwrap_or_default(),
            body["push"] == true,
        ),
        ("POST", "review/push") => push(&folder),
        ("POST", "review/create-pr") => create_pr(&folder),
        ("POST", "review/pr-list") => {
            let branches: Vec<String> = body["branches"]
                .as_array()
                .map(|b| b.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let numbers: Vec<u64> = body["numbers"]
                .as_array()
                .map(|n| n.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            Ok(pr_list(&folder, &branches, &numbers))
        }
        ("POST", "worktree/add") => worktree_add(&folder, &body),
        ("POST", "worktree/remove") => worktree_remove(
            &folder,
            &field("worktreePath").unwrap_or_default(),
            body["force"] == true,
        ),
        ("POST", "branch/switch") => branch_switch(&folder, &field("branch").unwrap_or_default()),
        _ => return None,
    };
    Some(match outcome {
        Ok(value) => (200, value),
        Err(detail) => (400, json!({"detail": detail})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one commit, and git's identity set for it.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
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
        let files = list["files"].as_array().unwrap();
        assert_eq!(files.len(), 2, "{list}");
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["status"], "M");
        assert_eq!(files[0]["added"], 1);
        assert_eq!(files[1]["status"], "?");
        assert_eq!(files[1]["added"], 2, "an untracked file counts its lines");

        let diff = review_diff(cwd, Path::new("a.txt"), "uncommitted", None, false).unwrap();
        assert!(diff.as_str().unwrap().contains("+two"), "{diff}");
        let added = review_diff(cwd, Path::new("new.txt"), "uncommitted", None, false).unwrap();
        assert!(added.as_str().unwrap().contains("+y"), "untracked: an all-added diff: {added}");
        assert!(file_diff(cwd, Path::new("a.txt")).unwrap().as_str().unwrap().contains("+two"));

        stage(cwd, Some("a.txt")).unwrap();
        let list = review_list(cwd, "uncommitted", None);
        assert_eq!(list["files"][0]["staged"], true);
        let cached = review_diff(cwd, Path::new("a.txt"), "uncommitted", None, true).unwrap();
        assert!(cached.as_str().unwrap().contains("+two"));
        unstage(cwd, Some("a.txt")).unwrap();
        assert_eq!(review_list(cwd, "uncommitted", None)["files"][0]["staged"], false);

        let context = commit_context(cwd);
        assert!(context["diff"].as_str().unwrap().contains("+two"));
        assert!(context["diff"].as_str().unwrap().contains("#   new.txt"));
        assert_eq!(context["recent"], "first");

        // Nothing staged: everything goes in.
        commit(cwd, "second", false).unwrap();
        assert!(review_list(cwd, "uncommitted", None)["files"].as_array().unwrap().is_empty());
        assert!(commit(cwd, "  ", false).is_err());
    }

    #[test]
    fn revert_restores_and_removes() {
        let dir = repo();
        let cwd = dir.path();
        std::fs::write(cwd.join("a.txt"), "changed\n").unwrap();
        std::fs::write(cwd.join("stray.txt"), "x").unwrap();
        revert(cwd, None).unwrap();
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap().replace("\r\n", "\n"), "one\n");
        assert!(!cwd.join("stray.txt").exists());
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
        let main = list.iter().find(|b| b["name"] == "main").unwrap();
        assert_eq!(main["isDefault"], true);
        assert_eq!(main["checkedOut"], true);
        assert!(base_branches(cwd).iter().any(|b| b["name"] == "feature"));

        let added = worktree_add(cwd, &json!({"name": "My Task"})).unwrap();
        assert_eq!(added["branch"], "hermes/my-task");
        assert!(Path::new(added["path"].as_str().unwrap()).is_dir());
        assert_eq!(worktrees(cwd).len(), 2);
        worktree_remove(cwd, added["path"].as_str().unwrap(), true).unwrap();
        assert_eq!(worktrees(cwd).len(), 1);

        branch_switch(cwd, "feature").unwrap();
        assert_eq!(git_line(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]), "feature");
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
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(review_list(dir.path(), "uncommitted", None)["files"], json!([]));
        assert!(branches(dir.path()).is_empty());
        assert_eq!(commit_context(dir.path())["diff"], "");
    }
}
