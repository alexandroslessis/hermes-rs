//! Where may the agent's file tools write? One answer, used by `check_edit` (and from CP1.2 on by
//! the terminal guard).
//!
//! Not from Hermes: Hermes has no such policy. Backend behaviour is ours (AGENTS.md rule 1,
//! `docs/backend/PHASES.md` Phase 1); the PORTING.md row is `diverged`.
//!
//! A path is classified into one of five [`PathClass`]es after being made canonical: the longest
//! existing prefix goes through `fs::canonicalize` (so junctions, symlinks, 8.3 short names and
//! `\\?\` prefixes can't hide where it points), and the rest, which doesn't exist yet, is cleaned
//! of what Windows ignores (trailing dots and spaces, `:stream` suffixes). Comparison is
//! case-insensitive and slash-agnostic.
//!
//! What this does *not* cover: MCP filesystem servers (they write on their own), the terminal
//! (CP1.2 adds a best-effort check; the real boundary is the sandbox of Phase 15), and restoring a
//! checkpoint (only the user starts it, from `/rollback` or the page, and it puts a folder the
//! agent wrote into back to an earlier snapshot of it). `check_edit` runs before the checkpoint
//! snapshot, so a refused edit never snapshots the folder it aimed at. A UNC path to a local share
//! (`\\localhost\C$\...`) is not mapped back to its drive path and classifies as `Outside` (it
//! still asks outside Yolo); the sandbox of Phase 15 closes that.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// What a resolved path is, for the purpose of writing to it with the file tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// The session's working folder and below, or its scratch folder.
    Workspace,
    /// The app's data folder (settings, `sessions.db`, approvals): never written by file tools.
    AppData,
    /// The skills and memory folders: written only through `skill_manage` / `memory`, which scan
    /// and limit what goes in.
    SelfImprove,
    /// Places that run code later (startup folders, shell profiles, git hooks, `~/.ssh`...):
    /// always asks, in every mode, even Yolo.
    Persistence,
    /// Everything else.
    Outside,
}

/// The folders the classes are made of.
#[derive(Debug, Clone)]
pub struct PolicyContext {
    pub cwd: PathBuf,
    pub scratch: PathBuf,
    /// `HERMES_RS_HOME`; empty when unknown (then `AppData` and `SelfImprove` never match).
    pub data_dir: PathBuf,
    pub home: PathBuf,
    /// `%ProgramData%` (the all-users Startup folder lives in it).
    pub program_data: Option<PathBuf>,
    /// `%SystemRoot%` (scheduled tasks live in it).
    pub windows_dir: Option<PathBuf>,
    /// The Documents folder as Windows knows it (often redirected to OneDrive); the PowerShell
    /// profiles live in it. `~/Documents` is checked as well.
    pub documents: Option<PathBuf>,
    /// The roaming AppData folder (can be redirected); the user's Startup folder lives in it.
    /// `~/AppData/Roaming` is checked as well.
    pub roaming: Option<PathBuf>,
}

impl PolicyContext {
    /// The context for a session, reading the system folders from the environment.
    pub fn new(cwd: PathBuf, scratch: PathBuf, data_dir: PathBuf, home: PathBuf) -> Self {
        let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            cwd,
            scratch,
            data_dir,
            home,
            program_data: env("ProgramData"),
            windows_dir: env("SystemRoot").or_else(|| env("windir")),
            documents: dirs::document_dir(),
            roaming: dirs::config_dir(),
        }
    }
}

/// Classifies `path` (already resolved against the working folder, as `SessionTools::resolve`
/// does; that is also the path the tool will open).
pub fn classify(path: &Path, ctx: &PolicyContext) -> PathClass {
    let target = key(&canonical(path));
    let under = |dir: &Path| !dir.as_os_str().is_empty() && within(&target, &key(&canonical(dir)));

    // Most dangerous first: a repo checked out in the home folder, or a workspace that *is* the
    // home folder, must not make `~/.ssh` or `.git/hooks` ordinary workspace files.
    if is_persistence(&target, ctx) {
        return PathClass::Persistence;
    }
    // The scratch folder lives inside the data folder, so it is checked before it.
    if under(&ctx.scratch) {
        return PathClass::Workspace;
    }
    if under(&ctx.data_dir.join("skills")) || under(&ctx.data_dir.join("memories")) {
        return PathClass::SelfImprove;
    }
    if under(&ctx.data_dir) {
        return PathClass::AppData;
    }
    if under(&ctx.cwd) {
        return PathClass::Workspace;
    }
    PathClass::Outside
}

fn is_persistence(target: &str, ctx: &PolicyContext) -> bool {
    let home = key(&canonical(&ctx.home));
    let file_in_home = |name: &str| target == format!("{home}/{name}");

    let mut dirs: Vec<String> = [
        ".ssh",
        ".config/git",
        ".config/powershell",
        "appdata/roaming/microsoft/windows/start menu/programs/startup",
    ]
    .iter()
    .map(|rest| format!("{home}/{rest}"))
    .collect();
    if let Some(roaming) = &ctx.roaming {
        let base = key(&canonical(roaming));
        dirs.push(format!(
            "{base}/microsoft/windows/start menu/programs/startup"
        ));
    }
    if let Some(program_data) = &ctx.program_data {
        let base = key(&canonical(program_data));
        dirs.push(format!(
            "{base}/microsoft/windows/start menu/programs/startup"
        ));
    }
    if let Some(windows) = &ctx.windows_dir {
        let base = key(&canonical(windows));
        dirs.push(format!("{base}/system32/tasks"));
        dirs.push(format!("{base}/tasks"));
    }
    if dirs.iter().any(|dir| within(target, dir)) {
        return true;
    }

    const HOME_FILES: [&str; 9] = [
        ".gitconfig",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".profile",
        ".zshrc",
        ".zprofile",
        ".zshenv",
    ];
    if HOME_FILES.iter().any(|name| file_in_home(name)) {
        return true;
    }
    // PowerShell's `$PROFILE` files: `profile.ps1`, `Microsoft.PowerShell_profile.ps1`, ...
    let mut documents = vec![format!("{home}/documents")];
    if let Some(dir) = &ctx.documents {
        documents.push(key(&canonical(dir)));
    }
    let in_profile_dir = documents.iter().any(|docs| {
        within(target, &format!("{docs}/windowspowershell"))
            || within(target, &format!("{docs}/powershell"))
    });
    if target.ends_with("profile.ps1") && in_profile_dir {
        return true;
    }
    // A repository's hooks and config run code on git operations (`core.fsmonitor`, aliases).
    let parts: Vec<&str> = target.split('/').collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == ".git" && (pair[1] == "hooks" || pair[1] == "config"))
}

/// The real place `path` points to: existing prefix canonicalized, the missing tail cleaned.
fn canonical(path: &Path) -> PathBuf {
    let mut existing = lexical(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let base = loop {
        match std::fs::canonicalize(&existing) {
            Ok(real) => break strip_verbatim(&real),
            Err(_) => match existing.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    existing.pop();
                }
                // Nothing on the path exists (not even its root): use what is left.
                None => break existing,
            },
        }
    };
    let mut out = base;
    for part in tail.iter().rev() {
        if let Some(clean) = clean_component(part) {
            out.push(clean);
        }
    }
    out
}

/// What Windows would open for a component that doesn't exist yet: `.ssh.` and `.ssh ` are
/// `.ssh`, and `x:stream` is the stream `stream` of `x`, so the file is `x`.
fn clean_component(part: &OsStr) -> Option<String> {
    let text = part.to_string_lossy();
    let name = text.split(':').next().unwrap_or("");
    let name = name.trim_end_matches(['.', ' ']);
    (!name.is_empty()).then(|| name.to_string())
}

/// `.` and `..` folded away without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` → `\\srv\share` (what `fs::canonicalize` returns on
/// Windows).
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Lowercase, forward slashes, no trailing slash: what paths are compared as.
fn key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_lowercase()
        .trim_end_matches('/')
        .to_string()
}

/// Whether `path` is `dir` or inside it (both from [`key`]).
fn within(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        ctx: PolicyContext,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let base = strip_verbatim(&base);
            let ctx = PolicyContext {
                cwd: base.join("work"),
                scratch: base.join("data").join("scratch").join("s1"),
                data_dir: base.join("data"),
                home: base.join("home"),
                program_data: Some(base.join("programdata")),
                windows_dir: Some(base.join("windows")),
                documents: Some(base.join("onedrive").join("Documents")),
                roaming: Some(base.join("roaming")),
            };
            for dir in [&ctx.cwd, &ctx.scratch, &ctx.home] {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::create_dir_all(ctx.data_dir.join("skills")).unwrap();
            std::fs::create_dir_all(ctx.home.join(".ssh")).unwrap();
            Self { _root: root, ctx }
        }

        fn class(&self, path: impl AsRef<Path>) -> PathClass {
            classify(path.as_ref(), &self.ctx)
        }
    }

    #[test]
    fn the_workspace_and_its_scratch_folder_are_workspace() {
        let f = Fixture::new();
        assert_eq!(f.class(f.ctx.cwd.join("src/new.rs")), PathClass::Workspace);
        assert_eq!(f.class(&f.ctx.cwd), PathClass::Workspace);
        // The scratch folder is inside the data folder but is not AppData.
        assert_eq!(f.class(f.ctx.scratch.join("out.txt")), PathClass::Workspace);
    }

    #[test]
    fn the_data_folder_is_app_data_and_skills_and_memories_are_self_improvement() {
        let f = Fixture::new();
        let data = &f.ctx.data_dir;
        assert_eq!(
            f.class(data.join("desktop-config.json")),
            PathClass::AppData
        );
        assert_eq!(f.class(data.join("sessions.db")), PathClass::AppData);
        assert_eq!(
            f.class(data.join("skills/x/SKILL.md")),
            PathClass::SelfImprove
        );
        assert_eq!(
            f.class(data.join("memories/MEMORY.md")),
            PathClass::SelfImprove
        );
    }

    #[test]
    fn everything_else_is_outside() {
        let f = Fixture::new();
        let base = f.ctx.cwd.parent().unwrap();
        assert_eq!(f.class(base.join("other/a.txt")), PathClass::Outside);
        assert_eq!(f.class(f.ctx.home.join("notes.md")), PathClass::Outside);
    }

    #[test]
    fn dot_dot_cannot_leave_the_workspace_or_reach_the_data_folder() {
        let f = Fixture::new();
        let up = f
            .ctx
            .cwd
            .join("src")
            .join("..")
            .join("..")
            .join("other.txt");
        assert_eq!(f.class(up), PathClass::Outside);
        let to_data = f.ctx.cwd.join("..").join("data").join("sessions.db");
        assert_eq!(f.class(to_data), PathClass::AppData);
    }

    #[test]
    fn persistence_places_are_persistence() {
        let f = Fixture::new();
        let home = &f.ctx.home;
        let program_data = f.ctx.program_data.as_ref().unwrap();
        let windows = f.ctx.windows_dir.as_ref().unwrap();
        for path in [
            home.join(".ssh/authorized_keys"),
            home.join(".gitconfig"),
            home.join(".bashrc"),
            home.join(".config/git/config"),
            home.join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/x.bat"),
            program_data.join("Microsoft/Windows/Start Menu/Programs/StartUp/x.bat"),
            home.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"),
            home.join("Documents/PowerShell/profile.ps1"),
            windows.join("System32/Tasks/evil"),
            f.ctx.cwd.join(".git/hooks/pre-commit"),
            f.ctx.cwd.join("sub/.git/config"),
        ] {
            assert_eq!(f.class(&path), PathClass::Persistence, "{path:?}");
        }
        // Not persistence: a normal file in a repo, and a script that isn't a profile.
        assert_eq!(f.class(f.ctx.cwd.join(".git/HEAD")), PathClass::Workspace);
        assert_eq!(
            f.class(home.join("Documents/PowerShell/tool.ps1")),
            PathClass::Outside
        );
    }

    #[test]
    fn redirected_documents_and_roaming_folders_are_covered() {
        let f = Fixture::new();
        let documents = f.ctx.documents.as_ref().unwrap();
        let roaming = f.ctx.roaming.as_ref().unwrap();
        for path in [
            documents.join("PowerShell/Microsoft.PowerShell_profile.ps1"),
            documents.join("WindowsPowerShell/profile.ps1"),
            roaming.join("Microsoft/Windows/Start Menu/Programs/Startup/x.lnk"),
        ] {
            assert_eq!(f.class(&path), PathClass::Persistence, "{path:?}");
        }
        assert_eq!(f.class(documents.join("notes.txt")), PathClass::Outside);
    }

    #[test]
    fn persistence_wins_over_workspace_when_the_workspace_is_the_home_folder() {
        let mut f = Fixture::new();
        f.ctx.cwd = f.ctx.home.clone();
        assert_eq!(
            f.class(f.ctx.home.join(".ssh/id_ed25519")),
            PathClass::Persistence
        );
        assert_eq!(f.class(f.ctx.home.join("src/a.rs")), PathClass::Workspace);
    }

    #[test]
    fn case_differences_do_not_matter() {
        let f = Fixture::new();
        let shouted = f
            .ctx
            .home
            .join(".SSH/Authorized_Keys")
            .to_string_lossy()
            .to_uppercase();
        assert_eq!(f.class(shouted), PathClass::Persistence);
        let data = f
            .ctx
            .data_dir
            .join("SESSIONS.DB")
            .to_string_lossy()
            .to_uppercase();
        assert_eq!(f.class(data), PathClass::AppData);
    }

    #[test]
    fn trailing_dots_and_spaces_and_streams_are_what_windows_ignores() {
        let f = Fixture::new();
        let home = &f.ctx.home;
        // `.ssh` exists here; these names reach it on Windows. The cleaning is plain string work,
        // so it holds on every platform for names that don't exist yet.
        assert_eq!(
            f.class(f.ctx.cwd.join(".git. /hooks /pre-commit")),
            PathClass::Persistence
        );
        assert_eq!(f.class(home.join(".bashrc:evil")), PathClass::Persistence);
        assert_eq!(f.class(home.join(".bashrc.")), PathClass::Persistence);
        assert_eq!(f.class(home.join(".ssh./id")), PathClass::Persistence);
    }

    #[test]
    fn an_empty_data_folder_matches_nothing() {
        let mut f = Fixture::new();
        f.ctx.data_dir = PathBuf::new();
        assert_eq!(f.class(f.ctx.cwd.join("a.txt")), PathClass::Workspace);
        assert_eq!(f.class(f.ctx.home.join("a.txt")), PathClass::Outside);
    }

    #[test]
    fn resolve_then_classify_handles_home_relative_and_relative_paths() {
        let f = Fixture::new();
        let c = &f.ctx;
        let resolved = crate::paths::resolve("~/.ssh/config", &c.cwd, &c.home);
        assert_eq!(classify(&resolved, c), PathClass::Persistence);
        let resolved = crate::paths::resolve("../data/desktop-config.json", &c.cwd, &c.home);
        assert_eq!(classify(&resolved, c), PathClass::AppData);
        let resolved = crate::paths::resolve("src/main.rs", &c.cwd, &c.home);
        assert_eq!(classify(&resolved, c), PathClass::Workspace);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_in_the_workspace_pointing_at_ssh_is_still_persistence() {
        let f = Fixture::new();
        let link = f.ctx.cwd.join("innocent");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(f.ctx.home.join(".ssh"))
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        assert_eq!(
            f.class(link.join("authorized_keys")),
            PathClass::Persistence
        );
        // ...and one into the data folder.
        let link = f.ctx.cwd.join("data-link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&f.ctx.data_dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        assert_eq!(f.class(link.join("sessions.db")), PathClass::AppData);
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_prefix_does_not_hide_the_target() {
        let f = Fixture::new();
        let verbatim = format!(r"\\?\{}", f.ctx.data_dir.join("sessions.db").display());
        assert_eq!(f.class(verbatim), PathClass::AppData);
    }
}
