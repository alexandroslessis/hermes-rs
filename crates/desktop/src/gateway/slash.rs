//! Slash commands and completion: `commands.catalog`, `complete.slash`, `complete.path`,
//! `slash.exec`, `command.dispatch`. What the commands do is `hermes_core::commands`
//! (`Core::run_command`); the JSON is `crate::commands`; [`PageCommands`] gives the core what only
//! the page's Settings know (the model's window, the session's provider, the importer).
//! Hermes: `tui_gateway/server.py` (`slash.exec`, `command.dispatch`, `complete.*`),
//! `hermes_cli/commands.py`.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::commands::{CommandEnv, CommandError, split_command};
use serde_json::json;

use super::Gateway;
use super::rpc::{CommandDispatch, CompletePath, CompleteSlash, Reply, RpcError, SlashExec};
use super::*;
use crate::commands;
use crate::providers::Settings;

impl Gateway {
    /// The skills are read from their folders: off the async workers.
    pub(super) async fn commands_catalog(self: &Arc<Self>) -> Reply {
        self.blocking(|g| commands::catalog(&g.core.commands(), |name| g.core.skill_usage(name).1))
            .await
    }

    pub(super) async fn complete_slash(self: &Arc<Self>, p: CompleteSlash) -> Reply {
        let text = p.text;
        self.blocking(move |g| commands::completions(&g.core.complete_command(&text)))
            .await
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
        let (name, arg) = split_command(p.command.as_deref().unwrap_or_default());
        let session = session_id(&p.raw).ok();
        let env = PageCommands::new(self, settings);
        self.core
            .run_command(session.as_deref(), name, arg, &env)
            .await
            .map(commands::outcome)
            .map_err(command_error)
    }

    pub(super) async fn command_dispatch(self: &Arc<Self>, p: CommandDispatch) -> Reply {
        let (raw, name, arg) = (p.raw, p.name, p.arg);
        // A coding command (`/plan`...) or a skill's: both read the session or the
        // skills folder, so off the async workers.
        self.blocking(move |g| {
            g.core
                .dispatch_command(session_id(&raw).ok().as_deref(), &name, &arg)
                .map(commands::outcome)
                .map_err(command_error)
        })
        .await?
    }

    /// `/status` (`session.status`): the session, its model and folder, tokens, context.
    pub(super) async fn status_text(self: &Arc<Self>, id: &str) -> Result<String, RpcError> {
        let env = PageCommands::new(self, &Settings::load(&self.data_dir));
        self.core.status_text(id, &env).await.map_err(command_error)
    }
}

/// A command's refusal as the page's error: Hermes's wording for "not a command here".
fn command_error(error: CommandError) -> RpcError {
    match error {
        CommandError::Invalid(text) => RpcError::invalid(text),
        CommandError::Failed(text) => RpcError::server(text),
        CommandError::NotACommand => RpcError::server("not a quick/plugin/bundle/skill command"),
    }
}

/// What the commands need from the page's side: the Settings the request was answered with.
struct PageCommands {
    gateway: Arc<Gateway>,
    settings: Settings,
}

impl PageCommands {
    fn new(gateway: &Arc<Gateway>, settings: &Settings) -> Self {
        Self {
            gateway: gateway.clone(),
            settings: settings.clone(),
        }
    }
}

#[async_trait::async_trait]
impl CommandEnv for PageCommands {
    async fn context_length(&self, provider: &str, model: &str) -> usize {
        self.gateway.context_length(provider, model).await
    }

    fn route(
        &self,
        session_id: &str,
    ) -> Result<(hermes_runtime::ProviderConfig, TurnOptions), CommandError> {
        let provider = self
            .gateway
            .provider_for(session_id, &self.settings)
            .map_err(|e| CommandError::Failed(e.message()))?;
        Ok((provider, self.gateway.turn_options(session_id)))
    }

    /// Every session of the original Hermes not imported yet (`hermes_import.rs`).
    async fn import_hermes(&self) -> Result<String, CommandError> {
        let (provider, model) = self
            .gateway
            .import_route(&self.settings)
            .map_err(|e| CommandError::Failed(e.message()))?;
        let runtime = self.gateway.runtime.clone();
        let data_dir = self.gateway.data_dir.clone();
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
                    text.push_str(&format!(
                        "\nNot imported ({}):\n{}",
                        failed.len(),
                        failed.join("\n")
                    ));
                }
                Ok(text)
            }
            Err(e) => Err(CommandError::Failed(e.to_string())),
        }
    }
}
