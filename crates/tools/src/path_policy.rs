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
    /// The root of the repository the session works in, when it has project skills to protect
    /// (`project_skills.rs`): a script inside one of its skill folders is flagged like the scripts
    /// of a skill the agent wrote, whoever wrote it.
    pub project: Option<PathBuf>,
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
            project: None,
        }
    }

    /// The same context for a session in the repository at `project`.
    pub fn with_project(mut self, project: Option<PathBuf>) -> Self {
        self.project = project;
        self
    }
}

/// Whether `path` is a script (below a `scripts` folder) of one of the project's skill folders.
fn is_project_skill_script(path: &Path, ctx: &PolicyContext) -> bool {
    let Some(root) = &ctx.project else {
        return false;
    };
    let target = key(&canonical(path));
    std::iter::once(crate::project_skills::WRITE_DIR)
        .chain(crate::project_skills::READ_ONLY_DIRS)
        .any(|rel| {
            let dir = key(&canonical(&root.join(rel)));
            within(&target, &dir)
                && target[dir.len()..]
                    .split('/')
                    .any(|part| part == "scripts")
        })
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

// --- Terminal commands -------------------------------------------------------------------------

/// Why a terminal command is flagged for naming a protected path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFlag {
    /// The reason shown to the user; also the key an approval is remembered by.
    pub description: &'static str,
    /// Persistence places always ask, in every mode, and the answer is never remembered.
    pub persistence: bool,
}

pub const DESC_PERSISTENCE: &str =
    "touches a place that runs code later (startup folder, shell profile, git hook, ssh)";
pub const DESC_APP_DATA: &str = "touches the app's own data folder (settings, history, approvals)";
pub const DESC_SELF_IMPROVE: &str =
    "touches the skills or memory folder (use skill_manage / memory instead)";
pub const DESC_SKILL_SCRIPT: &str = "runs a script the agent wrote into a skill";
pub const DESC_PROJECT_SKILL_SCRIPT: &str =
    "runs a script from a project skill (a file of this repository, not checked by the app)";

/// Looks for protected paths in the text of a terminal command and says why it should be flagged.
///
/// Defence in depth only: this is simple token extraction (quoted or bare words, with `~`, `$HOME`,
/// `$env:APPDATA`, `%USERPROFILE%` and a few other variables expanded), so a determined command
/// gets past it; the real boundary is the OS sandbox of Phase 15. Reads are flagged like writes.
///
/// `is_stock_script` says whether a file under the skills folder is an unmodified copy of a script
/// bundled with the app (those are exempt from the skill-script flag).
pub fn command_flag(
    command: &str,
    ctx: &PolicyContext,
    is_stock_script: &dyn Fn(&Path) -> bool,
) -> Option<CommandFlag> {
    let skills = ctx.data_dir.join("skills");
    let mut found: Option<CommandFlag> = None;
    for path in command_paths(command, ctx) {
        let (description, persistence) = match classify(&path, ctx) {
            PathClass::Persistence => (DESC_PERSISTENCE, true),
            PathClass::AppData => (DESC_APP_DATA, false),
            PathClass::SelfImprove => {
                // Only the part below the skills folder counts: a data folder that itself sits
                // under some `scripts` folder must not make every skills path a "script".
                let (target, root) = (key(&canonical(&path)), key(&canonical(&skills)));
                let in_skills = !ctx.data_dir.as_os_str().is_empty() && within(&target, &root);
                let is_script = in_skills
                    && target[root.len()..]
                        .split('/')
                        .any(|part| part == "scripts");
                if is_script {
                    if is_stock_script(&path) {
                        continue;
                    }
                    (DESC_SKILL_SCRIPT, false)
                } else {
                    (DESC_SELF_IMPROVE, false)
                }
            }
            PathClass::Workspace | PathClass::Outside => {
                if is_project_skill_script(&path, ctx) {
                    (DESC_PROJECT_SKILL_SCRIPT, false)
                } else {
                    continue;
                }
            }
        };
        if persistence {
            return Some(CommandFlag {
                description,
                persistence,
            });
        }
        found.get_or_insert(CommandFlag {
            description,
            persistence,
        });
    }
    found
}

/// The path-like words of a command, resolved against the working folder (`~` and the common
/// Windows/POSIX variables expanded).
pub fn command_paths(command: &str, ctx: &PolicyContext) -> Vec<PathBuf> {
    words(command)
        .into_iter()
        .map(|word| expand_variables(&word, ctx))
        .filter(|word| {
            word.contains('/')
                || word.contains('\\')
                || word.starts_with('~')
                || word.starts_with('.')
                || word.chars().nth(1) == Some(':')
        })
        .map(|word| crate::paths::resolve(&word, &ctx.cwd, &ctx.home))
        .collect()
}

/// The words of a command line: split at whitespace and shell punctuation, except inside quotes,
/// where a quoted string (spaces and all) is one word. An unquoted path with spaces in it is not
/// recognised (best effort, see [`command_flag`]).
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                words.push(std::mem::take(&mut current));
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                words.push(std::mem::take(&mut current));
                quote = Some(c);
            }
            None if c.is_whitespace()
                || matches!(c, '`' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | ',' | '=') =>
            {
                words.push(std::mem::take(&mut current));
            }
            None => current.push(c),
        }
    }
    words.push(current);
    words.retain(|word| !word.is_empty());
    words
}

/// `$HOME`, `${HOME}`, `$env:HOME` and `%HOME%` (case-insensitive) for the folders the policy
/// knows about; other variables are left alone.
fn expand_variables(word: &str, ctx: &PolicyContext) -> String {
    let slashed = |path: &Path| {
        path.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    };
    let home = slashed(&ctx.home);
    let roaming = format!("{home}/AppData/Roaming");
    let local = format!("{home}/AppData/Local");
    let data = slashed(&ctx.data_dir);
    let program_data = ctx
        .program_data
        .as_ref()
        .map(|p| slashed(p))
        .unwrap_or_default();
    let vars: [(&str, &str); 6] = [
        ("userprofile", &home),
        ("home", &home),
        ("appdata", &roaming),
        ("localappdata", &local),
        ("programdata", &program_data),
        ("hermes_rs_home", &data),
    ];
    let mut out = word.to_string();
    for (name, value) in vars {
        if value.is_empty() {
            continue;
        }
        for pattern in [
            format!("$env:{name}"),
            format!("${{{name}}}"),
            format!("${name}"),
            format!("%{name}%"),
        ] {
            // `$HOME` must not match the start of `$HOMEPATH`; `${..}` and `%..%` are delimited.
            let open_ended = !pattern.ends_with(['}', '%']);
            // Search after each replacement: a value containing the pattern can't loop forever.
            let mut from = 0;
            while let Some(found) = out[from..].to_ascii_lowercase().find(&pattern) {
                let at = from + found;
                let end = at + pattern.len();
                let continues = out[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                if open_ended && continues {
                    from = end;
                    continue;
                }
                out.replace_range(at..end, value);
                from = at + value.len();
            }
        }
    }
    out
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
                project: None,
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

    fn flag(f: &Fixture, command: &str) -> Option<CommandFlag> {
        command_flag(command, &f.ctx, &|_| false)
    }

    #[test]
    fn commands_naming_protected_paths_are_flagged() {
        let f = Fixture::new();
        let data = f.ctx.data_dir.display().to_string();
        let persistent = |command: &str| {
            let flag = flag(&f, command).unwrap_or_else(|| panic!("not flagged: {command}"));
            assert!(flag.persistence, "{command}: {flag:?}");
        };
        persistent("cat ~/.ssh/id_rsa");
        persistent("echo ssh-rsa AAA >> $HOME/.ssh/authorized_keys");
        persistent("echo evil >> ~/.bashrc");
        persistent(r#"copy x.bat "%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\x.bat""#);
        persistent(r"Set-Content $env:USERPROFILE\Documents\WindowsPowerShell\profile.ps1 'iex x'");
        persistent("echo '#!/bin/sh' > .git/hooks/pre-commit");
        persistent("cd ~/.ssh && ls");
        assert_eq!(
            flag(&f, &format!("rm {data}/sessions.db"))
                .unwrap()
                .description,
            DESC_APP_DATA
        );
        assert_eq!(
            flag(&f, "echo x >> $env:HERMES_RS_HOME/desktop-config.json")
                .unwrap()
                .description,
            DESC_APP_DATA
        );
        assert_eq!(
            flag(&f, &format!("echo x >> {data}/memories/MEMORY.md"))
                .unwrap()
                .description,
            DESC_SELF_IMPROVE
        );
        assert_eq!(
            flag(&f, &format!("python {data}/skills/x/scripts/run.py"))
                .unwrap()
                .description,
            DESC_SKILL_SCRIPT
        );
        // A persistence path wins over a lesser one in the same command.
        assert!(
            flag(&f, &format!("cp {data}/sessions.db ~/.ssh/x"))
                .unwrap()
                .persistence
        );
    }

    #[test]
    fn ordinary_commands_are_not_flagged() {
        let f = Fixture::new();
        for command in [
            "ls -la",
            "cargo build --release",
            "git commit -m \"fix hooks\"",
            "echo hi > out.txt",
            "cat src/main.rs | grep foo",
            "python scripts/run.py --flag=value",
            "type /c/Windows/win.ini",
        ] {
            assert_eq!(flag(&f, command), None, "{command}");
        }
        // The scratch folder lives in the data folder and is fine.
        let scratch = f.ctx.scratch.display().to_string();
        assert_eq!(flag(&f, &format!("echo x > {scratch}/a.txt")), None);
    }

    #[test]
    fn variables_expand_whole_names_only_and_never_loop() {
        let mut f = Fixture::new();
        let home = f.ctx.home.display().to_string().replace('\\', "/");
        assert_eq!(
            expand_variables("$HOME/.ssh", &f.ctx),
            format!("{home}/.ssh")
        );
        assert_eq!(
            expand_variables("$env:userprofile/x", &f.ctx),
            format!("{home}/x")
        );
        // `$HOMEPATH` is another variable, not `$HOME` + "PATH".
        assert_eq!(expand_variables("$HOMEPATH/.ssh", &f.ctx), "$HOMEPATH/.ssh");
        // A value that contains the pattern itself is replaced once.
        f.ctx.home = PathBuf::from("C:/Users/$home");
        assert_eq!(expand_variables("$home/x", &f.ctx), "C:/Users/$home/x");
    }

    #[test]
    fn a_data_folder_under_a_scripts_folder_does_not_make_every_skill_path_a_script() {
        let mut f = Fixture::new();
        let data = f.ctx.data_dir.join("scripts").join("data");
        f.ctx.data_dir = data.clone();
        let command = format!("cat {}/skills/x/SKILL.md", data.display());
        assert_eq!(flag(&f, &command).unwrap().description, DESC_SELF_IMPROVE);
    }

    #[test]
    fn stock_skill_scripts_are_exempt_and_other_skill_scripts_are_not() {
        let f = Fixture::new();
        let script = format!(
            "bash {}/skills/p5js/scripts/setup.sh",
            f.ctx.data_dir.display()
        );
        assert_eq!(command_flag(&script, &f.ctx, &|_| true), None);
        assert_eq!(
            command_flag(&script, &f.ctx, &|_| false)
                .unwrap()
                .description,
            DESC_SKILL_SCRIPT
        );
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
