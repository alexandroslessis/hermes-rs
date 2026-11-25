//! The Files tab's tree as data, with no views in it: which folders are open, what each has
//! listed so far, the flat list of visible rows, the selection and the keys that move it. A
//! folder's entries are asked for when it is opened ([`Tree::toggle`] says whether a listing is
//! needed) and arrive with [`Tree::apply`]; nothing below a closed folder is read.
//!
//! t3code: `apps/web/src/components/files/FileBrowserPanel.tsx` (the tree, rows revealed for a
//! file, expand and collapse all), `fileTreeExpansion.ts`, `loadFileAncestors.ts` (opening the
//! folders above a file so it can be shown), `useDirectoryEntries.ts` (folders loaded on demand).
//! Reference: `sidebar-filetree.png`.

use std::collections::{HashMap, HashSet};

use super::fs::{Entry, Listing};

/// What a folder has listed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Load {
    /// Asked for, not answered.
    Loading,
    Loaded(Listing),
    Failed(String),
}

/// One row of the visible list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Relative to the project, with `/`.
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub kind: RowKind,
    pub ignored: bool,
    /// A link that leads out of the project (not followed).
    pub escapes: bool,
    pub link: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    File,
    Dir {
        open: bool,
    },
    /// "Loading...", an error, or "N more not shown", under a folder; never selected.
    Note {
        error: bool,
    },
}

impl Row {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, RowKind::Dir { .. })
    }

    pub fn is_note(&self) -> bool {
        matches!(self.kind, RowKind::Note { .. })
    }
}

/// A key's meaning for the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    First,
    Last,
    /// Left: close the folder, or go to the folder above.
    Collapse,
    /// Right: open the folder, or go to its first entry.
    Expand,
}

#[derive(Debug, Default)]
pub struct Tree {
    /// Listings by folder (`""` is the project).
    loads: HashMap<String, Load>,
    open: HashSet<String>,
    selected: Option<String>,
    rows: Vec<Row>,
}

/// `a/b/c` -> `a/b` (`""` for a name at the top).
pub fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The row of the selection, if it is visible.
    pub fn selected_row(&self) -> Option<usize> {
        let path = self.selected.as_deref()?;
        self.rows
            .iter()
            .position(|r| !r.is_note() && r.path == path)
    }

    pub fn row_of(&self, path: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| !r.is_note() && r.path == path)
    }

    pub fn is_open(&self, dir: &str) -> bool {
        self.open.contains(dir)
    }

    /// Whether the project's own listing has arrived.
    pub fn root_loaded(&self) -> bool {
        matches!(self.loads.get(""), Some(Load::Loaded(_)))
    }

    /// The root listing was refused or failed: why.
    pub fn root_failure(&self) -> Option<&str> {
        match self.loads.get("") {
            Some(Load::Failed(why)) => Some(why),
            _ => None,
        }
    }

    /// Whether anything has been asked for (a first listing is in flight or done).
    pub fn started(&self) -> bool {
        self.loads.contains_key("")
    }

    /// The entry a path names, when its folder is loaded.
    pub fn entry(&self, path: &str) -> Option<&Entry> {
        let Some(Load::Loaded(listing)) = self.loads.get(parent_of(path)) else {
            return None;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        listing.entries.iter().find(|e| e.name == name)
    }

    /// Whether `dir` (a folder path) is ignored: itself or a folder above it was listed as such.
    pub fn is_ignored(&self, dir: &str) -> bool {
        !dir.is_empty() && self.entry(dir).is_some_and(|e| e.ignored)
    }

    /// Marks `dir` as asked for. `false` when it is already loaded or in flight.
    pub fn begin(&mut self, dir: &str) -> bool {
        match self.loads.get(dir) {
            Some(Load::Loading | Load::Loaded(_)) => false,
            _ => {
                self.loads.insert(dir.to_string(), Load::Loading);
                self.rebuild();
                true
            }
        }
    }

    /// A listing (or its failure) arrived for `dir`.
    pub fn apply(&mut self, dir: &str, answer: Result<Listing, String>) {
        let load = match answer {
            Ok(listing) => Load::Loaded(listing),
            Err(why) => Load::Failed(why),
        };
        self.loads.insert(dir.to_string(), load);
        self.rebuild();
    }

    /// Opens or closes a folder row. `Some(dir)` when its entries must be asked for.
    pub fn toggle(&mut self, dir: &str) -> Option<String> {
        if self.open.contains(dir) {
            self.open.remove(dir);
            self.rebuild();
            None
        } else {
            self.open_dir(dir)
        }
    }

    /// Opens a folder; `Some(dir)` when its entries must be asked for.
    pub fn open_dir(&mut self, dir: &str) -> Option<String> {
        if dir.is_empty() {
            return None;
        }
        // A folder that links out is not followed (its listing would be refused).
        if self.entry(dir).is_some_and(|e| e.escapes) {
            return None;
        }
        self.open.insert(dir.to_string());
        let ask = self.begin_load(dir);
        self.rebuild();
        ask
    }

    fn begin_load(&mut self, dir: &str) -> Option<String> {
        match self.loads.get(dir) {
            Some(Load::Loading | Load::Loaded(_)) => None,
            _ => {
                self.loads.insert(dir.to_string(), Load::Loading);
                Some(dir.to_string())
            }
        }
    }

    pub fn close_dir(&mut self, dir: &str) {
        if self.open.remove(dir) {
            self.rebuild();
        }
    }

    pub fn collapse_all(&mut self) {
        if self.selected.as_deref().is_some_and(|s| !s.is_empty()) {
            // The selection stays on its top folder when it was inside one that closes.
            if let Some(sel) = self.selected.clone() {
                let top = sel.split('/').next().unwrap_or(&sel).to_string();
                if top != sel {
                    self.selected = Some(top);
                }
            }
        }
        self.open.clear();
        self.rebuild();
    }

    pub fn any_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Opens every folder above `path` and selects it; the folders that need a listing.
    pub fn reveal(&mut self, path: &str) -> Vec<String> {
        let mut ask = Vec::new();
        let mut dir = String::new();
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop();
        for part in parts {
            dir = join(&dir, part);
            self.open.insert(dir.clone());
            if let Some(asked) = self.begin_load(&dir) {
                ask.push(asked);
            }
        }
        self.selected = Some(path.to_string());
        self.rebuild();
        ask
    }

    pub fn select(&mut self, path: &str) {
        self.selected = Some(path.to_string());
    }

    /// Forgets what was listed, keeping what is open and selected; the folders to list again
    /// (the project and every open one).
    pub fn invalidate(&mut self) -> Vec<String> {
        let mut dirs: Vec<String> = vec![String::new()];
        let mut open: Vec<String> = self.open.iter().cloned().collect();
        open.sort();
        dirs.extend(open);
        // Rows stay as they are until the new answers arrive (a refresh must not blank the list).
        dirs
    }

    /// Everything forgotten (another thread).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Moves the selection as a key would. Returns the folder to ask a listing of when the key
    /// opened one.
    pub fn step(&mut self, step: Step) -> Option<String> {
        let selectable: Vec<usize> = (0..self.rows.len())
            .filter(|&i| !self.rows[i].is_note())
            .collect();
        if selectable.is_empty() {
            return None;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|s| selectable.iter().position(|&i| self.rows[i].path == s));
        let target = |ix: usize| Some(self.rows[selectable[ix]].path.clone());
        let next = match step {
            Step::First => target(0),
            Step::Last => target(selectable.len() - 1),
            Step::Down => match current {
                None => target(0),
                Some(c) => target((c + 1).min(selectable.len() - 1)),
            },
            Step::Up => match current {
                None => target(selectable.len() - 1),
                Some(c) => target(c.saturating_sub(1)),
            },
            Step::Collapse | Step::Expand => {
                let Some(c) = current else {
                    self.selected = target(0);
                    return None;
                };
                let row = self.rows[selectable[c]].clone();
                if step == Step::Collapse {
                    if row.is_dir() && self.open.contains(&row.path) {
                        self.close_dir(&row.path);
                        return None;
                    }
                    // To the folder above.
                    let parent = parent_of(&row.path);
                    (!parent.is_empty()).then(|| parent.to_string())
                } else if let RowKind::Dir { open } = row.kind {
                    if !open {
                        return self.open_dir(&row.path);
                    }
                    // Open: down to its first entry, if it has one.
                    selectable
                        .get(c + 1)
                        .filter(|&&i| parent_of(&self.rows[i].path) == row.path)
                        .and_then(|&i| Some(self.rows[i].path.clone()))
                } else {
                    None
                }
            }
        };
        if let Some(path) = next {
            self.selected = Some(path);
        }
        None
    }

    // --- the visible rows ---

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        self.push_dir("", 0, &mut rows);
        self.rows = rows;
    }

    fn push_dir(&self, dir: &str, depth: usize, rows: &mut Vec<Row>) {
        let note = |text: String, error: bool| Row {
            path: join(dir, &format!("\u{0}{text}")),
            name: text,
            depth,
            kind: RowKind::Note { error },
            ignored: false,
            escapes: false,
            link: false,
        };
        match self.loads.get(dir) {
            None | Some(Load::Loading) => {
                if !dir.is_empty() {
                    rows.push(note("Loading...".to_string(), false));
                }
            }
            Some(Load::Failed(why)) => rows.push(note(why.clone(), true)),
            Some(Load::Loaded(listing)) => {
                for entry in &listing.entries {
                    let path = join(dir, &entry.name);
                    let open = entry.dir && self.open.contains(&path);
                    rows.push(Row {
                        path: path.clone(),
                        name: entry.name.clone(),
                        depth,
                        kind: if entry.dir {
                            RowKind::Dir { open }
                        } else {
                            RowKind::File
                        },
                        ignored: entry.ignored,
                        escapes: entry.escapes,
                        link: entry.link,
                    });
                    if open {
                        self.push_dir(&path, depth + 1, rows);
                    }
                }
                if listing.hidden > 0 {
                    rows.push(note(
                        format!("{} more entries are not shown", listing.hidden),
                        false,
                    ));
                }
                if listing.entries.is_empty() && !dir.is_empty() {
                    rows.push(note("Empty".to_string(), false));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dir: bool) -> Entry {
        Entry {
            name: name.to_string(),
            dir,
            ignored: false,
            link: false,
            escapes: false,
        }
    }

    fn listing(entries: &[(&str, bool)]) -> Listing {
        Listing {
            entries: entries.iter().map(|(n, d)| entry(n, *d)).collect(),
            hidden: 0,
        }
    }

    fn paths(tree: &Tree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    /// `src/` (`gen/`, `main.rs`), `docs/`, `README.md`.
    fn sample() -> Tree {
        let mut tree = Tree::new();
        assert!(tree.begin(""));
        tree.apply(
            "",
            Ok(listing(&[
                ("docs", true),
                ("src", true),
                ("README.md", false),
            ])),
        );
        tree
    }

    #[core::prelude::v1::test]
    fn a_folder_is_asked_for_when_it_opens_and_not_again_once_it_is_listed() {
        let mut tree = sample();
        assert_eq!(paths(&tree), ["docs", "src", "README.md"]);
        assert_eq!(tree.toggle("src"), Some("src".to_string()));
        // While it is loading, a note says so and nothing is asked twice.
        assert_eq!(paths(&tree), ["docs", "src", "  Loading...", "README.md"]);
        assert_eq!(tree.toggle("src"), None, "that closed it");
        assert_eq!(tree.toggle("src"), None, "open again: already in flight");
        tree.apply("src", Ok(listing(&[("gen", true), ("main.rs", false)])));
        assert_eq!(
            paths(&tree),
            ["docs", "src", "  gen", "  main.rs", "README.md"]
        );
        // Closing and opening again does not ask again either.
        assert_eq!(tree.toggle("src"), None);
        assert_eq!(tree.toggle("src"), None);
        assert_eq!(paths(&tree).len(), 5);
        // A failed listing is a row with the reason; opening a failed folder asks again.
        tree.apply("docs", Err("access denied".to_string()));
        assert_eq!(tree.toggle("docs"), Some("docs".to_string()));
        tree.apply("docs", Err("access denied".to_string()));
        assert!(
            tree.rows()
                .iter()
                .any(|r| r.name == "access denied" && r.is_note())
        );
    }

    #[core::prelude::v1::test]
    fn the_keys_walk_the_rows_open_and_close_folders_and_skip_notes() {
        let mut tree = sample();
        tree.toggle("src");
        tree.apply("src", Ok(listing(&[("gen", true), ("main.rs", false)])));
        assert_eq!(tree.selected(), None);
        tree.step(Step::Down);
        assert_eq!(tree.selected(), Some("docs"));
        tree.step(Step::Down);
        tree.step(Step::Down);
        assert_eq!(tree.selected(), Some("src/gen"));
        tree.step(Step::Up);
        assert_eq!(tree.selected(), Some("src"));
        // Left on an open folder closes it; left again goes nowhere (top level).
        tree.step(Step::Collapse);
        assert_eq!(paths(&tree), ["docs", "src", "README.md"]);
        tree.step(Step::Collapse);
        assert_eq!(tree.selected(), Some("src"));
        // Right opens (and asks), right again goes into it.
        assert_eq!(tree.step(Step::Expand), None, "already listed");
        assert_eq!(tree.selected(), Some("src"));
        tree.step(Step::Expand);
        assert_eq!(tree.selected(), Some("src/gen"));
        // Left on an entry goes up to its folder.
        tree.step(Step::Collapse);
        assert_eq!(tree.selected(), Some("src"));
        tree.step(Step::Last);
        assert_eq!(tree.selected(), Some("README.md"));
        tree.step(Step::Down);
        assert_eq!(
            tree.selected(),
            Some("README.md"),
            "the end stays at the end"
        );
        tree.step(Step::First);
        assert_eq!(tree.selected(), Some("docs"));
        // Right on an unopened, unlisted folder asks for it.
        assert_eq!(tree.step(Step::Expand), Some("docs".to_string()));
        // The loading note is never selected.
        tree.step(Step::Down);
        assert_eq!(tree.selected(), Some("src"));
    }

    #[core::prelude::v1::test]
    fn revealing_a_file_opens_every_folder_above_it_and_asks_for_the_unlisted_ones() {
        let mut tree = sample();
        let ask = tree.reveal("src/gen/deep/x.rs");
        assert_eq!(ask, ["src", "src/gen", "src/gen/deep"]);
        assert_eq!(tree.selected(), Some("src/gen/deep/x.rs"));
        tree.apply("src", Ok(listing(&[("gen", true)])));
        tree.apply("src/gen", Ok(listing(&[("deep", true)])));
        tree.apply(
            "src/gen/deep",
            Ok(listing(&[("x.rs", false), ("y.rs", false)])),
        );
        assert_eq!(
            paths(&tree),
            [
                "docs",
                "src",
                "  gen",
                "    deep",
                "      x.rs",
                "      y.rs",
                "README.md"
            ]
        );
        assert_eq!(tree.selected_row(), Some(4));
        // A second reveal of a file in a folder already open asks for nothing new.
        assert!(tree.reveal("src/gen/deep/y.rs").is_empty());
        // Collapse all keeps the selection on the top folder it was inside.
        tree.collapse_all();
        assert_eq!(paths(&tree), ["docs", "src", "README.md"]);
        assert_eq!(tree.selected(), Some("src"));
        assert!(!tree.any_open());
    }

    #[core::prelude::v1::test]
    fn a_refresh_keeps_the_rows_until_the_new_listing_lands() {
        let mut tree = sample();
        tree.toggle("src");
        tree.apply("src", Ok(listing(&[("main.rs", false)])));
        assert_eq!(tree.invalidate(), ["", "src"]);
        assert_eq!(paths(&tree).len(), 4, "nothing blanked");
        tree.apply("src", Ok(listing(&[("main.rs", false), ("new.rs", false)])));
        assert_eq!(paths(&tree).len(), 5);
        // An entry deleted since is gone from the rows, and its selection with it.
        tree.select("src/main.rs");
        tree.apply("src", Ok(listing(&[("new.rs", false)])));
        assert_eq!(tree.selected_row(), None);
    }

    #[core::prelude::v1::test]
    fn a_link_that_leaves_the_project_does_not_open_and_a_cut_listing_says_so() {
        let mut tree = Tree::new();
        tree.begin("");
        let mut out = entry("elsewhere", true);
        out.link = true;
        out.escapes = true;
        let mut cut = listing(&[("a.rs", false)]);
        cut.entries.insert(0, out);
        cut.hidden = 41;
        tree.apply("", Ok(cut));
        assert_eq!(tree.toggle("elsewhere"), None);
        assert!(!tree.is_open("elsewhere"), "never opened");
        assert_eq!(
            tree.rows().last().map(|r| r.name.as_str()),
            Some("41 more entries are not shown")
        );
        let mut ignored = entry("target", true);
        ignored.ignored = true;
        let mut tree = Tree::new();
        tree.begin("");
        tree.apply(
            "",
            Ok(Listing {
                entries: vec![ignored],
                hidden: 0,
            }),
        );
        assert!(tree.is_ignored("target"));
        assert!(!tree.is_ignored("other"));
    }
}
