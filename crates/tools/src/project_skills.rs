//! Project skills (Backend v2, Phase 8, CP8.2): skills that live in a repository and are loaded
//! only for sessions whose working folder is inside it.
//!
//! **Ours, not ported from Hermes** (decision D5 in `docs/backend/PHASES.md`). A repository's
//! skills are `<git root>/.agents/skills/<name>/SKILL.md` (the only place we write), plus, read-only,
//! `<git root>/.hermes/skills/` and `<git root>/.claude/skills/` when they exist.
//!
//! ## Which repository
//! [`find_root`]: the real (canonical) working folder, then the nearest folder above it that holds
//! `.git` (a directory, or the file a worktree has). So `..` and symlinks/junctions are resolved
//! before anything is decided, a repo nested in another is its own project, a worktree is its own
//! project (its checked-out files are the ones that count), and a working folder outside any
//! repository has **no** project skills. A repository at the user's home folder is not a project
//! (its `.agents` is the user's own, not a repo's). The scope is fixed when the session's tools
//! are made (the session's start, or its reopening); a later `cd` of the shell doesn't change it.
//!
//! ## Name clashes (D5)
//! Order of precedence for one name, highest first: the user's own library (global skills),
//! `.agents/skills`, `.hermes/skills`, `.claude/skills`. The first is the one served; the others
//! are shadowed (not listed, not indexed, not injected). A repository can therefore never replace
//! a global skill the user relies on (it is untrusted text, see below), and a skill in
//! `.agents/skills` customizes a read-only copy from another tool's folder. Creating a project
//! skill named like a shadowed read-only one is allowed (that is how you customize it).
//!
//! The library-first rule is ours: D5 only orders the three project folders. It is the order
//! Claude Code uses too (personal skills over project skills), and the price is small: a project
//! that wants to specialise a global skill gives its version another name (`git-myrepo`), which
//! the index and `skills_list` then offer next to the global one. The other order would let a
//! cloned repository replace `git` or `code-review` under a name the model already trusts.
//! Names are compared exactly; project names are lowercase ASCII only (the skill name rule), so
//! case and look-alike tricks can't make a second skill that Windows' file system would merge.
//!
//! What the system prompt's skills index shows of a project skill is its name and the first 60
//! characters of its description (scanned), frozen when the session is made: that does not taint
//! the session (it is as much as `skills_list` shows), the full text does (see below).
//!
//! ## Untrusted
//! A repository can be somebody else's clone. Its skills are loaded only when they pass the same
//! threat scan as every other skill, are at most 100 000 characters, have a name the skill rules
//! allow, and are plain files: symlinks and junctions (a `SKILL.md` that points at `~/.ssh/...`,
//! a `references` folder that points at another repo) are never followed, and a path that resolves
//! outside the repository is refused. A project skill never runs by itself; a command that runs
//! one of its scripts is flagged like the scripts of a skill the agent wrote (`path_policy.rs`);
//! and a session that has had a project skill's text put in front of the model (`skill_view`, or
//! the index attaching it to a message) is tainted (`taint.rs`), so memory and skill writes wait
//! for the user and flagged commands always ask.
//!
//! Nothing here blocks the async workers' threads for long, but all of it is file-system work:
//! call it from `spawn_blocking` where it is reachable from an async handler.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::skills::{SUPPORT_DIRS, SkillEntry, skill_entry};

/// The folder we write project skills to, relative to the repository's root.
pub const WRITE_DIR: &str = ".agents/skills";
/// Folders read (never written), after [`WRITE_DIR`], relative to the repository's root.
pub const READ_ONLY_DIRS: [&str; 2] = [".hermes/skills", ".claude/skills"];
/// The category project skills are listed under.
pub const CATEGORY: &str = "project";

const MAX_DEPTH: usize = 4;
const MAX_SKILLS_PER_DIR: usize = 200;
const MAX_ENTRIES_PER_DIR: usize = 2_000;
/// Folders one walk of a skill folder reads at most (see [`skill_files`]).
const MAX_FOLDERS: usize = 1_000;
const MAX_LINKED_FILES: usize = 200;
/// A skill's SKILL.md is not loaded above this many bytes (the writer's limit is characters, and a
/// character is at most 4 bytes).
const MAX_SKILL_BYTES: u64 = 4 * crate::skill_manage::MAX_SKILL_CONTENT_CHARS as u64;

/// A folder holding project skills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDir {
    pub path: PathBuf,
    /// Not written by us (`.hermes/skills`, `.claude/skills`).
    pub read_only: bool,
}

/// Whether `meta` is a symlink or a junction / other reparse point (never followed).
pub fn is_reparse(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The real place `path` points to, without Windows' `\\?\` prefix.
pub fn canonical(path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    let text = real.to_string_lossy();
    Some(if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        real
    })
}

/// Lowercase, forward slashes, no trailing slash: what paths are compared as.
fn key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_lowercase()
        .trim_end_matches('/')
        .to_string()
}

/// Whether `path` is `dir` or inside it (both real paths).
pub fn within(path: &Path, dir: &Path) -> bool {
    let (path, dir) = (key(path), key(dir));
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// The scope a project's rows carry in the local index (`project:<root>`).
pub fn scope_key(root: &Path) -> String {
    format!("project:{}", key(root))
}

/// A small text file read whole (a git pointer file); `None` if it is missing, a link or large.
fn small_text(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// For a linked worktree (`git worktree add`), the main checkout's root; `None` for anything
/// else (an ordinary repository, a submodule, a bare repository's worktree, a broken link).
///
/// A worktree's `.git` is a file `gitdir: <repo>/.git/worktrees/<name>`; that folder's `commondir`
/// names the shared `.git` and its `gitdir` names the worktree's `.git` back. The back-pointer is
/// required: the `.git` file is in the (possibly foreign) checkout, so a folder could claim any
/// repository's worktree folder; the pointer back is inside that repository's own `.git`, which
/// the folder can't write. A submodule's `.git` file points at `.git/modules/<name>`, which has
/// no `commondir`: it stays its own project.
pub fn main_checkout(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let pointer = small_text(&dot_git)?;
    let gitdir = pointer.lines().next()?.strip_prefix("gitdir:")?.trim();
    // `Path::join` keeps an absolute path as it is and resolves a relative one against the base.
    let gitdir = canonical(&root.join(gitdir))?;
    let back = small_text(&gitdir.join("gitdir"))?;
    let back = canonical(&gitdir.join(back.trim()))?;
    if key(&back) != key(&canonical(&dot_git)?) {
        return None;
    }
    let common = small_text(&gitdir.join("commondir"))?;
    let common = canonical(&gitdir.join(common.trim()))?;
    if !common
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
    {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    // The main checkout must be a repository root `find_root` would give (not the home folder).
    find_root(&main)
        .is_some_and(|found| key(&found) == key(&main))
        .then_some(main)
}

/// The scope of a repository's **project memory** (Phase 9): [`scope_key`] of its root, except
/// that a linked worktree shares its main checkout's (its notes are about the repository, and a
/// worktree is usually short-lived: memory kept under its own path would be orphaned when it is
/// removed). Project *skills* stay per checkout ([`scope_key`]): they are files in that checkout.
/// File-system work (a few small reads): call it off the async workers.
pub fn memory_scope_key(root: &Path) -> String {
    scope_key(&main_checkout(root).unwrap_or_else(|| root.to_path_buf()))
}

/// The repository a working folder belongs to: see the module doc. `None` when there is none.
pub fn find_root(cwd: &Path) -> Option<PathBuf> {
    let real = canonical(cwd)?;
    let home = dirs::home_dir().and_then(|h| canonical(&h));
    for dir in real.ancestors() {
        // A filesystem root is not a repository, whatever is in it.
        dir.parent()?;
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            if home.as_deref().is_some_and(|h| key(h) == key(dir)) {
                return None;
            }
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Whether `root` is, as it is spelled, a repository root `find_root` could have returned (so a
/// path from a staged item or a note is not trusted to be one).
pub fn is_root(root: &Path) -> bool {
    canonical(root).is_some_and(|real| key(&real) == key(root))
        && find_root(root).is_some_and(|found| key(&found) == key(root))
}

/// The project skill folders that exist in `root`, in precedence order. A folder (or its
/// `.agents` / `.hermes` / `.claude` parent) that is a symlink or junction, or not a folder, is
/// not one.
pub fn skill_dirs(root: &Path) -> Vec<SkillDir> {
    let mut out = Vec::new();
    for (rel, read_only) in std::iter::once((WRITE_DIR, false))
        .chain(READ_ONLY_DIRS.iter().map(|rel| (*rel, true)))
    {
        let mut path = root.to_path_buf();
        let mut real = true;
        for part in rel.split('/') {
            path.push(part);
            real &= std::fs::symlink_metadata(&path)
                .is_ok_and(|m| m.is_dir() && !is_reparse(&m));
        }
        if real {
            out.push(SkillDir { path, read_only });
        }
    }
    out
}

/// Whether the root's `.agents/skills` folder may be written to: absent (it will be created) or a
/// plain folder, with a plain `.agents` above it.
pub fn write_dir_usable(root: &Path) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for part in WRITE_DIR.split('/') {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !is_reparse(&meta) => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a plain folder (a file, symlink or junction), so the project's skills can't be written there.",
                    path.display()
                ));
            }
            Err(_) => {}
        }
    }
    Ok(path)
}

/// Every `SKILL.md` below `dir`: plain files in plain folders only, bounded in depth and count,
/// support folders of a skill and excluded folders skipped.
pub fn skill_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, folders: &mut usize) {
        // `folders` bounds the whole walk: a tree of empty folders (depth 4 × 2 000 entries each)
        // would otherwise be read in full on every query that names the repository.
        if depth > MAX_DEPTH || out.len() >= MAX_SKILLS_PER_DIR || *folders >= MAX_FOLDERS {
            return;
        }
        *folders += 1;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<(PathBuf, std::fs::Metadata)> = entries
            .flatten()
            .take(MAX_ENTRIES_PER_DIR)
            .filter_map(|e| Some((e.path(), e.metadata().ok()?)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let is_skill = entries.iter().any(|(path, meta)| {
            path.file_name().is_some_and(|n| n == "SKILL.md") && meta.is_file() && !is_reparse(meta)
        });
        if is_skill {
            out.push(dir.join("SKILL.md"));
        }
        for (path, meta) in entries {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !meta.is_dir()
                || is_reparse(&meta)
                || crate::skills::EXCLUDED_DIRS.contains(&name)
                || (is_skill && SUPPORT_DIRS.contains(&name))
            {
                continue;
            }
            walk(&path, depth + 1, out, folders);
        }
    }
    let mut out = Vec::new();
    walk(dir, 0, &mut out, &mut 0);
    out
}

/// What the threat scan said about a file, remembered per `(path, size, mtime)`: the catalog is
/// asked for on every `skills_list` and `skill_view`, and scanning ~200 files each time is waste.
static SCANNED: LazyLock<Mutex<HashMap<PathBuf, (u64, i128, Result<(), String>)>>> =
    LazyLock::new(Default::default);

fn stamp(meta: &std::fs::Metadata) -> (u64, i128) {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    (meta.len(), nanos)
}

/// Whether the text of a project skill may be loaded: size, and the threat scan every skill passes
/// when it is written. `Err` says why not.
pub fn accept(text: &str) -> Result<(), String> {
    if text.chars().count() > crate::skill_manage::MAX_SKILL_CONTENT_CHARS {
        return Err("SKILL.md is longer than the skill limit".into());
    }
    let scope = hermes_memory::threats::Scope::All;
    match hermes_memory::threats::first_threat_message(text, scope) {
        Some(threat) => Err(threat),
        None => Ok(()),
    }
}

/// The text of a project skill's SKILL.md if it may be loaded (a plain file, not too big, UTF-8,
/// passes [`accept`]); `Err` says why not (logged by the callers that skip it).
pub fn read_skill_md(path: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || is_reparse(&meta) {
        return Err("not a plain file".into());
    }
    if meta.len() > MAX_SKILL_BYTES {
        return Err("SKILL.md is too big".into());
    }
    let stamp = stamp(&meta);
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let cached = SCANNED
        .lock()
        .unwrap()
        .get(path)
        .filter(|(len, time, _)| (*len, *time) == stamp)
        .map(|(_, _, verdict)| verdict.clone());
    let verdict = match cached {
        Some(verdict) => verdict,
        None => {
            let verdict = accept(&text);
            let mut cache = SCANNED.lock().unwrap();
            if cache.len() > 2_000 {
                cache.clear();
            }
            cache.insert(path.to_path_buf(), (stamp.0, stamp.1, verdict.clone()));
            verdict
        }
    };
    verdict.map(|()| text)
}

/// The bytes of one of a project skill's supporting files: a plain file no bigger than a SKILL.md
/// may be (a repository can hold a file of any size, and `skill_view` would read it whole).
pub fn read_support(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || is_reparse(&meta) {
        return Err("not a plain file".into());
    }
    if meta.len() > MAX_SKILL_BYTES {
        return Err("the file is too big to load".into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// One project skill's catalog entry, or `None` (and a log line) when it may not be loaded.
fn entry_of(dir: &SkillDir, root: &Path, skill_md: &Path) -> Option<SkillEntry> {
    let text = match read_skill_md(skill_md) {
        Ok(text) => text,
        Err(why) => {
            tracing::warn!(
                "project skill {} not loaded: {why}",
                skill_md.display()
            );
            return None;
        }
    };
    let mut entry = skill_entry(&dir.path, dir.read_only, skill_md, &text)?;
    // A name the skill rules refuse (`../x`, a path, a name with spaces) is not served.
    if crate::skill_manage::validate_skill_name(&entry.name).is_some() {
        tracing::warn!(
            "project skill {} not loaded: its name is not a valid skill name",
            skill_md.display()
        );
        return None;
    }
    // The skill's own folder must be inside the repository (it is: no link was followed).
    if !within(&entry.dir, root) {
        return None;
    }
    entry.category = Some(match entry.category.take() {
        Some(category) => format!("{CATEGORY}/{category}"),
        None => CATEGORY.to_string(),
    });
    entry.project = Some(root.to_path_buf());
    Some(entry)
}

/// The skills of the repository at `root`, in precedence order, one per name (see the module doc).
/// Empty when `root` isn't a repository root (see [`is_root`]).
pub fn catalog(root: &Path) -> Vec<SkillEntry> {
    if !is_root(root) {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in skill_dirs(root) {
        for skill_md in skill_files(&dir.path) {
            let Some(entry) = entry_of(&dir, root, &skill_md) else {
                continue;
            };
            if seen.insert(entry.name.clone()) {
                out.push(entry);
            }
        }
    }
    out
}

/// The supporting files of a project skill, grouped by folder: plain files in plain folders only.
pub fn linked_files(dir: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for sub in SUPPORT_DIRS {
        let mut files = Vec::new();
        let top = dir.join(sub);
        // The support folder itself may be a link to somewhere else.
        if !std::fs::symlink_metadata(&top).is_ok_and(|m| m.is_dir() && !is_reparse(&m)) {
            continue;
        }
        let mut stack = vec![top];
        let mut folders = 0usize;
        while let Some(current) = stack.pop() {
            folders += 1;
            if folders > MAX_FOLDERS {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten().take(MAX_ENTRIES_PER_DIR) {
                let Ok(meta) = entry.metadata() else { continue };
                if is_reparse(&meta) {
                    continue;
                }
                let path = entry.path();
                if meta.is_dir() {
                    stack.push(path);
                } else if total < MAX_LINKED_FILES
                    && let Ok(rel) = path.strip_prefix(dir)
                {
                    total += 1;
                    files.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        files.sort();
        if !files.is_empty() {
            out.insert(sub.to_string(), files);
        }
    }
    out
}

/// The real place of `path`, which may not exist yet: its nearest existing part resolved (links
/// followed), the rest appended as written.
fn resolve_partly(path: &Path) -> Option<PathBuf> {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Some(mut real) = canonical(&probe) {
            for part in tail.iter().rev() {
                real.push(part);
            }
            return Some(real);
        }
        tail.push(probe.file_name()?.to_os_string());
        if !probe.pop() {
            return None;
        }
    }
}

/// Whether `target` (which may not exist yet) lies inside the real `skill_dir`: the nearest
/// existing part of it is resolved, so a folder inside the skill that is a link to somewhere else
/// can't carry a write out.
pub fn ensure_inside(skill_dir: &Path, target: &Path) -> Result<(), String> {
    let real_dir = resolve_partly(skill_dir).ok_or("the skill's folder can't be resolved")?;
    let real = resolve_partly(target).ok_or("the path can't be resolved")?;
    if within(&real, &real_dir) {
        Ok(())
    } else {
        Err(format!(
            "{} resolves outside the skill's folder (a link?); refusing it.",
            target.display()
        ))
    }
}

#[cfg(test)]
mod tests;
