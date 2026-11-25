//! The diff tab: the changed files of a scope as a list (flat or as a folder tree, status letter
//! and `+N -M` for each) over a unified diff of them all, with syntax highlighting, collapsible
//! files, a scope switch (this turn, this thread, the working tree against `HEAD`), revert of a
//! file after a confirmation, and "open in editor".
//!
//! The diff itself is GPUI Kit's `Diff` ("readonly, virtualized patch display inspired by Diffs
//! from Pierre", `gpui_kit::component::diff`): one virtual list of every file's rows, hunk
//! separators, word-level changes, a collapse control per file, its own selection and copy, and
//! horizontal scroll (or soft wrap) for long lines. This module feeds it a parsed patch
//! ([`super::data::parse_patch`]) and draws what is around it. What keeps a huge diff fast: only
//! visible rows are built (Kit's list), the patch is read and parsed on a background thread, files
//! of more than [`HUGE_FILE`] changed lines open collapsed, and so does everything after the
//! first [`OPEN_BUDGET`] changed lines ([`collapse_plan`]); only the first [`HIGHLIGHT_BUDGET`]
//! changed lines get syntax colours, the rest is plain text ([`plain_plan`]: Kit prepares every
//! file it holds, collapsed or not); the file list is a `uniform_list`.
//!
//! The panel never reads the core: it asks the [`crate::model::Backend`] (through the app model)
//! on a background task, one read at a time, and drops an answer that arrives after a newer
//! request (a scope switch, a refresh).
//!
//! t3code: `apps/web/src/components/DiffPanel.tsx` (the toolbar: counts, refresh, collapse all,
//! unified or split, wrap; the accordion of files), `diffs/DiffFileTree.tsx` +
//! `diffFileTree.logic.ts` (the list as a tree, a status for each file), `diffs/DiffFileStatus.tsx`
//! (a partial diff's mark), `diffs/StyledDiffCodeView.tsx` (the code's colours). References:
//! `sidebar-diff.png` (the toolbar and the collapsed file rows), `sidebar-diff-content.png` (an
//! opened file: line numbers, added lines in green). Gallery story: `panel-diff`.
//!
//! Not copied: t3code's review comments on lines, the pierre file-tree icons, the "hide test
//! files" switch.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Sizable as _, Size as KitSize,
        diff::{Diff, DiffMode, DiffState},
    },
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use super::data::{
    DiffData, DiffRequest, Entry, Parsed, Revert, Scope, Status, names_file, parse_patch,
};
use crate::design::{
    MiddleTruncate, Palette, ToastSpec,
    button::{Button, Size, Variant, icon_button},
    icons::icon,
    toast,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::model::AppModel;
use crate::theme::DesignTheme as _;
use crate::thread::cards::diff_colors;
use crate::thread::diff::compact_count;
use crate::thread::{ThreadModel, tools};

/// A file with more changed lines than this opens collapsed.
pub const HUGE_FILE: usize = 1_000;
/// Once this many changed lines are open, later files open collapsed.
pub const OPEN_BUDGET: usize = 5_000;
/// A row of the file list.
pub const LIST_ROW: f32 = 26.;
/// The file list shows at most this many rows before it scrolls.
pub const LIST_ROWS_MAX: usize = 7;

/// The files that open collapsed: those of more than [`HUGE_FILE`] changed lines, and every file
/// after the first [`OPEN_BUDGET`] changed lines (so the first screen of a very large diff is
/// cheap; one click opens the rest).
pub fn collapse_plan(entries: &[Entry]) -> Vec<String> {
    let mut open_lines = 0;
    let mut collapsed = Vec::new();
    for entry in entries {
        let lines = entry.added + entry.removed;
        if lines > HUGE_FILE || open_lines >= OPEN_BUDGET {
            collapsed.push(entry.path.clone());
        } else {
            open_lines += lines;
        }
    }
    collapsed
}

/// The language a file is drawn in when it is not highlighted (Kit skips syntax for it).
const PLAIN: &str = "text";

/// The scope a confirmation of `kind` was asked in.
fn confirm_scope(kind: Revert) -> Scope {
    match kind {
        Revert::ToThreadStart => Scope::Thread,
        _ => Scope::WorkingTree,
    }
}

/// At most this many changed lines get syntax colours; files past it (and every file of more than
/// [`HUGE_FILE`] changed lines) are drawn as plain text. Kit prepares syntax for every file it
/// holds, collapsed or not, one file after another on the background executor: about 1.6 ms a
/// line in a debug build, so a 6,000-line patch kept a core busy for 8 s.
pub const HIGHLIGHT_BUDGET: usize = 2_000;

/// The files drawn without syntax colours ([`HIGHLIGHT_BUDGET`]).
pub fn plain_plan(entries: &[Entry]) -> Vec<String> {
    let mut highlighted = 0;
    let mut plain = Vec::new();
    for entry in entries {
        let lines = entry.added + entry.removed;
        if lines > HUGE_FILE || highlighted + lines > HIGHLIGHT_BUDGET {
            plain.push(entry.path.clone());
        } else {
            highlighted += lines;
        }
    }
    plain
}

/// Where the read stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing asked for yet (no thread, or the tab was never shown).
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// A revert the user was asked to confirm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub path: String,
    pub kind: Revert,
    /// The file's blob id in the diff the user was shown: the backend refuses the revert when
    /// the file has changed since.
    pub seen: Option<String>,
}

/// What a read gave, as the panel shows it.
#[derive(Clone)]
struct Shown {
    scope: Scope,
    entries: Arc<Vec<Entry>>,
    unreadable: Vec<(String, String)>,
    note: Option<String>,
    skipped: usize,
    revert: Revert,
    /// Each file's blob id as the patch named it ([`Parsed::blobs`]).
    blobs: Arc<std::collections::HashMap<String, String>>,
}

/// A row of the file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListRow {
    Dir {
        name: String,
        path: String,
        depth: usize,
        open: bool,
        added: u32,
        removed: u32,
    },
    File {
        /// The index into the entries.
        entry: usize,
        depth: usize,
        /// Shown with its folder (the flat list) rather than by its name alone (the tree).
        full_path: bool,
    },
}

/// What the panel tells its owner.
#[derive(Clone, Debug)]
pub enum DiffPanelEvent {
    /// Something to say with a toast.
    Notice(ToastSpec),
    /// A read finished (the owner may now know whether a file is among the changes).
    Loaded,
    /// A revert went through (the thread's header re-reads git).
    Reverted,
}

pub struct DiffPanel {
    app: Entity<AppModel>,
    thread: Option<Entity<ThreadModel>>,
    scope: Scope,
    phase: Phase,
    shown: Option<Shown>,
    diff: Entity<DiffState>,
    generation: u64,
    split: bool,
    wrap: bool,
    list_open: bool,
    tree_view: bool,
    closed: HashSet<String>,
    rows: Arc<Vec<ListRow>>,
    list_scroll: UniformListScrollHandle,
    confirm: Option<Confirm>,
    /// A revert is under way.
    busy: bool,
    /// A file to show once the next read is in.
    pending: Option<String>,
    /// How many reads were started (tests).
    reads: usize,
    /// The panel is on screen with this tab shown: only then does it read.
    visible: bool,
    /// What was read is out of date (a turn ended while the panel was hidden).
    stale: bool,
    /// When the last read started (the window coming forward reads again after a pause).
    last: Option<std::time::Instant>,
}

impl EventEmitter<DiffPanelEvent> for DiffPanel {}

impl DiffPanel {
    pub fn new(app: Entity<AppModel>, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffState::new(Vec::new(), cx).with_mode(DiffMode::Unified));
        Self {
            app,
            thread: None,
            scope: Scope::default(),
            phase: Phase::Idle,
            shown: None,
            diff,
            generation: 0,
            split: false,
            wrap: true,
            list_open: true,
            tree_view: false,
            closed: HashSet::new(),
            rows: Arc::default(),
            list_scroll: UniformListScrollHandle::new(),
            confirm: None,
            busy: false,
            pending: None,
            reads: 0,
            visible: false,
            stale: false,
            last: None,
        }
    }

    // --- what the owner asks and learns ---------------------------------------------------------

    /// The thread whose changes are shown (`None`: no thread open). A different thread drops what
    /// was shown; the owner refreshes when the panel is visible.
    pub fn set_thread(&mut self, thread: Option<Entity<ThreadModel>>, cx: &mut Context<Self>) {
        if self.thread == thread {
            return;
        }
        self.thread = thread;
        self.generation += 1;
        self.shown = None;
        self.confirm = None;
        self.phase = Phase::Idle;
        self.rows = Arc::default();
        self.diff.update(cx, |s, cx| s.set_files(Vec::new(), cx));
        cx.notify();
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn entries(&self) -> &[Entry] {
        self.shown.as_ref().map_or(&[], |s| s.entries.as_slice())
    }

    pub fn reads(&self) -> usize {
        self.reads
    }

    pub fn confirm(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
    }

    pub fn state(&self) -> &Entity<DiffState> {
        &self.diff
    }

    pub fn rows(&self) -> &[ListRow] {
        &self.rows
    }

    pub fn is_split(&self) -> bool {
        self.split
    }

    pub fn is_wrapped(&self) -> bool {
        self.wrap
    }

    pub fn is_tree(&self) -> bool {
        self.tree_view
    }

    pub fn list_open(&self) -> bool {
        self.list_open
    }

    /// Whether `path` (as a card or a link spelled it) is one of the shown changes: `None` while
    /// no read has finished.
    pub fn has_file(&self, path: &str) -> Option<bool> {
        let shown = self.shown.as_ref()?;
        Some(shown.entries.iter().any(|e| names_file(path, &e.path)))
    }

    /// Whether `path` is changed in the working tree, as far as the working tree's changes have
    /// been read (`None`: they have not: another scope is shown, or nothing was read).
    pub fn working_tree_has(&self, path: &str) -> Option<bool> {
        let shown = self.shown.as_ref()?;
        (shown.scope == Scope::WorkingTree)
            .then(|| shown.entries.iter().any(|e| names_file(path, &e.path)))
    }

    /// Scrolls to the file `path` names, opening it; when the changes are not read yet, once they
    /// are.
    pub fn show_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let found = self
            .shown
            .as_ref()
            .and_then(|s| s.entries.iter().find(|e| names_file(path, &e.path)))
            .map(|e| e.path.clone());
        match found {
            Some(found) => {
                self.diff.update(cx, |s, cx| {
                    s.set_file_collapsed(&found, false, cx);
                    s.scroll_to_file(&found, cx);
                });
                self.pending = None;
            }
            None => self.pending = Some(path.to_string()),
        }
    }

    // --- reading ----------------------------------------------------------------------------------

    fn request(&self, cx: &App) -> Option<DiffRequest> {
        let thread = self.thread.as_ref()?.read(cx);
        Some(DiffRequest {
            session: thread.session().to_string(),
            scope: self.scope,
            turn: thread.turn_context(),
        })
    }

    /// Reads the changes of the current scope in the background; what is shown stays until the
    /// answer arrives.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.request(cx) else {
            return;
        };
        let Some(backend) = self.app.read(cx).backend() else {
            self.phase = Phase::Failed("Not connected to the core.".to_string());
            cx.notify();
            return;
        };
        self.generation += 1;
        self.reads += 1;
        self.stale = false;
        self.last = Some(std::time::Instant::now());
        let generation = self.generation;
        self.phase = Phase::Loading;
        cx.notify();
        let scope = request.scope;
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    let data = backend.diff(&request)?;
                    let mut parsed = parse_patch(&data.patch);
                    let named = request.turn.files.clone();
                    if data.narrowed {
                        parsed.retain(|path| named.iter().any(|n| names_file(n, path)));
                    } else if request.scope == Scope::Turn {
                        // Since the turn's base commit: what git says, minus the untracked files
                        // that were already there (a new file counts when a card wrote it).
                        parsed.retain_entries(|e| {
                            e.status != Status::Added
                                || named.iter().any(|n| names_file(n, &e.path))
                        });
                    }
                    Ok::<_, String>((data, parsed))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.loaded(generation, scope, read, cx));
        })
        .detach();
    }

    /// Whether the tab is on screen. It reads when it comes on screen with nothing (or something
    /// out of date) to show, and reads nothing while it is hidden.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if visible {
                self.show(cx);
            }
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Reads when the tab is on screen and has nothing for the thread and scope, or something out
    /// of date.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.thread.is_none() {
            return;
        }
        let needed = match &self.shown {
            None => !matches!(self.phase, Phase::Loading),
            Some(shown) => shown.scope != self.scope || self.stale,
        };
        if needed {
            self.refresh(cx);
        }
    }

    /// What was read may be out of date (a turn ended): read now when on screen, else when shown.
    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        if self.visible && self.thread.is_some() {
            self.refresh(cx);
        }
    }

    /// [`Self::refresh`] when on screen and the last read is over two seconds old (the window
    /// coming back to the front).
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let old = self
            .last
            .is_none_or(|last| last.elapsed() > std::time::Duration::from_secs(2));
        if self.visible && self.thread.is_some() && old && self.phase != Phase::Loading {
            self.refresh(cx);
        }
    }

    /// Chooses the scope without reading (a read follows).
    pub fn set_scope_quietly(&mut self, scope: Scope) {
        if self.scope != scope {
            self.confirm = None;
        }
        self.scope = scope;
    }

    /// Opening a file from a turn's card or footer: the turn's own changes, when the thread
    /// knows of any.
    pub fn focus_turn(&mut self, cx: &mut Context<Self>) {
        let known = self.thread.as_ref().is_some_and(|t| {
            let turn = t.read(cx).turn_context();
            turn.base.is_some() || !turn.files.is_empty()
        });
        if known && self.scope != Scope::Turn {
            self.set_scope_quietly(Scope::Turn);
            self.confirm = None;
        }
    }

    fn loaded(
        &mut self,
        generation: u64,
        scope: Scope,
        read: Result<(DiffData, Parsed), String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        match read {
            Ok((data, parsed)) => {
                let plan = collapse_plan(&parsed.entries);
                // Kit prepares syntax for every file, collapsed or not, on the background
                // executor (seconds of CPU for thousands of lines): past the budget, plain text.
                let plain_files = plain_plan(&parsed.entries);
                let plain: HashSet<&str> = plain_files.iter().map(String::as_str).collect();
                let files: Vec<_> = parsed
                    .files
                    .iter()
                    .map(|file| {
                        if plain.contains(file.path().as_ref() as &str) {
                            file.clone().with_language(PLAIN)
                        } else {
                            file.clone()
                        }
                    })
                    .collect();
                // A confirmation for a file that is no longer shown, or no longer the same,
                // is dropped: the user confirms what they see.
                if let Some(confirm) = &self.confirm {
                    let same = parsed.entries.iter().any(|e| e.path == confirm.path)
                        && parsed.blobs.get(&confirm.path) == confirm.seen.as_ref();
                    if !same || scope != confirm_scope(confirm.kind) {
                        self.confirm = None;
                    }
                }
                self.diff.update(cx, |state, cx| {
                    state.set_files(files, cx);
                    state.set_all_files_collapsed(false, cx);
                    state.set_files_collapsed(
                        plan.iter().map(|p| SharedString::from(p.clone())),
                        true,
                        cx,
                    );
                });
                self.shown = Some(Shown {
                    scope,
                    entries: Arc::new(parsed.entries),
                    unreadable: parsed.unreadable,
                    note: data.note,
                    skipped: data.skipped,
                    revert: data.revert,
                    blobs: Arc::new(parsed.blobs),
                });
                self.phase = Phase::Ready;
                self.rebuild_rows();
                if let Some(path) = self.pending.take() {
                    self.show_file(&path, cx);
                }
                cx.emit(DiffPanelEvent::Loaded);
            }
            Err(why) => {
                // A failed read of another scope must not leave the old scope's files under it.
                if self.shown.as_ref().is_some_and(|s| s.scope != scope) {
                    self.shown = None;
                    self.rows = Arc::default();
                    self.diff.update(cx, |s, cx| s.set_files(Vec::new(), cx));
                }
                self.phase = Phase::Failed(why);
            }
        }
        cx.notify();
    }

    /// Shows `data` as if it had just been read (the gallery and the tests: no background read).
    pub fn show_data(&mut self, scope: Scope, data: DiffData, cx: &mut Context<Self>) {
        self.scope = scope;
        let mut parsed = parse_patch(&data.patch);
        if data.narrowed
            && let Some(request) = self.request(cx)
        {
            let named = request.turn.files;
            parsed.retain(|path| named.iter().any(|n| names_file(n, path)));
        }
        self.generation += 1;
        let generation = self.generation;
        self.loaded(generation, scope, Ok((data, parsed)), cx);
    }

    // --- choices ----------------------------------------------------------------------------------

    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.confirm = None;
        self.refresh(cx);
    }

    pub fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        self.split = split;
        let mode = if split {
            DiffMode::Split
        } else {
            DiffMode::Unified
        };
        self.diff.update(cx, |s, cx| s.set_mode(mode, cx));
        cx.notify();
    }

    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.wrap = wrap;
        cx.notify();
    }

    pub fn set_tree(&mut self, tree: bool, cx: &mut Context<Self>) {
        self.tree_view = tree;
        self.rebuild_rows();
        cx.notify();
    }

    pub fn toggle_list(&mut self, cx: &mut Context<Self>) {
        self.list_open = !self.list_open;
        cx.notify();
    }

    pub fn toggle_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.closed.remove(path) {
            self.closed.insert(path.to_string());
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Collapses every file, or opens them all when all are collapsed.
    pub fn toggle_all_files(&mut self, cx: &mut Context<Self>) {
        let all_collapsed = self.shown.as_ref().is_some_and(|s| {
            let state = self.diff.read(cx);
            !s.entries.is_empty() && s.entries.iter().all(|e| state.is_file_collapsed(&e.path))
        });
        self.diff
            .update(cx, |s, cx| s.set_all_files_collapsed(!all_collapsed, cx));
    }

    fn rebuild_rows(&mut self) {
        let Some(shown) = &self.shown else {
            self.rows = Arc::default();
            return;
        };
        let entries = &shown.entries;
        let mut rows = Vec::new();
        if self.tree_view {
            let nodes = super::data::tree(entries);
            for row in tools::tree_rows(&nodes, &self.closed) {
                match row.node {
                    tools::Node::Dir {
                        name,
                        path,
                        additions,
                        deletions,
                        ..
                    } => rows.push(ListRow::Dir {
                        name,
                        path,
                        depth: row.depth,
                        open: row.open,
                        added: additions,
                        removed: deletions,
                    }),
                    tools::Node::File { path, .. } => {
                        if let Some(entry) = entries.iter().position(|e| e.path == path) {
                            rows.push(ListRow::File {
                                entry,
                                depth: row.depth,
                                full_path: false,
                            });
                        }
                    }
                }
            }
        } else {
            rows.extend((0..entries.len()).map(|entry| ListRow::File {
                entry,
                depth: 0,
                full_path: true,
            }));
        }
        self.rows = Arc::new(rows);
    }

    // --- one file ---------------------------------------------------------------------------------

    /// Clicking a row of the list: the diff scrolls to that file, opened.
    pub fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        self.diff.update(cx, |s, cx| {
            s.set_file_collapsed(path, false, cx);
            s.scroll_to_file(path, cx);
        });
    }

    /// Whether the shown scope lets `entry` be reverted, and if not, why.
    pub fn revert_state(&self, entry: &Entry) -> Result<Revert, &'static str> {
        let Some(shown) = &self.shown else {
            return Err("Nothing is shown");
        };
        match shown.revert {
            Revert::None => Err("Revert works in the working tree and this thread's changes"),
            kind => {
                if kind == Revert::ToHead
                    && matches!(entry.status, Status::Renamed | Status::Copied)
                {
                    Err("A renamed file cannot be reverted from here")
                } else if kind == Revert::ToHead && entry.status == Status::Conflicted {
                    Err("Resolve the conflict first")
                } else {
                    Ok(kind)
                }
            }
        }
    }

    /// Asks the user to confirm putting `path` back.
    pub fn ask_revert(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(entry) = self.entries().iter().find(|e| e.path == path).cloned() else {
            return;
        };
        if let Ok(kind) = self.revert_state(&entry) {
            let seen = self
                .shown
                .as_ref()
                .and_then(|s| s.blobs.get(path).cloned());
            self.confirm = Some(Confirm {
                path: path.to_string(),
                kind,
                seen,
            });
            cx.notify();
        }
    }

    pub fn cancel_revert(&mut self, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            cx.notify();
        }
    }

    /// Does the revert the user confirmed, in the background, then reads the changes again.
    pub fn confirm_revert(&mut self, cx: &mut Context<Self>) {
        let (Some(confirm), Some(request)) = (self.confirm.take(), self.request(cx)) else {
            return;
        };
        // The scope the confirmation was asked in (a scope switch clears it; this is a guard),
        // and one revert at a time.
        if self.busy || request.scope != confirm_scope(confirm.kind) {
            cx.notify();
            return;
        }
        let Some(backend) = self.app.read(cx).backend() else {
            return;
        };
        self.busy = true;
        cx.notify();
        let (path, seen) = (confirm.path, confirm.seen);
        cx.spawn(async move |this, cx| {
            let done = {
                let path = path.clone();
                cx.background_spawn(async move {
                    backend.revert_file(&request, &path, seen.as_deref())
                })
                .await
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let notice = match &done {
                    Ok(()) => {
                        cx.emit(DiffPanelEvent::Reverted);
                        ToastSpec::new(toast::Kind::Success, "File reverted").description(path)
                    }
                    Err(why) => ToastSpec::new(toast::Kind::Error, "Could not revert")
                        .description(why.clone()),
                };
                cx.emit(DiffPanelEvent::Notice(notice));
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Opens `path` in the editor (Zed, VS Code, or the default app: see the backend).
    pub fn open_in_editor(&mut self, path: &str, cx: &mut Context<Self>) {
        let (Some(thread), Some(backend)) = (self.thread.as_ref(), self.app.read(cx).backend())
        else {
            return;
        };
        let session = thread.read(cx).session().to_string();
        let path = path.to_string();
        cx.spawn(async move |this, cx| {
            let opened = {
                let path = path.clone();
                cx.background_spawn(async move { backend.open_in_editor(&session, &path, None) })
                    .await
            };
            let _ = this.update(cx, |_, cx| {
                let notice = match opened {
                    Ok(name) => ToastSpec::new(toast::Kind::Info, format!("Opened in {name}"))
                        .description(path),
                    Err(why) => ToastSpec::new(toast::Kind::Error, "Could not open the file")
                        .description(why),
                };
                cx.emit(DiffPanelEvent::Notice(notice));
            });
        })
        .detach();
    }
}

// --- drawing --------------------------------------------------------------------------------------

fn status_color(status: Status, palette: &Palette) -> gpui_kit::Hsla {
    match status {
        Status::Added => palette.success_foreground.hsla(),
        Status::Deleted => palette.destructive_foreground.hsla(),
        Status::Renamed | Status::Copied => palette.info_foreground.hsla(),
        Status::Modified | Status::Conflicted => palette.warning_foreground.hsla(),
    }
}

fn sized(name: IconName, size: f32) -> gpui_kit::component::Icon {
    icon(name).with_size(KitSize::Size(px(size)))
}

fn id2(name: &'static str, n: usize) -> ElementId {
    ElementId::NamedInteger(name.into(), n as u64)
}

/// `+N -M`: two right-aligned columns when `aligned`, so the counts of a list line up; else
/// side by side as t3code's inline label.
fn counts(
    id: ElementId,
    aligned: bool,
    added: usize,
    removed: usize,
    palette: &Palette,
    mono: &SharedString,
) -> AnyElement {
    let (add, del) = diff_colors(palette);
    let column = |text: String, color: gpui_kit::Hsla| {
        div()
            .when(aligned, |d| d.min_w(px(34.)))
            .flex()
            .justify_end()
            .text_color(color)
            .child(text)
    };
    div()
        .id(id)
        .flex()
        .gap(px(if aligned { 6. } else { 4. }))
        .flex_shrink_0()
        .font_family(mono.clone())
        .text_size(if aligned {
            text::XXS2.size
        } else {
            text::XS.size
        })
        .child(column(
            format!("+{}", compact_count(added.min(u32::MAX as usize) as u32)),
            add,
        ))
        .child(column(
            format!("-{}", compact_count(removed.min(u32::MAX as usize) as u32)),
            del,
        ))
        .probe()
        .into_any_element()
}

/// The status letter in a small tinted box.
fn letter_box(id: ElementId, status: Status, palette: &Palette, mono: &SharedString) -> AnyElement {
    let color = status_color(status, palette);
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(16.))
        .rounded(radius::SM)
        .border_1()
        .border_color(color.opacity(0.55))
        .bg(color.opacity(0.12))
        .text_color(color)
        .font_family(mono.clone())
        .text_size(text::XXS3.size)
        .font_weight(FontWeight::SEMIBOLD)
        .child(status.letter().to_string())
        .probe()
        .into_any_element()
}

impl DiffPanel {
    fn segmented(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        div()
            .id("diff-scopes")
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(radius::MD)
            .bg(palette.muted.hsla())
            .children(Scope::ALL.iter().map(|&scope| {
                Button::new(format!("diff-scope-{}", scope.id()), scope.label())
                    .variant(if scope == self.scope {
                        Variant::Outline
                    } else {
                        Variant::Ghost
                    })
                    .size(Size::Xs)
                    .tooltip(scope.hint())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_scope(scope, cx)))
            }))
            .probe()
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let mono = cx.theme().mono_font_family.clone();
        let (files, added, removed) = super::data::totals(self.entries());
        let refreshing = self.phase == Phase::Loading;
        let summary = div()
            .id("diff-summary")
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .px(px(4.))
            .rounded(radius::MD)
            .cursor_pointer()
            .hover(move |s| s.bg(palette.accent.hsla().opacity(0.6)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_list(cx)))
            .child(
                div()
                    .text_color(palette.muted_foreground.hsla())
                    .child(sized(
                        if self.list_open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        14.,
                    )),
            )
            .child(
                div()
                    .text_size(text::XS.size)
                    .text_color(palette.foreground.hsla())
                    .child(match files {
                        1 => "1 file".to_string(),
                        n => format!("{n} files"),
                    }),
            )
            .child(counts(
                "diff-total".into(),
                false,
                added,
                removed,
                &palette,
                &mono,
            ))
            .probe();
        let tools = div()
            .id("diff-tools")
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                icon_button("diff-refresh", IconName::RotateCcw, "Refresh")
                    .variant(Variant::GhostMuted)
                    .size(Size::Xs)
                    .disabled(refreshing)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                icon_button(
                    "diff-collapse",
                    IconName::ChevronsDownUp,
                    "Collapse or open all files",
                )
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_all_files(cx))),
            )
            .child(
                icon_button(
                    "diff-split",
                    if self.split {
                        IconName::Columns2
                    } else {
                        IconName::Rows3
                    },
                    if self.split {
                        "Unified view"
                    } else {
                        "Side by side"
                    },
                )
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .selected(self.split)
                .on_click(cx.listener(|this, _, _, cx| this.set_split(!this.split, cx))),
            )
            .child(
                icon_button(
                    "diff-wrap",
                    IconName::TextWrap,
                    if self.wrap {
                        "Scroll long lines"
                    } else {
                        "Wrap long lines"
                    },
                )
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .selected(self.wrap)
                .on_click(cx.listener(|this, _, _, cx| this.set_wrap(!this.wrap, cx))),
            )
            .child(
                icon_button(
                    "diff-tree",
                    IconName::ListTree,
                    if self.tree_view {
                        "Flat list"
                    } else {
                        "Folder tree"
                    },
                )
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .selected(self.tree_view)
                .on_click(cx.listener(|this, _, _, cx| this.set_tree(!this.tree_view, cx))),
            )
            .probe();
        div()
            .id("diff-toolbar")
            .flex()
            .flex_col()
            .gap(px(6.))
            .flex_shrink_0()
            .px(px(10.))
            .py(px(8.))
            .border_b_1()
            .border_color(palette.border.hsla())
            .child(self.segmented(cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(4.))
                    .child(summary)
                    .child(tools),
            )
            .probe()
            .into_any_element()
    }

    fn list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.shown.as_ref()?;
        if !self.list_open || shown.entries.is_empty() {
            return None;
        }
        let palette = cx.design_theme().palette;
        let mono = cx.theme().mono_font_family.clone();
        let rows = self.rows.clone();
        let entries = shown.entries.clone();
        let panel = cx.entity();
        let height = rows.len().min(LIST_ROWS_MAX) as f32 * LIST_ROW;
        Some(
            div()
                .id("diff-list-box")
                .flex_shrink_0()
                .border_b_1()
                .border_color(palette.border.hsla())
                .child(
                    uniform_list("diff-list", rows.len(), move |range: Range<usize>, _, _| {
                        range
                            .map(|ix| list_row(&panel, &rows[ix], ix, &entries, &palette, &mono))
                            .collect::<Vec<_>>()
                    })
                    .track_scroll(&self.list_scroll)
                    .h(px(height))
                    .probe(),
                )
                .into_any_element(),
        )
    }

    fn notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = cx.design_theme().palette;
        let shown = self.shown.as_ref()?;
        let mut lines: Vec<(String, bool)> = Vec::new();
        // A refresh that failed leaves the list as it was, and says so.
        if let Phase::Failed(why) = &self.phase {
            lines.push((format!("Could not refresh the changes: {why}"), true));
        }
        if let Some(note) = &shown.note
            && !(shown.entries.is_empty() && shown.unreadable.is_empty())
        {
            lines.push((note.clone(), false));
        }
        if shown.skipped > 0 {
            lines.push((
                format!(
                    "{} new file{} too large or too many to show",
                    shown.skipped,
                    if shown.skipped == 1 { " is" } else { "s are" }
                ),
                false,
            ));
        }
        for (path, why) in &shown.unreadable {
            lines.push((format!("{path} could not be read: {why}"), true));
        }
        for entry in shown.entries.iter().filter(|e| e.truncated) {
            lines.push((
                format!(
                    "{} is too large to show in full; its counts include every change",
                    entry.path
                ),
                false,
            ));
        }
        (!lines.is_empty()).then(|| {
            div()
                .id("diff-notes")
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_shrink_0()
                .px(px(12.))
                .py(px(6.))
                .text_size(text::XXS2.size)
                .children(lines.into_iter().map(|(line, bad)| {
                    div()
                        .text_color(if bad {
                            palette.destructive_foreground.hsla()
                        } else {
                            palette.muted_foreground.hsla()
                        })
                        .child(line)
                }))
                .probe()
                .into_any_element()
        })
    }

    fn confirm_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let palette = cx.design_theme().palette;
        let (question, detail, label) = match confirm.kind {
            Revert::ToHead => (
                format!("Discard all changes to {}?", confirm.path),
                "It goes back to HEAD, staged changes too (a new file is deleted). This cannot be undone.",
                "Discard changes",
            ),
            _ => (
                format!(
                    "Put {} back as it was before this thread's first edit?",
                    confirm.path
                ),
                "A checkpoint is taken first, so /rollback can undo it.",
                "Revert file",
            ),
        };
        Some(
            div()
                .id("diff-confirm")
                .flex()
                .flex_col()
                .gap(px(6.))
                .flex_shrink_0()
                .p(px(10.))
                .border_t_1()
                .border_color(palette.border.hsla())
                .bg(palette.warning_surface.hsla())
                .text_size(text::XS.size)
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.foreground.hsla())
                        .child(question),
                )
                .child(
                    div()
                        .text_color(palette.muted_foreground.hsla())
                        .child(detail),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(
                            Button::new("diff-confirm-cancel", "Cancel")
                                .variant(Variant::Outline)
                                .size(Size::Xs)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_revert(cx))),
                        )
                        .child(
                            Button::new("diff-confirm-ok", label)
                                .variant(Variant::Destructive)
                                .size(Size::Xs)
                                .on_click(cx.listener(|this, _, _, cx| this.confirm_revert(cx))),
                        ),
                )
                .probe()
                .into_any_element(),
        )
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let message = |text: String, id: &'static str| {
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .text_size(text::XS.size)
                .text_center()
                .text_color(palette.muted_foreground.hsla())
                .child(text)
                .probe()
                .into_any_element()
        };
        if self.thread.is_none() {
            return message(
                "Open a thread to see its changes.".to_string(),
                "diff-empty",
            );
        }
        match (&self.phase, &self.shown) {
            (Phase::Failed(why), None) => {
                return div()
                    .id("diff-failed")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .p(px(24.))
                    .text_size(text::XS.size)
                    .text_center()
                    .text_color(palette.destructive_foreground.hsla())
                    .child(why.clone())
                    .child(
                        Button::new("diff-retry", "Try again")
                            .variant(Variant::Outline)
                            .size(Size::Xs)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .probe()
                    .into_any_element();
            }
            (Phase::Idle | Phase::Loading, None) => {
                return message("Reading the changes...".to_string(), "diff-loading");
            }
            _ => {}
        }
        let shown = self.shown.as_ref().expect("a read finished");
        if shown.entries.is_empty() && shown.unreadable.is_empty() {
            let text = match &shown.note {
                Some(note) => note.clone(),
                None => match shown.scope {
                    Scope::Turn => "The latest turn changed nothing.",
                    Scope::Thread => "This thread has not changed any file.",
                    Scope::WorkingTree => "No changes against HEAD.",
                }
                .to_string(),
            };
            return message(text, "diff-empty");
        }
        let panel = cx.entity();
        let revert_kind = shown.revert;
        let busy = self.busy;
        let palette_c = palette;
        let diff = Diff::new(&self.diff)
            .soft_wrap(self.wrap)
            .render_header_suffix(move |file, _, cx| {
                let path = file.path().to_string();
                let deleted = file.status() == gpui_kit::component::diff::DiffFileStatus::Deleted;
                let entry = panel
                    .read(cx)
                    .entries()
                    .iter()
                    .find(|e| e.path == path)
                    .cloned();
                let revert = entry.as_ref().map(|e| panel.read(cx).revert_state(e));
                let open_panel = panel.clone();
                let revert_panel = panel.clone();
                let (open_path, revert_path) = (path.clone(), path.clone());
                let _ = palette_c;
                div()
                    .flex()
                    .items_center()
                    .gap(px(1.))
                    .flex_shrink_0()
                    .child(
                        icon_button(
                            ElementId::Name(format!("diff-open:{path}").into()),
                            IconName::ExternalLink,
                            if deleted {
                                "A deleted file cannot be opened".to_string()
                            } else {
                                "Open in editor".to_string()
                            },
                        )
                        .variant(Variant::GhostMuted)
                        .size(Size::Micro)
                        .disabled(deleted)
                        .on_click(move |_, _, cx| {
                            open_panel.update(cx, |p, cx| p.open_in_editor(&open_path, cx))
                        }),
                    )
                    .when(revert_kind != Revert::None, |row| {
                        let (enabled, why) = match revert {
                            Some(Ok(_)) => (!busy, "Revert file".to_string()),
                            Some(Err(why)) => (false, why.to_string()),
                            None => (false, String::new()),
                        };
                        row.child(
                            icon_button(
                                ElementId::Name(format!("diff-revert:{path}").into()),
                                IconName::Undo2,
                                if revert_kind == Revert::ToHead && enabled {
                                    "Discard changes (cannot be undone)".to_string()
                                } else {
                                    why
                                },
                            )
                            .variant(Variant::GhostMuted)
                            .size(Size::Micro)
                            .disabled(!enabled)
                            .on_click(move |_, _, cx| {
                                revert_panel.update(cx, |p, cx| p.ask_revert(&revert_path, cx))
                            }),
                        )
                    })
            });
        div()
            .id("diff-view")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("diff-code")
                    .flex_1()
                    .min_h_0()
                    .child(diff.size_full())
                    .probe(),
            )
            .probe()
            .into_any_element()
    }
}

/// One row of the file list.
fn list_row(
    panel: &Entity<DiffPanel>,
    row: &ListRow,
    ix: usize,
    entries: &[Entry],
    palette: &Palette,
    mono: &SharedString,
) -> AnyElement {
    let muted = palette.muted_foreground.hsla();
    let hover = palette.accent.hsla().opacity(0.6);
    let base = div()
        .id(id2("diff-row", ix))
        .flex()
        .w_full()
        .overflow_hidden()
        .items_center()
        .gap(px(8.))
        .h(px(LIST_ROW))
        .px(px(10.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover));
    match row {
        ListRow::Dir {
            name,
            path,
            depth,
            open,
            added,
            removed,
        } => {
            let (panel, path) = (panel.clone(), path.clone());
            base.pl(px(10. + *depth as f32 * 14.))
                .on_click(move |_, _, cx| panel.update(cx, |p, cx| p.toggle_folder(&path, cx)))
                .child(div().text_color(muted).child(sized(
                    if *open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    14.,
                )))
                .child(div().text_color(muted).child(sized(
                    if *open {
                        IconName::FolderOpen
                    } else {
                        IconName::FolderClosed
                    },
                    14.,
                )))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(text::XS.size)
                        .text_color(muted)
                        .child(name.clone()),
                )
                .child(counts(
                    id2("diff-stat", ix),
                    true,
                    *added as usize,
                    *removed as usize,
                    palette,
                    mono,
                ))
                .probe()
                .into_any_element()
        }
        ListRow::File {
            entry,
            depth,
            full_path,
        } => {
            let Some(e) = entries.get(*entry) else {
                return base.into_any_element();
            };
            let (panel, path) = (panel.clone(), e.path.clone());
            let name = e.path.rsplit('/').next().unwrap_or(&e.path).to_string();
            base.pl(px(10. + *depth as f32 * 14.))
                .on_click(move |_, _, cx| panel.update(cx, |p, cx| p.reveal(&path, cx)))
                .child(letter_box(id2("diff-letter", ix), e.status, palette, mono))
                .child(if *full_path {
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_size(text::XS.size)
                        .text_color(palette.foreground.hsla())
                        .child(MiddleTruncate::new(id2("diff-path", ix), e.path.clone()).tail(18))
                        .into_any_element()
                } else {
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(text::XS.size)
                        .text_color(palette.foreground.hsla())
                        .child(name)
                        .into_any_element()
                })
                .when(e.binary, |d| {
                    d.child(
                        div()
                            .text_size(text::XXS3.size)
                            .text_color(muted)
                            .child("binary"),
                    )
                })
                .child(counts(
                    id2("diff-stat", ix),
                    true,
                    e.added,
                    e.removed,
                    palette,
                    mono,
                ))
                .probe()
                .into_any_element()
        }
    }
}

impl Render for DiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let open = self.thread.is_some();
        div()
            .id("diff-panel")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .text_color(palette.foreground.hsla())
            // No thread, nothing to scope or count: the message alone.
            .when(open, |d| d.child(self.toolbar(cx)))
            .when(open, |d| d.children(self.list(cx)))
            .when(open, |d| d.children(self.notes(cx)))
            .child(self.body(cx))
            .when(open, |d| d.children(self.confirm_bar(cx)))
            .probe()
    }
}
