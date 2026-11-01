//! Projects, after Hermes's `hermes_cli/projects_db.py`: a name and the folders it spans. The
//! first folder is the primary one, where new sessions start. A session belongs to the project
//! with the longest folder that contains its working directory.

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{Store, now_ms};

pub(crate) const PROJECTS_SCHEMA: &str = "
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE project_folders (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (project_id, path)
);
";

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(PROJECTS_SCHEMA)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The folders the project spans; the first is the primary one.
    pub folders: Vec<String>,
    pub created_at: i64,
}

impl Project {
    pub fn primary_folder(&self) -> Option<&str> {
        self.folders.first().map(String::as_str)
    }
}

/// Whether `path` is `folder` or inside it (Windows paths: case and slash direction don't
/// matter).
pub fn is_under(folder: &str, path: &str) -> bool {
    let normalize = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (folder, path) = (normalize(folder), normalize(path));
    !folder.is_empty()
        && (path == folder
            || path
                .strip_prefix(&folder)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// The project owning `cwd`: the one with the longest folder containing it (Hermes's
/// `project_for_path`).
pub fn project_for_path<'a>(projects: &'a [Project], cwd: &str) -> Option<&'a Project> {
    projects
        .iter()
        .filter_map(|project| {
            project
                .folders
                .iter()
                .filter(|folder| is_under(folder, cwd))
                .map(String::len)
                .max()
                .map(|len| (project, len))
        })
        .max_by_key(|(_, len)| *len)
        .map(|(project, _)| project)
}

impl Store {
    /// Every project, oldest first.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn.lock();
        let mut statement =
            conn.prepare("SELECT id, name, created_at FROM projects ORDER BY created_at, id")?;
        let mut projects: Vec<Project> = statement
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    folders: Vec::new(),
                    created_at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut folders = conn
            .prepare("SELECT path FROM project_folders WHERE project_id = ?1 ORDER BY position")?;
        for project in &mut projects {
            project.folders = folders
                .query_map([&project.id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(projects)
    }

    pub fn create_project(&self, name: &str, folders: &[String]) -> Result<Project> {
        let project = Project {
            id: format!("prj_{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            folders: dedup(folders),
            created_at: now_ms(),
        };
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![project.id, project.name, project.created_at],
        )?;
        insert_folders(&tx, &project.id, &project.folders)?;
        tx.commit()?;
        Ok(project)
    }

    /// Renames the project and replaces its folders.
    pub fn update_project(&self, id: &str, name: &str, folders: &[String]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE projects SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        anyhow::ensure!(changed == 1, "no project {id}");
        tx.execute("DELETE FROM project_folders WHERE project_id = ?1", [id])?;
        insert_folders(&tx, id, &dedup(folders))?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes the project. Its sessions stay; they just no longer belong to it.
    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("DELETE FROM projects WHERE id = ?1", [id])?;
        Ok(())
    }
}

fn dedup(folders: &[String]) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for folder in folders {
        let folder = folder.trim().trim_end_matches(['/', '\\']).to_string();
        if !folder.is_empty() && !unique.iter().any(|f| f.eq_ignore_ascii_case(&folder)) {
            unique.push(folder);
        }
    }
    unique
}

fn insert_folders(conn: &Connection, project_id: &str, folders: &[String]) -> Result<()> {
    let mut statement = conn
        .prepare("INSERT INTO project_folders (project_id, path, position) VALUES (?1, ?2, ?3)")?;
    for (position, folder) in folders.iter().enumerate() {
        statement.execute(params![project_id, folder, position as i64])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_round_trip_with_ordered_folders() {
        let store = Store::open_in_memory().unwrap();
        let folders = vec![
            r"C:\work\app".to_string(),
            "C:/work/lib/".into(),
            r"c:\WORK\APP".into(),
        ];
        let project = store.create_project("App", &folders).unwrap();
        assert_eq!(project.folders, vec![r"C:\work\app", "C:/work/lib"]);
        assert_eq!(store.list_projects().unwrap(), vec![project.clone()]);

        store
            .update_project(&project.id, "App 2", &["D:/other".into()])
            .unwrap();
        let listed = store.list_projects().unwrap();
        assert_eq!(listed[0].name, "App 2");
        assert_eq!(listed[0].primary_folder(), Some("D:/other"));

        store.delete_project(&project.id).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn sessions_belong_to_the_longest_matching_folder() {
        let project = |id: &str, folders: &[&str]| Project {
            id: id.into(),
            name: id.into(),
            folders: folders.iter().map(|f| f.to_string()).collect(),
            created_at: 0,
        };
        let projects = vec![
            project("outer", &["C:/work"]),
            project("inner", &["D:/x", r"C:\work\app"]),
        ];
        let owner = |cwd| project_for_path(&projects, cwd).map(|p| p.id.as_str());
        assert_eq!(owner(r"c:\work\app\src"), Some("inner"));
        assert_eq!(owner("C:/work/other"), Some("outer"));
        assert_eq!(owner("C:/work"), Some("outer"));
        assert_eq!(owner("C:/workshop"), None);
        assert_eq!(owner("E:/"), None);
    }
}
