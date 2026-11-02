//! Hermes's filesystem checkpoints (`tools/checkpoint_manager.py`, MIT): before the first
//! file-changing tool call of a turn in a project, the project's files are committed to one shared
//! shadow git store (`checkpoints/store`, a bare repository with a ref and an index per project), so
//! `/rollback` can bring them back. The project's own `.git` is never touched, and the user's git
//! config isn't read (no signing prompts, hooks or credential helpers).
//!
//! An agent-write ledger records what each `write_file` / `patch` left, so a restore leaves files
//! the user changed by hand afterwards alone (`/rollback N --all` overrides).

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::hints::{canonical, is_under};

const REFS_PREFIX: &str = "refs/hermes";
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Directories with more files than this aren't snapshotted (`_MAX_FILES`).
const MAX_FILES: usize = 50_000;
/// Checkpoints kept per project (`max_snapshots`).
const MAX_SNAPSHOTS: usize = 20;
/// Files larger than this stay out of checkpoints (`max_file_size_mb`).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Newest ledger entries kept per project (`_LEDGER_MAX_ENTRIES`).
const LEDGER_MAX_ENTRIES: usize = 2000;
/// What makes a folder a project root (`get_working_dir_for_path`'s markers).
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "pyproject.toml",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "Makefile",
    "pom.xml",
    ".hg",
    "Gemfile",
];
/// `DEFAULT_EXCLUDES`, plus Windows build output.
const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules/",
    "dist/",
    "build/",
    "target/",
    "out/",
    ".next/",
    ".nuxt/",
    "__pycache__/",
    "*.pyc",
    "*.pyo",
    ".cache/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    "coverage/",
    ".coverage",
    ".venv/",
    "venv/",
    "env/",
    ".git/",
    ".hg/",
    ".svn/",
    ".worktrees/",
    "*.so",
    "*.dylib",
    "*.dll",
    "*.o",
    "*.a",
    "*.jar",
    "*.class",
    "*.exe",
    "*.obj",
    "*.pdb",
    "*.mp4",
    "*.mov",
    "*.mkv",
    "*.webm",
    "*.zip",
    "*.tar",
    "*.tar.gz",
    "*.tgz",
    "*.7z",
    "*.rar",
    "*.iso",
    ".env",
    ".env.*",
    ".env.local",
    ".env.*.local",
    ".DS_Store",
    "Thumbs.db",
    "*.log",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub hash: String,
    pub short_hash: String,
    /// ISO 8601 with offset.
    pub timestamp: String,
    pub reason: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Diff {
    pub stat: String,
    pub diff: String,
}

#[derive(Debug, Clone, Default)]
pub struct Restored {
    pub restored_to: String,
    pub reason: String,
    pub directory: PathBuf,
    pub file: Option<String>,
    /// Files changed by hand after Hermes last wrote them, left as they are.
    pub skipped_user_edits: Vec<String>,
    /// Files too large for checkpoints, which no checkpoint holds a copy of.
    pub skipped_oversize: Vec<String>,
    pub failed_deletes: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct LedgerEntry {
    hash: String,
    ts: i64,
}

#[derive(Serialize, Deserialize)]
struct ProjectMeta {
    workdir: String,
    created_at: i64,
    last_touch: i64,
}

/// The shared checkpoint store; one per app, used by every session.
pub struct Checkpoints {
    base: PathBuf,
    enabled: AtomicBool,
    /// Serializes git work on the store (Hermes's `store_lock`).
    lock: Mutex<()>,
}

/// A git run's (success, stdout, stderr).
type GitOutput = (bool, String, String);

impl Checkpoints {
    /// `base` is `<data>/checkpoints`; nothing is created until the first snapshot.
    pub fn new(base: PathBuf, enabled: bool) -> Self {
        Self {
            base,
            enabled: AtomicBool::new(enabled),
            lock: Mutex::new(()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn store(&self) -> PathBuf {
        self.base.join("store")
    }

    // ---- git --------------------------------------------------------------------------------

    /// Git against the store with `workdir` as the work tree and the project's own index.
    fn git(&self, args: &[&str], workdir: &Path, index: Option<&Path>) -> GitOutput {
        self.git_env(args, workdir, index, &[])
    }

    fn git_env(
        &self,
        args: &[&str],
        workdir: &Path,
        index: Option<&Path>,
        env: &[(&str, &str)],
    ) -> GitOutput {
        if !workdir.is_dir() {
            return (
                false,
                String::new(),
                format!("working directory not found: {}", workdir.display()),
            );
        }
        let mut command = isolated_git();
        command
            .args(args)
            .current_dir(workdir)
            .env("GIT_DIR", self.store())
            .env("GIT_WORK_TREE", workdir);
        match index {
            Some(index) => command.env("GIT_INDEX_FILE", index),
            None => command.env_remove("GIT_INDEX_FILE"),
        };
        for (key, value) in env {
            command.env(key, value);
        }
        run(command)
    }

    fn git_out(&self, args: &[&str], workdir: &Path) -> Option<String> {
        let (ok, out, _) = self.git(args, workdir, None);
        ok.then_some(out)
    }

    /// `_init_store`: a bare repository with its own identity, no signing, no auto-gc, and the
    /// default excludes.
    fn init_store(&self) -> Result<(), String> {
        let store = self.store();
        if store.join("HEAD").exists() {
            return Ok(());
        }
        for dir in ["indexes", "projects", "ledgers"] {
            std::fs::create_dir_all(store.join(dir)).map_err(|e| e.to_string())?;
        }
        let mut init = isolated_git();
        init.args(["init", "--bare"])
            .arg(&store)
            .current_dir(&self.base);
        let (ok, _, err) = run(init);
        if !ok {
            return Err(format!("Shadow store init failed: {err}"));
        }
        for (key, value) in [
            ("user.email", "hermes@local"),
            ("user.name", "Hermes Checkpoint"),
            ("commit.gpgsign", "false"),
            ("tag.gpgSign", "false"),
            ("gc.auto", "0"),
            ("core.autocrlf", "false"),
            ("core.longpaths", "true"),
        ] {
            self.git(&["config", key, value], &self.base, None);
        }
        std::fs::create_dir_all(store.join("info")).map_err(|e| e.to_string())?;
        std::fs::write(
            store.join("info/exclude"),
            DEFAULT_EXCLUDES.join("\n") + "\n",
        )
        .map_err(|e| e.to_string())
    }

    fn project_hash(workdir: &Path) -> String {
        // Stable across runs: FNV-1a of the lowercased path (Hermes: sha256(path)[:16]).
        let key = workdir.to_string_lossy().replace('\\', "/").to_lowercase();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in key.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    fn index_path(&self, workdir: &Path) -> PathBuf {
        self.store()
            .join("indexes")
            .join(Self::project_hash(workdir))
    }

    fn ref_name(workdir: &Path) -> String {
        format!("{REFS_PREFIX}/{}", Self::project_hash(workdir))
    }

    fn ref_tip(&self, workdir: &Path) -> Option<String> {
        let reference = format!("{}^{{commit}}", Self::ref_name(workdir));
        self.git_out(&["rev-parse", "--verify", "--quiet", &reference], workdir)
            .filter(|tip| !tip.is_empty())
    }

    fn touch_project(&self, workdir: &Path) {
        let path = self
            .store()
            .join("projects")
            .join(format!("{}.json", Self::project_hash(workdir)));
        let now = chrono::Utc::now().timestamp();
        let created_at = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<ProjectMeta>(&text).ok())
            .map_or(now, |meta| meta.created_at);
        let meta = ProjectMeta {
            workdir: crate::display_path(workdir),
            created_at,
            last_touch: now,
        };
        if let Ok(text) = serde_json::to_string(&meta) {
            let _ = std::fs::write(path, text);
        }
    }

    // ---- taking -----------------------------------------------------------------------------

    /// `get_working_dir_for_path`: the project a file belongs to (nearest folder with a project
    /// marker below home), else its folder.
    pub fn working_dir_for(&self, path: &Path, home: &Path) -> PathBuf {
        let path = canonical(path).unwrap_or_else(|| path.to_path_buf());
        let candidate = if path.is_dir() {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        };
        let home = canonical(home).unwrap_or_else(|| home.to_path_buf());
        for dir in candidate.ancestors() {
            if is_under(&home, dir) {
                break;
            }
            if PROJECT_MARKERS
                .iter()
                .any(|marker| dir.join(marker).exists())
            {
                return dir.to_path_buf();
            }
        }
        candidate
    }

    /// `ensure_checkpoint`: a snapshot of `workdir`, once per turn (`taken` is the turn's set).
    /// Never fails the tool call; `false` when nothing was taken.
    pub fn ensure(
        &self,
        workdir: &Path,
        reason: &str,
        home: &Path,
        taken: &mut HashSet<String>,
    ) -> bool {
        if !self.enabled() {
            return false;
        }
        let workdir = canonical(workdir).unwrap_or_else(|| workdir.to_path_buf());
        // Too broad: a drive root or home itself.
        let home = canonical(home).unwrap_or_else(|| home.to_path_buf());
        if workdir.parent().is_none() || is_under(&home, &workdir) {
            return false;
        }
        if !taken.insert(Self::project_hash(&workdir)) {
            return false;
        }
        let _guard = self.lock.lock();
        match self.take(&workdir, reason) {
            Ok(taken) => taken,
            Err(error) => {
                tracing::debug!("checkpoint failed (non-fatal): {error}");
                false
            }
        }
    }

    /// `_take`: stage the work tree into the project's index, and commit it when it differs from
    /// the last checkpoint.
    fn take(&self, workdir: &Path, reason: &str) -> Result<bool, String> {
        self.init_store()?;
        self.touch_project(workdir);
        if file_count_exceeds(workdir, MAX_FILES) {
            return Ok(false);
        }
        let index = self.index_path(workdir);
        let tip = self.ref_tip(workdir);
        // Seed the index from the last checkpoint so only changes since then are staged.
        match &tip {
            Some(tip) => {
                self.git(&["read-tree", tip], workdir, Some(&index));
            }
            None => {
                let _ = std::fs::remove_file(&index);
            }
        }
        let (ok, _, err) = self.git(&["add", "-A"], workdir, Some(&index));
        if !ok {
            return Err(format!("git add failed: {err}"));
        }
        self.drop_oversize(workdir, &index);
        match &tip {
            Some(tip) => {
                let (unchanged, _, _) = self.git(
                    &["diff-index", "--cached", "--quiet", tip],
                    workdir,
                    Some(&index),
                );
                if unchanged {
                    return Ok(false);
                }
            }
            None => {
                let (_, files, _) = self.git(&["ls-files", "--cached"], workdir, Some(&index));
                if files.trim().is_empty() {
                    return Ok(false);
                }
            }
        }
        let (ok, tree, err) = self.git(&["write-tree"], workdir, Some(&index));
        if !ok || tree.is_empty() {
            return Err(format!("write-tree failed: {err}"));
        }
        // A listing shows the subject line only: keep a multiline reason on one line.
        let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(tip) = &tip {
            args.extend(["-p", tip.as_str()]);
        }
        args.extend(["-m", reason.as_str()]);
        let (ok, commit, err) = self.git(&args, workdir, Some(&index));
        if !ok || commit.is_empty() {
            return Err(format!("commit-tree failed: {err}"));
        }
        let reference = Self::ref_name(workdir);
        let mut args = vec!["update-ref", reference.as_str(), commit.as_str()];
        if let Some(tip) = &tip {
            args.push(tip.as_str());
        }
        let (ok, _, err) = self.git(&args, workdir, None);
        if !ok {
            return Err(format!("update-ref failed: {err}"));
        }
        self.trim(workdir);
        Ok(true)
    }

    /// `_drop_oversize_from_index`: generated assets stay out of checkpoints.
    fn drop_oversize(&self, workdir: &Path, index: &Path) {
        let (ok, out, _) = self.git(&["ls-files", "--cached", "-z"], workdir, Some(index));
        if !ok {
            return;
        }
        let oversize: Vec<&str> = out
            .split('\0')
            .filter(|rel| !rel.is_empty() && exceeds_size_cap(&workdir.join(rel)))
            .collect();
        for chunk in oversize.chunks(200) {
            let mut args = vec!["rm", "--cached", "--quiet", "--"];
            args.extend(chunk);
            self.git(&args, workdir, Some(index));
        }
    }

    /// `Pruner.trim`: keeps the newest checkpoints by rewriting the ref onto fresh parents (same
    /// trees, messages and dates). Unreachable objects stay until the store is collected.
    fn trim(&self, workdir: &Path) {
        let reference = Self::ref_name(workdir);
        let Some(list) = self.git_out(&["rev-list", &reference], workdir) else {
            return;
        };
        let commits: Vec<&str> = list.lines().collect();
        if commits.len() <= MAX_SNAPSHOTS {
            return;
        }
        let mut parent: Option<String> = None;
        for commit in commits[..MAX_SNAPSHOTS].iter().rev() {
            let Some(info) =
                self.git_out(&["log", "-1", "--format=%T%n%aI%n%cI%n%B", commit], workdir)
            else {
                return;
            };
            let mut lines = info.splitn(4, '\n');
            let (tree, author, committer) = (
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
            );
            let message = lines.next().unwrap_or_default().trim().to_string();
            let mut args = vec!["commit-tree", tree];
            if let Some(parent) = &parent {
                args.extend(["-p", parent.as_str()]);
            }
            args.extend(["-m", message.as_str()]);
            let (ok, new, _) = self.git_env(
                &args,
                workdir,
                None,
                &[
                    ("GIT_AUTHOR_DATE", author),
                    ("GIT_COMMITTER_DATE", committer),
                ],
            );
            if !ok {
                return;
            }
            parent = Some(new);
        }
        if let Some(new_tip) = parent {
            self.git(
                &["update-ref", &reference, &new_tip, commits[0]],
                workdir,
                None,
            );
        }
    }

    // ---- the agent-write ledger -------------------------------------------------------------

    fn ledger_path(&self, workdir: &Path) -> PathBuf {
        self.store()
            .join("ledgers")
            .join(format!("{}.json", Self::project_hash(workdir)))
    }

    fn load_ledger(&self, workdir: &Path) -> HashMap<String, LedgerEntry> {
        std::fs::read_to_string(self.ledger_path(workdir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// `record_agent_write`: what Hermes's write left, so a later restore can tell a hand edit
    /// apart. Best effort.
    pub fn record_write(&self, path: &Path, home: &Path) {
        if !self.enabled() {
            return;
        }
        let Some(path) = canonical(path) else {
            return;
        };
        let Some(hash) = hash_file(&path) else {
            return;
        };
        let workdir = self.working_dir_for(&path, home);
        let _guard = self.lock.lock();
        if self.init_store().is_err() {
            return;
        }
        let mut ledger = self.load_ledger(&workdir);
        ledger.insert(
            key(&path),
            LedgerEntry {
                hash,
                ts: chrono::Utc::now().timestamp_millis(),
            },
        );
        if ledger.len() > LEDGER_MAX_ENTRIES {
            let mut entries: Vec<_> = ledger.into_iter().collect();
            entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.ts));
            entries.truncate(LEDGER_MAX_ENTRIES);
            ledger = entries.into_iter().collect();
        }
        if let Ok(text) = serde_json::to_string(&ledger) {
            let _ = std::fs::write(self.ledger_path(&workdir), text);
        }
    }

    // ---- listing, diffing, restoring --------------------------------------------------------

    /// `list_checkpoints`: newest first, with each one's change stats.
    pub fn list(&self, workdir: &Path) -> Vec<Checkpoint> {
        let workdir = canonical(workdir).unwrap_or_else(|| workdir.to_path_buf());
        if !self.store().join("HEAD").exists() {
            return Vec::new();
        }
        let max = MAX_SNAPSHOTS.to_string();
        let Some(out) = self.git_out(
            &[
                "log",
                &Self::ref_name(&workdir),
                "--format=%H|%h|%aI|%s",
                "-n",
                &max,
            ],
            &workdir,
        ) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let mut parts = line.splitn(4, '|');
                let mut checkpoint = Checkpoint {
                    hash: parts.next()?.to_string(),
                    short_hash: parts.next()?.to_string(),
                    timestamp: parts.next()?.to_string(),
                    reason: parts.next()?.to_string(),
                    files_changed: 0,
                    insertions: 0,
                    deletions: 0,
                };
                let parent = format!("{}~1", checkpoint.hash);
                if let Some(stat) = self.git_out(
                    &["diff", "--shortstat", &parent, &checkpoint.hash],
                    &workdir,
                ) {
                    parse_shortstat(&stat, &mut checkpoint);
                }
                Some(checkpoint)
            })
            .collect()
    }

    /// Validates a hash and checks it exists in the store.
    fn known_commit(&self, workdir: &Path, hash: &str) -> Result<(), String> {
        let valid = (4..=64).contains(&hash.len()) && hash.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(format!(
                "Invalid commit hash (expected 4-64 hex characters): {hash:?}"
            ));
        }
        if !self.store().join("HEAD").exists() {
            return Err("No checkpoints exist for this directory".into());
        }
        let (ok, _, _) = self.git(&["cat-file", "-t", hash], workdir, None);
        if ok {
            Ok(())
        } else {
            Err(format!("Checkpoint '{hash}' not found"))
        }
    }

    /// Stages the current tree into the project's index, runs `f`, then resets the index to the
    /// last checkpoint so it doesn't drift.
    fn with_current_tree<T>(&self, workdir: &Path, f: impl FnOnce(&Path) -> T) -> T {
        let index = self.index_path(workdir);
        self.git(&["add", "-A"], workdir, Some(&index));
        let result = f(&index);
        self.git(
            &["read-tree", &Self::ref_name(workdir)],
            workdir,
            Some(&index),
        );
        result
    }

    /// `diff`: a checkpoint against the current files.
    pub fn diff(&self, workdir: &Path, hash: &str) -> Result<Diff, String> {
        let workdir = canonical(workdir).unwrap_or_else(|| workdir.to_path_buf());
        let _guard = self.lock.lock();
        self.known_commit(&workdir, hash)?;
        let (stat, diff) = self.with_current_tree(&workdir, |index| {
            let stat = self.git(&["diff", "--stat", hash, "--cached"], &workdir, Some(index));
            let diff = self.git(
                &["diff", hash, "--cached", "--no-color", "--no-ext-diff"],
                &workdir,
                Some(index),
            );
            (stat, diff)
        });
        if !stat.0 && !diff.0 {
            return Err("Could not generate diff".into());
        }
        Ok(Diff {
            stat: if stat.0 { stat.1 } else { String::new() },
            diff: if diff.0 { diff.1 } else { String::new() },
        })
    }

    /// Puts one file back as it was at a checkpoint; a file the checkpoint doesn't have (made
    /// since) is deleted. A snapshot is taken first either way, so `/rollback` can undo it.
    pub fn revert_file(&self, workdir: &Path, hash: &str, file: &str) -> Result<(), String> {
        let canonical_dir = canonical(workdir).unwrap_or_else(|| workdir.to_path_buf());
        let existed = {
            let _guard = self.lock.lock();
            self.known_commit(&canonical_dir, hash)?;
            let spec = format!("{hash}:{}", file.replace('\\', "/"));
            self.git(&["cat-file", "-e", &spec], &canonical_dir, None).0
        };
        if existed {
            return self.restore(workdir, hash, Some(file), false).map(|_| ());
        }
        let path = Path::new(file);
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("File path must be relative to the project: {file:?}"));
        }
        let _guard = self.lock.lock();
        self.take(&canonical_dir, &format!("pre-revert snapshot (removing {file})"))
            .ok();
        std::fs::remove_file(canonical_dir.join(path)).map_err(|e| format!("{file}: {e}"))
    }

    /// `session_diff`: everything changed since the oldest kept checkpoint (the state before
    /// Hermes's first recorded edit); `None` when there are no checkpoints.
    pub fn session_diff(&self, workdir: &Path) -> Option<Result<Diff, String>> {
        let oldest = self.list(workdir).pop()?;
        Some(self.diff(workdir, &oldest.hash))
    }

    /// `restore`: the project's files (or one file) as they were at a checkpoint. `safe` (a whole
    /// folder only) keeps files changed by hand since Hermes last wrote them. A snapshot of the
    /// current state is taken first, so the rollback itself can be undone.
    pub fn restore(
        &self,
        workdir: &Path,
        hash: &str,
        file: Option<&str>,
        safe: bool,
    ) -> Result<Restored, String> {
        let workdir = canonical(workdir).unwrap_or_else(|| workdir.to_path_buf());
        let _guard = self.lock.lock();
        self.known_commit(&workdir, hash)?;
        if let Some(file) = file {
            let path = Path::new(file);
            let inside = !path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            if file.trim().is_empty() || !inside {
                return Err(format!(
                    "File path must be relative to the project: {file:?}"
                ));
            }
        }
        let plan = (safe && file.is_none())
            .then(|| self.safe_restore_plan(&workdir, hash))
            .transpose()?
            .flatten();
        let short = &hash[..hash.len().min(8)];
        self.take(
            &workdir,
            &format!("pre-rollback snapshot (restoring to {short})"),
        )
        .ok();
        let index = self.index_path(&workdir);
        let mut restored = Restored {
            restored_to: short.to_string(),
            directory: workdir.clone(),
            file: file.map(str::to_string),
            ..Default::default()
        };
        let (ok, err) = match plan {
            Some((restore, skipped)) => {
                restored.skipped_user_edits = skipped;
                let mut checkout = Vec::new();
                for rel in restore {
                    let (in_commit, _, _) = self.git(
                        &["cat-file", "-e", &format!("{hash}:{rel}")],
                        &workdir,
                        None,
                    );
                    if in_commit {
                        checkout.push(rel);
                    } else if exceeds_size_cap(&workdir.join(&rel)) {
                        restored.skipped_oversize.push(rel);
                    } else if std::fs::remove_file(workdir.join(&rel)).is_err()
                        && workdir.join(&rel).exists()
                    {
                        restored.failed_deletes.push(rel);
                    }
                }
                if checkout.is_empty() {
                    (true, String::new())
                } else {
                    let mut args = vec!["checkout", hash, "--"];
                    args.extend(checkout.iter().map(String::as_str));
                    let (ok, _, err) = self.git(&args, &workdir, Some(&index));
                    (ok, err)
                }
            }
            None => {
                let (ok, _, err) = self.git(
                    &["checkout", hash, "--", file.unwrap_or(".")],
                    &workdir,
                    Some(&index),
                );
                (ok, err)
            }
        };
        if !ok {
            return Err(format!("Restore failed: {err}"));
        }
        restored.reason = self
            .git_out(&["log", "--format=%s", "-1", hash], &workdir)
            .unwrap_or_else(|| "unknown".into());
        self.trim(&workdir);
        Ok(restored)
    }

    /// `_safe_restore_plan`: of the files changed since the checkpoint, those whose content is
    /// still what Hermes wrote (or deleted since) are restored, the rest kept. `None` when there's
    /// no ledger yet (a full restore then).
    fn safe_restore_plan(
        &self,
        workdir: &Path,
        hash: &str,
    ) -> Result<Option<(Vec<String>, Vec<String>)>, String> {
        let (ok, names, err) = self.with_current_tree(workdir, |index| {
            self.git(
                &["diff", "--name-only", "-z", hash, "--cached"],
                workdir,
                Some(index),
            )
        });
        if !ok {
            return Err(format!("Could not compute changed files: {err}"));
        }
        let ledger = self.load_ledger(workdir);
        if ledger.is_empty() {
            return Ok(None);
        }
        let (mut restore, mut skipped) = (Vec::new(), Vec::new());
        for rel in names.split('\0').filter(|rel| !rel.is_empty()) {
            let path = workdir.join(rel);
            let recorded = ledger.get(&key(&path)).map(|entry| entry.hash.as_str());
            match (recorded, hash_file(&path)) {
                (None, _) => skipped.push(rel.to_string()),
                (Some(_), None) => restore.push(rel.to_string()),
                (Some(recorded), Some(current)) if recorded == current => {
                    restore.push(rel.to_string());
                }
                _ => skipped.push(rel.to_string()),
            }
        }
        Ok(Some((restore, skipped)))
    }
}

/// `format_checkpoint_list`.
pub fn format_list(checkpoints: &[Checkpoint], directory: &str) -> String {
    if checkpoints.is_empty() {
        return format!("No checkpoints found for {directory}");
    }
    let mut lines = vec![format!("📸 Checkpoints for {directory}:\n")];
    for (index, checkpoint) in checkpoints.iter().enumerate() {
        let when = checkpoint
            .timestamp
            .split_once('T')
            .map(|(date, time)| format!("{date} {}", &time[..time.len().min(5)]))
            .unwrap_or_else(|| checkpoint.timestamp.clone());
        let stat = if checkpoint.files_changed > 0 {
            format!(
                "  ({} file{}, +{}/-{})",
                checkpoint.files_changed,
                if checkpoint.files_changed == 1 {
                    ""
                } else {
                    "s"
                },
                checkpoint.insertions,
                checkpoint.deletions
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "  {}. {}  {when}  {}{stat}",
            index + 1,
            checkpoint.short_hash,
            checkpoint.reason
        ));
    }
    lines.push("\n  /rollback <N>             restore to checkpoint N".into());
    lines.push("  /rollback diff <N>        preview changes since checkpoint N".into());
    lines.push("  /rollback <N> <file>      restore a single file from checkpoint N".into());
    lines.join("\n")
}

fn parse_shortstat(stat: &str, checkpoint: &mut Checkpoint) {
    let number_before = |word: &str| {
        let index = stat.find(word)?;
        stat[..index].split_whitespace().last()?.parse().ok()
    };
    checkpoint.files_changed = number_before(" file").unwrap_or(0);
    checkpoint.insertions = number_before(" insertion").unwrap_or(0);
    checkpoint.deletions = number_before(" deletion").unwrap_or(0);
}

fn exceeds_size_cap(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_SIZE)
}

fn file_count_exceeds(dir: &Path, limit: usize) -> bool {
    ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .hidden(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".git" | "node_modules" | "target")
        })
        .build()
        .take(limit + 1)
        .count()
        > limit
}

fn key(path: &Path) -> String {
    crate::display_path(path).to_lowercase()
}

/// A content hash of the file as git would store it (stable across app versions).
fn hash_file(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let mut command = isolated_git();
    command
        .args(["hash-object", "--no-filters", "--"])
        .arg(path);
    if let Some(parent) = path.parent() {
        command.current_dir(parent);
    }
    let (ok, out, _) = run(command);
    (ok && !out.is_empty()).then_some(out)
}

/// Git that reads no user or system config and never prompts, with no console window.
fn isolated_git() -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    let mut command = std::process::Command::new("git");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(var);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "NUL")
        .env("GIT_CONFIG_SYSTEM", "NUL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x0800_0000);
    command
}

/// Runs git with the timeout; stdout and stderr trimmed (NUL-separated output kept as is).
fn run(mut command: std::process::Command) -> GitOutput {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return (
                false,
                String::new(),
                format!("git could not start: {error}"),
            );
        }
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let err_reader = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = stderr.read_to_end(&mut err);
        err
    });
    let deadline = std::time::Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default())
        .trim()
        .to_string();
    match status {
        None => (
            false,
            String::new(),
            format!("git timed out after {}s", GIT_TIMEOUT.as_secs()),
        ),
        Some(status) => {
            let out = if out.contains('\0') {
                out
            } else {
                out.trim().to_string()
            };
            (status.success(), out, err)
        }
    }
}

/// `_is_destructive_command`: a terminal command that looks like it changes or deletes files.
pub fn is_destructive_command(command: &str) -> bool {
    static PATTERNS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)(?:^|\s|&&|\|\||;|`)(?:rm\s|rmdir\s|cp\s|install\s|mv\s|sed\s+-i|truncate\s|dd\s|shred\s|git\s+(?:reset|clean|checkout)\s|remove-item\s|move-item\s|copy-item\s|set-content\s|out-file\s|del\s|erase\s|rd\s|ren\s|move\s|copy\s)",
        )
        .expect("destructive pattern")
    });
    static REDIRECT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"[^>]>[^>]|^>[^>]").expect("redirect pattern")
    });
    !command.is_empty() && (PATTERNS.is_match(command) || REDIRECT.is_match(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Checkpoints) {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(project.path().join("a.txt"), "one\n").unwrap();
        let checkpoints = Checkpoints::new(data.path().join("checkpoints"), true);
        (data, project, checkpoints)
    }

    fn home() -> PathBuf {
        PathBuf::from(r"C:\nonexistent-home")
    }

    #[test]
    fn snapshot_once_per_turn_then_list_diff_and_restore() {
        let (_data, project, checkpoints) = setup();
        let dir = project.path();
        let mut turn = HashSet::new();
        assert!(checkpoints.ensure(dir, "before write_file", &home(), &mut turn));
        // Once per turn per project.
        assert!(!checkpoints.ensure(dir, "before patch", &home(), &mut turn));
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        checkpoints.record_write(&dir.join("a.txt"), &home());
        checkpoints.record_write(&dir.join("b.txt"), &home());

        let list = checkpoints.list(dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].reason, "before write_file");
        let diff = checkpoints.diff(dir, &list[0].hash).unwrap();
        assert!(diff.diff.contains("-one") && diff.diff.contains("+two"));
        assert!(format_list(&list, "proj").contains("1. "));

        let restored = checkpoints.restore(dir, &list[0].hash, None, true).unwrap();
        assert!(restored.skipped_user_edits.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert!(!dir.join("b.txt").exists());
        // The pre-rollback snapshot is newest now.
        assert!(
            checkpoints.list(dir)[0]
                .reason
                .starts_with("pre-rollback snapshot")
        );
    }

    #[test]
    fn reverting_one_file_restores_or_removes_it() {
        let (_data, project, checkpoints) = setup();
        let dir = project.path();
        checkpoints.ensure(dir, "before write_file", &home(), &mut HashSet::new());
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let hash = checkpoints.list(dir).pop().unwrap().hash;
        checkpoints.revert_file(dir, &hash, "a.txt").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        // Made after the checkpoint: reverting removes it.
        checkpoints.revert_file(dir, &hash, "b.txt").unwrap();
        assert!(!dir.join("b.txt").exists());
        assert!(checkpoints.revert_file(dir, &hash, "../x.txt").is_err());
    }

    #[test]
    fn a_safe_restore_keeps_hand_edits() {
        let (_data, project, checkpoints) = setup();
        let dir = project.path();
        checkpoints.ensure(dir, "before write_file", &home(), &mut HashSet::new());
        std::fs::write(dir.join("a.txt"), "hermes\n").unwrap();
        checkpoints.record_write(&dir.join("a.txt"), &home());
        // The user edits it afterwards.
        std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
        let hash = checkpoints.list(dir).pop().unwrap().hash;
        let restored = checkpoints.restore(dir, &hash, None, true).unwrap();
        assert_eq!(restored.skipped_user_edits, vec!["a.txt"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "mine\n"
        );
        // --all restores it anyway.
        checkpoints.restore(dir, &hash, None, false).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    }

    #[test]
    fn disabled_takes_nothing_and_destructive_commands_are_recognized() {
        let (_data, project, checkpoints) = setup();
        checkpoints.set_enabled(false);
        assert!(!checkpoints.ensure(project.path(), "x", &home(), &mut HashSet::new()));
        assert!(is_destructive_command("rm -rf build"));
        assert!(is_destructive_command("Remove-Item foo"));
        assert!(is_destructive_command("echo hi > out.txt"));
        assert!(!is_destructive_command("cargo test"));
        assert!(!is_destructive_command("echo hi >> log.txt"));
    }
}
