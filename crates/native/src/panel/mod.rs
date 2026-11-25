//! The right panel (Phase 6): a tab strip over the tab that is open. CP6.1 builds the strip and the
//! **Diff** tab ([`diff`]); CP6.2 the **Files** tab ([`files`]: the tree, the file view, the
//! finder); CP6.3 adds Terminal by adding a [`Tab`] to [`Tab::ALL`] and a body to [`RightPanel`]'s
//! render: the strip, the tab model ([`Tabs`]) and the actions it answers are already shaped for it.
//!
//! What the panel answers (the workspace forwards them):
//! - **Open diff** ([`crate::thread::OpenDiffTab`], from a file-edit card and the turn footer):
//!   the Diff tab opens; with a path, on this turn's changes, scrolled to that file.
//! - **A file link** ([`crate::thread::OpenFileInPanel`]) and a pick in the finder: the Files tab
//!   opens on that file (at the line the link names). A file that has changes in the working tree
//!   (when the diff has been read) offers "Diff" in its header, which opens the Diff tab on it.
//!
//! The panel never reads the core; [`diff::DiffPanel`] and [`files::FilesPanel`] ask the backend
//! through the app model.
//!
//! t3code: `apps/web/src/components/RightPanelTabs.tsx` (the strip: a tab is `h-6 rounded-md
//! pl-1.5 pr-2 text-xs` with its icon, the active one on `bg-accent`), `rightPanelStore.ts` (which
//! tabs are open), `DiffPanelShell.tsx` (the panel under the strip). Reference:
//! `sidebar-diff.png`, `sidebar-right.png` (the strip: a "Diff" tab with its icon),
//! `sidebar-filetree.png` (a "Files" tab). Gallery stories: `panel-diff`, `panel-files`.
//!
//! Not copied: the "+" new-tab button (the two tabs are always there), closing and reordering
//! tabs, preview and device tabs.

pub mod backend;
pub mod data;
pub mod diff;
pub mod files;
pub mod finder;
pub mod fixture;
pub mod fixture_files;
pub mod fs;
#[cfg(test)]
mod finder_tests;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;
pub mod tree;
pub mod viewer;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    assets::IconName,
    component::{Sizable as _, Size as KitSize},
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    design::{
        ToastSpec,
        icons::icon,
        tokens::{radius, text},
    },
    dev::ProbeExt as _,
    model::AppModel,
    theme::DesignTheme as _,
    thread::{OpenFileInPanel, ThreadEvent, ThreadModel},
};
use data::Scope;
use diff::{DiffPanel, DiffPanelEvent};
use files::{FilesEvent, FilesPanel};

/// A tab of the right panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Diff,
    /// The project's files: a tree, a file view, the finder.
    Files,
}

impl Tab {
    /// Every tab the panel can have, in strip order.
    pub const ALL: [Tab; 2] = [Tab::Diff, Tab::Files];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Diff => "Diff",
            Tab::Files => "Files",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Tab::Diff => "panel-tab-diff",
            Tab::Files => "panel-tab-files",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Tab::Diff => IconName::FileDiff,
            Tab::Files => IconName::Folder,
        }
    }
}

/// Which tabs are open and which one is shown. Diff and Files are always open (there is no "+"
/// button to bring one back); CP6.3's Terminal appears when something asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tabs {
    open: Vec<Tab>,
    active: Tab,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            open: vec![Tab::Diff, Tab::Files],
            active: Tab::Diff,
        }
    }
}

impl Tabs {
    pub fn open(&self) -> &[Tab] {
        &self.open
    }

    pub fn active(&self) -> Tab {
        self.active
    }

    /// Opens `tab` (in strip order) and makes it the active one.
    pub fn show(&mut self, tab: Tab) {
        if !self.open.contains(&tab) {
            self.open.push(tab);
            self.open
                .sort_by_key(|t| Tab::ALL.iter().position(|a| a == t));
        }
        self.active = tab;
    }

    /// Makes an open tab the active one.
    pub fn activate(&mut self, tab: Tab) -> bool {
        if self.open.contains(&tab) {
            self.active = tab;
            true
        } else {
            false
        }
    }

    /// Closes `tab` (not Diff or Files); the Diff tab shows when the active one is closed.
    pub fn close(&mut self, tab: Tab) {
        if matches!(tab, Tab::Diff | Tab::Files) {
            return;
        }
        self.open.retain(|t| *t != tab);
        if self.active == tab {
            self.active = Tab::Diff;
        }
    }
}

/// What the panel tells the workspace.
#[derive(Clone, Debug)]
pub enum RightPanelEvent {
    Notice(ToastSpec),
    /// A file was reverted: the thread's git state changed.
    Reverted,
    /// The Files tab's search field was clicked: the workspace opens the finder.
    OpenFinder,
}

pub struct RightPanel {
    tabs: Tabs,
    diff: Entity<DiffPanel>,
    files: Entity<FilesPanel>,
    thread: Option<Entity<ThreadModel>>,
    visible: bool,
    thread_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RightPanelEvent> for RightPanel {}

impl RightPanel {
    pub fn new(app: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(app.clone(), cx));
        let files = cx.new(|cx| FilesPanel::new(app, window, cx));
        let subscriptions = vec![
            cx.subscribe(&diff, Self::on_diff_event),
            cx.subscribe(&files, Self::on_files_event),
            // The window coming forward: files may have changed while it was away.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.diff.update(cx, |d, cx| d.refresh_if_stale(cx));
                    this.files.update(cx, |f, cx| f.refresh_if_stale(cx));
                }
            }),
        ];
        Self {
            tabs: Tabs::default(),
            diff,
            files,
            thread: None,
            visible: false,
            thread_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    pub fn diff(&self) -> &Entity<DiffPanel> {
        &self.diff
    }

    pub fn files(&self) -> &Entity<FilesPanel> {
        &self.files
    }

    /// The thread whose changes the panel shows.
    pub fn set_thread(&mut self, thread: Option<Entity<ThreadModel>>, cx: &mut Context<Self>) {
        if self.thread == thread {
            return;
        }
        self.thread = thread.clone();
        self.diff.update(cx, |d, cx| d.set_thread(thread.clone(), cx));
        self.files.update(cx, |f, cx| f.set_thread(thread, cx));
        self.on_thread_changed(cx);
        cx.notify();
    }

    fn on_thread_changed(&mut self, cx: &mut Context<Self>) {
        self.subscribe_thread(cx);
        if self.visible && self.tabs.active() == Tab::Diff {
            self.diff.update(cx, |d, cx| d.show(cx));
        }
        self.sync_diff_flag(cx);
    }

    fn subscribe_thread(&mut self, cx: &mut Context<Self>) {
        self.thread_subscription = self.thread.as_ref().map(|thread| {
            cx.subscribe(thread, |this, _, event: &ThreadEvent, cx| {
                if matches!(event, ThreadEvent::TurnEnded) {
                    this.diff.update(cx, |d, cx| d.invalidate(cx));
                    // The turn may have made or removed files.
                    this.files.update(cx, |f, cx| f.refresh_if_stale(cx));
                }
            })
        });
    }

    /// Whether the panel is on screen: it reads nothing while it is not.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.apply_visibility(cx);
    }

    /// Tells each tab whether it is on screen (the panel is shown and the tab is the active one).
    fn apply_visibility(&mut self, cx: &mut Context<Self>) {
        let (visible, active) = (self.visible, self.tabs.active());
        self.diff
            .update(cx, |d, cx| d.set_visible(visible && active == Tab::Diff, cx));
        self.files
            .update(cx, |f, cx| f.set_visible(visible && active == Tab::Files, cx));
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Shows `tab` (stories: nothing is read).
    pub fn show_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tabs.show(tab);
        self.apply_visibility(cx);
        cx.notify();
    }

    fn activate(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tabs.activate(tab) {
            self.apply_visibility(cx);
            cx.notify();
        }
    }

    /// Tells the file view whether the file it shows has changes in the working tree (so its
    /// header can offer the diff), as far as the diff has been read.
    fn sync_diff_flag(&mut self, cx: &mut Context<Self>) {
        let view = self.files.read(cx).view().clone();
        let shown = view.read(cx).shown().map(str::to_string);
        let in_diff = shown
            .and_then(|path| self.diff.read(cx).working_tree_has(&path))
            .unwrap_or(false);
        view.update(cx, |v, cx| v.set_in_diff(in_diff, cx));
    }

    /// "Open diff": the Diff tab, on this turn's changes when `path` names one of its files
    /// (a card or the turn footer), scrolled to that file.
    pub fn open_diff(&mut self, path: Option<&str>, cx: &mut Context<Self>) {
        self.tabs.show(Tab::Diff);
        self.apply_visibility(cx);
        self.diff.update(cx, |d, cx| {
            if let Some(path) = path {
                d.focus_turn(cx);
                d.show_file(path, cx);
            }
            d.show(cx);
        });
        cx.notify();
    }

    /// A file link or a pick in the finder: the Files tab, on that file (at `file.line`).
    pub fn open_file(
        &mut self,
        file: &OpenFileInPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tabs.show(Tab::Files);
        self.apply_visibility(cx);
        self.files
            .update(cx, |f, cx| f.open(&file.path, file.line, window, cx));
        cx.notify();
    }

    /// "Diff" in a file's header: the Diff tab on the working tree's changes, at that file.
    pub fn view_diff(&mut self, path: &str, cx: &mut Context<Self>) {
        self.tabs.show(Tab::Diff);
        self.apply_visibility(cx);
        self.diff.update(cx, |d, cx| {
            d.set_scope_quietly(Scope::WorkingTree);
            d.show_file(path, cx);
            d.show(cx);
        });
        cx.notify();
    }

    fn on_files_event(
        &mut self,
        _: Entity<FilesPanel>,
        event: &FilesEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            FilesEvent::Notice(spec) => cx.emit(RightPanelEvent::Notice(spec.clone())),
            FilesEvent::ViewDiff(path) => self.view_diff(path, cx),
            FilesEvent::OpenFinder => cx.emit(RightPanelEvent::OpenFinder),
            FilesEvent::Shown(_) => self.sync_diff_flag(cx),
        }
    }

    fn on_diff_event(
        &mut self,
        _: Entity<DiffPanel>,
        event: &DiffPanelEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            DiffPanelEvent::Notice(spec) => cx.emit(RightPanelEvent::Notice(spec.clone())),
            DiffPanelEvent::Reverted => cx.emit(RightPanelEvent::Reverted),
            DiffPanelEvent::Loaded => self.sync_diff_flag(cx),
        }
    }
}

/// The tab strip: a `h-10` row with the tabs left (t3code's `RightPanelTabs`).
fn tab_strip(panel: &Entity<RightPanel>, tabs: &Tabs, cx: &App) -> AnyElement {
    let palette = cx.design_theme().palette;
    div()
        .id("panel-tabs")
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(crate::design::panel::HEADER_HEIGHT))
        .flex_shrink_0()
        .px(px(8.))
        .border_b_1()
        .border_color(palette.border.hsla())
        .children(tabs.open().iter().map(|&tab| {
            let active = tab == tabs.active();
            let panel = panel.clone();
            div()
                .id(tab.id())
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .pl(px(6.))
                .pr(px(8.))
                .rounded(radius::MD)
                .cursor_pointer()
                .text_size(text::XS.size)
                .font_weight(if active {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if active {
                    palette.foreground.hsla()
                } else {
                    palette.muted_foreground.hsla()
                })
                .when(active, |d| d.bg(palette.accent.hsla()))
                .hover(move |s| s.bg(palette.accent.hsla().opacity(0.6)))
                .on_click(move |_, _, cx| panel.update(cx, |p, cx| p.activate(tab, cx)))
                .child(icon(tab.icon()).with_size(KitSize::Size(px(14.))))
                .child(tab.label())
                .probe()
        }))
        .probe()
        .into_any_element()
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.entity();
        let body = match self.tabs.active() {
            Tab::Diff => self.diff.clone().into_any_element(),
            Tab::Files => self.files.clone().into_any_element(),
        };
        div()
            .id("right-body")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(tab_strip(&panel, &self.tabs, cx))
            .child(div().flex_1().min_h_0().child(body))
            .probe()
    }
}
