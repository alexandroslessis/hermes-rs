//! The REST side of the gateway: `Gateway::rest` (the routes under `/api` that need the runtime or the
//! store, as `(status, body)`; `None` is Hermes's 404 "No such API endpoint"), the session routes, the
//! file tickets and the session rows the sidebar lists. Hermes: `hermes_cli/web_server.py`,
//! `hermes_cli/web_routers/*`.

use std::sync::Arc;

use super::*;
use serde_json::{Value, json};

impl Gateway {
    /// The app's data folder.
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// The file a page path names, relative to the session's folder when it names a session.
    pub fn file_path(&self, path: &str, session_id: Option<&str>) -> PathBuf {
        let cwd = session_id
            .filter(|id| !id.is_empty())
            .and_then(|id| self.runtime.store().session(id).ok().flatten())
            .map(|record| PathBuf::from(record.cwd));
        crate::files::resolve(path, cwd.as_deref())
    }

    /// Whether `ticket` was issued for `file` (`/api/files/ticket`).
    pub fn ticket_allows(&self, ticket: &str, file: &std::path::Path) -> bool {
        self.file_tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(ticket)
            .is_some_and(|issued| issued == file)
    }

    /// [`Self::blocking`] for a REST route: a thread that died answers 500 with the reason, never
    /// `None` (that is Hermes's 404 "No such API endpoint", which the page reads as "feature
    /// absent", rule 5).
    pub(super) async fn rest_blocking(
        self: &Arc<Self>,
        work: impl FnOnce(&Arc<Gateway>) -> Option<(u16, Value)> + Send + 'static,
    ) -> Option<(u16, Value)> {
        self.blocking(work)
            .await
            .unwrap_or_else(|error| Some((500, json!({"detail": error.message()}))))
    }

    /// REST routes this backend serves: `(status, body)`, or `None` for Hermes's 404.
    pub async fn rest(
        self: &Arc<Self>,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        if (method, path) == ("GET", "/api/profiles/sessions") {
            let query = query.map(str::to_string);
            return self
                .rest_blocking(move |g| Some((200, g.session_list(query.as_deref()))))
                .await;
        }
        // Settings ▸ Providers ▸ Custom endpoints (`endpoints.rs`).
        if let Some(rest) = path.strip_prefix("/api/providers/custom-endpoints") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let answer = match (method, rest.trim_start_matches('/')) {
                ("GET", "") => (200, crate::endpoints::list(&self.data_dir)),
                ("POST", "") => crate::endpoints::save(&self.data_dir, &body),
                ("POST", "validate") => crate::endpoints::validate(&self.data_dir, &body).await,
                ("POST", id) if id.ends_with("/activate") => {
                    let id = crate::rest::percent_decode(id.trim_end_matches("/activate"));
                    crate::endpoints::activate(&self.data_dir, &id).await
                }
                ("DELETE", id) if !id.is_empty() => {
                    let id = crate::rest::percent_decode(id);
                    crate::endpoints::delete(&self.data_dir, &id).await
                }
                _ => return None,
            };
            if method != "GET" {
                // A changed endpoint may have a new window or models.
                if let Ok(mut lengths) = self.context_lengths.lock() {
                    lengths.clear();
                }
                // ...or a new daily request limit.
                self.apply_settings();
            }
            return Some(answer);
        }
        // The Star map: learned skills and memories, viewed, edited and deleted (`learning.rs`).
        if let Some(route) = path.strip_prefix("/api/learning/") {
            let (skills, memory) = (self.runtime.skills(), self.runtime.memory());
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let id = crate::rest::query_param(query, "id")
                .or_else(|| body["id"].as_str().map(str::to_string))
                .unwrap_or_default();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let (method, route) = (method.to_string(), route.to_string());
            return tokio::task::spawn_blocking(move || match (method.as_str(), route.as_str()) {
                ("GET", "graph") => Some((200, crate::learning::graph(&skills, &memory))),
                ("GET", "node") => Some(crate::learning::detail(&skills, &memory, &id)),
                ("DELETE", "node") => Some(crate::learning::delete(&skills, &memory, &id)),
                ("PUT", "node") => Some(crate::learning::edit(&skills, &memory, &id, &content)),
                _ => None,
            })
            .await
            .ok()
            .flatten();
        }
        // Capabilities ▸ Tools (`toolsets.rs`).
        if let Some(route) = path.strip_prefix("/api/tools/") {
            let off = || {
                crate::toolsets::disabled_toolsets(
                    &self.rest.config_value(crate::toolsets::CONFIG_KEY),
                )
            };
            return match (method, route) {
                ("GET", "toolsets") => Some((200, crate::toolsets::list(&off()))),
                ("GET", "terminal/backends") => Some((200, crate::toolsets::terminal_backends())),
                ("GET", rest) if rest.starts_with("toolsets/") && rest.ends_with("/config") => {
                    let name = rest
                        .trim_start_matches("toolsets/")
                        .trim_end_matches("/config");
                    Some((
                        200,
                        crate::toolsets::config(&crate::rest::percent_decode(name)),
                    ))
                }
                ("PUT", rest) if rest.starts_with("toolsets/") && !rest[9..].contains('/') => {
                    let name = crate::rest::percent_decode(&rest[9..]);
                    let body: Value = serde_json::from_slice(body).unwrap_or_default();
                    let enabled = body["enabled"].as_bool().unwrap_or(true);
                    match crate::toolsets::switch(&off(), &name, enabled) {
                        Some(next) => {
                            self.rest
                                .set_config_value(crate::toolsets::CONFIG_KEY, json!(next));
                            self.runtime
                                .set_disabled_tools(crate::toolsets::disabled_tools(&next));
                            Some((200, json!({"ok": true, "name": name, "enabled": enabled})))
                        }
                        None => Some((404, json!({"detail": format!("No toolset {name}.")}))),
                    }
                }
                _ => None,
            };
        }
        // The composer's attachments (`attachments.rs`).
        if (method, path) == ("POST", "/api/chat/image-upload") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let data_dir = self.data_dir.clone();
            return tokio::task::spawn_blocking(move || {
                crate::attachments::upload_image(&data_dir, &body)
            })
            .await
            .ok();
        }
        // The side panel's file preview and spot editor (`files.rs`). Paths are relative to the
        // session's folder when the page names a session.
        let param = |name: &str| crate::rest::query_param(query, name);
        if (method, path) == ("GET", "/api/fs/read-data-url") {
            let file = self.file_path(
                &param("path").unwrap_or_default(),
                param("session_id").as_deref(),
            );
            if crate::files::is_sensitive(&file) {
                return Some((
                    403,
                    json!({"detail": "That file holds credentials and isn't shown."}),
                ));
            }
            let file = file.display().to_string();
            return tokio::task::spawn_blocking(move || crate::attachments::read_data_url(&file))
                .await
                .ok();
        }
        if (method, path) == ("GET", "/api/fs/read-text") {
            let file = self.file_path(
                &param("path").unwrap_or_default(),
                param("session_id").as_deref(),
            );
            return tokio::task::spawn_blocking(move || crate::files::read_text(&file))
                .await
                .ok();
        }
        if (method, path) == ("GET", "/api/fs/git-root") {
            let file = self.file_path(&param("path").unwrap_or_default(), None);
            return tokio::task::spawn_blocking(move || (200, crate::files::git_root(&file)))
                .await
                .ok();
        }
        if method == "POST"
            && matches!(
                path,
                "/api/fs/write-text" | "/api/files/mkdir" | "/api/files/ticket"
            )
        {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let named = body["path"].as_str().unwrap_or_default();
            if named.trim().is_empty() {
                return Some((400, json!({"detail": "path is required"})));
            }
            let file = self.file_path(named, body["session_id"].as_str());
            return match path {
                "/api/fs/write-text" => {
                    let content = body["content"].as_str().unwrap_or_default().to_string();
                    tokio::task::spawn_blocking(move || crate::files::write_text(&file, &content))
                        .await
                        .ok()
                }
                "/api/files/mkdir" => {
                    tokio::task::spawn_blocking(move || crate::files::mkdir(&file))
                        .await
                        .ok()
                }
                // A download link or media element can't send headers: the page asks for a
                // ticket bound to the file first (Hermes's `issue_file_ticket`).
                _ => {
                    let ticket = uuid::Uuid::new_v4().simple().to_string();
                    let mut tickets = self.file_tickets.lock().unwrap_or_else(|e| e.into_inner());
                    // (Old tickets go: a page makes a few per preview.)
                    if tickets.len() > 512 {
                        tickets.clear();
                    }
                    tickets.insert(ticket.clone(), file);
                    Some((200, json!({"ticket": ticket})))
                }
            };
        }
        // Sign-in providers (Nous Portal, Claude, Codex): none here.
        // The profiles window's SOUL.md editor (profiles.rs): the default profile's persona, read
        // and written in the data folder. The multi-profile routes (create, rename, delete,
        // export, import) aren't ported — a single-profile backend has no second profile.
        if let Some(name) = path
            .strip_prefix("/api/profiles/")
            .and_then(|rest| rest.strip_suffix("/soul"))
        {
            let name = crate::rest::percent_decode(name);
            if method == "GET" {
                let data_dir = self.data_dir.clone();
                return tokio::task::spawn_blocking(move || {
                    crate::profiles::soul(&data_dir, &name)
                })
                .await
                .ok();
            }
            if method == "PUT" {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                let content = body["content"].as_str().unwrap_or_default().to_string();
                let data_dir = self.data_dir.clone();
                return tokio::task::spawn_blocking(move || {
                    crate::profiles::save_soul(&data_dir, &name, &content)
                })
                .await
                .ok();
            }
        }
        if (method, path) == ("GET", "/api/providers/oauth") {
            return Some((200, json!({"providers": []})));
        }
        // Hermes's catalog of provider keys (`.env`): none here, an endpoint keeps its own key.
        // The Providers page waits for it before showing any view.
        if (method, path) == ("GET", "/api/env") {
            return Some((200, json!({})));
        }
        // The sidebar's search: the sessions whose messages match, one hit each, newest first
        // (Hermes's `/api/sessions/search`, over the store's full-text index).
        if (method, path) == ("GET", "/api/sessions/search") {
            let text = crate::rest::query_param(query, "q").unwrap_or_default();
            let store = self.runtime.store();
            // (The store matches the listed roles only.)
            let roles = ["user".to_string(), "assistant".to_string()];
            let hits = if text.trim().is_empty() {
                Vec::new()
            } else {
                store
                    .search(&hermes_store::SearchQuery {
                        query: text.trim(),
                        roles: &roles,
                        exclude_sessions: &[],
                        after_ms: None,
                        before_ms: None,
                        sort: hermes_store::SearchSort::Newest,
                        limit: 200,
                    })
                    .unwrap_or_default()
            };
            let mut seen = std::collections::HashSet::new();
            let results: Vec<Value> = hits
                .into_iter()
                .filter(|hit| seen.insert(hit.session_id.clone()))
                .filter_map(|hit| {
                    let record = store.session(&hit.session_id).ok().flatten()?;
                    Some(json!({
                        "session_id": hit.session_id,
                        "lineage_root": hit.session_id,
                        "snippet": hit.snippet,
                        "role": hit.role,
                        "model": record.model,
                        "source": "desktop",
                        "title": record.title,
                        "session_started": record.created_at as f64 / 1000.,
                        "last_active": record.updated_at as f64 / 1000.,
                    }))
                })
                .take(50)
                .collect();
            return Some((200, json!({"results": results})));
        }
        // Settings ▸ Advanced ▸ Logs: this run's log (the app logs to its own log file).
        if (method, path) == ("GET", "/api/logs") {
            let lines = crate::rest::query_param(query, "lines")
                .and_then(|l| l.parse().ok())
                .unwrap_or(200usize);
            let file = self.data_dir.join("logs").join("desktop.log");
            let text = tokio::fs::read_to_string(&file).await.unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            let tail: Vec<&str> = all[all.len().saturating_sub(lines)..].to_vec();
            return Some((
                200,
                json!({"file": file.display().to_string(), "lines": tail}),
            ));
        }
        // Command Center ▸ Maintenance: the curator and the memory files.
        match (method, path) {
            ("GET", "/api/curator") => {
                return Some((
                    200,
                    crate::maintenance::curator_status(&self.runtime.skills()),
                ));
            }
            ("PUT", "/api/curator/paused") => {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                let paused = body["paused"] == true;
                return Some((
                    200,
                    crate::maintenance::set_curator_paused(&self.runtime.skills(), paused),
                ));
            }
            ("POST", "/api/curator/run") => {
                let started = crate::maintenance::run_curator(
                    self.runtime.clone(),
                    self.actions.clone(),
                    self.data_dir.clone(),
                );
                return Some((
                    200,
                    json!({"ok": true, "pid": 0, "name": crate::maintenance::CURATOR_RUN,
                           "already_running": !started}),
                ));
            }
            ("GET", "/api/memory") => {
                return Some((200, crate::maintenance::memory_status(&self.data_dir)));
            }
            ("POST", "/api/memory/reset") => {
                let body: Value = serde_json::from_slice(body).unwrap_or_default();
                // Files and memory.db (which may wait for another process's lock): off the
                // async workers.
                let memory = self.runtime.memory();
                return Some(
                    tokio::task::spawn_blocking(move || {
                        crate::maintenance::reset_memory(&memory, &body)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        (
                            500,
                            json!({"detail": format!("The reset did not finish: {e}")}),
                        )
                    }),
                );
            }
            _ => {}
        }
        if let Some(name) = path
            .strip_prefix("/api/actions/")
            .filter(|_| method == "GET")
        {
            let name = crate::rest::percent_decode(name.trim_end_matches("/status"));
            let limit = crate::rest::query_param(query, "lines")
                .and_then(|l| l.parse().ok())
                .unwrap_or(200);
            return Some((200, self.actions.status(&name, limit)));
        }
        if path == "/api/staging" || path.starts_with("/api/staging/") {
            return crate::staging::route(&self.runtime, method, path).await;
        }
        if path == "/api/skills" || path.starts_with("/api/skills/") {
            let store = self.runtime.skills();
            return crate::skills::route(&store, &self.rest, method, path, query, body);
        }
        // Settings ▸ Model: the default model (the hermes-rs settings') and the side jobs' models
        // (`aux_models.rs`).
        if (method, path) == ("GET", "/api/model/auxiliary") {
            return Some((200, crate::aux_models::list(&self.data_dir)));
        }
        // One click: every side job on a local model (ours; the patched Settings page calls it).
        if path == "/api/model/auxiliary/local" && matches!(method, "GET" | "POST") {
            let well_known = &crate::aux_models::WELL_KNOWN_LOCAL;
            if method == "GET" {
                return Some(crate::aux_models::local_list(&self.data_dir, well_known).await);
            }
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let answer = crate::aux_models::use_local(&self.data_dir, &body, well_known).await;
            if let Ok(mut lengths) = self.context_lengths.lock() {
                lengths.clear();
            }
            return Some(answer);
        }
        if (method, path) == ("POST", "/api/model/set") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            match body["scope"].as_str() {
                None | Some("main") => {}
                Some("auxiliary") => return Some(crate::aux_models::assign(&self.data_dir, &body)),
                Some(scope) => {
                    return Some((
                        400,
                        json!({"detail": format!("Unknown model scope {scope}")}),
                    ));
                }
            }
            let (Some(provider), Some(model)) = (body["provider"].as_str(), body["model"].as_str())
            else {
                return Some((400, json!({"detail": "provider and model are required"})));
            };
            if Settings::load(&self.data_dir).provider(provider).is_none() {
                return Some((
                    400,
                    json!({"detail": format!("Unknown provider {provider}")}),
                ));
            }
            return Some(
                match crate::providers::save_default(&self.data_dir, provider, model) {
                    Ok(()) => (
                        200,
                        json!({"ok": true, "provider": provider, "model": model}),
                    ),
                    Err(error) => (500, json!({"detail": format!("{error:#}")})),
                },
            );
        }
        if (method, path) == ("PUT", "/api/config") {
            let answer = self.rest.handle(method, path, query, body);
            self.apply_settings();
            return answer;
        }
        // "Reveal in File Explorer" (patches/0016): show it selected in its containing
        // folder, as the Electron bridge's revealPath does, which the page's shim has
        // no answer for (so the local-only menu item hides).
        if (method, path) == ("POST", "/api/hermes-rs/reveal") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let link = body["url"].as_str().unwrap_or_default().to_string();
            let done = tokio::task::spawn_blocking(move || {
                crate::open_file::reveal_plan(&link).and_then(|open| crate::open_file::run(&open))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            return Some(match done {
                Ok(()) => (200, json!({"ok": true})),
                Err(error) => (400, json!({"detail": error})),
            });
        }
        // A `file:` link opened from the page (patches/0013, `open_file.rs`): its default app,
        // or Explorer with it selected when it's a program.
        if (method, path) == ("POST", "/api/hermes-rs/open-file") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let link = body["url"].as_str().unwrap_or_default().to_string();
            let done = tokio::task::spawn_blocking(move || {
                crate::open_file::plan(&link).and_then(|open| {
                    crate::open_file::run(&open)?;
                    Ok(matches!(open, crate::open_file::Open::Reveal(_)))
                })
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            return Some(match done {
                Ok(revealed) => (200, json!({"ok": true, "revealed": revealed})),
                Err(error) => (400, json!({"detail": error})),
            });
        }
        // "Choose a folder" (patches/0002): Windows' own folder dialog, opened by this process,
        // which is the desktop window's. Cancelling answers no paths.
        if (method, path) == ("POST", "/api/hermes-rs/select-folder") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let title = body["title"].as_str().unwrap_or_default().to_string();
            let start = body["defaultPath"].as_str().unwrap_or_default().to_string();
            let picked = tokio::task::spawn_blocking(move || {
                let mut dialog = rfd::FileDialog::new();
                if !title.is_empty() {
                    dialog = dialog.set_title(&title);
                }
                if !start.is_empty() && Path::new(&start).is_dir() {
                    dialog = dialog.set_directory(&start);
                }
                dialog.pick_folder()
            })
            .await
            .ok()
            .flatten();
            let paths: Vec<String> = picked
                .map(|p| p.display().to_string())
                .into_iter()
                .collect();
            return Some((200, json!({"paths": paths})));
        }
        // "Save as" for `/save` (patches/0008): Windows' own dialog, then the file written here.
        if (method, path) == ("POST", "/api/hermes-rs/save-file") {
            let body: Value = serde_json::from_slice(body).unwrap_or_default();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let title = body["title"].as_str().unwrap_or_default().to_string();
            let default_path = body["defaultPath"].as_str().unwrap_or_default().to_string();
            let filters: Vec<(String, Vec<String>)> = body["filters"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    let name = f["name"].as_str().unwrap_or("File").to_string();
                    let extensions = f["extensions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    (name, extensions)
                })
                .collect();
            let saved = tokio::task::spawn_blocking(move || {
                let mut dialog = rfd::FileDialog::new();
                if !title.is_empty() {
                    dialog = dialog.set_title(&title);
                }
                let default = Path::new(&default_path);
                if let Some(name) = default.file_name() {
                    dialog = dialog.set_file_name(name.to_string_lossy());
                }
                if let Some(dir) = default.parent().filter(|d| d.is_dir()) {
                    dialog = dialog.set_directory(dir);
                }
                for (name, extensions) in &filters {
                    dialog = dialog.add_filter(name, extensions);
                }
                let path = dialog.save_file()?;
                Some(std::fs::write(&path, content).map(|()| path))
            })
            .await
            .ok()
            .flatten();
            return Some((
                200,
                match saved {
                    None => json!({"saved": false}),
                    Some(Ok(path)) => json!({"saved": true, "path": path.display().to_string()}),
                    Some(Err(error)) => json!({"saved": false, "error": error.to_string()}),
                },
            ));
        }
        // The file browser and the web folder picker (`/api/fs/list`).
        if (method, path) == ("GET", "/api/fs/list") {
            let folder = crate::rest::query_param(query, "path").unwrap_or_default();
            let listing = tokio::task::spawn_blocking(move || crate::paths::list_dir(&folder))
                .await
                .unwrap_or_else(|_| json!({"entries": [], "error": "read-error"}));
            return Some((200, listing));
        }
        // The coding rail and the review pane (`git_review.rs`; the folder is `path`).
        if path.starts_with("/api/git/") {
            let (method, path) = (method.to_string(), path.to_string());
            let (query, body) = (query.map(str::to_string), body.to_vec());
            return tokio::task::spawn_blocking(move || {
                crate::git_review::route(&method, &path, query.as_deref(), &body)
            })
            .await
            .ok()
            .flatten();
        }
        if (method, path) == ("GET", "/api/model/options") {
            let settings = Settings::load(&self.data_dir);
            let session = crate::rest::query_param(query, "session_id");
            return Some((200, self.model_options(&settings, session.as_deref()).await));
        }
        if let Some(id) = path
            .strip_prefix("/api/sessions/")
            .and_then(|rest| rest.strip_suffix("/messages"))
        {
            if method == "GET" {
                let (id, query) = (crate::rest::percent_decode(id), query.map(str::to_string));
                return self
                    .rest_blocking(move |g| Some(g.stored_messages(&id, query.as_deref())))
                    .await;
            }
        }
        if let Some(id) = path.strip_prefix("/api/sessions/") {
            let id = crate::rest::percent_decode(id);
            if !id.contains('/') && id != "search" {
                return self.session_route(method, &id, body).await;
            }
        }
        // The generic routes read and write the settings and other small files: off the async
        // workers.
        let (method, path, query, body) = (
            method.to_string(),
            path.to_string(),
            query.map(str::to_string),
            body.to_vec(),
        );
        self.rest_blocking(move |g| g.rest.handle(&method, &path, query.as_deref(), &body))
            .await
    }

    /// `/api/sessions/{id}`: a session's row, its sidebar flags (`PATCH`), deleting it.
    pub(super) async fn session_route(
        self: &Arc<Self>,
        method: &str,
        id: &str,
        body: &[u8],
    ) -> Option<(u16, Value)> {
        // The store and the archive file: off the async workers (the delete goes through the
        // runtime, which does its own).
        let (method, session, body) = (method.to_string(), id.to_string(), body.to_vec());
        if method == "DELETE" {
            let looked_up = session.clone();
            let exists = match self
                .blocking(move |g| {
                    g.runtime
                        .store()
                        .session(&looked_up)
                        .ok()
                        .flatten()
                        .is_some()
                })
                .await
            {
                Ok(exists) => exists,
                Err(error) => return Some((500, json!({"detail": error.message()}))),
            };
            if !exists {
                return Some((404, json!({"detail": format!("Session {id} not found")})));
            }
            return match self.runtime.delete_session(id).await {
                Ok(()) => {
                    let _ = self
                        .blocking(move |g| g.core.archive().forget(&session))
                        .await;
                    Some((200, json!({"ok": true})))
                }
                Err(error) => Some((500, json!({"detail": format!("{error:#}")}))),
            };
        }
        self.rest_blocking(move |g| {
            let id = session.as_str();
            let store = g.runtime.store();
            let Some(record) = store.session(id).ok().flatten() else {
                return Some((404, json!({"detail": format!("Session {id} not found")})));
            };
            match method.as_str() {
                "GET" => {
                    let count = store.message_count(id).unwrap_or(0);
                    Some((200, g.session_row(&g.core.summary(record, count))))
                }
                "PATCH" => {
                    let changes: Value = serde_json::from_slice(&body).unwrap_or_default();
                    if let Some(archived) = changes["archived"].as_bool()
                        && let Err(error) = g.set_archived(id, archived)
                    {
                        return Some((500, json!({"detail": error.message()})));
                    }
                    if let Some(pinned) = changes["pinned"].as_bool() {
                        if let Err(error) = store.set_pinned(id, pinned) {
                            return Some((500, json!({"detail": format!("{error:#}")})));
                        }
                    }
                    if let Some(title) = changes["title"].as_str() {
                        let _ = store.set_title(id, title.trim());
                    }
                    // `unread` is the page's own bookkeeping; nothing to keep for it.
                    Some((200, json!({"ok": true})))
                }
                _ => None,
            }
        })
        .await
    }

    /// `/api/sessions/{id}/messages`: the stored transcript, a page of it (`SessionMessagesResponse`).
    pub(super) fn stored_messages(&self, id: &str, query: Option<&str>) -> (u16, Value) {
        let record = self.runtime.store().session(id).ok().flatten();
        let messages = match self.runtime.messages(id) {
            Ok(messages) => transcript::messages(
                &messages,
                &record.as_ref().map_or("", |r| r.provider.as_str()),
                &record.as_ref().map_or("", |r| r.model.as_str()),
            ),
            Err(error) => return (404, json!({"detail": format!("{error:#}")})),
        };
        let param = |name: &str| {
            crate::rest::query_param(query, name).and_then(|v| v.parse::<usize>().ok())
        };
        let limit = param("limit").unwrap_or(messages.len().max(1));
        let offset = param("offset").unwrap_or(0);
        let latest = crate::rest::query_param(query, "order").as_deref() == Some("latest");
        // `latest` pages count back from the newest message; the page still reads oldest first.
        let (start, end) = if latest {
            let end = messages.len().saturating_sub(offset);
            (end.saturating_sub(limit), end)
        } else {
            (
                offset.min(messages.len()),
                (offset + limit).min(messages.len()),
            )
        };
        let page: Vec<Value> = messages[start..end]
            .iter()
            .map(|m| {
                let mut row = m.clone();
                row["content"] = m["text"].clone();
                row
            })
            .collect();
        (
            200,
            json!({
                "session_id": id,
                "profile": "default",
                "pagination": {
                    "limit": limit,
                    "offset": offset,
                    "order": if latest { "latest" } else { "oldest" },
                    "returned": page.len(),
                },
                "messages": page,
            }),
        )
    }

    /// One session as the sidebar lists it (`SessionInfo`). `message_count` must be true: the page
    /// treats a resume that returns no messages for a row it counted as a lost transcript.
    pub(super) fn session_row(&self, s: &hermes_core::SessionSummary) -> Value {
        let record = &s.record;
        json!({
            "id": record.id,
            "title": record.title,
            "cwd": record.cwd,
            "model": record.model,
            "source": "desktop",
            "preview": record.title,
            "started_at": record.created_at as f64 / 1000.,
            "last_active": record.updated_at as f64 / 1000.,
            "ended_at": null,
            "message_count": s.message_count,
            "input_tokens": 0,
            "output_tokens": 0,
            "is_active": s.running,
            "pinned": record.pinned,
            "archived": s.archived,
            "profile": "default",
            // The git identity a move recorded (the sidebar groups by the repo root); a session
            // that never moved carries none and groups by its cwd.
            "git_branch": s.git_branch,
            "git_repo_root": s.git_repo_root,
            // A branch is listed under the session it came from.
            "parent_session_id": s.parent,
        })
    }

    /// Every session the sidebar's project tree shows (archived ones aren't), newest first.
    pub(super) fn session_rows(&self) -> Vec<Value> {
        self.core
            .sessions(crate::archive::Filter::Exclude)
            .iter()
            .map(|s| self.session_row(s))
            .collect()
    }

    /// `/api/profiles/sessions`: the sidebar's list (`PaginatedSessions`).
    pub(super) fn session_list(&self, query: Option<&str>) -> Value {
        let param = |name: &str| {
            crate::rest::query_param(query, name).and_then(|v| v.parse::<usize>().ok())
        };
        let limit = param("limit").unwrap_or(50);
        let offset = param("offset").unwrap_or(0);
        // `archived=exclude` (the sidebar, default), `only` (Settings ▸ Sessions), `include`.
        let filter =
            crate::archive::Filter::parse(crate::rest::query_param(query, "archived").as_deref());
        let all = self.core.sessions(filter);
        let sessions: Vec<Value> = all
            .iter()
            .skip(offset)
            .take(limit)
            .map(|s| self.session_row(s))
            .collect();
        json!({
            "sessions": sessions,
            "total": all.len(),
            "limit": limit,
            "offset": offset,
            "profile_totals": {"default": all.len()},
        })
    }
}
