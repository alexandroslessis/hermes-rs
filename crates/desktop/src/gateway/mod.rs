//! Hermes's `tui_gateway` protocol over the hermes-rs runtime: newline-delimited JSON-RPC 2.0
//! on a WebSocket, `gateway.ready` right after connecting, agent progress as `event`
//! notifications (`tui_gateway/ws.py`, `tui_gateway/server.py`). Result shapes follow the
//! contract the frontend is generated from (`apps/shared/src/gateway-contract.openrpc.json`).
//! A method this backend doesn't implement answers JSON-RPC's -32601, which the frontend treats
//! as "not supported here" and hides.
//!
//! Hermes: `tui_gateway/server.py`, `tui_gateway/methods_prompt.py`, `tui_gateway/methods_session.py`; what the page expects: `apps/desktop/src/api/canonical-protocol.ts`.
//!
//! Layout (Phase 13 CP13f). This file holds `Gateway` itself, the socket (`connection`, `handle`,
//! `call`) and `dispatch`, one `match` over `rpc::Request` whose arms call the handlers; the
//! handlers and the helpers they share are in one file per area, all `impl Gateway`:
//!
//! | file | what |
//! |---|---|
//! | `rpc.rs` | `Request` (every served method and its params), `RpcError`, `Reply` |
//! | `sessions.rs` | create, resume, title, delete, branch, mutate, rewind, rollback, export, compress, status, context |
//! | `turn.rs` | `prompt.submit` and the turn's event loop, the queue, steer, redirect, interrupt, approvals, `/btw`, `/bg` |
//! | `slash.rs` | `slash.exec`, `command.dispatch`, `complete.*`, the commands that need the session |
//! | `settings.rs` | `config.*`, `setup.*`, the model picker, Settings applied to the runtime, provider limits |
//! | `projects.rs` | `projects.*` |
//! | `attachments.rs` | `file.attach`, `image.*` |
//! | `delegation.rs` | `process.*`, `subagent.*`, `delegation.*` |
//! | `usage.rs` | `requests.status`, `insights.get`, `staging.*`, generation speed and token usage |
//! | `rest.rs` | `Gateway::rest` (the REST routes that need the runtime), the session routes and rows |
//!
//! To add a method: a variant in `rpc::Request` (+ its name in `rpc::SERVED`), a handler in the
//! area's file, one arm in `dispatch`; see `docs/ARCHITECTURE.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use hermes_runtime::{Runtime, RuntimeEvent, TurnOptions, TurnStatus};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::commands;
use crate::hub::{Hub, Outbox};
use crate::providers::Settings;
use crate::transcript;

mod attachments;
mod delegation;
mod projects;
mod rest;
pub mod rpc;
mod sessions;
mod settings;
mod slash;
mod turn;
mod usage;

#[cfg(test)]
use rpc::{INVALID_PARAMS, SERVER_ERROR};
use rpc::{P, Raw, Reply, Request, RpcError};
use settings::{FALLBACK_CONTEXT, ModelFacts};

/// What `Gateway::call` answers a client in this process (the ACP server): the code and the
/// message of an `RpcError`.
type RpcResult = Result<Value, (i64, String)>;

/// Hermes fences turn control and prompt answers by an execution generation. This backend runs
/// one agent per session in-process, so a single generation serves every turn.
pub const EXECUTION_GENERATION: u64 = 0;

/// Each session's generated tokens and generation seconds (the status bar's tokens per second),
/// in the data folder.
const GENERATION_FILE: &str = "generation-stats.json";

/// Hermes's compaction status lines (`agent/conversation_compression.py` `COMPACTION_STATUS`,
/// `COMPACTION_DONE_STATUS`).
const COMPACTING_TEXT: &str =
    "🗜️ Compacting context — summarizing earlier conversation so I can continue...";
const COMPACTED_TEXT: &str = "✓ Context compaction complete — continuing turn...";

pub struct Gateway {
    pub runtime: Arc<Runtime>,
    pub rest: crate::rest::Rest,
    /// Background actions the page follows by name (the curator's run).
    actions: Arc<crate::maintenance::Actions>,
    data_dir: PathBuf,
    /// Changes when the process restarts, so reconnecting clients reset their replay state.
    replay_epoch: String,
    /// Event routes and open questions, shared with the runtime's bridge.
    hub: Arc<Hub>,
    /// Admission id → the started turn waiting for its `prompt.submit` reply to go out first.
    starts: std::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// `provider\nmodel` → its context window, as the provider lists it (asked once).
    context_lengths: std::sync::Mutex<std::collections::HashMap<String, ModelFacts>>,
    /// Session id → output tokens generated and the seconds the model calls took (kept in
    /// `GENERATION_FILE`; the status bar's tokens per second).
    generation: std::sync::Mutex<std::collections::HashMap<String, (u64, f64)>>,
    /// File ticket → the file it opens (download links and media elements).
    file_tickets: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
    /// Branch → the session it was branched from (`branch.rs`).
    branches: crate::branch::Links,
    /// Session id → the git identity of its workspace (`workspace.rs`), kept beside the store.
    workspaces: std::sync::Arc<crate::workspace::Workspaces>,
    /// A branch request's idempotency key → the branch it made (a retried request gets it again).
    branch_keys: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Archived sessions and the auto-archive sweep (`archive.rs`).
    archive: crate::archive::Archive,
    /// Each page socket and the session it shows now (its last `session.resume` or
    /// `session.create`): opening another one is "switched away from" the first (Phase 7).
    viewing: std::sync::Mutex<Vec<(Outbox, String)>>,
}

impl Gateway {
    pub fn new(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf) -> Self {
        Self::build(runtime, hub, data_dir, true)
    }

    /// The gateway the `--acp` process (`acp/`) talks to: the same, without the background jobs
    /// (the curator, the auto-archive sweep). The desktop app, which may run at the same time on
    /// the same data folder, owns those.
    pub fn new_for_acp(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf) -> Self {
        Self::build(runtime, hub, data_dir, false)
    }

    fn build(runtime: Arc<Runtime>, hub: Arc<Hub>, data_dir: PathBuf, background: bool) -> Self {
        let data_dir_for_stats = data_dir.clone();
        let gateway = Self {
            runtime,
            rest: crate::rest::Rest::new(data_dir.clone()),
            actions: Default::default(),
            data_dir,
            replay_epoch: uuid::Uuid::new_v4().to_string(),
            hub,
            starts: Default::default(),
            context_lengths: Default::default(),
            file_tickets: Default::default(),
            branches: crate::branch::Links::load(&data_dir_for_stats),
            workspaces: std::sync::Arc::new(crate::workspace::Workspaces::load(
                &data_dir_for_stats,
            )),
            archive: crate::archive::Archive::load(&data_dir_for_stats),
            branch_keys: Default::default(),
            viewing: Default::default(),
            generation: std::sync::Mutex::new(
                std::fs::read(data_dir_for_stats.join(GENERATION_FILE))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default(),
            ),
        };
        // The runtime keeps the approval policy in memory: Settings' choice applies from the start.
        gateway.apply_settings();
        if background {
            crate::maintenance::schedule_curator(
                gateway.runtime.clone(),
                gateway.actions.clone(),
                gateway.data_dir.clone(),
            );
        }
        // So are the skills switched off in Capabilities ▸ Skills.
        crate::skills::apply_config(&gateway.runtime.skills(), &gateway.rest);
        gateway
    }

    /// Background work that needs the shared gateway: the hourly auto-archive check (it sweeps at
    /// most once a day, and only when Settings ▸ Sessions turned it on).
    pub fn schedule(this: &Arc<Self>) {
        Self::install_review_route(this);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(this);
        handle.spawn(async move {
            let mut hourly = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                hourly.tick().await;
                let Some(this) = weak.upgrade() else { break };
                let _ = tokio::task::spawn_blocking(move || this.auto_archive()).await;
            }
        });
    }

    /// How a session's background review reaches its model (Phase 7, `review_schedule.rs`): the
    /// session's provider and the options a turn of it would get now (its window, effort, the
    /// auxiliary review model). Holds the gateway weakly: the runtime it is installed in is the
    /// gateway's own.
    pub fn install_review_route(this: &Arc<Self>) {
        let weak = Arc::downgrade(this);
        this.runtime
            .set_review_route(Some(Arc::new(move |id: &str| {
                let gateway = weak.upgrade()?;
                let settings = Settings::load(&gateway.data_dir);
                let provider = gateway.provider_for(id, &settings).ok()?;
                Some((provider, gateway.turn_options(id)))
            })));
    }

    pub async fn connection(self: Arc<Self>, socket: WebSocket) {
        let (mut sink, mut stream) = socket.split();
        let (outbox, mut outgoing) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if sink.send(WsMessage::Text(frame.into())).await.is_err() {
                    break;
                }
            }
        });
        send(
            &outbox,
            json!({
                "jsonrpc": "2.0",
                "method": "event",
                "params": {"type": "gateway.ready", "payload": {
                    "skin": {},
                    "change_events": false,
                    "heartbeat": true,
                    "replay_epoch": self.replay_epoch,
                }},
            }),
        );
        while let Some(Ok(message)) = stream.next().await {
            let WsMessage::Text(text) = message else {
                continue;
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(frame) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let this = self.clone();
                let outbox = outbox.clone();
                // Each request on its own task: a slow one never holds up the heartbeat.
                tokio::spawn(async move { this.handle(frame, outbox).await });
            }
        }
        writer.abort();
    }

    async fn handle(self: Arc<Self>, frame: Value, outbox: Outbox) {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            // A response to a server→client request (approvals, clarify): not wired yet.
            return;
        };
        let id = frame.get("id").cloned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        tracing::debug!(target: "hermes_desktop::wire", "← {method} {}", params["session_id"]);
        if matches!(
            method,
            "session.create" | "session.resume" | "session.close"
        ) {
            tracing::info!("{method} {}", params["session_id"]);
        }
        let began = std::time::Instant::now();
        let result = self.clone().dispatch(method, &params, &outbox).await;
        if began.elapsed() > std::time::Duration::from_secs(5) {
            tracing::warn!("RPC {method} took {:.1?}", began.elapsed());
        }
        let Some(id) = id else {
            return;
        };
        let reply = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => {
                if let RpcError::MethodNotFound(_) = error {
                    crate::server::log_missing(&format!("RPC {method}"), &params.to_string());
                } else {
                    tracing::warn!("RPC {method} failed: {}", error.message());
                }
                // The page's canonical protocol and the move's notice read a busy refusal's
                // reason (`data.reason`), not just the prose: `to_json` adds it.
                json!({"jsonrpc": "2.0", "id": id, "error": error.to_json()})
            }
        };
        send(&outbox, reply);
        // A started turn's events follow its admission receipt.
        if method == "prompt.submit" {
            self.release_admission(&params);
        }
    }

    /// Lets a started turn's events flow: they wait for the receipt of the `prompt.submit` that
    /// started them to be sent.
    fn release_admission(&self, params: &Value) {
        let Some(admission) = admission_id(params) else {
            return;
        };
        let start = self
            .starts
            .lock()
            .ok()
            .and_then(|mut s| s.remove(&admission));
        if let Some(start) = start {
            let _ = start.send(());
        }
    }

    /// One call, as the page's socket makes it, for another client in this process (the ACP
    /// server): the answer is returned instead of sent, and a started turn's events are released
    /// at once. Events follow on `outbox`.
    pub async fn call(self: Arc<Self>, method: &str, params: Value, outbox: &Outbox) -> RpcResult {
        let result = self.clone().dispatch(method, &params, outbox).await;
        if method == "prompt.submit" {
            self.release_admission(&params);
        }
        result.map_err(RpcError::into_parts)
    }

    /// Runs `work` on a blocking thread: SQLite, file and repository work must not hold an async
    /// worker (ground rule 7; a store read also waits for the writer's pending commit). A thread
    /// that panicked or was cancelled answers as a failed request, never a panic here.
    async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&Arc<Gateway>) -> T + Send + 'static,
    ) -> Result<T, RpcError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || work(&this))
            .await
            .map_err(|e| RpcError::server(format!("The request stopped: {e}")))
    }

    /// Answers one request: `Request::parse` names the method (a name it doesn't know is
    /// -32601, "not here", ground rule 5) and each arm calls its handler (`projects.rs`,
    /// `settings.rs`, `sessions.rs`, `turn.rs`, `slash.rs`, `attachments.rs`, `delegation.rs`,
    /// `usage.rs`).
    async fn dispatch(self: Arc<Self>, method: &str, params: &Value, outbox: &Outbox) -> Reply {
        let settings = Settings::load(&self.data_dir);
        match Request::parse(method, params)? {
            Request::GatewayPing(_) => Ok(json!({"ok": true})),
            Request::ClientCapabilities(_) => Ok(json!({"server_requests": []})),
            Request::SetupStatus(_) => self.setup_status(&settings),
            Request::OnboardingState(_) => Ok(json!({"run": false, "eligible": false})),
            Request::ProfilesList(_) => self.profiles_list(&settings),
            Request::ProjectsTree(P(p)) | Request::ProjectsProjectSessions(P(p)) => {
                self.projects_tree(p).await
            }
            Request::ProjectsList(_) => self.projects_list().await,
            Request::ProjectsCreate(P(p)) => self.projects_create(p).await,
            Request::ProjectsUpdate(P(p)) | Request::ProjectsAddFolder(P(p)) => {
                self.projects_update(p).await
            }
            Request::ProjectsDelete(P(p)) => self.projects_delete(p).await,
            Request::ProjectsSetActive(P(p)) => self.projects_set_active(p),
            // No scanning for repositories yet: the tree shows the ones sessions ran in.
            Request::ProjectsDiscoverRepos(_) => Ok(json!({"repos": []})),
            Request::ProjectsRecordRepos(_) => Ok(json!({"repos": [], "accepted": true})),
            Request::SessionActiveList(_) => {
                Ok(json!({"sessions": self.blocking(|g| g.active_sessions()).await?}))
            }
            Request::ModelOptions(P(p)) => {
                Ok(self.model_options(&settings, p.session_id.as_deref()).await)
            }
            // Today's model requests for a provider; the writes a tainted session left for the
            // user to review. Ours, not Hermes's.
            Request::RequestsStatus(P(p)) => self.requests_status_call(p, &settings),
            Request::StagingList(_) => Ok(crate::staging::list(&self.runtime).await),
            Request::StagingApply(P(p)) => self.staging_decide(p, true).await,
            Request::StagingReject(P(p)) => self.staging_decide(p, false).await,
            Request::ConfigGet(P(p)) => self.config_get(p, &settings),
            Request::ConfigSet(P(p)) => self.config_set(p, &settings).await,
            Request::PetInfo(_) => Ok(Self::pet_info()),
            Request::SetupRuntimeCheck(_) => self.setup_runtime_check(&settings),
            Request::SharedMetricsStatus(_) => Ok(Self::shared_metrics_status()),
            Request::FreeTierStatus(_) => Ok(Self::free_tier_status()),
            Request::WakeStatus(_) => Ok(Self::wake_status()),
            Request::CommandsCatalog(_) => self.commands_catalog().await,
            Request::CompleteSlash(P(p)) => self.complete_slash(p).await,
            Request::SlashExec(P(p)) => self.slash_exec(p, &settings).await,
            Request::SessionStatus(P(p)) => self.session_status(p).await,
            Request::CommandDispatch(P(p)) => self.command_dispatch(p).await,
            Request::SessionTitle(P(p)) => self.session_title(p).await,
            Request::SessionMutate(P(Raw(p))) => self.mutate(&p, &settings).await,
            // These read the stored conversation or the checkpoints on disk.
            Request::SessionRewind(P(Raw(p))) => self.blocking(move |g| g.rewind_turn(&p)).await?,
            Request::RollbackList(P(Raw(p))) => {
                self.blocking(move |g| g.rollback("rollback.list", &p))
                    .await?
            }
            Request::RollbackDiff(P(Raw(p))) => {
                self.blocking(move |g| g.rollback("rollback.diff", &p))
                    .await?
            }
            Request::RollbackRestore(P(Raw(p))) => {
                self.blocking(move |g| g.rollback("rollback.restore", &p))
                    .await?
            }
            Request::SessionExport(P(Raw(p))) => {
                self.blocking(move |g| g.export_session(&p)).await?
            }
            Request::SessionForeignList(P(p)) => self.foreign_list(p).await,
            Request::SessionForeignPreview(P(p)) => self.foreign_preview(p).await,
            Request::SessionForeignImport(P(p)) => self.foreign_import(p, &settings).await,
            Request::SessionBranch(P(Raw(p))) => {
                self.session_branch(p, &settings, outbox, false).await
            }
            Request::SessionBranchWhole(P(Raw(p))) | Request::SessionBranchStored(P(Raw(p))) => {
                self.session_branch(p, &settings, outbox, true).await
            }
            Request::PromptBtw(P(p)) => self.prompt_btw(p, &settings),
            Request::PromptBackground(P(p)) => self.prompt_background(p, &settings),
            Request::FileAttach(P(Raw(p))) => self.file_attach(p).await,
            Request::ImageAttach(P(p)) => self.image_attach(p).await,
            Request::ImageAttachBytes(P(p)) => self.image_attach_bytes(p).await,
            Request::ImageDetach(P(p)) => self.image_detach(p),
            Request::InsightsGet(P(p)) => self.insights(p).await,
            Request::SessionCompress(P(Raw(p))) => self.compress_session(&p, &settings).await,
            Request::SessionDelete(P(p)) => self.session_delete(p).await,
            // Nothing to release: a session lives in the store, not in a worker.
            Request::SessionClose(_) => Ok(json!({"closed": true})),
            Request::SessionCreate(P(Raw(p))) => self.session_create(p, &settings, outbox).await,
            Request::SessionResume(P(Raw(p))) => self.session_resume(p, outbox, true).await,
            Request::SessionHistory(P(Raw(p))) => self.session_resume(p, outbox, false).await,
            Request::PromptSubmit(P(Raw(p))) => self.prompt_submit(p, &settings, outbox).await,
            Request::ApprovalRespond(P(p)) => self.respond(p, "approval"),
            Request::ClarifyRespond(P(p)) => self.respond(p, "clarify"),
            Request::SessionContextBreakdown(P(p)) => self.context_breakdown(&p.id()?).await,
            Request::ApprovalPending(P(p)) => self.approval_pending(p),
            Request::ProcessList(P(p)) => self.process_list(p),
            Request::ProcessKill(P(p)) => self.process_kill(p).await,
            Request::ProcessStop(P(p)) => self.process_stop(p).await,
            Request::SubagentList(P(p)) => self.subagent_list(p),
            Request::SubagentTail(P(p)) => self.subagent_tail(p),
            Request::SubagentSteer(P(p)) => self.subagent_steer(p),
            Request::SubagentInterrupt(P(p)) => self.subagent_interrupt(p),
            Request::DelegationStatus(_) => self.delegation_status(),
            Request::DelegationPause(P(p)) => self.delegation_pause(p),
            Request::CompletePath(P(p)) => self.complete_path(p).await,
            Request::SessionSteer(P(p)) => self.session_steer(p),
            Request::PromptCancel(P(p)) => self.prompt_cancel(p),
            Request::PromptSteerQueued(P(p)) => self.prompt_steer_queued(p),
            Request::PromptReplaceQueued(P(p)) => self.prompt_replace_queued(p),
            Request::SessionRedirect(P(p)) => self.session_redirect(p, outbox),
            Request::SessionInterrupt(P(p)) => self.session_interrupt(p),
        }
    }

    /// The effort picked for this session, if any (not Settings' default). Kept by the
    /// session's actor (`PageState::effort`).
    pub(crate) fn picked_effort(&self, id: &str) -> Option<String> {
        self.runtime.existing_session_handle(id)?.page().effort
    }

    /// The session's reasoning effort: picked in the composer, else Settings' default.
    pub(crate) fn effort(&self, id: &str) -> String {
        self.picked_effort(id)
            .or_else(|| {
                self.rest
                    .config_value("agent.reasoning_effort")
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }
}

/// The page's id for a `prompt.submit` (its admission id).
fn admission_id(params: &Value) -> Option<String> {
    params["submission_id"]
        .as_str()
        .or(params["input_id"].as_str())
        .map(str::to_string)
}

fn session_id(params: &Value) -> Result<String, RpcError> {
    params["session_id"]
        .as_str()
        .map(str::to_string)
        .ok_or(RpcError::invalid("session_id is required"))
}

fn send(outbox: &Outbox, frame: Value) {
    let _ = outbox.send(frame.to_string());
}

#[cfg(test)]
mod rpc_table_tests;
#[cfg(test)]
mod tests;
