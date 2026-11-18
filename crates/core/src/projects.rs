//! The sidebar's projects: the user's projects first (from the shared store, the GPUI app's too),
//! each owning the sessions whose folder lies in one of its folders; then one automatic project
//! per git repository the other sessions ran in; and a "Home" bucket, first, for the rest. A
//! repository shows as one lane (Hermes splits lanes by branch and worktree; hermes-rs records
//! neither per session). Typed over any session row ([`TreeRow`]); the Tauri page's
//! `projects.tree` JSON is built from it in `crates/desktop/src/projects.rs`.
//!
//! Hermes: `tui_gateway/project_tree.py` (`build_tree`, `_build_repos`, `_seed_folder_repos`,
//! `_project_node`, `_path_key`).

use std::collections::HashMap;
use std::path::Path;

use hermes_store::{Project, project_for_path};

/// Hermes's synthetic bucket for sessions no project claims.
pub const NO_PROJECT_ID: &str = "__no_project__";
/// Its label.
pub const NO_PROJECT_LABEL: &str = "Home";
/// The lane a repository's sessions sit in (`DEFAULT_BRANCH_LABEL`).
pub const LANE_LABEL: &str = "main";

/// What the tree needs of a session row.
pub trait TreeRow {
    /// The session's folder.
    fn cwd(&self) -> &str;
    /// When it was last active (seconds), if known.
    fn last_active(&self) -> Option<f64>;
}

/// A folder's last part (`C:\work\mine` → `mine`).
pub fn base_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// A path's identity for grouping (Hermes's `_path_key`): its segments, split on either slash,
/// case-folded, without a trailing slash. Windows spells one folder `C:\a\b`, `C:/a/b` or
/// `c:\a\b\` depending on who recorded it (the folder dialog, the page, a terminal); compared as
/// text they were different folders, and a project showed its folder twice.
pub fn path_key(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("/")
}

/// The git repository a folder is in (the nearest folder holding `.git`), if any.
pub fn git_root(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(|root| root.display().to_string())
}

/// Newest first, keeping the given order among equals.
fn newest_first<R: TreeRow>(rows: &mut [&R]) {
    rows.sort_by(|a, b| {
        b.last_active()
            .partial_cmp(&a.last_active())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// What a repository entry under a project is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoKind {
    /// A folder sessions ran in, with its one lane (`is_git`: a git repository's root).
    Lane { is_git: bool },
    /// A folder the project declares that no session ran in (an empty project still shows it).
    Folder,
    /// The Home bucket's one lane.
    Home,
}

/// A repository (or folder) under a project.
#[derive(Debug)]
pub struct Repo<'a, R> {
    /// Its folder; `None` for Home.
    pub root: Option<String>,
    pub label: String,
    pub kind: RepoKind,
    /// Its sessions, newest first.
    pub sessions: Vec<&'a R>,
}

/// What a project node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// One of the user's projects.
    Project,
    /// A repository sessions ran in that no project claims.
    Auto,
    /// The Home bucket.
    NoProject,
}

/// A project in the tree.
#[derive(Debug)]
pub struct Node<'a, R> {
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub kind: NodeKind,
    pub repos: Vec<Repo<'a, R>>,
    /// Its sessions, in the order the rows came.
    pub sessions: Vec<&'a R>,
}

impl<R: TreeRow> Node<'_, R> {
    /// The newest activity among its sessions (0 without any).
    pub fn last_active(&self) -> f64 {
        self.sessions
            .iter()
            .filter_map(|s| s.last_active())
            .fold(0.0, f64::max)
    }

    /// Its newest `limit` sessions.
    pub fn previews(&self, limit: usize) -> Vec<&R> {
        let mut previews = self.sessions.clone();
        newest_first(&mut previews);
        previews.truncate(limit);
        previews
    }
}

/// The tree, and every session it placed (in the order the nodes list them).
#[derive(Debug)]
pub struct Tree<'a, R> {
    pub nodes: Vec<Node<'a, R>>,
    pub scoped: Vec<&'a R>,
}

/// `repo → lane → sessions` for one project's sessions, plus a repo for each declared folder no
/// session covers.
fn repos<'a, R: TreeRow>(
    sessions: &[&'a R],
    folders: &[String],
    roots: &mut HashMap<String, Option<String>>,
) -> Vec<Repo<'a, R>> {
    let mut order: Vec<String> = Vec::new();
    let mut by_root: HashMap<String, (bool, Vec<&'a R>)> = HashMap::new();
    for session in sessions {
        let cwd = session.cwd().to_string();
        let git = roots
            .entry(cwd.clone())
            .or_insert_with(|| git_root(&cwd))
            .clone();
        let (root, is_git) = match git {
            Some(root) => (root, true),
            None => (cwd, false),
        };
        let key = path_key(&root);
        if !by_root.contains_key(&key) {
            order.push(root.clone());
        }
        by_root
            .entry(key)
            .or_insert((is_git, Vec::new()))
            .1
            .push(*session);
    }
    let mut out: Vec<Repo<'a, R>> = order
        .iter()
        .map(|root| {
            let (is_git, rows) = &by_root[&path_key(root)];
            let mut rows = rows.clone();
            newest_first(&mut rows);
            Repo {
                root: Some(root.clone()),
                label: base_name(root),
                kind: RepoKind::Lane { is_git: *is_git },
                sessions: rows,
            }
        })
        .collect();
    for folder in folders {
        let root =
            git_root(folder).unwrap_or_else(|| folder.trim_end_matches(['/', '\\']).to_string());
        if !out.iter().any(|repo| {
            repo.root
                .as_deref()
                .is_some_and(|id| path_key(id) == path_key(&root))
        }) {
            out.push(Repo {
                label: base_name(&root),
                root: Some(root),
                kind: RepoKind::Folder,
                sessions: Vec::new(),
            });
        }
    }
    out
}

/// The tree (`build_tree`) over the sessions' rows.
pub fn tree<'a, R: TreeRow>(projects: &[Project], sessions: &'a [R]) -> Tree<'a, R> {
    let mut roots: HashMap<String, Option<String>> = HashMap::new();
    let mut owned: HashMap<&str, Vec<&'a R>> = HashMap::new();
    let mut unowned: Vec<&'a R> = Vec::new();
    for session in sessions {
        match project_for_path(projects, session.cwd()) {
            Some(project) => owned.entry(project.id.as_str()).or_default().push(session),
            None => unowned.push(session),
        }
    }
    let mut nodes = Vec::new();
    let mut scoped: Vec<&'a R> = Vec::new();
    // The user's projects, shown even without sessions.
    for project in projects {
        let rows = owned.remove(project.id.as_str()).unwrap_or_default();
        scoped.extend(rows.iter().copied());
        nodes.push(Node {
            id: project.id.clone(),
            label: project.name.clone(),
            path: project.primary_folder().map(str::to_string),
            kind: NodeKind::Project,
            repos: repos(&rows, &project.folders, &mut roots),
            sessions: rows,
        });
    }
    // Automatic projects: the git repositories the other sessions ran in.
    let mut auto_order: Vec<String> = Vec::new();
    let mut auto: HashMap<String, Vec<&'a R>> = HashMap::new();
    let mut homeless: Vec<&'a R> = Vec::new();
    for session in unowned {
        let cwd = session.cwd().to_string();
        match roots
            .entry(cwd.clone())
            .or_insert_with(|| git_root(&cwd))
            .clone()
        {
            Some(root) => {
                // One project per folder however it's spelled; shown as first seen.
                let key = path_key(&root);
                if !auto.contains_key(&key) {
                    auto_order.push(root.clone());
                }
                auto.entry(key).or_default().push(session);
            }
            None => homeless.push(session),
        }
    }
    for root in auto_order {
        let rows = auto[&path_key(&root)].clone();
        scoped.extend(rows.iter().copied());
        nodes.push(Node {
            id: root.clone(),
            label: base_name(&root),
            path: Some(root),
            kind: NodeKind::Auto,
            repos: repos(&rows, &[], &mut roots),
            sessions: rows,
        });
    }
    // Home leads the list when anything is left for it.
    if !homeless.is_empty() {
        scoped.extend(homeless.iter().copied());
        let mut lane = homeless.clone();
        newest_first(&mut lane);
        nodes.insert(
            0,
            Node {
                id: NO_PROJECT_ID.to_string(),
                label: NO_PROJECT_LABEL.to_string(),
                path: None,
                kind: NodeKind::NoProject,
                repos: vec![Repo {
                    root: None,
                    label: NO_PROJECT_LABEL.to_string(),
                    kind: RepoKind::Home,
                    sessions: lane,
                }],
                sessions: homeless,
            },
        );
    }
    Tree { nodes, scoped }
}

/// A project's edit (`projects.update`, `projects.add_folder`): a new name, and a folder added
/// (first when it becomes the primary one; moved there if it was already listed).
#[derive(Debug, Default)]
pub struct ProjectEdit {
    pub name: Option<String>,
    pub folder: Option<String>,
    pub primary: bool,
}

impl crate::Core {
    /// The user's projects (the shared store's, the GPUI app's too). Blocking.
    pub fn projects(&self) -> Vec<Project> {
        self.runtime.store().list_projects().unwrap_or_default()
    }

    /// Makes a project of these folders, `primary` first. Blocking.
    pub fn create_project(
        &self,
        name: &str,
        mut folders: Vec<String>,
        primary: Option<String>,
    ) -> anyhow::Result<Project> {
        if let Some(primary) = primary.filter(|p| !p.is_empty()) {
            folders.retain(|f| f != &primary);
            folders.insert(0, primary);
        }
        self.runtime.store().create_project(name, &folders)
    }

    /// Edits a project; `Ok(None)` when there is no such project. Blocking.
    pub fn update_project(&self, id: &str, edit: ProjectEdit) -> anyhow::Result<Option<Project>> {
        let Some(mut project) = self.projects().into_iter().find(|p| p.id == id) else {
            return Ok(None);
        };
        if let Some(name) = edit.name.as_deref() {
            project.name = name.trim().to_string();
        }
        if let Some(path) = edit.folder {
            project.folders.retain(|f| f != &path);
            if edit.primary {
                project.folders.insert(0, path);
            } else {
                project.folders.push(path);
            }
        }
        self.runtime
            .store()
            .update_project(&project.id, &project.name, &project.folders)?;
        Ok(Some(project))
    }

    /// Removes a project (its sessions stay; they fall to their repository or Home). Blocking.
    pub fn delete_project(&self, id: &str) -> anyhow::Result<()> {
        self.runtime.store().delete_project(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Row(&'static str, String, f64);

    impl TreeRow for Row {
        fn cwd(&self) -> &str {
            &self.1
        }
        fn last_active(&self) -> Option<f64> {
            Some(self.2)
        }
    }

    fn ids<'a>(rows: &[&'a Row]) -> Vec<&'a str> {
        rows.iter().map(|r| r.0).collect()
    }

    fn mine(folder: &str) -> Project {
        Project {
            id: "prj_1".into(),
            name: "Mine".into(),
            folders: vec![folder.into()],
            created_at: 1,
        }
    }

    #[test]
    fn sessions_go_to_their_project_then_their_repository_then_home() {
        let repo = env!("CARGO_MANIFEST_DIR").to_string();
        let elsewhere = hermes_testdir::tempdir().unwrap();
        let rows = vec![
            Row("a", "C:/work/mine/src".into(), 3.0),
            Row("b", repo, 2.0),
            Row("c", elsewhere.path().display().to_string(), 1.0),
        ];
        let tree = tree(&[mine("C:/work/mine")], &rows);
        assert_eq!(tree.nodes[0].kind, NodeKind::NoProject);
        assert_eq!(ids(&tree.nodes[0].sessions), ["c"]);
        assert_eq!(tree.nodes[1].label, "Mine");
        assert_eq!(ids(&tree.nodes[1].sessions), ["a"]);
        // The declared folder shows as a repo, the session's folder under it doesn't add one.
        assert_eq!(tree.nodes[1].repos.len(), 2);
        assert_eq!(tree.nodes[2].kind, NodeKind::Auto);
        assert_eq!(ids(&tree.nodes[2].sessions), ["b"]);
        assert_eq!(tree.nodes[2].repos[0].kind, RepoKind::Lane { is_git: true });
        assert_eq!(ids(&tree.scoped), ["a", "b", "c"]);
    }

    #[test]
    fn one_folder_is_one_entry_however_it_is_spelled() {
        // A declared folder with backslashes; sessions recorded with slashes, other case, a
        // trailing separator: one repo with all of them, not one per spelling.
        let rows = vec![
            Row("a", "C:/work/mine".into(), 2.0),
            Row("b", r"c:\Work\Mine\".into(), 1.0),
        ];
        let built = tree(&[mine(r"C:\work\mine")], &rows);
        let node = &built.nodes[0];
        assert_eq!(node.label, "Mine");
        assert_eq!(node.repos.len(), 1, "{:?}", node.repos);
        assert_eq!(ids(&node.repos[0].sessions), ["a", "b"]);

        // An automatic project (this repository) reached with both kinds of slashes.
        let repo = env!("CARGO_MANIFEST_DIR");
        let rows = vec![
            Row("c", repo.replace('\\', "/"), 2.0),
            Row("d", repo.replace('/', r"\"), 1.0),
        ];
        let built = tree(&[], &rows);
        assert_eq!(built.nodes.len(), 1, "{:?}", built.nodes);
        assert_eq!(built.nodes[0].repos.len(), 1);
        assert_eq!(built.nodes[0].sessions.len(), 2);
    }

    #[test]
    fn previews_are_the_newest_and_the_node_knows_its_last_activity() {
        let dir = hermes_testdir::tempdir().unwrap();
        let cwd = dir.path().display().to_string();
        let rows = vec![
            Row("old", cwd.clone(), 1.0),
            Row("new", cwd.clone(), 9.0),
            Row("mid", cwd, 5.0),
        ];
        let built = tree(&[], &rows);
        let home = &built.nodes[0];
        assert_eq!(ids(&home.previews(2)), ["new", "mid"]);
        assert_eq!(home.last_active(), 9.0);
        assert_eq!(ids(&home.repos[0].sessions), ["new", "mid", "old"]);
    }
}
