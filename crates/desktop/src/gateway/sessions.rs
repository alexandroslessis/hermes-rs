//! Session calls: create, resume, title, delete, status, branch and the Import screen's foreign
//! sessions. Hermes: `tui_gateway/methods_session.py`, `tui_gateway/server.py`.

use std::sync::Arc;

use serde_json::{Value, json};

use super::rpc::{ForeignId, ForeignList, Reply, RpcError, SessionRef, SessionTitle};
use super::*;
use super::{Gateway, session_id};
use crate::hub::Outbox;
use crate::providers::Settings;

impl Gateway {
    pub(super) async fn session_status(self: &Arc<Self>, p: SessionRef) -> Reply {
        let id = p.id()?;
        let output = self.status_text(&id).await?;
        Ok(json!({"output": output}))
    }

    pub(super) async fn session_title(self: &Arc<Self>, p: SessionTitle) -> Reply {
        let id = p.session.id()?;
        let new_title = p.title;
        let session = id.clone();
        let title = self
            .blocking(move |g| {
                if let Some(title) = &new_title {
                    g.rename(&session, title)?;
                }
                Ok::<_, RpcError>(
                    g.runtime
                        .store()
                        .session(&session)
                        .ok()
                        .flatten()
                        .and_then(|record| record.title)
                        .unwrap_or_default(),
                )
            })
            .await??;
        Ok(json!({"title": title, "session_key": id}))
    }

    pub(super) async fn session_delete(self: &Arc<Self>, p: SessionRef) -> Reply {
        let id = p.id()?;
        self.runtime
            .delete_session(&id)
            .await
            .map_err(RpcError::failed)?;
        self.core
            .forget_archived(&id)
            .await
            .map_err(|e| RpcError::server(format!("The request stopped: {e:#}")))?;
        Ok(json!({"deleted": id}))
    }

    /// The Import screen: sessions from the original Hermes (`hermes_import.rs`).
    pub(super) async fn foreign_list(self: &Arc<Self>, p: ForeignList) -> Reply {
        let (source, offset) = (p.source, p.offset);
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            crate::hermes_import::list(&data_dir, source.as_deref(), offset)
        })
        .await
        .map_err(|e| RpcError::server(e.to_string()))
    }

    pub(super) async fn foreign_preview(self: &Arc<Self>, p: ForeignId) -> Reply {
        let id = p.id;
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || crate::hermes_import::preview(&data_dir, &id))
            .await
            .map_err(|e| RpcError::server(e.to_string()))?
            .map_err(RpcError::invalid)
    }

    pub(super) async fn foreign_import(
        self: &Arc<Self>,
        p: ForeignId,
        settings: &Settings,
    ) -> Reply {
        let id = p.id;
        let (provider, model) = self.import_route(settings)?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let target = crate::hermes_import::Target {
                runtime: &this.runtime,
                provider,
                model,
                data_dir: &this.data_dir,
            };
            crate::hermes_import::import(&target, &id)
        })
        .await
        .map_err(|e| RpcError::server(e.to_string()))?
        .map_err(RpcError::invalid)
    }

    /// `/branch` and the message menu's Branch (`branch.rs`).
    pub(super) async fn session_branch(
        self: &Arc<Self>,
        params: Value,
        settings: &Settings,
        outbox: &Outbox,
        whole: bool,
    ) -> Reply {
        let (s, o) = (settings.clone(), outbox.clone());
        self.blocking(move |g| g.branch(&params, &s, &o, whole, whole))
            .await?
    }

    /// Creating builds the system prompt (skills, memory, files) and writes the store.
    pub(super) async fn session_create(
        self: &Arc<Self>,
        params: Value,
        settings: &Settings,
        outbox: &Outbox,
    ) -> Reply {
        let (s, o) = (settings.clone(), outbox.clone());
        self.blocking(move |g| g.create_session(&params, &s, &o))
            .await?
    }

    /// Resuming loads the whole conversation. Only `session.resume` is the page showing the
    /// session (`session.history` reads it).
    pub(super) async fn session_resume(
        self: &Arc<Self>,
        params: Value,
        outbox: &Outbox,
        shown: bool,
    ) -> Reply {
        let (p, o) = (params.clone(), outbox.clone());
        let resumed = self.blocking(move |g| g.resume_session(&p, &o)).await?;
        if shown
            && resumed.is_ok()
            && let Ok(id) = session_id(&params)
        {
            self.viewing(&id, outbox);
        }
        resumed
    }
}

impl Gateway {
    /// The page on `outbox` now shows session `id`: the one it showed before was switched away
    /// from (under `idle_once` its review may run now), and `id` is active again (its review
    /// waits for the idle time). A page that only reconnects shows the same id: nothing is left.
    pub(super) fn viewing(&self, id: &str, outbox: &Outbox) {
        let left = {
            let Ok(mut viewing) = self.viewing.lock() else {
                return;
            };
            viewing.retain(|(page, _)| !page.is_closed());
            let left = match viewing
                .iter_mut()
                .find(|(page, _)| page.same_channel(outbox))
            {
                Some((_, shown)) if shown != id => Some(std::mem::replace(shown, id.to_string())),
                Some(_) => None,
                None => {
                    viewing.push((outbox.clone(), id.to_string()));
                    None
                }
            };
            // Another window that still shows it hasn't left it.
            left.filter(|left| !viewing.iter().any(|(_, shown)| shown == left))
        };
        if let Some(left) = left {
            self.runtime.note_session_left(&left);
        }
        self.runtime.note_session_viewed(id);
    }

    pub(super) fn active_sessions(&self) -> Vec<Value> {
        self.core
            .live_sessions()
            .into_iter()
            .map(|s| {
                // The page holds a session busy only while it's listed as `working` (or
                // `waiting`, on the user: a card is open); any other status idles it on the next
                // poll, which blinked Stop and Steer every 1.5 s mid-turn.
                let status = if self.core.is_waiting(&s.id) {
                    "waiting"
                } else {
                    "working"
                };
                json!({
                    "id": s.id,
                    "session_key": s.id,
                    "current": false,
                    "title": s.title.unwrap_or_default(),
                    "preview": "",
                    "model": s.model,
                    "status": status,
                    "message_count": 0,
                    "started_at": s.created_at as f64 / 1000.,
                    "last_active": s.updated_at as f64 / 1000.,
                })
            })
            .collect()
    }

    /// The context gauge (`session.context_breakdown`, Hermes's `agent/context_breakdown.py`):
    /// the frozen system prompt and the conversation, estimated at ~4 characters a token, against
    /// the model's window. How full it is comes from the last request's real input tokens when the
    /// session has made one, else from the estimate.
    pub(super) async fn context_breakdown(self: &Arc<Self>, id: &str) -> Reply {
        // The session row and its whole conversation: store reads, off the async workers.
        let session = id.to_string();
        let (record, messages, (builtin, mcp)) = self
            .blocking(move |g| {
                let record = g
                    .runtime
                    .store()
                    .session(&session)
                    .ok()
                    .flatten()
                    .ok_or(RpcError::server(format!("Session not found: {session}")))?;
                let messages = g.runtime.messages(&session).unwrap_or_default();
                // Every request also carries the tool definitions: the built-in tools and MCP
                // servers' (a deferred session's, Phase 11: the ones it actually sends).
                let tools = g.runtime.session_tool_definitions(&session);
                Ok::<_, RpcError>((record, messages, tools))
            })
            .await??;
        let tokens = |chars: usize| chars.div_ceil(4);
        let system = tokens(record.system_prompt.len());
        let conversation = tokens(serde_json::to_string(&messages).map_or(0, |json| json.len()));
        let json_tokens = |tools: &[Value]| {
            if tools.is_empty() {
                0
            } else {
                tokens(serde_json::to_string(tools).map_or(0, |json| json.len()))
            }
        };
        let (tool_definitions, mcp_tools) = (json_tokens(&builtin), json_tokens(&mcp));
        let estimated_total = system + tool_definitions + mcp_tools + conversation;
        let context_max = self.core.context_length(&record.provider, &record.model).await;
        let session = id.to_string();
        let measured = self
            .blocking(move |g| g.runtime.store().usage_totals(&session))
            .await?
            .map_or(0, |usage| usage.last_input_tokens.max(0) as usize);
        let (mut used, source) = if measured > 0 {
            (measured, "provider_usage")
        } else {
            (estimated_total, "local_estimate")
        };
        if context_max > 0 {
            used = used.min(context_max);
        }
        let category = |id: &str, label: &str, color: &str, tokens: usize| json!({"id": id, "label": label, "color": color, "tokens": tokens});
        // Hermes's order; an empty category is left out, except the conversation.
        let categories: Vec<Value> = [
            (
                "system_prompt",
                "System prompt",
                "var(--context-usage-system)",
                system,
            ),
            (
                "tool_definitions",
                "Tool definitions",
                "var(--context-usage-tools)",
                tool_definitions,
            ),
            ("mcp", "MCP", "var(--context-usage-mcp)", mcp_tools),
            (
                "conversation",
                "Conversation",
                "var(--context-usage-conversation)",
                conversation,
            ),
        ]
        .into_iter()
        .filter(|(id, _, _, tokens)| *tokens > 0 || *id == "conversation")
        .map(|(id, label, color, tokens)| category(id, label, color, tokens))
        .collect();
        Ok(json!({
            "categories": categories,
            "estimated_total": estimated_total,
            "context_max": context_max,
            "context_used": used,
            "context_percent": if context_max > 0 { (used * 100 / context_max).min(100) } else { 0 },
            "context_source": source,
            "context_estimated": source != "provider_usage",
            "model": record.model,
        }))
    }

    /// A model picked while the last turn ran, applied now that it has finished.
    pub(super) fn apply_pending_route(&self, id: &str) {
        if self.core.apply_pending_route(id) {
            self.emit(id, "session.info", self.session_info(id));
        }
    }

    /// Hermes's canonical `session.mutate`: a session edit presenting the revision the page last
    /// saw. Edits here are one user's, so the revision is reported but not enforced.
    pub(super) async fn mutate(self: &Arc<Self>, params: &Value, settings: &Settings) -> Reply {
        let id = session_id(params)?;
        let payload = &params["payload"];
        let mut receipt = match params["operation"].as_str().unwrap_or_default() {
            "rename" => {
                let title = payload["title"].as_str().unwrap_or_default().to_string();
                let (session, new_title) = (id.clone(), title.clone());
                self.blocking(move |g| g.rename(&session, &new_title))
                    .await??;
                json!({"title": title.trim()})
            }
            "model" => {
                let model = payload["model"].as_str().unwrap_or_default();
                let provider = payload["provider"].as_str().unwrap_or_default();
                // The canonical pick is session policy: it never rewrites the defaults.
                // Picked mid-reply, it waits for the next turn, but shows as the session's model
                // at once (no "applies next turn" notice).
                let pick = json!({"session_id": id, "value": format!("{model} --provider {provider} --session")});
                let settings = settings.clone();
                self.blocking(move |g| g.switch_model(&pick, &settings))
                    .await??;
                json!({"model": model, "provider": provider, "status": "applied"})
            }
            "reasoning" => {
                let effort = payload["effort"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                self.core.pick_effort(&id, Some(effort.clone()));
                self.emit(&id, "session.info", self.session_info(&id));
                json!({"effort": effort, "status": "applied"})
            }
            // Restore / edit: drop the named user row and everything after it (the page then
            // submits the text again).
            "rewind" => {
                let row = payload["target_message_id"]
                    .as_u64()
                    .ok_or(RpcError::invalid("target_message_id is required"))?;
                if self.runtime.is_running(&id) {
                    return Err(RpcError::Busy);
                }
                let session = id.clone();
                let kept = self
                    .blocking(move |g| {
                        g.runtime
                            .rewind(&session, transcript::kept_before_row(row as usize))
                    })
                    .await?
                    .map_err(|e| RpcError::server(format!("{e:#}")))?;
                json!({"status": "applied", "target_message_id": row, "message_count": kept.len()})
            }
            // Move to project (`session.workspace.move`, and the composer's folder picker on an
            // open chat — both become this mutation in the page's canonical protocol): the stored
            // cwd and git identity move together (`workspace.rs`), idle sessions only. The live
            // state is dropped so the next turn's tools and shell start in the new folder.
            "workspace" => {
                let requested = payload["cwd"]
                    .as_str()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .ok_or(RpcError::invalid("cwd is required"))?;
                // `~` and `file:` links are the page's spellings.
                let folder = crate::files::resolve(requested, None);
                let (cwd, branch, root) =
                    self.core
                        .move_session(&id, folder)
                        .await
                        .map_err(|e| match e {
                            crate::workspace::MoveError::InvalidFolder(folder) => {
                                RpcError::invalid(format!("No such folder: {folder}"))
                            }
                            crate::workspace::MoveError::Store(message) => {
                                RpcError::server(message)
                            }
                            crate::workspace::MoveError::Busy => RpcError::Busy,
                            crate::workspace::MoveError::Stopped(e) => {
                                RpcError::server(format!("the move failed: {e}"))
                            }
                        })?;
                self.emit(&id, "session.info", self.session_info(&id));
                json!({"cwd": cwd, "branch": branch, "git_repo_root": root})
            }
            // The keyboard shortcut's `session.archive` (`canonical-protocol.ts`).
            "archive" => {
                let archived = payload["archived"].as_bool().unwrap_or(true);
                let session = id.clone();
                self.blocking(move |g| g.set_archived(&session, archived))
                    .await??;
                json!({"archived": archived})
            }
            other => {
                return Err(RpcError::server(format!(
                    "{other} isn't available in hermes-rs yet"
                )));
            }
        };
        receipt["session_id"] = json!(id);
        receipt["revision"] = json!(self.bump_revision(&id));
        receipt["execution_generation"] = json!(EXECUTION_GENERATION);
        Ok(receipt)
    }

    /// `session.rewind` (Desktop's typed `/undo` and `/retry`, `slash-session-commands.ts`): drop
    /// the named user row and everything after it, answering with the dropped prompt, which `/undo`
    /// puts back in the composer and `/retry` sends again. `replay_only` asks for a rewind whose
    /// reply was lost; this backend answers every rewind, so there is none.
    pub(super) fn rewind_turn(&self, params: &Value) -> Reply {
        let id = session_id(params)?;
        let nothing = || json!({"session_id": id, "status": "nothing", "rewound_count": 0});
        if params["replay_only"] == true {
            return Ok(nothing());
        }
        let Some(row) = params["target_message_id"].as_u64() else {
            return Ok(nothing());
        };
        let rewound = match self.core.rewind_to_row(&id, row as usize) {
            Ok(Some(rewound)) => rewound,
            Ok(None) => return Ok(nothing()),
            Err(hermes_core::sessions::RewindError::Busy) => {
                return Err(RpcError::server("session busy"));
            }
            Err(hermes_core::sessions::RewindError::Failed(e)) => {
                return Err(RpcError::server(format!("{e:#}")));
            }
        };
        Ok(json!({
            "session_id": id,
            "status": "applied",
            "rewound_count": rewound.removed,
            "target_message": {"id": row, "content": rewound.typed},
            "revision": self.bump_revision(&id),
            "execution_generation": EXECUTION_GENERATION,
        }))
    }

    /// `/rollback` (`slash-desktop-commands.ts`): the session folder's file checkpoints, a diff
    /// against one, restoring one (Hermes's `rollback.list|diff|restore`, from
    /// `tools/checkpoint_manager.py`). `hash` is a hash prefix or a number in the list.
    pub(super) fn rollback(&self, method: &str, params: &Value) -> Reply {
        let id = session_id(params)?;
        let error = |e: String| RpcError::server(e);
        let checkpoints = self.runtime.checkpoints();
        if !checkpoints.enabled() {
            return match method {
                "rollback.list" => Ok(json!({"enabled": false, "checkpoints": []})),
                _ => Err(error(
                    "Checkpoints are off: Settings ▸ Safety ▸ Checkpoints.".into(),
                )),
            };
        }
        let workdir = self
            .runtime
            .checkpoint_dir(&id)
            .map_err(|e| error(format!("{e:#}")))?;
        let list = checkpoints.list(&workdir);
        if method == "rollback.list" {
            let rows: Vec<Value> = list
                .iter()
                .map(|c| json!({"hash": c.hash, "timestamp": c.timestamp, "message": c.reason}))
                .collect();
            return Ok(json!({"enabled": true, "checkpoints": rows}));
        }
        let wanted = params["hash"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let hash = match wanted.parse::<usize>() {
            Ok(n) if (1..=list.len()).contains(&n) => Some(list[n - 1].hash.clone()),
            _ if wanted.is_empty() => None,
            _ => list
                .iter()
                .find(|c| c.hash.starts_with(&wanted))
                .map(|c| c.hash.clone()),
        }
        .ok_or_else(|| error(format!("No checkpoint {wanted}; /rollback lists them.")))?;
        if method == "rollback.diff" {
            let diff = checkpoints.diff(&workdir, &hash).map_err(error)?;
            return Ok(json!({"stat": diff.stat, "diff": diff.diff}));
        }
        if self.runtime.is_running(&id) {
            return Err(error("session busy".into()));
        }
        let file = params["file_path"]
            .as_str()
            .filter(|f| !f.trim().is_empty());
        match self
            .runtime
            .restore_checkpoint(&id, &hash, file, params["all"] == true)
        {
            Ok((restored, removed)) => {
                let mut kept = Vec::new();
                if !restored.skipped_user_edits.is_empty() {
                    kept.push(format!(
                        "Kept your hand edits: {} (/rollback {} --all restores those too).",
                        restored.skipped_user_edits.join(", "),
                        &hash[..8.min(hash.len())]
                    ));
                }
                if !restored.skipped_oversize.is_empty() {
                    kept.push(format!(
                        "Kept (too large for checkpoints): {}.",
                        restored.skipped_oversize.join(", ")
                    ));
                }
                let reason = [vec![restored.reason.clone()], kept].concat().join("\n");
                // (After a whole-folder restore the page reloads the transcript itself.)
                Ok(json!({
                    "success": true,
                    "restored_to": restored.restored_to,
                    "reason": reason,
                    "directory": restored.directory.display().to_string(),
                    "file": restored.file,
                    "history_removed": removed,
                }))
            }
            Err(e) => Ok(json!({"success": false, "error": format!("{e:#}")})),
        }
    }

    /// `/compress [focus]` (`session.compress`): summarizes the conversation now, as Hermes's
    /// context compression does when the window fills, and answers with the shorter transcript
    /// the page shows from then on.
    pub(super) async fn compress_session(
        self: &Arc<Self>,
        params: &Value,
        settings: &Settings,
    ) -> Reply {
        let id = session_id(params)?;
        let error = |e: anyhow::Error| RpcError::server(format!("{e:#}"));
        // The row, the conversation and the size of the request: store reads, off the async
        // workers.
        let session = id.clone();
        let (record, before_messages, before_tokens) = self
            .blocking(move |g| {
                let record = g
                    .runtime
                    .store()
                    .session(&session)
                    .map_err(|e| RpcError::server(format!("{e:#}")))?
                    .ok_or(RpcError::server(format!("Session {session} not found")))?;
                let messages = g
                    .runtime
                    .messages(&session)
                    .map_err(|e| RpcError::server(format!("{e:#}")))?
                    .len();
                let tokens = g
                    .runtime
                    .context_breakdown(&session)
                    .map(|b| b.total())
                    .unwrap_or(0);
                Ok::<_, RpcError>((record, messages, tokens))
            })
            .await??;
        let provider = settings
            .provider(&record.provider)
            .or(settings.active())
            .ok_or(RpcError::server("No provider is configured"))?
            .runtime();
        let focus = params["focus_topic"]
            .as_str()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string);
        let mut turn = self
            .runtime
            .compress(&id, &provider, self.core.turn_options(&id), focus)
            .map_err(error)?;
        let mut failed = None;
        while let Some(event) = turn.events.recv().await {
            if let RuntimeEvent::TurnFinished(TurnStatus::Failed(e)) = event {
                failed = Some(e);
            }
        }
        // A conversation too short to summarize ends the runtime's compress turn with this
        // (`Runtime::compress`): nothing happened, nothing failed.
        if failed.as_deref() == Some("Nothing to compress yet.") {
            failed = None;
        }
        let session = id.clone();
        let (after, after_tokens) = self
            .blocking(move |g| {
                let after = g.runtime.messages(&session)?;
                let tokens = g
                    .runtime
                    .context_breakdown(&session)
                    .map(|b| b.total())
                    .unwrap_or(0);
                Ok::<_, anyhow::Error>((after, tokens))
            })
            .await?
            .map_err(error)?;
        let compressed = failed.is_none() && after.len() < before_messages;
        let headline = match (&failed, compressed) {
            (Some(e), _) => format!("Compression failed: {e}"),
            (None, true) => format!("Compressed: {before_messages} → {} messages", after.len()),
            (None, false) => "Nothing to compress yet.".to_string(),
        };
        self.emit(&id, "session.info", self.session_info(&id));
        Ok(json!({
            "status": if failed.is_some() { "aborted" } else if compressed { "compressed" } else { "noop" },
            "removed": before_messages.saturating_sub(after.len()),
            "before_messages": before_messages,
            "after_messages": after.len(),
            "before_tokens": before_tokens,
            "after_tokens": after_tokens,
            "messages": transcript::messages(&after, &record.provider, &record.model),
            "summary": {
                "headline": headline,
                "token_line": compressed.then(|| format!("~{before_tokens} → ~{after_tokens} tokens")),
                "aborted": failed.is_some(),
                "noop": failed.is_none() && !compressed,
            },
        }))
    }

    /// `session.export` (`/save [json|md|html] [name] [redact]`): the conversation as file content;
    /// the page saves it through a Save As dialog.
    pub(super) fn export_session(&self, params: &Value) -> Reply {
        let id = session_id(params)?;
        let format =
            hermes_core::export::Format::parse(params["format"].as_str().unwrap_or("json"))
                .ok_or(RpcError::invalid("format is json, md or html"))?;
        let store = self.runtime.store();
        let error = |e: anyhow::Error| RpcError::server(format!("{e:#}"));
        let record = store
            .session(&id)
            .map_err(error)?
            .ok_or(RpcError::server(format!("Session {id} not found")))?;
        let messages = store.load_messages(&id).map_err(error)?;
        let (content, filename) =
            crate::export::export(&record, &messages, format, params["redact"] == true);
        Ok(json!({"content": content, "filename": filename, "format": format.extension()}))
    }

    /// Renames a session; the page's lists hear it through Hermes's `session.title` event.
    /// Archives or restores a session (the sidebar, the shortcut, Settings ▸ Sessions). An
    /// archived session leaves the pins, as Hermes's does.
    pub(super) fn set_archived(&self, id: &str, archived: bool) -> Result<(), RpcError> {
        match self.core.set_archived(id, archived) {
            Ok(true) => Ok(()),
            Ok(false) => Err(RpcError::invalid(format!("No session {id}."))),
            Err(e) => Err(RpcError::server(format!("{e:#}"))),
        }
    }

    pub(super) fn rename(&self, id: &str, title: &str) -> Result<(), RpcError> {
        self.core
            .rename(id, title)
            .map_err(|e| RpcError::server(format!("{e:#}")))
    }

    /// The session's edit counter (Hermes's CAS revision), which the page presents with each edit.
    /// (Kept by the session's actor: `PageState::revision`.)
    pub(super) fn revision(&self, id: &str) -> u64 {
        self.runtime
            .existing_session_handle(id)
            .map_or(1, |handle| handle.page().revision)
    }

    pub(super) fn bump_revision(&self, id: &str) -> u64 {
        self.runtime
            .session_handle(id)
            .ok()
            .and_then(|handle| handle.bump_revision().ok())
            .unwrap_or(1)
    }

    pub(super) fn session_info(&self, id: &str) -> Value {
        let record = self.runtime.store().session(id).ok().flatten();
        let running = self.is_live(id);
        match record {
            Some(record) => json!({
                // A pick made mid-reply shows at once; it takes effect from the next turn.
                "model": self.core.pending_route(id).map_or(record.model, |(_, model)| model),
                "provider": self.core.pending_route(id).map_or(record.provider, |(provider, _)| provider),
                "cwd": record.cwd,
                // The git identity a move recorded; the sidebar groups by the repo root.
                "git_branch": self.core.workspaces().identity(id).and_then(|(branch, _)| branch),
                "git_repo_root": self.core.workspaces().identity(id).and_then(|(_, root)| root),
                "title": record.title.unwrap_or_default(),
                "stored_session_id": record.id,
                "running": running,
                "version": crate::rest::VERSION,
                // The protocol this backend speaks; the page warns "out of date" without it.
                "desktop_protocol": "hermes-gateway-v1",
                "profile_name": "default",
                "reasoning_effort": self.core.effort(id),
                "revision": self.revision(id),
                "execution_generation": EXECUTION_GENERATION,
                // The queue panel's rows: prompts waiting for the running turn.
                "pending": self.pending_rows(id),
                // The status bar's numbers for a session opened before any turn of this run.
                "usage": self.usage_snapshot(id, None),
                // The status bar's approval mode (applied from an event naming its profile).
                "approval_mode": self.rest.config_value("approvals.mode"),
            }),
            None => json!({"stored_session_id": id, "running": running}),
        }
    }

    /// `session.branch` (up to a message: `through_message_id`, else `count` of the page's
    /// rows), `session.branch_whole` and `session.branch_stored` (all of it, `whole`): a new
    /// session starting with the parent's conversation, listed under it (`branch.rs`).
    pub(super) fn branch(
        &self,
        params: &Value,
        settings: &Settings,
        outbox: &Outbox,
        whole: bool,
        omit_messages: bool,
    ) -> Reply {
        let parent = params["parent_session_id"]
            .as_str()
            .or(params["session_id"].as_str())
            .filter(|id| !id.is_empty())
            .ok_or(RpcError::invalid("session_id is required"))?
            .to_string();
        let point = if whole {
            hermes_core::BranchPoint::Whole
        } else if let Some(row) = params["through_message_id"].as_u64() {
            hermes_core::BranchPoint::ThroughRow(row as usize)
        } else if let Some(count) = params["count"].as_u64().filter(|c| *c > 0) {
            // The page's rows: through the count-th one.
            hermes_core::BranchPoint::ThroughEntry(count as usize)
        } else {
            hermes_core::BranchPoint::Whole
        };
        let request = hermes_core::BranchRequest {
            parent: parent.clone(),
            point,
            cwd: params["cwd"].as_str().map(str::to_string),
            name: params["name"].as_str().map(str::to_string),
            idempotency_key: params["idempotency_key"].as_str().map(str::to_string),
        };
        let made = self
            .core
            .branch(request, || self.session_provider(&parent, settings))
            .map_err(|e| match e {
                hermes_core::BranchError::UnknownSession(id) => {
                    RpcError::invalid(format!("No session {id}."))
                }
                hermes_core::BranchError::Empty => {
                    RpcError::invalid("nothing to branch — send a message first")
                }
                hermes_core::BranchError::Provider(error) => error,
                hermes_core::BranchError::Failed(e) => RpcError::server(format!("{e:#}")),
            })?;
        let (child, kept, record) = (made.child, made.kept, made.parent);
        self.subscribe(&child, outbox);
        let title = self
            .runtime
            .store()
            .session(&child)
            .ok()
            .flatten()
            .and_then(|r| r.title)
            .unwrap_or_default();
        let mut answer = json!({
            "session_id": child,
            "stored_session_id": child,
            "title": title,
            "parent": parent,
            "message_count": kept.len(),
            "info": self.session_info(&child),
            "revision": self.revision(&child),
            "execution_generation": EXECUTION_GENERATION,
        });
        if omit_messages {
            answer["messages_omitted"] = json!(true);
        } else {
            answer["messages"] =
                json!(transcript::messages(&kept, &record.provider, &record.model));
        }
        Ok(answer)
    }

    pub(super) fn create_session(
        &self,
        params: &Value,
        settings: &Settings,
        outbox: &Outbox,
    ) -> Reply {
        let provider_id = params["provider"]
            .as_str()
            .map(str::to_string)
            .or_else(|| settings.active().map(|p| p.id.clone()))
            .ok_or(RpcError::server("No provider is configured"))?;
        let provider = settings
            .provider(&provider_id)
            .ok_or(RpcError::server(format!("Unknown provider {provider_id}")))?;
        let model = params["model"]
            .as_str()
            .map(str::to_string)
            .or_else(|| settings.model.clone())
            .ok_or(RpcError::server("No model is chosen"))?;
        let cwd = params["cwd"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("C:/"));
        // A branch the page builds itself: the messages it starts with, and its parent.
        let seeded: Vec<hermes_llm::conversation::message::Message> = params["messages"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let text = row["content"].as_str().filter(|t| !t.trim().is_empty())?;
                        let message = match row["role"].as_str() {
                            Some("assistant") => {
                                hermes_llm::conversation::message::Message::assistant()
                                    .with_text(text)
                            }
                            _ => hermes_llm::conversation::message::Message::user().with_text(text),
                        };
                        Some(message.with_generated_id_if_missing())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (record, seeded) = self
            .core
            .create_session(hermes_core::NewSession {
                provider: provider.runtime(),
                model,
                cwd,
                title: params["title"].as_str().map(str::to_string),
                seeded,
                parent: params["parent_session_id"].as_str().map(str::to_string),
            })
            .map_err(|e| RpcError::server(format!("{e:#}")))?;
        self.viewing(&record.id, outbox);
        self.subscribe(&record.id, outbox);
        Ok(json!({
            "session_id": record.id,
            "stored_session_id": record.id,
            "message_count": seeded.len(),
            "messages": transcript::messages(&seeded, &record.provider, &record.model),
            "info": self.session_info(&record.id),
            "revision": self.revision(&record.id),
            "execution_generation": EXECUTION_GENERATION,
        }))
    }

    pub(super) fn resume_session(&self, params: &Value, outbox: &Outbox) -> Reply {
        let id = session_id(params)?;
        // Hermes's wording: the page drops an id it doesn't know to a fresh draft.
        let record = self
            .runtime
            .store()
            .session(&id)
            .ok()
            .flatten()
            .ok_or(RpcError::server(format!("Session not found: {id}")))?;
        let messages = self.runtime.messages(&id).map_err(|e| {
            tracing::warn!("resuming {id}: {e:#}");
            RpcError::server(format!("{e:#}"))
        })?;
        let transcript = transcript::messages(&messages, &record.provider, &record.model);
        self.subscribe(&id, outbox);
        Ok(json!({
            "session_id": id,
            "stored_session_id": id,
            "message_count": transcript.len(),
            "count": transcript.len(),
            "messages": transcript,
            "info": self.session_info(&id),
            "running": self.runtime.is_running(&id),
            // Questions still waiting: the page shows their cards again.
            "prompts": crate::hub::open_payloads(&self.core.open_questions(&id)),
            // The page's edit and turn-control fences.
            "revision": self.revision(&id),
            "execution_generation": EXECUTION_GENERATION,
            // Prompts still queued behind the running turn (the queue panel).
            "pending_submissions": self
                .pending_rows(&id)
                .into_iter()
                .map(|mut row| {
                    row["user"] = row["text"].clone();
                    row
                })
                .collect::<Vec<_>>(),
        }))
    }
}
