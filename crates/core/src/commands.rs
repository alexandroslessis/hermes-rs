//! The composer's `/` menu: the catalog (the coding commands, `/import-hermes`, `/sandbox`, and the
//! runtime's skills and bundles), completions as the user types, and running a pick
//! ([`Core::run_command`]): a coding command answers with a prompt to send, a skill with the
//! loaded skill (sent as the prompt), a built-in report (`/status`, `/context`, `/usage`,
//! `/insights`, `/diff`, `/tools`, `/memory`, `/sandbox`, `/refine`, `/import-hermes`) with text
//! the chat shows as a note. Hermes's built-in commands the page runs itself (`/new`, `/model`,
//! ...) never reach here. Also: what a typed message was as a command (the transcript and titles
//! show `/name instruction` for a message that loaded a skill, a bundle or a coding command's
//! prompt), and `/review`'s reviewer briefing.
//!
//! What the commands need from the front end (the model's context window, the provider a session
//! talks to, the importer of the original Hermes's sessions) is a [`CommandEnv`]; [`CoreCommands`]
//! is the one over the core's own Settings, which every front end can use.
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.slash`), `tui_gateway/command_discovery.py`,
//! `tui_gateway/server.py` (`slash.exec`, `command.dispatch`), `hermes_cli/commands.py` (the
//! coding commands), `cli_commands_mixin.py` (the reports), `tools/skills_tool.py` (the skill
//! invocation a message carries), `agent/review.py` (`snapshot_recent_messages`,
//! `collect_parent_loaded_skills`).

use std::path::Path;
use std::sync::Arc;

use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_runtime::{ProviderConfig, TurnOptions};
use hermes_tools::SkillStore;
use serde_json::Value;

use crate::Core;

/// The built-in commands this backend runs and offers in the `/` menu: Hermes's coding commands
/// ([`coding_command`]), as `hermes_cli/commands.py` describes them, and `/import-hermes` and
/// `/sandbox` (ours).
pub const CODING_COMMANDS: [(&str, &str); 6] = [
    (
        "plan",
        "Write a markdown implementation plan to .hermes/plans/ without executing anything",
    ),
    (
        "init",
        "Generate or update AGENTS.md project instructions from a repo scan",
    ),
    (
        "review",
        "Spawn an independent subagent to review the work just discussed (PR, code, docs)",
    ),
    (
        "learn",
        "Learn a reusable skill from anything you describe (dirs, URLs, this chat, notes)",
    ),
    (
        "import-hermes",
        "Import every session from the original Hermes on this computer",
    ),
    (
        "sandbox",
        "Show the shell sandbox's folders; /sandbox remove-labels puts their labels back",
    ),
];

/// Where a `/` menu entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// One of [`CODING_COMMANDS`].
    Builtin,
    /// A skill bundle.
    Bundle,
    /// A skill in the data folder.
    Local,
    /// A skill from an external folder.
    External,
}

impl Origin {
    /// The page's name for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Builtin => "builtin",
            Origin::Bundle => "bundle",
            Origin::Local => "local",
            Origin::External => "external",
        }
    }
}

/// One entry of the `/` menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// Without the slash.
    pub name: String,
    pub description: String,
    pub origin: Origin,
}

/// A command the `/` menu offers: the coding commands, then the skills and bundles.
pub fn entries(skills: &SkillStore) -> Vec<CommandEntry> {
    let entry = |name: String, description: String, origin| CommandEntry {
        name,
        description,
        origin,
    };
    let mut out: Vec<CommandEntry> = CODING_COMMANDS
        .iter()
        .map(|(name, description)| {
            entry(name.to_string(), description.to_string(), Origin::Builtin)
        })
        .collect();
    out.extend(
        skills
            .bundles()
            .into_iter()
            .map(|bundle| entry(bundle.slug, bundle.description, Origin::Bundle)),
    );
    out.extend(skills.catalog().into_iter().map(|skill| {
        let origin = if skill.external {
            Origin::External
        } else {
            Origin::Local
        };
        entry(skill.name, skill.description, origin)
    }));
    // A skill named like a coding command can't be run by name (the command wins, as in Hermes).
    let builtin = |name: &str| CODING_COMMANDS.iter().any(|(command, _)| *command == name);
    out.retain(|e| e.origin == Origin::Builtin || !builtin(&e.name));
    out
}

/// The entries whose name starts with (else contains) what follows the `/`, prefix matches first,
/// then by name. Empty without a leading `/`, and past the name (arguments get no completions).
pub fn complete(skills: &SkillStore, text: &str) -> Vec<CommandEntry> {
    let Some(query) = text.strip_prefix('/') else {
        return Vec::new();
    };
    if query.contains(char::is_whitespace) {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let mut matches: Vec<(u8, CommandEntry)> = entries(skills)
        .into_iter()
        .filter_map(|entry| {
            let lower = entry.name.to_lowercase();
            let rank = if lower.starts_with(&query) {
                0
            } else if lower.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, entry))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    matches.into_iter().map(|(_, entry)| entry).collect()
}

/// What running a command gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Text the chat shows as a note.
    Output(String),
    /// A prompt to send as a normal turn, shown as `display` (the command typed).
    Send { message: String, display: String },
    /// A loaded skill or bundle: `message` is the prompt, `display` what the chat shows.
    Skill {
        name: String,
        message: String,
        display: String,
    },
}

/// Why a command didn't run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The request is incomplete (no session where one is needed, `/init` without a folder).
    Invalid(String),
    /// The command failed.
    Failed(String),
    /// No command or skill has this name.
    NotACommand,
}

impl CommandError {
    fn failed(error: anyhow::Error) -> Self {
        CommandError::Failed(format!("{error:#}"))
    }

    fn stopped(error: tokio::task::JoinError) -> Self {
        CommandError::Failed(format!("The request stopped: {error}"))
    }
}

/// What the commands need from the front end (its Settings and what it asked the providers).
#[async_trait::async_trait]
pub trait CommandEnv: Send + Sync {
    /// A model's context window, as the provider lists it (else the front end's fallback).
    async fn context_length(&self, provider: &str, model: &str) -> usize;
    /// The provider a session's turns go to and the options a turn of it would get now.
    fn route(&self, session_id: &str) -> Result<(ProviderConfig, TurnOptions), CommandError>;
    /// `/import-hermes`: imports every session of the original Hermes; the report.
    async fn import_hermes(&self) -> Result<String, CommandError>;
}

/// The context window assumed when a provider lists none (Hermes's `DEFAULT_FALLBACK_CONTEXT`).
pub const FALLBACK_CONTEXT: usize = 256_000;

/// The [`CommandEnv`] over the core's own Settings: what every front end that has no Settings of
/// its own to give uses (a model's window, a session's provider and options, the importer).
pub struct CoreCommands {
    core: Arc<Core>,
    settings: crate::providers::Settings,
}

impl CoreCommands {
    /// With the Settings as they are now.
    pub fn new(core: &Arc<Core>) -> Self {
        Self::with_settings(core, crate::providers::Settings::load(core.data_dir()))
    }

    /// With the Settings a request was answered with.
    pub fn with_settings(core: &Arc<Core>, settings: crate::providers::Settings) -> Self {
        Self {
            core: core.clone(),
            settings,
        }
    }
}

#[async_trait::async_trait]
impl CommandEnv for CoreCommands {
    async fn context_length(&self, provider: &str, model: &str) -> usize {
        self.core.context_length(provider, model).await
    }

    fn route(&self, session_id: &str) -> Result<(ProviderConfig, TurnOptions), CommandError> {
        let provider = self
            .core
            .provider_for(session_id, &self.settings)
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok((provider, self.core.turn_options(session_id)))
    }

    /// Every session of the original Hermes not imported yet (`hermes_import.rs`).
    async fn import_hermes(&self) -> Result<String, CommandError> {
        let (provider, model) = self
            .core
            .import_route(&self.settings)
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        let runtime = self.core.runtime().clone();
        let data_dir = self.core.data_dir().to_path_buf();
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

/// `/plan`, `/init`, `/learn`: Hermes's `send` dispatch, a prompt sent as a normal turn (the agent
/// does the work with its own tools), shown as the command typed. `/review` sends itself
/// (`/review [instructions]`): the turn starts its reviewer (`Core::submit`). `None` for another
/// command; `cwd` and `existing_agents` are the session's folder and its AGENTS.md.
pub fn coding_command(
    name: &str,
    argument: &str,
    cwd: &str,
    existing_agents: Option<&str>,
) -> Option<CommandOutcome> {
    use hermes_prompts::{coding, review};
    let argument = argument.trim();
    let message = match name.trim_start_matches('/') {
        "plan" => coding::plan_message(argument),
        "init" => coding::init_message(cwd, existing_agents, argument),
        "learn" => review::learn_message(argument),
        "review" => format!("/review {argument}").trim_end().to_string(),
        _ => return None,
    };
    let display = format!("/{} {argument}", name.trim_start_matches('/'))
        .trim_end()
        .to_string();
    Some(CommandOutcome::Send { message, display })
}

/// The command a stored message was typed as (`/name instruction`) when it loads a skill or a
/// bundle; `None` for an ordinary message. The transcript and titles show the command.
pub fn typed_command(text: &str) -> Option<String> {
    use hermes_prompts::{coding, review};
    let (command, argument) = hermes_tools::describe_bundle_invocation(text)
        .or_else(|| hermes_tools::skills::describe_invocation(text))
        // The coding commands send a prompt of their own (`coding_command`).
        .or_else(|| coding::plan_request(text).map(|task| ("plan".to_string(), task)))
        .or_else(|| coding::init_request(text).map(|notes| ("init".to_string(), notes)))
        .or_else(|| review::learn_request(text).map(|request| ("learn".to_string(), request)))?;
    Some(format!("/{command} {argument}").trim_end().to_string())
}

/// The instructions of a `/review [instructions]` message, `None` for another message.
pub fn review_instructions(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("/review")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// The `/review` reviewer's goal and briefing from the session's conversation: its last user and
/// assistant messages (their text), and the skills it loaded.
pub fn review_task(messages: &[Message], instructions: &str) -> (String, String) {
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
            let text = typed_command(&text).unwrap_or(text);
            (!text.is_empty()).then_some((is_user, text))
        })
        .take(hermes_prompts::coding::REVIEW_CONTEXT_MESSAGES)
        .collect();
    recent.reverse();
    let mut skills: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !skills.iter().any(|s| s == name) && skills.len() < 8 {
            skills.push(name.to_string());
        }
    };
    // The skills the local index attached count as loaded (Phase 5: no `skill_view` call).
    for message in messages {
        if let Some(names) = message
            .metadata
            .operation_note(hermes_runtime::preinject::NOTE, "skills")
            .and_then(Value::as_array)
        {
            names.iter().filter_map(Value::as_str).for_each(&mut add);
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
        if call.name == "skill_view"
            && args.get("file_path").is_none()
            && let Some(name) = args.get("name").and_then(Value::as_str)
        {
            add(name);
        }
    }
    hermes_prompts::coding::review_task(&recent, instructions, &skills)
}

/// `name argument` split as the `/` menu sends it: the name without its slash, the rest trimmed.
pub fn split_command(command: &str) -> (&str, &str) {
    let command = command.trim().trim_start_matches('/');
    command
        .split_once(char::is_whitespace)
        .map(|(name, argument)| (name, argument.trim()))
        .unwrap_or((command, ""))
}

impl Core {
    /// The `/` menu: every entry. Blocking (reads the skills' folders).
    pub fn commands(&self) -> Vec<CommandEntry> {
        entries(&self.runtime.skills())
    }

    /// How often a skill was viewed and used, for the catalog.
    pub fn skill_usage(&self, name: &str) -> (u64, u64) {
        self.runtime.skills().usage(name)
    }

    /// Completions for what the user typed after `/`. Blocking.
    pub fn complete_command(&self, text: &str) -> Vec<CommandEntry> {
        complete(&self.runtime.skills(), text)
    }

    /// Runs `/name argument` from the `/` menu: a coding command, a built-in report, then a skill
    /// or bundle; [`CommandError::NotACommand`] for anything else. `session_id` is the session
    /// the menu was opened in.
    pub async fn run_command(
        self: &Arc<Self>,
        session_id: Option<&str>,
        name: &str,
        argument: &str,
        env: &dyn CommandEnv,
    ) -> Result<CommandOutcome, CommandError> {
        let (session, n, a) = (
            session_id.map(str::to_string),
            name.to_string(),
            argument.to_string(),
        );
        let this = self.clone();
        // Reads the session and its folder's AGENTS.md: off the async workers.
        let coding =
            tokio::task::spawn_blocking(move || this.coding_command(session.as_deref(), &n, &a))
                .await
                .map_err(CommandError::stopped)?;
        if let Some(answer) = coding {
            return answer;
        }
        if let Some(answer) = self.report(session_id, name, argument, env).await {
            return answer.map(CommandOutcome::Output);
        }
        let (this, n, a) = (self.clone(), name.to_string(), argument.to_string());
        tokio::task::spawn_blocking(move || this.skill_command(&n, &a))
            .await
            .map_err(CommandError::stopped)?
            .ok_or(CommandError::NotACommand)
    }

    /// Runs `/name argument` as the page's `command.dispatch` does: a coding command, else a skill
    /// or bundle (no reports). Blocking.
    pub fn dispatch_command(
        &self,
        session_id: Option<&str>,
        name: &str,
        argument: &str,
    ) -> Result<CommandOutcome, CommandError> {
        if let Some(answer) = self.coding_command(session_id, name, argument) {
            return answer;
        }
        self.skill_command(name, argument)
            .ok_or(CommandError::NotACommand)
    }

    /// `/plan`, `/init`, `/learn`, `/review` ([`coding_command`]) for a session: `None` for
    /// another command. `/init` writes the session's folder's AGENTS.md; `/review` needs a
    /// conversation. Blocking.
    pub fn coding_command(
        &self,
        session_id: Option<&str>,
        name: &str,
        argument: &str,
    ) -> Option<Result<CommandOutcome, CommandError>> {
        let name = name.trim_start_matches('/');
        if !matches!(name, "plan" | "init" | "learn" | "review") {
            return None;
        }
        let record = session_id.and_then(|id| self.runtime.store().session(id).ok().flatten());
        let cwd = record.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
        if name == "review"
            && record
                .as_ref()
                .is_none_or(|r| self.runtime.messages(&r.id).map_or(true, |m| m.is_empty()))
        {
            return Some(Ok(CommandOutcome::Output(
                "Nothing to review yet — send a message first.".into(),
            )));
        }
        if name == "init" && cwd.is_empty() {
            return Some(Err(CommandError::Invalid(
                "/init needs a session with a folder".into(),
            )));
        }
        let existing = (name == "init")
            .then(|| std::fs::read_to_string(Path::new(&cwd).join("AGENTS.md")).ok())
            .flatten()
            .map(|text| text.trim_start_matches('\u{feff}').to_string());
        coding_command(name, argument, &cwd, existing.as_deref()).map(Ok)
    }

    /// A skill or bundle as Hermes's `skill` dispatch: its loaded message. Blocking.
    pub fn skill_command(&self, name: &str, argument: &str) -> Option<CommandOutcome> {
        let name = name.trim_start_matches('/');
        let invocation = format!("/{name} {argument}").trim_end().to_string();
        let message = self.runtime.skill_command(&invocation)?;
        Some(CommandOutcome::Skill {
            name: name.to_string(),
            message,
            // What the chat shows for the (long) loaded skill.
            display: invocation,
        })
    }

    /// Hermes's built-in commands that report on the session (shown as a note in the chat), and
    /// `/refine`, which starts a review. `None` for anything else.
    async fn report(
        self: &Arc<Self>,
        session_id: Option<&str>,
        name: &str,
        arg: &str,
        env: &dyn CommandEnv,
    ) -> Option<Result<String, CommandError>> {
        let need_id = || {
            session_id
                .map(str::to_string)
                .ok_or(CommandError::Invalid("session_id is required".into()))
        };
        let output = match name {
            // Every session of the original Hermes not imported yet.
            "import-hermes" => env.import_hermes().await,
            // The shell sandbox (`terminal.sandbox`, Phase 15). The record and the labels are
            // files: off the async workers.
            "sandbox" => {
                let (data_dir, arg) = (self.data_dir.clone(), arg.to_string());
                tokio::task::spawn_blocking(move || sandbox_text(&data_dir, &arg))
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))
            }
            "status" => match need_id() {
                Ok(id) => self.status_text(&id, env).await,
                Err(e) => Err(e),
            },
            "context" | "ctx" => match need_id() {
                Ok(id) => self.context_text(&id, env).await,
                Err(e) => Err(e),
            },
            "usage" => match need_id() {
                Ok(id) => {
                    let window = self.window_for(&id, env).await;
                    let this = self.clone();
                    tokio::task::spawn_blocking(move || this.runtime.usage_text(&id, window))
                        .await
                        .map_err(CommandError::stopped)
                        .and_then(|text| text.map_err(CommandError::failed))
                }
                Err(e) => Err(e),
            },
            // These read the store and run git: off the async workers.
            "insights" => {
                let (this, arg) = (self.clone(), arg.to_string());
                tokio::task::spawn_blocking(move || this.runtime.insights_text(&arg))
                    .await
                    .map_err(CommandError::stopped)
            }
            "diff" => match need_id() {
                Ok(id) => {
                    let (this, arg) = (self.clone(), arg.to_string());
                    tokio::task::spawn_blocking(move || this.runtime.diff_report(&id, &arg))
                        .await
                        .map_err(CommandError::stopped)
                        .and_then(|report| report.map_err(CommandError::failed))
                }
                Err(e) => Err(e),
            },
            "tools" => Ok(self.tools_text()),
            "memory" => Ok(self.memory_text().await),
            "refine" => match need_id().and_then(|id| Ok((env.route(&id)?, id))) {
                Ok(((provider, options), id)) => {
                    let focus = (!arg.is_empty()).then(|| arg.to_string());
                    self.runtime
                        .refine(&id, &provider, options, focus)
                        .map(|()| {
                            "Reviewing this conversation for lessons to save to memory and \
                             skills; the review runs in the background."
                                .to_string()
                        })
                        .map_err(CommandError::failed)
                }
                Err(e) => Err(e),
            },
            _ => return None,
        };
        Some(output)
    }

    /// The session model's context window, as the gauge shows it.
    async fn window_for(self: &Arc<Self>, id: &str, env: &dyn CommandEnv) -> Option<usize> {
        let (this, session) = (self.clone(), id.to_string());
        let record = tokio::task::spawn_blocking(move || {
            this.runtime.store().session(&session).ok().flatten()
        })
        .await
        .ok()
        .flatten()?;
        Some(env.context_length(&record.provider, &record.model).await)
    }

    /// `/status`: the session, its model and folder, tokens so far, the context in use.
    pub async fn status_text(
        self: &Arc<Self>,
        id: &str,
        env: &dyn CommandEnv,
    ) -> Result<String, CommandError> {
        // The session's rows and the size of its request: store reads, off the async workers.
        let (this, session) = (self.clone(), id.to_string());
        let (record, totals, messages, used) = tokio::task::spawn_blocking(move || {
            let store = this.runtime.store();
            let record = store
                .session(&session)
                .map_err(CommandError::failed)?
                .ok_or(CommandError::Failed(format!("Session {session} not found")))?;
            let totals = store.usage_totals(&session).unwrap_or_default();
            let messages = store.message_count(&session).unwrap_or(0);
            let used = this
                .runtime
                .context_breakdown(&session)
                .map(|b| b.total())
                .unwrap_or(0);
            Ok::<_, CommandError>((record, totals, messages, used))
        })
        .await
        .map_err(CommandError::stopped)??;
        let window = env.context_length(&record.provider, &record.model).await;
        Ok([
            format!(
                "Session: {} ({})",
                record.title.as_deref().unwrap_or("Untitled"),
                record.id
            ),
            format!("Model: {} via {}", record.model, record.provider),
            format!("Folder: {}", record.cwd),
            format!("Messages: {messages}"),
            format!(
                "Tokens: {} in, {} out over {} calls",
                totals.input_tokens, totals.output_tokens, totals.calls
            ),
            format!(
                "Context: ~{used} of {window} ({}%)",
                (used * 100).checked_div(window).unwrap_or(0)
            ),
        ]
        .join("\n"))
    }

    /// `/context`: what the next request is made of, by part, against the model's window.
    pub async fn context_text(
        self: &Arc<Self>,
        id: &str,
        env: &dyn CommandEnv,
    ) -> Result<String, CommandError> {
        let (this, session) = (self.clone(), id.to_string());
        let b = tokio::task::spawn_blocking(move || this.runtime.context_breakdown(&session))
            .await
            .map_err(CommandError::stopped)?
            .map_err(CommandError::failed)?;
        let window = self.window_for(id, env).await.unwrap_or(FALLBACK_CONTEXT);
        let total = b.total();
        let row = |name: &str, n: usize| {
            format!(
                "  {name:<18}~{n:>8}  {:>3}%",
                (n * 100).checked_div(total).unwrap_or(0)
            )
        };
        Ok([
            format!(
                "Context: ~{total} of {window} tokens ({}% of the window)",
                (total * 100).checked_div(window).unwrap_or(0)
            ),
            row("System prompt", b.system_prompt),
            row("Tool definitions", b.tool_definitions),
            row("Skills", b.skills),
            row("Memory", b.memory),
            row("Conversation", b.conversation),
            "Estimates (characters / 4); /compress summarizes the conversation now.".into(),
        ]
        .join("\n"))
    }

    /// `/tools`: the built-in and MCP tools with the first sentence of each description.
    fn tools_text(&self) -> String {
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
        out.join("\n")
    }

    /// `/memory`: memory writes waiting for approval and the staged ones.
    async fn memory_text(self: &Arc<Self>) -> String {
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
                "{staged} memory change(s) wait for your review in Settings ▸ Memory & Context."
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
        out.join("\n")
    }
}

/// `/sandbox` (the level, the grants and the labelled folders) and `/sandbox remove-labels`.
pub fn sandbox_text(data_dir: &Path, arg: &str) -> String {
    use hermes_tools::sandbox_labels;
    let level = match hermes_tools::sandbox_level() {
        hermes_tools::Sandbox::Low => "low",
        hermes_tools::Sandbox::Off => "off",
    };
    let listed = |out: &mut Vec<String>, paths: &[std::path::PathBuf]| {
        out.extend(paths.iter().map(|p| format!("  {}", p.display())));
    };
    match arg.trim() {
        "remove-labels" | "remove" | "unlabel" => match sandbox_labels::remove_all(data_dir) {
            Ok(removal) => {
                let mut out = vec![format!(
                    "Sandbox labels removed: {} folder(s) put back as they were.",
                    removal.restored.len()
                )];
                listed(&mut out, &removal.restored);
                if !removal.gone.is_empty() {
                    out.push(format!(
                        "{} folder(s) no longer exist (forgotten; a folder that was moved keeps \
                         its label where it is now):",
                        removal.gone.len()
                    ));
                    listed(&mut out, &removal.gone);
                }
                if !removal.changed.is_empty() {
                    out.push(format!(
                        "{} folder(s) carry a label someone else set since (left as they are):",
                        removal.changed.len()
                    ));
                    listed(&mut out, &removal.changed);
                }
                if !removal.failed.is_empty() {
                    out.push(format!(
                        "{} folder(s) could not be put back (still recorded; try again):",
                        removal.failed.len()
                    ));
                    out.extend(
                        removal
                            .failed
                            .iter()
                            .map(|(p, e)| format!("  {}: {e}", p.display())),
                    );
                }
                if level == "low" {
                    out.push(
                        "terminal.sandbox is still \"low\": the next command labels its folders \
                         again. Set it to \"off\" in desktop-config.json (and restart) to stop."
                            .into(),
                    );
                }
                out.join("\n")
            }
            Err(e) => format!("Could not read the sandbox label record: {e}"),
        },
        "" => {
            let mut out = vec![format!(
                "terminal.sandbox: {level} (desktop-config.json; \"low\" runs commands at low \
                 integrity, in PowerShell even when the terminal is Git Bash)."
            )];
            let grants = sandbox_labels::grants();
            if grants.is_empty() {
                out.push("Granted folders (terminal.sandbox_grants): none.".into());
            } else {
                out.push("Granted folders (terminal.sandbox_grants):".into());
                listed(&mut out, &grants);
            }
            match sandbox_labels::list(data_dir) {
                Ok(folders) if folders.is_empty() => {
                    out.push("No folder is labelled Low by this app.".into())
                }
                Ok(folders) => {
                    out.push(format!(
                        "Folders labelled Low by this app ({}):",
                        folders.len()
                    ));
                    out.extend(
                        folders
                            .iter()
                            .map(|f| format!("  {}  ({})", f.path.display(), f.kind)),
                    );
                    out.push("Put them back as they were with /sandbox remove-labels.".into());
                }
                Err(e) => out.push(format!("The label record could not be read: {e}")),
            }
            out.join("\n")
        }
        other => {
            format!("Unknown /sandbox argument '{other}'. Use /sandbox or /sandbox remove-labels.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_takes_its_instructions_and_nothing_else_is_review() {
        assert_eq!(review_instructions("/review"), Some(""));
        assert_eq!(
            review_instructions("  /review the parser "),
            Some("the parser")
        );
        assert_eq!(
            review_instructions("/review  look at tests "),
            Some("look at tests")
        );
        assert_eq!(review_instructions("/reviewer"), None);
        assert_eq!(review_instructions("please /review"), None);
    }

    #[test]
    fn an_ordinary_message_is_no_command() {
        assert_eq!(typed_command("hello there"), None);
    }

    fn sent(outcome: CommandOutcome) -> (String, String) {
        match outcome {
            CommandOutcome::Send { message, display } => (message, display),
            other => panic!("not a prompt to send: {other:?}"),
        }
    }

    #[test]
    fn coding_commands_send_their_prompt_and_show_the_command() {
        let (message, display) =
            sent(coding_command("plan", " add a cache ", "C:/r", None).unwrap());
        assert_eq!(display, "/plan add a cache");
        assert!(message.starts_with("[/plan — plan mode]"));
        // Stored, it reads back as the command.
        assert_eq!(
            typed_command(&message).as_deref(),
            Some("/plan add a cache")
        );

        let (init, _) = sent(coding_command("/init", "", "C:/r", Some("# Mine")).unwrap());
        assert!(init.contains("MERGE DISCIPLINE"));
        assert_eq!(typed_command(&init).as_deref(), Some("/init"));

        let (learn, _) = sent(coding_command("learn", "the build steps", "", None).unwrap());
        assert_eq!(
            typed_command(&learn).as_deref(),
            Some("/learn the build steps")
        );

        let (review, _) = sent(coding_command("review", "edge cases", "", None).unwrap());
        assert_eq!(review, "/review edge cases");
        assert!(coding_command("status", "", "", None).is_none());
    }

    #[test]
    fn a_command_splits_into_its_name_and_trimmed_argument() {
        assert_eq!(
            split_command(" /plan  add a cache "),
            ("plan", "add a cache")
        );
        assert_eq!(split_command("status"), ("status", ""));
    }

    fn store_with(skills: &[(&str, &str)]) -> (SkillStore, hermes_testdir::TestDir) {
        let root = hermes_testdir::tempdir().unwrap();
        for (name, description) in skills {
            let dir = root.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: {description}\n---\n\nDo the {name} thing.\n"
                ),
            )
            .unwrap();
        }
        (
            SkillStore::new(root.path().to_path_buf(), Default::default()),
            root,
        )
    }

    #[test]
    fn completions_rank_prefix_matches_first() {
        let (store, _root) = store_with(&[
            ("planner", "Plan work"),
            ("explain-plan", "Explain"),
            ("review", "Review"),
        ]);
        let found = complete(&store, "/pla");
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        // The `/plan` command, then the prefix-matching skill, then the one containing it.
        assert_eq!(names, ["plan", "planner", "explain-plan"]);
        assert_eq!(found[0].origin, Origin::Builtin);
        assert_eq!(found[1].origin, Origin::Local);
        assert_eq!(found[1].description, "Plan work");
        // Past the name there's nothing to complete; without a slash neither.
        assert!(complete(&store, "/plan now").is_empty());
        assert!(complete(&store, "plan").is_empty());
        // A skill named like a command (`review`) yields to it.
        let all = entries(&store);
        assert_eq!(
            all.iter().filter(|e| e.origin != Origin::Builtin).count(),
            2
        );
        assert_eq!(all[0].name, "plan");
        assert_eq!(complete(&store, "/review").len(), 1);
    }

    #[test]
    fn a_loaded_skill_reads_back_as_its_command() {
        let (store, _root) = store_with(&[("plan", "Plan work")]);
        let message = store.invocation_message("plan", "the release").unwrap();
        assert_eq!(
            typed_command(&message).as_deref(),
            Some("/plan the release")
        );
    }
}
