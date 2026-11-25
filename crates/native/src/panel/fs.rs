//! What the Files tab asks of the disk, with no views in it: a folder's entries with what git
//! ignores marked, one file read and classified (text, image, binary), the list of every file for
//! the finder, and the rules that keep all of it inside the thread's folder. The real core's
//! backend calls these with the thread's folder as `root`; the fixture backend's disk mode does
//! the same, so the tests run the real code on real temporary trees.
//!
//! **Where files may be:** a path (a link in a message, a row of the tree) is resolved against
//! `root` with `hermes_core::files::resolve` (relative, absolute, `file:`, `~`) and refused unless
//! it is lexically below `root` (names compared without case) **before the disk is asked**
//! ([`Refused::Outside`]: `../x`, another drive, `C:foo`, `\\host\share`, `\\?\`, `\\.\pipe`);
//! `src/../Cargo.toml` is fine. Names Windows reads as something else are refused (`a.txt:stream`,
//! `CON`, `nul.txt`, a trailing dot or space). The links on the way are read (`read_link`), not
//! followed, and a path whose links lead out (or to another machine: opening `\\host\share` sends
//! the Windows sign-in there) is [`Refused::Escapes`]; only then is it made canonical and checked
//! again against the canonical `root`. Credentials are refused with the core's project-browser
//! rule (`hermes_core::files::is_private`: the Tauri preview's `is_sensitive`, plus the runtime's
//! secret files (keys, `.pem`, `.netrc`...), credential folders (`.ssh`, `.aws`...), and `.git`),
//! on the asked path and on what it resolves to, and the app's data folder is kept out
//! ([`keep_out`]). A file is opened once and the handle is checked (a disk file, its final path
//! still inside: [`read_file`]), so a swap after the checks changes nothing that is read; the read
//! is bounded. Nothing here opens, runs or writes anything else.
//!
//! **What git ignores:** the `.gitignore` files from the repository's top (the nearest folder with
//! a `.git`, else `root`) down to the folder listed, and `.git/info/exclude`, evaluated deepest
//! first so a nested file's `!negation` wins ([`Ignores`], over the `ignore` crate's matcher, the
//! one Ripgrep and the runtime's tools use). Not read: the user's global excludes file. An ignored
//! folder is listed on demand (it can hold a hundred thousand entries, so a listing is cut at
//! [`LIST_LIMIT`]) and everything in it counts as ignored. The finder's index leaves ignored
//! folders alone altogether.
//!
//! t3code: `apps/web/src/components/files/FileBrowserPanel.tsx` + `useDirectoryEntries.ts` (a
//! folder's entries on demand, ignored folders dimmed and not expanded by "expand all"),
//! `ProjectFilePicker.logic.ts` and `projectFilesQueryState.ts` (the finder's result cap),
//! `FilePreviewPanel.tsx` (large and binary files), `decodeImagePreview.ts` (images).

use std::path::{Component, Path, PathBuf};

use hermes_core::files;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// A folder lists at most this many entries (the rest are counted).
pub const LIST_LIMIT: usize = 10_000;
/// A file up to this size gets syntax colours; a larger one is drawn as plain text.
pub const HIGHLIGHT_MAX: u64 = 2 * 1024 * 1024;
/// A file larger than this is not opened (the editor is offered instead).
pub const OPEN_MAX: u64 = 20 * 1024 * 1024;
/// The finder indexes at most this many files.
pub const INDEX_LIMIT: usize = 200_000;
/// The finder's index grows in batches of this many paths.
pub const BATCH: usize = 2_000;

/// Why a path is not shown. `message()` is the sentence for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The thread's folder is gone.
    NoFolder,
    /// The path is not inside the thread's folder.
    Outside(String),
    /// The path is inside, but a link on the way leads out.
    Escapes(String),
    /// Credentials.
    Sensitive,
    NotFound(String),
    NotAFile(String),
    TooLarge {
        path: String,
        size: u64,
    },
    /// The disk refused.
    Unreadable(String),
}

impl Refused {
    pub fn message(&self) -> String {
        match self {
            Refused::NoFolder => "This thread's folder no longer exists.".to_string(),
            Refused::Outside(path) => {
                format!("{path} is outside this thread's folder, so it is not shown here.")
            }
            Refused::Escapes(path) => format!(
                "{path} goes through a link that leads outside this thread's folder, so it is not followed."
            ),
            Refused::Sensitive => "That file holds credentials and isn't shown.".to_string(),
            Refused::NotFound(path) => format!("{path} was not found."),
            Refused::NotAFile(path) => format!("{path} is not a file."),
            Refused::TooLarge { path, size } => format!(
                "{path} is {}; files over {} are not opened here.",
                human_size(*size),
                human_size(OPEN_MAX)
            ),
            Refused::Unreadable(why) => why.clone(),
        }
    }

    /// Whether "Open in editor" makes sense: a file that is too large for the view still opens
    /// there; a refused path never does.
    pub fn can_open_in_editor(&self) -> bool {
        matches!(self, Refused::TooLarge { .. })
    }
}

/// `1.4 MB`, `812 bytes`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

// --- where a path is ------------------------------------------------------------------------------

/// A path found inside the folder: how the tree names it and where it is.
#[derive(Clone, Debug)]
pub struct Located {
    /// Relative to the folder, with `/`; empty for the folder itself.
    pub rel: String,
    /// The canonical path.
    pub abs: PathBuf,
    /// The canonical folder.
    pub root: PathBuf,
}

/// `path` with `.` and `..` folded away, without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn io_refusal(error: &std::io::Error, asked: &str) -> Refused {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            Refused::NotFound(asked.to_string())
        }
        std::io::ErrorKind::PermissionDenied => {
            Refused::Unreadable(format!("{asked} is not readable (access denied)."))
        }
        _ => Refused::Unreadable(format!("{asked} could not be read: {error}.")),
    }
}

/// Folders never shown even when a project holds them (canonical): the app's data folder.
static KEPT_OUT: std::sync::RwLock<Vec<PathBuf>> = std::sync::RwLock::new(Vec::new());

/// Keeps `folder` and everything in it out of every listing, read, index and editor launch of a
/// project that holds it (a project whose folder is inside it still shows its own files): the
/// app's data folder (`Core::data_dir`: sessions, settings, MCP tokens), as the runtime's path
/// policy keeps the agent's file tools out of it.
pub fn keep_out(folder: &Path) {
    let Ok(canonical) = std::fs::canonicalize(folder) else {
        return;
    };
    let mut kept = KEPT_OUT.write().unwrap_or_else(|e| e.into_inner());
    if !kept.contains(&canonical) {
        kept.push(canonical);
    }
}

/// Whether `path` (canonical) is in a kept-out folder that does not also hold `root` (canonical).
fn kept_out(path: &Path, root: &Path) -> bool {
    KEPT_OUT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|k| path.starts_with(k) && !root.starts_with(k))
}

/// Not to be shown: credentials, keys, git's own folder (`hermes_core::files::is_private`) or a
/// kept-out folder. `path` canonical.
fn private(path: &Path, root: &Path) -> bool {
    files::is_private(path) || kept_out(path, root)
}

/// A path component compared as Windows compares names (without case).
fn name_key(part: Component<'_>) -> String {
    let text = part.as_os_str().to_string_lossy();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

/// The components of `path` below `root`, compared lexically (no disk), or `None` when `path` is
/// not below it. Both go through [`plain`] and [`lexical`] first.
fn below(path: &Path, root: &Path) -> Option<Vec<std::ffi::OsString>> {
    let path = lexical(&plain(path));
    let root = lexical(&plain(root));
    let mut parts = path.components();
    for want in root.components() {
        let got = parts.next()?;
        if name_key(got) != name_key(want) {
            return None;
        }
    }
    Some(parts.map(|c| c.as_os_str().to_os_string()).collect())
}

/// Whether `name` is one Windows treats as something else than a file of that name: a device
/// (`CON`, `NUL`, `COM1.txt`...), a stream of a file (`a.txt:stream`, `.env::$DATA`), or one whose
/// trailing dots or spaces Windows drops (`.env.` opens `.env`).
fn odd_name(name: &str) -> bool {
    const DEVICES: [&str; 6] = ["con", "prn", "aux", "nul", "conin$", "conout$"];
    if name.contains(':') || name.ends_with('.') && name != "." && name != ".." || name.ends_with(' ')
    {
        return true;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_lowercase();
    DEVICES.contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// Follows the links on the way from `root` down to `candidate` by reading them, never by opening
/// what they point at, and says whether every one stays (lexically) inside `root`. A link that
/// leads out, or to another machine (`\\host\share`: just opening it would send the user's
/// Windows sign-in to that host), is not followed. Up to 16 links deep; more counts as out.
/// `roots`: the folder as the session names it and as the disk does (a link's target may use
/// either spelling, an 8.3 short name among them); one spelled some third way counts as out.
fn links_stay_inside(roots: &[&Path], candidate: &Path, depth: usize) -> bool {
    let Some((root, parts)) = roots
        .iter()
        .find_map(|root| below(candidate, root).map(|parts| (*root, parts)))
    else {
        return false;
    };
    if depth > 16 {
        return false;
    }
    let mut at = plain(root);
    for (n, part) in parts.iter().enumerate() {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let Ok(target) = std::fs::read_link(&at) else {
                    return false;
                };
                let target = match at.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
                let mut next = plain(&target);
                for rest in &parts[n + 1..] {
                    next.push(rest);
                }
                return links_stay_inside(roots, &next, depth + 1);
            }
            Ok(_) => {}
            // Not there: nothing further can be a link.
            Err(_) => return true,
        }
    }
    true
}

/// Finds what `asked` names inside `root`, or says why it is not shown. `asked` may be relative to
/// `root`, absolute, a `file:` URL or `~/...`.
///
/// Nothing outside `root` is touched: a path that is not lexically below `root` is refused before
/// the disk is asked (so a link to `\\host\share` in a message never reaches the network), the
/// links on the way are read, not followed, until they are known to stay inside, and only then is
/// the path made canonical and checked again.
pub fn locate(root: &Path, asked: &str) -> Result<Located, Refused> {
    let asked = asked.trim();
    let root_c = std::fs::canonicalize(root).map_err(|_| Refused::NoFolder)?;
    if asked.is_empty() {
        return Ok(Located {
            rel: String::new(),
            abs: root_c.clone(),
            root: root_c,
        });
    }
    if asked.contains('\0') {
        return Err(Refused::NotFound(asked.replace('\0', "")));
    }
    let candidate = plain(&files::resolve(asked, Some(root)));
    // Below `root` as given, or as the disk spells it (a session folder kept in another case or
    // through a link).
    let base = if below(&candidate, root).is_some() {
        plain(root)
    } else if below(&candidate, &root_c).is_some() {
        plain(&root_c)
    } else {
        return Err(Refused::Outside(asked.to_string()));
    };
    let parts = below(&candidate, &base).unwrap_or_default();
    if parts.iter().any(|p| odd_name(&p.to_string_lossy())) {
        return Err(Refused::NotAFile(asked.to_string()));
    }
    if files::is_private(&candidate) {
        return Err(Refused::Sensitive);
    }
    if !links_stay_inside(&[root, &root_c], &candidate, 0) {
        return Err(Refused::Escapes(asked.to_string()));
    }
    let abs = std::fs::canonicalize(&candidate).map_err(|e| io_refusal(&e, asked))?;
    if !abs.starts_with(&root_c) {
        return Err(Refused::Escapes(asked.to_string()));
    }
    if private(&abs, &root_c) {
        return Err(Refused::Sensitive);
    }
    let rel = abs
        .strip_prefix(&root_c)
        .map(rel_string)
        .unwrap_or_default();
    Ok(Located {
        rel,
        abs,
        root: root_c,
    })
}

/// `path` without the verbatim prefix `canonicalize` gives on Windows (a program started on it,
/// an editor, wants the plain `C:\...` form). A UNC share keeps its prefix.
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// `a/b/c` from a relative path.
fn rel_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

// --- what git ignores -----------------------------------------------------------------------------

/// The `.gitignore` rules that apply below a folder, shallowest first.
#[derive(Clone, Default)]
pub struct Ignores {
    chain: Vec<Gitignore>,
}

fn matcher(dir: &Path, file: &Path) -> Option<Gitignore> {
    // A plain file only: a `.gitignore` that is a link is not followed (it could lead anywhere,
    // another machine's share included).
    if !std::fs::symlink_metadata(file).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut builder = GitignoreBuilder::new(dir);
    // Git for Windows ignores case by default (`core.ignorecase`).
    builder
        .case_insensitive(cfg!(windows))
        .expect("a bool option");
    let _ = builder.add(file);
    builder.build().ok().filter(|m| !m.is_empty())
}

impl Ignores {
    /// The rules for the entries of `dir` (canonical, inside the canonical `root`): the repository
    /// top's down to `dir`.
    pub fn for_dir(root: &Path, dir: &Path) -> Self {
        let top = root
            .ancestors()
            // `symlink_metadata`: a `.git` that is a link is seen, not followed.
            .find(|a| std::fs::symlink_metadata(a.join(".git")).is_ok())
            .unwrap_or(root)
            .to_path_buf();
        let mut chain = Vec::new();
        let exclude = top.join(".git").join("info").join("exclude");
        if let Some(m) = matcher(&top, &exclude) {
            chain.push(m);
        }
        // From the top down to `dir`: every folder's own `.gitignore`.
        let mut folders: Vec<&Path> = dir
            .ancestors()
            .take_while(|a| a.starts_with(&top))
            .collect();
        folders.reverse();
        for folder in folders {
            if let Some(m) = matcher(folder, &folder.join(".gitignore")) {
                chain.push(m);
            }
        }
        Self { chain }
    }

    /// The rules after entering `dir`.
    pub fn enter(&mut self, dir: &Path) -> bool {
        match matcher(dir, &dir.join(".gitignore")) {
            Some(m) => {
                self.chain.push(m);
                true
            }
            None => false,
        }
    }

    pub fn leave(&mut self) {
        self.chain.pop();
    }

    /// Whether `path` (below the folder the rules are for) is ignored: the deepest rule that
    /// says anything decides.
    pub fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        for m in self.chain.iter().rev() {
            match m.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

// --- a folder's entries ---------------------------------------------------------------------------

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    /// Git ignores it (or a folder above it).
    pub ignored: bool,
    /// A symbolic link or a junction.
    pub link: bool,
    /// A link that leads out of the folder: not followed.
    pub escapes: bool,
}

/// Whether a link is a folder's (Windows records it on the link; elsewhere unknown without
/// following it).
fn link_is_dir(kind: &std::fs::FileType) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::FileTypeExt::is_symlink_dir(kind)
    }
    #[cfg(not(windows))]
    {
        let _ = kind;
        false
    }
}

/// `name` as it is drawn: control characters and the bidirectional formatting ones (which can
/// make `gpj.exe` read as `exe.jpg`) become U+FFFD, one for one, so match positions still fit.
pub fn shown_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control()
                || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
            {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// A folder's entries, folders first then by name (case does not count), and how many more there
/// were than [`LIST_LIMIT`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub hidden: usize,
}

/// The entries of `rel` (a folder of `root`; empty for `root` itself). `inherit_ignored`: the
/// folder is itself ignored, so everything in it is.
pub fn list_dir(root: &Path, rel: &str, inherit_ignored: bool) -> Result<Listing, Refused> {
    let at = locate(root, rel)?;
    let read = std::fs::read_dir(&at.abs).map_err(|e| io_refusal(&e, rel))?;
    let ignores = if inherit_ignored {
        Ignores::default()
    } else {
        Ignores::for_dir(&at.root, &at.abs)
    };
    let mut entries: Vec<Entry> = Vec::new();
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let Ok(kind) = item.file_type() else { continue };
        let link = kind.is_symlink();
        let path = at.abs.join(&name);
        let (dir, escapes) = if link {
            // What it points at, and whether that is still inside the folder: read first, and
            // opened only when it stays inside (a link to another machine is never touched).
            if links_stay_inside(&[root, &at.root], &path, 0) {
                match std::fs::canonicalize(&path) {
                    Ok(target) => (target.is_dir(), !target.starts_with(&at.root)),
                    Err(_) => (false, false),
                }
            } else {
                (link_is_dir(&kind), true)
            }
        } else {
            (kind.is_dir(), false)
        };
        let ignored = inherit_ignored || ignores.ignored(&path, dir);
        entries.push(Entry {
            name,
            dir,
            ignored,
            link,
            escapes,
        });
    }
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    let hidden = entries.len().saturating_sub(LIST_LIMIT);
    entries.truncate(LIST_LIMIT);
    Ok(Listing { entries, hidden })
}

// --- reading a file -------------------------------------------------------------------------------

/// What a file is, once read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(TextFile),
    /// A picture the viewer decodes (off the UI thread).
    Image(Vec<u8>),
    /// Not text and not a picture: its size is all there is to say.
    Binary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFile {
    pub text: String,
    /// The bytes were not all valid UTF-8; the wrong ones are shown as U+FFFD.
    pub lossy: bool,
    /// The syntax's name for Kit's highlighter (the extension, or `text`).
    pub language: String,
    /// Small enough for syntax colours ([`HIGHLIGHT_MAX`]).
    pub highlight: bool,
    /// Markdown: the view can show it rendered.
    pub markdown: bool,
    /// An SVG: the view can show it as a picture.
    pub svg: bool,
    pub lines: usize,
    /// Lines longer than [`LINE_MAX`] were cut for the view.
    pub cut_lines: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRead {
    /// Relative to the folder, with `/`.
    pub rel: String,
    pub name: String,
    pub size: u64,
    pub content: Content,
}

/// `_fs_looks_binary` (`hermes_core::files`): a NUL, or over 12% control characters, near the start.
fn looks_binary(data: &[u8]) -> bool {
    let data = &data[..data.len().min(4096)];
    if data.is_empty() {
        return false;
    }
    if data.contains(&0) {
        return true;
    }
    let suspicious = data
        .iter()
        .filter(|b| **b < 32 && !matches!(b, 9 | 10 | 13))
        .count();
    suspicious as f64 / data.len() as f64 > 0.12
}

/// The extension in lower case, or the whole name for `Makefile` and its kind.
fn language_of(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_string(),
        // `.gitignore`, `Makefile`, `Dockerfile`: the name is the syntax's token.
        _ => lower.trim_start_matches('.').to_string(),
    }
}

/// Whether `name` is a picture a decoder reads.
pub fn is_image_name(name: &str) -> bool {
    matches!(
        language_of(name).as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico"
    )
}

/// The longest line the view shows whole, in bytes; a longer one is cut there and ends with `…`.
pub const LINE_MAX: usize = 10_000;

/// `text` with every line longer than [`LINE_MAX`] cut (at a character boundary), and whether any
/// was. Line numbers stay the same.
fn cut_long_lines(text: String) -> (String, bool) {
    if !text.split('\n').any(|line| line.len() > LINE_MAX) {
        return (text, false);
    }
    let mut out = String::with_capacity(text.len().min(64 * 1024 * 1024));
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        if line.len() > LINE_MAX {
            let mut end = LINE_MAX;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            out.push_str(&line[..end]);
            out.push('…');
        } else {
            out.push_str(line);
        }
    }
    (out, true)
}

/// What `bytes` of the file called `name` are.
pub fn classify(name: &str, bytes: Vec<u8>, size: u64) -> Content {
    if is_image_name(name) {
        return Content::Image(bytes);
    }
    let language = language_of(name);
    let highlight = size <= HIGHLIGHT_MAX;
    // A large markdown file or SVG is shown as its text: rendering it is the work the limit spares.
    let svg = highlight && language == "svg";
    if !svg && looks_binary(&bytes) {
        return Content::Binary;
    }
    let lossy = std::str::from_utf8(&bytes).is_err();
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    // A UTF-8 byte order mark is not a character of the text.
    if text.starts_with('\u{FEFF}') {
        text.drain(..'\u{FEFF}'.len_utf8());
    }
    let lines = text.bytes().filter(|b| *b == b'\n').count() + usize::from(!text.ends_with('\n'));
    // A minified file's one long line would be laid out whole on the UI thread (seconds for a few
    // megabytes): lines are cut for the view, which says so.
    // Not an SVG or markdown under the highlight limit: they are drawn from the whole text.
    let markdown = highlight && matches!(language.as_str(), "md" | "markdown" | "mdx");
    let (text, cut_lines) = if svg || markdown {
        (text, false)
    } else {
        cut_long_lines(text)
    };
    Content::Text(TextFile {
        cut_lines,
        markdown,
        svg,
        language: if highlight && !svg {
            language
        } else if highlight {
            "xml".to_string()
        } else {
            "text".to_string()
        },
        highlight,
        lossy,
        lines,
        text,
    })
}

/// Reads the file `asked` names (see [`locate`]), at most [`OPEN_MAX`] bytes of it.
pub fn read_file(root: &Path, asked: &str) -> Result<FileRead, Refused> {
    use std::io::Read as _;
    let at = locate(root, asked)?;
    // A folder cannot be opened as a file on Windows: say what it is first.
    let meta = std::fs::metadata(&at.abs).map_err(|e| io_refusal(&e, asked))?;
    if !meta.is_file() {
        return Err(Refused::NotAFile(at.rel));
    }
    if meta.len() > OPEN_MAX {
        return Err(Refused::TooLarge {
            path: at.rel,
            size: meta.len(),
        });
    }
    // Opened once; what was opened is checked on the handle (the path could have been swapped for
    // a link since `locate`), and the read is bounded (a file can grow while it is read).
    let file = std::fs::File::open(&at.abs).map_err(|e| io_refusal(&e, asked))?;
    opened_inside(&file, &at)?;
    let meta = file.metadata().map_err(|e| io_refusal(&e, asked))?;
    if !meta.is_file() {
        return Err(Refused::NotAFile(at.rel));
    }
    let mut bytes = Vec::with_capacity(meta.len().min(OPEN_MAX) as usize);
    (&file)
        .take(OPEN_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_refusal(&e, asked))?;
    let size = bytes.len() as u64;
    if size > OPEN_MAX {
        return Err(Refused::TooLarge { path: at.rel, size });
    }
    let name = at
        .abs
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(FileRead {
        content: classify(&name, bytes, size),
        rel: at.rel,
        name,
        size,
    })
}

/// Checks what a handle opened by `at` really is: a file on a disk (not a device or a pipe), whose
/// own path (Windows: `GetFinalPathNameByHandleW`, the path after every link) is still inside the
/// folder and not private. This closes the gap between [`locate`] and the open; the read is from
/// this handle, so a later swap does not change what is read.
fn opened_inside(file: &std::fs::File, at: &Located) -> Result<(), Refused> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FILE_NAME_NORMALIZED, FILE_TYPE_DISK, GetFileType, GetFinalPathNameByHandleW,
            VOLUME_NAME_DOS,
        };
        let handle = HANDLE(file.as_raw_handle());
        // SAFETY: `handle` is the open file's, alive for these calls.
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
            return Err(Refused::NotAFile(at.rel.clone()));
        }
        let mut buffer = vec![0u16; 1024];
        loop {
            // SAFETY: as above; the buffer is ours and its length is passed.
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    handle,
                    &mut buffer,
                    windows::Win32::Storage::FileSystem::GETFINALPATHNAMEBYHANDLE_FLAGS(
                        FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0,
                    ),
                )
            } as usize;
            if len == 0 {
                return Err(Refused::Unreadable(format!(
                    "{} could not be checked.",
                    at.rel
                )));
            }
            if len > buffer.len() {
                buffer.resize(len + 1, 0);
                continue;
            }
            let real = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
            if !real.starts_with(&at.root) {
                return Err(Refused::Escapes(at.rel.clone()));
            }
            if private(&real, &at.root) {
                return Err(Refused::Sensitive);
            }
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (file, at);
        Ok(())
    }
}

/// The file `asked` names, found and checked, for opening it elsewhere (the editor): the same
/// refusals as reading, but a binary or huge file is fine.
pub fn locate_file(root: &Path, asked: &str) -> Result<Located, Refused> {
    let at = locate(root, asked)?;
    match std::fs::metadata(&at.abs) {
        Ok(meta) if meta.is_file() => Ok(at),
        Ok(_) => Err(Refused::NotAFile(at.rel)),
        Err(e) => Err(io_refusal(&e, asked)),
    }
}

// --- the finder's index ---------------------------------------------------------------------------

/// How an index run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexSummary {
    pub files: usize,
    /// There were more than [`INDEX_LIMIT`].
    pub truncated: bool,
}

struct Walk<'a> {
    emit: &'a mut dyn FnMut(Vec<String>) -> bool,
    batch: Vec<String>,
    files: usize,
    limit: usize,
    stopped: bool,
    /// `emit` said it wants no more.
    refused: bool,
    truncated: bool,
    /// The canonical folder walked.
    root: PathBuf,
}

impl Walk<'_> {
    fn push(&mut self, path: String) {
        if self.files >= self.limit {
            self.truncated = true;
            self.stopped = true;
            return;
        }
        self.files += 1;
        self.batch.push(path);
        if self.batch.len() >= BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() && !(self.emit)(std::mem::take(&mut self.batch)) {
            self.stopped = true;
            self.refused = true;
        }
    }
}

fn walk(dir: &Path, prefix: &str, ignores: &mut Ignores, state: &mut Walk<'_>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let pushed = ignores.enter(dir);
    let mut folders: Vec<String> = Vec::new();
    for item in read.flatten() {
        if state.stopped {
            break;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(kind) = item.file_type() else { continue };
        // Links are in the tree, not in the finder: they could lead out or in circles.
        if kind.is_symlink() || name == ".git" {
            continue;
        }
        let is_dir = kind.is_dir();
        let path = dir.join(&name);
        if ignores.ignored(&path, is_dir) {
            continue;
        }
        if is_dir {
            folders.push(name);
        } else if !files::is_private(Path::new(&name)) && !odd_name(&name) {
            state.push(format!("{prefix}{name}"));
        }
    }
    for name in folders {
        if state.stopped {
            break;
        }
        // A folder named like a credentials folder (`pairing`, `mcp-tokens`, `.ssh`) or a
        // kept-out one (the app's data folder) is not entered.
        if files::is_private(Path::new(&name)) || kept_out(&dir.join(&name), &state.root) {
            continue;
        }
        walk(
            &dir.join(&name),
            &format!("{prefix}{name}/"),
            ignores,
            state,
        );
    }
    if pushed {
        ignores.leave();
    }
}

/// Every file of `root` that git does not ignore, as relative paths with `/`, handed to `emit` in
/// batches of [`BATCH`] as the walk goes (so a finder can answer before the walk ends); `emit`
/// returns `false` to stop. Links, `.git` and credential files are left out.
pub fn index_files(
    root: &Path,
    limit: usize,
    emit: &mut dyn FnMut(Vec<String>) -> bool,
) -> Result<IndexSummary, Refused> {
    let at = locate(root, "")?;
    let mut ignores = Ignores::for_dir(&at.root, &at.abs);
    // `for_dir` already holds `root`'s own `.gitignore`: `walk` enters it again, so start one
    // level up in the chain.
    ignores.leave_own(&at.abs);
    let mut state = Walk {
        emit,
        batch: Vec::new(),
        files: 0,
        limit,
        stopped: false,
        refused: false,
        truncated: false,
        root: at.root.clone(),
    };
    walk(&at.abs, "", &mut ignores, &mut state);
    if !state.refused {
        state.flush();
    }
    Ok(IndexSummary {
        files: state.files,
        truncated: state.truncated,
    })
}

impl Ignores {
    /// Takes `dir`'s own rules off the end of the chain when `for_dir` put them there (the walk
    /// enters a folder itself).
    fn leave_own(&mut self, dir: &Path) {
        if matcher(dir, &dir.join(".gitignore")).is_some() {
            self.chain.pop();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn names(listing: &Listing) -> Vec<(String, bool, bool)> {
        listing
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.dir, e.ignored))
            .collect()
    }

    /// A project: a repository with a `.gitignore`, a nested one with a negation, a build folder.
    pub(crate) fn project() -> hermes_testdir::TestDir {
        let dir = hermes_testdir::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git").join("info")).unwrap();
        write(root, ".git/HEAD", "ref: refs/heads/main\n");
        write(root, ".git/info/exclude", "scratch.txt\n");
        write(root, ".gitignore", "target/\n*.log\nnode_modules/\n");
        write(root, "Cargo.toml", "[package]\n");
        write(root, "README.md", "# Title\n");
        write(root, "scratch.txt", "not tracked\n");
        write(root, "debug.log", "noise\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "src/lib.rs", "pub fn x() {}\n");
        write(root, "src/gen/.gitignore", "*.tmp\n!keep.tmp\n");
        write(root, "src/gen/a.tmp", "x");
        write(root, "src/gen/keep.tmp", "y");
        write(root, "src/gen/b.rs", "z");
        write(root, "src/gen/deep/c.tmp", "w");
        write(root, "target/debug/out.bin", "\0\0");
        write(root, "node_modules/pkg/index.js", "1");
        write(root, ".env", "SECRET=1\n");
        dir
    }

    #[core::prelude::v1::test]
    fn a_listing_has_folders_first_and_marks_what_git_ignores() {
        let dir = project();
        let root = list_dir(dir.path(), "", false).unwrap();
        assert_eq!(
            names(&root),
            [
                ("node_modules".to_string(), true, true),
                ("src".to_string(), true, false),
                ("target".to_string(), true, true),
                (".env".to_string(), false, false),
                (".gitignore".to_string(), false, false),
                ("Cargo.toml".to_string(), false, false),
                ("debug.log".to_string(), false, true),
                ("README.md".to_string(), false, false),
                // `.git/info/exclude`
                ("scratch.txt".to_string(), false, true),
            ]
        );
        assert!(
            !root.entries.iter().any(|e| e.name == ".git"),
            ".git is never listed"
        );
        assert_eq!(root.hidden, 0);
    }

    #[core::prelude::v1::test]
    fn a_nested_gitignore_applies_to_its_folder_and_its_negation_wins() {
        let dir = project();
        let gen_dir = list_dir(dir.path(), "src/gen", false).unwrap();
        assert_eq!(
            names(&gen_dir),
            [
                ("deep".to_string(), true, false),
                (".gitignore".to_string(), false, false),
                ("a.tmp".to_string(), false, true),
                ("b.rs".to_string(), false, false),
                ("keep.tmp".to_string(), false, false),
            ]
        );
        // The nested rule reaches the folders below it.
        let deep = list_dir(dir.path(), "src/gen/deep", false).unwrap();
        assert_eq!(names(&deep), [("c.tmp".to_string(), false, true)]);
        // `*.log` from the top applies in a subfolder too.
        write(dir.path(), "src/trace.log", "");
        let src = list_dir(dir.path(), "src", false).unwrap();
        assert!(
            src.entries
                .iter()
                .any(|e| e.name == "trace.log" && e.ignored)
        );
    }

    #[core::prelude::v1::test]
    fn everything_in_an_ignored_folder_is_ignored_when_it_is_listed_on_demand() {
        let dir = project();
        let inside = list_dir(dir.path(), "target", true).unwrap();
        assert_eq!(names(&inside), [("debug".to_string(), true, true)]);
        // Without the flag nothing in it matches a pattern (the folder's own name did).
        let plain = list_dir(dir.path(), "target", false).unwrap();
        assert!(!plain.entries[0].ignored);
    }

    #[core::prelude::v1::test]
    fn a_folder_that_is_not_a_repository_still_reads_its_own_gitignore() {
        let dir = hermes_testdir::tempdir().unwrap();
        write(dir.path(), ".gitignore", "build/\n");
        write(dir.path(), "build/x.o", "");
        write(dir.path(), "a.c", "");
        let listing = list_dir(dir.path(), "", false).unwrap();
        assert_eq!(
            names(&listing),
            [
                ("build".to_string(), true, true),
                (".gitignore".to_string(), false, false),
                ("a.c".to_string(), false, false),
            ]
        );
    }

    #[core::prelude::v1::test]
    fn a_project_inside_a_repository_uses_the_repositorys_gitignore() {
        let dir = project();
        // The thread's folder is `src` of the repository: `*.log` still applies there.
        write(dir.path(), "src/noise.log", "");
        let listing = list_dir(&dir.path().join("src"), "", false).unwrap();
        assert!(
            listing
                .entries
                .iter()
                .any(|e| e.name == "noise.log" && e.ignored)
        );
        let mut seen = Vec::new();
        let summary = index_files(&dir.path().join("src"), INDEX_LIMIT, &mut |batch| {
            seen.extend(batch);
            true
        })
        .unwrap();
        assert!(!seen.iter().any(|p| p == "noise.log"));
        assert_eq!(summary.files, seen.len());
    }

    #[core::prelude::v1::test]
    fn a_long_listing_is_cut_and_says_how_many_are_left() {
        let dir = hermes_testdir::tempdir().unwrap();
        for n in 0..(LIST_LIMIT + 25) {
            std::fs::write(dir.path().join(format!("f{n:05}.txt")), "").unwrap();
        }
        let listing = list_dir(dir.path(), "", false).unwrap();
        assert_eq!(listing.entries.len(), LIST_LIMIT);
        assert_eq!(listing.hidden, 25);
        assert_eq!(listing.entries[0].name, "f00000.txt");
    }

    #[core::prelude::v1::test]
    fn the_index_leaves_out_ignored_folders_git_credentials_and_links() {
        let dir = project();
        let mut all = Vec::new();
        let summary = index_files(dir.path(), INDEX_LIMIT, &mut |batch| {
            all.extend(batch);
            true
        })
        .unwrap();
        all.sort();
        assert_eq!(
            all,
            [
                ".gitignore",
                "Cargo.toml",
                "README.md",
                "src/gen/.gitignore",
                "src/gen/b.rs",
                "src/gen/keep.tmp",
                "src/lib.rs",
                "src/main.rs",
            ],
            "ignored (target, node_modules, *.log, exclude, nested), .env and .git are left out"
        );
        assert_eq!(
            summary,
            IndexSummary {
                files: 8,
                truncated: false
            }
        );
    }

    #[core::prelude::v1::test]
    fn the_index_is_cut_at_its_limit_and_can_be_stopped() {
        let dir = hermes_testdir::tempdir().unwrap();
        for n in 0..50 {
            write(dir.path(), &format!("d{}/f{n}.txt", n % 5), "");
        }
        let mut count = 0;
        let summary = index_files(dir.path(), 20, &mut |batch| {
            count += batch.len();
            true
        })
        .unwrap();
        assert_eq!((count, summary.files, summary.truncated), (20, 20, true));
        // The receiver going away stops the walk at its first batch.
        let mut batches = 0;
        let summary = index_files(dir.path(), INDEX_LIMIT, &mut |_| {
            batches += 1;
            false
        })
        .unwrap();
        assert_eq!(batches, 1);
        assert!(summary.files <= 50);
    }

    // --- refusals ---

    #[core::prelude::v1::test]
    fn paths_outside_the_folder_and_dot_dot_are_refused_and_benign_ones_are_not() {
        let dir = project();
        let outer = hermes_testdir::tempdir().unwrap();
        write(outer.path(), "secret.txt", "x");
        let root = dir.path().join("src");
        // `..` out of the folder, an absolute path elsewhere, `~`.
        assert!(matches!(
            read_file(&root, "../README.md"),
            Err(Refused::Outside(_))
        ));
        assert!(matches!(
            read_file(&root, &outer.path().join("secret.txt").to_string_lossy()),
            Err(Refused::Outside(_))
        ));
        assert!(matches!(
            read_file(&root, "~/definitely-not-here.txt"),
            Err(Refused::Outside(_))
        ));
        assert!(matches!(
            list_dir(&root, "../src/../..", false),
            Err(Refused::Outside(_))
        ));
        // A `..` that comes back inside is fine, and so is the absolute path of a file inside.
        assert_eq!(read_file(&root, "gen/../main.rs").unwrap().rel, "main.rs");
        let abs = root.join("lib.rs");
        assert_eq!(
            read_file(&root, &abs.to_string_lossy()).unwrap().rel,
            "lib.rs"
        );
        assert_eq!(
            read_file(
                &root,
                &format!("file:///{}", abs.to_string_lossy().replace('\\', "/"))
            )
            .unwrap()
            .rel,
            "lib.rs"
        );
        // Not there.
        assert!(matches!(
            read_file(&root, "nope.rs"),
            Err(Refused::NotFound(_))
        ));
        // A folder is not a file.
        assert!(matches!(read_file(&root, "gen"), Err(Refused::NotAFile(_))));
    }

    #[core::prelude::v1::test]
    fn credential_files_are_refused_by_name_and_through_their_folder() {
        let dir = project();
        assert_eq!(read_file(dir.path(), ".env"), Err(Refused::Sensitive));
        write(dir.path(), "config/.env.local", "A=1");
        write(dir.path(), "mcp-tokens/token.json", "{}");
        write(dir.path(), "auth.json", "{}");
        for path in ["config/.env.local", "mcp-tokens/token.json", "auth.json"] {
            assert_eq!(
                read_file(dir.path(), path),
                Err(Refused::Sensitive),
                "{path}"
            );
        }
        assert_eq!(
            list_dir(dir.path(), "mcp-tokens", false),
            Err(Refused::Sensitive)
        );
        assert!(Refused::Sensitive.message().contains("credentials"));
        assert!(locate_file(dir.path(), ".env").is_err());
    }

    #[core::prelude::v1::test]
    fn keys_credential_folders_and_git_internals_are_refused_as_the_runtime_refuses_them() {
        let dir = project();
        write(dir.path(), ".git/config", "[remote] url = https://token@host/x\n");
        write(dir.path(), "deploy/id_rsa", "-----BEGIN");
        write(dir.path(), "certs/site.pem", "-----BEGIN");
        write(dir.path(), ".ssh/config", "Host x");
        write(dir.path(), ".npmrc", "//registry/:_authToken=x");
        for path in [
            ".git/config",
            ".GIT/HEAD",
            "deploy/id_rsa",
            "certs/site.pem",
            ".ssh/config",
            ".npmrc",
        ] {
            assert_eq!(read_file(dir.path(), path), Err(Refused::Sensitive), "{path}");
            assert!(locate_file(dir.path(), path).is_err(), "{path}");
        }
        assert_eq!(list_dir(dir.path(), ".ssh", false), Err(Refused::Sensitive));
        let mut all = Vec::new();
        index_files(dir.path(), INDEX_LIMIT, &mut |b| {
            all.extend(b);
            true
        })
        .unwrap();
        for hidden in ["deploy/id_rsa", "certs/site.pem", ".ssh/config", ".npmrc"] {
            assert!(!all.iter().any(|p| p == hidden), "{hidden} is indexed");
        }
    }

    #[core::prelude::v1::test]
    fn a_kept_out_folder_is_refused_inside_a_project_but_not_as_the_project_itself() {
        let dir = project();
        write(dir.path(), "appdata/hermes/config.json", "{\"key\": 1}");
        write(dir.path(), "appdata/hermes/notes.md", "x");
        keep_out(&dir.path().join("appdata").join("hermes"));
        assert_eq!(
            read_file(dir.path(), "appdata/hermes/notes.md"),
            Err(Refused::Sensitive)
        );
        assert_eq!(
            list_dir(dir.path(), "appdata/hermes", false),
            Err(Refused::Sensitive)
        );
        let mut all = Vec::new();
        index_files(dir.path(), INDEX_LIMIT, &mut |b| {
            all.extend(b);
            true
        })
        .unwrap();
        assert!(!all.iter().any(|p| p.starts_with("appdata/hermes")));
        // A thread whose folder is that folder still sees its own files.
        let inner = dir.path().join("appdata").join("hermes");
        assert_eq!(read_file(&inner, "notes.md").unwrap().rel, "notes.md");
    }

    #[core::prelude::v1::test]
    fn windows_path_forms_that_lead_elsewhere_are_refused_before_the_disk_is_asked() {
        let dir = project();
        let root = dir.path();
        for outside in [
            r"\\nohost.invalid\share\x.md",
            "file:////nohost.invalid/share/x.md",
            r"\\?\UNC\nohost.invalid\share\x.md",
            r"\\.\pipe\some-pipe",
            r"\\.\C:\Windows\win.ini",
            "C:Windows",
            "../x",
        ] {
            let started = std::time::Instant::now();
            assert!(
                matches!(read_file(root, outside), Err(Refused::Outside(_))),
                "{outside}: {:?}",
                read_file(root, outside)
            );
            // No network lookup happened (a share would take seconds to fail).
            assert!(started.elapsed() < std::time::Duration::from_secs(2), "{outside}");
        }
        // The folder's own files spelled another way are fine: the case, the verbatim prefix.
        let upper = root.join("SRC").join("MAIN.RS");
        assert_eq!(
            read_file(root, &upper.to_string_lossy()).unwrap().rel,
            "src/main.rs"
        );
        let verbatim = std::fs::canonicalize(root.join("Cargo.toml")).unwrap();
        assert_eq!(
            read_file(root, &verbatim.to_string_lossy()).unwrap().rel,
            "Cargo.toml"
        );
    }

    #[core::prelude::v1::test]
    fn names_windows_reads_as_streams_devices_or_other_files_are_refused() {
        let dir = project();
        for odd in [
            ".env::$DATA",
            "README.md:hidden",
            "CON",
            "nul.txt",
            "src/COM1",
            "lpt9.log",
            ".env.",
            ".env ",
            "README.md.",
        ] {
            let read = read_file(dir.path(), odd);
            assert!(
                matches!(read, Err(Refused::NotAFile(_)) | Err(Refused::Sensitive)),
                "{odd}: {read:?}"
            );
        }
        // Ordinary names that only look alike.
        assert!(!odd_name("console.rs") && !odd_name("com.rs") && !odd_name("auxiliary.md"));
        assert!(!odd_name("..") && !odd_name("a.b.c"));
    }

    #[core::prelude::v1::test]
    fn a_link_to_another_machine_is_listed_but_never_opened() {
        let dir = project();
        // A symbolic link needs Developer Mode or rights; skipped without them (a junction cannot
        // point at a share).
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(
            r"\\nohost.invalid\share",
            dir.path().join("remote"),
        )
        .is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink("//nohost.invalid/share", dir.path().join("remote"))
            .is_ok();
        if !made {
            eprintln!("skipped: this user cannot make a symbolic link");
            return;
        }
        let started = std::time::Instant::now();
        let listing = list_dir(dir.path(), "", false).unwrap();
        let remote = listing.entries.iter().find(|e| e.name == "remote").unwrap();
        assert!(remote.link && remote.escapes, "{remote:?}");
        assert!(matches!(
            read_file(dir.path(), "remote/x.md"),
            Err(Refused::Escapes(_))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[core::prelude::v1::test]
    fn names_with_control_or_direction_characters_are_shown_escaped_one_for_one() {
        let name = "invoice\u{202E}gpj.exe\u{7}";
        let shown = shown_name(name);
        assert_eq!(shown, "invoice\u{FFFD}gpj.exe\u{FFFD}");
        assert_eq!(shown.chars().count(), name.chars().count());
        assert_eq!(shown_name("main.rs"), "main.rs");
    }

    /// A directory link (a junction when symbolic links need rights this user lacks) from
    /// `link` to `target`. `false` when none can be made.
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(target, link).is_ok() {
                return true;
            }
            // A junction needs no rights: `mklink /J`.
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[core::prelude::v1::test]
    fn a_link_that_leads_out_is_listed_but_not_followed() {
        let dir = project();
        let outer = hermes_testdir::tempdir().unwrap();
        write(outer.path(), "secret.txt", "outside");
        if !make_dir_link(outer.path(), &dir.path().join("elsewhere")) {
            eprintln!("skipped: this user can make neither a symbolic link nor a junction");
            return;
        }
        let listing = list_dir(dir.path(), "", false).unwrap();
        let link = listing
            .entries
            .iter()
            .find(|e| e.name == "elsewhere")
            .unwrap();
        assert!(link.link && link.dir && link.escapes, "{link:?}");
        assert!(matches!(
            list_dir(dir.path(), "elsewhere", false),
            Err(Refused::Escapes(_))
        ));
        assert!(matches!(
            read_file(dir.path(), "elsewhere/secret.txt"),
            Err(Refused::Escapes(_))
        ));
        // The finder does not go through it either.
        let mut all = Vec::new();
        index_files(dir.path(), INDEX_LIMIT, &mut |b| {
            all.extend(b);
            true
        })
        .unwrap();
        assert!(!all.iter().any(|p| p.starts_with("elsewhere")));
        // A link that stays inside is followed.
        if make_dir_link(&dir.path().join("src"), &dir.path().join("alias")) {
            assert_eq!(
                read_file(dir.path(), "alias/main.rs").unwrap().rel,
                "src/main.rs"
            );
            let inside = list_dir(dir.path(), "", false).unwrap();
            let alias = inside.entries.iter().find(|e| e.name == "alias").unwrap();
            assert!(alias.link && !alias.escapes);
        }
    }

    // --- reading ---

    #[core::prelude::v1::test]
    fn a_text_file_is_read_with_its_language_and_line_count() {
        let dir = project();
        let read = read_file(dir.path(), "src/main.rs").unwrap();
        assert_eq!(
            (read.rel.as_str(), read.name.as_str(), read.size),
            ("src/main.rs", "main.rs", 13)
        );
        let Content::Text(text) = read.content else {
            panic!("text")
        };
        assert_eq!(text.language, "rs");
        assert!(text.highlight && !text.lossy && !text.markdown && !text.svg);
        assert_eq!((text.lines, text.text.as_str()), (1, "fn main() {}\n"));
        // No newline at the end still counts its last line; markdown and svg are flagged.
        assert_eq!(classify_text("a.md", b"a\nb").lines, 2);
        assert!(classify_text("README.md", b"# x").markdown);
        let svg = classify_text("logo.svg", b"<svg/>");
        assert!(svg.svg && svg.language == "xml");
        assert_eq!(classify_text("Makefile", b"all:").language, "makefile");
    }

    fn classify_text(name: &str, bytes: &[u8]) -> TextFile {
        match classify(name, bytes.to_vec(), bytes.len() as u64) {
            Content::Text(text) => text,
            other => panic!("not text: {other:?}"),
        }
    }

    #[core::prelude::v1::test]
    fn invalid_utf8_is_shown_lossy_and_binary_and_images_are_told_apart() {
        let lossy = classify_text("old.txt", &[b'c', b'a', b'f', 0xE9, b'\n']);
        assert!(lossy.lossy);
        assert_eq!(lossy.text, "caf\u{FFFD}\n");
        assert_eq!(classify("a.bin", vec![0, 1, 2, 0], 4), Content::Binary);
        assert_eq!(
            classify("a.exe", b"MZ\0\0\x03\0".to_vec(), 6),
            Content::Binary
        );
        assert!(matches!(
            classify("a.PNG", vec![1, 2, 3], 3),
            Content::Image(_)
        ));
        assert!(matches!(classify("a.webp", vec![], 0), Content::Image(_)));
        // An empty text file is text.
        assert_eq!(classify_text("empty.txt", b"").lines, 1);
        // A line over the limit is cut at a character boundary; the others and the count stay.
        let long = format!("short\nx{}\nend", "é".repeat(LINE_MAX));
        let cut = classify_text("min.js", long.as_bytes());
        assert!(cut.cut_lines && cut.lines == 3);
        let middle = cut.text.split('\n').nth(1).unwrap();
        assert!(middle.len() <= LINE_MAX + '…'.len_utf8() && middle.ends_with('…'));
        assert!(cut.text.starts_with("short\n") && cut.text.ends_with("\nend"));
        assert!(!classify_text("a.rs", b"fn a() {}").cut_lines);
        // An SVG keeps its long path data (the picture is drawn from it).
        let svg = format!("<svg><path d=\"{}\"/></svg>", "M0 0".repeat(LINE_MAX));
        assert!(!classify_text("p.svg", svg.as_bytes()).cut_lines);
        // A UTF-8 byte order mark is dropped.
        assert_eq!(classify_text("bom.txt", b"\xEF\xBB\xBFhi").text, "hi");
        // Over the highlight limit, markdown and SVG are shown as text, not rendered.
        let mut big = b"# t\n".to_vec();
        big.resize(HIGHLIGHT_MAX as usize + 1, b'a');
        let size = big.len() as u64;
        match classify("big.md", big.clone(), size) {
            Content::Text(t) => assert!(!t.markdown && t.language == "text"),
            other => panic!("{other:?}"),
        }
        match classify("big.svg", big, size) {
            Content::Text(t) => assert!(!t.svg && t.language == "text"),
            other => panic!("{other:?}"),
        }
    }

    #[core::prelude::v1::test]
    fn a_file_over_two_megabytes_is_plain_text_and_one_over_twenty_is_refused() {
        let dir = hermes_testdir::tempdir().unwrap();
        let big = vec![b'a'; (HIGHLIGHT_MAX + 10) as usize];
        std::fs::write(dir.path().join("big.rs"), &big).unwrap();
        let read = read_file(dir.path(), "big.rs").unwrap();
        let Content::Text(text) = read.content else {
            panic!("text")
        };
        assert!(!text.highlight);
        assert_eq!(text.language, "text");
        // A sparse file: no 20 MB of data written.
        let huge = std::fs::File::create(dir.path().join("huge.log")).unwrap();
        huge.set_len(OPEN_MAX + 1).unwrap();
        match read_file(dir.path(), "huge.log") {
            Err(refused @ Refused::TooLarge { .. }) => {
                assert!(
                    refused.message().contains("20.0 MB"),
                    "{}",
                    refused.message()
                );
                assert!(refused.can_open_in_editor());
                // Still findable for the editor.
                assert!(locate_file(dir.path(), "huge.log").is_ok());
            }
            other => panic!("{other:?}"),
        }
        assert!(!Refused::Sensitive.can_open_in_editor());
    }

    #[core::prelude::v1::test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(human_size(812), "812 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }

    // --- the big tree ---

    /// 100,000 empty files in 1,000 folders, made quickly.
    pub(crate) fn big_tree(files: usize, folders: usize) -> hermes_testdir::TestDir {
        let dir = hermes_testdir::tempdir().unwrap();
        for d in 0..folders {
            std::fs::create_dir_all(dir.path().join(format!("pkg{d:04}/src"))).unwrap();
        }
        for n in 0..files {
            let d = n % folders;
            let path = dir.path().join(format!("pkg{d:04}/src/file{n:06}.rs"));
            std::fs::File::create(path).unwrap();
        }
        dir
    }

    #[core::prelude::v1::test]
    fn indexing_a_hundred_thousand_files_streams_batches_and_takes_a_few_seconds_at_most() {
        let dir = big_tree(100_000, 1_000);
        let started = std::time::Instant::now();
        let mut first = None;
        let mut batches = 0;
        let mut total = 0;
        let summary = index_files(dir.path(), INDEX_LIMIT, &mut |batch| {
            first.get_or_insert_with(|| started.elapsed());
            batches += 1;
            total += batch.len();
            true
        })
        .unwrap();
        let took = started.elapsed();
        println!(
            "index of 100,000 files: first batch after {:?}, all after {took:?}, {batches} batches",
            first.unwrap()
        );
        assert_eq!(
            (summary.files, total, summary.truncated),
            (100_000, 100_000, false)
        );
        assert_eq!(batches, 100_000 / BATCH);
        // Generous: a debug build on a slow disk. Recorded in the progress note.
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
        // One folder with all of them lists in a bounded time too (cut at the limit).
        let started = std::time::Instant::now();
        let listing = list_dir(dir.path(), "", false).unwrap();
        assert_eq!(listing.entries.len(), 1_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
