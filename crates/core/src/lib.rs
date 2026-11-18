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
//! | `commands.rs` | what a typed message was as a command (the rest of the `/` menu: CP1.2) |
//! | `lock.rs` | "Hermes is already open": one app per data folder |
//! | `git.rs` | running `git` quietly |
//!
//! Threads (README "Architecture"): `Core` holds the tokio runtime's `Handle`; a front end on
//! another executor (GPUI) starts work with [`Core::spawn`]. Methods that touch the store or files
//! are synchronous and blocking: call them from `spawn_blocking` (ground rule 7), as the gateway's
//! `blocking` does; the `async` ones do their own.

pub mod archive;
pub mod branch;
pub mod commands;
pub mod events;
pub mod git;
pub mod lock;
pub mod projects;
pub mod questions;
pub mod sessions;
pub mod stream;
pub mod transcript;
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

/// The application layer over one data folder's runtime.
pub struct Core {
    runtime: Arc<Runtime>,
    handle: tokio::runtime::Handle,
    data_dir: PathBuf,
    events: Arc<EventBus>,
    questions: Arc<questions::Questions>,
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
            data_dir,
            events,
            questions,
            branch_keys: Mutex::default(),
        }))
    }

    /// The runtime underneath (sessions, turns, tools, the store).
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// The data folder (`%APPDATA%\hermes-rs`, or `HERMES_RS_HOME`).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
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
