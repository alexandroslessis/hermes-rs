//! The right panel's calls on `hermes_core`, one function each (the [`crate::model::Backend`]
//! trait's `diff`, `revert_file`, `head_commit` and `open_in_editor` for the real core, kept out of
//! `model/backend.rs`). They block: the panel calls them from a background task.
//!
//! - **Working tree:** `hermes_core::git_review::review_patch` (`git diff HEAD` and the untracked
//!   files as all-added diffs). Revert is `git_review::revert` (`checkout HEAD` then `clean`):
//!   destructive, nothing brings the file back.
//! - **This turn:** `review_patch("lastTurn", base)` with the commit `HEAD` was at when the turn
//!   started ([`head_commit`], taken by the thread model). With no base (a turn from the store)
//!   the working tree's patch is answered with `narrowed` set, and the panel keeps only the files
//!   the turn's edit cards named. No revert here: "back to `HEAD`" would not be "back to before
//!   the turn".
//! - **This thread:** the runtime's own per-session review (`Runtime::changes`: the files this
//!   thread's `write_file` / `patch` calls wrote, compared with the checkpoint taken at its first
//!   edit when checkpoints are on, else with `HEAD`). Revert is `Runtime::revert_file`, which takes
//!   a checkpoint first so `/rollback` can undo it. Asking wakes the session in the runtime.
//! - **Hunk revert:** the core has none (`git_review::revert` is per file); not offered.
//!
//! Paths in a patch are relative to the repository's root, not to the thread's folder (a thread
//! may work in a subfolder); [`repo_root`] is what they are resolved against.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hermes_core::Core;
use hermes_core::git::{self, GIT_TIMEOUT};
use hermes_core::git_review;

use super::data::{
    DiffData, DiffRequest, Revert, Scope, absolute, editor_command, find_on_path,
    patch_from_changes,
};

/// The thread's folder.
fn session_cwd(core: &Core, session: &str) -> Result<PathBuf, String> {
    let record = core
        .runtime()
        .store()
        .session(session)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| "That thread no longer exists.".to_string())?;
    Ok(PathBuf::from(record.cwd))
}

/// The top of the repository `cwd` is in (`cwd` itself when git does not say).
pub fn repo_root(cwd: &Path) -> PathBuf {
    let top = git::git_line(cwd, &["rev-parse", "--show-toplevel"]);
    if top.is_empty() {
        cwd.to_path_buf()
    } else {
        PathBuf::from(top.replace('/', "\\"))
    }
}

/// The commit `HEAD` is at in the thread's folder.
pub fn head_commit(core: &Core, session: &str) -> Option<String> {
    let cwd = session_cwd(core, session).ok()?;
    git::rev_parse(&cwd, None)
}

/// The diff of `request.scope` as a patch.
pub fn read_diff(core: &Core, request: &DiffRequest) -> Result<DiffData, String> {
    let cwd = session_cwd(core, &request.session)?;
    if !cwd.is_dir() {
        return Ok(DiffData {
            note: Some("The thread's folder no longer exists.".to_string()),
            ..DiffData::default()
        });
    }
    match request.scope {
        Scope::WorkingTree => tree(&cwd, Revert::ToHead),
        Scope::Turn => match &request.turn.base {
            Some(base) => match git_review::review_patch(&cwd, "lastTurn", Some(base)) {
                Ok(patch) => Ok(from_patch(&cwd, patch, Revert::None, false)),
                // The base is gone (history rewritten, the repository replaced): what the
                // working tree has, narrowed to the turn's own files.
                Err(_) => narrowed(&cwd, request),
            },
            None => narrowed(&cwd, request),
        },
        Scope::Thread => {
            let changes = core
                .runtime()
                .changes(&request.session)
                .map_err(|e| format!("{e:#}"))?;
            let revert = match changes.source {
                Some(hermes_runtime::changes::ChangesSource::Session) => Revert::ToThreadStart,
                _ => Revert::None,
            };
            Ok(DiffData {
                patch: patch_from_changes(&changes.files),
                note: changes.note,
                skipped: 0,
                revert,
                narrowed: false,
            })
        }
    }
}

fn tree(cwd: &Path, revert: Revert) -> Result<DiffData, String> {
    let patch = git_review::review_patch(cwd, "uncommitted", None)?;
    Ok(from_patch(cwd, patch, revert, false))
}

/// The working tree's patch for a turn whose base commit is not known.
fn narrowed(cwd: &Path, request: &DiffRequest) -> Result<DiffData, String> {
    let mut data = tree(cwd, Revert::None)?;
    data.narrowed = true;
    if request.turn.files.is_empty() {
        data.patch.clear();
        data.note = Some("No edits of the latest turn are known.".to_string());
    } else {
        data.note = Some(
            "Showing the working tree's changes to the files the latest turn edited.".to_string(),
        );
    }
    Ok(data)
}

fn from_patch(
    cwd: &Path,
    patch: git_review::ReviewPatch,
    revert: Revert,
    narrowed: bool,
) -> DiffData {
    let note = (patch.patch.trim().is_empty() && git::repo_status(cwd).is_none())
        .then(|| "This folder is not a git repository.".to_string());
    DiffData {
        patch: patch.patch,
        note,
        skipped: patch.untracked_skipped,
        revert,
        narrowed,
    }
}

/// `path` as a git pathspec that means that file from the repository's top.
fn pathspec(path: &str) -> String {
    format!(":(top,literal){path}")
}

/// Whether the file `path` names is still what the user was shown: it is still among git's
/// changes, and (when the diff named its blob) its content is still that blob. A diff read
/// earlier and a file changed since (the agent, an editor) must not be discarded unseen.
fn still_as_shown(cwd: &Path, path: &str, seen: Option<&str>) -> Result<(), String> {
    let spec = pathspec(path);
    let (_, listed) = git::git_timed(
        cwd,
        &["status", "--porcelain", "--untracked-files=all", "--", &spec],
        GIT_TIMEOUT,
    )?;
    if listed.trim().is_empty() {
        return Err(format!(
            "{path} has no changes to revert now (the list was out of date); nothing was done."
        ));
    }
    let Some(seen) = seen else {
        return Ok(());
    };
    let file = absolute(&repo_root(cwd), path);
    let now = if file.is_file() {
        let name = file.to_string_lossy().into_owned();
        match git::git_timed(cwd, &["hash-object", "--", &name], GIT_TIMEOUT)? {
            (true, out) => out.trim().to_string(),
            (false, _) => return Err(format!("{path} could not be read; nothing was done.")),
        }
    } else if file.exists() {
        // A folder (a nested repository, a submodule): not a file this panel reverts.
        return Err(format!("{path} is not a file; nothing was done."));
    } else {
        "0".repeat(40)
    };
    if now.starts_with(seen) {
        Ok(())
    } else {
        Err(format!(
            "{path} changed after the diff was read; look at it again before reverting. Nothing was done."
        ))
    }
}

/// Puts a changed file back, as the request's scope allows. `seen`: the file's blob id as the
/// shown diff named it ([`super::data::blob_of`]).
pub fn revert(
    core: &Core,
    request: &DiffRequest,
    path: &str,
    seen: Option<&str>,
) -> Result<(), String> {
    match request.scope {
        Scope::WorkingTree => {
            let cwd = session_cwd(core, &request.session)?;
            let spec = pathspec(path);
            still_as_shown(&cwd, path, seen)?;
            git_review::revert(&cwd, Some(&spec))?;
            // `revert` does not say whether it worked: ask git whether the file still differs.
            let (_, left) =
                git::git_timed(&cwd, &["status", "--porcelain", "--", &spec], GIT_TIMEOUT)?;
            if left.trim().is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{path} could not be reverted (it is still changed)."
                ))
            }
        }
        Scope::Thread => core
            .runtime()
            .revert_file(&request.session, path)
            .map_err(|e| format!("{e:#}")),
        Scope::Turn => Err("A file cannot be reverted from the turn's diff.".to_string()),
    }
}

/// Opens `path` (as the diff names it) at `line`: in Zed or VS Code when one is on the `PATH`,
/// else with the system's default program (`hermes_core::open_file`, which shows an executable
/// in Explorer instead of running it). There is no editor setting until Phase 7. The name of
/// what opened it.
pub fn open_in_editor(
    core: &Core,
    session: &str,
    path: &str,
    line: Option<u32>,
) -> Result<String, String> {
    let cwd = session_cwd(core, session)?;
    // A diff's path is relative and inside the repository; anything else is not opened.
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative.has_root()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!("{path} is not a file of this repository."));
    }
    let file = absolute(&repo_root(&cwd), path);
    if !file.is_file() {
        return Err(format!("{} does not exist.", file.display()));
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let find = |name: &str| find_on_path(&path_var, name);
    if let Some(command) = editor_command(&find, &file, line) {
        use std::os::windows::process::CommandExt as _;
        Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(git::CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("{} did not start: {e}", command.editor))?;
        return Ok(command.editor.to_string());
    }
    let plan = hermes_core::open_file::plan(&file.to_string_lossy())?;
    hermes_core::open_file::run(&plan)?;
    Ok("the default app".to_string())
}
