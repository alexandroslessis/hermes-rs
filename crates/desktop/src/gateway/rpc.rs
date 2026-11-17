//! The page's JSON-RPC methods as a type: `Request` (one variant per method this backend serves,
//! tagged by `method`, its params as `params`) and `RpcError` (the three codes the page reads).
//!
//! Hermes's `tui_gateway/server.py` routes by a dict of method name to handler; the page's side is
//! `apps/desktop/src/api/canonical-protocol.ts`. A name that is not a variant is -32601 ("not
//! here", ground rule 5): `Request::parse` answers it, with the old message, for any spelling
//! that is not exactly a served name (other case, spaces, empty).
//!
//! Params are **lenient on purpose**. Hermes's page sends loose JSON (a missing field, a number
//! where a string was expected, `null` params, an object with fields a method ignores) and the
//! handlers always read it as `params["field"].as_str()...`, so the old backend never answered
//! "invalid params" for the shape of the JSON itself: a field of the wrong type was the same as an
//! absent one, and what a method requires it checks itself (`SessionRef::id()` is the
//! "session_id is required" the methods answered). Each params type here is built by
//! `FromParams::from_params` from the raw value with exactly those reads, so parsing never
//! fails; serde's own struct deserializer would reject `null` and accept an array, so it is not
//! used. Methods whose handler hands the params on to a long function that reads many fields
//! (`session.create`, `prompt.submit`, `session.mutate`, ...) carry `Raw`, the value as sent.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// JSON-RPC's "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const SERVER_ERROR: i64 = -32000;

/// A session whose turn runs refuses edits, restores and moves (the frozen policy would change
/// under it). The page reads the typed reason off the error's `data` (`canonical-protocol.ts`
/// `failure()`, the move's "busy" notice) and matches the prose otherwise (`isSessionBusyError`).
pub const BUSY: &str = "session busy";

/// Why a request failed, with the code and text the page and the ACP server read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// -32601: not a method this backend serves. Holds the method name as the page sent it.
    MethodNotFound(String),
    /// -32602: a required param is missing or refused.
    InvalidParams(String),
    /// -32000: it failed; the message is shown to the user.
    Server(String),
    /// -32000 `session busy`, with `data: {"reason": "session_busy"}`.
    Busy,
}

impl RpcError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidParams(message.into())
    }

    /// -32000. The busy refusal's text is the typed `Busy` whichever way it was written (the
    /// old check for the `data` was on the message).
    pub fn server(message: impl Into<String>) -> Self {
        let message = message.into();
        if message == BUSY {
            Self::Busy
        } else {
            Self::Server(message)
        }
    }

    /// A failure from `anyhow`, with its whole chain (`{e:#}`), as the handlers always wrote it.
    pub fn failed(error: impl std::fmt::Display) -> Self {
        Self::server(format!("{error:#}"))
    }

    pub fn code(&self) -> i64 {
        match self {
            Self::MethodNotFound(_) => METHOD_NOT_FOUND,
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::Server(_) | Self::Busy => SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::MethodNotFound(method) => format!("Method not found: {method}"),
            Self::InvalidParams(message) | Self::Server(message) => message.clone(),
            Self::Busy => BUSY.to_string(),
        }
    }

    /// The error object's `data`: the typed reason of a busy session, nothing else has one.
    pub fn data(&self) -> Option<Value> {
        matches!(self, Self::Busy).then(|| json!({"reason": "session_busy"}))
    }

    /// The error as the JSON-RPC `error` member.
    pub fn to_json(&self) -> Value {
        let mut error = json!({"code": self.code(), "message": self.message()});
        if let Some(data) = self.data() {
            error["data"] = data;
        }
        error
    }

    /// `(code, message)`, the shape `Gateway::call` gives the ACP server.
    pub fn into_parts(self) -> (i64, String) {
        (self.code(), self.message())
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for RpcError {}

/// What a handler returns.
pub type Reply = Result<Value, RpcError>;

/// A method's params, read the way the handlers always read them (see the module doc).
pub trait FromParams {
    fn from_params(params: &Value) -> Self;
}

/// Deserializes through the raw value, so every shape of `params` (absent, `null`, an array, a
/// number) gives a value instead of an error.
#[derive(Debug, Clone, PartialEq)]
pub struct P<T>(pub T);

impl<'de, T: FromParams> Deserialize<'de> for P<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(P(T::from_params(&value)))
    }
}

/// A method that takes no params: whatever came with it is ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct Nothing;

impl FromParams for Nothing {
    fn from_params(_: &Value) -> Self {
        Nothing
    }
}

/// The params as the page sent them, for the handlers that read many fields themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw(pub Value);

impl FromParams for Raw {
    fn from_params(params: &Value) -> Self {
        Raw(params.clone())
    }
}

fn string(params: &Value, field: &str) -> Option<String> {
    params[field].as_str().map(str::to_string)
}

fn text(params: &Value, field: &str) -> String {
    string(params, field).unwrap_or_default()
}

/// A string field that is also absent when empty.
fn non_empty(params: &Value, field: &str) -> Option<String> {
    string(params, field).filter(|s| !s.is_empty())
}

/// `{session_id}`: which session. `id()` is `session_id(params)?`: a missing or non-string id is
/// -32602 "session_id is required".
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRef {
    pub session_id: Option<String>,
}

impl SessionRef {
    pub fn id(&self) -> Result<String, RpcError> {
        self.session_id
            .clone()
            .ok_or_else(|| RpcError::invalid("session_id is required"))
    }
}

impl FromParams for SessionRef {
    fn from_params(params: &Value) -> Self {
        Self {
            session_id: string(params, "session_id"),
        }
    }
}

/// `projects.tree` / `projects.project_sessions`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectsTree {
    pub preview_limit: usize,
    pub project_id: Option<String>,
}

impl FromParams for ProjectsTree {
    fn from_params(params: &Value) -> Self {
        Self {
            preview_limit: params["preview_limit"].as_u64().unwrap_or(3) as usize,
            project_id: string(params, "project_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectsCreate {
    /// Trimmed; "Project" when absent.
    pub name: String,
    pub folders: Vec<String>,
    pub primary_path: Option<String>,
    /// `use` is exactly `true`.
    pub activate: bool,
}

impl FromParams for ProjectsCreate {
    fn from_params(params: &Value) -> Self {
        Self {
            name: params["name"]
                .as_str()
                .unwrap_or("Project")
                .trim()
                .to_string(),
            folders: params["folders"]
                .as_array()
                .map(|f| {
                    f.iter()
                        .filter_map(|p| p.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            primary_path: non_empty(params, "primary_path"),
            activate: params["use"] == true,
        }
    }
}

/// `projects.update` / `projects.add_folder`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectsEdit {
    pub id: String,
    /// Absent when blank.
    pub name: Option<String>,
    pub path: Option<String>,
    pub is_primary: bool,
}

impl FromParams for ProjectsEdit {
    fn from_params(params: &Value) -> Self {
        Self {
            id: text(params, "id"),
            name: string(params, "name").filter(|n| !n.trim().is_empty()),
            path: non_empty(params, "path"),
            is_primary: params["is_primary"] == true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectId {
    pub id: String,
}

impl FromParams for ProjectId {
    fn from_params(params: &Value) -> Self {
        Self {
            id: text(params, "id"),
        }
    }
}

/// `projects.set_active`: the id as sent (null clears it).
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveId {
    pub id: Value,
}

impl FromParams for ActiveId {
    fn from_params(params: &Value) -> Self {
        Self {
            id: params["id"].clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelOptions {
    pub session_id: Option<String>,
}

impl FromParams for ModelOptions {
    fn from_params(params: &Value) -> Self {
        Self {
            session_id: string(params, "session_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestsStatus {
    pub provider: Option<String>,
}

impl FromParams for RequestsStatus {
    fn from_params(params: &Value) -> Self {
        Self {
            provider: string(params, "provider"),
        }
    }
}

/// `staging.apply` / `staging.reject`.
#[derive(Debug, Clone, PartialEq)]
pub struct StagingId {
    pub id: Option<String>,
}

impl FromParams for StagingId {
    fn from_params(params: &Value) -> Self {
        Self {
            id: string(params, "id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigGet {
    pub key: Option<String>,
}

impl FromParams for ConfigGet {
    fn from_params(params: &Value) -> Self {
        Self {
            key: string(params, "key"),
        }
    }
}

/// `config.set`: `key` "model" is the model picker (it reads the whole params); any other key is
/// a Settings value, or a session's reasoning effort.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigSet {
    pub raw: Value,
    /// `key` is exactly the string "model".
    pub model: bool,
    pub key: String,
    pub value: Value,
    pub session_id: Option<String>,
}

impl FromParams for ConfigSet {
    fn from_params(params: &Value) -> Self {
        Self {
            raw: params.clone(),
            model: params["key"] == "model",
            key: text(params, "key"),
            value: params["value"].clone(),
            session_id: string(params, "session_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompleteSlash {
    pub text: String,
}

impl FromParams for CompleteSlash {
    fn from_params(params: &Value) -> Self {
        Self {
            text: text(params, "text"),
        }
    }
}

/// `slash.exec`: the command line, and the params for the commands that read the session.
#[derive(Debug, Clone, PartialEq)]
pub struct SlashExec {
    pub raw: Value,
    pub command: Option<String>,
}

impl FromParams for SlashExec {
    fn from_params(params: &Value) -> Self {
        Self {
            raw: params.clone(),
            command: string(params, "command"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandDispatch {
    pub raw: Value,
    pub name: String,
    pub arg: String,
}

impl FromParams for CommandDispatch {
    fn from_params(params: &Value) -> Self {
        Self {
            raw: params.clone(),
            name: text(params, "name"),
            arg: text(params, "arg"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTitle {
    pub session: SessionRef,
    pub title: Option<String>,
}

impl FromParams for SessionTitle {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            title: string(params, "title"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForeignList {
    pub source: Option<String>,
    pub offset: usize,
}

impl FromParams for ForeignList {
    fn from_params(params: &Value) -> Self {
        Self {
            source: string(params, "source"),
            offset: params["offset"].as_u64().unwrap_or(0) as usize,
        }
    }
}

/// `session.foreign.preview` / `.import`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignId {
    pub id: String,
}

impl FromParams for ForeignId {
    fn from_params(params: &Value) -> Self {
        Self {
            id: text(params, "id"),
        }
    }
}

/// `prompt.btw` / `prompt.background`: the text, trimmed.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionText {
    pub session: SessionRef,
    pub text: String,
}

impl FromParams for SessionText {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            text: text(params, "text").trim().to_string(),
        }
    }
}

/// `session.steer` / `session.redirect`: the text as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSteer {
    pub session: SessionRef,
    pub text: String,
}

impl FromParams for SessionSteer {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            text: text(params, "text"),
        }
    }
}

/// `image.attach`: the path, trimmed (and the params, for `image.attach_bytes`'s upload).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageAttach {
    pub raw: Value,
    pub session: SessionRef,
    pub path: String,
}

impl FromParams for ImageAttach {
    fn from_params(params: &Value) -> Self {
        Self {
            raw: params.clone(),
            session: SessionRef::from_params(params),
            path: text(params, "path").trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsightsGet {
    /// A positive whole number of days, else 30.
    pub days: i64,
}

impl FromParams for InsightsGet {
    fn from_params(params: &Value) -> Self {
        Self {
            days: params["days"].as_i64().filter(|d| *d > 0).unwrap_or(30),
        }
    }
}

/// `approval.respond` / `clarify.respond`: the prompt's id (`prompt_id`, else `request_id`) and
/// both answer fields (the method reads `choice` or `answer`).
#[derive(Debug, Clone, PartialEq)]
pub struct Respond {
    pub prompt_id: Option<String>,
    pub choice: String,
    pub answer: String,
}

impl FromParams for Respond {
    fn from_params(params: &Value) -> Self {
        Self {
            prompt_id: params["prompt_id"]
                .as_str()
                .or(params["request_id"].as_str())
                .map(str::to_string),
            choice: text(params, "choice"),
            answer: text(params, "answer"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessKill {
    pub session: SessionRef,
    pub process_id: String,
}

impl FromParams for ProcessKill {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            process_id: text(params, "process_id"),
        }
    }
}

/// `subagent.tail` / `.interrupt`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentRef {
    pub session: SessionRef,
    pub subagent_id: String,
}

impl FromParams for SubagentRef {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            subagent_id: text(params, "subagent_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentSteer {
    pub session: SessionRef,
    pub subagent_id: String,
    /// Trimmed.
    pub text: String,
}

impl FromParams for SubagentSteer {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            subagent_id: text(params, "subagent_id"),
            text: text(params, "text").trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DelegationPause {
    /// True unless `paused` is a JSON boolean.
    pub paused: bool,
}

impl FromParams for DelegationPause {
    fn from_params(params: &Value) -> Self {
        Self {
            paused: params["paused"].as_bool().unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletePath {
    pub word: String,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
}

impl FromParams for CompletePath {
    fn from_params(params: &Value) -> Self {
        Self {
            word: text(params, "word"),
            cwd: non_empty(params, "cwd"),
            session_id: string(params, "session_id"),
        }
    }
}

/// The queue panel's calls: which queued prompt, and (for an edit) what replaces it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueRef {
    pub session: SessionRef,
    pub admission_id: String,
    pub text: Value,
    pub submission_id: Value,
}

impl FromParams for QueueRef {
    fn from_params(params: &Value) -> Self {
        Self {
            session: SessionRef::from_params(params),
            admission_id: text(params, "admission_id"),
            text: params["text"].clone(),
            submission_id: params["submission_id"].clone(),
        }
    }
}

/// Every method this backend serves. The variant's name is the method with dots as capitals;
/// `Request::parse` is the only way in.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "gateway.ping")]
    GatewayPing(P<Nothing>),
    #[serde(rename = "client.capabilities")]
    ClientCapabilities(P<Nothing>),
    #[serde(rename = "setup.status")]
    SetupStatus(P<Nothing>),
    #[serde(rename = "onboarding.state")]
    OnboardingState(P<Nothing>),
    #[serde(rename = "profiles.list")]
    ProfilesList(P<Nothing>),
    #[serde(rename = "projects.tree")]
    ProjectsTree(P<ProjectsTree>),
    #[serde(rename = "projects.project_sessions")]
    ProjectsProjectSessions(P<ProjectsTree>),
    #[serde(rename = "projects.list")]
    ProjectsList(P<Nothing>),
    #[serde(rename = "projects.create")]
    ProjectsCreate(P<ProjectsCreate>),
    #[serde(rename = "projects.update")]
    ProjectsUpdate(P<ProjectsEdit>),
    #[serde(rename = "projects.add_folder")]
    ProjectsAddFolder(P<ProjectsEdit>),
    #[serde(rename = "projects.delete")]
    ProjectsDelete(P<ProjectId>),
    #[serde(rename = "projects.set_active")]
    ProjectsSetActive(P<ActiveId>),
    #[serde(rename = "projects.discover_repos")]
    ProjectsDiscoverRepos(P<Nothing>),
    #[serde(rename = "projects.record_repos")]
    ProjectsRecordRepos(P<Nothing>),
    #[serde(rename = "session.active_list")]
    SessionActiveList(P<Nothing>),
    #[serde(rename = "model.options")]
    ModelOptions(P<ModelOptions>),
    #[serde(rename = "requests.status")]
    RequestsStatus(P<RequestsStatus>),
    #[serde(rename = "staging.list")]
    StagingList(P<Nothing>),
    #[serde(rename = "staging.apply")]
    StagingApply(P<StagingId>),
    #[serde(rename = "staging.reject")]
    StagingReject(P<StagingId>),
    #[serde(rename = "config.get")]
    ConfigGet(P<ConfigGet>),
    #[serde(rename = "config.set")]
    ConfigSet(P<ConfigSet>),
    #[serde(rename = "pet.info")]
    PetInfo(P<Nothing>),
    #[serde(rename = "setup.runtime_check")]
    SetupRuntimeCheck(P<Nothing>),
    #[serde(rename = "shared_metrics.status")]
    SharedMetricsStatus(P<Nothing>),
    #[serde(rename = "free_tier.status")]
    FreeTierStatus(P<Nothing>),
    #[serde(rename = "wake.status")]
    WakeStatus(P<Nothing>),
    #[serde(rename = "commands.catalog")]
    CommandsCatalog(P<Nothing>),
    #[serde(rename = "complete.slash")]
    CompleteSlash(P<CompleteSlash>),
    #[serde(rename = "slash.exec")]
    SlashExec(P<SlashExec>),
    #[serde(rename = "session.status")]
    SessionStatus(P<SessionRef>),
    #[serde(rename = "command.dispatch")]
    CommandDispatch(P<CommandDispatch>),
    #[serde(rename = "session.title")]
    SessionTitle(P<SessionTitle>),
    #[serde(rename = "session.mutate")]
    SessionMutate(P<Raw>),
    #[serde(rename = "session.rewind")]
    SessionRewind(P<Raw>),
    #[serde(rename = "rollback.list")]
    RollbackList(P<Raw>),
    #[serde(rename = "rollback.diff")]
    RollbackDiff(P<Raw>),
    #[serde(rename = "rollback.restore")]
    RollbackRestore(P<Raw>),
    #[serde(rename = "session.export")]
    SessionExport(P<Raw>),
    #[serde(rename = "session.foreign.list")]
    SessionForeignList(P<ForeignList>),
    #[serde(rename = "session.foreign.preview")]
    SessionForeignPreview(P<ForeignId>),
    #[serde(rename = "session.foreign.import")]
    SessionForeignImport(P<ForeignId>),
    #[serde(rename = "session.branch")]
    SessionBranch(P<Raw>),
    #[serde(rename = "session.branch_whole")]
    SessionBranchWhole(P<Raw>),
    #[serde(rename = "session.branch_stored")]
    SessionBranchStored(P<Raw>),
    #[serde(rename = "prompt.btw")]
    PromptBtw(P<SessionText>),
    #[serde(rename = "prompt.background")]
    PromptBackground(P<SessionText>),
    #[serde(rename = "file.attach")]
    FileAttach(P<Raw>),
    #[serde(rename = "image.attach")]
    ImageAttach(P<ImageAttach>),
    #[serde(rename = "image.attach_bytes")]
    ImageAttachBytes(P<ImageAttach>),
    #[serde(rename = "image.detach")]
    ImageDetach(P<ImageAttach>),
    #[serde(rename = "insights.get")]
    InsightsGet(P<InsightsGet>),
    #[serde(rename = "session.compress")]
    SessionCompress(P<Raw>),
    #[serde(rename = "session.delete")]
    SessionDelete(P<SessionRef>),
    #[serde(rename = "session.close")]
    SessionClose(P<Nothing>),
    #[serde(rename = "session.create")]
    SessionCreate(P<Raw>),
    #[serde(rename = "session.resume")]
    SessionResume(P<Raw>),
    #[serde(rename = "session.history")]
    SessionHistory(P<Raw>),
    #[serde(rename = "prompt.submit")]
    PromptSubmit(P<Raw>),
    #[serde(rename = "approval.respond")]
    ApprovalRespond(P<Respond>),
    #[serde(rename = "clarify.respond")]
    ClarifyRespond(P<Respond>),
    #[serde(rename = "session.context_breakdown")]
    SessionContextBreakdown(P<SessionRef>),
    #[serde(rename = "approval.pending")]
    ApprovalPending(P<SessionRef>),
    #[serde(rename = "process.list")]
    ProcessList(P<SessionRef>),
    #[serde(rename = "process.kill")]
    ProcessKill(P<ProcessKill>),
    #[serde(rename = "process.stop")]
    ProcessStop(P<SessionRef>),
    #[serde(rename = "subagent.list")]
    SubagentList(P<SessionRef>),
    #[serde(rename = "subagent.tail")]
    SubagentTail(P<SubagentRef>),
    #[serde(rename = "subagent.steer")]
    SubagentSteer(P<SubagentSteer>),
    #[serde(rename = "subagent.interrupt")]
    SubagentInterrupt(P<SubagentRef>),
    #[serde(rename = "delegation.status")]
    DelegationStatus(P<Nothing>),
    #[serde(rename = "delegation.pause")]
    DelegationPause(P<DelegationPause>),
    #[serde(rename = "complete.path")]
    CompletePath(P<CompletePath>),
    #[serde(rename = "session.steer")]
    SessionSteer(P<SessionSteer>),
    #[serde(rename = "prompt.cancel")]
    PromptCancel(P<QueueRef>),
    #[serde(rename = "prompt.steer_queued")]
    PromptSteerQueued(P<QueueRef>),
    #[serde(rename = "prompt.replace_queued")]
    PromptReplaceQueued(P<QueueRef>),
    #[serde(rename = "session.redirect")]
    SessionRedirect(P<SessionSteer>),
    #[serde(rename = "session.interrupt")]
    SessionInterrupt(P<SessionRef>),
}

impl Request {
    /// The request for `method` with `params` (`Null` when the frame had none). A name that is
    /// not exactly a served method is `MethodNotFound`; params never make a served method fail
    /// here (see the module doc).
    pub fn parse(method: &str, params: &Value) -> Result<Self, RpcError> {
        // `from_value` wants an owned value: one clone of the params, which the handlers clone
        // anyway.
        serde_json::from_value(json!({"method": method, "params": params}))
            .map_err(|_| RpcError::MethodNotFound(method.to_string()))
    }
}

/// Every name `Request` serves (the page's methods), one per variant.
#[cfg(test)]
pub const SERVED: &[&str] = &[
    "gateway.ping",
    "client.capabilities",
    "setup.status",
    "onboarding.state",
    "profiles.list",
    "projects.tree",
    "projects.project_sessions",
    "projects.list",
    "projects.create",
    "projects.update",
    "projects.add_folder",
    "projects.delete",
    "projects.set_active",
    "projects.discover_repos",
    "projects.record_repos",
    "session.active_list",
    "model.options",
    "requests.status",
    "staging.list",
    "staging.apply",
    "staging.reject",
    "config.get",
    "config.set",
    "pet.info",
    "setup.runtime_check",
    "shared_metrics.status",
    "free_tier.status",
    "wake.status",
    "commands.catalog",
    "complete.slash",
    "slash.exec",
    "session.status",
    "command.dispatch",
    "session.title",
    "session.mutate",
    "session.rewind",
    "rollback.list",
    "rollback.diff",
    "rollback.restore",
    "session.export",
    "session.foreign.list",
    "session.foreign.preview",
    "session.foreign.import",
    "session.branch",
    "session.branch_whole",
    "session.branch_stored",
    "prompt.btw",
    "prompt.background",
    "file.attach",
    "image.attach",
    "image.attach_bytes",
    "image.detach",
    "insights.get",
    "session.compress",
    "session.delete",
    "session.close",
    "session.create",
    "session.resume",
    "session.history",
    "prompt.submit",
    "approval.respond",
    "clarify.respond",
    "session.context_breakdown",
    "approval.pending",
    "process.list",
    "process.kill",
    "process.stop",
    "subagent.list",
    "subagent.tail",
    "subagent.steer",
    "subagent.interrupt",
    "delegation.status",
    "delegation.pause",
    "complete.path",
    "session.steer",
    "prompt.cancel",
    "prompt.steer_queued",
    "prompt.replace_queued",
    "session.redirect",
    "session.interrupt",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(method: &str, params: Value) -> Request {
        Request::parse(method, &params).unwrap_or_else(|e| panic!("{method}: {e:?}"))
    }

    #[test]
    fn the_served_list_is_the_enum() {
        // A variant added without its name here (or the reverse) fails this, so the other tests
        // really cover every method.
        let source = include_str!("rpc.rs");
        let from = source.find("pub enum Request {").unwrap();
        let to = source.find("impl Request {").unwrap();
        let text = &source[from..to];
        for method in SERVED {
            assert!(
                text.contains(&format!("rename = \"{method}\")")),
                "{method}"
            );
        }
        assert_eq!(text.matches("rename = \"").count(), SERVED.len());
    }

    #[test]
    fn every_served_method_parses_from_any_params() {
        let shapes = [
            Value::Null,
            json!({}),
            json!([]),
            json!(["a", 1]),
            json!("text"),
            json!(7),
            json!(1.5),
            json!(true),
            json!({"session_id": 5, "text": null, "folders": "x", "days": "7"}),
            json!({"session_id": "s", "payload": {"a": [1, {"b": null}]}}),
        ];
        assert_eq!(SERVED.len(), 80);
        for method in SERVED {
            for shape in &shapes {
                assert!(Request::parse(method, shape).is_ok(), "{method} {shape}");
            }
        }
    }

    #[test]
    fn a_name_that_is_not_exactly_served_is_method_not_found_with_the_old_text() {
        for name in [
            "nope.nothing",
            "Gateway.Ping",
            "GATEWAY.PING",
            "gateway.ping ",
            " gateway.ping",
            "gateway.ping\n",
            "gateway.ping\0",
            "",
            " ",
            ".",
            "gateway",
            "gateway.",
            "session.save",
            "session.list",
            "terminal.resize",
            "voice.toggle",
            "config.show",
            "event",
            "rpc.discover",
            "session.foreign",
            "prompt.submit.",
            "projects.Tree",
            "session.branch ",
            "gateway.ping/",
        ] {
            let error = Request::parse(name, &json!({"session_id": "s"})).unwrap_err();
            assert_eq!(
                error,
                RpcError::MethodNotFound(name.to_string()),
                "{name:?}"
            );
            assert_eq!(error.code(), -32601);
            assert_eq!(error.message(), format!("Method not found: {name}"));
            assert_eq!(error.data(), None);
            assert_eq!(
                error.to_json(),
                json!({"code": -32601, "message": format!("Method not found: {name}")})
            );
        }
        // The names the page probes and this backend deliberately doesn't serve.
        for name in [
            "session.save",
            "terminal.resize",
            "voice.toggle",
            "session.list",
        ] {
            assert!(!SERVED.contains(&name));
        }
    }

    #[test]
    fn errors_keep_their_codes_texts_and_data() {
        assert_eq!(
            RpcError::invalid("x").into_parts(),
            (-32602, "x".to_string())
        );
        assert_eq!(
            RpcError::server("y").into_parts(),
            (-32000, "y".to_string())
        );
        assert_eq!(
            RpcError::Busy.into_parts(),
            (-32000, "session busy".to_string())
        );
        assert_eq!(
            RpcError::Busy.to_json(),
            json!({"code": -32000, "message": "session busy", "data": {"reason": "session_busy"}})
        );
        assert_eq!(
            RpcError::server("y").to_json(),
            json!({"code": -32000, "message": "y"})
        );
        // The same text as a plain failure is the busy refusal too (the old check was on the
        // message): `rollback` and `rewind_turn` wrote it as a string.
        assert_eq!(RpcError::server("session busy"), RpcError::Busy);
        assert_eq!(RpcError::failed("a b").message(), "a b");
    }

    #[test]
    fn the_pages_payloads_land_in_the_typed_fields() {
        // Payloads the live checks (and so the page) send.
        let Request::ProjectsTree(P(tree)) = parse("projects.tree", json!({"preview_limit": 1000}))
        else {
            panic!()
        };
        assert_eq!((tree.preview_limit, tree.project_id), (1000, None));
        let Request::ProjectsCreate(P(create)) = parse(
            "projects.create",
            json!({"name": "  X ", "folders": ["a", 3, "b"], "primary_path": "b", "use": true}),
        ) else {
            panic!()
        };
        assert_eq!(create.name, "X");
        assert_eq!(create.folders, vec!["a", "b"]);
        assert_eq!(create.primary_path.as_deref(), Some("b"));
        assert!(create.activate);
        let Request::SessionTitle(P(title)) =
            parse("session.title", json!({"session_id": "s1", "title": "T"}))
        else {
            panic!()
        };
        assert_eq!(title.session.id().unwrap(), "s1");
        assert_eq!(title.title.as_deref(), Some("T"));
        let Request::ConfigSet(P(set)) = parse(
            "config.set",
            json!({"session_id": "s", "key": "reasoning", "value": "high"}),
        ) else {
            panic!()
        };
        assert!(!set.model);
        assert_eq!(set.key, "reasoning");
        assert_eq!(set.session_id.as_deref(), Some("s"));
        let Request::ConfigSet(P(set)) = parse(
            "config.set",
            json!({"key": "model", "value": "m --provider p"}),
        ) else {
            panic!()
        };
        assert!(set.model);
        let Request::PromptReplaceQueued(P(queue)) = parse(
            "prompt.replace_queued",
            json!({"session_id": "s", "admission_id": "adm-one", "text": "e", "submission_id": "adm-one-edited"}),
        ) else {
            panic!()
        };
        assert_eq!(queue.admission_id, "adm-one");
        assert_eq!(queue.text, json!("e"));
        assert_eq!(queue.submission_id, json!("adm-one-edited"));
        let Request::ApprovalRespond(P(answer)) = parse(
            "approval.respond",
            json!({"session_id": "s", "prompt_id": "p1", "choice": "deny"}),
        ) else {
            panic!()
        };
        assert_eq!(answer.prompt_id.as_deref(), Some("p1"));
        assert_eq!(answer.choice, "deny");
        // `request_id` stands in for `prompt_id`.
        let Request::ClarifyRespond(P(answer)) = parse(
            "clarify.respond",
            json!({"request_id": "r1", "answer": "blue"}),
        ) else {
            panic!()
        };
        assert_eq!(
            (answer.prompt_id.as_deref(), answer.answer.as_str()),
            (Some("r1"), "blue")
        );
        let Request::SubagentSteer(P(steer)) = parse(
            "subagent.steer",
            json!({"session_id": "s", "subagent_id": "sa", "text": " wrap up "}),
        ) else {
            panic!()
        };
        assert_eq!(
            (steer.subagent_id.as_str(), steer.text.as_str()),
            ("sa", "wrap up")
        );
        let Request::InsightsGet(P(insights)) = parse("insights.get", json!({"days": 7})) else {
            panic!()
        };
        assert_eq!(insights.days, 7);
        let Request::PromptSubmit(P(Raw(raw))) = parse(
            "prompt.submit",
            json!({"session_id": "s", "text": "hi", "submission_id": "a"}),
        ) else {
            panic!()
        };
        assert_eq!(raw["submission_id"], "a");
    }

    #[test]
    fn a_field_of_the_wrong_type_reads_as_absent_and_numbers_do_not_become_strings() {
        // `params["x"].as_str()` was None for a number: still is.
        let Request::SessionTitle(P(title)) =
            parse("session.title", json!({"session_id": 5, "title": 6}))
        else {
            panic!()
        };
        assert_eq!(
            title.session.id(),
            Err(RpcError::invalid("session_id is required"))
        );
        assert_eq!(title.title, None);
        let Request::InsightsGet(P(insights)) = parse("insights.get", json!({"days": "7"})) else {
            panic!()
        };
        assert_eq!(insights.days, 30);
        for days in [json!(0), json!(-3), json!(1.5)] {
            let Request::InsightsGet(P(i)) = parse("insights.get", json!({"days": days})) else {
                panic!()
            };
            assert_eq!(i.days, 30);
        }
        let Request::DelegationPause(P(pause)) =
            parse("delegation.pause", json!({"paused": "false"}))
        else {
            panic!()
        };
        assert!(pause.paused);
        let Request::DelegationPause(P(pause)) = parse("delegation.pause", Value::Null) else {
            panic!()
        };
        assert!(pause.paused);
        let Request::ProjectsCreate(P(create)) = parse(
            "projects.create",
            json!({"use": 1, "name": "", "primary_path": ""}),
        ) else {
            panic!()
        };
        assert!(!create.activate);
        assert_eq!(create.name, "");
        assert_eq!(create.primary_path, None);
        // An array is not read positionally, as serde's struct deserializer would.
        let Request::ProjectsTree(P(tree)) = parse("projects.tree", json!(["a", "b"])) else {
            panic!()
        };
        assert_eq!((tree.preview_limit, tree.project_id), (3, None));
    }
}
