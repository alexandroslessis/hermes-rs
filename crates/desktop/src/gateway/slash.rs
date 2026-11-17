//! Slash commands and completion: `commands.catalog`, `complete.slash`, `complete.path`,
//! `slash.exec`, `command.dispatch` (the logic is in `commands.rs`; the commands that need a
//! session, the model or the store are here). Hermes: `tui_gateway/server.py` (`slash.exec`,
//! `command.dispatch`, `complete.*`), `hermes_cli/commands.py`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::Gateway;
use super::rpc::{CommandDispatch, CompletePath, CompleteSlash, Reply, RpcError, SlashExec};
use super::*;
use crate::commands;
use crate::providers::Settings;

impl Gateway {
    /// The skills are read from their folders: off the async workers.
    pub(super) async fn commands_catalog(self: &Arc<Self>) -> Reply {
        Ok(self
            .blocking(|g| commands::catalog(&g.runtime.skills()))
            .await?)
    }

    pub(super) async fn complete_slash(self: &Arc<Self>, p: CompleteSlash) -> Reply {
        let text = p.text;
        Ok(self
            .blocking(move |g| commands::complete(&g.runtime.skills(), &text))
            .await?)
    }

    pub(super) async fn complete_path(self: &Arc<Self>, p: CompletePath) -> Reply {
        let word = p.word;
        let (cwd, session) = (p.cwd.map(PathBuf::from), p.session_id);
        // The session's folder is read from the store and a name search walks the
        // project: off the async workers.
        let items = self
            .blocking(move |g| {
                let root = cwd
                    .or_else(|| {
                        let record = g
                            .runtime
                            .store()
                            .session(session.as_deref()?)
                            .ok()
                            .flatten()?;
                        Some(PathBuf::from(record.cwd))
                    })
                    .or_else(dirs::home_dir)
                    .unwrap_or_default();
                crate::paths::complete(&word, &root)
            })
            .await?;
        Ok(json!({"items": items}))
    }

    /// Hermes's wording for "not a command here": the page then tries `command.dispatch`, and
    /// reports the slash.exec error when that finds nothing either.
    pub(super) async fn slash_exec(self: &Arc<Self>, p: SlashExec, settings: &Settings) -> Reply {
        // The page polls each session's goal; there are no goals here.
        if p.command
            .as_deref()
            .is_some_and(|c| c.trim_start_matches('/').starts_with("goal"))
        {
            return Ok(json!({"type": "exec", "output": ""}));
        }
        let command = p.command.as_deref().unwrap_or_default();
        let (name, arg) = command
            .trim()
            .trim_start_matches('/')
            .split_once(char::is_whitespace)
            .unwrap_or((command.trim().trim_start_matches('/'), ""));
        let (raw, n, a) = (p.raw.clone(), name.to_string(), arg.to_string());
        if let Some(answer) = self
            .blocking(move |g| g.coding_command(&raw, &n, &a))
            .await?
        {
            return answer;
        }
        if let Some(answer) = self.builtin_command(&p.raw, command, settings).await {
            return answer;
        }
        let command = command.to_string();
        self.blocking(move |g| commands::exec(&g.runtime, &command))
            .await?
            .ok_or(RpcError::server("not a quick/plugin/bundle/skill command"))
    }

    pub(super) async fn command_dispatch(self: &Arc<Self>, p: CommandDispatch) -> Reply {
        let (raw, name, arg) = (p.raw, p.name, p.arg);
        // A coding command (`/plan`...) or a skill's: both read the session or the
        // skills folder, so off the async workers.
        self.blocking(move |g| {
            // (`coding_command` answers `None` for any other name; asked once with an
            // empty argument first, as before, then with the real one.)
            if g.coding_command(&raw, &name, "").is_some() {
                return g
                    .coding_command(&raw, &name, &arg)
                    .expect("a coding command");
            }
            commands::dispatch(&g.runtime, &name, &arg)
                .ok_or(RpcError::server("not a quick/plugin/bundle/skill command"))
        })
        .await?
    }
}

impl Gateway {
    /// The `/review` reviewer's goal and briefing: the session's last user and assistant messages
    /// (their text), and the skills it loaded (`snapshot_recent_messages`,
    /// `collect_parent_loaded_skills`).
    pub(super) fn review_task(&self, id: &str, instructions: &str) -> (String, String) {
        use hermes_llm::conversation::message::MessageContent;
        let messages = self.runtime.messages(id).unwrap_or_default();
        let mut recent: Vec<(bool, String)> = messages
            .iter()
            .rev()
            .filter(|m| m.is_user_visible())
            .filter_map(|m| {
                let is_user = m.role == rmcp::model::Role::User;
                let text: Vec<&str> = m
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        MessageContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let text = text.join("\n");
                // What the user wrote, without the skills the local index attached.
                let text = if is_user {
                    hermes_runtime::preinject::strip(m, text)
                } else {
                    text
                };
                let text = text.trim().to_string();
                // A command's prompt is shown as the command it was typed as.
                let text = commands::typed_command(&text).unwrap_or(text);
                (!text.is_empty()).then_some((is_user, text))
            })
            .take(hermes_prompts::coding::REVIEW_CONTEXT_MESSAGES)
            .collect();
        recent.reverse();
        let mut skills: Vec<String> = Vec::new();
        // The skills the local index attached count as loaded (Phase 5: no `skill_view` call).
        for message in &messages {
            if let Some(names) = message
                .metadata
                .operation_note(hermes_runtime::preinject::NOTE, "skills")
                .and_then(Value::as_array)
            {
                for name in names.iter().filter_map(Value::as_str) {
                    if !skills.iter().any(|s| s == name) && skills.len() < 8 {
                        skills.push(name.to_string());
                    }
                }
            }
        }
        for request in messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(MessageContent::as_tool_request)
        {
            let Ok(call) = &request.tool_call else {
                continue;
            };
            let args = call.arguments.clone().unwrap_or_default();
            // Whole-skill loads only (a reference file is a detail of the skill).
            if call.name == "skill_view" && args.get("file_path").is_none() {
                if let Some(name) = args.get("name").and_then(Value::as_str) {
                    if !skills.iter().any(|s| s == name) && skills.len() < 8 {
                        skills.push(name.to_string());
                    }
                }
            }
        }
        hermes_prompts::coding::review_task(&recent, instructions, &skills)
    }

    /// `/plan`, `/init`, `/learn`, `/review` (`commands::coding_command`): `None` for another
    /// command. `/init` writes the session's folder's AGENTS.md; `/review` needs a conversation.
    pub(super) fn coding_command(&self, params: &Value, name: &str, arg: &str) -> Option<Reply> {
        let name = name.trim_start_matches('/');
        if !matches!(name, "plan" | "init" | "learn" | "review") {
            return None;
        }
        let record = session_id(params)
            .ok()
            .and_then(|id| self.runtime.store().session(&id).ok().flatten());
        let cwd = record.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
        if name == "review"
            && record
                .as_ref()
                .is_none_or(|r| self.runtime.messages(&r.id).map_or(true, |m| m.is_empty()))
        {
            return Some(Ok(json!({"type": "exec",
                "output": "Nothing to review yet — send a message first."})));
        }
        if name == "init" && cwd.is_empty() {
            return Some(Err(RpcError::invalid(
                "/init needs a session with a folder",
            )));
        }
        let existing = (name == "init")
            .then(|| std::fs::read_to_string(std::path::Path::new(&cwd).join("AGENTS.md")).ok())
            .flatten()
            .map(|text| text.trim_start_matches('\u{feff}').to_string());
        commands::coding_command(name, arg, &cwd, existing.as_deref()).map(Ok)
    }

    /// Hermes's built-in commands that report on the session (`slash.exec` output, shown as a
    /// note in the chat): `/status`, `/context`, `/usage`, `/insights`, `/diff`, `/tools`,
    /// `/memory`, and `/refine`, which starts a review. `None` for anything else (skills).
    /// Hermes: `hermes_cli/commands.py`, `cli_commands_mixin.py`.
    pub(super) async fn builtin_command(
        self: &Arc<Self>,
        params: &Value,
        command: &str,
        settings: &Settings,
    ) -> Option<Reply> {
        let command = command.trim().trim_start_matches('/');
        let (name, arg) = command
            .split_once(char::is_whitespace)
            .map(|(n, a)| (n, a.trim()))
            .unwrap_or((command, ""));
        let id = session_id(params).ok();
        let error = |e: anyhow::Error| RpcError::server(format!("{e:#}"));
        let need_id = || {
            id.clone()
                .ok_or(RpcError::invalid("session_id is required"))
        };
        let output: Result<String, RpcError> = match name {
            // Every session of the original Hermes not imported yet (`hermes_import.rs`).
            "import-hermes" => match self.import_route(settings) {
                Ok((provider, model)) => {
                    let runtime = self.runtime.clone();
                    let data_dir = self.data_dir.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        let sources = crate::hermes_import::sources();
                        let target = crate::hermes_import::Target {
                            runtime: &runtime,
                            provider,
                            model,
                            data_dir: &data_dir,
                        };
                        (sources, crate::hermes_import::import_all(&target))
                    })
                    .await;
                    match found {
                        Ok((sources, _)) if sources.is_empty() => Ok(
                            "No Hermes data found here (looked in HERMES_HOME, %LOCALAPPDATA%\\hermes and ~\\.hermes)."
                                .to_string(),
                        ),
                        Ok((sources, (imported, already, failed))) => {
                            let mut text = format!(
                                "Imported {imported} session{} from Hermes ({} database{}); {already} already here.",
                                if imported == 1 { "" } else { "s" },
                                sources.len(),
                                if sources.len() == 1 { "" } else { "s" },
                            );
                            if !failed.is_empty() {
                                text.push_str(&format!("\nNot imported ({}):\n{}", failed.len(), failed.join("\n")));
                            }
                            Ok(text)
                        }
                        Err(e) => Err(RpcError::server(e.to_string())),
                    }
                }
                Err(e) => Err(e),
            },
            "status" => match need_id() {
                Ok(id) => self.status_text(&id).await,
                Err(e) => Err(e),
            },
            "context" | "ctx" => match need_id() {
                Ok(id) => self.context_text(&id).await,
                Err(e) => Err(e),
            },
            "usage" => match need_id() {
                Ok(id) => {
                    let window = self.window_for(&id).await;
                    self.blocking(move |g| g.runtime.usage_text(&id, window))
                        .await
                        .and_then(|text| text.map_err(error))
                }
                Err(e) => Err(e),
            },
            // These read the store and run git: off the async workers.
            "insights" => {
                let arg = arg.to_string();
                self.blocking(move |g| g.runtime.insights_text(&arg)).await
            }
            "diff" => match need_id() {
                Ok(id) => {
                    let arg = arg.to_string();
                    self.blocking(move |g| g.runtime.diff_report(&id, &arg))
                        .await
                        .and_then(|report| report.map_err(error))
                }
                Err(e) => Err(e),
            },
            "tools" => {
                let (builtin, mcp) = self.runtime.tool_definitions();
                let line = |t: &Value| {
                    let description = t["description"].as_str().unwrap_or_default();
                    let first = description.split(['.', '\n']).next().unwrap_or_default();
                    format!(
                        "  {}  {}",
                        t["name"].as_str().unwrap_or_default(),
                        first.trim()
                    )
                };
                let mut out = vec![format!("Tools ({}):", builtin.len())];
                out.extend(builtin.iter().map(line));
                if !mcp.is_empty() {
                    out.push(format!("\nMCP tools ({}):", mcp.len()));
                    out.extend(mcp.iter().map(line));
                }
                Ok(out.join("\n"))
            }
            "memory" => {
                let runtime = self.runtime.clone();
                // Both lists are files: off the async workers.
                let (pending, gate, staged) = tokio::task::spawn_blocking(move || {
                    let memory = runtime.memory();
                    let gate = if memory.config().write_approval {
                        "on"
                    } else {
                        "off"
                    };
                    // Since Phase 6 the agent's own writes wait in the staging area (reviewed in
                    // Settings ▸ Memory & Context); the old queue holds older and review items.
                    let staged = runtime
                        .staging()
                        .list()
                        .iter()
                        .filter(|item| item.kind == hermes_tools::staging::Kind::Memory)
                        .count();
                    (memory.pending(), gate, staged)
                })
                .await
                .unwrap_or_default();
                let review = (staged > 0).then(|| {
                    format!(
                        "{staged} memory change(s) wait for your review in Settings ▸ Memory & \
                         Context."
                    )
                });
                let mut out = if pending.is_empty() {
                    if review.is_none() {
                        vec![format!(
                            "No memory writes are waiting for approval (approval gate: {gate})."
                        )]
                    } else {
                        vec![format!("Approval gate: {gate}.")]
                    }
                } else {
                    let mut out = vec![format!(
                        "Memory writes waiting for approval (gate: {gate}):"
                    )];
                    out.extend(pending.iter().map(|w| format!("  {}  {}", w.id, w.summary)));
                    out
                };
                out.extend(review);
                Ok(out.join("\n"))
            }
            "refine" => {
                match need_id().and_then(|id| Ok((self.provider_for(&id, settings)?, id))) {
                    Ok((provider, id)) => {
                        let focus = (!arg.is_empty()).then(|| arg.to_string());
                        self.runtime
                        .refine(&id, &provider, self.turn_options(&id), focus)
                        .map(|()| {
                            "Reviewing this conversation for lessons to save to memory and skills; \
                             the review runs in the background."
                                .to_string()
                        })
                        .map_err(error)
                    }
                    Err(e) => Err(e),
                }
            }
            _ => return None,
        };
        Some(output.map(|output| json!({"type": "exec", "output": output})))
    }
}
