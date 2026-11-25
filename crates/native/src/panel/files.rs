//! The Files tab: the project's folder as a tree you open folder by folder, and the file view
//! ([`super::viewer`]) it opens files in. **Tree mode** shows a toolbar (refresh, a "Search files"
//! field that opens the finder, collapse all) over the tree; **file mode** replaces it with the
//! file and a back button, so a narrow panel is never split in two. The tree's root is the open
//! thread's folder.
//!
//! **Lazy:** only the root is read when the tab is first shown; a folder is read when it is
//! opened (on a background task, through the backend: [`crate::model::Backend::project_list`]),
//! never one that is closed, so a `node_modules` or `target` costs nothing until it is opened;
//! what git ignores is shown dimmed; a folder listing is cut at [`super::fs::LIST_LIMIT`] entries
//! and says how many were left out. The visible rows are one flat list ([`super::tree::Tree`])
//! drawn by a virtual list, so only the rows on screen are built, however many folders are open.
//! A refresh (the button, the window coming forward, a turn ending) lists the root and the open
//! folders again and keeps the rows meanwhile.
//!
//! **Keys** (while the tree has the focus): Up and Down move, Left closes a folder or goes to the
//! one above, Right opens a folder or goes into it, Home and End, Enter and Space open a file or
//! toggle a folder.
//!
//! A file asked for by [`FilesPanel::open`] (a link in a message, the finder) is read and shown;
//! when it is read the tree opens the folders above it and selects it. Anything outside the
//! folder, credentials or links out are refused by the backend and shown as a message.
//!
//! t3code: `apps/web/src/components/files/FileBrowserPanel.tsx` (the toolbar: refresh, "Search
//! files", expand and collapse all; the tree; dimmed ignored folders), `FileExplorerPane.tsx`,
//! `useDirectoryEntries.ts`, `loadFileAncestors.ts`, `RightPanelTabs.tsx` (the Files tab).
//! Reference: `sidebar-filetree.png`. Gallery story: `panel-files`.
//!
//! Not copied: the resizable tree column beside the file (the tree and the file share the panel by
//! turns), drag-to-mention, the context menu, git status dots on folders, per-type colour icons.

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    UniformListScrollHandle, Window, actions,
    assets::IconName,
    component::{Sizable as _, Size as KitSize},
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use super::tree::{Row, RowKind, Step, Tree};
use super::viewer::{FileView, ViewEvent};
use crate::design::{
    KbdGroup, Palette, ToastSpec,
    button::{Size, Variant, icon_button},
    icons::icon,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::keymap;
use crate::model::AppModel;
use crate::theme::DesignTheme as _;
use crate::thread::ThreadModel;

actions!(
    hermes_files,
    [
        /// Down: the next row.
        TreeDown,
        /// Up: the previous row.
        TreeUp,
        /// Home: the first row.
        TreeFirst,
        /// End: the last row.
        TreeLast,
        /// Right: open a folder, or go into it.
        TreeExpand,
        /// Left: close a folder, or go to the one above.
        TreeCollapse,
        /// Enter and Space: open the file, or open or close the folder.
        TreeActivate,
    ]
);

/// The key context the tree declares.
pub const CONTEXT: &str = "FileTree";

/// Registers the tree's keys.
pub fn bind(cx: &mut App) {
    use gpui_kit::KeyBinding;
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", TreeDown, context),
        KeyBinding::new("up", TreeUp, context),
        KeyBinding::new("home", TreeFirst, context),
        KeyBinding::new("end", TreeLast, context),
        KeyBinding::new("right", TreeExpand, context),
        KeyBinding::new("left", TreeCollapse, context),
        KeyBinding::new("enter", TreeActivate, context),
        KeyBinding::new("space", TreeActivate, context),
    ]);
}

/// A row of the tree.
pub const ROW_HEIGHT: f32 = 26.;
/// How far each level of folders is indented.
pub const INDENT: f32 = 14.;

/// The icon of a file: text, picture, code, or any.
pub fn file_icon(name: &str) -> IconName {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" | "mdx" | "txt" | "log" | "rst" => IconName::FileText,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff" => {
            IconName::FileImage
        }
        "rs" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "json" | "toml" | "yaml" | "yml"
        | "sh" | "ps1" | "c" | "h" | "cpp" | "hpp" | "go" | "java" | "rb" | "css" | "html"
        | "xml" | "sql" | "lua" | "kt" | "cs" | "php" | "swift" => IconName::FileCode,
        _ => IconName::File,
    }
}

/// What the tab shows: the tree, or one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Tree,
    File,
}

/// What the Files tab tells the panel.
#[derive(Clone, Debug)]
pub enum FilesEvent {
    Notice(ToastSpec),
    /// "View diff" for this file.
    ViewDiff(String),
    /// The search field was clicked.
    OpenFinder,
    /// A file was shown (its canonical path): the panel can tell whether it has changes.
    Shown(String),
}

pub struct FilesPanel {
    app: Entity<AppModel>,
    thread: Option<Entity<ThreadModel>>,
    session: Option<String>,
    tree: Tree,
    view: Entity<FileView>,
    mode: Mode,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    visible: bool,
    /// Bumped when the thread changes: an answer for an older one is dropped.
    generation: u64,
    /// The file the tree is to scroll to once its folders are listed.
    reveal: Option<String>,
    /// Listings in flight.
    pending: usize,
    /// Listings asked for, for the tests.
    lists: Vec<String>,
    last: Option<std::time::Instant>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FilesEvent> for FilesPanel {}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FilesPanel {
    pub fn new(app: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.new(|_| FileView::new(app.clone()));
        let subscription = cx.subscribe_in(&view, window, Self::on_view_event);
        Self {
            app,
            thread: None,
            session: None,
            tree: Tree::new(),
            view,
            mode: Mode::Tree,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            visible: false,
            generation: 0,
            reveal: None,
            pending: 0,
            lists: Vec::new(),
            last: None,
            _subscriptions: vec![subscription],
        }
    }

    /// Gives the tree its listings and open folders directly (stories: nothing is read).
    pub fn seed(
        &mut self,
        listings: Vec<(String, super::fs::Listing)>,
        open: &[&str],
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        for (dir, listing) in listings {
            self.tree.apply(&dir, Ok(listing));
        }
        for dir in open {
            self.tree.open_dir(dir);
        }
        if let Some(path) = selected {
            self.tree.select(path);
        }
        cx.notify();
    }

    /// Shows the file view (stories).
    pub fn show_file_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::File;
        cx.notify();
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn view(&self) -> &Entity<FileView> {
        &self.view
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Every folder listing asked for so far (`""` is the project), in order.
    pub fn lists(&self) -> &[String] {
        &self.lists
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The thread whose folder the tab shows. Another thread forgets the tree and the file.
    pub fn set_thread(&mut self, thread: Option<Entity<ThreadModel>>, cx: &mut Context<Self>) {
        if self.thread == thread {
            return;
        }
        let session = thread.as_ref().map(|t| t.read(cx).session().to_string());
        self.thread = thread;
        self.session = session.clone();
        self.generation += 1;
        self.tree.clear();
        self.lists.clear();
        self.reveal = None;
        self.pending = 0;
        self.mode = Mode::Tree;
        self.view.update(cx, |v, cx| v.set_session(session, cx));
        if self.visible {
            self.show(cx);
        }
        cx.notify();
    }

    /// Whether the tab is on screen: it reads nothing while it is not.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if visible {
                self.show(cx);
            }
        }
    }

    /// Reads the project's folder when the tab is on screen and has not yet.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.visible && self.session.is_some() && self.tree.begin("") {
            self.list("".to_string(), cx);
        }
    }

    /// Lists the project and every open folder again (rows stay until the answers land).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        if !self.tree.started() {
            self.show(cx);
            return;
        }
        self.last = Some(std::time::Instant::now());
        for dir in self.tree.invalidate() {
            self.list(dir, cx);
        }
    }

    /// [`Self::refresh`] when on screen and the last read is over two seconds old (the window
    /// coming back to the front, a turn that ended).
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let old = self
            .last
            .is_none_or(|last| last.elapsed() > std::time::Duration::from_secs(2));
        if self.visible && self.session.is_some() && old && self.pending == 0 {
            self.refresh(cx);
        }
    }

    fn list(&mut self, dir: String, cx: &mut Context<Self>) {
        let (Some(session), Some(backend)) = (self.session.clone(), self.app.read(cx).backend())
        else {
            return;
        };
        let inherit = self.tree.is_ignored(&dir);
        let generation = self.generation;
        self.pending += 1;
        self.lists.push(dir.clone());
        self.last = Some(std::time::Instant::now());
        cx.spawn(async move |this, cx| {
            let answer = {
                let dir = dir.clone();
                cx.background_spawn(async move { backend.project_list(&session, &dir, inherit) })
                    .await
            };
            let _ = this.update(cx, |this, cx| this.listed(generation, dir, answer, cx));
        })
        .detach();
    }

    fn listed(
        &mut self,
        generation: u64,
        dir: String,
        answer: Result<super::fs::Listing, super::fs::Refused>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.pending = self.pending.saturating_sub(1);
        self.tree.apply(&dir, answer.map_err(|r| r.message()));
        self.scroll_to_reveal();
        cx.notify();
    }

    /// Scrolls to the file being revealed once its row exists.
    fn scroll_to_reveal(&mut self) {
        if let Some(path) = self.reveal.clone()
            && let Some(ix) = self.tree.row_of(&path)
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
            self.reveal = None;
        }
    }

    // --- opening ---

    /// Shows a file (a path relative to the project, absolute or a `file:` link), with `line`
    /// selected. The tree catches up when the file has been read (its folders open, it is selected).
    pub fn open(
        &mut self,
        path: &str,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mode = Mode::File;
        self.view.update(cx, |v, cx| v.open(path, line, window, cx));
        cx.notify();
    }

    /// The file's canonical path arrived: reveal it in the tree.
    fn on_shown(&mut self, rel: &str, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        if self.tree.begin("") {
            self.list(String::new(), cx);
        }
        for dir in self.tree.reveal(rel) {
            self.list(dir, cx);
        }
        self.reveal = Some(rel.to_string());
        self.scroll_to_reveal();
        cx.emit(FilesEvent::Shown(rel.to_string()));
        cx.notify();
    }

    fn on_view_event(
        &mut self,
        _: &Entity<FileView>,
        event: &ViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ViewEvent::Back => self.back(window, cx),
            ViewEvent::Notice(spec) => cx.emit(FilesEvent::Notice(spec.clone())),
            ViewEvent::ViewDiff(path) => cx.emit(FilesEvent::ViewDiff(path.clone())),
            ViewEvent::Shown(rel) => self.on_shown(rel, cx),
        }
    }

    /// Back from a file to the tree.
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Tree;
        self.focus.focus(window, cx);
        // The file's row is where the tree was left.
        if let Some(path) = self.view.read(cx).shown().map(str::to_string) {
            self.reveal = Some(path);
            self.scroll_to_reveal();
        }
        cx.notify();
    }

    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        cx.notify();
    }

    /// A click on row `ix`.
    fn click(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some(row) = self.tree.rows().get(ix).cloned() else {
            return;
        };
        self.activate_row(&row, window, cx);
    }

    fn activate_row(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        match row.kind {
            RowKind::Note { .. } => {}
            RowKind::Dir { .. } => {
                self.tree.select(&row.path);
                if let Some(dir) = self.tree.toggle(&row.path) {
                    self.list(dir, cx);
                }
            }
            RowKind::File => {
                self.tree.select(&row.path);
                self.open(&row.path, None, window, cx);
            }
        }
        cx.notify();
    }

    fn key(&mut self, step: Step, cx: &mut Context<Self>) {
        if let Some(dir) = self.tree.step(step) {
            self.list(dir, cx);
        }
        if let Some(ix) = self.tree.selected_row() {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn key_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tree.selected_row() else {
            return;
        };
        if let Some(row) = self.tree.rows().get(ix).cloned() {
            self.activate_row(&row, window, cx);
        }
    }
}

// --- drawing ----------------------------------------------------------------------------------------

fn sized(name: IconName, size: f32) -> gpui_kit::component::Icon {
    icon(name).with_size(KitSize::Size(px(size)))
}

fn id2(name: &'static str, n: usize) -> ElementId {
    ElementId::NamedInteger(name.into(), n as u64)
}

impl FilesPanel {
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let refreshing = self.pending > 0;
        let any_open = self.tree.any_open();
        let shortcut = keymap::find("files.find")
            .and_then(keymap::shortcut_label)
            .unwrap_or_default();
        div()
            .id("files-toolbar")
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(crate::design::panel::HEADER_HEIGHT))
            .flex_shrink_0()
            .px(px(8.))
            .border_b_1()
            .border_color(palette.border.hsla())
            .child(
                icon_button("files-refresh", IconName::RefreshCw, "Refresh the files")
                    .variant(Variant::GhostMuted)
                    .size(Size::Xs)
                    .disabled(refreshing)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                div()
                    .id("files-search")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(radius::MD)
                    .cursor_pointer()
                    .text_size(text::XS.size)
                    .text_color(palette.muted_foreground.hsla())
                    .hover(move |s| s.bg(palette.accent.hsla().opacity(0.6)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FilesEvent::OpenFinder)))
                    .child(sized(IconName::Search, 14.))
                    .child(div().flex_1().min_w_0().truncate().child("Search files"))
                    .when(!shortcut.is_empty(), |d| d.child(KbdGroup::new(&shortcut)))
                    .probe(),
            )
            .child(
                icon_button(
                    "files-collapse",
                    IconName::ChevronsDownUp,
                    "Collapse all folders",
                )
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .disabled(!any_open)
                .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            )
            .probe()
            .into_any_element()
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let Some(row) = self.tree.rows().get(ix) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let muted = palette.muted_foreground.hsla();
        let left = px(8. + row.depth as f32 * INDENT);
        let selected = self.tree.selected() == Some(row.path.as_str()) && !row.is_note();
        let base = div()
            .id(id2("file-row", ix))
            .flex()
            .w_full()
            .items_center()
            .gap(px(6.))
            .h(px(ROW_HEIGHT))
            .pl(left)
            .pr(px(8.))
            .overflow_hidden();
        match &row.kind {
            RowKind::Note { error } => base
                .pl(left + px(20.))
                .text_size(text::XXS2.size)
                .text_color(if *error {
                    palette.destructive_foreground.hsla()
                } else {
                    muted
                })
                .child(div().min_w_0().truncate().child(super::fs::shown_name(&row.name)))
                .probe()
                .into_any_element(),
            kind => {
                let is_dir = matches!(kind, RowKind::Dir { .. });
                let open = matches!(kind, RowKind::Dir { open: true });
                let dim = row.ignored || row.escapes;
                let name_color = if dim {
                    palette.foreground.hsla().opacity(0.5)
                } else {
                    palette.foreground.hsla()
                };
                base.cursor_pointer()
                    .when(selected, |d| d.bg(palette.accent.hsla()))
                    .hover(move |s| s.bg(palette.accent.hsla().opacity(0.6)))
                    .on_click(cx.listener(move |this, _, window, cx| this.click(ix, window, cx)))
                    .child(
                        div()
                            .id(id2("file-chevron", ix))
                            .flex_shrink_0()
                            .size(px(14.))
                            .text_color(muted)
                            .when(is_dir, |d| {
                                d.child(sized(
                                    if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    },
                                    14.,
                                ))
                            })
                            .probe(),
                    )
                    .child(
                        div()
                            .id(id2("file-icon", ix))
                            .flex_shrink_0()
                            .text_color(if dim { muted.opacity(0.6) } else { muted })
                            .child(sized(
                                match (is_dir, open) {
                                    (true, true) => IconName::FolderOpen,
                                    (true, false) => IconName::FolderClosed,
                                    (false, _) => file_icon(&row.name),
                                },
                                14.,
                            ))
                            .probe(),
                    )
                    .child(
                        div()
                            .id(id2("file-name", ix))
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(text::XS.size)
                            .text_color(name_color)
                            .child(SharedString::from(super::fs::shown_name(&row.name)))
                            .probe(),
                    )
                    .when(row.escapes, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_color(muted)
                                .child(sized(IconName::Lock, 12.)),
                        )
                    })
                    .probe()
                    .into_any_element()
            }
        }
    }

    fn message(
        &self,
        id: &'static str,
        text_: String,
        palette: &Palette,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        div()
            .id(id)
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(24.))
            .text_size(text::XS.size)
            .text_center()
            .text_color(palette.muted_foreground.hsla())
            .child(text_)
    }

    fn tree_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        if self.session.is_none() {
            return self
                .message(
                    "files-empty",
                    "Open a thread to see its files.".to_string(),
                    &palette,
                )
                .probe()
                .into_any_element();
        }
        if let Some(why) = self.tree.root_failure() {
            return self
                .message("files-failed", why.to_string(), &palette)
                .text_color(palette.destructive_foreground.hsla())
                .child(
                    crate::design::button::Button::new("files-retry", "Try again")
                        .variant(Variant::Outline)
                        .size(Size::Xs)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .probe()
                .into_any_element();
        }
        if !self.tree.root_loaded() {
            return self
                .message(
                    "files-loading",
                    "Reading the folder...".to_string(),
                    &palette,
                )
                .probe()
                .into_any_element();
        }
        if self.tree.rows().is_empty() {
            return self
                .message("files-empty", "This folder is empty.".to_string(), &palette)
                .probe()
                .into_any_element();
        }
        let panel = cx.entity();
        let count = self.tree.rows().len();
        div()
            .id("files-list")
            .flex_1()
            .min_h_0()
            .child(
                uniform_list("files-rows", count, move |range: Range<usize>, _, cx| {
                    panel.update(cx, |p, cx| {
                        range.map(|ix| p.row(ix, cx)).collect::<Vec<_>>()
                    })
                })
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .probe()
            .into_any_element()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let root = div()
            .id("files-panel")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TreeDown, _, cx| this.key(Step::Down, cx)))
            .on_action(cx.listener(|this, _: &TreeUp, _, cx| this.key(Step::Up, cx)))
            .on_action(cx.listener(|this, _: &TreeFirst, _, cx| this.key(Step::First, cx)))
            .on_action(cx.listener(|this, _: &TreeLast, _, cx| this.key(Step::Last, cx)))
            .on_action(cx.listener(|this, _: &TreeExpand, _, cx| this.key(Step::Expand, cx)))
            .on_action(cx.listener(|this, _: &TreeCollapse, _, cx| this.key(Step::Collapse, cx)))
            .on_action(
                cx.listener(|this, _: &TreeActivate, window, cx| this.key_activate(window, cx)),
            )
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .text_color(palette.foreground.hsla());
        match self.mode {
            Mode::File => root.child(self.view.clone()).probe(),
            Mode::Tree => root
                .child(self.toolbar(cx))
                .child(self.tree_body(cx))
                .probe(),
        }
    }
}
