//! A thread's terminals in one surface, and the hub that keeps every thread's.
//!
//! [`TerminalGroup`] is what the drawer (and the right panel's Terminal tab) shows: the active
//! terminal filling the surface; with one terminal, t3code's small floating toolbar at the top
//! right (new, close); with several, t3code's terminal list at the right (a 144 px column: a
//! 22 px header with new and close, then a 24 px row per terminal with its icon and label, the active one
//! highlighted, each with a close button). A label is the program's title (OSC 0/2) or
//! "Terminal N"; there is no rename. The first time a group is shown it starts a terminal; after the
//! last one is closed it shows t3code's empty state ("No terminal sessions for this thread yet."
//! and a New terminal button).
//!
//! Terminals belong to the thread (their shell starts in the thread's folder) and to the surface:
//! the drawer's and the panel's are separate, as in t3code. Switching threads or hiding the drawer
//! keeps them: the shells keep running and their output keeps going through the emulators; only the
//! drawing stops. A shell that exits (`exit`) leaves its group; closing a terminal ends its shell
//! and everything it started.
//!
//! t3code: `apps/web/src/components/ThreadTerminalDrawer.tsx` (the toolbar pill at `right-2
//! top-2`, the `w-36` aside, its `h-[22px]` header, `TerminalSidebarGroup` rows of `h-6` with `rounded-md text-xs pr-2 pl-1.5`;
//! deviation: a close button at the row's end, where t3code swaps the row's icon for one on hover),
//! the empty state,
//! `useThreadTerminalActions.ts` (new, close). Reference: `app-terminal-open.png` (one terminal:
//! the toolbar at the top right).

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, assets::IconName, div,
    prelude::FluentBuilder as _, px,
};

use super::session::Pty;
use super::view::{TerminalEvent, TerminalView};
use crate::{
    design::{
        Button, ToastSpec,
        button::{self, Variant},
        icons::icon,
        tokens::{radius, text},
    },
    dev::ProbeExt as _,
    model::AppModel,
    theme::DesignTheme as _,
};

/// Where a group is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The bottom drawer (`Ctrl+J`).
    Drawer,
    /// The right panel's Terminal tab.
    Panel,
}

impl Surface {
    fn id(self) -> &'static str {
        match self {
            Surface::Drawer => "drawer",
            Surface::Panel => "panel",
        }
    }
}

/// The width of the terminal list (t3code's `w-36`).
pub const LIST_WIDTH: f32 = 144.;
/// The list header's height (`h-[22px]`).
pub const HEADER_HEIGHT: f32 = 22.;
/// A list row's height (`TerminalSidebarGroup`'s `h-6`).
pub const ROW_HEIGHT: f32 = 24.;
/// A thread has at most this many terminals per surface (the core runs at most 16 shells).
pub const MAX_TERMINALS: usize = 8;

/// Makes shells for a group with no app model (stories and tests).
pub type PtyMaker = Arc<dyn Fn(u16, u16) -> Arc<dyn Pty>>;

/// What a group tells its owner.
#[derive(Clone, Debug)]
pub enum GroupEvent {
    Notice(ToastSpec),
    /// Terminals came, went, or a label changed.
    Changed,
}

/// A thread's terminals in one surface.
pub struct TerminalGroup {
    app: Option<Entity<AppModel>>,
    maker: Option<PtyMaker>,
    session: String,
    surface: Surface,
    terminals: Vec<Entity<TerminalView>>,
    active: usize,
    started: bool,
    next_number: usize,
    focus: FocusHandle,
    subscriptions: HashMap<EntityId, Subscription>,
}

impl EventEmitter<GroupEvent> for TerminalGroup {}

impl Focusable for TerminalGroup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.terminals.get(self.active) {
            Some(terminal) => terminal.read(cx).focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

impl TerminalGroup {
    pub fn new(
        app: Entity<AppModel>,
        session: String,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::make(Some(app), None, session, surface, cx)
    }

    /// A group whose shells come from `maker` (stories, tests).
    pub fn with_maker(maker: PtyMaker, surface: Surface, cx: &mut Context<Self>) -> Self {
        Self::make(None, Some(maker), String::new(), surface, cx)
    }

    fn make(
        app: Option<Entity<AppModel>>,
        maker: Option<PtyMaker>,
        session: String,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            app,
            maker,
            session,
            surface,
            terminals: Vec::new(),
            active: 0,
            started: false,
            next_number: 1,
            focus: cx.focus_handle(),
            subscriptions: HashMap::new(),
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn surface(&self) -> Surface {
        self.surface
    }

    pub fn terminals(&self) -> &[Entity<TerminalView>] {
        &self.terminals
    }

    pub fn active(&self) -> Option<&Entity<TerminalView>> {
        self.terminals.get(self.active)
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The first time the group is shown: one terminal.
    pub fn ensure_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.started {
            self.started = true;
            self.new_terminal(window, cx);
        }
    }

    /// A new terminal, made the active one.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.started = true;
        if self.terminals.len() >= MAX_TERMINALS {
            cx.emit(GroupEvent::Notice(
                ToastSpec::new(crate::design::toast::Kind::Info, "Too many terminals")
                    .description(format!("A thread has at most {MAX_TERMINALS} here.")),
            ));
            return;
        }
        let number = self.next_number;
        self.next_number += 1;
        // The size of the terminal shown now, until the new one knows its own.
        let grid = self
            .active()
            .and_then(|t| t.read(cx).layout())
            .map_or((80, 24), |l| (l.cols as u16, l.rows as u16));
        let terminal = match (&self.app, &self.maker) {
            (Some(app), _) => {
                let (app, session) = (app.clone(), self.session.clone());
                cx.new(|cx| TerminalView::spawn(app, session, number, grid, window, cx))
            }
            (None, Some(maker)) => {
                let pty = maker(grid.0, grid.1);
                cx.new(|cx| TerminalView::with_pty(pty, number, grid, window, cx))
            }
            (None, None) => return,
        };
        let subscription = cx.subscribe_in(&terminal, window, Self::on_terminal_event);
        self.subscriptions
            .insert(terminal.entity_id(), subscription);
        self.terminals.push(terminal.clone());
        self.active = self.terminals.len() - 1;
        window.focus(&terminal.read(cx).focus_handle(cx), cx);
        cx.emit(GroupEvent::Changed);
        cx.notify();
    }

    fn on_terminal_event(
        &mut self,
        terminal: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::Title => {
                cx.emit(GroupEvent::Changed);
                cx.notify();
            }
            TerminalEvent::Exited => self.remove(terminal.entity_id(), window, cx),
            TerminalEvent::NewRequested => self.new_terminal(window, cx),
            TerminalEvent::CloseRequested => self.close(terminal.entity_id(), window, cx),
            TerminalEvent::Notice(spec) => cx.emit(GroupEvent::Notice(spec.clone())),
        }
    }

    /// Makes terminal `ix` the shown one, with the focus.
    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.get(ix) {
            self.active = ix;
            window.focus(&terminal.read(cx).focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// Gives the focus to the shown terminal.
    pub fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(terminal) = self.active() {
            window.focus(&terminal.read(cx).focus_handle(cx), cx);
        }
    }

    /// Closes the terminal `id`: its shell and everything it started end.
    pub fn close(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.iter().find(|t| t.entity_id() == id) {
            terminal.update(cx, |t, cx| t.close(cx));
        }
        self.remove(id, window, cx);
    }

    /// Closes the shown terminal.
    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active().map(|t| t.entity_id()) {
            self.close(id, window, cx);
        }
    }

    fn remove(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.terminals.iter().position(|t| t.entity_id() == id) else {
            return;
        };
        let had_focus = self.terminals[ix]
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        self.terminals.remove(ix);
        self.subscriptions.remove(&id);
        if self.active >= self.terminals.len() {
            self.active = self.terminals.len().saturating_sub(1);
        } else if ix < self.active {
            self.active -= 1;
        }
        if had_focus {
            match self.active() {
                Some(next) => window.focus(&next.read(cx).focus_handle(cx), cx),
                None => window.focus(&self.focus, cx),
            }
        }
        cx.emit(GroupEvent::Changed);
        cx.notify();
    }

    /// Ends every terminal of the group (the thread went away, or the app quits).
    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        for terminal in self.terminals.drain(..) {
            terminal.update(cx, |t, cx| t.close(cx));
        }
        self.subscriptions.clear();
        self.active = 0;
        cx.notify();
    }

    /// The active terminal's label (the right panel's tab shows it).
    pub fn label(&self, cx: &App) -> Option<String> {
        self.active().map(|t| t.read(cx).label())
    }

    fn tool_button(
        &self,
        id: &'static str,
        name: IconName,
        tooltip: &'static str,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = cx.design_theme().palette;
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .px(px(4.))
            .cursor_pointer()
            .text_color(palette.foreground.with_alpha(230).hsla())
            .hover(move |s| s.bg(palette.accent.hsla()))
            .tooltip(move |window, cx| {
                crate::design::tooltip::view(tooltip.into(), None, window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(name).size(px(13.)))
            .probe()
            .into_any_element()
    }

    /// The floating toolbar of a single terminal (t3code's pill at the top right).
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let divider = || {
            div()
                .w(px(1.))
                .h(px(16.))
                .bg(palette.border.with_alpha(204).hsla())
        };
        div()
            .id("terminal-toolbar")
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .flex()
            .items_center()
            .h(px(22.))
            .overflow_hidden()
            .rounded(radius::MD)
            .border_1()
            .border_color(palette.border.with_alpha(204).hsla())
            .bg(palette.background.hsla())
            .child(self.tool_button(
                "terminal-new",
                IconName::Plus,
                "New terminal (Ctrl+Shift+T)",
                |this, window, cx| this.new_terminal(window, cx),
                cx,
            ))
            .child(divider())
            .child(self.tool_button(
                "terminal-close",
                IconName::Trash,
                "Close terminal (Ctrl+Shift+W)",
                |this, window, cx| this.close_active(window, cx),
                cx,
            ))
            .probe()
            .into_any_element()
    }

    /// t3code's terminal list (several terminals).
    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let header = div()
            .id("terminal-list-header")
            .flex()
            .justify_end()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .border_b_1()
            .border_color(palette.border.with_alpha(179).hsla())
            .child(self.tool_button(
                "terminal-new",
                IconName::Plus,
                "New terminal (Ctrl+Shift+T)",
                |this, window, cx| this.new_terminal(window, cx),
                cx,
            ))
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .bg(palette.border.with_alpha(179).hsla()),
            )
            .child(self.tool_button(
                "terminal-close",
                IconName::Trash,
                "Close terminal (Ctrl+Shift+W)",
                |this, window, cx| this.close_active(window, cx),
                cx,
            ))
            .probe();
        let rows = self.terminals.iter().enumerate().map(|(ix, terminal)| {
            let active = ix == self.active;
            let label = terminal.read(cx).label();
            let id = terminal.entity_id();
            div()
                .id(SharedString::from(format!("terminal-row:{ix}")))
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(ROW_HEIGHT))
                .pl(px(6.))
                .pr(px(8.))
                .rounded(radius::MD)
                .cursor_pointer()
                .text_size(text::XS.size)
                .text_color(if active {
                    palette.foreground.hsla()
                } else {
                    palette.muted_foreground.hsla()
                })
                .when(active, |d| d.bg(palette.accent.hsla()))
                .hover(move |s| s.bg(palette.accent.with_alpha(179).hsla()))
                .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
                .child(icon(IconName::SquareTerminal).size(px(12.)).flex_shrink_0())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("terminal-row-close:{ix}")))
                        .flex_shrink_0()
                        .rounded(radius::SM)
                        .text_color(palette.muted_foreground.hsla())
                        .hover(move |s| s.text_color(palette.foreground.hsla()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close(id, window, cx);
                        }))
                        .child(icon(IconName::Close).size(px(10.)))
                        .probe(),
                )
                .probe()
        });
        div()
            .id("terminal-list")
            .flex()
            .flex_col()
            .w(px(LIST_WIDTH))
            .flex_shrink_0()
            .h_full()
            .border_1()
            .border_color(palette.border.with_alpha(179).hsla())
            .bg(palette.muted.with_alpha(26).hsla())
            .child(header)
            .child(
                div()
                    .id("terminal-list-rows")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .p(px(4.))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .probe()
            .into_any_element()
    }
}

impl Render for TerminalGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let surface = self.surface.id();
        let root = div()
            .id(SharedString::from(format!("terminal-group-{surface}")))
            .track_focus(&self.focus)
            .relative()
            .flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(palette.terminal_background.hsla());
        let Some(active) = self.active().cloned() else {
            return root
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(16.))
                .bg(palette.background.hsla())
                .text_size(text::SM.size)
                .text_color(palette.muted_foreground.hsla())
                .child(
                    div()
                        .id("terminal-empty")
                        .child("No terminal sessions for this thread yet.")
                        .probe(),
                )
                .child(
                    Button::new("terminal-empty-new", "New terminal")
                        .variant(Variant::Outline)
                        .size(button::Size::Xs)
                        .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
                )
                .probe();
        };
        let several = self.terminals.len() > 1;
        root.child(
            div()
                .id("terminal-area")
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(active)
                .when(!several, |d| d.child(self.toolbar(cx)))
                .probe(),
        )
        .when(several, |d| d.gap(px(6.)).child(self.list(cx)))
        .probe()
    }
}

/// Every thread's terminal groups of one surface: made on first use, kept until the thread's
/// terminals are closed or the app quits.
pub struct TerminalHub {
    app: Entity<AppModel>,
    surface: Surface,
    groups: HashMap<String, Entity<TerminalGroup>>,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<GroupEvent> for TerminalHub {}

impl TerminalHub {
    pub fn new(app: Entity<AppModel>, surface: Surface) -> Self {
        Self {
            app,
            surface,
            groups: HashMap::new(),
            subscriptions: Vec::new(),
        }
    }

    /// Thread `session`'s group (made now if it has none).
    pub fn group(&mut self, session: &str, cx: &mut Context<Self>) -> Entity<TerminalGroup> {
        if let Some(group) = self.groups.get(session) {
            return group.clone();
        }
        let (app, surface) = (self.app.clone(), self.surface);
        let group = cx.new(|cx| TerminalGroup::new(app, session.to_string(), surface, cx));
        self.subscriptions
            .push(cx.subscribe(&group, |_, _, event: &GroupEvent, cx| {
                cx.emit(event.clone())
            }));
        self.groups.insert(session.to_string(), group.clone());
        group
    }

    /// Thread `session`'s group, if it has one.
    pub fn existing(&self, session: &str) -> Option<&Entity<TerminalGroup>> {
        self.groups.get(session)
    }

    /// Every group (tests).
    pub fn groups(&self) -> impl Iterator<Item = (&String, &Entity<TerminalGroup>)> {
        self.groups.iter()
    }

    /// Ends every terminal of every thread.
    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        for group in self.groups.values() {
            group.update(cx, |g, cx| g.close_all(cx));
        }
    }
}
