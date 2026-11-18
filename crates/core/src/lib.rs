//! `hermes_core`: the backend's application layer, one crate every front end calls. The Tauri
//! page's gateway (`crates/desktop/src/gateway/`), the ACP agent (through that gateway) and the
//! native UI (`crates/native`) all reach the runtime through a [`Core`]: typed Rust calls and one
//! event stream ([`CoreEvent`]), no JSON-RPC and no Hermes field names. Those (the protocol's
//! *shape*) stay in `crates/desktop`; the *logic* lives here.
//!
//! What it holds beside the runtime: the questions waiting for the user (the runtime's
//! `UserBridge`, `questions.rs`), the event bus (`events.rs`), and the side files the store's frozen
//! schema can't hold (`branches.json`, `session-workspaces.json`, `archived-sessions.json`).
//!
//! | module | what |
//! |---|---|
//! | `events.rs` | `CoreEvent`, the bus, `subscribe` / `listen` |
//! | `questions.rs` | approvals and clarify questions: `CoreBridge` (the runtime's `UserBridge`) |
//! | `sessions.rs` | list, create, rename, delete, archive, branch, move to a folder, stop |
//! | `stream.rs` | a running turn's agent events as `CoreEvent`s |
//! | `transcript.rs` | a stored conversation as the user sees it, its rows, a failed turn's error |
//! | `projects.rs` | the sidebar's project tree |
//! | `branch.rs`, `archive.rs`, `workspace.rs` | the side files |
//! | `turns.rs` | a prompt and its turn: submit, the queue, the turn's loop, titles |
//! | `commands.rs` | the `/` menu: catalog, completion, running a command; what a message was |
//! | `attachments.rs` | images and files attached in the composer |
//! | `lock.rs` | "Hermes is already open": one app per data folder |
//! | `config.rs` | Hermes's config record (`desktop-config.json`), read over the defaults by dotted key |
//! | `providers.rs`, `endpoints.rs` | the providers in `config.json`, their keys, custom endpoints (add, test, default, remove) |
//! | `aux_models.rs`, `fallback.rs` | the side jobs' models (and "use a local model"), the fallback chain |
//! | `settings.rs` | Settings put on the runtime, the model picker and switching, a model's window, a session's provider, a turn's options |
//! | `usage.rs` | the status bar's usage, generation speed, the request ledger's numbers, activity |
//! | `toolsets.rs`, `skills.rs`, `profiles.rs` | Capabilities ▸ Tools and Skills, the SOUL.md editor |
//! | `maintenance.rs` | the skills curator (run, schedule, pause), the built-in memory files, background actions |
//! | `staging.rs`, `learning.rs` | the writes waiting for review, the Star map |
//! | `hermes_import.rs` | sessions from the original Hermes |
//! | `git.rs` | running `git` quietly |
//!
//! Threads (README "Architecture"): `Core` holds the tokio runtime's `Handle`; a front end on
//! another executor (GPUI) starts work with [`Core::spawn`]. Methods that touch the store or files
//! are synchronous and blocking: call them from `spawn_blocking` (ground rule 7), as the gateway's
//! `blocking` does; the `async` ones do their own.

pub mod archive;
pub mod attachments;
pub mod aux_models;
pub mod branch;
pub mod commands;
pub mod config;
pub mod delegation;
pub mod endpoints;
pub mod events;
pub mod export;
pub mod fallback;
pub mod files;
pub mod git;
pub mod git_review;
pub mod hermes_import;
pub mod learning;
pub mod lock;
pub mod maintenance;
pub mod open_file;
pub mod paths;
pub mod profiles;
pub mod projects;
pub mod providers;
pub mod questions;
pub mod sessions;
pub mod settings;
#[cfg(test)]
mod settings_tests;
pub mod skills;
pub mod staging;
pub mod stream;
pub mod terminal;
pub mod toolsets;
pub mod transcript;
pub mod turns;
pub mod usage;
pub mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use hermes_runtime::{Runtime, RuntimeConfig};
use hermes_tools::approval::ApprovalChoice;
use tokio::sync::mpsc;

pub use events::{CoreEvent, EventBus, Listener, QuestionKind};
pub use lock::{AppLock, LockError};
pub use questions::{APPROVAL_CHOICES, Ask, OpenQuestion};
pub use sessions::{Branch, BranchError, BranchPoint, BranchRequest, NewSession, SessionSummary};
pub use turns::{
    NoHooks, Outcome, SubmitError, SubmitTurn, Submitted, Truncate, TurnEnd, TurnHooks,
    TurnSettings,
};

/// The application layer over one data folder's runtime.
pub struct Core {
    runtime: Arc<Runtime>,
    handle: tokio::runtime::Handle,
    data_dir: PathBuf,
    events: Arc<EventBus>,
    questions: Arc<questions::Questions>,
    /// Hermes's config record, as Settings writes it (`config.rs`).
    config: Arc<config::ConfigRecord>,
    /// What the providers said about their models (windows, vision), asked once (`settings.rs`).
    models: settings::ModelCache,
    /// Each session's generation speed (`usage.rs`).
    generation: usage::Generation,
    /// Background actions followed by name: the curator's run (`maintenance.rs`).
    actions: Arc<maintenance::Actions>,
    /// Branch → the session it was branched from (`branch.rs`).
    branches: branch::Links,
    /// Session id → the git identity of its workspace (`workspace.rs`), kept beside the store.
    workspaces: Arc<workspace::Workspaces>,
    /// Archived sessions and the auto-archive sweep (`archive.rs`).
    archive: archive::Archive,
    /// A branch request's idempotency key → the branch it made (a retried request gets it again).
    branch_keys: Mutex<HashMap<String, String>>,
}

impl Core {
    /// Opens the data folder `config` names: the runtime (store, skills, tools; a first run seeds
    /// the bundled skills, so this takes a moment) with this core's bridge to the user, and the
    /// side files. Blocking: call it off the async workers.
    pub fn new(config: RuntimeConfig, handle: tokio::runtime::Handle) -> Result<Arc<Self>> {
        let events = Arc::new(EventBus::default());
        let questions = Arc::new(questions::Questions::new(events.clone()));
        let bridge = Arc::new(questions::CoreBridge {
            questions: questions.clone(),
            events: events.clone(),
        });
        let data_dir = config.data_dir.clone();
        let runtime = Arc::new(Runtime::new(config, bridge)?);
        Ok(Arc::new(Self {
            runtime,
            handle,
            branches: branch::Links::load(&data_dir),
            workspaces: Arc::new(workspace::Workspaces::load(&data_dir)),
            archive: archive::Archive::load(&data_dir),
            config: Arc::new(config::ConfigRecord::load(&data_dir)),
            actions: Arc::default(),
            models: Default::default(),
            generation: usage::Generation::load(&data_dir),
            data_dir,
            events,
            questions,
            branch_keys: Mutex::default(),
        }))
    }

    /// Puts the stored Settings on the runtime, once, right after [`Core::new`]: every front end
    /// calls it before it serves anything (`Core::new` applies nothing). In order: the config
    /// record's keys (`apply_settings`: approvals, sandbox, ledger limits, budget, checkpoints,
    /// memory, switched-off tools), the auto-archive sweep when it is on, the scheduled curator
    /// when `background` (the desktop app and the native UI; not the ACP agent, which may run
    /// beside the desktop app on the same folder), and the skills switched off in Capabilities ▸
    /// Skills. Blocking (files and the store): call it off the async workers.
    pub fn start(&self, background: bool) {
        self.apply_settings();
        self.sweep_archive();
        if background {
            self.schedule_curator();
        }
        skills::apply_config(&self.runtime.skills(), &self.config);
    }

    /// The runtime underneath (sessions, turns, tools, the store).
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// The data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Hermes's config record (`desktop-config.json`): Settings' keys, read and written one dotted
    /// key at a time.
    pub fn config(&self) -> &Arc<config::ConfigRecord> {
        &self.config
    }

    /// The background actions the page follows by name (the curator's run).
    pub fn actions(&self) -> &Arc<maintenance::Actions> {
        &self.actions
    }

    /// Runs the skills curator in the background (`CuratorStarted`, then `CuratorFinished`);
    /// `false` when a run is already going.
    pub fn run_curator(&self) -> bool {
        maintenance::run_curator(
            &self.handle,
            self.runtime.clone(),
            self.actions.clone(),
            self.events.clone(),
            self.data_dir.clone(),
        )
    }

    /// Starts the scheduled curator (checked every ten minutes; see `maintenance.rs`). The desktop
    /// app owns it; the ACP agent, which may run beside it on the same folder, does not.
    pub fn schedule_curator(&self) {
        maintenance::schedule_curator(
            &self.handle,
            self.runtime.clone(),
            self.actions.clone(),
            self.events.clone(),
            self.data_dir.clone(),
        );
    }

    /// The tokio runtime the core's work runs on.
    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }

    /// Starts `work` on the core's tokio runtime (for a front end on another executor).
    pub fn spawn<F>(&self, work: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(work)
    }

    /// Every event from now on, on a channel.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<CoreEvent> {
        self.events.subscribe()
    }

    /// Adds a listener called in order, in the publisher's thread, with every event.
    pub fn listen(&self, listener: Arc<dyn Listener>) {
        self.events.listen(listener);
    }

    /// Hands an event to the front ends; `true` when one showed it.
    pub fn publish(&self, event: CoreEvent) -> bool {
        self.events.publish(event)
    }

    /// The user's answer to an approval card. `false` when it is no longer open.
    pub fn answer_approval(&self, prompt_id: &str, choice: ApprovalChoice) -> bool {
        self.questions
            .answer(prompt_id, questions::choice_text(choice).to_string())
    }

    /// The user's answer to a clarify question: free text, or the picked choices joined with
    /// commas for a multi-select; empty skips it. `false` when it is no longer open.
    pub fn answer_clarify(&self, prompt_id: &str, answer: String) -> bool {
        self.questions.answer(prompt_id, answer)
    }

    /// An answer as the page sends it: the text, whatever kind of question the id names (an
    /// approval's `once` / `session` / `always`, anything else denies).
    pub fn answer(&self, prompt_id: &str, answer: String) -> bool {
        self.questions.answer(prompt_id, answer)
    }

    /// A session's open questions, for a front end that (re)opens it.
    pub fn open_questions(&self, session_id: &str) -> Vec<OpenQuestion> {
        self.questions.open(session_id)
    }

    /// Whether the session waits for the user (an open question).
    pub fn is_waiting(&self, session_id: &str) -> bool {
        self.questions.is_waiting(session_id)
    }

    /// Drops a session's open questions without stopping its turn's runtime (their cards close;
    /// an approval is denied).
    pub fn drop_questions(&self, session_id: &str) {
        self.questions.cancel_session(session_id);
    }

    /// The branch links (`branches.json`).
    pub fn branches(&self) -> &branch::Links {
        &self.branches
    }

    /// The sessions' recorded git identities (`session-workspaces.json`).
    pub fn workspaces(&self) -> &workspace::Workspaces {
        &self.workspaces
    }

    /// The archived sessions (`archived-sessions.json`).
    pub fn archive(&self) -> &archive::Archive {
        &self.archive
    }
}
