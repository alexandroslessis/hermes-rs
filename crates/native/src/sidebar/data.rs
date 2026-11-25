//! The sidebar's data, with no GPUI in it: what the core says about projects and threads
//! ([`Snapshot`]), the edits the user can make ([`Op`], applied to the snapshot at once so the
//! list answers instantly and to the core in the background), and the list the sidebar draws from
//! them ([`layout`]): pinned threads first, then the projects with their threads newest first, the
//! "No project" group last, the filter applied, folders collapsed or capped.
//!
//! Everything the view and the keyboard navigation need to decide is a plain function here, so it
//! is tested without a window: the sort, the grouping, the filter, which rows exist and in which
//! order (the up/down arrows walk [`Tree::keys`]), the relative time and a thread's status.
//!
//! t3code: `apps/web/src/components/Sidebar.logic.ts` (`resolveSidebarThreadStatus`,
//! `sortThreads`, the folder dot `resolveSidebarFolderDot`), `timestampFormat.ts`
//! (`formatRelativeTime`: "now", "5m", "3h", "2d"), `Sidebar.tsx` (the folder cap, "Show more").

use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The id of the group for threads no project claims (the core's bucket, shown last here).
pub const NO_PROJECT: &str = hermes_core::projects::NO_PROJECT_ID;
/// What that group is called.
pub const NO_PROJECT_LABEL: &str = "No project";
/// How many threads a folder shows before "Show more" (t3code's folder preview).
pub const FOLDER_LIMIT: usize = 8;
/// What a thread with no title is called.
pub const UNTITLED: &str = "New thread";

/// What a project is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    /// One the user added (can be renamed, recoloured and removed).
    User,
    /// A git repository sessions ran in that no project claims (listed like a project, not edited).
    Auto,
    /// The "No project" group.
    None,
}

/// A group in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectItem {
    /// The store's project id, the repository folder for an automatic one, [`NO_PROJECT`].
    pub id: String,
    pub name: String,
    /// Its folder (shown when the pointer rests on the header); `None` for "No project".
    pub path: Option<String>,
    pub kind: ProjectKind,
}

/// A thread (a stored session) as the sidebar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadItem {
    pub id: String,
    pub title: String,
    /// The id of the [`ProjectItem`] it is listed under.
    pub project: String,
    pub cwd: String,
    pub pinned: bool,
    /// Last activity, milliseconds since the epoch.
    pub updated_ms: i64,
    /// A turn (or the prompts queued behind it) is in flight.
    pub working: bool,
    /// A question (an approval, a clarify) waits for the user.
    pub waiting: bool,
    /// What waits is a question of the agent, not an approval (`waiting` is also set): the row says
    /// "Input" (t3code) instead of "Approval".
    pub asking: bool,
    pub branch: Option<String>,
}

impl ThreadItem {
    /// The title to show, on one line: a stored title may hold newlines or tabs (a title the
    /// model wrote, an import), which a 36 px row cannot draw.
    pub fn display_title(&self) -> std::borrow::Cow<'_, str> {
        let title = self.title.trim();
        if title.is_empty() {
            UNTITLED.into()
        } else if title.chars().any(char::is_control) {
            title
                .split(char::is_control)
                .filter(|part| !part.trim().is_empty())
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
                .into()
        } else {
            title.into()
        }
    }
}

/// Projects and threads as the core listed them (archived threads are not in it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub projects: Vec<ProjectItem>,
    pub threads: Vec<ThreadItem>,
}

impl Snapshot {
    pub fn project(&self, id: &str) -> Option<&ProjectItem> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn thread(&self, id: &str) -> Option<&ThreadItem> {
        self.threads.iter().find(|t| t.id == id)
    }

    /// Whether the user has added no project and there is no thread either: the first run.
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty() && !self.projects.iter().any(|p| p.kind == ProjectKind::User)
    }

    /// The thread with the latest activity.
    pub fn latest_thread(&self) -> Option<&ThreadItem> {
        self.threads.iter().max_by_key(|t| t.updated_ms)
    }

    /// Applies an edit to the list at once (the core's answer, and a refresh, follow). `Move`
    /// needs the target's folder, found in the list; an unknown id changes nothing.
    pub fn apply(&mut self, op: &Op) {
        match op {
            Op::Rename { id, title } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *id) {
                    thread.title = title.trim().to_string();
                }
            }
            Op::Pin { id, pinned } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *id) {
                    thread.pinned = *pinned;
                }
            }
            Op::Archive { id } | Op::Delete { id } => self.threads.retain(|t| t.id != *id),
            Op::Move { id, project } => {
                let path = self.project(project).and_then(|p| p.path.clone());
                if let (Some(thread), Some(path)) =
                    (self.threads.iter_mut().find(|t| t.id == *id), path)
                {
                    thread.project = project.clone();
                    thread.cwd = path;
                }
            }
            Op::AddProject { folder, name } => {
                let id = format!("new:{folder}");
                if !self
                    .projects
                    .iter()
                    .any(|p| p.path.as_deref() == Some(folder))
                {
                    // Before the automatic ones and the "No project" group.
                    let at = self
                        .projects
                        .iter()
                        .rposition(|p| p.kind == ProjectKind::User)
                        .map_or(0, |ix| ix + 1);
                    self.projects.insert(
                        at,
                        ProjectItem {
                            id,
                            name: name.clone(),
                            path: Some(folder.clone()),
                            kind: ProjectKind::User,
                        },
                    );
                }
            }
            Op::RenameProject { id, name } => {
                if let Some(project) = self.projects.iter_mut().find(|p| p.id == *id) {
                    project.name = name.trim().to_string();
                }
            }
            Op::RemoveProject { id } => {
                // Its threads fall to the "No project" group until the core's list says better.
                self.projects.retain(|p| p.id != *id);
                for thread in self.threads.iter_mut().filter(|t| t.project == *id) {
                    thread.project = NO_PROJECT.to_string();
                }
            }
        }
        self.ensure_no_project_group();
    }

    /// A "No project" group exists exactly when a thread is in it.
    fn ensure_no_project_group(&mut self) {
        let needed = self.threads.iter().any(|t| t.project == NO_PROJECT);
        let has = self.projects.iter().any(|p| p.kind == ProjectKind::None);
        if needed && !has {
            self.projects.push(ProjectItem {
                id: NO_PROJECT.to_string(),
                name: NO_PROJECT_LABEL.to_string(),
                path: None,
                kind: ProjectKind::None,
            });
        } else if !needed && has {
            self.projects.retain(|p| p.kind != ProjectKind::None);
        }
    }
}

/// An edit the user made: the one vocabulary of the model, the backends and the tests.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Rename {
        id: String,
        title: String,
    },
    Pin {
        id: String,
        pinned: bool,
    },
    Archive {
        id: String,
    },
    Delete {
        id: String,
    },
    /// Moves a thread to a project (its folder becomes the thread's).
    Move {
        id: String,
        project: String,
    },
    AddProject {
        folder: String,
        name: String,
    },
    RenameProject {
        id: String,
        name: String,
    },
    RemoveProject {
        id: String,
    },
}

/// What the model knows about threads beyond the core's list.
#[derive(Clone, Copy, Debug, Default)]
pub struct Marks<'a> {
    /// Finished while the user looked elsewhere.
    pub unread: Option<&'a HashSet<String>>,
    /// The last turn failed.
    pub failed: Option<&'a HashSet<String>>,
}

impl Marks<'_> {
    fn has(set: Option<&HashSet<String>>, id: &str) -> bool {
        set.is_some_and(|s| s.contains(id))
    }
}

/// A thread's state in the right slot of its row (t3code's `topStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus {
    /// A flagged command or edit waits for the user's approval.
    Approval,
    /// The agent asked a question and waits for the answer.
    Input,
    /// The agent is working.
    Working,
    /// The last turn failed.
    Failed,
    /// Finished and not looked at yet (t3code shows "Done").
    Unread,
}

/// The status of a thread, most urgent first: a waiting question, a running turn, a failure, an
/// unread completion; `None` for a thread at rest.
pub fn status_of(thread: &ThreadItem, marks: &Marks<'_>) -> Option<RowStatus> {
    if thread.waiting {
        Some(if thread.asking {
            RowStatus::Input
        } else {
            RowStatus::Approval
        })
    } else if thread.working {
        Some(RowStatus::Working)
    } else if Marks::has(marks.failed, &thread.id) {
        Some(RowStatus::Failed)
    } else if Marks::has(marks.unread, &thread.id) {
        Some(RowStatus::Unread)
    } else {
        None
    }
}

/// The dot a closed folder shows for the threads it hides (t3code's `resolveSidebarFolderDot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderDot {
    /// Something waits for the user.
    Attention,
    Failed,
    Unread,
    Working,
}

fn folder_dot(threads: &[&ThreadItem], marks: &Marks<'_>) -> Option<FolderDot> {
    let statuses: Vec<RowStatus> = threads.iter().filter_map(|t| status_of(t, marks)).collect();
    let has = |s| statuses.contains(&s);
    if has(RowStatus::Approval) || has(RowStatus::Input) {
        Some(FolderDot::Attention)
    } else if has(RowStatus::Failed) {
        Some(FolderDot::Failed)
    } else if has(RowStatus::Unread) {
        Some(FolderDot::Unread)
    } else if has(RowStatus::Working) {
        Some(FolderDot::Working)
    } else {
        None
    }
}

/// A thread's age as t3code's rows say it: `now`, `5m`, `3h`, `2d`.
pub fn relative_time(now_ms: i64, then_ms: i64) -> String {
    let seconds = (now_ms - then_ms).max(0) / 1000;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if seconds < 60 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if hours < 24 {
        format!("{hours}h")
    } else {
        format!("{days}d")
    }
}

/// A row the keyboard can reach.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A project's header.
    Folder(String),
    /// A thread, pinned or not (the same thread is never listed twice).
    Thread(String),
    /// The "Show more" row of a folder.
    More(String),
}

/// A project and the threads of it that show.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub project: String,
    /// Closed: only the header (and its count and dot) shows.
    pub collapsed: bool,
    /// The threads that show, newest first (the unpinned ones).
    pub threads: Vec<String>,
    /// How many more "Show more" would list.
    pub hidden: usize,
    /// All its unpinned threads (the count a closed folder shows).
    pub total: usize,
    pub dot: Option<FolderDot>,
}

/// The sidebar's list for one state of the filter and the folders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    /// Pinned threads, newest first.
    pub pinned: Vec<String>,
    pub groups: Vec<Group>,
    /// How many threads match the filter (all of them without one).
    pub matches: usize,
}

impl Tree {
    /// The rows in the order they are drawn, for the arrow keys.
    pub fn keys(&self) -> Vec<RowKey> {
        let mut keys: Vec<RowKey> = self.pinned.iter().cloned().map(RowKey::Thread).collect();
        for group in &self.groups {
            keys.push(RowKey::Folder(group.project.clone()));
            if !group.collapsed {
                keys.extend(group.threads.iter().cloned().map(RowKey::Thread));
                if group.hidden > 0 {
                    keys.push(RowKey::More(group.project.clone()));
                }
            }
        }
        keys
    }
}

/// What decides the list besides the data.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// The filter box's text: every word must be in the title or the project's name.
    pub filter: &'a str,
    /// Folders the user closed (ignored while filtering: a match must not hide).
    pub collapsed: Option<&'a BTreeSet<String>>,
    /// Folders the user asked "Show more" for.
    pub expanded: Option<&'a HashSet<String>>,
    pub marks: Marks<'a>,
}

fn matches(filter: &[String], thread: &ThreadItem, project: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let haystack = format!("{} {}", thread.display_title(), project).to_lowercase();
    filter.iter().all(|word| haystack.contains(word))
}

/// Builds the list: pinned threads, then the user's projects, the automatic ones, and "No project"
/// last; each kind ordered by its newest thread (projects with none keep the store's order after
/// those with some); threads newest first.
pub fn layout(snapshot: &Snapshot, options: &Options<'_>) -> Tree {
    let words: Vec<String> = options
        .filter
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let filtering = !words.is_empty();
    let project_name = |id: &str| snapshot.project(id).map_or("", |p| p.name.as_str());

    let mut shown: Vec<&ThreadItem> = snapshot
        .threads
        .iter()
        .filter(|t| matches(&words, t, project_name(&t.project)))
        .collect();
    shown.sort_by(|a, b| {
        b.updated_ms
            .cmp(&a.updated_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    let matched = shown.len();

    let pinned: Vec<String> = shown
        .iter()
        .filter(|t| t.pinned)
        .map(|t| t.id.clone())
        .collect();

    let mut keyed: Vec<(u8, i64, usize, &ProjectItem)> = snapshot
        .projects
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let latest = snapshot
                .threads
                .iter()
                .filter(|t| t.project == p.id)
                .map(|t| t.updated_ms)
                .max()
                .unwrap_or(i64::MIN);
            let rank = match p.kind {
                ProjectKind::User => 0,
                ProjectKind::Auto => 1,
                ProjectKind::None => 2,
            };
            (rank, latest, ix, p)
        })
        .collect();
    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut groups = Vec::new();
    for (_, _, _, project) in keyed {
        let mine: Vec<&ThreadItem> = shown
            .iter()
            .copied()
            .filter(|t| t.project == project.id && !t.pinned)
            .collect();
        // While filtering, a project with no match is not listed (an empty one included).
        if filtering && mine.is_empty() {
            continue;
        }
        let collapsed = !filtering
            && options
                .collapsed
                .is_some_and(|set| set.contains(&project.id));
        let all_shown = filtering
            || options
                .expanded
                .is_some_and(|set| set.contains(&project.id));
        let take = if all_shown { mine.len() } else { FOLDER_LIMIT };
        let dot = collapsed
            .then(|| folder_dot(&mine, &options.marks))
            .flatten();
        groups.push(Group {
            project: project.id.clone(),
            collapsed,
            hidden: mine.len().saturating_sub(take),
            total: mine.len(),
            threads: mine.iter().take(take).map(|t| t.id.clone()).collect(),
            dot,
        });
    }
    Tree {
        pinned,
        groups,
        matches: matched,
    }
}

/// The keys the arrow keys move between and the row after or before `current` (wrapping stops at
/// the ends). With no current row, the first (down) or the last (up).
pub fn step(keys: &[RowKey], current: Option<&RowKey>, down: bool) -> Option<RowKey> {
    if keys.is_empty() {
        return None;
    }
    let at = current.and_then(|c| keys.iter().position(|k| k == c));
    let next = match (at, down) {
        (None, true) => 0,
        (None, false) => keys.len() - 1,
        (Some(ix), true) => (ix + 1).min(keys.len() - 1),
        (Some(ix), false) => ix.saturating_sub(1),
    };
    Some(keys[next].clone())
}

/// The remembered per-project colours and closed folders (stored in `native-ui.json`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Project id -> colour name ([`crate::sidebar::color::ProjectColor::name`]).
    pub colors: BTreeMap<String, String>,
    /// Ids of the folders the user closed.
    pub collapsed: BTreeSet<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn thread(id: &str, title: &str, project: &str, updated: i64) -> ThreadItem {
        ThreadItem {
            id: id.into(),
            title: title.into(),
            project: project.into(),
            cwd: String::new(),
            pinned: false,
            updated_ms: updated,
            working: false,
            waiting: false,
            asking: false,
            branch: None,
        }
    }

    pub(crate) fn project(id: &str, name: &str, kind: ProjectKind) -> ProjectItem {
        ProjectItem {
            id: id.into(),
            name: name.into(),
            path: (kind != ProjectKind::None).then(|| format!(r"C:\work\{name}")),
            kind,
        }
    }

    pub(crate) fn sample() -> Snapshot {
        Snapshot {
            projects: vec![
                project("p1", "engine", ProjectKind::User),
                project("p2", "structure", ProjectKind::User),
                project("p3", "empty", ProjectKind::User),
                project(r"C:\repos\auto", "auto", ProjectKind::Auto),
                project(NO_PROJECT, NO_PROJECT_LABEL, ProjectKind::None),
            ],
            threads: vec![
                thread("a", "Fix the parser", "p1", 100),
                thread("b", "Port the palette", "p1", 300),
                thread("c", "halo", "p2", 500),
                thread("d", "Scratch", NO_PROJECT, 900),
                thread("e", "Auto thread", r"C:\repos\auto", 50),
            ],
        }
    }

    fn ids(tree: &Tree) -> Vec<String> {
        tree.keys()
            .into_iter()
            .map(|k| match k {
                RowKey::Folder(p) => format!("[{p}]"),
                RowKey::Thread(t) => t,
                RowKey::More(p) => format!("+{p}"),
            })
            .collect()
    }

    #[core::prelude::v1::test]
    fn threads_are_newest_first_projects_by_their_newest_thread_and_no_project_is_last() {
        let tree = layout(&sample(), &Options::default());
        // structure (500) before engine (300); the empty project after both; the repo; then No project.
        assert_eq!(
            ids(&tree),
            [
                "[p2]",
                "c",
                "[p1]",
                "b",
                "a",
                "[p3]",
                r"[C:\repos\auto]",
                "e",
                "[__no_project__]",
                "d"
            ]
        );
    }

    #[core::prelude::v1::test]
    fn pinned_threads_come_first_and_are_not_listed_twice() {
        let mut snapshot = sample();
        snapshot.threads[0].pinned = true; // a (100)
        snapshot.threads[3].pinned = true; // d (900, No project)
        let tree = layout(&snapshot, &Options::default());
        assert_eq!(tree.pinned, ["d", "a"], "newest pinned first");
        let keys = ids(&tree);
        assert_eq!(&keys[..2], ["d", "a"]);
        assert_eq!(keys.iter().filter(|k| *k == "a").count(), 1);
        // The group that lost all its threads to the pins is still listed (and says so).
        let none = tree
            .groups
            .iter()
            .find(|g| g.project == NO_PROJECT)
            .unwrap();
        assert!(none.threads.is_empty());
    }

    #[core::prelude::v1::test]
    fn the_filter_matches_title_or_project_words_in_any_order_and_drops_empty_groups() {
        let snapshot = sample();
        let titles = |filter: &str| {
            let tree = layout(
                &snapshot,
                &Options {
                    filter,
                    ..Options::default()
                },
            );
            ids(&tree)
        };
        assert_eq!(titles("PARSER"), ["[p1]", "a"]);
        assert_eq!(titles("palette port"), ["[p1]", "b"]);
        // The project's name matches all of its threads.
        assert_eq!(titles("structure"), ["[p2]", "c"]);
        assert_eq!(titles("zzz"), Vec::<String>::new());
        // Empty / blank: everything.
        assert_eq!(
            titles("  ").len(),
            layout(&snapshot, &Options::default()).keys().len()
        );
    }

    #[core::prelude::v1::test]
    fn a_closed_folder_hides_its_threads_unless_filtering_and_shows_its_dot() {
        let mut snapshot = sample();
        snapshot.threads[1].waiting = true; // b, in p1
        let collapsed: BTreeSet<String> = ["p1".to_string()].into();
        let options = Options {
            collapsed: Some(&collapsed),
            ..Options::default()
        };
        let tree = layout(&snapshot, &options);
        let group = tree.groups.iter().find(|g| g.project == "p1").unwrap();
        assert!(group.collapsed);
        assert_eq!(group.total, 2);
        assert_eq!(group.dot, Some(FolderDot::Attention));
        assert!(!ids(&tree).contains(&"b".to_string()));
        // A match inside a closed folder must show.
        let tree = layout(
            &snapshot,
            &Options {
                filter: "palette",
                ..options
            },
        );
        assert!(ids(&tree).contains(&"b".to_string()));
    }

    #[core::prelude::v1::test]
    fn a_folder_shows_eight_threads_then_show_more() {
        let mut snapshot = Snapshot {
            projects: vec![project("p", "big", ProjectKind::User)],
            threads: (0..12)
                .map(|i| thread(&format!("t{i}"), "x", "p", i64::from(i)))
                .collect(),
        };
        let tree = layout(&snapshot, &Options::default());
        let group = &tree.groups[0];
        assert_eq!((group.threads.len(), group.hidden, group.total), (8, 4, 12));
        assert_eq!(group.threads[0], "t11", "the newest first");
        assert_eq!(tree.keys().last(), Some(&RowKey::More("p".into())));
        let expanded: HashSet<String> = ["p".to_string()].into();
        let tree = layout(
            &snapshot,
            &Options {
                expanded: Some(&expanded),
                ..Options::default()
            },
        );
        assert_eq!(
            (tree.groups[0].threads.len(), tree.groups[0].hidden),
            (12, 0)
        );
        // Filtering lists every match.
        snapshot.threads[0].title = "needle".into();
        let tree = layout(
            &snapshot,
            &Options {
                filter: "needle",
                ..Options::default()
            },
        );
        assert_eq!(tree.matches, 1);
    }

    #[core::prelude::v1::test]
    fn status_is_the_most_urgent_thing_and_none_at_rest() {
        let unread: HashSet<String> = ["u".into(), "w".into()].into();
        let failed: HashSet<String> = ["f".into(), "w".into()].into();
        let marks = Marks {
            unread: Some(&unread),
            failed: Some(&failed),
        };
        let mut t = thread("x", "x", "p", 0);
        assert_eq!(status_of(&t, &marks), None);
        t.id = "u".into();
        assert_eq!(status_of(&t, &marks), Some(RowStatus::Unread));
        t.id = "f".into();
        assert_eq!(status_of(&t, &marks), Some(RowStatus::Failed));
        t.working = true;
        assert_eq!(
            status_of(&t, &marks),
            Some(RowStatus::Working),
            "working beats failed"
        );
        t.waiting = true;
        assert_eq!(
            status_of(&t, &marks),
            Some(RowStatus::Approval),
            "waiting beats all"
        );
    }

    #[core::prelude::v1::test]
    fn relative_time_is_t3codes_compact_form() {
        let now = 10_000_000_000;
        let ago = |ms: i64| relative_time(now, now - ms);
        assert_eq!(ago(0), "now");
        assert_eq!(ago(59_999), "now");
        assert_eq!(ago(60_000), "1m");
        assert_eq!(ago(59 * 60_000 + 59_000), "59m");
        assert_eq!(ago(3_600_000), "1h");
        assert_eq!(ago(23 * 3_600_000 + 1), "23h");
        assert_eq!(ago(24 * 3_600_000), "1d");
        assert_eq!(ago(10 * 86_400_000), "10d");
        // A clock that disagrees by a little shows "now", not a negative.
        assert_eq!(relative_time(now, now + 5000), "now");
    }

    #[core::prelude::v1::test]
    fn arrow_keys_walk_the_rows_and_stop_at_the_ends() {
        let tree = layout(&sample(), &Options::default());
        let keys = tree.keys();
        assert_eq!(step(&keys, None, true), Some(keys[0].clone()));
        assert_eq!(step(&keys, None, false), Some(keys.last().unwrap().clone()));
        assert_eq!(step(&keys, Some(&keys[0]), true), Some(keys[1].clone()));
        assert_eq!(step(&keys, Some(&keys[0]), false), Some(keys[0].clone()));
        let last = keys.last().unwrap();
        assert_eq!(step(&keys, Some(last), true), Some(last.clone()));
        // A row that vanished (a filter changed) starts over like no row.
        assert_eq!(
            step(&keys, Some(&RowKey::Thread("gone".into())), true),
            Some(keys[0].clone())
        );
        assert_eq!(step(&[], None, true), None);
    }

    #[core::prelude::v1::test]
    fn edits_apply_to_the_list_at_once() {
        let mut s = sample();
        s.apply(&Op::Rename {
            id: "a".into(),
            title: "  New title ".into(),
        });
        assert_eq!(s.thread("a").unwrap().title, "New title");
        s.apply(&Op::Pin {
            id: "a".into(),
            pinned: true,
        });
        assert!(s.thread("a").unwrap().pinned);
        s.apply(&Op::Archive { id: "a".into() });
        s.apply(&Op::Delete { id: "b".into() });
        assert!(s.thread("a").is_none() && s.thread("b").is_none());
        s.apply(&Op::Move {
            id: "c".into(),
            project: "p1".into(),
        });
        assert_eq!(s.thread("c").unwrap().project, "p1");
        assert_eq!(s.thread("c").unwrap().cwd, r"C:\work\engine");
        // A move to a group with no folder (No project) is not possible: nothing changes.
        s.apply(&Op::Move {
            id: "c".into(),
            project: NO_PROJECT.into(),
        });
        assert_eq!(s.thread("c").unwrap().project, "p1");
        s.apply(&Op::RenameProject {
            id: "p1".into(),
            name: "motor".into(),
        });
        assert_eq!(s.project("p1").unwrap().name, "motor");
        s.apply(&Op::RemoveProject { id: "p1".into() });
        assert!(s.project("p1").is_none());
        assert_eq!(s.thread("c").unwrap().project, NO_PROJECT);
        s.apply(&Op::AddProject {
            folder: r"C:\x\new".into(),
            name: "new".into(),
        });
        let added = s.projects.iter().position(|p| p.name == "new").unwrap();
        let none = s
            .projects
            .iter()
            .position(|p| p.kind == ProjectKind::None)
            .unwrap();
        assert!(
            added < none,
            "a new project is listed before the automatic ones and No project"
        );
        // The same folder twice is one project.
        s.apply(&Op::AddProject {
            folder: r"C:\x\new".into(),
            name: "again".into(),
        });
        assert_eq!(
            s.projects
                .iter()
                .filter(|p| p.path.as_deref() == Some(r"C:\x\new"))
                .count(),
            1
        );
    }

    #[core::prelude::v1::test]
    fn the_no_project_group_exists_exactly_when_it_has_a_thread() {
        let mut s = sample();
        s.apply(&Op::Delete { id: "d".into() });
        assert!(!s.projects.iter().any(|p| p.kind == ProjectKind::None));
        s.apply(&Op::RemoveProject { id: "p2".into() });
        assert!(s.projects.iter().any(|p| p.kind == ProjectKind::None));
        assert!(Snapshot::default().is_empty());
        assert!(!sample().is_empty());
    }

    #[core::prelude::v1::test]
    fn an_untitled_thread_has_a_name() {
        assert_eq!(thread("x", "  ", "p", 0).display_title(), UNTITLED);
    }

    #[core::prelude::v1::test]
    fn a_title_is_shown_on_one_line() {
        assert_eq!(
            thread("x", "Fix\nthe\r\n\tbug 🐛", "p", 0).display_title(),
            "Fix the bug 🐛"
        );
        assert_eq!(thread("x", " plain ", "p", 0).display_title(), "plain");
    }
}
