//! What the diff tab knows, with no views in it: the three scopes and the request for one, the
//! patch a backend answers with and the files parsed out of it ([`Parsed`], [`Entry`]: status
//! letter, counts, binary and truncated flags), the folder tree of the changed files, how a path a
//! card or a link names is matched with a changed file, and how a file is opened in the user's
//! editor ([`editor_command`], built and tested without starting anything).
//!
//! Parsing is GPUI Kit's own (`gpui_kit::component::diff::DiffFile::parse_with`, the
//! "readonly, virtualized patch display" it draws): it reads git's unified diffs with renames,
//! deletions, binary notes and `\ No newline at end of file`. A patch it refuses as a whole is
//! read again one file at a time, so one odd file does not hide the rest ([`Parsed::unreadable`]).
//!
//! t3code: `apps/web/src/components/diffs/diffFileTree.logic.ts` (status of an entry from the
//! file's diff type, one tree entry per path), `DiffFileStatus.tsx` (the partial / failed marks),
//! `lib/diffRendering.ts` (`resolveFileDiffPath`).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use gpui_kit::component::diff::{DiffFile, DiffFileStatus, DiffParseOptions};

use crate::thread::tools::ChangedFile;

/// What the diff compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// The newest turn of the thread: what changed since the commit `HEAD` pointed at when the
    /// turn began (and what the turn's own edit cards named, when that commit is not known).
    Turn,
    /// Everything this thread's own edits changed (the runtime's per-session review: the
    /// checkpoint taken at the thread's first edit when checkpoints are on, else git).
    Thread,
    /// The working tree against `HEAD`, whoever changed it.
    #[default]
    WorkingTree,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Turn, Scope::Thread, Scope::WorkingTree];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Turn => "This turn",
            Scope::Thread => "This thread",
            Scope::WorkingTree => "Working tree",
        }
    }

    /// What the choice means, for its tooltip.
    pub fn hint(self) -> &'static str {
        match self {
            Scope::Turn => "Changes since the latest turn began",
            Scope::Thread => "Changes made by this thread's edits",
            Scope::WorkingTree => "All uncommitted changes against HEAD",
        }
    }

    /// A stable id for element ids and tests.
    pub fn id(self) -> &'static str {
        match self {
            Scope::Turn => "turn",
            Scope::Thread => "thread",
            Scope::WorkingTree => "tree",
        }
    }
}

/// What the thread tells the diff about its newest turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnContext {
    /// The commit `HEAD` was at when the turn started (a turn seen live in this run).
    pub base: Option<String>,
    /// The files the turn's edit cards named, as they spelled them: they narrow the working
    /// tree's diff to the turn when the base is not known.
    pub files: Vec<String>,
}

/// One diff to read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffRequest {
    pub session: String,
    pub scope: Scope,
    pub turn: TurnContext,
}

/// A backend's answer: a Git-style unified patch of every changed file of the scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffData {
    pub patch: String,
    /// What to tell the user instead of, or beside, the files (not a repository, nothing edited
    /// yet, the base is unknown and the answer is approximate...).
    pub note: Option<String>,
    /// Untracked files left out for size or count.
    pub skipped: usize,
    /// Whether a file can be reverted in this scope.
    pub revert: Revert,
    /// Whether the files are narrowed to what the turn's edit cards named (the base is unknown).
    pub narrowed: bool,
}

/// How a file of the shown scope can be put back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Revert {
    /// It cannot, in this scope.
    #[default]
    None,
    /// To `HEAD`: tracked files restored, untracked ones deleted. Nothing can bring it back.
    ToHead,
    /// To how the file was before the thread's first edit; the core takes a checkpoint first, so
    /// `/rollback` can undo it.
    ToThreadStart,
}

/// A changed file's status, as git names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Conflicted,
}

impl Status {
    /// The letter in the list (`git status`'s).
    pub fn letter(self) -> char {
        match self {
            Status::Added => 'A',
            Status::Modified => 'M',
            Status::Deleted => 'D',
            Status::Renamed => 'R',
            Status::Copied => 'C',
            Status::Conflicted => 'U',
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Status::Added => "Added",
            Status::Modified => "Modified",
            Status::Deleted => "Deleted",
            Status::Renamed => "Renamed",
            Status::Copied => "Copied",
            Status::Conflicted => "Conflicted",
        }
    }
}

/// One row of the changed-files list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The new path, `/`-separated, relative to the repository (or the folder the diff was read
    /// in).
    pub path: String,
    /// The old path of a rename or copy.
    pub original: Option<String>,
    pub status: Status,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    /// The diff stops before the file's end (the patch was cut at a size limit).
    pub truncated: bool,
}

/// A patch read: the files Kit draws, the list rows, and what could not be read.
#[derive(Clone, Default)]
pub struct Parsed {
    pub files: Vec<DiffFile>,
    pub entries: Vec<Entry>,
    /// Files whose part of the patch could not be read: `(path, why)`.
    pub unreadable: Vec<(String, String)>,
    /// The new side's blob id of each file, as the patch's `index` line names it (abbreviated;
    /// `0000000` for a deleted file): what the file was when the diff was read. A revert is
    /// refused when the file is no longer that ([`blob_of`]).
    pub blobs: std::collections::HashMap<String, String>,
}

fn status_of(file: &DiffFile) -> Status {
    match file.status() {
        DiffFileStatus::Added => Status::Added,
        DiffFileStatus::Deleted => Status::Deleted,
        DiffFileStatus::Renamed => Status::Renamed,
        DiffFileStatus::Copied => Status::Copied,
        DiffFileStatus::Conflicted => Status::Conflicted,
        _ => Status::Modified,
    }
}

impl Entry {
    fn of(file: &DiffFile) -> Self {
        Self {
            path: file.path().to_string(),
            original: file
                .original_path()
                .map(|p| p.to_string())
                .filter(|o| o.as_str() != file.path().as_ref() as &str),
            status: status_of(file),
            added: file.additions(),
            removed: file.deletions(),
            binary: file.is_binary(),
            truncated: file.is_truncated(),
        }
    }

    /// As the turn footer and the tree spell a changed file.
    pub fn changed_file(&self) -> ChangedFile {
        ChangedFile {
            path: self.path.clone(),
            additions: self.added.min(u32::MAX as usize) as u32,
            deletions: self.removed.min(u32::MAX as usize) as u32,
        }
    }
}

/// Natural-ish order of paths: case-insensitive, folders and files as t3code's tree sorts them
/// (plain path order is what its flat list shows).
fn path_key(path: &str) -> String {
    path.to_lowercase()
}

impl Parsed {
    fn from_files(
        mut files: Vec<DiffFile>,
        unreadable: Vec<(String, String)>,
        patch: &str,
    ) -> Self {
        files.sort_by_key(|f| path_key(f.path().as_ref()));
        let entries = files.iter().map(Entry::of).collect();
        let blobs = sections(patch)
            .iter()
            .filter_map(|s| Some((section_path(s), blob_of(s)?)))
            .collect();
        Self {
            files,
            entries,
            unreadable,
            blobs,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.unreadable.is_empty()
    }

    /// Files, added and removed lines over every entry.
    pub fn totals(&self) -> (usize, usize, usize) {
        totals(&self.entries)
    }

    /// Only the entries `keep` holds for (the files follow).
    pub fn retain_entries(&mut self, keep: impl Fn(&Entry) -> bool) {
        let kept: Vec<bool> = self.entries.iter().map(&keep).collect();
        let mut at = 0;
        self.files.retain(|_| {
            at += 1;
            kept[at - 1]
        });
        self.entries.retain(&keep);
    }

    /// Only the files for which `keep(path)` holds (the original paths still count for renames).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.files.retain(|f| {
            keep(f.path().as_ref()) || f.original_path().is_some_and(|o| keep(o.as_ref()))
        });
        self.entries = self.files.iter().map(Entry::of).collect();
        self.unreadable.retain(|(path, _)| keep(path));
    }
}

/// `(files, added, removed)` of `entries`.
pub fn totals(entries: &[Entry]) -> (usize, usize, usize) {
    entries.iter().fold((0, 0, 0), |(n, a, r), e| {
        (n + 1, a + e.added, r + e.removed)
    })
}

/// The patch's files. A patch Kit refuses as a whole is cut at its `diff --git` lines and read
/// file by file; each file that still fails is listed with the parser's reason.
pub fn parse_patch(patch: &str) -> Parsed {
    if patch.trim().is_empty() {
        return Parsed::default();
    }
    // A patch cut at a size limit ends inside a hunk: accept it, the file is marked truncated.
    let options = DiffParseOptions::new().truncated_hunks(true);
    match DiffFile::parse_with(patch, options) {
        Ok(files) => Parsed::from_files(files, Vec::new(), patch),
        Err(_) => {
            let mut files = Vec::new();
            let mut unreadable = Vec::new();
            for section in sections(patch) {
                match DiffFile::parse_with(&section, options) {
                    Ok(parsed) => files.extend(parsed),
                    Err(error) => unreadable.push((section_path(&section), error.to_string())),
                }
            }
            Parsed::from_files(files, unreadable, patch)
        }
    }
}

/// The new side's blob id a file's section names in its `index <old>..<new>[ <mode>]` line
/// (before the first hunk): `None` when it has none (a mode-only change, the runtime's
/// per-thread patch).
pub fn blob_of(section: &str) -> Option<String> {
    section
        .lines()
        .take_while(|line| !line.starts_with("@@") && !line.starts_with("--- "))
        .find_map(|line| line.strip_prefix("index "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|range| range.split_once(".."))
        .map(|(_, new)| new.to_string())
        .filter(|new| !new.is_empty() && new.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The patch cut at each `diff --git` line.
fn sections(patch: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") || out.is_empty() {
            out.push(String::new());
        }
        if let Some(last) = out.last_mut() {
            last.push_str(line);
        }
    }
    out.retain(|s| s.starts_with("diff --git "));
    out
}

/// The new path a section's `diff --git a/x b/x` line names.
fn section_path(section: &str) -> String {
    let head = section.lines().next().unwrap_or_default();
    let rest = head.strip_prefix("diff --git ").unwrap_or(head);
    match rest.rsplit_once(" b/") {
        Some((_, path)) => path.trim_matches('"').to_string(),
        None => rest.to_string(),
    }
}

/// A path as the diff and a card would compare it: `/`, lowercase, no leading `./`.
pub fn normal(path: &str) -> String {
    let path = path.trim().replace('\\', "/").to_lowercase();
    path.trim_start_matches("./").to_string()
}

/// Whether `named` (a path as a card, a link or a tool call spelled it: absolute or relative,
/// either slash) names the changed file `changed` (relative to the repository).
pub fn names_file(named: &str, changed: &str) -> bool {
    let (named, changed) = (normal(named), normal(changed));
    if named.is_empty() || changed.is_empty() {
        return false;
    }
    named == changed
        || named.ends_with(&format!("/{changed}"))
        || changed.ends_with(&format!("/{named}"))
}

/// A live turn's changed files: what git says changed since the turn started, joined with what
/// the turn's edit cards named. Kept from git: every changed or deleted or renamed tracked file
/// and a new file only when a card wrote it (an untracked file that was already there is not the
/// turn's); kept from the cards: files git does not list (ignored by git, or written and put back,
/// which the cards cannot tell apart). Counts come from git where it lists the file. The cards'
/// order first (the order the turn touched them), then the other files by path.
pub fn merge_turn_files(edited: &[ChangedFile], real: &[Entry]) -> Vec<ChangedFile> {
    let mut out: Vec<ChangedFile> = Vec::new();
    for card in edited {
        match real.iter().find(|e| names_file(&card.path, &e.path)) {
            Some(entry) => out.push(entry.changed_file()),
            None => out.push(card.clone()),
        }
    }
    for entry in real {
        let wanted = entry.status != Status::Added;
        if wanted && !out.iter().any(|f| f.path == entry.path) {
            out.push(entry.changed_file());
        }
    }
    out
}

/// The folder tree of the entries, for the list's folder view.
pub fn tree(entries: &[Entry]) -> Vec<crate::thread::tools::Node> {
    let files: Vec<ChangedFile> = entries.iter().map(Entry::changed_file).collect();
    crate::thread::tools::build_tree(&files)
}

/// The folders of `entries` (every ancestor path), for "collapse all".
pub fn folders(entries: &[Entry]) -> HashSet<String> {
    crate::thread::tools::dir_paths(&tree(entries))
        .into_iter()
        .collect()
}

/// A patch for what the runtime's per-thread review knows (its files carry hunks without the
/// git header): the header lines a parser needs, then the hunks.
pub fn patch_from_changes(files: &[hermes_runtime::changes::FileChange]) -> String {
    let mut out = String::new();
    for file in files {
        let path = &file.path;
        out.push_str(&format!("diff --git a/{path} b/{path}\n"));
        if file.patch.starts_with("Binary files") {
            out.push_str(&file.patch);
            out.push('\n');
            continue;
        }
        // Git's order: the mode line, then the two file headers.
        if file.created {
            out.push_str("new file mode 100644\n--- /dev/null\n");
        } else if file.deleted {
            out.push_str(&format!("deleted file mode 100644\n--- a/{path}\n"));
        } else {
            out.push_str(&format!("--- a/{path}\n"));
        }
        if file.deleted {
            out.push_str("+++ /dev/null\n");
        } else {
            out.push_str(&format!("+++ b/{path}\n"));
        }
        out.push_str(&file.patch);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// `path` made absolute under `cwd` (an absolute path is kept; `/` becomes `\`).
pub fn absolute(cwd: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path.replace('/', "\\"));
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

// --- the editor ---------------------------------------------------------------------------------

/// A program to start and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorCommand {
    pub editor: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// The editor to open a file in: Zed when it is installed, else VS Code, each found with
/// `find` (the file name to look for: `zed.exe`, `code.cmd`...). `None` when neither is; the
/// caller then opens the file with the system's default program. There is no setting for the
/// editor until Phase 7.
pub fn editor_command(
    find: &dyn Fn(&str) -> Option<PathBuf>,
    file: &Path,
    line: Option<u32>,
) -> Option<EditorCommand> {
    let target = match line {
        Some(line) => format!("{}:{line}", file.display()),
        None => file.display().to_string(),
    };
    if let Some(program) = find("zed.exe") {
        return Some(EditorCommand {
            editor: "Zed",
            program,
            args: vec![target],
        });
    }
    ["code.cmd", "code.exe"].iter().find_map(|name| {
        find(name).map(|program| EditorCommand {
            editor: "VS Code",
            program,
            args: vec!["-g".to_string(), target.clone()],
        })
    })
}

/// The first `name` found in the folders of the `PATH` value `path_var`, as an absolute path
/// (that path is what is started). A relative entry (`.`, an empty one) is skipped: it would
/// resolve against whatever folder the app runs in, a repository's own `zed.exe` included.
pub fn find_on_path(path_var: &OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(path: &str, body: &str) -> String {
        format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{body}")
    }

    #[test]
    fn a_git_patch_gives_one_entry_per_file_with_status_and_counts() {
        let text = [
            patch("src/b.rs", "@@ -1,2 +1,2 @@\n-old\n+new\n same\n"),
            "diff --git a/old.txt b/new name.txt\nsimilarity index 90%\nrename from old.txt\nrename to new name.txt\nindex 1..2 100644\n--- a/old.txt\n+++ b/new name.txt\n@@ -1 +1 @@\n-a\n+b\n".to_string(),
            "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\nindex 1..0\n--- a/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n".to_string(),
            "diff --git a/fresh.txt b/fresh.txt\nnew file mode 100644\nindex 0..1\n--- /dev/null\n+++ b/fresh.txt\n@@ -0,0 +1,3 @@\n+1\n+2\n+3\n".to_string(),
            "diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n".to_string(),
        ]
        .concat();
        let parsed = parse_patch(&text);
        assert!(parsed.unreadable.is_empty(), "{:?}", parsed.unreadable);
        let rows: Vec<(String, char, usize, usize, bool)> = parsed
            .entries
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    e.status.letter(),
                    e.added,
                    e.removed,
                    e.binary,
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("fresh.txt".to_string(), 'A', 3, 0, false),
                ("gone.txt".to_string(), 'D', 0, 2, false),
                ("logo.png".to_string(), 'M', 0, 0, true),
                ("new name.txt".to_string(), 'R', 1, 1, false),
                ("src/b.rs".to_string(), 'M', 1, 1, false),
            ]
        );
        assert_eq!(parsed.entries[3].original.as_deref(), Some("old.txt"));
        assert_eq!(parsed.totals(), (5, 5, 4));
    }

    #[test]
    fn edge_patches_parse() {
        // No newline at the end of the file, and CRLF line endings.
        let no_newline = patch(
            "a.txt",
            "@@ -1 +1 @@\n-one\n\\ No newline at end of file\n+two\n\\ No newline at end of file\n",
        );
        let parsed = parse_patch(&no_newline);
        assert_eq!(parsed.entries.len(), 1, "{:?}", parsed.unreadable);
        assert_eq!((parsed.entries[0].added, parsed.entries[0].removed), (1, 1));
        let crlf = patch("w.txt", "@@ -1,2 +1,2 @@\r\n-one\r\n+uno\r\n keep\r\n");
        let parsed = parse_patch(&crlf);
        assert_eq!(parsed.entries.len(), 1, "{:?}", parsed.unreadable);
        assert_eq!((parsed.entries[0].added, parsed.entries[0].removed), (1, 1));
        // An empty new file has no hunks.
        let empty =
            "diff --git a/empty.txt b/empty.txt\nnew file mode 100644\nindex 0000000..e69de29\n";
        let parsed = parse_patch(empty);
        assert_eq!(parsed.entries.len(), 1, "{:?}", parsed.unreadable);
        assert_eq!(parsed.entries[0].status, Status::Added);
        // Nothing at all.
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("\n\n").is_empty());
    }

    #[test]
    fn a_huge_hunk_and_a_cut_patch_still_parse() {
        let body: String = (0..5000).map(|n| format!("+line {n}\n")).collect();
        let text = patch("big.txt", &format!("@@ -0,0 +1,5000 @@\n{body}"));
        let parsed = parse_patch(&text);
        assert_eq!(parsed.entries[0].added, 5000);
        // Cut in the middle of the hunk (a size limit): accepted, and the file says so.
        let cut = patch(
            "big.txt",
            &format!("@@ -0,0 +1,5000 @@\n{}", &body[..body.len() / 2]),
        );
        let parsed = parse_patch(&cut);
        assert_eq!(parsed.entries.len(), 1, "{:?}", parsed.unreadable);
        assert!(parsed.entries[0].added < 5000 && parsed.entries[0].added > 100);
    }

    #[test]
    fn a_file_the_parser_refuses_does_not_hide_the_others() {
        let good = patch("ok.txt", "@@ -1 +1 @@\n-a\n+b\n");
        let bad =
            "diff --git a/bad.txt b/bad.txt\n--- a/bad.txt\n+++ b/bad.txt\n@@ nonsense @@\n+x\n";
        let parsed = parse_patch(&format!("{good}{bad}"));
        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["ok.txt"]
        );
        assert_eq!(parsed.unreadable.len(), 1);
        assert_eq!(parsed.unreadable[0].0, "bad.txt");
    }

    #[test]
    fn a_path_named_by_a_card_finds_its_changed_file() {
        assert!(names_file(r"C:\work\proj\src\Main.rs", "src/main.rs"));
        assert!(names_file("src/main.rs", "src/main.rs"));
        assert!(names_file("./src/main.rs", "src/main.rs"));
        assert!(names_file("main.rs", "src/main.rs"));
        assert!(!names_file("src/mainx.rs", "src/main.rs"));
        assert!(!names_file("other/main.rs", "src/main.rs"));
        assert!(!names_file("", "src/main.rs"));
    }

    #[test]
    fn retain_narrows_files_and_entries_together() {
        let text = [
            patch("a.rs", "@@ -1 +1 @@\n-1\n+2\n"),
            patch("b/c.rs", "@@ -1 +1 @@\n-1\n+2\n"),
        ]
        .concat();
        let mut parsed = parse_patch(&text);
        parsed.retain(|path| names_file(r"C:\x\b\c.rs", path));
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.entries[0].path, "b/c.rs");
    }

    #[test]
    fn the_runtime_changes_become_a_patch_that_parses() {
        use hermes_runtime::changes::FileChange;
        let files = [
            FileChange {
                path: "src/a.rs".into(),
                added: 1,
                removed: 1,
                created: false,
                deleted: false,
                patch: "@@ -1 +1 @@\n-x\n+y\n".into(),
            },
            FileChange {
                path: "new.txt".into(),
                added: 1,
                removed: 0,
                created: true,
                deleted: false,
                patch: "@@ -0,0 +1 @@\n+hi\n".into(),
            },
            FileChange {
                path: "gone.txt".into(),
                added: 0,
                removed: 1,
                created: false,
                deleted: true,
                patch: "@@ -1 +0,0 @@\n-bye\n".into(),
            },
            FileChange {
                path: "logo.png".into(),
                added: 0,
                removed: 0,
                created: false,
                deleted: false,
                patch: "Binary files a/logo.png and b/logo.png differ".into(),
            },
        ];
        let parsed = parse_patch(&patch_from_changes(&files));
        assert!(parsed.unreadable.is_empty(), "{:?}", parsed.unreadable);
        let letters: Vec<(String, char)> = parsed
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.status.letter()))
            .collect();
        assert_eq!(
            letters,
            vec![
                ("gone.txt".to_string(), 'D'),
                ("logo.png".to_string(), 'M'),
                ("new.txt".to_string(), 'A'),
                ("src/a.rs".to_string(), 'M'),
            ]
        );
    }

    #[test]
    fn the_editor_is_zed_then_vs_code_then_the_default_program() {
        let file = Path::new(r"C:\proj\src\main.rs");
        let only = |name: &'static str| {
            move |asked: &str| (asked == name).then(|| PathBuf::from(format!(r"C:\bin\{name}")))
        };
        let zed = editor_command(&only("zed.exe"), file, Some(12)).unwrap();
        assert_eq!(zed.editor, "Zed");
        assert_eq!(zed.program, PathBuf::from(r"C:\bin\zed.exe"));
        assert_eq!(zed.args, vec![r"C:\proj\src\main.rs:12".to_string()]);
        let code = editor_command(&only("code.cmd"), file, Some(3)).unwrap();
        assert_eq!(code.editor, "VS Code");
        assert_eq!(
            code.args,
            vec!["-g".to_string(), r"C:\proj\src\main.rs:3".to_string()]
        );
        let plain = editor_command(&only("code.exe"), file, None).unwrap();
        assert_eq!(
            plain.args,
            vec!["-g".to_string(), r"C:\proj\src\main.rs".to_string()]
        );
        // Both installed: Zed wins. Neither: the caller opens the default program.
        let both = |asked: &str| Some(PathBuf::from(asked));
        assert_eq!(editor_command(&both, file, None).unwrap().editor, "Zed");
        assert_eq!(editor_command(&|_: &str| None, file, None), None);
    }

    #[test]
    fn programs_are_found_on_the_path() {
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::write(dir.path().join("zed.exe"), b"").unwrap();
        let path = std::env::join_paths([Path::new(r"C:\no\such\folder"), dir.path()]).unwrap();
        assert_eq!(
            find_on_path(&path, "zed.exe"),
            Some(dir.path().join("zed.exe"))
        );
        assert_eq!(find_on_path(&path, "code.cmd"), None);
        // A relative entry is never searched (it would be the current folder's program): the
        // test runs in the crate's folder, which has a `Cargo.toml`.
        let cwd = std::env::current_dir().unwrap();
        assert!(cwd.join("Cargo.toml").is_file());
        let relative = std::env::join_paths([Path::new("."), Path::new("")]).unwrap();
        assert_eq!(find_on_path(&relative, "Cargo.toml"), None, "{cwd:?}");
    }

    #[test]
    fn a_files_blob_is_read_from_its_index_line() {
        let text = [
            "diff --git a/a.txt b/a.txt\nindex 1234567..89abcde 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n+b\n",
            "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\nindex 1234567..0000000\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n",
            "diff --git a/logo.png b/logo.png\nindex 1111111..2222222 100644\nBinary files a/logo.png and b/logo.png differ\n",
            "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n",
        ]
        .concat();
        let parsed = parse_patch(&text);
        assert_eq!(parsed.blobs.get("a.txt").map(String::as_str), Some("89abcde"));
        assert_eq!(
            parsed.blobs.get("gone.txt").map(String::as_str),
            Some("0000000")
        );
        assert_eq!(
            parsed.blobs.get("logo.png").map(String::as_str),
            Some("2222222")
        );
        assert_eq!(parsed.blobs.get("run.sh"), None, "a mode change has none");
    }

    #[test]
    fn a_turns_files_are_gits_where_git_knows_and_the_cards_elsewhere() {
        let entry = |path: &str, status, added, removed| Entry {
            path: path.to_string(),
            original: None,
            status,
            added,
            removed,
            binary: false,
            truncated: false,
        };
        let card = |path: &str, a, d| ChangedFile {
            path: path.to_string(),
            additions: a,
            deletions: d,
        };
        let edited = vec![
            card(r"C:\proj\src\a.rs", 9, 9),
            card("notes/new.md", 3, 0),
            card("ignored/out.log", 1, 0),
        ];
        let real = vec![
            entry("src/a.rs", Status::Modified, 2, 1),
            entry("src/by_terminal.rs", Status::Modified, 4, 4),
            entry("notes/new.md", Status::Added, 3, 0),
            // Was there before the turn: not the turn's.
            entry("scratch/old-untracked.txt", Status::Added, 50, 0),
            entry("gone.rs", Status::Deleted, 0, 7),
        ];
        let files = merge_turn_files(&edited, &real);
        let named: Vec<(&str, u32, u32)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.additions, f.deletions))
            .collect();
        assert_eq!(
            named,
            vec![
                ("src/a.rs", 2, 1),
                ("notes/new.md", 3, 0),
                ("ignored/out.log", 1, 0),
                ("src/by_terminal.rs", 4, 4),
                ("gone.rs", 0, 7),
            ]
        );
    }

    #[test]
    fn the_tree_groups_by_folder() {
        let entries: Vec<Entry> = ["src/a.rs", "src/ui/b.rs", "README.md"]
            .iter()
            .map(|p| Entry {
                path: p.to_string(),
                original: None,
                status: Status::Modified,
                added: 1,
                removed: 0,
                binary: false,
                truncated: false,
            })
            .collect();
        let nodes = tree(&entries);
        assert_eq!(nodes.len(), 2, "src folder and README");
        assert!(folders(&entries).contains("src"));
    }
}
