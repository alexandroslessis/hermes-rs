//! The sidebar view's state and what it does (the drawing is in `render.rs`): the keyboard cursor,
//! the folders the user closed, the filter box, the inline rename, the "are you sure" question,
//! and every action a row, a header, a menu or a key can start. Each action that changes data
//! goes through the [`AppModel`] (`apply(Op)`): nothing here calls the core, and nothing blocks.
//!
//! t3code: `apps/web/src/components/Sidebar.tsx` (the folder, thread and filter handlers, the
//! context menus' items), `sidebar/SidebarThreadRow.tsx` (`ThreadRenameInput`: Enter commits,
//! Escape cancels, blur commits; double click and F2 start it).

use std::collections::HashSet;
use std::path::PathBuf;

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    PathPromptOptions, ScrollHandle, Subscription, Task, Window, component::input::InputEvent,
    component::input::InputState,
};

use super::data::{NO_PROJECT, Op, Options, Prefs, ProjectKind, RowKey, Tree, layout, step};
use crate::model::AppModel;

/// What the sidebar asks of the frame around it.
#[derive(Clone, Debug, PartialEq)]
pub enum SidebarEvent {
    /// The collapse button.
    ToggleSidebar,
    /// The search row (Ctrl+K).
    OpenPalette,
    OpenSettings,
    OpenUsage,
    /// The footer's theme button.
    ToggleTheme,
    /// The remembered part changed (a folder closed, a colour picked): write it down.
    PrefsChanged(Prefs),
}

/// What is being renamed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTarget {
    Thread(String),
    Project(String),
}

pub(super) struct Rename {
    pub target: RenameTarget,
    pub input: Entity<InputState>,
    pub _subscription: Subscription,
}

/// What a confirmation is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmKind {
    DeleteThread(String),
    RemoveProject(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub kind: ConfirmKind,
    pub title: String,
    pub detail: String,
    /// The accept button's label.
    pub accept: &'static str,
}

pub(super) struct Filter {
    pub input: Entity<InputState>,
    pub _subscription: Subscription,
}

/// The sidebar.
pub struct Sidebar {
    pub(super) model: Entity<AppModel>,
    pub(super) focus: FocusHandle,
    pub(super) scroll: ScrollHandle,
    pub(super) prefs: Prefs,
    /// Folders the user asked "Show more" for.
    pub(super) expanded: HashSet<String>,
    /// The row the keyboard is on.
    pub(super) cursor: Option<RowKey>,
    /// Where each row is among the list's children (set by `render`), for scrolling to it.
    pub(super) row_ix: std::collections::HashMap<RowKey, usize>,
    pub(super) filter: Option<Filter>,
    pub(super) rename: Option<Rename>,
    pub(super) confirm: Option<Confirm>,
    _model: Subscription,
    _clock: Task<()>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Sidebar {
    /// The sidebar over `model`, with the remembered folders and colours.
    pub fn new(model: Entity<AppModel>, prefs: Prefs, cx: &mut Context<Self>) -> Self {
        let observed = cx.observe(&model, |_, _, cx| cx.notify());
        // Relative times move on: a redraw every half minute (it costs one frame).
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            model,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            prefs,
            expanded: HashSet::new(),
            cursor: None,
            row_ix: Default::default(),
            filter: None,
            rename: None,
            confirm: None,
            _model: observed,
            _clock: clock,
        }
    }

    // --- reading ------------------------------------------------------------------------------

    /// What the filter box says (empty when it is closed).
    pub fn filter_text(&self, cx: &App) -> String {
        self.filter
            .as_ref()
            .map(|f| f.input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// The list for the model's data, the filter and the closed folders.
    pub fn tree(&self, cx: &App) -> Tree {
        let model = self.model.read(cx);
        let filter = self.filter_text(cx);
        layout(
            model.snapshot(),
            &Options {
                filter: &filter,
                collapsed: Some(&self.prefs.collapsed),
                expanded: Some(&self.expanded),
                marks: model.marks(),
            },
        )
    }

    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }

    pub fn cursor(&self) -> Option<&RowKey> {
        self.cursor.as_ref()
    }

    pub fn filter_open(&self) -> bool {
        self.filter.is_some()
    }

    pub fn confirming(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
    }

    pub fn renaming(&self) -> Option<&RenameTarget> {
        self.rename.as_ref().map(|r| &r.target)
    }

    /// Whether a text field of the sidebar has the keyboard.
    fn typing(&self, window: &Window, cx: &App) -> bool {
        let focused =
            |input: &Entity<InputState>| input.read(cx).focus_handle(cx).is_focused(window);
        self.rename.as_ref().is_some_and(|r| focused(&r.input))
            || self.filter.as_ref().is_some_and(|f| focused(&f.input))
    }

    /// The model, for the frame.
    pub fn model(&self) -> &Entity<AppModel> {
        &self.model
    }

    // --- focus and the keyboard cursor --------------------------------------------------------

    /// Puts the keyboard in the sidebar.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub(super) fn set_cursor(&mut self, key: Option<RowKey>, cx: &mut Context<Self>) {
        if self.cursor != key {
            self.cursor = key;
            cx.notify();
        }
    }

    /// Moves the cursor to the next or the previous row (the first row when there is none).
    pub fn move_cursor(&mut self, down: bool, cx: &mut Context<Self>) {
        let tree = self.tree(cx);
        let keys = tree.keys();
        let next = step(&keys, self.cursor.as_ref(), down);
        self.reveal(next.as_ref());
        self.set_cursor(next, cx);
    }

    /// The cursor goes to the first (`false`) or the last (`true`) row.
    pub fn jump_cursor(&mut self, last: bool, cx: &mut Context<Self>) {
        let keys = self.tree(cx).keys();
        let to = if last { keys.last() } else { keys.first() }.cloned();
        self.reveal(to.as_ref());
        self.set_cursor(to, cx);
    }

    /// Scrolls the list so the row shows (`render.rs` records each row's place in the list).
    fn reveal(&self, key: Option<&RowKey>) {
        if let Some(ix) = key.and_then(|k| self.row_ix.get(k)) {
            self.scroll.scroll_to_item(*ix);
        }
    }

    /// Enter on the row under the cursor: opens the thread, opens or closes the folder, shows
    /// more.
    pub fn activate_cursor(&mut self, cx: &mut Context<Self>) {
        match self.cursor.clone() {
            Some(RowKey::Thread(id)) => self.open_thread(&id, cx),
            Some(RowKey::Folder(project)) => self.toggle_folder(&project, cx),
            Some(RowKey::More(project)) => self.show_more(&project, cx),
            None => {}
        }
    }

    // --- opening -----------------------------------------------------------------------------

    pub fn open_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.open(id, cx));
        self.set_cursor(Some(RowKey::Thread(id.to_string())), cx);
    }

    /// A new thread in `project` (the main area shows it; nothing is stored until it is sent).
    pub fn new_thread(&mut self, project: &str, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.new_thread(project, cx));
    }

    /// A new thread where the user is: the open thread's project, else the newest project's.
    pub fn new_thread_here(&mut self, cx: &mut Context<Self>) {
        let project = {
            let model = self.model.read(cx);
            model.current_project().map(str::to_string).or_else(|| {
                model
                    .snapshot()
                    .latest_thread()
                    .map(|t| t.project.clone())
                    .or_else(|| model.snapshot().projects.first().map(|p| p.id.clone()))
            })
        };
        self.new_thread(project.as_deref().unwrap_or(NO_PROJECT), cx);
    }

    // --- folders -----------------------------------------------------------------------------

    fn prefs_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(SidebarEvent::PrefsChanged(self.prefs.clone()));
        cx.notify();
    }

    pub fn toggle_folder(&mut self, project: &str, cx: &mut Context<Self>) {
        let closed = self.prefs.collapsed.contains(project);
        self.set_folder_closed(project, !closed, cx);
    }

    pub fn set_folder_closed(&mut self, project: &str, closed: bool, cx: &mut Context<Self>) {
        let changed = if closed {
            self.prefs.collapsed.insert(project.to_string())
        } else {
            self.prefs.collapsed.remove(project)
        };
        if changed {
            self.prefs_changed(cx);
        }
    }

    pub fn show_more(&mut self, project: &str, cx: &mut Context<Self>) {
        self.expanded.insert(project.to_string());
        cx.notify();
    }

    /// The colour of a project's tile: the one the user picked, else one from its name.
    pub fn set_project_color(
        &mut self,
        project: &str,
        color: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match color {
            Some(name) => self
                .prefs
                .colors
                .insert(project.to_string(), name.to_string()),
            None => self.prefs.colors.remove(project),
        };
        self.prefs_changed(cx);
    }

    // --- the filter --------------------------------------------------------------------------

    /// Opens the filter box and puts the keyboard in it (Ctrl+F, the filter button).
    pub fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.is_none() {
            self.filter = Some(Self::make_filter(window, cx));
        }
        if let Some(filter) = &self.filter {
            filter.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn make_filter(window: &mut Window, cx: &mut Context<Self>) -> Filter {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter threads"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    // The rows change under the cursor.
                    this.cursor = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.activate_first_match(window, cx),
                _ => {}
            },
        );
        Filter {
            input,
            _subscription: subscription,
        }
    }

    /// A sidebar that starts with the filter box open on `text` (the gallery's stories build
    /// their state this way: a story is rebuilt on every frame, so it must notify no one).
    pub fn with_filter(mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = Self::make_filter(window, cx);
        filter.input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        self.filter = Some(filter);
        self
    }

    /// A sidebar that starts with a thread's title being edited.
    pub fn with_rename_of(mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = self
            .model
            .read(cx)
            .snapshot()
            .thread(id)
            .map(|t| t.title.clone());
        if let Some(current) = current {
            self.rename = Some(Self::make_rename(
                RenameTarget::Thread(id.to_string()),
                current,
                window,
                cx,
            ));
        }
        self
    }

    /// A sidebar that starts with the "delete this thread?" question open.
    pub fn with_delete_question(mut self, id: &str, cx: &mut Context<Self>) -> Self {
        let title = self
            .model
            .read(cx)
            .snapshot()
            .thread(id)
            .map(|t| t.display_title().to_string());
        if let Some(title) = title {
            self.confirm = Some(Self::delete_question(id, &title));
        }
        self
    }

    /// Enter in the filter box opens the first thread that matches.
    fn activate_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.tree(cx).keys().into_iter().find_map(|k| match k {
            RowKey::Thread(id) => Some(id),
            _ => None,
        });
        if let Some(id) = first {
            self.open_thread(&id, cx);
            self.focus(window, cx);
        }
    }

    /// Closes the filter box and everything shows again.
    pub fn close_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.take().is_some() {
            self.cursor = None;
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// The filter button: opens the box, or closes it.
    pub fn toggle_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.is_some() {
            self.close_filter(window, cx);
        } else {
            self.open_filter(window, cx);
        }
    }

    /// Sets the filter's text (tests and "search in project").
    pub fn set_filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_filter(window, cx);
        if let Some(filter) = &self.filter {
            filter
                .input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
        cx.notify();
    }

    // --- renaming ----------------------------------------------------------------------------

    fn start_rename(
        &mut self,
        target: RenameTarget,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rename = Self::make_rename(target, current, window, cx);
        rename.input.update(cx, |input, cx| input.focus(window, cx));
        self.rename = Some(rename);
        cx.notify();
    }

    fn make_rename(
        target: RenameTarget,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Rename {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value(current, window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
                _ => {}
            },
        );
        Rename {
            target,
            input,
            _subscription: subscription,
        }
    }

    /// F2, the double click, the menu: rename a thread in place.
    pub fn start_rename_thread(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .model
            .read(cx)
            .snapshot()
            .thread(id)
            .map(|t| t.title.clone());
        if let Some(current) = current {
            self.start_rename(RenameTarget::Thread(id.to_string()), current, window, cx);
        }
    }

    /// Renames a project (only the user's own).
    pub fn start_rename_project(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .model
            .read(cx)
            .snapshot()
            .project(id)
            .filter(|p| p.kind == ProjectKind::User)
            .map(|p| p.name.clone());
        if let Some(current) = current {
            self.start_rename(RenameTarget::Project(id.to_string()), current, window, cx);
        }
    }

    /// Enter or leaving the field: keeps the new name when it is not empty and not the old one.
    pub fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let text = rename.input.read(cx).value().trim().to_string();
        match &rename.target {
            RenameTarget::Thread(id) => {
                let old = self
                    .model
                    .read(cx)
                    .snapshot()
                    .thread(id)
                    .map(|t| t.title.clone());
                if !text.is_empty() && old.as_deref() != Some(text.as_str()) {
                    self.apply(
                        Op::Rename {
                            id: id.clone(),
                            title: text,
                        },
                        cx,
                    );
                }
            }
            RenameTarget::Project(id) => {
                let old = self
                    .model
                    .read(cx)
                    .snapshot()
                    .project(id)
                    .map(|p| p.name.clone());
                if !text.is_empty() && old.as_deref() != Some(text.as_str()) {
                    self.apply(
                        Op::RenameProject {
                            id: id.clone(),
                            name: text,
                        },
                        cx,
                    );
                }
            }
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// Escape in the rename field.
    pub fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    // --- editing -----------------------------------------------------------------------------

    fn apply(&mut self, op: Op, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.apply(op, cx));
    }

    pub fn toggle_pin(&mut self, id: &str, cx: &mut Context<Self>) {
        let pinned = self
            .model
            .read(cx)
            .snapshot()
            .thread(id)
            .is_some_and(|t| t.pinned);
        self.apply(
            Op::Pin {
                id: id.to_string(),
                pinned: !pinned,
            },
            cx,
        );
    }

    pub fn archive(&mut self, id: &str, cx: &mut Context<Self>) {
        self.leave_row(id, cx);
        self.apply(Op::Archive { id: id.to_string() }, cx);
    }

    /// A thread is about to leave the list (archive, delete): the keyboard cursor on it moves to
    /// the next row (or the previous one at the end) so the keyboard is not lost.
    fn leave_row(&mut self, id: &str, cx: &mut Context<Self>) {
        let gone = RowKey::Thread(id.to_string());
        if self.cursor.as_ref() != Some(&gone) {
            return;
        }
        let keys = self.tree(cx).keys();
        self.cursor = step(&keys, Some(&gone), true)
            .filter(|k| *k != gone)
            .or_else(|| step(&keys, Some(&gone), false))
            .filter(|k| *k != gone);
    }

    /// Moves a thread to a project (a drop on its header, or the menu).
    pub fn move_to_project(&mut self, id: &str, project: &str, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let already = model
            .snapshot()
            .thread(id)
            .is_some_and(|t| t.project == project);
        let has_folder = model
            .snapshot()
            .project(project)
            .is_some_and(|p| p.path.is_some());
        if already || !has_folder {
            return;
        }
        self.apply(
            Op::Move {
                id: id.to_string(),
                project: project.to_string(),
            },
            cx,
        );
    }

    /// Asks before deleting a thread: it clears its whole conversation.
    pub fn request_delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let title = self
            .model
            .read(cx)
            .snapshot()
            .thread(id)
            .map(|t| t.display_title().to_string());
        if let Some(title) = title {
            self.confirm = Some(Self::delete_question(id, &title));
            cx.notify();
        }
    }

    fn delete_question(id: &str, title: &str) -> Confirm {
        Confirm {
            kind: ConfirmKind::DeleteThread(id.to_string()),
            title: format!("Delete “{title}”?"),
            detail: "This permanently clears the conversation. It cannot be undone.".into(),
            accept: "Delete",
        }
    }

    /// Asks before removing a project from the list (its threads and folder stay).
    pub fn request_remove_project(&mut self, id: &str, cx: &mut Context<Self>) {
        let name = self
            .model
            .read(cx)
            .snapshot()
            .project(id)
            .filter(|p| p.kind == ProjectKind::User)
            .map(|p| p.name.clone());
        if let Some(name) = name {
            self.confirm = Some(Confirm {
                kind: ConfirmKind::RemoveProject(id.to_string()),
                title: format!("Remove “{name}”?"),
                detail: "The folder and its threads are kept; its threads move to the project \
                         their folder belongs to, or to No project."
                    .into(),
                accept: "Remove",
            });
            cx.notify();
        }
    }

    /// The accept button (or Enter) of the question.
    pub fn accept_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        match confirm.kind {
            ConfirmKind::DeleteThread(id) => {
                self.leave_row(&id, cx);
                self.apply(Op::Delete { id }, cx);
            }
            ConfirmKind::RemoveProject(id) => {
                if self.cursor == Some(RowKey::Folder(id.clone())) {
                    self.cursor = None;
                }
                // Its remembered colour and closed state go with it (native-ui.json does not
                // keep entries for projects that no longer exist).
                let had =
                    self.prefs.colors.remove(&id).is_some() | self.prefs.collapsed.remove(&id);
                if had {
                    self.prefs_changed(cx);
                }
                self.apply(Op::RemoveProject { id }, cx);
            }
        }
        cx.notify();
    }

    pub fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            cx.notify();
        }
    }

    /// Delete on the row under the cursor.
    pub fn delete_cursor(&mut self, cx: &mut Context<Self>) {
        match self.cursor.clone() {
            Some(RowKey::Thread(id)) => self.request_delete(&id, cx),
            Some(RowKey::Folder(project)) => self.request_remove_project(&project, cx),
            _ => {}
        }
    }

    /// F2 on the row under the cursor.
    pub fn rename_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.cursor.clone() {
            Some(RowKey::Thread(id)) => self.start_rename_thread(&id, window, cx),
            Some(RowKey::Folder(project)) => self.start_rename_project(&project, window, cx),
            _ => {}
        }
    }

    // --- adding a project --------------------------------------------------------------------

    /// The folder picker; the chosen folder becomes a project.
    pub fn add_project_dialog(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(folder) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.add_project(folder, cx));
            }
        })
        .detach();
    }

    /// Adds a folder as a project (named after the folder).
    pub fn add_project(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let path = folder.display().to_string();
        // A folder that already is one of the user's projects is not added twice (Windows paths:
        // case and a trailing separator do not make another folder).
        let key = |p: &str| p.trim_end_matches(['/', '\\']).to_lowercase();
        let existing = self
            .model
            .read(cx)
            .snapshot()
            .projects
            .iter()
            .find(|p| p.kind == ProjectKind::User && p.path.as_deref().map(key) == Some(key(&path)))
            .map(|p| p.id.clone());
        if let Some(id) = existing {
            self.set_folder_closed(&id, false, cx);
            self.cursor = Some(RowKey::Folder(id));
            cx.notify();
            return;
        }
        let name = folder
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().to_string());
        self.apply(Op::AddProject { folder: path, name }, cx);
    }

    // --- clipboard ---------------------------------------------------------------------------

    /// A project's folder on the clipboard.
    pub fn copy_project_path(&mut self, project: &str, cx: &mut Context<Self>) {
        let path = self
            .model
            .read(cx)
            .snapshot()
            .project(project)
            .and_then(|p| p.path.clone());
        if let Some(path) = path {
            cx.write_to_clipboard(ClipboardItem::new_string(path));
        }
    }

    pub fn copy_thread_id(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
    }

    // --- key actions -------------------------------------------------------------------------

    pub(super) fn on_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.cancel_confirm(cx);
        } else if self.rename.is_some() {
            self.cancel_rename(window, cx);
        } else if self.filter.is_some() {
            self.close_filter(window, cx);
        }
    }

    pub(super) fn on_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.accept_confirm(cx);
        } else if !self.typing(window, cx) {
            self.activate_cursor(cx);
        }
    }

    pub(super) fn on_move(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() || self.rename.is_some() {
            return;
        }
        // From the filter box, the arrows go to the list and the keyboard follows them.
        if self.typing(window, cx) {
            self.focus(window, cx);
        }
        self.move_cursor(down, cx);
    }

    pub(super) fn on_jump(&mut self, last: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_none() && !self.typing(window, cx) {
            self.jump_cursor(last, cx);
        }
    }

    pub(super) fn on_fold(&mut self, closed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() || self.typing(window, cx) {
            return;
        }
        if let Some(RowKey::Folder(project)) = self.cursor.clone() {
            self.set_folder_closed(&project, closed, cx);
        }
    }

    pub(super) fn on_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_none() && !self.typing(window, cx) {
            self.rename_cursor(window, cx);
        }
    }

    pub(super) fn on_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_none() && !self.typing(window, cx) {
            self.delete_cursor(cx);
        }
    }
}
