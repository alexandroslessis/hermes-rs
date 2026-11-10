//! Move to project: re-homing a stored session's workspace. The sidebar's drag onto a project
//! row and the composer's folder picker on an open chat both reach it (`session.workspace.move`
//! and `session.cwd.set` become the page's canonical `workspace` mutation,
//! `canonical-protocol.ts`). The stored cwd is replaced, never merged, with the new folder's git
//! identity beside it (the sidebar groups by the row's cwd/git identity); the page refuses a
//! move while a turn runs. The store's schema (shared with the GPUI app) has no git columns, so
//! the identity lives in the data folder's `session-workspaces.json`.
//!
//! Hermes: `gateway/session_mutation_policy.py` (`prepare_workspace` — the folder probe),
//! `hermes_state_mutation_prepared.py` (`workspace_in_transaction` — the store move, which
//! REPLACES the git identity), `tui_gateway/git_probe.py` (`branch`, `common_repo_root`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::git::git_line;

const FILE: &str = "session-workspaces.json";

/// The mutation's failures, mapped to the protocol's errors at the gateway: an unknown folder is
/// bad input (Hermes's `invalid_params`), a store failure is ours.
#[derive(Debug)]
pub enum MoveError {
    /// The folder doesn't exist.
    InvalidFolder(String),
    /// The store write failed.
    Store(String),
}

/// Per-session git identity (the branch checked out in the session's cwd and the repo root that
/// owns it), kept in `session-workspaces.json`.
#[derive(Serialize, Deserialize)]
struct Entry {
    branch: Option<String>,
    repo_root: Option<String>,
}

/// Session id → git identity, loaded once and written back whole on a move.
pub struct Workspaces {
    path: PathBuf,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Workspaces {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, entries: Mutex::new(entries) }
    }

    /// `(branch, repo root)` of the session's workspace, `None` for a session that never moved
    /// (its grouping follows the stored cwd alone).
    pub fn identity(&self, id: &str) -> Option<(Option<String>, Option<String>)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(id).map(|entry| (entry.branch.clone(), entry.repo_root.clone()))
    }

    fn set(&self, id: &str, branch: Option<String>, repo_root: Option<String>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if branch.is_none() && repo_root.is_none() {
            // A move into a non-git folder replaces the identity with "not a repository":
            // absent is the same null the page's reads fall back to.
            entries.remove(id);
        } else {
            entries.insert(id.to_string(), Entry { branch, repo_root });
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&*entries) {
            let _ = std::fs::write(&self.path, bytes);
        }
    }
}

/// The checked-out branch (Hermes's `git_probe.branch`): `git branch --show-current`, falling
/// back to the short HEAD sha for a detached HEAD; `None` outside a repository.
fn probe_branch(cwd: &Path) -> Option<String> {
    let shown = git_line(cwd, &["branch", "--show-current"]);
    if !shown.is_empty() {
        return Some(shown);
    }
    let short = git_line(cwd, &["rev-parse", "--short", "HEAD"]);
    (!short.is_empty()).then_some(short)
}

/// The main (common) repo root for `cwd`, folding linked worktrees (Hermes's
/// `git_probe.common_repo_root`): the parent of the shared `--git-common-dir` is the one true
/// root, `--show-toplevel` is a linked worktree's own. Forward slashes, so it compares equal to
/// what the page's cache holds (native `\` on Windows made the main checkout look like a
/// worktree).
fn probe_repo_root(cwd: &Path) -> Option<String> {
    let gitdir = git_line(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    if !gitdir.is_empty() {
        let gitdir = PathBuf::from(&gitdir);
        if gitdir.file_name().is_some_and(|name| name == ".git") {
            if let Some(parent) = gitdir.parent() {
                return Some(to_git_slashes(parent));
            }
        }
    }
    let toplevel = git_line(cwd, &["rev-parse", "--show-toplevel"]);
    (!toplevel.is_empty()).then(|| to_git_slashes(Path::new(&toplevel)))
}

fn to_git_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The folder a move lands in: `~` and `file:` links resolved, through its real location so the
/// stored spelling is one the page's cache compares equal (Hermes's `abspath(expanduser(...))`).
fn resolve_folder(requested: &str) -> Result<PathBuf, MoveError> {
    let folder = crate::files::resolve(requested, None);
    if !folder.is_dir() {
        return Err(MoveError::InvalidFolder(folder.display().to_string()));
    }
    Ok(real_location(&folder))
}

/// `canonicalize` without the `\\?\` prefix (the same spelling `git_review.rs` compares paths
/// with); falls back to the joined components when the folder can't be canonicalized.
fn real_location(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|canonical| {
            let text = canonical.to_string_lossy().to_string();
            PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        })
        .unwrap_or_else(|_| path.components().collect())
}

/// The `workspace` mutation: the session's cwd and git identity move together (every row of the
/// conversation in Hermes; one session row here — a branch is its own conversation with its own
/// cwd). The live state is dropped by the gateway afterwards (`Runtime::evict`), so the next
/// turn's tools and shell start in the new folder.
pub fn move_session(
    store: &hermes_store::Store,
    workspaces: &Workspaces,
    id: &str,
    requested: &str,
) -> Result<(String, Option<String>, Option<String>), MoveError> {
    let folder = resolve_folder(requested)?;
    // The probe first: a non-repo folder answers a null identity without touching the store.
    let branch = probe_branch(&folder);
    let repo_root = probe_repo_root(&folder);
    let cwd = folder.display().to_string();
    store
        .set_session_cwd(id, &cwd)
        .map_err(|e| MoveError::Store(format!("{e:#}")))?;
    workspaces.set(id, branch.clone(), repo_root.clone());
    Ok((cwd, branch, repo_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CREATE_NO_WINDOW;
    use std::os::windows::process::CommandExt as _;

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .expect("git ran");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A tempdir that is a git repository on `feature`, with one commit (a branch needs one).
    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-b", "feature"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "test"]);
        std::fs::write(dir.path().join("a.txt"), "hi").expect("wrote the file");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "one"]);
        let path = dunce(dir.path());
        (dir, path)
    }

    fn dunce(path: &Path) -> String {
        let canonical = std::fs::canonicalize(path).expect("canonicalized");
        let text = canonical.to_string_lossy().to_string();
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
    }

    fn store_in(dir: &Path) -> hermes_store::Store {
        hermes_store::Store::open(&dir.join("sessions.db")).expect("store opened")
    }

    fn session_in(store: &hermes_store::Store, cwd: &str) -> String {
        let record = store
            .create_session(hermes_store::NewSession {
                cwd: cwd.to_string(),
                provider: "test".to_string(),
                model: "test".to_string(),
                system_prompt: String::new(),
            })
            .expect("session created");
        record.id
    }

    #[test]
    fn a_move_replaces_the_cwd_and_carries_the_git_identity() {
        let data = tempfile::tempdir().expect("tempdir");
        let store = store_in(&data.path());
        let workspaces = Workspaces::load(data.path());
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let from = elsewhere.path().join("old-home");
        std::fs::create_dir(&from).expect("made the old folder");
        let id = session_in(&store, &from.display().to_string());

        let (repo_dir, repo_path) = repo();
        let repo_root = repo_path.replace('\\', "/");
        let (cwd, branch, root) =
            move_session(&store, &workspaces, &id, &repo_path).expect("moved");
        assert_eq!(cwd, repo_path);
        assert_eq!(branch.as_deref(), Some("feature"));
        assert_eq!(root.as_deref(), Some(repo_root.as_str()));
        assert_eq!(
            store.session(&id).expect("read").expect("session").cwd,
            repo_path
        );
        assert_eq!(
            workspaces.identity(&id),
            Some((Some("feature".to_string()), Some(repo_root.clone())))
        );
        drop(repo_dir);
    }

    #[test]
    fn a_move_into_a_plain_folder_replaces_the_identity_with_none() {
        let data = tempfile::tempdir().expect("tempdir");
        let store = store_in(&data.path());
        let workspaces = Workspaces::load(data.path());
        let (repo_dir, repo_path) = repo();
        let id = session_in(&store, &repo_path);

        let plain = tempfile::tempdir().expect("tempdir");
        let plain_path = dunce(plain.path());
        let (cwd, branch, root) =
            move_session(&store, &workspaces, &id, &plain_path).expect("moved");
        assert_eq!(cwd, plain_path);
        assert_eq!(branch, None);
        assert_eq!(root, None);
        // The old repo identity is gone, not merged: the sidebar groups by what the folder is now.
        assert_eq!(workspaces.identity(&id), None);
        drop(repo_dir);
    }

    #[test]
    fn an_unknown_folder_is_refused_and_writes_nothing() {
        let data = tempfile::tempdir().expect("tempdir");
        let store = store_in(&data.path());
        let workspaces = Workspaces::load(data.path());
        let id = session_in(&store, "C:\\was\\here");

        let missing = data.path().join("no-such-folder");
        match move_session(&store, &workspaces, &id, &missing.display().to_string()) {
            Err(MoveError::InvalidFolder(_)) => {}
            other => panic!("expected an invalid folder, got {other:?}"),
        }
        assert_eq!(
            store.session(&id).expect("read").expect("session").cwd,
            "C:\\was\\here"
        );
    }

    #[test]
    fn a_linked_worktree_reports_the_main_repo_root() {
        let (main_dir, main_path) = repo();
        let worktree = tempfile::tempdir().expect("tempdir");
        let name = worktree.path().join("linked");
        git(
            main_dir.path(),
            &["worktree", "add", &name.display().to_string(), "-b", "linked"],
        );

        let branch = probe_branch(&name);
        assert_eq!(branch.as_deref(), Some("linked"));
        // `--show-toplevel` is the worktree's own folder; the common dir's parent is the main repo.
        assert_eq!(
            probe_repo_root(&name).as_deref(),
            Some(main_path.replace('\\', "/")).as_deref()
        );
        assert_eq!(
            probe_repo_root(main_dir.path()).as_deref(),
            Some(main_path.replace('\\', "/")).as_deref()
        );
    }

    #[test]
    fn a_detached_head_reports_the_short_sha() {
        let (main_dir, _main_path) = repo();
        git(main_dir.path(), &["checkout", "--detach", "HEAD"]);
        let branch = probe_branch(main_dir.path());
        assert!(branch.is_some_and(|b| !b.is_empty() && !b.contains(' ')));
    }
}
