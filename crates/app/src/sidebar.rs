//! Grouping sessions for the sidebar, as Hermes's projects do: a session belongs to the project
//! with the longest folder containing its working directory. Sessions outside every project fall
//! back to Hermes's automatic grouping: the git repository containing the folder, or the folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hermes_store::{Project, SessionRecord, project_for_path};

pub struct ProjectGroup {
    /// The group's identity: the project id, or `auto:` and the folder for automatic groups.
    pub key: String,
    /// The named project, `None` for an automatic group.
    pub project: Option<Project>,
    /// Where new sessions in the group start: the project's primary folder, or the group's root.
    pub root: PathBuf,
    pub name: String,
    pub sessions: Vec<SessionRecord>,
}

pub fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Groups sessions (already newest first). Projects come first, the most recently active on
/// top and empty ones after; then the automatic groups, by their newest session. `roots` caches
/// folder → git root so the filesystem is probed once per folder.
pub fn group_sessions(
    sessions: &[SessionRecord],
    projects: &[Project],
    roots: &mut HashMap<String, PathBuf>,
) -> Vec<ProjectGroup> {
    let mut named: Vec<ProjectGroup> = Vec::new();
    let mut auto: Vec<ProjectGroup> = Vec::new();
    for session in sessions {
        if let Some(project) = project_for_path(projects, &session.cwd) {
            match named.iter_mut().find(|group| group.key == project.id) {
                Some(group) => group.sessions.push(session.clone()),
                None => named.push(project_group(project, vec![session.clone()])),
            }
            continue;
        }
        let root = roots
            .entry(session.cwd.clone())
            .or_insert_with(|| project_root(Path::new(&session.cwd)))
            .clone();
        let key = format!("auto:{}", root.display());
        match auto.iter_mut().find(|group| group.key == key) {
            Some(group) => group.sessions.push(session.clone()),
            None => auto.push(ProjectGroup {
                key,
                project: None,
                name: folder_name(&root),
                root,
                sessions: vec![session.clone()],
            }),
        }
    }
    // Projects without sessions still show, so a new one can be started in them.
    for project in projects {
        if !named.iter().any(|group| group.key == project.id) {
            named.push(project_group(project, Vec::new()));
        }
    }
    named.extend(auto);
    named
}

fn project_group(project: &Project, sessions: Vec<SessionRecord>) -> ProjectGroup {
    ProjectGroup {
        key: project.id.clone(),
        root: PathBuf::from(project.primary_folder().unwrap_or_default()),
        name: project.name.clone(),
        project: Some(project.clone()),
        sessions,
    }
}

/// The last component of `path`, or all of it for a drive root.
pub fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// "now", "5m", "3h", "2d", "4w", then the date.
pub fn relative_time(timestamp_ms: i64, now_ms: i64) -> String {
    let minutes = (now_ms - timestamp_ms).max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        1440..=10079 => format!("{}d", minutes / 1440),
        10080..=40319 => format!("{}w", minutes / 10080),
        _ => chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .map(|date| date.format("%b %d").to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, cwd: &str) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            title: None,
            cwd: cwd.into(),
            provider: "p".into(),
            model: "m".into(),
            system_prompt: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn sessions_group_by_git_root_in_recency_order() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("crates/app")).unwrap();
        let other = dir.path().join("notes");
        std::fs::create_dir_all(&other).unwrap();
        let sessions = vec![
            session("a", &repo.join("crates/app").display().to_string()),
            session("b", &other.display().to_string()),
            session("c", &repo.display().to_string()),
        ];
        let groups = group_sessions(&sessions, &[], &mut HashMap::new());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "repo");
        assert_eq!(
            groups[0]
                .sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(groups[1].name, "notes");
    }

    #[test]
    fn projects_take_their_sessions_and_show_even_when_empty() {
        let project = |id: &str, folders: &[&str]| Project {
            id: id.into(),
            name: id.to_uppercase(),
            folders: folders.iter().map(|f| f.to_string()).collect(),
            created_at: 0,
        };
        let projects = vec![
            project("app", &["C:/nowhere/app", "C:/nowhere/lib"]),
            project("empty", &["C:/nowhere/empty"]),
        ];
        let sessions = vec![
            session("a", "C:/nowhere/lib/src"),
            session("b", "C:/nowhere/scratch"),
            session("c", "C:/nowhere/app"),
        ];
        let groups = group_sessions(&sessions, &projects, &mut HashMap::new());
        let summary: Vec<(String, Vec<&str>)> = groups
            .iter()
            .map(|g| {
                (
                    g.key.clone(),
                    g.sessions.iter().map(|s| s.id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("app".to_string(), vec!["a", "c"]),
                ("empty".to_string(), vec![]),
                ("auto:C:/nowhere/scratch".to_string(), vec!["b"]),
            ]
        );
        assert_eq!(groups[0].root, PathBuf::from("C:/nowhere/app"));
        assert_eq!(groups[0].name, "APP");
    }

    #[test]
    fn relative_times() {
        let now = 1_800_000_000_000;
        assert_eq!(relative_time(now - 10_000, now), "now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2d");
    }
}
