//! The right panel (Phase 6): a tab strip over the tab that is open. CP6.1 builds the strip and the
//! **Diff** tab ([`diff`]); CP6.2 the **Files** tab ([`files`]: the tree, the file view, the
//! finder); CP6.3 the **Terminal** tab (the thread's panel terminals, [`crate::terminal::group`]:
//! separate from the drawer's, as in t3code) and the strip's "+" button, which shows t3code's
//! "Open a surface" chooser (Terminal `T`, Files `F`, Diff `D`) in place of the tab's body. Diff
//! and Files are always in the strip; Terminal appears when chosen and has a close button (its
//! shells keep running: the "+" chooser brings them back; the trash button of the terminal itself
//! ends one). The Terminal tab's label is the active terminal's (a program's title, else
//! "Terminal N").
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
//! `sidebar-filetree.png` (a "Files" tab), `sidebar-right.png` ("Open a surface": a centred
//! `max-w-xs` list of `h-8` rows, icon, label, `Kbd` letter). Gallery stories: `panel-diff`,
//! `panel-files`, `panel-terminal`.
//!
//! Not copied: reordering tabs, closing Diff or Files, the browser, pull request, device and side
//! chat surfaces (dropped by the plan).

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
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    SharedString,
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
    design::kbd::Kbd,
    dev::ProbeExt as _,
    model::AppModel,
    terminal::group::{GroupEvent, Surface, TerminalGroup, TerminalHub},
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
    /// The thread's panel terminals.
    Terminal,
}

impl Tab {
    /// Every tab the panel can have, in strip order.
    pub const ALL: [Tab; 3] = [Tab::Diff, Tab::Files, Tab::Terminal];

    /// What the "+" chooser offers, in its order (t3code's: Terminal, Files, Diff).
    pub const CHOOSER: [Tab; 3] = [Tab::Terminal, Tab::Files, Tab::Diff];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Diff => "Diff",
            Tab::Files => "Files",
            Tab::Terminal => "Terminal",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Tab::Diff => "panel-tab-diff",
            Tab::Files => "panel-tab-files",
            Tab::Terminal => "panel-tab-terminal",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Tab::Diff => IconName::FileDiff,
            Tab::Files => IconName::Folder,
            Tab::Terminal => IconName::SquareTerminal,
        }
    }

    /// The chooser's letter (t3code's `surfaceShortcutKey`).
    pub fn key(self) -> &'static str {
        match self {
            Tab::Diff => "D",
            Tab::Files => "F",
            Tab::Terminal => "T",
        }
    }
}

/// Which tabs are open and which one is shown. Diff and Files are always open; Terminal appears
/// when chosen with "+" and can be closed.
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
    /// The panel's terminals, per thread.
    terminals: Entity<TerminalHub>,
    /// The "+" chooser is showing in place of the tab's body.
    choosing: bool,
    chooser_focus: FocusHandle,
    thread: Option<Entity<ThreadModel>>,
    visible: bool,
    thread_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RightPanelEvent> for RightPanel {}

impl RightPanel {
    pub fn new(app: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(app.clone(), cx));
        let terminals = cx.new(|_| TerminalHub::new(app.clone(), Surface::Panel));
        let files = cx.new(|cx| FilesPanel::new(app, window, cx));
        let subscriptions = vec![
            cx.subscribe(&diff, Self::on_diff_event),
            cx.subscribe(&files, Self::on_files_event),
            cx.subscribe(&terminals, |_, _, event: &GroupEvent, cx| match event {
                GroupEvent::Notice(spec) => cx.emit(RightPanelEvent::Notice(spec.clone())),
                // A label in the strip may have changed.
                GroupEvent::Changed => cx.notify(),
            }),
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
            terminals,
            choosing: false,
            chooser_focus: cx.focus_handle(),
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
        self.choosing = false;
        self.apply_visibility(cx);
        cx.notify();
    }

    fn activate(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tabs.activate(tab) {
            self.choosing = false;
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
        self.choosing = false;
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
        self.choosing = false;
        self.apply_visibility(cx);
        self.files
            .update(cx, |f, cx| f.open(&file.path, file.line, window, cx));
        cx.notify();
    }

    /// "Diff" in a file's header: the Diff tab on the working tree's changes, at that file.
    pub fn view_diff(&mut self, path: &str, cx: &mut Context<Self>) {
        self.tabs.show(Tab::Diff);
        self.choosing = false;
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

    // --- the "+" chooser and the Terminal tab ---------------------------------------------------

    /// The "+" button: shows (or hides) the "Open a surface" chooser.
    pub fn toggle_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choosing = !self.choosing;
        if self.choosing {
            window.focus(&self.chooser_focus, cx);
        }
        cx.notify();
    }

    pub fn is_choosing(&self) -> bool {
        self.choosing
    }

    /// A pick in the chooser: that tab (a Terminal tab starts the thread's first panel terminal).
    pub fn choose(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.choosing = false;
        match tab {
            Tab::Diff => self.open_diff(None, cx),
            Tab::Files => self.show_tab(Tab::Files, cx),
            Tab::Terminal => self.open_terminal(window, cx),
        }
    }

    /// The Terminal tab, with the thread's panel terminals (the first starts now) and the focus.
    pub fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(Tab::Terminal, cx);
        if let Some(group) = self.terminal_group(cx) {
            group.update(cx, |g, cx| {
                g.ensure_started(window, cx);
                g.focus_active(window, cx);
            });
        }
    }

    /// Closes the Terminal tab (its shells keep running; "+" then Terminal brings them back).
    pub fn close_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tabs.close(tab);
        self.apply_visibility(cx);
        cx.notify();
    }

    /// The panel's terminal hub.
    pub fn terminals(&self) -> &Entity<TerminalHub> {
        &self.terminals
    }

    /// The open thread's panel terminals (made now if it has none).
    pub fn terminal_group(&mut self, cx: &mut Context<Self>) -> Option<Entity<TerminalGroup>> {
        let session = self.thread.as_ref()?.read(cx).session().to_string();
        Some(self.terminals.update(cx, |hub, cx| hub.group(&session, cx)))
    }

    fn chooser_key(&mut self, event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = event.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                self.choosing = false;
                cx.notify();
            }
            key => {
                if let Some(tab) = Tab::CHOOSER
                    .into_iter()
                    .find(|t| t.key().eq_ignore_ascii_case(key))
                {
                    cx.stop_propagation();
                    self.choose(tab, window, cx);
                }
            }
        }
    }

    /// t3code's "Open a surface": a centred list of rows (icon, label, letter).
    fn chooser(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let rows = Tab::CHOOSER.into_iter().map(|tab| {
            div()
                .id(SharedString::from(format!("surface-{}", tab.label().to_lowercase())))
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(32.))
                .px(px(10.))
                .rounded(radius::LG)
                .cursor_pointer()
                .text_size(text::SM.size)
                .hover(move |s| s.bg(palette.accent.with_alpha(153).hsla()))
                .on_click(cx.listener(move |this, _, window, cx| this.choose(tab, window, cx)))
                .child(icon(tab.icon()).with_size(KitSize::Size(px(16.))))
                .child(div().flex_1().min_w_0().child(tab.label()))
                .child(Kbd::new(tab.key()))
                .probe()
        });
        div()
            .id("surface-chooser")
            .track_focus(&self.chooser_focus)
            .on_key_down(cx.listener(Self::chooser_key))
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .px(px(24.))
            .child(
                div()
                    .w_full()
                    .max_w(px(320.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .id("surface-chooser-title")
                            .mb(px(12.))
                            .text_center()
                            .text_size(text::SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.foreground.hsla())
                            .child("Open a surface")
                            .probe(),
                    )
                    .children(rows),
            )
            .probe()
            .into_any_element()
    }

    fn terminal_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.terminal_group(cx) {
            Some(group) => {
                group.update(cx, |g, cx| g.ensure_started(window, cx));
                div()
                    .id("panel-terminal")
                    .size_full()
                    .child(group)
                    .probe()
                    .into_any_element()
            }
            None => {
                let palette = cx.design_theme().palette;
                div()
                    .id("panel-terminal-none")
                    .flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .text_size(text::SM.size)
                    .text_color(palette.muted_foreground.hsla())
                    .child("Open a thread to use its terminal.")
                    .probe()
                    .into_any_element()
            }
        }
    }

    /// The Terminal tab's label: its active terminal's.
    fn tab_label(&self, tab: Tab, cx: &App) -> SharedString {
        if tab == Tab::Terminal
            && let Some(session) = self.thread.as_ref().map(|t| t.read(cx).session().to_string())
            && let Some(label) = self
                .terminals
                .read(cx)
                .existing(&session)
                .and_then(|g| g.read(cx).label(cx))
        {
            return label.into();
        }
        tab.label().into()
    }
}

/// The tab strip: a `h-10` row with the tabs left and the "+" button after them (t3code's
/// `RightPanelTabs`).
fn tab_strip(
    panel: &Entity<RightPanel>,
    tabs: &Tabs,
    labels: &[SharedString],
    choosing: bool,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let plus = {
        let panel = panel.clone();
        div()
            .id("panel-tab-new")
            .flex()
            .items_center()
            .justify_center()
            .size(px(24.))
            .flex_shrink_0()
            .rounded(radius::MD)
            .cursor_pointer()
            .text_color(palette.muted_foreground.hsla())
            .when(choosing, |d| {
                d.bg(palette.accent.hsla())
                    .text_color(palette.foreground.hsla())
            })
            .hover(move |s| s.bg(palette.accent.hsla().opacity(0.6)))
            .tooltip(|window, cx| {
                crate::design::tooltip::view("Open a surface".into(), None, window, cx)
            })
            .on_click(move |_, window, cx| panel.update(cx, |p, cx| p.toggle_chooser(window, cx)))
            .child(icon(IconName::Plus).with_size(KitSize::Size(px(14.))))
            .probe()
    };
    div()
        .id("panel-tabs")
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(crate::design::panel::HEADER_HEIGHT))
        .flex_shrink_0()
        .px(px(8.))
        .overflow_hidden()
        .border_b_1()
        .border_color(palette.border.hsla())
        .children(tabs.open().iter().zip(labels.iter().cloned()).map(|(&tab, label)| {
            let active = tab == tabs.active() && !choosing;
            let panel = panel.clone();
            let closable = tab == Tab::Terminal;
            div()
                .id(tab.id())
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .min_w_0()
                .max_w(px(180.))
                .pl(px(6.))
                .pr(px(if closable { 4. } else { 8. }))
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
                .on_click({
                    let panel = panel.clone();
                    move |_, window, cx| {
                        panel.update(cx, |p, cx| {
                            if tab == Tab::Terminal {
                                p.open_terminal(window, cx);
                            } else {
                                p.activate(tab, cx);
                            }
                        })
                    }
                })
                .child(icon(tab.icon()).with_size(KitSize::Size(px(14.))))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
                .when(closable, |d| {
                    d.child(
                        div()
                            .id("panel-tab-terminal-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .flex_shrink_0()
                            .rounded(radius::SM)
                            .hover(move |s| s.bg(palette.accent.hsla()))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                panel.update(cx, |p, cx| p.close_tab(Tab::Terminal, cx));
                            })
                            .child(icon(IconName::Close).with_size(KitSize::Size(px(10.))))
                            .probe(),
                    )
                })
                .probe()
        }))
        .child(plus)
        .probe()
        .into_any_element()
}

impl Render for RightPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.entity();
        let labels: Vec<SharedString> = self
            .tabs
            .open()
            .iter()
            .map(|&tab| self.tab_label(tab, cx))
            .collect();
        let body = if self.choosing {
            self.chooser(cx)
        } else {
            match self.tabs.active() {
                Tab::Diff => self.diff.clone().into_any_element(),
                Tab::Files => self.files.clone().into_any_element(),
                Tab::Terminal => self.terminal_body(window, cx),
            }
        };
        div()
            .id("right-body")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(tab_strip(&panel, &self.tabs, &labels, self.choosing, cx))
            .child(div().flex_1().min_h_0().child(body))
            .probe()
    }
}
