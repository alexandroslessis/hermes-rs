//! The sidebar's Projects view as Hermes's page reads it (`projects.tree`, `ProjectRecord`). The
//! grouping (projects, automatic repositories, Home) is `hermes_core::projects`; this is its JSON
//! over the sidebar's session rows.
//!
//! Hermes: `tui_gateway/project_tree.py` (`build_tree`, `_project_node`, the field names).

use hermes_core::projects::{LANE_LABEL, Node, NodeKind, Repo, RepoKind, TreeRow, base_name};
use hermes_store::Project;
use serde_json::{Value, json};

pub use hermes_core::projects::NO_PROJECT_ID;

/// A sidebar row (`session_row`) as the tree reads it.
struct Row<'a>(&'a Value);

impl TreeRow for Row<'_> {
    fn cwd(&self) -> &str {
        self.0["cwd"].as_str().unwrap_or_default()
    }

    fn last_active(&self) -> Option<f64> {
        self.0["last_active"].as_f64()
    }
}

/// A project as `projects.list` / `projects.create` describe it (`ProjectRecord`).
pub fn record(project: &Project) -> Value {
    json!({
        "id": project.id,
        "slug": project.id,
        "name": project.name,
        "primary_path": project.primary_folder(),
        "archived": false,
        "created_at": project.created_at,
        "folders": project.folders.iter().enumerate().map(|(i, path)| json!({
            "path": path,
            "label": base_name(path),
            "is_primary": i == 0,
        })).collect::<Vec<_>>(),
    })
}

fn rows(rows: &[&Row<'_>]) -> Vec<Value> {
    rows.iter().map(|row| row.0.clone()).collect()
}

/// One repository entry (`_build_repos`, `_seed_folder_repos`); `hydrate` false leaves the lanes'
/// session lists empty but keeps the counts.
fn repo(repo: &Repo<'_, Row<'_>>, hydrate: bool) -> Value {
    let sessions = |list: &[&Row<'_>]| if hydrate { rows(list) } else { Vec::new() };
    match repo.kind {
        RepoKind::Lane { is_git } => {
            let root = repo.root.as_deref().unwrap_or_default();
            json!({
                "id": root,
                "label": repo.label,
                "path": root,
                "sessionCount": repo.sessions.len(),
                "groups": [{
                    "id": format!("{root}::branch::{LANE_LABEL}"),
                    "label": LANE_LABEL,
                    "path": root,
                    "isMain": true,
                    "isKanban": false,
                    "isGit": is_git,
                    "sessions": sessions(&repo.sessions),
                }],
            })
        }
        RepoKind::Folder => {
            let root = repo.root.as_deref().unwrap_or_default();
            json!({"id": root, "label": repo.label, "path": root, "groups": [], "sessionCount": 0})
        }
        RepoKind::Home => json!({
            "id": NO_PROJECT_ID,
            "label": repo.label,
            "path": null,
            "sessionCount": repo.sessions.len(),
            "groups": [{
                "id": NO_PROJECT_ID,
                "label": repo.label,
                "path": null,
                "isMain": false,
                "isKanban": false,
                "sessions": sessions(&repo.sessions),
            }],
        }),
    }
}

/// One project node (`_project_node`).
fn node(node: &Node<'_, Row<'_>>, preview_limit: usize, hydrate: bool) -> Value {
    json!({
        "id": node.id,
        "label": node.label,
        "path": node.path,
        "color": null,
        "icon": null,
        "isAuto": node.kind == NodeKind::Auto,
        "isNoProject": node.kind == NodeKind::NoProject,
        "sessionCount": node.sessions.len(),
        "lastActive": node.last_active(),
        "totalTokens": 0,
        "totalCostUsd": 0,
        "repos": node.repos.iter().map(|r| repo(r, hydrate)).collect::<Vec<_>>(),
        "previewSessions": rows(&node.previews(preview_limit)),
        "sessionIds": node.sessions.iter().filter_map(|s| s.0["id"].as_str()).collect::<Vec<_>>(),
    })
}

/// `projects.tree` (`build_tree`) over the sidebar's session rows.
pub fn tree(
    projects: &[Project],
    sessions: &[Value],
    preview_limit: usize,
    hydrate: bool,
) -> Value {
    let wrapped: Vec<Row<'_>> = sessions.iter().map(Row).collect();
    let built = hermes_core::projects::tree(projects, &wrapped);
    let nodes: Vec<Value> = built
        .nodes
        .iter()
        .map(|n| node(n, preview_limit, hydrate))
        .collect();
    let scoped: Vec<Value> = built.scoped.iter().map(|s| s.0["id"].clone()).collect();
    json!({"projects": nodes, "scoped_session_ids": scoped})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_is_hermess_shape() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let elsewhere = hermes_testdir::tempdir().unwrap();
        let elsewhere = elsewhere.path().display().to_string();
        let project = Project {
            id: "prj_1".into(),
            name: "Mine".into(),
            folders: vec!["C:/work/mine".into()],
            created_at: 1,
        };
        let sessions = vec![
            json!({"id": "a", "cwd": "C:/work/mine/src", "last_active": 3.0}),
            json!({"id": "b", "cwd": repo, "last_active": 2.0}),
            json!({"id": "c", "cwd": elsewhere, "last_active": 1.0}),
        ];
        let tree = tree(&[project], &sessions, 3, false);
        let projects = tree["projects"].as_array().unwrap();
        assert_eq!(projects[0]["id"], NO_PROJECT_ID);
        assert_eq!(projects[0]["isNoProject"], true);
        assert_eq!(projects[0]["sessionIds"], json!(["c"]));
        assert_eq!(projects[0]["repos"][0]["groups"][0]["sessions"], json!([]));
        assert_eq!(projects[1]["label"], "Mine");
        assert_eq!(projects[1]["sessionIds"], json!(["a"]));
        assert_eq!(projects[1]["previewSessions"][0]["id"], "a");
        // The declared folder shows as a repo with no lanes.
        assert_eq!(projects[1]["repos"][1]["groups"], json!([]));
        assert_eq!(projects[1]["repos"][1]["sessionCount"], 0);
        assert_eq!(projects[2]["isAuto"], true);
        assert_eq!(projects[2]["sessionIds"], json!(["b"]));
        assert_eq!(projects[2]["repos"][0]["groups"][0]["isGit"], true);
        assert_eq!(tree["scoped_session_ids"], json!(["a", "b", "c"]));
        // Hydrated, a lane lists its rows as given.
        let one = super::tree(&[], &sessions[2..], 3, true);
        assert_eq!(
            one["projects"][0]["repos"][0]["groups"][0]["sessions"][0]["id"],
            "c"
        );
    }
}
