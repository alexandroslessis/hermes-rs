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
//! The thread view's turn footer (CP4.2) asks for two more core operations through the same seam:
//! Retry ([`Backend::retry`]: `Core::submit` of the turn's typed prompt with the conversation cut
//! before it, `Truncate::BeforeRow`, as one step) and Branch ([`Backend::branch`]: `Core::branch` through the turn's last reply).
//!
//! The thread view (CP4.1) reads a thread's history a page at a time ([`Backend::history`]: the
//! stored messages through `hermes_core::transcript::entries`, read from the store, so opening a
//! thread does not wake its session in the runtime) and, in a `--dev` build of the app only, sends
//! a prompt ([`Backend::submit`]: `Core::submit`; the composer is Phase 5).
//!
//! The thread header (CP4.3) asks for what it shows ([`Backend::header_info`]: the thread's folder,
//! model and the git state of the folder) and for the file a save dialog writes
//! ([`Backend::export`]: `hermes_core::export::document` over the stored messages).
//!
//! Hermes: none (the sidebar's data is the page's `projects.tree` and `session.list` in
//! `crates/desktop`, built from the same core calls; the history is what `session.resume` reads).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermes_core::{
    BranchError, BranchPoint, BranchRequest, Core, CoreEvent, SubmitError, SubmitTurn, Truncate,
    archive::Filter,
    projects::{self, NodeKind, ProjectEdit, TreeRow},
    workspace::MoveError,
};
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_runtime::ProviderConfig;
use hermes_store::Project;
use tokio::sync::mpsc;

use crate::sidebar::data::{
    NO_PROJECT, NO_PROJECT_LABEL, Op, ProjectItem, ProjectKind, Snapshot, ThreadItem,
};
use crate::panel::{
    backend as panel_backend,
    data::{DiffData, DiffRequest},
};
use crate::thread::composer::ask::{Answer, Choice};
use crate::thread::composer::attach::{self, Prepared, Refusal};
use crate::thread::composer::data::{
    Controls, Mode, ModelChoices, ModelEntry, Pick, Picks, QueueOp, QueuedItem, SendRequest, Sent,
};
use crate::thread::composer::mention::{CommandOrigin, CommandSpec, Suggestion};
use crate::thread::data::{Attachment, HistoryItem, HistoryPage, items_from_entries, page};
use crate::thread::header_data::{Exported, GitState, GitSummary, HeaderInfo};
use crate::thread::recording::Recording;

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
    /// A page of a thread's history: at most `limit` items ending before item `before` (the
    /// newest when `None`).
    fn history(
        &self,
        _session: &str,
        _before: Option<usize>,
        _limit: usize,
    ) -> Result<HistoryPage, String> {
        Ok(HistoryPage::default())
    }
    /// Sends a prompt in a thread (the dev-only input until Phase 5's composer). The turn's
    /// events arrive through [`Backend::events`].
    fn submit(&self, _session: &str, _text: &str) -> Result<(), String> {
        Err("Sending a prompt is not available here.".to_string())
    }
    /// Retry: drops the turn whose typed prompt is stored row `user_row` and everything after it,
    /// then sends that prompt again. The new turn's events arrive through [`Backend::events`].
    fn retry(&self, _session: &str, _user_row: usize) -> Result<(), String> {
        Err("Retry is not available here.".to_string())
    }
    /// Branch from here: a new thread with the conversation through stored row `through_row`;
    /// the new thread's id.
    fn branch(&self, _session: &str, _through_row: usize) -> Result<String, String> {
        Err("Branching is not available here.".to_string())
    }
    /// What the thread header shows about a thread: its folder, model and the git state of the
    /// folder. Runs `git`, so it blocks.
    fn header_info(&self, _session: &str) -> Result<HeaderInfo, String> {
        Err("Thread details are not available here.".to_string())
    }
    /// The thread as a file's content (Markdown or HTML), for the save dialog.
    fn export(
        &self,
        _session: &str,
        _format: hermes_core::export::Format,
    ) -> Result<Exported, String> {
        Err("Exporting is not available here.".to_string())
    }
    /// What the composer's footer shows about a thread: its model, effort and mode, the queue
    /// behind a running turn and how full the context is. Reads the store and the core's
    /// session state, so it blocks.
    fn controls(&self, _session: &str) -> Result<Controls, String> {
        Err("Thread controls are not available here.".to_string())
    }
    /// The same for a new thread: Settings' default model, effort and mode.
    fn draft_controls(&self) -> Result<Controls, String> {
        Err("Thread controls are not available here.".to_string())
    }
    /// The models the configured endpoints list (`session`'s current one is marked by the caller).
    /// Asks the endpoints over the network, so it can be slow.
    fn model_choices(&self, _session: Option<&str>) -> Result<ModelChoices, String> {
        Err("The model list is not available here.".to_string())
    }
    /// A new, empty thread in `project`'s folder (a model `(provider, model)` for it, else
    /// Settings' default); the new thread's id. Nothing is sent.
    fn create_thread(
        &self,
        _project: &str,
        _model: Option<(String, String)>,
    ) -> Result<String, String> {
        Err("Creating a thread is not available here.".to_string())
    }
    /// Sends the composer's text in a thread: `picks` (effort, mode of a new thread) are applied
    /// first; then the prompt starts a turn, waits behind the running one, or is steered into it.
    /// The turn's events arrive through [`Backend::events`].
    fn send(&self, _session: &str, _request: &SendRequest, _picks: &Picks) -> Result<Sent, String> {
        Err("Sending a prompt is not available here.".to_string())
    }
    /// Stops the thread's running turn.
    fn stop(&self, _session: &str) -> Result<(), String> {
        Err("Stopping is not available here.".to_string())
    }
    /// The `@` menu's entries for `word` (`@src/ma`): the core's path completion in the folder of
    /// the thread (or, on the new-thread screen, of the project). Walks folders: it blocks.
    fn complete_path(
        &self,
        _scope: &CompletionScope,
        _word: &str,
    ) -> Result<Vec<Suggestion>, String> {
        Ok(Vec::new())
    }
    /// The `/` menu's entries the core lists (the coding commands, the skills and bundles). Reads
    /// the skills' folders: it blocks.
    fn commands(&self) -> Result<Vec<CommandSpec>, String> {
        Ok(Vec::new())
    }
    /// Runs `/name argument` through the core (`Core::run_command`); `session` is the thread the
    /// composer is in (`None` on the new-thread screen).
    fn run_command(
        &self,
        _session: Option<&str>,
        _name: &str,
        _argument: &str,
    ) -> Result<CommandAnswer, String> {
        Err("Commands are not available here.".to_string())
    }
    /// Reads what an attachment holds before it is shown in the strip: its size and kind, the
    /// thumbnail of an image. The limits are the core's. Reads files and decodes images: it blocks.
    fn prepare_attachment(&self, source: &AttachmentSource) -> Result<Prepared, Refusal> {
        match source {
            AttachmentSource::Path(path) => attach::prepare_path(path),
            AttachmentSource::Bytes { data, name } => attach::prepare_bytes(data.to_vec(), name),
        }
    }
    /// Takes a queued prompt out, runs it now inside the turn, or replaces its text.
    fn queue_op(&self, _session: &str, _op: &QueueOp) -> Result<(), String> {
        Err("The queue is not available here.".to_string())
    }
    /// A footer pick for a stored thread (the model applies from the next turn, as the core
    /// defers it while a turn runs).
    fn pick(&self, _session: &str, _pick: &Pick) -> Result<(), String> {
        Err("Changing this is not available here.".to_string())
    }
    /// Makes `provider`'s `model` the default for new threads (a pick on the new-thread screen).
    fn set_default_model(&self, _provider: &str, _model: &str) -> Result<(), String> {
        Err("Changing the default model is not available here.".to_string())
    }
    /// The user's answer to an approval or a clarify question (`Core::answer_approval` /
    /// `Core::answer_clarify`). `Ok(false)`: the question is no longer open (answered elsewhere,
    /// settled), which is not an error: asking twice is harmless. `Settled` follows on the events.
    fn answer(&self, _prompt_id: &str, _answer: &Answer) -> Result<bool, String> {
        Err("Answering is not available here.".to_string())
    }
    /// The right panel's diff: a Git-style patch of every changed file of `request.scope` in the
    /// thread's folder (CP6.1). Runs `git`, so it blocks.
    fn diff(&self, _request: &DiffRequest) -> Result<DiffData, String> {
        Err("The diff is not available here.".to_string())
    }
    /// Puts one changed file back (what [`DiffData::revert`] said the scope allows). `seen` is
    /// the file's blob id as the shown diff named it: the revert is refused when the file is no
    /// longer what the user saw (it changed after the diff was read). Blocks.
    fn revert_file(
        &self,
        _request: &DiffRequest,
        _path: &str,
        _seen: Option<&str>,
    ) -> Result<(), String> {
        Err("Reverting is not available here.".to_string())
    }
    /// The commit `HEAD` is at in the thread's folder: what a turn that is starting is compared
    /// with when it ends. `None` outside a repository.
    fn head_commit(&self, _session: &str) -> Option<String> {
        None
    }
    /// Opens a changed file (a path as the diff names it) in the user's editor at `line`; the
    /// name of what opened it ("Zed", "VS Code", "the default app").
    fn open_in_editor(
        &self,
        _session: &str,
        _path: &str,
        _line: Option<u32>,
    ) -> Result<String, String> {
        Err("Opening a file is not available here.".to_string())
    }
}

/// Where the `@` menu looks for files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionScope {
    /// The folder of this thread.
    Thread(String),
    /// The folder of this project (a new thread's).
    Project(String),
}

/// Something to attach, as the user gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentSource {
    /// A file (dropped, picked, or pasted from Explorer).
    Path(PathBuf),
    /// An image from the clipboard.
    Bytes { data: Arc<Vec<u8>>, name: String },
}

/// What running a command answered (`hermes_core::commands::CommandOutcome`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandAnswer {
    /// Text for the timeline; nothing is sent to the model.
    Output(String),
    /// A prompt to send as a normal turn, shown as `display`.
    Send { message: String, display: String },
    /// A skill or bundle loaded: `message` is the prompt, `display` what the timeline shows.
    Skill { message: String, display: String },
    /// Not a command of the core's: the text is an ordinary prompt.
    NotACommand,
}

/// What [`CoreBackend::send`] wrote for a prompt's attachments: the images' paths (under the data
/// folder's `images`), the `@file:` references of the files copied into the thread's
/// `.hermes/attachments`, and every file written, removed again when the prompt is not sent.
#[derive(Debug, Default)]
struct SavedAttachments {
    images: Vec<String>,
    references: Vec<String>,
    written: Vec<PathBuf>,
}

impl SavedAttachments {
    /// Removes what was written (a prompt the core did not take leaves no copies behind).
    fn discard(&self) {
        for path in &self.written {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// What a turn started by the native app asks of its front end: the session's provider (the
/// `--dev-provider` one when given), the options every turn runs with, computed when the turn
/// starts (so an effort picked since applies), and, between a turn and the next queued prompt,
/// the model picked while the turn ran (the core defers such a pick; the Tauri page does this in
/// its own hooks, `gateway/turn.rs`). No title model: a new thread gets the core's provisional
/// title from its first message.
pub struct NativeHooks {
    core: Arc<Core>,
    dev_provider: Option<ProviderConfig>,
    picked: PickedRoutes,
}

/// The model each session was given while a turn ran, until [`NativeHooks::between_turns`] has
/// applied it: the composer's footer reads it, so a read between the core taking the deferred pick
/// and storing it (the core publishes `Idle` before it applies the pick, with no event after)
/// still shows the picked model, without a timed second read.
type PickedRoutes = Arc<Mutex<HashMap<String, (String, String)>>>;

impl hermes_core::TurnHooks for NativeHooks {
    fn settings(&self, record: &hermes_store::SessionRecord) -> hermes_core::TurnSettings {
        let provider = self.dev_provider.clone().or_else(|| {
            hermes_core::providers::Settings::load(self.core.data_dir())
                .provider(&record.provider)
                .map(|p| p.runtime())
        });
        hermes_core::TurnSettings {
            provider,
            titles_configured: false,
            title_model: None,
        }
    }

    fn options(&self, session_id: &str) -> hermes_runtime::TurnOptions {
        self.core.turn_options(session_id)
    }

    fn between_turns(&self, session_id: &str) {
        // One step with a pick being made (`CoreBackend::pick` holds the lock across its switch).
        let mut picked = self.picked.lock().unwrap_or_else(|e| e.into_inner());
        self.core.apply_pending_route(session_id);
        // Stored now (or dropped by a failed move): the record says what the next turn runs on.
        picked.remove(session_id);
    }
}

/// The git state of a folder, for the header: not on disk, not in a repository, or the repository's
/// branch and changes. Runs `git status` (blocking).
pub fn git_state(cwd: &str) -> GitState {
    let folder = std::path::Path::new(cwd.trim());
    if cwd.trim().is_empty() || !folder.is_dir() {
        return GitState::NoFolder;
    }
    match hermes_core::git::repo_status(folder) {
        Some(status) => GitState::Repo(GitSummary::from_status(&status)),
        None => GitState::NoRepo,
    }
}

/// The composer's name for one of the runtime's modes.
pub fn mode_of_runtime(mode: hermes_tools::ApprovalMode) -> Mode {
    use hermes_tools::ApprovalMode as A;
    match mode {
        A::Manual => Mode::Manual,
        A::AcceptEdits => Mode::AcceptEdits,
        A::Auto => Mode::Auto,
        A::Plan => Mode::Plan,
        A::Yolo => Mode::Yolo,
    }
}

/// The runtime's name for a mode.
pub fn approval_mode(mode: Mode) -> hermes_tools::ApprovalMode {
    use hermes_tools::ApprovalMode as A;
    match mode {
        Mode::Manual => A::Manual,
        Mode::AcceptEdits => A::AcceptEdits,
        Mode::Auto => A::Auto,
        Mode::Plan => A::Plan,
        Mode::Yolo => A::Yolo,
    }
}

/// The images attached to a stored message, decoded (an image whose data does not decode is
/// left out: a thumbnail of nothing helps no one).
pub fn images_of(message: Option<&Message>) -> Vec<Attachment> {
    use base64::Engine as _;
    let Some(message) = message else {
        return Vec::new();
    };
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Image(image) => base64::engine::general_purpose::STANDARD
                .decode(image.data.as_bytes())
                .ok()
                .map(|bytes| (image.mime_type.clone(), bytes)),
            _ => None,
        })
        .enumerate()
        .map(|(ix, (mime, bytes))| Attachment {
            name: format!("image {}", ix + 1),
            mime,
            bytes: Arc::new(bytes),
        })
        .collect()
}

/// A page of a thread's history (as [`page`]) from its stored messages. Every message is turned
/// into an item (the page's place depends on all of them: the store has no ranged read), but
/// only the page's images are decoded.
pub fn history_page(messages: &[Message], before: Option<usize>, limit: usize) -> HistoryPage {
    let entries = hermes_core::transcript::entries(messages);
    let total = items_from_entries(&entries, |_, _| Vec::new()).len();
    let end = before.unwrap_or(total).min(total);
    let wanted = end.saturating_sub(limit)..end;
    let items = items_from_entries(&entries, |row, item| {
        if wanted.contains(&item) {
            images_of(row.checked_sub(1).and_then(|ix| messages.get(ix)))
        } else {
            Vec::new()
        }
    });
    page(&items, before, limit)
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
                asking: false,
                branch: s.git_branch.clone(),
            });
        }
    }
    snapshot
}

/// The real backend: a [`Core`].
pub struct CoreBackend {
    core: Arc<Core>,
    /// `--dev-provider <url>`: the dev-only input's prompts go to this OpenAI-compatible endpoint
    /// (a mock provider) instead of the thread's configured one.
    dev_provider: Option<ProviderConfig>,
    /// Models picked while a turn ran, until applied between turns (shared with the hooks).
    picked: PickedRoutes,
}

impl CoreBackend {
    pub fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            dev_provider: None,
            picked: PickedRoutes::default(),
        }
    }

    /// The provider a prompt of `session` goes to: `--dev-provider`'s, else the session's own.
    fn provider_of(&self, session: &str) -> Result<ProviderConfig, String> {
        match &self.dev_provider {
            Some(provider) => Ok(provider.clone()),
            None => self
                .core
                .session_provider(
                    session,
                    &hermes_core::providers::Settings::load(self.core.data_dir()),
                )
                .map_err(|e| e.to_string()),
        }
    }

    fn hooks(&self) -> Arc<NativeHooks> {
        Arc::new(NativeHooks {
            core: self.core.clone(),
            dev_provider: self.dev_provider.clone(),
            picked: self.picked.clone(),
        })
    }

    /// The mode a session runs in: its own while it is loaded in the runtime (a pick, `/yolo`),
    /// else the one it starts from when it is loaded, Settings' (`SessionTools::mode`: smart
    /// approvals are Auto, approvals off are YOLO, else Accept edits). Never a guess: an unloaded
    /// session forgets an earlier pick, and so does this.
    fn mode_of(&self, session: Option<&str>) -> Mode {
        let runtime = self.core.runtime();
        if let Some(mode) = session.and_then(|id| runtime.loaded_approval_mode(id)) {
            return mode_of_runtime(mode);
        }
        let approvals = runtime.approvals();
        if approvals.smart() {
            Mode::Auto
        } else if !approvals.asks() {
            Mode::Yolo
        } else {
            Mode::AcceptEdits
        }
    }

    fn set_mode(&self, session: &str, mode: Mode) -> Result<(), String> {
        self.core
            .runtime()
            .set_approval_mode(session, approval_mode(mode))
            .map_err(|e| format!("{e:#}"))
    }

    /// Sends the dev-only input's prompts to `base_url` (e.g. `scripts/perf/mock_llm.py`'s
    /// `http://127.0.0.1:47900/v1`).
    pub fn with_dev_provider(mut self, base_url: Option<String>) -> Self {
        self.dev_provider = base_url.map(|base_url| ProviderConfig {
            id: "dev-mock".into(),
            base_url,
            api_key: Some("mock".into()),
        });
        self
    }

    /// Saves the images of a prompt (under the data folder) and copies its other files into the
    /// thread's folder: the images' paths, and the `@file:` references to add to the text. The
    /// core refuses what is too large or not an image with its own sentence.
    fn save_attachments(
        &self,
        session: &str,
        attachments: &[attach::Attachment],
    ) -> Result<SavedAttachments, String> {
        let mut saved = SavedAttachments::default();
        match self.save_into(session, attachments, &mut saved) {
            Ok(()) => Ok(saved),
            Err(why) => {
                // Nothing of a prompt that was not sent stays behind.
                saved.discard();
                Err(why)
            }
        }
    }

    fn save_into(
        &self,
        session: &str,
        attachments: &[attach::Attachment],
        saved: &mut SavedAttachments,
    ) -> Result<(), String> {
        use base64::Engine as _;
        use hermes_core::attachments::{AttachSource, MAX_BYTES, image_from_base64};
        if attachments.is_empty() {
            return Ok(());
        }
        let cwd = self
            .core
            .runtime()
            .store()
            .session(session)
            .ok()
            .flatten()
            .map(|record| PathBuf::from(record.cwd));
        for attachment in attachments {
            let name = &attachment.name;
            match (attachment.kind, &attachment.source) {
                (attach::Kind::Image, source) => {
                    let bytes = match source {
                        attach::Source::Bytes { data, .. } => data.to_vec(),
                        attach::Source::Path(path) => {
                            // A file that grew since it was added is not read whole to be refused.
                            let size = std::fs::metadata(path)
                                .map_err(|e| format!("{name}: Can't read it ({e})."))?
                                .len();
                            if size as usize > MAX_BYTES {
                                return Err(format!(
                                    "{name}: The image is larger than {} MiB.",
                                    MAX_BYTES >> 20
                                ));
                            }
                            std::fs::read(path)
                                .map_err(|e| format!("{name}: Can't read it ({e})."))?
                        }
                    };
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let image = image_from_base64(self.core.data_dir(), &encoded, name)
                        .map_err(|e| format!("{name}: {}", e.message()))?;
                    saved.written.push(image.path.clone());
                    saved.images.push(image.path.display().to_string());
                }
                (attach::Kind::File, source) => {
                    let source = match source {
                        attach::Source::Path(path) => {
                            AttachSource::Path(path.display().to_string())
                        }
                        attach::Source::Bytes { data, .. } => AttachSource::DataUrl(
                            base64::engine::general_purpose::STANDARD.encode(data.as_slice()),
                        ),
                    };
                    let attached = self
                        .core
                        .attach_file(session, name, Some(source))
                        .map_err(|e| format!("{name}: {e}"))?;
                    // `ref_path` is relative to the thread's folder (absolute when outside it).
                    let copy = cwd
                        .as_ref()
                        .map_or_else(|| PathBuf::from(&attached.ref_path), |cwd| {
                            cwd.join(&attached.ref_path)
                        });
                    saved.written.push(copy);
                    saved.references.push(attached.ref_text);
                }
            }
        }
        Ok(())
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

    fn history(
        &self,
        session: &str,
        before: Option<usize>,
        limit: usize,
    ) -> Result<HistoryPage, String> {
        // From the store, not `Runtime::messages`: that opens the session in the runtime, and an
        // open session counts as live (the sidebar would show every thread looked at as working).
        let messages = self
            .core
            .runtime()
            .store()
            .load_messages(session)
            .map_err(|e| format!("{e:#}"))?;
        Ok(history_page(&messages, before, limit))
    }

    /// One step, not "rewind, then submit": everything that can refuse is checked before the
    /// conversation is cut (the prompt is read first; a missing provider fails here), and the cut
    /// is the submit's own (`SubmitTurn::truncate`, which the core refuses while a turn runs
    /// without cutting anything). A rewind followed by a failed submit would lose the turn and
    /// the prompt.
    fn retry(&self, session: &str, user_row: usize) -> Result<(), String> {
        let core = &self.core;
        let messages = core
            .runtime()
            .store()
            .load_messages(session)
            .map_err(|e| format!("{e:#}"))?;
        let typed = messages
            .get(hermes_core::transcript::kept_before_row(user_row))
            .filter(|m| hermes_core::transcript::is_typed(m))
            .map(hermes_runtime::preinject::user_text)
            .ok_or_else(|| "That message is no longer in the thread.".to_string())?;
        let provider = match &self.dev_provider {
            Some(provider) => provider.clone(),
            None => core
                .session_provider(
                    session,
                    &hermes_core::providers::Settings::load(core.data_dir()),
                )
                .map_err(|e| format!("Nothing was changed: {e}"))?,
        };
        let _ = provider;
        let hooks = self.hooks();
        let turn = SubmitTurn {
            text: typed,
            truncate: Some(Truncate::BeforeRow(user_row)),
            ..SubmitTurn::default()
        };
        // Core::submit spawns the turn on the core's runtime: this is a GPUI background
        // thread, not one of its workers.
        let _runtime = core.handle().enter();
        core.submit(session, turn, hooks)
            .map(|_| ())
            .map_err(|e| match e {
                SubmitError::Busy => {
                    "A turn is running in this thread: retry when it finishes.".to_string()
                }
                other => other.message().to_string(),
            })
    }

    fn branch(&self, session: &str, through_row: usize) -> Result<String, String> {
        let core = &self.core;
        let request = BranchRequest {
            parent: session.to_string(),
            point: BranchPoint::ThroughRow(through_row),
            cwd: None,
            name: None,
            idempotency_key: None,
        };
        let provider = || match &self.dev_provider {
            Some(provider) => Ok(provider.clone()),
            None => core.session_provider(
                session,
                &hermes_core::providers::Settings::load(core.data_dir()),
            ),
        };
        let _runtime = core.handle().enter();
        match core.branch(request, provider) {
            Ok(branch) => Ok(branch.child),
            Err(BranchError::UnknownSession(_)) => Err("That thread no longer exists.".to_string()),
            Err(BranchError::Empty) => Err("There is nothing to branch from there.".to_string()),
            Err(BranchError::Provider(e)) => Err(e.to_string()),
            Err(BranchError::Failed(e)) => Err(format!("{e:#}")),
        }
    }

    fn header_info(&self, session: &str) -> Result<HeaderInfo, String> {
        let record = self
            .core
            .runtime()
            .store()
            .session(session)
            .map_err(|e| format!("{e:#}"))?
            .ok_or_else(|| "That thread no longer exists.".to_string())?;
        Ok(HeaderInfo {
            git: git_state(&record.cwd),
            archived: self.core.archive().is_archived(session),
            cwd: record.cwd,
            model: record.model,
            provider: record.provider,
        })
    }

    /// Masked (`redact`), as a file that may be shared should be; the page's `/save` masks only
    /// when asked.
    fn export(
        &self,
        session: &str,
        format: hermes_core::export::Format,
    ) -> Result<Exported, String> {
        let store = self.core.runtime().store();
        let record = store
            .session(session)
            .map_err(|e| format!("{e:#}"))?
            .ok_or_else(|| "That thread no longer exists.".to_string())?;
        let messages = store.load_messages(session).map_err(|e| format!("{e:#}"))?;
        let entries = hermes_core::transcript::entries(&messages);
        let content = hermes_core::export::document(&record, &entries, format, true)
            .ok_or_else(|| "That format cannot be saved here.".to_string())?;
        Ok(Exported {
            content,
            file_name: hermes_core::export::file_name(&record, format),
        })
    }

    fn submit(&self, session: &str, text: &str) -> Result<(), String> {
        let core = &self.core;
        // The `--dev-send` driver keeps its old leniency: no provider is a failed turn, not a
        // refusal.
        let hooks = self.hooks();
        let turn = SubmitTurn {
            text: text.to_string(),
            ..SubmitTurn::default()
        };
        // As in retry: the turn is spawned on the core's runtime.
        let _runtime = core.handle().enter();
        core.submit(session, turn, hooks)
            .map(|_| ())
            .map_err(|e| e.message().to_string())
    }

    fn controls(&self, session: &str) -> Result<Controls, String> {
        let core = &self.core;
        let record = core
            .runtime()
            .store()
            .session(session)
            .map_err(|e| format!("{e:#}"))?
            .ok_or_else(|| "That thread no longer exists.".to_string())?;
        let settings = hermes_core::providers::Settings::load(core.data_dir());
        // A model picked while a turn runs already shows (it applies from the next turn); ours
        // until the hooks applied it, so the moment between the core taking it and storing it
        // shows it too.
        let picked = self
            .picked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned();
        let (provider, model) = picked
            .or_else(|| core.pending_route(session))
            .unwrap_or((record.provider.clone(), record.model));
        let queue = core
            .queue(session)
            .into_iter()
            .map(|turn| QueuedItem {
                id: turn.admission_id.unwrap_or_default(),
                text: turn.text,
                shown: turn.shown,
            })
            .collect();
        let usage = core.usage_snapshot(session, None);
        Ok(Controls {
            running: core.is_live(session) || core.runtime().is_running(session),
            queue,
            provider_name: settings
                .provider(&provider)
                .map_or_else(|| provider.clone(), |p| p.name.clone()),
            provider,
            model,
            effort: core.effort(session),
            mode: self.mode_of(Some(session)),
            context: usage.context.map(|c| (c.used, c.max)),
        })
    }

    fn draft_controls(&self) -> Result<Controls, String> {
        let core = &self.core;
        let settings = hermes_core::providers::Settings::load(core.data_dir());
        let provider = settings.active();
        let model = settings
            .model
            .clone()
            .or_else(|| provider.and_then(|p| p.default_model.clone()))
            .unwrap_or_default();
        Ok(Controls {
            provider: provider.map(|p| p.id.clone()).unwrap_or_default(),
            provider_name: provider.map(|p| p.name.clone()).unwrap_or_default(),
            model,
            effort: core
                .config()
                .value("agent.reasoning_effort")
                .as_str()
                .unwrap_or("")
                .to_string(),
            mode: self.mode_of(None),
            ..Controls::default()
        })
    }

    fn model_choices(&self, session: Option<&str>) -> Result<ModelChoices, String> {
        let core = &self.core;
        let settings = hermes_core::providers::Settings::load(core.data_dir());
        // Asks each endpoint's `/models` (async in the core): this is a background thread, not a
        // tokio worker, so `block_on` is allowed (as for `delete_session`).
        let options = core
            .handle()
            .block_on(core.model_options(&settings, session));
        let mut entries = Vec::new();
        for provider in options.providers {
            for model in provider.models {
                entries.push(ModelEntry {
                    provider: provider.id.clone(),
                    provider_name: provider.name.clone(),
                    model,
                });
            }
        }
        Ok(ModelChoices { entries })
    }

    fn create_thread(
        &self,
        project: &str,
        model: Option<(String, String)>,
    ) -> Result<String, String> {
        let core = &self.core;
        let settings = hermes_core::providers::Settings::load(core.data_dir());
        let chosen = model.as_ref().and_then(|(id, _)| settings.provider(id));
        let provider = match (&self.dev_provider, chosen.or(settings.active())) {
            (Some(provider), _) => provider.clone(),
            (None, Some(provider)) => provider.runtime(),
            (None, None) => return Err("No provider is configured.".to_string()),
        };
        let model = model
            .map(|(_, model)| model)
            .or_else(|| settings.model.clone())
            .or_else(|| {
                settings
                    .active()
                    .and_then(|provider| provider.default_model.clone())
            })
            .filter(|m| !m.is_empty())
            .ok_or_else(|| "No model is chosen.".to_string())?;
        let cwd = self
            .folder_of(project)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("C:/"));
        let (record, _) = core
            .create_session(hermes_core::NewSession {
                provider,
                model,
                cwd,
                title: None,
                seeded: Vec::new(),
                parent: None,
            })
            .map_err(|e| format!("{e:#}"))?;
        Ok(record.id)
    }

    fn send(&self, session: &str, request: &SendRequest, picks: &Picks) -> Result<Sent, String> {
        let core = &self.core;
        // Refused before anything is changed: no provider, no turn.
        let provider = self.provider_of(session)?;
        // Images are saved for the prompt, other files copied into the thread's folder and named
        // in it (`@file:`): the core's own checks run here, and its sentence is what is shown.
        let saved = self.save_attachments(session, &request.attachments)?;
        let mut text = request.text.clone();
        if !saved.references.is_empty() {
            text = format!("{text}\n\n{}", saved.references.join("\n"));
        }
        if let Some(effort) = &picks.effort {
            core.pick_effort(session, effort.clone());
        }
        if let Some(mode) = picks.mode
            && let Err(why) = self.set_mode(session, mode)
        {
            saved.discard();
            return Err(why);
        }
        // The turn is spawned on the core's runtime; this is a GPUI background thread.
        let _runtime = core.handle().enter();
        // A steer carries text only: with attachments the prompt waits its turn instead.
        if request.steer
            && request.attachments.is_empty()
            && core.runtime().is_running(session)
            && core.steer(session, &text)
        {
            return Ok(Sent::Steered);
        }
        let turn = SubmitTurn {
            admission_id: Some(request.admission_id.clone()).filter(|id| !id.is_empty()),
            text,
            images: saved.images.clone(),
            ..SubmitTurn::default()
        };
        let _ = provider;
        match core.submit(session, turn, self.hooks()) {
            Ok(hermes_core::Submitted::Started { .. }) => Ok(Sent::Started),
            Ok(hermes_core::Submitted::Queued { .. }) => Ok(Sent::Queued),
            Err(e) => {
                saved.discard();
                Err(e.message().to_string())
            }
        }
    }

    fn complete_path(
        &self,
        scope: &CompletionScope,
        word: &str,
    ) -> Result<Vec<Suggestion>, String> {
        let root = match scope {
            CompletionScope::Thread(session) => self
                .core
                .runtime()
                .store()
                .session(session)
                .ok()
                .flatten()
                .map(|record| PathBuf::from(record.cwd)),
            CompletionScope::Project(project) => self.folder_of(project),
        }
        .or_else(dirs::home_dir)
        .unwrap_or_default();
        Ok(hermes_core::paths::complete(word, &root)
            .into_iter()
            .map(|item| Suggestion::from_item(&item.text, &item.display, &item.meta))
            .collect())
    }

    fn commands(&self) -> Result<Vec<CommandSpec>, String> {
        use hermes_core::commands::Origin;
        Ok(self
            .core
            .commands()
            .into_iter()
            .map(|entry| {
                let origin = match entry.origin {
                    Origin::Builtin => CommandOrigin::Builtin,
                    _ => CommandOrigin::Skill,
                };
                CommandSpec::from_core(&entry.name, &entry.description, origin)
            })
            .collect())
    }

    fn run_command(
        &self,
        session: Option<&str>,
        name: &str,
        argument: &str,
    ) -> Result<CommandAnswer, String> {
        use hermes_core::commands::{CommandError, CommandOutcome, CoreCommands};
        let settings = hermes_core::providers::Settings::load(self.core.data_dir());
        let env = CoreCommands::with_settings(&self.core, settings);
        // Called from a GPUI background thread: the core's runtime does the async part.
        let answered = self
            .core
            .handle()
            .block_on(self.core.run_command(session, name, argument, &env));
        match answered {
            Ok(CommandOutcome::Output(text)) => Ok(CommandAnswer::Output(text)),
            Ok(CommandOutcome::Send { message, display }) => {
                Ok(CommandAnswer::Send { message, display })
            }
            Ok(CommandOutcome::Skill {
                message, display, ..
            }) => Ok(CommandAnswer::Skill { message, display }),
            Err(CommandError::NotACommand) => Ok(CommandAnswer::NotACommand),
            Err(CommandError::Invalid(why)) | Err(CommandError::Failed(why)) => Err(why),
        }
    }

    fn stop(&self, session: &str) -> Result<(), String> {
        self.core.stop(session);
        Ok(())
    }

    fn queue_op(&self, session: &str, op: &QueueOp) -> Result<(), String> {
        let core = &self.core;
        let hooks = self.hooks();
        match op {
            QueueOp::Remove(id) => {
                core.remove_queued(session, id, &*hooks);
                Ok(())
            }
            QueueOp::Steer(id) => core
                .steer_queued(session, id, &*hooks)
                .map_err(|e| match e {
                    hermes_core::turns::SteerQueued::Idle => {
                        "No turn is running: the prompt starts by itself.".to_string()
                    }
                    hermes_core::turns::SteerQueued::Started => {
                        "That prompt has already started.".to_string()
                    }
                }),
            QueueOp::Replace { id, text, new_id } => core
                .replace_queued(
                    session,
                    id,
                    text.clone(),
                    Some(new_id.clone()).filter(|id| !id.is_empty()),
                    &*hooks,
                )
                .map(|_| ())
                .ok_or_else(|| "That prompt has already started.".to_string()),
        }
    }

    fn pick(&self, session: &str, pick: &Pick) -> Result<(), String> {
        let core = &self.core;
        match pick {
            Pick::Model { provider, model } => {
                let settings = hermes_core::providers::Settings::load(core.data_dir());
                // Held across the switch: the hooks applying a pick between turns wait for it, so
                // the two are one after the other (never a pick remembered after it was applied).
                let mut picked = self.picked.lock().unwrap_or_else(|e| e.into_inner());
                core.switch_model(
                    &hermes_core::settings::ModelSwitch {
                        model: model.clone(),
                        provider: Some(provider.clone()),
                        // A thread's pick is the thread's; the new-thread screen sets the default.
                        session_only: true,
                        session_id: Some(session.to_string()),
                    },
                    &settings,
                    &|_| {},
                )?;
                // Deferred (a turn runs): remembered until the hooks apply it between turns. Not
                // deferred (applied now, or the running model picked again): nothing pending.
                match core.pending_route(session) {
                    Some(route) => picked.insert(session.to_string(), route),
                    None => picked.remove(session),
                };
                Ok(())
            }
            Pick::Effort(effort) => {
                core.pick_effort(session, effort.clone());
                Ok(())
            }
            Pick::Mode(mode) => self.set_mode(session, *mode),
        }
    }

    fn set_default_model(&self, provider: &str, model: &str) -> Result<(), String> {
        hermes_core::providers::save_default(self.core.data_dir(), provider, model)
            .map_err(|e| format!("{e:#}"))
    }

    fn diff(&self, request: &DiffRequest) -> Result<DiffData, String> {
        panel_backend::read_diff(&self.core, request)
    }

    fn revert_file(
        &self,
        request: &DiffRequest,
        path: &str,
        seen: Option<&str>,
    ) -> Result<(), String> {
        panel_backend::revert(&self.core, request, path, seen)
    }

    fn head_commit(&self, session: &str) -> Option<String> {
        panel_backend::head_commit(&self.core, session)
    }

    fn open_in_editor(
        &self,
        session: &str,
        path: &str,
        line: Option<u32>,
    ) -> Result<String, String> {
        panel_backend::open_in_editor(&self.core, session, path, line)
    }

    fn answer(&self, prompt_id: &str, answer: &Answer) -> Result<bool, String> {
        use hermes_tools::approval::ApprovalChoice;
        Ok(match answer {
            Answer::Approval(choice) => self.core.answer_approval(
                prompt_id,
                match choice {
                    Choice::Once => ApprovalChoice::Once,
                    Choice::Session => ApprovalChoice::Session,
                    Choice::Always => ApprovalChoice::Always,
                    Choice::Deny => ApprovalChoice::Deny,
                },
            ),
            Answer::Text(text) => self.core.answer_clarify(prompt_id, text.clone()),
        })
    }
}

/// Fake data in memory. Edits change the list as the core's would and are recorded; a failure can
/// be scripted; events are sent by hand. Threads have the fixture's histories
/// ([`super::fixture::history`]) unless given others; a prompt sent in one replays the recorded
/// session (`--fixture --dev`) when a recording is set, else it is only recorded.
pub struct FixtureBackend {
    state: Mutex<Snapshot>,
    log: Mutex<Vec<Op>>,
    failure: Mutex<Option<String>>,
    sender: Mutex<Option<mpsc::UnboundedSender<CoreEvent>>>,
    histories: Mutex<HashMap<String, Arc<Vec<HistoryItem>>>>,
    /// Pages asked for, as `(session, before)`.
    pages: Mutex<Vec<(String, Option<usize>)>>,
    sent: Mutex<Vec<(String, String)>>,
    replay: Mutex<Option<Recording>>,
    thread_ops: Mutex<Vec<ThreadOp>>,
    /// The header's answers set by a test or a story (a thread without one gets
    /// [`FixtureBackend::default_header`]).
    headers: Mutex<HashMap<String, HeaderInfo>>,
    header_reads: Mutex<usize>,
    exports: Mutex<Vec<(String, hermes_core::export::Format)>>,
    /// The composer's footer: each thread's controls (a thread without set ones gets the default).
    controls: Mutex<HashMap<String, Controls>>,
    choices: Mutex<ModelChoices>,
    /// What the composer asked for, in order.
    composer_log: Mutex<Vec<ComposerOp>>,
    /// The `@` menu's entries (those whose text holds the word asked for are returned).
    paths: Mutex<Vec<Suggestion>>,
    /// The `/` menu's entries from the core.
    catalog: Mutex<Vec<CommandSpec>>,
    /// Answers of commands set by a test; a command without one answers with a line of output.
    answers: Mutex<HashMap<String, CommandAnswer>>,
    /// The questions [`FixtureBackend::ask`] opened and no answer closed yet, with the session
    /// and kind their `Settled` carries.
    open_asks: Mutex<HashMap<String, (String, hermes_core::QuestionKind)>>,
    /// The questions an answer reached (a second answer is "no longer open").
    answered_asks: Mutex<std::collections::HashSet<String>>,
    /// The right panel's diff: what each scope answers, and what was asked of it.
    diffs: Mutex<crate::panel::fixture::FixtureDiffs>,
}

/// A composer operation that reached a [`FixtureBackend`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerOp {
    /// A send, with the picks it carried.
    Send {
        session: String,
        request: SendRequest,
        picks: Picks,
    },
    Stop(String),
    Queue {
        session: String,
        op: QueueOp,
    },
    Pick {
        session: String,
        pick: Pick,
    },
    /// A new thread in `project`, with the model chosen for it.
    Create {
        project: String,
        model: Option<(String, String)>,
    },
    DefaultModel {
        provider: String,
        model: String,
    },
    /// The `@` menu asked for `word`.
    Complete {
        scope: CompletionScope,
        word: String,
    },
    /// A command was run.
    Command {
        session: Option<String>,
        name: String,
        argument: String,
    },
    /// An attachment was read for the strip.
    Prepare(AttachmentSource),
    /// An approval or a clarify question was answered.
    Answer { prompt_id: String, answer: Answer },
}

/// A turn footer's action that reached a [`FixtureBackend`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadOp {
    Retry { session: String, user_row: usize },
    Branch { session: String, through_row: usize },
}

impl FixtureBackend {
    pub fn new(snapshot: Snapshot) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(snapshot),
            log: Mutex::default(),
            failure: Mutex::default(),
            sender: Mutex::default(),
            histories: Mutex::default(),
            pages: Mutex::default(),
            sent: Mutex::default(),
            replay: Mutex::default(),
            thread_ops: Mutex::default(),
            headers: Mutex::default(),
            header_reads: Mutex::default(),
            exports: Mutex::default(),
            controls: Mutex::default(),
            choices: Mutex::new(Self::default_choices()),
            composer_log: Mutex::default(),
            paths: Mutex::new(Self::default_paths()),
            catalog: Mutex::new(Self::default_catalog()),
            answers: Mutex::default(),
            open_asks: Mutex::default(),
            answered_asks: Mutex::default(),
            diffs: Mutex::new(crate::panel::fixture::FixtureDiffs::sample()),
        })
    }

    /// The right panel's fake repository (what each scope answers, what was asked of it).
    pub fn diffs(&self) -> std::sync::MutexGuard<'_, crate::panel::fixture::FixtureDiffs> {
        self.diffs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The models a fixture offers: two providers, a handful of models.
    pub fn default_choices() -> ModelChoices {
        let entry = |provider: &str, name: &str, model: &str| ModelEntry {
            provider: provider.into(),
            provider_name: name.into(),
            model: model.into(),
        };
        ModelChoices {
            entries: vec![
                entry("fixture", "Fixture", "fixture-model"),
                entry("fixture", "Fixture", "fixture-large"),
                entry("local", "Local", "qwen3-coder"),
                entry("local", "Local", "llama-3.3-70b"),
            ],
        }
    }

    /// The files the `@` menu offers in the fixture.
    pub fn default_paths() -> Vec<Suggestion> {
        let items = [
            ("@file:src/main.rs", "main.rs", "src"),
            ("@file:src/lib.rs", "lib.rs", "src"),
            ("@file:README.md", "README.md", ""),
            ("@file:Cargo.toml", "Cargo.toml", ""),
            ("@folder:src/", "src/", "dir"),
            ("@folder:docs/", "docs/", "dir"),
        ];
        items
            .iter()
            .map(|(text, display, meta)| Suggestion::from_item(text, display, meta))
            .collect()
    }

    /// The core's commands in the fixture.
    pub fn default_catalog() -> Vec<CommandSpec> {
        vec![
            CommandSpec::from_core(
                "plan",
                "Write a markdown implementation plan without executing anything",
                CommandOrigin::Builtin,
            ),
            CommandSpec::from_core(
                "review",
                "Spawn an independent subagent to review the work just discussed",
                CommandOrigin::Builtin,
            ),
            CommandSpec::from_core(
                "init",
                "Generate or update AGENTS.md project instructions from a repo scan",
                CommandOrigin::Builtin,
            ),
            CommandSpec::from_core(
                "deploy-checklist",
                "Walk through the release checklist",
                CommandOrigin::Skill,
            ),
        ]
    }

    /// The `@` menu's entries.
    pub fn set_paths(&self, paths: Vec<Suggestion>) {
        *self.paths.lock().unwrap_or_else(|e| e.into_inner()) = paths;
    }

    /// The `/` menu's entries from the core.
    pub fn set_catalog(&self, catalog: Vec<CommandSpec>) {
        *self.catalog.lock().unwrap_or_else(|e| e.into_inner()) = catalog;
    }

    /// What `/name` answers.
    pub fn set_answer(&self, name: &str, answer: CommandAnswer) {
        self.answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), answer);
    }

    /// What a thread's controls read before a test sets them: the fixture model, medium effort,
    /// the default mode, nothing queued, nothing running.
    pub fn default_controls() -> Controls {
        Controls {
            provider: "fixture".into(),
            provider_name: "Fixture".into(),
            model: "fixture-model".into(),
            effort: "medium".into(),
            ..Controls::default()
        }
    }

    /// Gives a thread these controls (the key `""` is the new-thread screen's).
    pub fn set_controls(&self, session: &str, controls: Controls) {
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), controls);
    }

    /// Changes a thread's controls in place.
    pub fn update_controls(&self, session: &str, f: impl FnOnce(&mut Controls)) {
        let mut all = self.controls.lock().unwrap_or_else(|e| e.into_inner());
        f(all
            .entry(session.to_string())
            .or_insert_with(Self::default_controls));
    }

    /// The model list the picker gets.
    pub fn set_choices(&self, choices: ModelChoices) {
        *self.choices.lock().unwrap_or_else(|e| e.into_inner()) = choices;
    }

    /// What the composer asked for so far, in order.
    pub fn composer_ops(&self) -> Vec<ComposerOp> {
        self.composer_log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn log_composer(&self, op: ComposerOp) {
        self.composer_log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(op);
    }

    fn scripted_failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Gives a thread this header (folder, model, git state).
    pub fn set_header(&self, session: &str, info: HeaderInfo) {
        self.headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), info);
    }

    /// How many times the header was read (the "not per frame" tests count it).
    pub fn header_reads(&self) -> usize {
        *self.header_reads.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The exports asked for so far, as `(session, format)`.
    pub fn exports(&self) -> Vec<(String, hermes_core::export::Format)> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// What a thread without a set header answers: its listed folder and branch, a model.
    pub fn default_header(&self, session: &str) -> Option<HeaderInfo> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let thread = state.threads.iter().find(|t| t.id == session)?;
        Some(HeaderInfo {
            cwd: thread.cwd.clone(),
            model: "fixture-model".to_string(),
            provider: "fixture".to_string(),
            archived: false,
            git: match &thread.branch {
                Some(branch) => GitState::Repo(GitSummary {
                    branch: Some(branch.clone()),
                    ..GitSummary::default()
                }),
                None => GitState::NoRepo,
            },
        })
    }

    /// Gives a thread this history.
    pub fn set_history(&self, session: &str, items: Vec<HistoryItem>) {
        self.histories
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), Arc::new(items));
    }

    /// The pages asked for so far, as `(session, before)`.
    pub fn pages(&self) -> Vec<(String, Option<usize>)> {
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The prompts sent so far, as `(session, text)`.
    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Makes a sent prompt replay `recording`'s events (one every `step`), as a mock provider's
    /// turn would arrive.
    pub fn replay_on_submit(&self, recording: Recording) {
        *self.replay.lock().unwrap_or_else(|e| e.into_inner()) = Some(recording);
    }

    /// The footer actions received so far (Retry, Branch), in order.
    pub fn thread_ops(&self) -> Vec<ThreadOp> {
        self.thread_ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The edits received so far, in order.
    pub fn ops(&self) -> Vec<Op> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Makes the next edits fail with `message` (`None`: work again).
    pub fn fail_with(&self, message: Option<&str>) {
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = message.map(str::to_string);
    }

    /// Opens a question as the core would: sends its `ApprovalRequested` / `ClarifyRequested`
    /// event and remembers it, so [`Backend::answer`] closes it with a `Settled` and a second
    /// answer finds it closed.
    pub fn ask(&self, event: CoreEvent) {
        let kind = match &event {
            CoreEvent::ApprovalRequested { .. } => hermes_core::QuestionKind::Approval,
            CoreEvent::ClarifyRequested { .. } => hermes_core::QuestionKind::Clarify,
            _ => return,
        };
        let prompt_id = match &event {
            CoreEvent::ApprovalRequested { prompt_id, .. }
            | CoreEvent::ClarifyRequested { prompt_id, .. } => prompt_id.clone(),
            _ => return,
        };
        self.open_asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(prompt_id, (event.session_id().to_string(), kind));
        self.emit(event);
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

    fn history(
        &self,
        session: &str,
        before: Option<usize>,
        limit: usize,
    ) -> Result<HistoryPage, String> {
        self.pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((session.to_string(), before));
        let items = self
            .histories
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
            .unwrap_or_else(|| Arc::new(super::fixture::history(session)));
        Ok(page(&items, before, limit))
    }

    fn retry(&self, session: &str, user_row: usize) -> Result<(), String> {
        self.thread_ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ThreadOp::Retry {
                session: session.to_string(),
                user_row,
            });
        if let Some(message) = &*self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(message.clone());
        }
        Ok(())
    }

    fn branch(&self, session: &str, through_row: usize) -> Result<String, String> {
        self.thread_ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ThreadOp::Branch {
                session: session.to_string(),
                through_row,
            });
        if let Some(message) = &*self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(message.clone());
        }
        // The branch is a new thread beside its parent, titled as the core would.
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let child = format!("{session}-branch");
        if let Some(parent) = state.threads.iter().find(|t| t.id == session).cloned() {
            let mut thread = parent;
            thread.id = child.clone();
            thread.title = format!("{} (branch)", thread.title);
            thread.pinned = false;
            thread.working = false;
            thread.waiting = false;
            state.threads.push(thread);
        }
        Ok(child)
    }

    fn header_info(&self, session: &str) -> Result<HeaderInfo, String> {
        *self.header_reads.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        if let Some(info) = self
            .headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
        {
            return Ok(info.clone());
        }
        self.default_header(session)
            .ok_or_else(|| "That thread no longer exists.".to_string())
    }

    fn export(
        &self,
        session: &str,
        format: hermes_core::export::Format,
    ) -> Result<Exported, String> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((session.to_string(), format));
        if let Some(message) = &*self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(message.clone());
        }
        let title = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thread(session)
            .map(|t| t.display_title().to_string())
            .ok_or_else(|| "That thread no longer exists.".to_string())?;
        Ok(Exported {
            content: format!("# {title}\n"),
            file_name: format!("hermes-{session}.{}", format.extension()),
        })
    }

    fn submit(&self, session: &str, text: &str) -> Result<(), String> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((session.to_string(), text.to_string()));
        if let Some(message) = &*self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(message.clone());
        }
        let replay = self
            .replay
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let (Some(recording), Some(sender)) = (replay, sender) {
            let session = session.to_string();
            // A plain thread, as a provider's stream would arrive: the model drains the channel
            // on its own task. Only `--fixture --dev` sets a replay.
            std::thread::spawn(move || {
                for event in recording.events_for(&session) {
                    std::thread::sleep(crate::thread::recording::REPLAY_STEP);
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(())
    }

    fn controls(&self, session: &str) -> Result<Controls, String> {
        Ok(self
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
            .unwrap_or_else(Self::default_controls))
    }

    fn draft_controls(&self) -> Result<Controls, String> {
        Ok(self
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("")
            .cloned()
            .unwrap_or_else(Self::default_controls))
    }

    fn model_choices(&self, _session: Option<&str>) -> Result<ModelChoices, String> {
        Ok(self
            .choices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    fn create_thread(
        &self,
        project: &str,
        model: Option<(String, String)>,
    ) -> Result<String, String> {
        self.log_composer(ComposerOp::Create {
            project: project.to_string(),
            model: model,
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = format!("new-{}", state.threads.len() + 1);
        let cwd = state
            .project(project)
            .and_then(|p| p.path.clone())
            .unwrap_or_default();
        state.threads.push(crate::sidebar::data::ThreadItem {
            id: id.clone(),
            title: String::new(),
            project: project.to_string(),
            cwd,
            pinned: false,
            updated_ms: crate::model::fixture::NOW_MS,
            working: false,
            waiting: false,
            asking: false,
            branch: None,
        });
        Ok(id)
    }

    fn send(&self, session: &str, request: &SendRequest, picks: &Picks) -> Result<Sent, String> {
        self.log_composer(ComposerOp::Send {
            session: session.to_string(),
            request: request.clone(),
            picks: picks.clone(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        let running = self
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .is_some_and(|c| c.running);
        if running && request.steer {
            return Ok(Sent::Steered);
        }
        if running {
            self.update_controls(session, |c| {
                c.queue.push(QueuedItem {
                    id: request.admission_id.clone(),
                    text: request.text.clone(),
                    shown: false,
                })
            });
            self.emit(CoreEvent::QueueChanged {
                session_id: session.to_string(),
            });
            return Ok(Sent::Queued);
        }
        // Not running: it starts (a replay set with `replay_on_submit` plays out).
        self.submit(session, &request.text)?;
        Ok(Sent::Started)
    }

    fn complete_path(
        &self,
        scope: &CompletionScope,
        word: &str,
    ) -> Result<Vec<Suggestion>, String> {
        self.log_composer(ComposerOp::Complete {
            scope: scope.clone(),
            word: word.to_string(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        let lower = word.trim_start_matches('@').to_lowercase();
        let query = lower
            .strip_prefix("file:")
            .or_else(|| lower.strip_prefix("folder:"))
            .unwrap_or(&lower)
            .to_string();
        Ok(self
            .paths
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| query.is_empty() || s.text.to_lowercase().contains(&query))
            .cloned()
            .collect())
    }

    fn commands(&self) -> Result<Vec<CommandSpec>, String> {
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        Ok(self
            .catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    fn run_command(
        &self,
        session: Option<&str>,
        name: &str,
        argument: &str,
    ) -> Result<CommandAnswer, String> {
        self.log_composer(ComposerOp::Command {
            session: session.map(str::to_string),
            name: name.to_string(),
            argument: argument.to_string(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        if let Some(answer) = self
            .answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Ok(answer.clone());
        }
        let known = self
            .catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|c| c.name == name)
            || crate::thread::composer::mention::report_commands()
                .iter()
                .any(|c| c.name == name);
        Ok(if known {
            CommandAnswer::Output(
                format!("Output of /{name} {argument}")
                    .trim_end()
                    .to_string(),
            )
        } else {
            CommandAnswer::NotACommand
        })
    }

    fn prepare_attachment(&self, source: &AttachmentSource) -> Result<Prepared, Refusal> {
        self.log_composer(ComposerOp::Prepare(source.clone()));
        match source {
            AttachmentSource::Path(path) => attach::prepare_path(path),
            AttachmentSource::Bytes { data, name } => attach::prepare_bytes(data.to_vec(), name),
        }
    }

    fn stop(&self, session: &str) -> Result<(), String> {
        self.log_composer(ComposerOp::Stop(session.to_string()));
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        Ok(())
    }

    fn queue_op(&self, session: &str, op: &QueueOp) -> Result<(), String> {
        self.log_composer(ComposerOp::Queue {
            session: session.to_string(),
            op: op.clone(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        self.update_controls(session, |c| match op {
            QueueOp::Remove(id) | QueueOp::Steer(id) => c.queue.retain(|q| &q.id != id),
            QueueOp::Replace { id, text, new_id } => {
                if let Some(at) = c.queue.iter().position(|q| &q.id == id) {
                    let mut item = c.queue.remove(at);
                    item.text = text.clone();
                    item.id = new_id.clone();
                    c.queue.push(item);
                }
            }
        });
        self.emit(CoreEvent::QueueChanged {
            session_id: session.to_string(),
        });
        Ok(())
    }

    fn pick(&self, session: &str, pick: &Pick) -> Result<(), String> {
        self.log_composer(ComposerOp::Pick {
            session: session.to_string(),
            pick: pick.clone(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        self.update_controls(session, |c| match pick {
            Pick::Model { provider, model } => {
                c.provider = provider.clone();
                c.model = model.clone();
            }
            Pick::Effort(effort) => c.effort = effort.clone().unwrap_or_default(),
            Pick::Mode(mode) => c.mode = *mode,
        });
        Ok(())
    }

    fn set_default_model(&self, provider: &str, model: &str) -> Result<(), String> {
        self.log_composer(ComposerOp::DefaultModel {
            provider: provider.to_string(),
            model: model.to_string(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        self.update_controls("", |c| {
            c.provider = provider.to_string();
            c.model = model.to_string();
        });
        Ok(())
    }

    fn diff(&self, request: &DiffRequest) -> Result<DiffData, String> {
        self.diffs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read(request)
    }

    fn revert_file(
        &self,
        request: &DiffRequest,
        path: &str,
        seen: Option<&str>,
    ) -> Result<(), String> {
        self.diffs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revert(request, path, seen)
    }

    fn head_commit(&self, _session: &str) -> Option<String> {
        self.diffs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .head
            .clone()
    }

    fn open_in_editor(
        &self,
        session: &str,
        path: &str,
        line: Option<u32>,
    ) -> Result<String, String> {
        let mut diffs = self.diffs.lock().unwrap_or_else(|e| e.into_inner());
        diffs.opened.push((session.to_string(), path.to_string(), line));
        Ok("the fixture editor".to_string())
    }

    fn answer(&self, prompt_id: &str, answer: &Answer) -> Result<bool, String> {
        self.log_composer(ComposerOp::Answer {
            prompt_id: prompt_id.to_string(),
            answer: answer.clone(),
        });
        if let Some(message) = self.scripted_failure() {
            return Err(message);
        }
        // A second answer, or one for a question the core settled meanwhile, finds it closed.
        if !self
            .answered_asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(prompt_id.to_string())
        {
            return Ok(false);
        }
        let open = self
            .open_asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(prompt_id);
        if let Some((session_id, kind)) = open {
            self.emit(CoreEvent::Settled {
                session_id,
                prompt_id: prompt_id.to_string(),
                kind,
            });
        }
        Ok(true)
    }
}
