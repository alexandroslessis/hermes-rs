//! The folders a `terminal.sandbox = "low"` command may write to, and how they are made so.
//!
//! A Low process can write only to objects labelled Low (`sandbox.rs`). So, when a session at
//! `low` starts a command, its **workspace** (the repository root, else the folder the session
//! started in), its **scratch** folder (`<data>/scratch/<session>`: `TEMP` / `TMP` point there),
//! the shared **cache** folder (`<data>/sandbox-cache`: the package managers' caches point there,
//! [`cache_env`]) and the user's **grants** (`terminal.sandbox_grants` in
//! `desktop-config.json`) get an inheritable Low mandatory label, `S:(ML;OICI;NW;;;LW)`, and
//! what is already inside them the inherited label Windows gives new files there
//! (`(ML;ID;NW;;;LW)`, [`label_tree`]). Labels are set through a handle, one object at a time
//! and nothing but the label (see [`win`] for why not `SetNamedSecurityInfoW`); the handle is
//! opened without following a link and must still be under the labelled folder, so symbolic
//! links and junctions (also ones a command swaps in during the walk) are neither followed nor
//! labelled. Hard-linked files are left alone (their label would reach their other names).
//!
//! Every folder we label is written to `<data>/sandbox-labels.json` **before** the label is
//! set, with its label as it was (its SDDL, in practice empty: no label), and marked complete
//! once everything inside has its label, so [`remove_all`] (`/sandbox remove-labels`) can put
//! back what was there: the folder's own label, and the inherited labels below it removed
//! (deepest first). Owner, group and DACL are never written. One difference stays: an object
//! that had no SACL at all reads `S:NO_ACCESS_CONTROL` afterwards (an empty one, which means
//! the same: no label, no audit); removing that too needs `SeSecurityPrivilege`. Refused, with
//! the reason ([`Protected`]): a drive root; the user profile, the app's data folder,
//! `APPDATA`, `LOCALAPPDATA`, a `PATH` folder, or any folder containing one of them; the system
//! folders and the profile's places a planted file would run from (Startup, PowerShell profiles,
//! `.cargo`, `.rustup`, `.ssh`), in or around them; other users' profiles; anything in the data
//! folder but the scratch and cache folders; and a folder that already has a label we did not
//! set (someone else's choice: we neither override it nor would know how to restore it). The
//! record is locked across processes (`sandbox-labels.lock`: the app and `hermes-acp.exe` share
//! it). A folder already Low by inheritance
//! (inside a grant, or under `AppData\LocalLow`) is left alone and not recorded.
//!
//! [`blocked_hint`] reads a sandboxed command's output for an access-denied error and adds the
//! hint the model needs ("blocked by sandbox: <path>; ...").
//!
//! Hermes: none (ours, as `sandbox.rs`). The label is the one Microsoft's "Windows Integrity
//! Mechanism Design" and `icacls /setintegritylevel (OI)(CI)low` use.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project_skills::{canonical, within};

/// The record of the folders we labelled, in the data folder.
pub const RECORD_FILE: &str = "sandbox-labels.json";
/// The shared cache folder (cargo, npm, uv, pip, go) for sandboxed commands, in the data folder.
pub const CACHE_DIR: &str = "sandbox-cache";
/// The label we set: Low, no-write-up, inherited by files (OI) and folders (CI).
pub const LOW_LABEL_SDDL: &str = "S:(ML;OICI;NW;;;LW)";

/// One folder we labelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labelled {
    pub path: PathBuf,
    /// Its label before we set ours (`LABEL_SECURITY_INFORMATION` as SDDL; `""`: none).
    pub original: String,
    /// `workspace`, `scratch`, `cache` or `grant`.
    pub kind: String,
    /// Unix seconds.
    #[serde(default)]
    pub labelled_at: u64,
    /// Everything inside it has the inherited label too (else the next `low` command finishes
    /// the walk: the app may have stopped in the middle of a large folder).
    #[serde(default)]
    pub complete: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    folders: Vec<Labelled>,
}

/// One read-modify-write of the record at a time (sessions start commands concurrently).
static RECORD_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The lock file beside the record: the desktop app and `hermes-acp.exe` share the data folder,
/// so the in-process mutex alone could let two processes each save a record without the
/// other's folder (a label without a record).
const LOCK_FILE: &str = "sandbox-labels.lock";

/// Held while the record is read, labels are set and the record is saved: this process's
/// mutex, then an exclusive lock on [`LOCK_FILE`] (released when the file closes).
struct RecordGuard {
    _file: std::fs::File,
    _local: parking_lot::MutexGuard<'static, ()>,
}

fn lock_record(data_dir: &Path) -> io::Result<RecordGuard> {
    let local = RECORD_LOCK.lock();
    std::fs::create_dir_all(data_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(LOCK_FILE))?;
    file.lock()?;
    Ok(RecordGuard {
        _file: file,
        _local: local,
    })
}

fn record_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RECORD_FILE)
}

/// A missing record is empty; an unreadable one is an error (labelling stops rather than
/// forget the folders it lists).
fn load(data_dir: &Path) -> io::Result<Record> {
    match std::fs::read(record_path(data_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not valid: {e}", record_path(data_dir).display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(e),
    }
}

/// Written whole to a temporary file and renamed over the record.
fn save(data_dir: &Path, record: &Record) -> io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = record_path(data_dir);
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(record).map_err(io::Error::other)?)?;
    std::fs::rename(&temp, &path)
}

/// The folders we labelled (for `/sandbox`).
pub fn list(data_dir: &Path) -> io::Result<Vec<Labelled>> {
    let _guard = lock_record(data_dir)?;
    Ok(load(data_dir)?.folders)
}

/// A folder's label, read from its SDDL (`LABEL_SECURITY_INFORMATION` only).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Label {
    /// No mandatory label (the default: Medium).
    None,
    /// Exactly the label we set ([`LOW_LABEL_SDDL`]), explicit.
    LowExplicit,
    /// A Low label inherited from a folder above it.
    LowInherited,
    /// Anything else: someone else's label.
    Other,
}

/// The `(ML;flags;rights;;;level)` entries of an SDDL string.
fn label_aces(sddl: &str) -> Vec<(String, String, String)> {
    let mut aces = Vec::new();
    let mut rest = sddl;
    while let Some(start) = rest.find("(ML;") {
        let after = &rest[start + 1..];
        let Some(end) = after.find(')') else { break };
        let fields: Vec<&str> = after[..end].split(';').collect();
        if fields.len() >= 6 {
            aces.push((
                fields[1].to_ascii_uppercase(),
                fields[2].to_ascii_uppercase(),
                fields[5].to_ascii_uppercase(),
            ));
        }
        rest = &after[end..];
    }
    aces
}

pub(crate) fn classify(sddl: &str) -> Label {
    let aces = label_aces(sddl);
    match aces.as_slice() {
        [] => Label::None,
        [(flags, rights, level)] if rights == "NW" && level == "LW" => {
            let flags: Vec<&str> = flags
                .as_bytes()
                .chunks(2)
                .map(|pair| std::str::from_utf8(pair).unwrap_or(""))
                .collect();
            let has = |flag: &str| flags.contains(&flag);
            if has("ID") {
                Label::LowInherited
            } else if has("OI") && has("CI") && flags.len() == 2 {
                Label::LowExplicit
            } else {
                Label::Other
            }
        }
        _ => Label::Other,
    }
}

/// The places a label must never go near. A Low-writable folder is one any sandboxed command
/// can plant files in, so besides the system folders this keeps out the places from which a
/// planted file would run later at the user's own level: the folders on `PATH`, the Startup
/// folder, PowerShell's profile folders, `~\.cargo` and `~\.rustup` (toolchain binaries),
/// `~\.ssh`.
#[derive(Debug, Clone)]
pub(crate) struct Protected {
    /// The user profile, the app's data folder, `APPDATA`, `LOCALAPPDATA` and every `PATH`
    /// folder: never labelled, nor anything containing them.
    pub(crate) keep_out: Vec<PathBuf>,
    /// `Windows`, `Program Files`, `ProgramData`, `PUBLIC` and the sensitive folders of the
    /// profile: never labelled, nor anything in or around them.
    pub(crate) system: Vec<PathBuf>,
    /// The user's own profile folders, and the folder that holds every user's profile
    /// (`C:\Users`): anything in the latter but outside the former (another user's profile,
    /// `Public`, `Default`) is refused.
    pub(crate) profiles: Vec<PathBuf>,
    pub(crate) profiles_root: Option<PathBuf>,
    pub(crate) data_dir: PathBuf,
}

impl Protected {
    /// This machine's: the profile from `USERPROFILE` and `dirs::home_dir`, the system folders
    /// from their environment variables (real paths where they exist).
    pub(crate) fn here(data_dir: &Path) -> Self {
        let real = |path: PathBuf| canonical(&path).unwrap_or(path);
        let env = |name: &str| std::env::var_os(name).map(|dir| real(PathBuf::from(dir)));
        let profiles: Vec<PathBuf> = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .into_iter()
            .chain(dirs::home_dir())
            .map(real)
            .collect();
        let profiles_root = profiles
            .first()
            .and_then(|p| p.parent())
            .filter(|root| root.parent().is_some())
            .map(Path::to_path_buf);
        let data_dir = real(data_dir.to_path_buf());
        let mut keep_out = profiles.clone();
        keep_out.push(data_dir.clone());
        keep_out.extend(["APPDATA", "LOCALAPPDATA"].iter().filter_map(|name| env(name)));
        if let Some(path) = std::env::var_os("PATH") {
            keep_out.extend(
                std::env::split_paths(&path)
                    .filter(|dir| dir.is_absolute())
                    .map(real),
            );
        }
        let mut system: Vec<PathBuf> = [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
            "PUBLIC",
        ]
        .iter()
        .filter_map(|name| env(name))
        .collect();
        for profile in &profiles {
            for sub in [".ssh", ".cargo", ".rustup"] {
                system.push(real(profile.join(sub)));
            }
        }
        if let Some(appdata) = env("APPDATA") {
            system.push(real(appdata.join(r"Microsoft\Windows\Start Menu")));
        }
        if let Some(documents) = dirs::document_dir() {
            for sub in ["WindowsPowerShell", "PowerShell"] {
                system.push(real(documents.join(sub)));
            }
        }
        Self {
            keep_out,
            system,
            profiles,
            profiles_root,
            data_dir,
        }
    }

    /// Why `folder` (a real path) may not be labelled, or `None`.
    pub(crate) fn refusal(&self, folder: &Path) -> Option<String> {
        let shown = folder.display();
        if !folder.is_absolute() {
            return Some(format!("{shown} is not an absolute path"));
        }
        if folder.parent().is_none() {
            return Some(format!("{shown} is a drive root"));
        }
        // The data folder first: its session scratch folders and the cache are the one place
        // inside `APPDATA` that is labelled.
        if within(folder, &self.data_dir) {
            let scratch = self.data_dir.join("scratch");
            let cache = self.data_dir.join(CACHE_DIR);
            let allowed = (within(folder, &scratch) && folder != scratch) || within(folder, &cache);
            if !allowed {
                return Some(format!("{shown} is inside the app's data folder"));
            }
        }
        for dir in &self.keep_out {
            if within(dir, folder) {
                return Some(format!(
                    "{shown} is or contains {} (the user profile, the app's data folder, \
                     AppData or a folder on PATH)",
                    dir.display()
                ));
            }
        }
        for dir in &self.system {
            if within(dir, folder) || within(folder, dir) {
                return Some(format!(
                    "{shown} is, contains or is inside the protected folder {}",
                    dir.display()
                ));
            }
        }
        if let Some(root) = &self.profiles_root {
            if within(folder, root) && !self.profiles.iter().any(|p| within(folder, p)) {
                return Some(format!(
                    "{shown} is in {} but not in your own profile",
                    root.display()
                ));
            }
        }
        None
    }
}

/// What [`ensure_low`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ensured {
    /// Labelled now (and recorded).
    Labelled,
    /// Labelled now (and recorded), but some things inside were not: why (they stay read-only
    /// to commands).
    Partly(String),
    /// Already carries our recorded label.
    AlreadyOurs,
    /// Already Low by inheritance: nothing set, nothing recorded.
    AlreadyLow,
}

/// Opens `real` (a canonical folder path) without following a link, and checks that the path
/// still leads to it: a folder swapped for a junction since `canonical` read it is refused.
fn open_root(real: &Path) -> Result<win::Object, String> {
    let root = win::open(real).map_err(|e| format!("{}: {e}", real.display()))?;
    if root.is_link {
        return Err(format!("not labelled: {} is a link", real.display()));
    }
    match root.final_path() {
        Ok(path) if same(&path, real) => Ok(root),
        Ok(path) => Err(format!(
            "not labelled: {} now leads to {}",
            real.display(),
            path.display()
        )),
        Err(e) => Err(format!("{}: {e}", real.display())),
    }
}

/// Labels `folder` Low unless it is refused, recording it first. Idempotent.
pub(crate) fn ensure_low(
    data_dir: &Path,
    protected: &Protected,
    folder: &Path,
    kind: &str,
) -> Result<Ensured, String> {
    let real = canonical(folder).ok_or_else(|| format!("{} does not exist", folder.display()))?;
    if !real.is_dir() {
        return Err(format!("{} is not a folder", real.display()));
    }
    if let Some(why) = protected.refusal(&real) {
        return Err(format!("not labelled: {why}"));
    }
    // Tests label only their own temporary folders, whatever a bug elsewhere asks for.
    #[cfg(test)]
    assert!(
        canonical(&std::env::temp_dir()).is_some_and(|temp| within(&real, &temp) && real != temp),
        "a test tried to label {} (outside the temp folder)",
        real.display()
    );
    // Every read and write below goes through this handle: the object `real` named now, never
    // what a link put at that path later.
    let root = open_root(&real)?;
    let _guard = lock_record(data_dir).map_err(|e| e.to_string())?;
    let mut record = load(data_dir).map_err(|e| e.to_string())?;
    let recorded = record.folders.iter().position(|f| same(&f.path, &real));
    let sddl = root.label().map_err(|e| format!("{}: {e}", real.display()))?;
    let complete = recorded.is_some_and(|i| record.folders[i].complete);
    match (classify(&sddl), recorded) {
        (Label::LowExplicit, Some(_)) if complete => Ok(Ensured::AlreadyOurs),
        (Label::LowInherited, _) => Ok(Ensured::AlreadyLow),
        // Not labelled yet (or ours, but the walk below it was cut short).
        (Label::None, _) | (Label::LowExplicit, Some(_)) => {
            // Recorded first: a crash in between leaves a record of a folder without (all of)
            // the label, which the next command finishes or removal undoes, never a label
            // without a record.
            if recorded.is_none() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                record.folders.push(Labelled {
                    path: real.clone(),
                    original: sddl,
                    kind: kind.to_string(),
                    labelled_at: now,
                    complete: false,
                });
                prune_gone_scratch(&mut record, data_dir);
                save(data_dir, &record).map_err(|e| e.to_string())?;
            }
            if let Err(error) = root.set_label(LOW_LABEL_SDDL) {
                if recorded.is_none() {
                    record.folders.retain(|f| !same(&f.path, &real));
                    let _ = save(data_dir, &record);
                }
                return Err(format!("could not label {}: {error}", real.display()));
            }
            let tally = label_tree(&real);
            // The walk went through (what failed would fail again: not walked at every command).
            if let Some(entry) = record.folders.iter_mut().find(|f| same(&f.path, &real)) {
                entry.complete = true;
            }
            save(data_dir, &record).map_err(|e| e.to_string())?;
            let mut why = Vec::new();
            if tally.failed > 0 {
                why.push(format!("{} item(s) could not be labelled", tally.failed));
            }
            if tally.shared > 0 {
                why.push(format!(
                    "{} hard-linked file(s) were left as they are (a label would reach their \
                     other names, outside the folder too)",
                    tally.shared
                ));
            }
            if why.is_empty() {
                Ok(Ensured::Labelled)
            } else {
                Ok(Ensured::Partly(format!(
                    "{}: {}; they stay read-only to commands",
                    real.display(),
                    why.join(", ")
                )))
            }
        }
        (Label::LowExplicit, None) | (Label::Other, _) => Err(format!(
            "not labelled: {} already has a label this app did not set ({sddl})",
            real.display()
        )),
    }
}

/// The inherited label a labelled folder gives a folder in it (and its files and folders).
const INHERITED_DIR: &str = "S:(ML;OICIID;NW;;;LW)";
/// ... and a file in it.
const INHERITED_FILE: &str = "S:(ML;ID;NW;;;LW)";

/// What a walk does with one entry under a labelled folder.
enum Visit {
    /// A symbolic link, junction or other reparse point (never followed: its target may be
    /// anywhere), something no longer under the walk's root (a folder swapped for a link while
    /// the walk ran), a folder or file with someone else's label, or a protected SACL (it
    /// doesn't inherit): left alone.
    Skip,
    /// No label: gets the inherited one (when labelling).
    Unlabelled,
    /// The inherited Low label (ours, from the folder above).
    Inherited,
}

/// Opens one entry of a walk (never following a link) and says what to do with it. The handle
/// is what the label is then read and set through, so the object checked is the one changed.
fn visit(path: &Path, root: &Path) -> io::Result<(win::Object, Visit)> {
    let object = win::open(path)?;
    if object.is_link {
        return Ok((object, Visit::Skip));
    }
    // Where the handle really is: a folder on the way down may have been replaced by a junction
    // since the walk listed it (a sandboxed command can do that inside a Low folder).
    let real = object.final_path()?;
    if !within(&real, root) || same(&real, root) {
        return Ok((object, Visit::Skip));
    }
    let sddl = object.label()?;
    if sddl.starts_with("S:P") {
        return Ok((object, Visit::Skip));
    }
    let visit = match classify(&sddl) {
        Label::None => Visit::Unlabelled,
        Label::LowInherited => Visit::Inherited,
        Label::LowExplicit | Label::Other => Visit::Skip,
    };
    Ok((object, visit))
}

/// The paths of a folder's entries.
fn entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect()
}

/// What a labelling walk left out.
#[derive(Debug, Default)]
struct Tally {
    /// Entries that could not be read or labelled.
    failed: usize,
    /// Files with more than one name (hard links): one file, one descriptor, so a label set
    /// through the name inside the folder would make its names elsewhere (a pnpm store, a
    /// file outside the workspace) writable by commands too. Left unlabelled.
    shared: usize,
}

/// Gives everything under `root` (just labelled) the inherited label, top down: what Windows
/// gives files made there from now on. Stops at links and at labels that aren't ours, and
/// leaves hard-linked files alone.
fn label_tree(root: &Path) -> Tally {
    let mut tally = Tally::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(children) = entries(&dir) else {
            tally.failed += 1;
            continue;
        };
        for path in children {
            let descend = match visit(&path, root) {
                Ok((object, Visit::Unlabelled)) => {
                    if !object.is_dir && object.links > 1 {
                        tally.shared += 1;
                        continue;
                    }
                    let label = if object.is_dir { INHERITED_DIR } else { INHERITED_FILE };
                    match object.set_label(label) {
                        Ok(()) => object.is_dir,
                        Err(_) => {
                            tally.failed += 1;
                            false
                        }
                    }
                }
                Ok((object, Visit::Inherited)) => object.is_dir,
                Ok((_, Visit::Skip)) => false,
                Err(_) => {
                    tally.failed += 1;
                    false
                }
            };
            if descend {
                stack.push(path);
            }
        }
    }
    tally
}

/// Removes the inherited labels under `dir` (inside `root`), deepest first (so a removal cut
/// short can be run again: what is left still hangs from a labelled folder). Returns how many
/// could not be.
fn unlabel_tree(dir: &Path, root: &Path) -> usize {
    let Ok(children) = entries(dir) else {
        return 1;
    };
    let mut failed = 0;
    for path in children {
        match visit(&path, root) {
            Ok((object, Visit::Inherited)) => {
                if object.is_dir {
                    failed += unlabel_tree(&path, root);
                }
                if object.set_label("").is_err() {
                    failed += 1;
                }
            }
            Ok((_, Visit::Unlabelled | Visit::Skip)) => {}
            Err(_) => failed += 1,
        }
    }
    failed
}

/// Session scratch folders the app deleted (with their session) leave the record.
fn prune_gone_scratch(record: &mut Record, data_dir: &Path) {
    let scratch = canonical(&data_dir.join("scratch"));
    record.folders.retain(|f| {
        f.kind != "scratch" || f.path.exists() || scratch.as_ref().is_none_or(|s| !within(&f.path, s))
    });
}

fn same(a: &Path, b: &Path) -> bool {
    within(a, b) && within(b, a)
}

/// What [`remove_all`] did.
#[derive(Debug, Default)]
pub struct Removal {
    /// Labels put back as they were.
    pub restored: Vec<PathBuf>,
    /// Folders that no longer exist (dropped from the record).
    pub gone: Vec<PathBuf>,
    /// Folders whose label someone changed since (left as they are, dropped from the record).
    pub changed: Vec<PathBuf>,
    /// Folders whose label could not be put back (kept in the record).
    pub failed: Vec<(PathBuf, String)>,
}

/// Puts back the original label of every recorded folder (deepest first) and removes the
/// inherited labels below it, tolerating folders that are gone. What could not be restored
/// stays recorded.
pub fn remove_all(data_dir: &Path) -> io::Result<Removal> {
    let _guard = lock_record(data_dir)?;
    let mut record = load(data_dir)?;
    let mut folders = std::mem::take(&mut record.folders);
    folders.sort_by_key(|f| std::cmp::Reverse(f.path.as_os_str().len()));
    let mut removal = Removal::default();
    for folder in folders {
        if std::fs::symlink_metadata(&folder.path).is_err() {
            removal.gone.push(folder.path);
            continue;
        }
        // The folder through a handle that doesn't follow a link; a path that now is a link or
        // leads elsewhere is not the folder we labelled: left alone, forgotten.
        let root = match open_root(&folder.path) {
            Ok(root) => root,
            Err(e) if e.starts_with("not labelled") => {
                removal.changed.push(folder.path);
                continue;
            }
            Err(e) => {
                removal.failed.push((folder.path.clone(), e));
                record.folders.push(folder);
                continue;
            }
        };
        let label = root.label().map(|sddl| classify(&sddl));
        let result = match label {
            // Ours (or never set: a crash after recording; the walk below is harmless then).
            Ok(Label::LowExplicit | Label::None) => {
                let failed = unlabel_tree(&folder.path, &folder.path);
                let restored = if label.as_ref().is_ok_and(|l| *l == Label::LowExplicit) {
                    root.set_label(&folder.original)
                } else {
                    Ok(())
                };
                match (failed, restored) {
                    (0, Ok(())) => Ok(()),
                    (0, Err(e)) => Err(e.to_string()),
                    (n, _) => Err(format!("{n} item(s) inside kept their label")),
                }
            }
            Ok(_) => {
                removal.changed.push(folder.path);
                continue;
            }
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => removal.restored.push(folder.path),
            Err(e) => {
                removal.failed.push((folder.path.clone(), e));
                record.folders.push(folder);
            }
        }
    }
    save(data_dir, &record)?;
    Ok(removal)
}

/// The user's granted folders (`terminal.sandbox_grants`).
static GRANTS: parking_lot::RwLock<Vec<PathBuf>> = parking_lot::RwLock::new(Vec::new());

/// Sets the granted folders (the app calls it at startup and after Settings change). They are
/// labelled the next time a sandboxed command starts.
pub fn set_sandbox_grants(folders: Vec<PathBuf>) {
    *GRANTS.write() = folders;
}

/// The granted folders as set.
pub fn grants() -> Vec<PathBuf> {
    GRANTS.read().clone()
}

/// What a sandboxed command needs before it starts: the folders labelled, the cache variables.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    /// Lines for the tool result (a folder that could not be labelled, and why).
    pub(crate) notes: Vec<String>,
    /// Environment variables to set on the command.
    pub(crate) env: Vec<(&'static str, PathBuf)>,
    /// The folders the command may write to (labelled, ours or inherited).
    pub(crate) writable: Vec<PathBuf>,
}

/// Labels the session's workspace and scratch, the cache folder and the grants (each idempotent)
/// and says which cache variables to set. Blocking: the first labelling of a large folder walks
/// everything in it.
pub(crate) fn prepare(
    data_dir: &Path,
    workspace: &Path,
    scratch: &Path,
    grants: &[PathBuf],
) -> Prepared {
    let protected = Protected::here(data_dir);
    let mut prepared = Prepared::default();
    let cache = data_dir.join(CACHE_DIR);
    let _ = std::fs::create_dir_all(scratch);
    let _ = std::fs::create_dir_all(&cache);
    let mut folders: Vec<(&Path, &str)> = vec![(workspace, "workspace"), (scratch, "scratch")];
    folders.push((&cache, "cache"));
    folders.extend(grants.iter().map(|g| (g.as_path(), "grant")));
    let mut cache_ok = false;
    for (folder, kind) in folders {
        if kind == "grant" && !folder.is_absolute() {
            prepared.notes.push(format!(
                "sandbox: grant {} ignored: not an absolute path",
                folder.display()
            ));
            continue;
        }
        match ensure_low(data_dir, &protected, folder, kind) {
            Ok(ensured) => {
                if let Ensured::Partly(why) = ensured {
                    prepared.notes.push(format!("sandbox: {kind} folder {why}"));
                }
                prepared
                    .writable
                    .push(canonical(folder).unwrap_or_else(|| folder.to_path_buf()));
                cache_ok |= kind == "cache";
            }
            Err(why) => {
                tracing::warn!("sandbox {kind}: {why}");
                prepared.notes.push(format!(
                    "sandbox: the {kind} folder is not writable by commands: {why}"
                ));
            }
        }
    }
    if cache_ok {
        copy_cargo_config(&cache.join("cargo"));
        prepared.env = cache_env(&cache);
    }
    prepared
}

/// The package managers' cache variables, pointed into the shared Low cache folder. Tested at
/// Low on this machine (CP15.2): cargo fails to download a crate into `~\.cargo\registry`
/// ("Access is denied") but builds from what is already there; npm fails outright (`EPERM` on
/// `%LOCALAPPDATA%\npm-cache`); uv fails outright ("Failed to initialize cache"). pip and go
/// were not installed here: pip only warns without a cache, go refuses to build without
/// `GOCACHE`, so they are set too. `TEMP` / `TMP` already point into scratch (`shell_env`).
pub(crate) fn cache_env(cache: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("CARGO_HOME", cache.join("cargo")),
        ("npm_config_cache", cache.join("npm")),
        ("YARN_CACHE_FOLDER", cache.join("yarn")),
        ("UV_CACHE_DIR", cache.join("uv")),
        ("PIP_CACHE_DIR", cache.join("pip")),
        ("GOCACHE", cache.join("go").join("build")),
        ("GOMODCACHE", cache.join("go").join("mod")),
    ]
}

/// cargo reads its settings from `CARGO_HOME`: the user's `config.toml` comes along, so moving
/// `CARGO_HOME` doesn't change how cargo behaves. Never `credentials.toml`, and no registry
/// token or signing key from `config.toml` either ([`without_credentials`]): the copy is in a
/// folder every sandboxed command may write to, and a token kept there would outlive its
/// rotation. (Reading is not what the sandbox restricts: a command can read the original.)
fn copy_cargo_config(cargo_home: &Path) {
    let source_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")));
    let Some(source_home) = source_home else {
        return;
    };
    for name in ["config.toml", "config"] {
        let target = cargo_home.join(name);
        let Ok(text) = std::fs::read_to_string(source_home.join(name)) else {
            // Gone (or unreadable) at the source: the copy goes too.
            let _ = std::fs::remove_file(&target);
            continue;
        };
        let Some(text) = without_credentials(&text) else {
            // Not TOML cargo would read anyway: nothing copied rather than a token by mistake.
            let _ = std::fs::remove_file(&target);
            continue;
        };
        if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
            let _ = std::fs::create_dir_all(cargo_home);
            let _ = std::fs::write(&target, &text);
        }
    }
}

/// The keys of cargo's configuration that hold a credential (`registry.token`,
/// `registries.<name>.token`, the asymmetric-token `secret-key` and `secret-key-subject`):
/// removed wherever they are. `None` when the text is not TOML.
pub(crate) fn without_credentials(text: &str) -> Option<String> {
    fn strip(table: &mut toml::Table) {
        table.retain(|key, _| !matches!(key, "token" | "secret-key" | "secret-key-subject"));
        for (_, value) in table.iter_mut() {
            if let toml::Value::Table(inner) = value {
                strip(inner);
            }
        }
    }
    let mut table: toml::Table = text.parse().ok()?;
    strip(&mut table);
    toml::to_string(&table).ok()
}

/// What a sandboxed command's output says when Windows refused a write.
const DENIED: [&str; 6] = [
    "access is denied",
    "access to the path",
    "permission denied",
    "os error 5",
    "eperm",
    "unauthorizedaccessexception",
];

/// The hint for a sandboxed command whose output shows an access-denied error on a path outside
/// the folders it may write to: `None` when there is no such error, or its path is one of
/// `writable` (a file locked by another program says "Access is denied" too).
pub(crate) fn blocked_hint(output: &str, writable: &[PathBuf]) -> Option<String> {
    let mut unknown = false;
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if !DENIED.iter().any(|marker| lower.contains(marker)) {
            continue;
        }
        match path_in(line) {
            Some(path) => {
                if !writable.iter().any(|dir| within(Path::new(&path), dir)) {
                    return Some(hint(&path));
                }
            }
            None => unknown = true,
        }
    }
    unknown.then(|| hint("(a path the output does not show)"))
}

fn hint(path: &str) -> String {
    format!(
        "blocked by sandbox: {path}; grant the folder in Settings (terminal.sandbox_grants in \
         desktop-config.json) or move the work into the workspace"
    )
}

/// The first Windows path in an error line: quoted (`'…'`, `"…"`, `` `…` ``) or up to the next
/// space.
fn path_in(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    for start in 0..bytes.len().saturating_sub(2) {
        let drive = bytes[start].is_ascii_alphabetic()
            && bytes[start + 1] == b':'
            && matches!(bytes[start + 2], b'\\' | b'/')
            && (start == 0 || !bytes[start - 1].is_ascii_alphanumeric());
        if !drive {
            continue;
        }
        let quote = start
            .checked_sub(1)
            .map(|i| bytes[i])
            .filter(|b| matches!(b, b'\'' | b'"' | b'`'));
        let rest = &line[start..];
        let end = match quote {
            Some(q) => rest.find(q as char).unwrap_or(rest.len()),
            None => rest
                .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ',' | ';'))
                .unwrap_or(rest.len()),
        };
        let path = rest[..end].trim_end_matches([':', '.', ')']);
        return Some(path.to_string());
    }
    None
}

/// The Win32 calls: each `unsafe` block is one call, with what it relies on.
///
/// Labels are read and set on one object at a time through a handle (`GetKernelObjectSecurity`
/// / `SetKernelObjectSecurity` with `LABEL_SECURITY_INFORMATION` only: what `SetFileSecurityW`
/// does after opening the file), never with `SetNamedSecurityInfoW`: that one (as `icacls`)
/// re-runs Windows' inheritance over the whole tree and rewrites the DACLs it passes (it marks
/// them auto-inherited, `D:AI`, and on folders whose DACL isn't, turns inherited entries into
/// explicit ones), so the descriptors could not be put back as they were. The label's
/// inheritance is done by [`super::label_tree`] instead. The handle is opened with
/// `FILE_FLAG_OPEN_REPARSE_POINT`, so a link is never followed: the object checked is the object
/// labelled, whatever replaces the path in between.
pub(crate) mod win {
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::path::{Path, PathBuf};

    use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetKernelObjectSecurity, LABEL_SECURITY_INFORMATION,
        OBJECT_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetKernelObjectSecurity,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_NAME_NORMALIZED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetFileInformationByHandle, GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL,
        WRITE_OWNER,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// One file or folder, opened without following a link.
    pub(crate) struct Object {
        handle: OwnedHandle,
        pub(crate) is_dir: bool,
        /// A reparse point: a symbolic link, a junction, or another kind (never followed).
        pub(crate) is_link: bool,
        /// How many names (hard links) the file has.
        pub(crate) links: u32,
    }

    /// Opens `path` itself (a link as the link), for reading its label and, where allowed,
    /// setting it (`WRITE_OWNER`; without it a later `set_label` fails).
    pub(crate) fn open(path: &Path) -> io::Result<Object> {
        let name = wide(path);
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
        let open = |access: u32| {
            // SAFETY: `name` is NUL-terminated and alive for the call; no security attributes,
            // no template. The handle returned is owned by the caller (wrapped just below).
            unsafe {
                CreateFileW(PCWSTR(name.as_ptr()), access, share, None, OPEN_EXISTING, flags, None)
            }
        };
        let raw = open(READ_CONTROL.0 | WRITE_OWNER.0).or_else(|_| open(READ_CONTROL.0))?;
        // SAFETY: a valid handle CreateFileW just returned, owned by nothing else.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: a valid file handle (alive in `handle`) and a valid out-pointer.
        unsafe { GetFileInformationByHandle(HANDLE(handle.as_raw_handle()), &mut info) }?;
        Ok(Object {
            is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
            is_link: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
            links: info.nNumberOfLinks,
            handle,
        })
    }

    impl Object {
        fn raw(&self) -> HANDLE {
            HANDLE(self.handle.as_raw_handle())
        }

        /// Where the object really is (`\\?\` removed), as `canonical` spells it.
        pub(crate) fn final_path(&self) -> io::Result<PathBuf> {
            let mut buffer = vec![0u16; 512];
            loop {
                // SAFETY: a valid handle; the call writes at most `buffer.len()` units.
                let len = unsafe {
                    GetFinalPathNameByHandleW(
                        self.raw(),
                        &mut buffer,
                        // Both are 0: the normalized name, with a drive letter.
                        FILE_NAME_NORMALIZED,
                    )
                } as usize;
                if len == 0 {
                    return Err(io::Error::last_os_error());
                }
                if len < buffer.len() {
                    let text = String::from_utf16_lossy(&buffer[..len]);
                    let text = match text.strip_prefix(r"\\?\UNC\") {
                        Some(rest) => format!(r"\\{rest}"),
                        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
                    };
                    return Ok(PathBuf::from(text));
                }
                // Too small: `len` is the size needed, with the NUL.
                buffer.resize(len + 1, 0);
            }
        }

        /// The object's mandatory label as SDDL (`""`, `S:NO_ACCESS_CONTROL` or `S:(ML;...)`).
        pub(crate) fn label(&self) -> io::Result<String> {
            let info = LABEL_SECURITY_INFORMATION;
            let mut needed = 0u32;
            // SAFETY: the size query: a valid handle, no buffer (length 0), a valid out-pointer.
            // It fails with ERROR_INSUFFICIENT_BUFFER by design.
            let _ = unsafe { GetKernelObjectSecurity(self.raw(), info.0, None, 0, &mut needed) };
            if needed == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
            // SAFETY: `buffer` holds at least `needed` bytes, aligned; a valid handle.
            unsafe {
                GetKernelObjectSecurity(
                    self.raw(),
                    info.0,
                    Some(descriptor),
                    (buffer.len() * 8) as u32,
                    &mut needed,
                )
            }?;
            to_sddl(descriptor, info)
        }

        /// Sets this one object's mandatory label to the label entries of `label_sddl` (no
        /// entry: the label is removed). Nothing else in its descriptor changes, nothing below
        /// it either.
        pub(crate) fn set_label(&self, label_sddl: &str) -> io::Result<()> {
            let descriptor = from_sddl(label_sddl)?;
            // SAFETY: a valid handle; `descriptor` is the valid descriptor made above, alive
            // until the end of this function. Only LABEL_SECURITY_INFORMATION is applied.
            unsafe {
                SetKernelObjectSecurity(
                    self.raw(),
                    LABEL_SECURITY_INFORMATION,
                    PSECURITY_DESCRIPTOR(descriptor.0),
                )
            }?;
            Ok(())
        }
    }

    /// Memory Windows allocated for us with `LocalAlloc`: freed once, on drop.
    struct Local(*mut core::ffi::c_void);

    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a pointer a Win32 call allocated with LocalAlloc and handed to us;
                // freed exactly once, here.
                unsafe { LocalFree(Some(HLOCAL(self.0))) };
            }
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    /// A security descriptor (self-relative, alive while this runs) as SDDL, the parts `info`
    /// names.
    fn to_sddl(
        descriptor: PSECURITY_DESCRIPTOR,
        info: OBJECT_SECURITY_INFORMATION,
    ) -> io::Result<String> {
        let mut text = PWSTR::null();
        // SAFETY: `descriptor` is a valid self-relative descriptor the caller keeps alive;
        // `text` a valid out-pointer for a string the call allocates (freed by `Local`).
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                info,
                &mut text,
                None,
            )
        }?;
        let text = Local(text.0.cast());
        // SAFETY: a NUL-terminated UTF-16 string from the call above, alive until `text` drops.
        let string = unsafe { PWSTR(text.0.cast()).to_string() }.map_err(io::Error::other)?;
        Ok(string)
    }

    /// A descriptor made from SDDL (an empty string: a descriptor with nothing in it), freed on
    /// drop.
    fn from_sddl(sddl: &str) -> io::Result<Local> {
        let text: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut raw = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `text` is a NUL-terminated SDDL string; the descriptor the call allocates is
        // freed by `Local`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(text.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }?;
        Ok(Local(raw.0))
    }

    /// Owner, group, DACL and label, as SDDL, read by path (tests compare it before and after).
    #[cfg(test)]
    pub(crate) fn descriptor(path: &Path) -> io::Result<String> {
        use windows::Win32::Security::{
            DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetFileSecurityW,
            OWNER_SECURITY_INFORMATION,
        };
        let info = OWNER_SECURITY_INFORMATION
            | GROUP_SECURITY_INFORMATION
            | DACL_SECURITY_INFORMATION
            | LABEL_SECURITY_INFORMATION;
        let name = wide(path);
        let mut needed = 0u32;
        // SAFETY: the size query: `name` is NUL-terminated, no buffer (length 0), a valid
        // out-pointer for the size. It fails with ERROR_INSUFFICIENT_BUFFER by design.
        let _ = unsafe { GetFileSecurityW(PCWSTR(name.as_ptr()), info.0, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        // `u64`s: a descriptor wants pointer alignment.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
        // SAFETY: `buffer` holds at least `needed` bytes, aligned; `name` is NUL-terminated.
        let read = unsafe {
            GetFileSecurityW(
                PCWSTR(name.as_ptr()),
                info.0,
                Some(descriptor),
                (buffer.len() * 8) as u32,
                &mut needed,
            )
        };
        if !read.as_bool() {
            return Err(io::Error::last_os_error());
        }
        to_sddl(descriptor, info)
    }

    /// A path's own label (a link's, not its target's), for tests.
    #[cfg(test)]
    pub(crate) fn read_label(path: &Path) -> io::Result<String> {
        open(path)?.label()
    }

    /// Sets a path's own label (never through a link), for tests.
    #[cfg(test)]
    pub(crate) fn set_label(path: &Path, label_sddl: &str) -> io::Result<()> {
        open(path)?.set_label(label_sddl)
    }
}

#[cfg(test)]
#[path = "sandbox_labels_tests.rs"]
mod tests;
