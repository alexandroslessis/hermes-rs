//! Running system `git` without a window, a prompt or stdin, and the coding rail's git reads: the
//! working tree's branch and changes (`repo_status`), a ref's commit, the porcelain parsing the
//! review pane shares. A folder that isn't a repository answers empty.
//!
//! Hermes: `hermes_cli/web_git.py` (`_run_git`, `repo_status`, `_walk_entries`,
//! `resolve_rename_path`), which serves `/api/git/status` and `/api/git/review/rev-parse`.

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `CREATE_NO_WINDOW`: no console flashes for a child process.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long a `git` (or `gh`) child may run before it is killed. A repository on a dead network
/// share, an `fsmonitor` that never answers or a credential helper waiting on a prompt would
/// otherwise block its caller (a background thread per call) for ever.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// How a limited child ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// It exited by itself: `(succeeded, stdout, stderr)`.
    Exited(bool, String, String),
    /// It ran past the limit and was killed.
    TimedOut,
    /// It could not be started: the reason.
    NotStarted(String),
}

/// Runs `command` with piped output and no stdin, killing it when it runs past `limit`. The
/// pipes are read on their own threads (a full pipe would stall the child); after a kill they are
/// given a moment and then abandoned, since a grandchild that inherited them (`git`'s helpers)
/// may hold them open for as long as it lives.
pub fn run_limited(command: &mut Command, limit: Duration) -> Ran {
    use std::sync::mpsc;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Ran::NotStarted(error.to_string()),
    };
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut data);
            }
            let _ = send.send(data);
        });
        receive
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // The whole tree: `git` starts helpers (credential managers, hooks, `ssh`,
                // pagers) that would otherwise live on, holding locks and the pipes. Found
                // through the live parent, so before the parent itself is killed.
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                // The pipes may still be open (a process that left the tree): a short grace,
                // then leave them.
                let _ = stdout.recv_timeout(Duration::from_millis(200));
                return Ran::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                kill_tree(child.id());
                let _ = child.kill();
                return Ran::NotStarted(error.to_string());
            }
        }
    };
    // The child has exited; what it wrote is in the pipes already unless a helper keeps them.
    let grace = Duration::from_secs(2).max(deadline.saturating_duration_since(Instant::now()));
    let text = |receive: mpsc::Receiver<Vec<u8>>| {
        receive
            .recv_timeout(grace)
            .map(|data| String::from_utf8_lossy(&data).into_owned())
            .unwrap_or_default()
    };
    let (out, err) = (text(stdout), text(stderr));
    Ran::Exited(status.success(), out, err)
}

/// Kills process `pid` and every process it started (`taskkill /T /F`, from System32 by its full
/// path so a `taskkill` on the `PATH` is never the one run). Called while `pid` is still alive:
/// `taskkill` finds the tree through the live parent.
fn kill_tree(pid: u32) {
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let taskkill = Path::new(&system).join("System32").join("taskkill.exe");
    let _ = Command::new(taskkill)
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// `(succeeded, stdout)` of `git args` in `cwd`, or `Err` with a reason when `git` could not be
/// started or ran past `limit` and was killed.
pub fn git_timed(cwd: &Path, args: &[&str], limit: Duration) -> Result<(bool, String), String> {
    git_program("git", cwd, args, limit)
}

/// [`git_timed`] for any `program` (the seam the timeout test uses with a slow script).
pub fn git_program(
    program: &str,
    cwd: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<(bool, String), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    match run_limited(&mut command, limit) {
        Ran::Exited(ok, out, _) => Ok((ok, out)),
        Ran::TimedOut => {
            tracing::warn!("{program} {} timed out after {limit:?}", args.join(" "));
            Err(format!(
                "{program} {} did not answer within {} s and was stopped",
                args.first().copied().unwrap_or(""),
                limit.as_secs()
            ))
        }
        Ran::NotStarted(reason) => Err(reason),
    }
}

/// `(succeeded, stdout)` of `git args` in `cwd`. A `git` that runs past [`GIT_TIMEOUT`] is killed
/// and answers as a failure (`(false, "")`), like one that is not installed.
pub fn git(cwd: &Path, args: &[&str]) -> (bool, String) {
    git_timed(cwd, args, GIT_TIMEOUT).unwrap_or((false, String::new()))
}

/// `git args`' output, trimmed; empty when it failed.
pub fn git_line(cwd: &Path, args: &[&str]) -> String {
    let (ok, out) = git(cwd, args);
    if ok {
        out.trim().to_string()
    } else {
        String::new()
    }
}

/// The changed-file list's cap (Hermes's `files[:200]`).
const FILE_CAP: usize = 200;
/// Untracked files whose lines are counted (`_UNTRACKED_SCAN_CAP`), and the size past which one
/// counts as 0 (`_UNTRACKED_LINE_MAX_BYTES`).
const UNTRACKED_SCAN_CAP: usize = 500;
const UNTRACKED_LINE_MAX_BYTES: u64 = 1024 * 1024;

/// `git rev-parse <ref>` (HEAD by default): the commit, or `None` outside a repository.
pub fn rev_parse(cwd: &Path, reference: Option<&str>) -> Option<String> {
    Some(git_line(cwd, &["rev-parse", reference.unwrap_or("HEAD")])).filter(|sha| !sha.is_empty())
}

/// `old => new` and `dir/{old => new}/f` name the new path (`resolve_rename_path`).
pub fn renamed_to(raw: &str) -> String {
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
pub fn entries(raw: &str) -> Vec<(char, String, String)> {
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
pub fn untracked_lines(cwd: &Path, relative: &str) -> u64 {
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

/// The repository's trunk ("main", "master"â€¦), preferring origin's default branch.
pub fn default_branch(cwd: &Path) -> Option<String> {
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

/// One changed path in the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflicted: bool,
}

/// The coding rail's status of a repository (`HermesRepoStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    /// `None` when detached.
    pub branch: Option<String>,
    pub default_branch: Option<String>,
    pub detached: bool,
    pub ahead: i64,
    pub behind: i64,
    /// Every changed path (the page shows the first [`RepoStatus::FILE_CAP`]).
    pub files: Vec<FileStatus>,
    /// Lines added and removed against `HEAD`, untracked files' lines included.
    pub added: u64,
    pub removed: u64,
}

impl RepoStatus {
    /// The changed-file list's cap (Hermes's `files[:200]`).
    pub const FILE_CAP: usize = FILE_CAP;

    /// How many changed paths `flag` holds for.
    pub fn count(&self, flag: impl Fn(&FileStatus) -> bool) -> usize {
        self.files.iter().filter(|f| flag(f)).count()
    }
}

/// The working tree's branch and changes (`repo_status`); `None` outside a repository.
pub fn repo_status(cwd: &Path) -> Option<RepoStatus> {
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
    let files: Vec<FileStatus> = entries(&raw)
        .into_iter()
        .map(|(tag, xy, path)| {
            let mut codes = xy.chars();
            let (x, y) = (codes.next().unwrap_or('.'), codes.next().unwrap_or('.'));
            let tracked = tag == '1' || tag == '2';
            FileStatus {
                path,
                staged: tracked && x != '.' && x != '?',
                unstaged: tag == '?' || (tracked && y != '.' && y != '?'),
                untracked: tag == '?',
                conflicted: tag == 'u',
            }
        })
        .collect();
    let (mut added, removed) = numstat_totals(cwd, &["HEAD"]);
    // `git diff HEAD` leaves untracked files out: a new-file-only change would read +0.
    added += files
        .iter()
        .take(UNTRACKED_SCAN_CAP)
        .filter(|f| f.untracked)
        .map(|f| untracked_lines(cwd, &f.path))
        .sum::<u64>();
    Some(RepoStatus {
        branch,
        default_branch: default_branch(cwd),
        detached,
        ahead,
        behind,
        files,
        added,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_child_is_killed_at_the_limit() {
        let started = Instant::now();
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        assert_eq!(
            run_limited(&mut command, Duration::from_millis(400)),
            Ran::TimedOut
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_hung_git_answers_with_an_error_even_when_a_helper_holds_its_pipes() {
        // A fake `git`: a script that starts a long-lived grandchild which inherits the pipes.
        let dir = hermes_testdir::tempdir().unwrap();
        let script = dir.path().join("slow-git.cmd");
        std::fs::write(&script, "@echo off\r\nping -n 60 127.0.0.1 > nul\r\n").unwrap();
        let started = Instant::now();
        let error = git_program(
            script.to_str().unwrap(),
            dir.path(),
            &["status"],
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_timeout_kills_the_whole_process_tree() {
        // A fake `git` whose grandchild would write a file three seconds later: killed with
        // its parent, it never does.
        let dir = hermes_testdir::tempdir().unwrap();
        let script = dir.path().join("tree-git.cmd");
        std::fs::write(
            &script,
            "@echo off\r\npowershell -NoProfile -Command \"Start-Sleep -Seconds 3; Set-Content -Path marker.txt -Value x\"\r\n",
        )
        .unwrap();
        let error = git_program(
            script.to_str().unwrap(),
            dir.path(),
            &["status"],
            Duration::from_millis(1500),
        )
        .unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !dir.path().join("marker.txt").exists(),
            "the grandchild outlived the timeout"
        );
    }

    #[test]
    fn a_quick_git_is_not_disturbed_by_the_limit() {
        let dir = hermes_testdir::tempdir().unwrap();
        let limit = Duration::from_secs(30);
        assert!(git_timed(dir.path(), &["init", "-q"], limit).unwrap().0);
        // A failure is still just `false`, and `git()` keeps its shape.
        assert!(!git(dir.path(), &["no-such-subcommand"]).0);
        let (ok, out) = git_timed(dir.path(), &["--version"], limit).unwrap();
        assert!(ok && out.starts_with("git version"));
        assert!(git_timed(dir.path(), &["--version"], limit).is_ok());
        assert!(git_program("no-such-program-xyz", dir.path(), &[], limit).is_err());
    }

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
        assert!(status.branch.is_some() || status.detached);
        assert!(rev_parse(repo, None).is_some_and(|sha| sha.len() >= 40));
        assert!(repo_status(&std::env::temp_dir().join("hermes-no-such-dir")).is_none());
    }

    #[test]
    fn a_new_repository_counts_its_untracked_lines() {
        let dir = hermes_testdir::tempdir().unwrap();
        let repo = dir.path();
        assert!(git(repo, &["init", "-q", "-b", "main"]).0);
        std::fs::write(repo.join("a.txt"), "one\ntwo").unwrap();
        let status = repo_status(repo).unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.files.len(), 1);
        assert!(status.files[0].untracked && status.files[0].unstaged);
        assert_eq!(status.count(|f| f.staged), 0);
        assert_eq!(status.added, 2);
    }
}
