//! The right panel (Phase 6): a tab strip over the tab that is open. CP6.1 builds the strip and the
//! **Diff** tab ([`diff`]); CP6.2 adds Files and CP6.3 Terminal by adding a [`Tab`] to
//! [`Tab::ALL`] and a body to [`RightPanel`]'s render: the strip, the tab model ([`Tabs`]) and the
//! actions it answers are already shaped for them. Only the tabs that exist are shown.
//!
//! What the panel answers (the workspace forwards them):
//! - **Open diff** ([`crate::thread::OpenDiffTab`], from a file-edit card and the turn footer):
//!   the Diff tab opens; with a path, on this turn's changes, scrolled to that file.
//! - **A file link** ([`crate::thread::OpenFileInPanel`]): until CP6.2 has a file view, the file
//!   opens in the Diff tab when it is one of the working tree's changes (scrolled to it); else the
//!   Files tab appears with a placeholder that names the file and says the file view arrives with
//!   CP6.2. No toast.
//!
//! The panel never reads the core; [`diff::DiffPanel`] asks the backend through the app model.
//!
//! t3code: `apps/web/src/components/RightPanelTabs.tsx` (the strip: a tab is `h-6 rounded-md
//! pl-1.5 pr-2 text-xs` with its icon, the active one on `bg-accent`), `rightPanelStore.ts` (which
//! tabs are open), `DiffPanelShell.tsx` (the panel under the strip). Reference: `sidebar-diff.png`,
//! `sidebar-right.png` (the strip: a "Diff" tab with its icon). Gallery story: `panel-diff`.
//!
//! Not copied: the "+" new-tab button (nothing to add until CP6.2/6.3), closing and reordering
//! tabs, preview and device tabs.

pub mod backend;
pub mod data;
pub mod diff;
pub mod fixture;
#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;

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

/// A tab of the right panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Diff,
    /// CP6.2 builds the file view; until then it holds a placeholder for a file link.
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

/// Which tabs are open and which one is shown. Diff is always open; the others appear when
/// something asks for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tabs {
    open: Vec<Tab>,
    active: Tab,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            open: vec![Tab::Diff],
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

    /// Closes `tab` (not Diff); the Diff tab shows when the active one is closed.
    pub fn close(&mut self, tab: Tab) {
        if tab == Tab::Diff {
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
}

pub struct RightPanel {
    tabs: Tabs,
    diff: Entity<DiffPanel>,
    /// The file a link named that is not among the changes (shown in the Files placeholder).
    file: Option<OpenFileInPanel>,
    /// A file link waiting for the changes to be read, to decide where it opens.
    waiting: Option<OpenFileInPanel>,
    thread: Option<Entity<ThreadModel>>,
    visible: bool,
    thread_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RightPanelEvent> for RightPanel {}

impl RightPanel {
    pub fn new(app: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(app, cx));
        let subscriptions = vec![
            cx.subscribe(&diff, Self::on_diff_event),
            // The window coming forward: files may have changed while it was away.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.diff.update(cx, |d, cx| d.refresh_if_stale(cx));
                }
            }),
        ];
        Self {
            tabs: Tabs::default(),
            diff,
            file: None,
            waiting: None,
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

    pub fn file(&self) -> Option<&OpenFileInPanel> {
        self.file.as_ref()
    }

    /// The thread whose changes the panel shows.
    pub fn set_thread(&mut self, thread: Option<Entity<ThreadModel>>, cx: &mut Context<Self>) {
        if self.thread == thread {
            return;
        }
        self.thread = thread.clone();
        self.file = None;
        self.waiting = None;
        self.diff.update(cx, |d, cx| d.set_thread(thread, cx));
        self.on_thread_changed(cx);
        cx.notify();
    }

    fn on_thread_changed(&mut self, cx: &mut Context<Self>) {
        self.subscribe_thread(cx);
        if self.visible && self.tabs.active() == Tab::Diff {
            self.diff.update(cx, |d, cx| d.show(cx));
        }
    }

    fn subscribe_thread(&mut self, cx: &mut Context<Self>) {
        self.thread_subscription = self.thread.as_ref().map(|thread| {
            cx.subscribe(thread, |this, _, event: &ThreadEvent, cx| {
                if matches!(event, ThreadEvent::TurnEnded) {
                    this.diff.update(cx, |d, cx| d.invalidate(cx));
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
        self.diff.update(cx, |d, cx| {
            d.set_visible(visible && self.tabs.active() == Tab::Diff, cx)
        });
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    fn activate(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tabs.activate(tab) {
            let visible = self.visible && tab == Tab::Diff;
            self.diff.update(cx, |d, cx| d.set_visible(visible, cx));
            cx.notify();
        }
    }

    /// "Open diff": the Diff tab, on this turn's changes when `path` names one of its files
    /// (a card or the turn footer), scrolled to that file.
    pub fn open_diff(&mut self, path: Option<&str>, cx: &mut Context<Self>) {
        self.tabs.show(Tab::Diff);
        self.file = None;
        self.waiting = None;
        let visible = self.visible;
        self.diff.update(cx, |d, cx| {
            d.set_visible(visible, cx);
            if let Some(path) = path {
                d.focus_turn(cx);
                d.show_file(path, cx);
            }
            d.show(cx);
        });
        cx.notify();
    }

    /// A file link: the file in the diff when it is changed, else the Files tab's placeholder
    /// (CP6.2 builds the file view).
    pub fn open_file(&mut self, file: &OpenFileInPanel, cx: &mut Context<Self>) {
        let changed = self.diff.read(cx).has_file(&file.path);
        match changed {
            Some(true) => self.show_changed(file, cx),
            Some(false) => self.show_placeholder(file, cx),
            None => {
                // Not read yet: read the working tree, then decide.
                self.waiting = Some(file.clone());
                self.tabs.show(Tab::Diff);
                let visible = self.visible;
                self.diff.update(cx, |d, cx| {
                    d.set_visible(visible, cx);
                    d.set_scope_quietly(Scope::WorkingTree);
                    d.show(cx);
                });
                cx.notify();
            }
        }
    }

    fn show_changed(&mut self, file: &OpenFileInPanel, cx: &mut Context<Self>) {
        self.tabs.show(Tab::Diff);
        self.file = None;
        let visible = self.visible;
        self.diff.update(cx, |d, cx| {
            d.set_visible(visible, cx);
            d.show_file(&file.path, cx);
        });
        cx.notify();
    }

    fn show_placeholder(&mut self, file: &OpenFileInPanel, cx: &mut Context<Self>) {
        self.file = Some(file.clone());
        self.tabs.show(Tab::Files);
        self.diff.update(cx, |d, cx| d.set_visible(false, cx));
        cx.notify();
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
            DiffPanelEvent::Loaded => {
                if let Some(file) = self.waiting.take() {
                    if self.diff.read(cx).has_file(&file.path) == Some(true) {
                        self.show_changed(&file, cx);
                    } else {
                        self.show_placeholder(&file, cx);
                    }
                }
            }
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
        let palette = cx.design_theme().palette;
        let panel = cx.entity();
        let body = match self.tabs.active() {
            Tab::Diff => self.diff.clone().into_any_element(),
            Tab::Files => {
                let label = self.file.as_ref().map(|f| f.label()).unwrap_or_default();
                div()
                    .id("panel-files")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .p(px(24.))
                    .text_size(text::XS.size)
                    .text_center()
                    .text_color(palette.muted_foreground.hsla())
                    .child(
                        div()
                            .id("panel-file")
                            .text_color(palette.foreground.hsla())
                            .child(label)
                            .probe(),
                    )
                    .child("The file view arrives with CP6.2.")
                    .probe()
                    .into_any_element()
            }
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
