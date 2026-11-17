//! `projects.*`: the sidebar's projects (named groups of folders, in the store's `projects`
//! table). Hermes: `tui_gateway/methods_session.py` (projects); the shapes are in `projects.rs`.

use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{
    ActiveId, ProjectId, ProjectsCreate, ProjectsEdit, ProjectsTree, Reply, RpcError,
};

impl Gateway {
    /// `projects.tree` (the tree, counts) and `projects.project_sessions` (one project's
    /// sessions, hydrated).
    pub(super) async fn projects_tree(self: &Arc<Self>, p: ProjectsTree) -> Reply {
        let preview = p.preview_limit;
        let (projects, rows) = self
            .blocking(|g| {
                (
                    g.runtime.store().list_projects().unwrap_or_default(),
                    g.session_rows(),
                )
            })
            .await?;
        // The tree lists counts; one project's sessions come hydrated.
        let single = p.project_id.clone();
        let mut tree = tokio::task::spawn_blocking(move || {
            crate::projects::tree(&projects, &rows, preview, single.is_some())
        })
        .await
        .unwrap_or_default();
        match p.project_id.as_deref() {
            Some(id) => {
                let project = tree["projects"]
                    .as_array()
                    .and_then(|all| all.iter().find(|p| p["id"] == id).cloned());
                Ok(json!({"project": project}))
            }
            None => {
                tree["active_id"] = self.rest.config_value("projects.active_id");
                Ok(tree)
            }
        }
    }

    pub(super) async fn projects_list(self: &Arc<Self>) -> Reply {
        Ok(self.blocking(|g| g.project_list()).await?)
    }

    pub(super) async fn projects_create(self: &Arc<Self>, p: ProjectsCreate) -> Reply {
        let name = p.name;
        let mut folders = p.folders;
        if let Some(primary) = p.primary_path.filter(|p| !p.is_empty()) {
            folders.retain(|f| f != &primary);
            folders.insert(0, primary);
        }
        let project = self
            .blocking(move |g| g.runtime.store().create_project(&name, &folders))
            .await?
            .map_err(RpcError::failed)?;
        if p.activate {
            self.rest
                .set_config_value("projects.active_id", json!(project.id));
        }
        Ok(json!({"project": crate::projects::record(&project)}))
    }

    /// `projects.update` and `projects.add_folder` (the same edit).
    pub(super) async fn projects_update(self: &Arc<Self>, p: ProjectsEdit) -> Reply {
        let id = p.id.as_str();
        let wanted = id.to_string();
        let mut project = self
            .blocking(move |g| {
                g.runtime
                    .store()
                    .list_projects()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|p| p.id == wanted)
            })
            .await?
            .ok_or(RpcError::server(format!("No project {id}")))?;
        if let Some(name) = p.name.as_deref() {
            project.name = name.trim().to_string();
        }
        if let Some(path) = p.path.as_deref() {
            project.folders.retain(|f| f != path);
            if p.is_primary {
                project.folders.insert(0, path.to_string());
            } else {
                project.folders.push(path.to_string());
            }
        }
        let changed = project.clone();
        self.blocking(move |g| {
            g.runtime
                .store()
                .update_project(&changed.id, &changed.name, &changed.folders)
        })
        .await?
        .map_err(RpcError::failed)?;
        Ok(json!({"project": crate::projects::record(&project)}))
    }

    pub(super) async fn projects_delete(self: &Arc<Self>, p: ProjectId) -> Reply {
        let id = p.id.as_str();
        let gone = id.to_string();
        self.blocking(move |g| g.runtime.store().delete_project(&gone))
            .await?
            .map_err(RpcError::failed)?;
        if self.rest.config_value("projects.active_id") == id {
            self.rest
                .set_config_value("projects.active_id", Value::Null);
        }
        Ok(self.blocking(|g| g.project_list()).await?)
    }

    pub(super) fn projects_set_active(&self, p: ActiveId) -> Reply {
        let id = p.id;
        self.rest.set_config_value("projects.active_id", id.clone());
        Ok(json!({"active_id": id}))
    }
}

impl Gateway {
    /// `projects.list`: the user's projects and the active one.
    pub(super) fn project_list(&self) -> Value {
        let projects: Vec<Value> = self
            .runtime
            .store()
            .list_projects()
            .unwrap_or_default()
            .iter()
            .map(crate::projects::record)
            .collect();
        json!({"projects": projects, "active_id": self.rest.config_value("projects.active_id")})
    }
}
