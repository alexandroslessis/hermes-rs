//! A fake project for the Files tab, and what was asked of it: the fixture backend's answers for
//! the gallery, `hermes-native --fixture` and the tests. Two kinds: a **virtual** project (files
//! held in memory, nothing on disk: the stories and the recorded app) and a **disk** project (the
//! same calls answered by [`super::fs`] over a real folder: the tests that need real `.gitignore`
//! files, links and size limits). Both answer through the same [`super::fs`] types, so a view
//! cannot tell them from the real core's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::fs::{
    self, BATCH, Entry, FileRead, INDEX_LIMIT, IndexSummary, LIST_LIMIT, Listing, OPEN_MAX,
    Refused, classify,
};

/// Files in memory, with folders implied by their paths.
#[derive(Clone, Debug, Default)]
pub struct VirtualProject {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Paths (files or folders) git ignores.
    pub ignored: BTreeSet<String>,
    /// Paths that are links leading out of the project.
    pub escaping: BTreeSet<String>,
    /// Sizes to report for files whose content is not kept (a "huge" file).
    pub claimed: BTreeMap<String, u64>,
}

impl VirtualProject {
    fn is_dir(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        path.is_empty() || self.files.keys().any(|k| k.starts_with(&prefix))
    }

    fn is_ignored(&self, path: &str) -> bool {
        // The path or a folder above it.
        let mut at = path;
        loop {
            if self.ignored.contains(at) {
                return true;
            }
            match at.rsplit_once('/') {
                Some((up, _)) => at = up,
                None => return false,
            }
        }
    }

    /// A path as the tree names it, or why it is refused.
    fn normalize(&self, asked: &str) -> Result<String, Refused> {
        let asked = asked.trim().replace('\\', "/");
        let looks_absolute = asked.starts_with('/')
            || asked.starts_with('~')
            || asked.chars().nth(1) == Some(':')
            || asked.starts_with("file:");
        if looks_absolute {
            return Err(Refused::Outside(asked));
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in asked.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(Refused::Outside(asked.clone()));
                    }
                }
                other => parts.push(other),
            }
        }
        let rel = parts.join("/");
        if hermes_core::files::is_private(std::path::Path::new(&rel)) {
            return Err(Refused::Sensitive);
        }
        // A link on the way that leads out.
        let mut at = String::new();
        for part in &parts {
            at = if at.is_empty() {
                part.to_string()
            } else {
                format!("{at}/{part}")
            };
            if self.escaping.contains(&at) {
                return Err(Refused::Escapes(asked));
            }
        }
        Ok(rel)
    }

    pub fn list(&self, dir: &str, inherit_ignored: bool) -> Result<Listing, Refused> {
        let dir = self.normalize(dir)?;
        if !self.is_dir(&dir) {
            return Err(Refused::NotFound(dir));
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut seen: BTreeMap<String, bool> = BTreeMap::new();
        for key in self.files.keys().chain(self.escaping.iter()) {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((name, _)) => {
                    seen.insert(name.to_string(), true);
                }
                None => {
                    seen.entry(rest.to_string()).or_insert(false);
                }
            }
        }
        let mut entries: Vec<Entry> = seen
            .into_iter()
            .map(|(name, dir_flag)| {
                let path = format!("{prefix}{name}");
                let escapes = self.escaping.contains(&path);
                Entry {
                    ignored: inherit_ignored || self.is_ignored(&path),
                    dir: dir_flag || (escapes && !self.files.contains_key(&path)),
                    link: escapes,
                    escapes,
                    name,
                }
            })
            .collect();
        entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
        let hidden = entries.len().saturating_sub(LIST_LIMIT);
        entries.truncate(LIST_LIMIT);
        Ok(Listing { entries, hidden })
    }

    pub fn read(&self, asked: &str) -> Result<FileRead, Refused> {
        let rel = self.normalize(asked)?;
        if self.is_dir(&rel) {
            return Err(Refused::NotAFile(rel));
        }
        let size = self
            .claimed
            .get(&rel)
            .copied()
            .or_else(|| self.files.get(&rel).map(|b| b.len() as u64))
            .ok_or_else(|| Refused::NotFound(rel.clone()))?;
        if size > OPEN_MAX {
            return Err(Refused::TooLarge { path: rel, size });
        }
        let bytes = self.files.get(&rel).cloned().unwrap_or_default();
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        Ok(FileRead {
            content: classify(&name, bytes, size),
            rel,
            name,
            size,
        })
    }

    pub fn index(&self, emit: &mut dyn FnMut(Vec<String>) -> bool) -> IndexSummary {
        let all: Vec<String> = self
            .files
            .keys()
            .filter(|k| {
                !self.is_ignored(k)
                    && !hermes_core::files::is_private(std::path::Path::new(k.as_str()))
            })
            .take(INDEX_LIMIT)
            .cloned()
            .collect();
        let files = all.len();
        for chunk in all.chunks(BATCH) {
            if !emit(chunk.to_vec()) {
                break;
            }
        }
        IndexSummary {
            files,
            truncated: false,
        }
    }
}

/// Where a fixture backend's project comes from.
#[derive(Clone, Debug)]
pub enum Source {
    Virtual(VirtualProject),
    /// A real folder, answered by [`super::fs`].
    Disk(PathBuf),
}

/// The fixture project's state and log.
#[derive(Debug)]
pub struct FixtureFiles {
    pub source: Source,
    /// Every folder listed: `(session, folder, inherit_ignored)`.
    pub listed: Vec<(String, String, bool)>,
    /// Every file read: `(session, path as asked)`.
    pub read: Vec<(String, String)>,
    /// How many times the finder asked for the index.
    pub indexed: usize,
    /// Every open in the editor: `(session, path, line)`.
    pub opened: Vec<(String, String, Option<u32>)>,
    /// The next listing / read fails with this text.
    pub failure: Option<Refused>,
}

impl Default for FixtureFiles {
    fn default() -> Self {
        Self::sample()
    }
}

impl FixtureFiles {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            listed: Vec::new(),
            read: Vec::new(),
            indexed: 0,
            opened: Vec::new(),
            failure: None,
        }
    }

    /// The project the gallery and the recorded app show.
    pub fn sample() -> Self {
        Self::new(Source::Virtual(sample_project()))
    }

    pub fn disk(root: PathBuf) -> Self {
        Self::new(Source::Disk(root))
    }

    pub fn list(&mut self, session: &str, dir: &str, inherit: bool) -> Result<Listing, Refused> {
        self.listed
            .push((session.to_string(), dir.to_string(), inherit));
        if let Some(refused) = self.failure.take() {
            return Err(refused);
        }
        match &self.source {
            Source::Virtual(project) => project.list(dir, inherit),
            Source::Disk(root) => fs::list_dir(root, dir, inherit),
        }
    }

    pub fn read(&mut self, session: &str, path: &str) -> Result<FileRead, Refused> {
        self.read.push((session.to_string(), path.to_string()));
        if let Some(refused) = self.failure.take() {
            return Err(refused);
        }
        match &self.source {
            Source::Virtual(project) => project.read(path),
            Source::Disk(root) => fs::read_file(root, path),
        }
    }

    pub fn index(
        &mut self,
        emit: &mut dyn FnMut(Vec<String>) -> bool,
    ) -> Result<IndexSummary, Refused> {
        self.indexed += 1;
        match &self.source {
            Source::Virtual(project) => Ok(project.index(emit)),
            Source::Disk(root) => fs::index_files(root, INDEX_LIMIT, emit),
        }
    }

    /// Checks `path` as the editor open would and logs it.
    pub fn open(&mut self, session: &str, path: &str, line: Option<u32>) -> Result<String, String> {
        let checked = match &self.source {
            Source::Virtual(project) => project.normalize(path).map(|_| ()),
            Source::Disk(root) => fs::locate_file(root, path).map(|_| ()),
        };
        checked.map_err(|r| r.message())?;
        self.opened
            .push((session.to_string(), path.to_string(), line));
        Ok("the fixture editor".to_string())
    }
}

/// A line of Rust, numbered, to make a file of any length.
fn rust_lines(count: usize) -> String {
    let mut out = String::new();
    for n in 1..=count {
        out.push_str(&format!(
            "pub fn item_{n}(value: u32) -> u32 {{ value.wrapping_mul({n}) + 1 }}\n"
        ));
    }
    out
}

const WORKSPACE_RS: &str = r#"//! The window's frame: the sidebar, the main column and the right panel, and the handles between.

use gpui_kit::{Pixels, Size};

/// The narrowest the main column gets before a panel closes.
pub const MAIN_MIN_WIDTH: f32 = 280.;

/// Where the three panes sit for a window of a given size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameLayout {
    pub sidebar: f32,
    pub main: f32,
    pub right: f32,
}

/// The width of each pane, read from the saved sizes and clamped to what fits.
pub fn frame_layout(window: Size<Pixels>, state: &UiState) -> FrameLayout {
    let w = window.width.as_f32();
    let sidebar = state.sidebar.size.clamp(MIN, MAX).min(w - MAIN_MIN_WIDTH);
    let right = if state.right_panel.open {
        state.right_panel.size.clamp(MIN, MAX)
    } else {
        0.
    };
    let main = (w - sidebar - right).max(MAIN_MIN_WIDTH);
    FrameLayout { sidebar, main, right }
}

pub struct UiState {
    pub sidebar: PaneState,
    pub right_panel: PaneState,
}

pub struct PaneState {
    pub open: bool,
    pub size: f32,
}

const MIN: f32 = 208.;
const MAX: f32 = 560.;
"#;

const README_MD: &str = "# halo\n\nA small project to try the **Files** tab on.\n\n## Layout\n\n- [`src/main.rs`](src/main.rs#L3) starts it\n- [the guide](docs/guide/intro.md) explains it\n- [the logo](assets/logo.png)\n- [the site](https://example.com/halo)\n\n> [!TIP]\n> Press Ctrl+P to find a file by name.\n\n| File | What |\n|---|---|\n| `src/lib.rs` | the library |\n| `src/util/mod.rs` | helpers |\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n1. Build it.\n2. Run it.\n";

const INTRO_MD: &str = "# Introduction\n\nRead this first, then the [readme](../../README.md).\n\nThe code is in [`main.rs`](../../src/main.rs).\n";

/// The project of the gallery and `--fixture`.
pub fn sample_project() -> VirtualProject {
    let mut project = VirtualProject::default();
    let mut put = |path: &str, text: &str| {
        project
            .files
            .insert(path.to_string(), text.as_bytes().to_vec());
    };
    put(".gitignore", "target/\nnode_modules/\n*.log\n");
    put(
        "Cargo.toml",
        "[package]\nname = \"halo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nserde = \"1\"\ntokio = { version = \"1\", features = [\"rt\"] }\nanyhow = \"1\"\n",
    );
    put("README.md", README_MD);
    put("docs/guide/intro.md", INTRO_MD);
    put("docs/notes.txt", "Plain notes.\nA second line.\n");
    put(
        "src/main.rs",
        "use std::io;\n\nfn main() {\n    let name = std::env::args().nth(1).unwrap_or_default();\n    println!(\"hello, {name}\");\n    run();\n}\n\nfn run() {\n    let mut line = String::new();\n    let _ = io::stdin().read_line(&mut line);\n}\n",
    );
    put("src/lib.rs", &rust_lines(40));
    put("src/generated.rs", &rust_lines(3000));
    put(
        "src/util/mod.rs",
        "pub mod strings;\n\npub fn clamp(value: i32, low: i32, high: i32) -> i32 {\n    value.max(low).min(high)\n}\n",
    );
    put(
        "src/util/strings.rs",
        "pub fn shout(text: &str) -> String {\n    text.to_uppercase()\n}\n",
    );
    put("crates/native/src/workspace.rs", WORKSPACE_RS);
    put(
        "tools/greet.py",
        "import sys\n\n\ndef greet(name: str) -> str:\n    return f\"hello, {name}\"\n\n\nprint(greet(sys.argv[1]))\n",
    );
    put(
        "data/long.json",
        &format!(
            "{{\n  \"text\": \"{}\",\n  \"end\": true\n}}\n",
            "word ".repeat(80)
        ),
    );
    put(
        "assets/logo.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"96\" height=\"96\" viewBox=\"0 0 96 96\"><circle cx=\"48\" cy=\"48\" r=\"40\" fill=\"#3b82f6\"/><circle cx=\"48\" cy=\"48\" r=\"18\" fill=\"#ffffff\"/></svg>\n",
    );
    project.files.insert(
        "assets/logo.png".to_string(),
        crate::thread::composer::attach::sample_png(160, 100, 160),
    );
    project.files.insert(
        "assets/data.bin".to_string(),
        (0..=255u8).cycle().take(2048).collect(),
    );
    // Windows-1252 text: `cafÃ©` with a single byte for the accent.
    project.files.insert(
        "docs/old.txt".to_string(),
        b"caf\xE9 au lait\nna\xEFve\n".to_vec(),
    );
    project
        .files
        .insert(".env".to_string(), b"TOKEN=secret\n".to_vec());
    project
        .files
        .insert("target/debug/halo.exe".to_string(), vec![0; 64]);
    project.files.insert(
        "node_modules/left-pad/index.js".to_string(),
        b"module.exports = 1;\n".to_vec(),
    );
    project
        .files
        .insert("debug.log".to_string(), b"noise\n".to_vec());
    project.ignored.extend(
        ["target", "node_modules", "debug.log"]
            .into_iter()
            .map(str::to_string),
    );
    // A file with no content kept that is over the size limit.
    project
        .files
        .insert("data/huge.sql".to_string(), Vec::new());
    project
        .claimed
        .insert("data/huge.sql".to_string(), 34 * 1024 * 1024);
    // A link that leads out of the project.
    project.escaping.insert("outside".to_string());
    project
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::fs::Content;

    #[core::prelude::v1::test]
    fn the_virtual_project_lists_reads_and_refuses_like_the_real_one() {
        let project = sample_project();
        let root = project.list("", false).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "assets",
                "crates",
                "data",
                "docs",
                "node_modules",
                "outside",
                "src",
                "target",
                "tools",
                ".env",
                ".gitignore",
                "Cargo.toml",
                "debug.log",
                "README.md"
            ]
        );
        assert!(
            root.entries
                .iter()
                .find(|e| e.name == "target")
                .unwrap()
                .ignored
        );
        assert!(project.list("target/debug", true).unwrap().entries[0].ignored);
        assert!(matches!(
            project.list("outside", false),
            Err(Refused::Escapes(_))
        ));
        assert!(matches!(project.read("../x"), Err(Refused::Outside(_))));
        assert!(matches!(
            project.read("C:/Windows/win.ini"),
            Err(Refused::Outside(_))
        ));
        assert_eq!(project.read(".env"), Err(Refused::Sensitive));
        assert!(matches!(
            project.read("data/huge.sql"),
            Err(Refused::TooLarge { .. })
        ));
        assert!(matches!(project.read("src"), Err(Refused::NotAFile(_))));
        assert!(matches!(
            project.read("src/nope.rs"),
            Err(Refused::NotFound(_))
        ));
        assert!(matches!(
            project.read("assets/data.bin").unwrap().content,
            Content::Binary
        ));
        assert!(matches!(
            project.read("assets/logo.png").unwrap().content,
            Content::Image(_)
        ));
        assert_eq!(
            project.read("src/./util/../main.rs").unwrap().rel,
            "src/main.rs"
        );
        let mut all = Vec::new();
        let summary = project.index(&mut |b| {
            all.extend(b);
            true
        });
        assert_eq!(summary.files, all.len());
        assert!(all.contains(&"src/main.rs".to_string()));
        assert!(
            !all.iter()
                .any(|p| p.starts_with("target") || p == ".env" || p == "debug.log")
        );
    }
}
