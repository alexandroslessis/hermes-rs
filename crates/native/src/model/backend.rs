//! What the app model asks of the backend, as one small trait, and its two implementations:
//! [`CoreBackend`] over `hermes_core::Core` (the real one: the user's projects and sessions) and
//! [`FixtureBackend`] (fake data in memory that records every operation, for the gallery, the
//! snapshots and the tests, and for `hermes-native --fixture`).
//!
//! The trait is the seam the tests use to check that a rename, an archive, a delete, a move or a
//! project edit reaches the right core operation: the model only ever calls [`Backend::run`] with
//! an [`Op`], and [`CoreBackend::run`] is the one place that turns an `Op` into `Core` calls (itself
//! tested against a real core on a temporary data folder).
//!
//! Every method here blocks (store reads and writes, git probes for the automatic projects): the
//! model calls them from GPUI's background executor, never from a handler or `render`
//! (ground rule 7). `delete` and `move` are async in the core; [`CoreBackend`] runs them on the
//! core's own tokio handle with `block_on`, which is allowed from a thread that is not a tokio
//! worker, as the background executor's threads are not.
//!
//! Hermes: none (the sidebar's data is the page's `projects.tree` and `session.list` in
//! `crates/desktop`, built from the same core calls).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermes_core::{
    Core, CoreEvent,
    archive::Filter,
    projects::{self, NodeKind, ProjectEdit, TreeRow},
    workspace::MoveError,
};
use hermes_store::Project;
use tokio::sync::mpsc;

use crate::sidebar::data::{
    NO_PROJECT, NO_PROJECT_LABEL, Op, ProjectItem, ProjectKind, Snapshot, ThreadItem,
};

/// The sidebar's view of the backend. Methods block; see the module docs.
pub trait Backend: Send + Sync + 'static {
    /// The projects and the (unarchived) threads now.
    fn snapshot(&self) -> Result<Snapshot, String>;
    /// Does one edit; the message is what the user sees if it fails.
    fn run(&self, op: &Op) -> Result<(), String>;
    /// Every core event from now on (`None`: a backend with no live events).
    fn events(&self) -> Option<mpsc::UnboundedReceiver<CoreEvent>> {
        None
    }
}

/// One stored session as the project tree groups it.
struct Row<'a>(&'a hermes_core::SessionSummary);

impl TreeRow for Row<'_> {
    fn cwd(&self) -> &str {
        &self.0.record.cwd
    }

    fn last_active(&self) -> Option<f64> {
        Some(self.0.record.updated_at as f64)
    }
}

/// The sidebar's list from the core's projects and sessions: the core's own grouping (a project
/// claims the sessions whose folder lies in one of its folders, then one automatic project per git
/// repository, then the rest), with the rest called "No project".
pub fn build_snapshot(
    user_projects: &[Project],
    sessions: &[hermes_core::SessionSummary],
) -> Snapshot {
    let rows: Vec<Row<'_>> = sessions.iter().map(Row).collect();
    let tree = projects::tree(user_projects, &rows);
    let mut snapshot = Snapshot::default();
    for node in &tree.nodes {
        let kind = match node.kind {
            NodeKind::Project => ProjectKind::User,
            NodeKind::Auto => ProjectKind::Auto,
            NodeKind::NoProject => ProjectKind::None,
        };
        let id = node.id.clone();
        snapshot.projects.push(ProjectItem {
            name: if kind == ProjectKind::None {
                NO_PROJECT_LABEL.to_string()
            } else {
                node.label.clone()
            },
            path: node.path.clone(),
            kind,
            id: id.clone(),
        });
        for row in &node.sessions {
            let s = row.0;
            snapshot.threads.push(ThreadItem {
                id: s.record.id.clone(),
                title: s.record.title.clone().unwrap_or_default(),
                project: id.clone(),
                cwd: s.record.cwd.clone(),
                pinned: s.record.pinned,
                updated_ms: s.record.updated_at,
                working: s.live || s.running,
                waiting: s.waiting,
                branch: s.git_branch.clone(),
            });
        }
    }
    snapshot
}

/// The real backend: a [`Core`].
pub struct CoreBackend {
    core: Arc<Core>,
}

impl CoreBackend {
    pub fn new(core: Arc<Core>) -> Self {
        Self { core }
    }

    /// The folder a thread moves into when dropped on `project`: a user project's primary folder,
    /// or an automatic project's repository folder (its id).
    fn folder_of(&self, project: &str) -> Option<PathBuf> {
        if project == NO_PROJECT {
            return None;
        }
        let user = self.core.projects();
        match user.iter().find(|p| p.id == project) {
            Some(p) => p.primary_folder().map(PathBuf::from),
            None => Some(PathBuf::from(project)).filter(|p| p.is_dir()),
        }
    }
}

impl Backend for CoreBackend {
    fn snapshot(&self) -> Result<Snapshot, String> {
        // `Core::sessions` reads an unreadable store as "no sessions"; the sidebar must not
        // show an empty list (and the first-run hero) for a database it cannot read, so the store
        // is asked once first and its error becomes the model's "Could not read your threads".
        self.core
            .runtime()
            .store()
            .list_sessions(1)
            .map_err(|e| format!("{e:#}"))?;
        let user = self.core.projects();
        let sessions = self.core.sessions(Filter::Exclude);
        Ok(build_snapshot(&user, &sessions))
    }

    fn run(&self, op: &Op) -> Result<(), String> {
        let core = &self.core;
        let failed = |e: anyhow::Error| format!("{e:#}");
        match op {
            Op::Rename { id, title } => core.rename(id, title).map_err(failed),
            Op::Pin { id, pinned } => core
                .runtime()
                .store()
                .set_pinned(id, *pinned)
                .map_err(failed),
            Op::Archive { id } => match core.set_archived(id, true).map_err(failed)? {
                true => Ok(()),
                false => Err("That thread no longer exists.".to_string()),
            },
            Op::Delete { id } => core
                .handle()
                .block_on(core.delete_session(id))
                .map_err(failed),
            Op::Move { id, project } => {
                let folder = self
                    .folder_of(project)
                    .ok_or_else(|| "That project has no folder to move into.".to_string())?;
                core.handle()
                    .block_on(core.move_session(id, folder))
                    .map(|_| ())
                    .map_err(|e| match e {
                        MoveError::InvalidFolder(folder) => {
                            format!("The folder {folder} no longer exists.")
                        }
                        MoveError::Busy => {
                            "A turn is running in this thread: move it when it finishes."
                                .to_string()
                        }
                        MoveError::Store(m) | MoveError::Stopped(m) => m,
                    })
            }
            Op::AddProject { folder, name } => core
                .create_project(name, vec![folder.clone()], Some(folder.clone()))
                .map(|_| ())
                .map_err(failed),
            Op::RenameProject { id, name } => match core
                .update_project(
                    id,
                    ProjectEdit {
                        name: Some(name.clone()),
                        ..ProjectEdit::default()
                    },
                )
                .map_err(failed)?
            {
                Some(_) => Ok(()),
                None => Err("That project no longer exists.".to_string()),
            },
            Op::RemoveProject { id } => core.delete_project(id).map_err(failed),
        }
    }

    fn events(&self) -> Option<mpsc::UnboundedReceiver<CoreEvent>> {
        Some(self.core.subscribe())
    }
}

/// Fake data in memory. Edits change the list as the core's would and are recorded; a failure can
/// be scripted; events are sent by hand.
pub struct FixtureBackend {
    state: Mutex<Snapshot>,
    log: Mutex<Vec<Op>>,
    failure: Mutex<Option<String>>,
    sender: Mutex<Option<mpsc::UnboundedSender<CoreEvent>>>,
}

impl FixtureBackend {
    pub fn new(snapshot: Snapshot) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(snapshot),
            log: Mutex::default(),
            failure: Mutex::default(),
            sender: Mutex::default(),
        })
    }

    /// The edits received so far, in order.
    pub fn ops(&self) -> Vec<Op> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Makes the next edits fail with `message` (`None`: work again).
    pub fn fail_with(&self, message: Option<&str>) {
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = message.map(str::to_string);
    }

    /// Sends a core event to whoever subscribed ([`Backend::events`]).
    pub fn emit(&self, event: CoreEvent) {
        if let Some(sender) = &*self.sender.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = sender.send(event);
        }
    }

    /// Replaces the list (a thread started working, a new one appeared).
    pub fn set_snapshot(&self, snapshot: Snapshot) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// Changes the stored list in place.
    pub fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

impl Backend for FixtureBackend {
    fn snapshot(&self) -> Result<Snapshot, String> {
        Ok(self.state.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn run(&self, op: &Op) -> Result<(), String> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(op.clone());
        if let Some(message) = &*self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(message.clone());
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(op);
        Ok(())
    }

    fn events(&self) -> Option<mpsc::UnboundedReceiver<CoreEvent>> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.sender.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
        Some(receiver)
    }
}
