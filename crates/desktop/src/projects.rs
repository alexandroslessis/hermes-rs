//! The sidebar's Projects view (`projects.tree`; Hermes's `tui_gateway/project_tree.py`): the
//! user's projects first (from the shared store, the GPUI app's too), each owning the sessions
//! whose folder lies in one of its folders; then one automatic project per git repository the
//! other sessions ran in; and a "Home" bucket, first, for the rest. A repository shows as one lane
//! (Hermes splits lanes by branch and worktree; hermes-rs records neither per session).

use std::collections::HashMap;
use std::path::Path;

use hermes_store::{Project, project_for_path};
use serde_json::{Value, json};

/// Hermes's synthetic bucket for sessions no project claims.
pub const NO_PROJECT_ID: &str = "__no_project__";
const NO_PROJECT_LABEL: &str = "Home";
/// The lane a repository's sessions sit in (`DEFAULT_BRANCH_LABEL`).
const LANE_LABEL: &str = "main";

fn base_name(path: &str) -> String {
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
fn path_key(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("/")
}

/// The git repository a folder is in (the nearest folder holding `.git`), if any.
fn git_root(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(|root| root.display().to_string())
}

fn last_active(sessions: &[&Value]) -> f64 {
    sessions
        .iter()
        .filter_map(|s| s["last_active"].as_f64())
        .fold(0.0, f64::max)
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

/// `repo → lane → sessions` for one project's sessions, plus a repo for each declared folder no
/// session covers (so an empty project still shows its folders). `hydrate` false leaves the lanes'
/// session lists empty but keeps the counts (`_build_repos`, `_seed_folder_repos`).
fn repos(
    sessions: &[&Value],
    folders: &[String],
    hydrate: bool,
    roots: &mut HashMap<String, Option<String>>,
) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut by_root: HashMap<String, (bool, Vec<&Value>)> = HashMap::new();
    for session in sessions {
        let cwd = session["cwd"].as_str().unwrap_or_default().to_string();
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
            .push(session);
    }
    let mut out: Vec<Value> = order
        .iter()
        .map(|root| {
            let (is_git, rows) = &by_root[&path_key(root)];
            let mut rows = rows.clone();
            rows.sort_by(|a, b| b["last_active"].as_f64().partial_cmp(&a["last_active"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
            json!({
                "id": root,
                "label": base_name(root),
                "path": root,
                "sessionCount": rows.len(),
                "groups": [{
                    "id": format!("{root}::branch::{LANE_LABEL}"),
                    "label": LANE_LABEL,
                    "path": root,
                    "isMain": true,
                    "isKanban": false,
                    "isGit": is_git,
                    "sessions": if hydrate { rows.into_iter().cloned().collect() } else { Vec::new() },
                }],
            })
        })
        .collect();
    for folder in folders {
        let root =
            git_root(folder).unwrap_or_else(|| folder.trim_end_matches(['/', '\\']).to_string());
        if !out.iter().any(|repo| {
            repo["id"]
                .as_str()
                .is_some_and(|id| path_key(id) == path_key(&root))
        }) {
            out.push(json!({"id": root, "label": base_name(&root), "path": root, "groups": [], "sessionCount": 0}));
        }
    }
    out
}

/// One project node (`_project_node`).
#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    label: &str,
    path: Option<&str>,
    repos: Vec<Value>,
    sessions: &[&Value],
    preview_limit: usize,
    is_auto: bool,
    is_no_project: bool,
) -> Value {
    let mut previews = sessions.to_vec();
    previews.sort_by(|a, b| {
        b["last_active"]
            .as_f64()
            .partial_cmp(&a["last_active"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    previews.truncate(preview_limit);
    json!({
        "id": id,
        "label": label,
        "path": path,
        "color": null,
        "icon": null,
        "isAuto": is_auto,
        "isNoProject": is_no_project,
        "sessionCount": sessions.len(),
        "lastActive": last_active(sessions),
        "totalTokens": 0,
        "totalCostUsd": 0,
        "repos": repos,
        "previewSessions": previews,
        "sessionIds": sessions.iter().filter_map(|s| s["id"].as_str()).collect::<Vec<_>>(),
    })
}

/// `projects.tree` (`build_tree`) over the sidebar's session rows.
pub fn tree(
    projects: &[Project],
    sessions: &[Value],
    preview_limit: usize,
    hydrate: bool,
) -> Value {
    let mut roots: HashMap<String, Option<String>> = HashMap::new();
    let mut owned: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut unowned: Vec<&Value> = Vec::new();
    for session in sessions {
        match project_for_path(projects, session["cwd"].as_str().unwrap_or_default()) {
            Some(project) => owned.entry(project.id.as_str()).or_default().push(session),
            None => unowned.push(session),
        }
    }
    let mut result = Vec::new();
    let mut scoped: Vec<Value> = Vec::new();
    // The user's projects, shown even without sessions.
    for project in projects {
        let rows = owned.remove(project.id.as_str()).unwrap_or_default();
        scoped.extend(rows.iter().map(|s| s["id"].clone()));
        let repos = repos(&rows, &project.folders, hydrate, &mut roots);
        result.push(node(
            &project.id,
            &project.name,
            project.primary_folder(),
            repos,
            &rows,
            preview_limit,
            false,
            false,
        ));
    }
    // Automatic projects: the git repositories the other sessions ran in.
    let mut auto_order: Vec<String> = Vec::new();
    let mut auto: HashMap<String, Vec<&Value>> = HashMap::new();
    let mut homeless: Vec<&Value> = Vec::new();
    for session in unowned {
        let cwd = session["cwd"].as_str().unwrap_or_default().to_string();
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
        let rows = &auto[&path_key(&root)];
        scoped.extend(rows.iter().map(|s| s["id"].clone()));
        let repos = repos(rows, &[], hydrate, &mut roots);
        result.push(node(
            &root,
            &base_name(&root),
            Some(&root),
            repos,
            rows,
            preview_limit,
            true,
            false,
        ));
    }
    // Home leads the list when anything is left for it.
    if !homeless.is_empty() {
        scoped.extend(homeless.iter().map(|s| s["id"].clone()));
        let mut lane_rows = homeless.clone();
        lane_rows.sort_by(|a, b| {
            b["last_active"]
                .as_f64()
                .partial_cmp(&a["last_active"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let home_repo = json!({
            "id": NO_PROJECT_ID,
            "label": NO_PROJECT_LABEL,
            "path": null,
            "sessionCount": homeless.len(),
            "groups": [{
                "id": NO_PROJECT_ID,
                "label": NO_PROJECT_LABEL,
                "path": null,
                "isMain": false,
                "isKanban": false,
                "sessions": if hydrate { lane_rows.into_iter().cloned().collect() } else { Vec::new() },
            }],
        });
        result.insert(
            0,
            node(
                NO_PROJECT_ID,
                NO_PROJECT_LABEL,
                None,
                vec![home_repo],
                &homeless,
                preview_limit,
                false,
                true,
            ),
        );
    }
    json!({"projects": result, "scoped_session_ids": scoped})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_go_to_their_project_then_their_repository_then_home() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let elsewhere =
            std::env::temp_dir().join(format!("hermes-no-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        let elsewhere = elsewhere.display().to_string();
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
        assert_eq!(projects[0]["sessionIds"], json!(["c"]));
        assert_eq!(projects[1]["label"], "Mine");
        assert_eq!(projects[1]["sessionIds"], json!(["a"]));
        // The declared folder shows as a repo, the session's folder under it doesn't add one.
        assert_eq!(projects[1]["repos"].as_array().unwrap().len(), 2);
        assert_eq!(projects[2]["isAuto"], true);
        assert_eq!(projects[2]["sessionIds"], json!(["b"]));
        assert_eq!(projects[2]["repos"][0]["groups"][0]["isGit"], true);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn one_folder_is_one_entry_however_it_is_spelled() {
        // A declared folder with backslashes; sessions recorded with slashes, other case, a
        // trailing separator: one repo with all of them, not one per spelling.
        let project = Project {
            id: "prj_1".into(),
            name: "Mine".into(),
            folders: vec![r"C:\work\mine".into()],
            created_at: 1,
        };
        let sessions = vec![
            json!({"id": "a", "cwd": "C:/work/mine", "last_active": 2.0}),
            json!({"id": "b", "cwd": r"c:\Work\Mine\", "last_active": 1.0}),
        ];
        let built = tree(&[project], &sessions, 3, true);
        let mine = &built["projects"][0];
        assert_eq!(mine["label"], "Mine");
        let repos = mine["repos"].as_array().unwrap();
        assert_eq!(repos.len(), 1, "{repos:#?}");
        assert_eq!(repos[0]["sessionCount"], 2);

        // An automatic project (this repository) reached with both kinds of slashes.
        let repo = env!("CARGO_MANIFEST_DIR");
        let sessions = vec![
            json!({"id": "c", "cwd": repo.replace('\\', "/"), "last_active": 2.0}),
            json!({"id": "d", "cwd": repo.replace('/', r"\"), "last_active": 1.0}),
        ];
        let built = tree(&[], &sessions, 3, true);
        let projects = built["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1, "{projects:#?}");
        assert_eq!(projects[0]["repos"].as_array().unwrap().len(), 1);
        assert_eq!(projects[0]["sessionCount"], 2);
    }
}
