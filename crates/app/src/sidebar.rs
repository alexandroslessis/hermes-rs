//! Grouping sessions into projects for the sidebar, as Hermes's "Group by → Projects" does: a
//! session belongs to the git repository containing its folder, or to the folder itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hermes_store::SessionRecord;

pub struct ProjectGroup {
    /// The project's root folder, used as the group's identity.
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

/// Groups sessions (already newest first) by project; groups are ordered by their newest session.
/// `roots` caches folder → project root so the filesystem is probed once per folder.
pub fn group_sessions(
    sessions: &[SessionRecord],
    roots: &mut HashMap<String, PathBuf>,
) -> Vec<ProjectGroup> {
    let mut groups: Vec<ProjectGroup> = Vec::new();
    for session in sessions {
        let root = roots
            .entry(session.cwd.clone())
            .or_insert_with(|| project_root(Path::new(&session.cwd)))
            .clone();
        match groups.iter_mut().find(|group| group.root == root) {
            Some(group) => group.sessions.push(session.clone()),
            None => groups.push(ProjectGroup {
                name: root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string()),
                root,
                sessions: vec![session.clone()],
            }),
        }
    }
    groups
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
        let groups = group_sessions(&sessions, &mut HashMap::new());
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
    fn relative_times() {
        let now = 1_800_000_000_000;
        assert_eq!(relative_time(now - 10_000, now), "now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2d");
    }
}
