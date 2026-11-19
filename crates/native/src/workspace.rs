//! The app frame: the window's regions and what they do. A sidebar at the left (resizable,
//! collapsible), a content column to its right with the title bar on top and below it the main
//! area, a right panel (resizable, closed by default) and a bottom drawer under the main area
//! (closed by default); the command palette and the toast stack float over everything. All the
//! sizes are remembered in `native-ui.json` ([`crate::ui_state`]).
//!
//! Sizes are decided in one pure function, [`frame_layout`]: from the window's size and the
//! remembered state it gives each region's size, so the rules (the sidebar between 208 and 640
//! px, the right panel between 280 and 720, the middle never under 320, the right panel hiding
//! itself when the window is too narrow for it) are tested without a window. The view only draws
//! what it says. The borders are dragged with our own thin handles (GPUI's `on_drag`): each
//! handle moves *one* size and nothing else, so dragging one border past its limit can never
//! shift space onto another pane. That is the behaviour GPUI Kit's `h_resizable` lacked
//! (`docs/PITFALLS.md`, Native UI), and `dragging_a_border_past_its_limit_*` below is the CP0.1
//! test again for this frame.
//!
//! The sidebar is the real one since Phase 3 ([`crate::sidebar`], over the app model
//! [`crate::model::AppModel`]); the main area shows the empty states of the sidebar's world (no
//! project yet, no open thread) or a placeholder for the open thread (the thread view is Phase 4),
//! the panel and the drawer are placeholders for Phase 6.
//!
//! t3code: `apps/web/src/components/AppSidebarLayout.tsx` (the frame, the sidebar width limits:
//! `threadSidebarWidth.ts`), `components/ui/sidebar.tsx` (`Sidebar`, `SidebarRail`: the drag
//! handle), `RightPanelTabs.tsx` / `rightPanelLayout.ts` (the right panel), `ThreadTerminalDrawer.tsx`
//! (the drawer), `shortcuts/*` (what the toggles are bound to). Reference: `app-empty-dark.png`
//! (the frame: a 254 px sidebar with its toggle, brand, search row and thread rows, the topbar
//! across the rest), `app-sidebar_collapsed.png` (the sidebar collapsed: the toggle moves to the
//! topbar), `app-terminal-open.png` (the drawer), `sidebar-right.png` (the right panel).

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::{
    Action, AnyElement, App, AppContext as _, Bounds, Context, DragMoveEvent, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    Render, Size, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    WindowBounds, assets::IconName, div, point, prelude::FluentBuilder as _, px, size,
};

use crate::{
    design::{
        EmptyState, Panel, Toasts,
        button::{self, Variant, icon_button},
        panel::Edge,
        toast::{self, ToastId, ToastSpec},
        tokens::{Appearance, text},
    },
    dev::ProbeExt as _,
    keymap::{self, *},
    model::{AppModel, ModelEvent, Selection, fixture},
    palette::{self, Item, Kind, Mode, Palette, PaletteEvent},
    sidebar::{Sidebar, SidebarEvent, data::Snapshot, empty},
    theme::{self, DesignTheme as _, Themes},
    titlebar::{self, TitleBar},
    ui_state::{self, MIN_WINDOW, UiState},
};

/// The middle never gets narrower than this (docs/PITFALLS.md: the CP0.1 report).
pub const MAIN_MIN_WIDTH: f32 = 320.;
/// The main area never gets lower than this when the drawer is open.
pub const MAIN_MIN_HEIGHT: f32 = 160.;
/// How wide the invisible band that grabs a border is.
pub const HANDLE: f32 = 6.;

/// The sizes of the regions for one window size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameLayout {
    /// The sidebar's width, 0 when collapsed.
    pub sidebar: f32,
    /// The right panel's width, 0 when closed or when the window is too narrow for it.
    pub right: f32,
    /// The main area's width.
    pub main: f32,
    /// The drawer's height, 0 when closed or when the window is too low for it.
    pub drawer: f32,
}

/// Decides the regions' sizes for a window of `window` size (logical pixels) from `state`.
///
/// - The sidebar keeps its width, clamped to its limits and to what leaves the middle its minimum.
/// - The right panel takes what is left over the middle minimum; if even its minimum does not fit,
///   the sidebar gives up width down to its own minimum first; if it still does not fit, the panel
///   is not shown (its state is kept: it comes back when the window is wide enough).
/// - The drawer is clamped so the main area keeps [`MAIN_MIN_HEIGHT`], and is not shown below
///   its minimum.
pub fn frame_layout(window: Size<Pixels>, state: &UiState) -> FrameLayout {
    let (w, h) = (window.width.as_f32(), window.height.as_f32());
    let mut sidebar = if state.sidebar.open {
        state
            .sidebar
            .size
            .clamp(ui_state::sidebar::MIN, ui_state::sidebar::MAX)
            .min((w - MAIN_MIN_WIDTH).max(ui_state::sidebar::MIN))
    } else {
        0.
    };
    let mut right = 0.;
    if state.right_panel.open {
        // The sidebar gives up width, but only if that is what makes the panel fit.
        let squeezed = w - MAIN_MIN_WIDTH - ui_state::right_panel::MIN;
        if w - sidebar - MAIN_MIN_WIDTH < ui_state::right_panel::MIN
            && squeezed >= ui_state::sidebar::MIN
        {
            sidebar = squeezed.min(sidebar);
        }
        let room = w - sidebar - MAIN_MIN_WIDTH;
        if room >= ui_state::right_panel::MIN {
            right = state
                .right_panel
                .size
                .clamp(ui_state::right_panel::MIN, ui_state::right_panel::MAX)
                .min(room);
        }
    }
    let mut drawer = 0.;
    if state.drawer.open {
        let room = h - titlebar::HEIGHT - MAIN_MIN_HEIGHT;
        if room >= ui_state::drawer::MIN {
            drawer = state
                .drawer
                .size
                .clamp(ui_state::drawer::MIN, ui_state::drawer::MAX)
                .min(room);
        }
    }
    FrameLayout {
        sidebar,
        right,
        main: w - sidebar - right,
        drawer,
    }
}

/// The window bounds to open with: the saved ones when they are still on a screen, else the
/// saved size centred on the first (primary) display.
pub fn restore_window(saved: &ui_state::WindowState, displays: &[Bounds<Pixels>]) -> WindowBounds {
    let window_size = size(px(saved.width), px(saved.height));
    let centred = || {
        let screen = displays
            .first()
            .copied()
            .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))));
        let width = window_size.width.min(screen.size.width);
        let height = window_size.height.min(screen.size.height);
        Bounds::new(
            point(
                screen.origin.x + (screen.size.width - width) / 2.,
                screen.origin.y + (screen.size.height - height) / 2.,
            ),
            size(width, height),
        )
    };
    let bounds = match (saved.x, saved.y) {
        (Some(x), Some(y)) => {
            let origin = point(px(x), px(y));
            // At least a 120 x 40 piece of the title bar must be on some screen, or the window
            // could not be grabbed (a monitor that was unplugged).
            let grab = Bounds::new(origin, size(px(120.), px(40.)));
            match displays.iter().find(|d| d.intersect(&grab) == grab) {
                // No bigger than that screen (a size saved on a larger monitor).
                Some(screen) => Bounds::new(
                    origin,
                    size(
                        window_size.width.min(screen.size.width),
                        window_size.height.min(screen.size.height),
                    ),
                ),
                None => centred(),
            }
        }
        _ => centred(),
    };
    if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// Which border a drag moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The sidebar's right edge.
    Sidebar,
    /// The right panel's left edge.
    Right,
    /// The drawer's top edge.
    Drawer,
}

/// The payload of a border drag.
#[derive(Clone, Copy)]
struct ResizeDrag(Pane);

/// GPUI wants a view to show while dragging; the border itself is the feedback.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// The app frame view.
pub struct Workspace {
    state: UiState,
    data_dir: Option<PathBuf>,
    maximized: bool,
    toasts: Toasts,
    started: Instant,
    toast_timer: Option<Task<()>>,
    palette: Option<Entity<Palette>>,
    palette_subscription: Option<Subscription>,
    focus: FocusHandle,
    save_task: Option<Task<()>>,
    model: Entity<AppModel>,
    sidebar: Entity<Sidebar>,
    /// The thread a toast's action opens.
    toast_threads: HashMap<ToastId, String>,
    _bounds: Subscription,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// The frame over fake projects and threads ([`fixture`]): the gallery, the snapshots and the
    /// tests. `state` is what was remembered; `data_dir` is where `native-ui.json` is written
    /// (`None`: not saved).
    pub fn new(
        state: UiState,
        data_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = cx.new(AppModel::fixture);
        Self::with_model(state, data_dir, model, window, cx)
    }

    /// The frame over `model` (the real core's, from the app).
    pub fn with_model(
        state: UiState,
        data_dir: Option<PathBuf>,
        model: Entity<AppModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.window_moved(window, cx);
        });
        let sidebar = cx.new(|cx| Sidebar::new(model.clone(), state.sidebar_list.clone(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.subscribe(&model, Self::on_model_event),
            // The palette's list and the title follow the model.
            cx.observe(&model, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            data_dir,
            maximized: window.is_maximized(),
            toasts: Toasts::new(),
            started: Instant::now(),
            toast_timer: None,
            palette: None,
            palette_subscription: None,
            focus,
            save_task: None,
            model,
            sidebar,
            toast_threads: HashMap::new(),
            _bounds: bounds,
            _subscriptions: subscriptions,
        }
    }

    /// The app model (projects, threads, the open one).
    pub fn model(&self) -> &Entity<AppModel> {
        &self.model
    }

    /// The sidebar view.
    pub fn sidebar_view(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::ToggleSidebar => self.toggle_sidebar(cx),
            SidebarEvent::OpenPalette => self.open_palette(Mode::All, window, cx),
            SidebarEvent::OpenSettings => self.run_command("settings.open", window, cx),
            SidebarEvent::OpenUsage => {
                self.push_toast(
                    ToastSpec::new(toast::Kind::Info, "Usage")
                        .description("The usage screen arrives in Phase 7."),
                    cx,
                );
            }
            SidebarEvent::ToggleTheme => self.toggle_theme(cx),
            SidebarEvent::PrefsChanged(prefs) => {
                self.state.sidebar_list = prefs.clone();
                self.changed(cx);
            }
        }
    }

    fn on_model_event(&mut self, _: Entity<AppModel>, event: &ModelEvent, cx: &mut Context<Self>) {
        match event {
            ModelEvent::Toast { spec, open } => {
                let id = self.push_toast(spec.clone(), cx);
                if let Some(thread) = open {
                    self.toast_threads.insert(id, thread.clone());
                }
            }
        }
    }

    /// The remembered state (what the next save writes).
    pub fn state(&self) -> &UiState {
        &self.state
    }

    /// The toasts, for tests.
    pub fn toasts(&self) -> &Toasts {
        &self.toasts
    }

    /// Whether the palette is open.
    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// The palette entity while open.
    pub fn palette(&self) -> Option<&Entity<Palette>> {
        self.palette.as_ref()
    }

    /// Focuses the frame (so its shortcuts work).
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn window_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.maximized = window.is_maximized();
        match window.window_bounds() {
            WindowBounds::Windowed(b) => {
                self.state.window = ui_state::WindowState {
                    x: Some(b.origin.x.as_f32()),
                    y: Some(b.origin.y.as_f32()),
                    width: b.size.width.as_f32(),
                    height: b.size.height.as_f32(),
                    maximized: false,
                };
            }
            WindowBounds::Maximized(b) => {
                // The restore bounds are kept, so un-maximising next time goes back to them.
                self.state.window.maximized = true;
                self.state.window.x = Some(b.origin.x.as_f32());
                self.state.window.y = Some(b.origin.y.as_f32());
                self.state.window.width = b.size.width.as_f32();
                self.state.window.height = b.size.height.as_f32();
            }
            WindowBounds::Fullscreen(_) => {}
        }
        self.state = std::mem::take(&mut self.state).sanitized();
        self.changed(cx);
    }

    /// Something remembered changed: redraw and write the file after a pause.
    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(dir) = self.data_dir.clone() else {
            return;
        };
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ui_state::SAVE_AFTER).await;
            let Ok(state) = this.read_with(cx, |this, _| this.state.clone()) else {
                return;
            };
            // The file is written off the UI thread (ground rule 7).
            let written = cx.background_spawn(async move { state.save(&dir) }).await;
            if let Err(error) = written {
                tracing::warn!("could not write native-ui.json: {error}");
            }
        }));
    }

    /// Writes the file now (at quit).
    pub fn save_now(&self) {
        if let Some(dir) = &self.data_dir
            && let Err(error) = self.state.save(dir)
        {
            tracing::warn!("could not write native-ui.json: {error}");
        }
    }

    /// Collapses or opens the sidebar.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.state.sidebar.open = !self.state.sidebar.open;
        self.changed(cx);
    }

    /// Opens or closes the right panel.
    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.state.right_panel.open = !self.state.right_panel.open;
        self.changed(cx);
    }

    /// Opens or closes the bottom drawer.
    pub fn toggle_drawer(&mut self, cx: &mut Context<Self>) {
        self.state.drawer.open = !self.state.drawer.open;
        self.changed(cx);
    }

    /// Moves a border to the pointer at `pointer` (window coordinates) in a window of `window`
    /// size: sets that one size, within its limits, and nothing else.
    pub fn drag_border(
        &mut self,
        pane: Pane,
        pointer: gpui_kit::Point<Pixels>,
        window: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (w, h) = (window.width.as_f32(), window.height.as_f32());
        let frame = frame_layout(window, &self.state);
        match pane {
            Pane::Sidebar => {
                let room = w - frame.right - MAIN_MIN_WIDTH;
                self.state.sidebar.size = pointer.x.as_f32().clamp(
                    ui_state::sidebar::MIN,
                    ui_state::sidebar::MAX.min(room.max(ui_state::sidebar::MIN)),
                );
            }
            Pane::Right => {
                let room = w - frame.sidebar - MAIN_MIN_WIDTH;
                self.state.right_panel.size = (w - pointer.x.as_f32()).clamp(
                    ui_state::right_panel::MIN,
                    ui_state::right_panel::MAX.min(room.max(ui_state::right_panel::MIN)),
                );
            }
            Pane::Drawer => {
                let room = h - titlebar::HEIGHT - MAIN_MIN_HEIGHT;
                self.state.drawer.size = (h - pointer.y.as_f32()).clamp(
                    ui_state::drawer::MIN,
                    ui_state::drawer::MAX.min(room.max(ui_state::drawer::MIN)),
                );
            }
        }
        self.changed(cx);
    }

    /// Puts a size back to its default (a double click on its border).
    pub fn reset_border(&mut self, pane: Pane, cx: &mut Context<Self>) {
        match pane {
            Pane::Sidebar => self.state.sidebar.size = ui_state::sidebar::DEFAULT,
            Pane::Right => self.state.right_panel.size = ui_state::right_panel::DEFAULT,
            Pane::Drawer => self.state.drawer.size = ui_state::drawer::DEFAULT,
        }
        self.changed(cx);
    }

    // --- toasts ---------------------------------------------------------------------------

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    /// Shows a notice.
    pub fn push_toast(&mut self, spec: ToastSpec, cx: &mut Context<Self>) -> ToastId {
        let now = self.now();
        let id = self.toasts.push(spec, now);
        self.arm_toast_timer(cx);
        cx.notify();
        id
    }

    /// A toast's action button: opens the thread the notice is about, and the notice goes.
    fn run_toast_action(&mut self, id: ToastId, cx: &mut Context<Self>) {
        if let Some(thread) = self.toast_threads.remove(&id) {
            self.model.update(cx, |m, cx| m.open(&thread, cx));
        }
        self.dismiss_toast(id, cx);
    }

    /// The title bar's text: the open thread's project and name (t3code's breadcrumb), the new
    /// thread's project, or "No active thread".
    fn topbar_title(&self, cx: &App) -> String {
        let model = self.model.read(cx);
        let snapshot = model.snapshot();
        match model.selected() {
            Selection::Thread(id) => match snapshot.thread(id) {
                Some(t) => format!(
                    "{}  /  {}",
                    snapshot.project(&t.project).map_or("", |p| p.name.as_str()),
                    t.display_title()
                ),
                None => "No active thread".to_string(),
            },
            Selection::Draft(project) => format!(
                "{}  /  New thread",
                snapshot.project(project).map_or("", |p| p.name.as_str())
            ),
            Selection::None => "No active thread".to_string(),
        }
    }

    fn dismiss_toast(&mut self, id: ToastId, cx: &mut Context<Self>) {
        self.toast_threads.remove(&id);
        self.toasts.dismiss(id);
        self.arm_toast_timer(cx);
        cx.notify();
    }

    /// Removes the toasts due at clock reading `now` (the timer's work; tests call it directly).
    pub fn tick_toasts(&mut self, now: Duration, cx: &mut Context<Self>) {
        if !self.toasts.expire(now).is_empty() {
            cx.notify();
        }
        self.arm_toast_timer(cx);
    }

    /// Sleeps until the next toast is due, then expires it.
    fn arm_toast_timer(&mut self, cx: &mut Context<Self>) {
        self.toast_timer = None;
        let Some(deadline) = self.toasts.next_deadline() else {
            return;
        };
        let wait = deadline.saturating_sub(self.now());
        self.toast_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                let now = this.now();
                this.tick_toasts(now, cx);
            });
        }));
    }

    // --- palette --------------------------------------------------------------------------

    /// The palette's entries for fake data (the gallery's story): every action, the fixture's
    /// threads and projects.
    pub fn palette_items() -> Vec<Item> {
        Self::palette_items_for(&fixture::snapshot())
    }

    /// The palette's entries: every action, then the threads (newest first) and the projects of
    /// `snapshot`.
    pub fn palette_items_for(snapshot: &Snapshot) -> Vec<Item> {
        let mut items = palette::action_items();
        let mut threads: Vec<_> = snapshot.threads.iter().collect();
        threads.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        for thread in threads {
            let project = snapshot
                .project(&thread.project)
                .map_or("", |p| p.name.as_str());
            items.push(Item {
                kind: Kind::Thread,
                id: thread.id.clone(),
                title: thread.display_title().to_string(),
                subtitle: Some(match &thread.branch {
                    Some(branch) => format!("{project} · {branch}"),
                    None => project.to_string(),
                }),
                shortcut: None,
            });
        }
        for project in snapshot
            .projects
            .iter()
            .filter(|p| p.kind != crate::sidebar::data::ProjectKind::None)
        {
            items.push(Item {
                kind: Kind::Project,
                id: project.id.clone(),
                title: project.name.clone(),
                subtitle: project.path.clone(),
                shortcut: None,
            });
        }
        items
    }

    /// Opens the palette in `mode`, or closes it when it is already open in that mode.
    pub fn open_palette(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        let items = Self::palette_items_for(self.model.read(cx).snapshot());
        let palette = cx.new(|cx| Palette::new(mode, items, window, cx));
        self.palette_subscription = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Dismiss => this.close_palette(window, cx),
                PaletteEvent::Run(item) => {
                    let item = item.clone();
                    this.close_palette(window, cx);
                    this.run_item(&item, window, cx);
                }
            },
        ));
        palette.update(cx, |p, cx| p.focus_input(window, cx));
        self.palette = Some(palette);
        cx.notify();
    }

    /// Closes the palette and gives the focus back to the frame.
    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.palette_subscription = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Does what a palette entry means.
    pub fn run_item(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        match item.kind {
            Kind::Action => self.run_command(&item.id, window, cx),
            Kind::Thread => {
                self.model.update(cx, |m, cx| m.open(&item.id, cx));
            }
            // A project: a new thread in it (what t3code's palette does).
            Kind::Project => {
                self.model.update(cx, |m, cx| m.new_thread(&item.id, cx));
            }
        }
    }

    /// Runs the command called `id` (a name from [`keymap::COMMANDS`]).
    pub fn run_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "commandPalette.toggle" => self.open_palette(Mode::All, window, cx),
            "commandPalette.actions" => self.open_palette(Mode::Actions, window, cx),
            "sidebar.toggle" => self.toggle_sidebar(cx),
            "rightPanel.toggle" => self.toggle_right_panel(cx),
            "terminal.toggle" => self.toggle_drawer(cx),
            "appearance.cycle" => self.cycle_theme(cx),
            "workspace.focusNext" => window.focus_next(cx),
            "workspace.focusPrevious" => window.focus_prev(cx),
            "chat.new" => self.sidebar.update(cx, |s, cx| s.new_thread_here(cx)),
            "settings.open" => {
                self.push_toast(
                    ToastSpec::new(toast::Kind::Info, "Settings")
                        .description("The settings screens arrive in Phase 7."),
                    cx,
                );
            }
            other => tracing::warn!("no handler for command {other}"),
        }
    }

    /// The sidebar's theme button: t3code's light theme when the dark one is worn, and back (the
    /// choice is remembered; `Ctrl+Alt+Shift+A` still cycles through every theme).
    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let themes = Themes::global(cx);
        let to = if cx.design_theme().appearance.is_dark() {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        let def = themes.default_for(to).clone();
        theme::apply(&def, cx);
        self.state.theme = Some(def.name);
        self.changed(cx);
    }

    /// Switches to the next theme of the list (built in, bundled, the user's folder).
    pub fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        let themes = Themes::global(cx);
        let names: Vec<String> = themes.all().iter().map(|t| t.name.clone()).collect();
        let current = cx.design_theme().name.clone();
        let next = names
            .iter()
            .position(|n| *n == current)
            .map_or(0, |ix| (ix + 1) % names.len());
        if let Some(def) = themes.find(&names[next]).cloned() {
            theme::apply(&def, cx);
            self.state.theme = Some(def.name);
            self.changed(cx);
        }
    }

    // --- drawing --------------------------------------------------------------------------

    fn panel_toggles(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.state;
        div()
            .id("panel-toggles")
            .flex()
            .items_center()
            .gap(px(4.))
            .pr(px(8.))
            .child(
                icon_button(
                    "toggle-drawer",
                    IconName::PanelBottom,
                    "Toggle terminal drawer (Ctrl+J)",
                )
                .variant(Variant::Ghost)
                .size(button::Size::Sm)
                .selected(s.drawer.open)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_drawer(cx))),
            )
            .child(
                icon_button(
                    "toggle-right",
                    IconName::PanelRight,
                    "Toggle right panel (Ctrl+Alt+B)",
                )
                .variant(Variant::Ghost)
                .size(button::Size::Sm)
                .selected(s.right_panel.open)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_right_panel(cx))),
            )
            .probe()
            .into_any_element()
    }

    fn sidebar_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        icon_button(
            "toggle-sidebar",
            IconName::PanelLeft,
            "Toggle sidebar (Ctrl+B)",
        )
        .variant(Variant::Ghost)
        .size(button::Size::Sm)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
        .into_any_element()
    }

    fn sidebar(&self, width: f32) -> AnyElement {
        div()
            .id("sidebar-slot")
            .flex_shrink_0()
            .w(px(width))
            .h_full()
            .child(self.sidebar.clone())
            .into_any_element()
    }

    /// What the main area is about: the sidebar's empty states, or (until Phase 4) a placeholder
    /// for the thread that is open.
    fn main_area(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let model = self.model.read(cx);
        let snapshot = model.snapshot();
        let starting = matches!(model.phase(), crate::model::Phase::Starting);
        let body: AnyElement = match model.selected() {
            Selection::Thread(id) => {
                let thread = snapshot.thread(id);
                EmptyState::new(
                    "empty",
                    thread.map_or("Thread".to_string(), |t| t.display_title().to_string()),
                )
                .icon(IconName::MessageSquare)
                .description("The thread view arrives in Phase 4.")
                .size(crate::design::empty_state::Size::Hero)
                .into_any_element()
            }
            Selection::Draft(project) => {
                let name = snapshot
                    .project(project)
                    .map_or("this project".to_string(), |p| p.name.clone());
                EmptyState::new("empty", format!("What should we build in {name}?"))
                    .icon(IconName::MessageSquare)
                    .description("The composer arrives in Phase 5.")
                    .size(crate::design::empty_state::Size::Hero)
                    .into_any_element()
            }
            Selection::None if starting => div().into_any_element(),
            Selection::None if matches!(model.phase(), crate::model::Phase::Failed(_)) => {
                let why = match model.phase() {
                    crate::model::Phase::Failed(why) => why.clone(),
                    _ => String::new(),
                };
                EmptyState::new("open-failed", "Hermes could not open its data")
                    .icon(IconName::CircleAlert)
                    .description(why)
                    .into_any_element()
            }
            Selection::None if snapshot.is_empty() => {
                let add = self.sidebar.clone();
                let scratch = self.sidebar.clone();
                empty::no_projects_hero(
                    move |_, cx| add.update(cx, |s, cx| s.add_project_dialog(cx)),
                    move |_, cx| {
                        scratch.update(cx, |s, cx| {
                            s.new_thread(crate::sidebar::data::NO_PROJECT, cx)
                        })
                    },
                )
                .into_any_element()
            }
            Selection::None => empty::no_active_thread().into_any_element(),
        };
        div()
            .id("main")
            .flex()
            .flex_1()
            .min_w(px(MAIN_MIN_WIDTH))
            .min_h_0()
            .overflow_hidden()
            .bg(palette.background.hsla())
            .child(body)
            .probe()
            .into_any_element()
    }

    fn handle(
        &self,
        id: &'static str,
        pane: Pane,
        vertical: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let palette = cx.design_theme().palette;
        let line = palette.ring.with_alpha(160).hsla();
        div()
            .id(id)
            .absolute()
            .when(vertical, |d| d.cursor_row_resize())
            .when(!vertical, |d| d.cursor_col_resize())
            .hover(move |s| s.bg(line))
            .on_drag(ResizeDrag(pane), |_, _, _, cx| cx.new(|_| NoGhost))
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, _, cx| {
                    if event.click_count() == 2 {
                        this.reset_border(pane, cx);
                    }
                }),
            )
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let viewport = window.viewport_size();
        let frame = frame_layout(viewport, &self.state);
        let maximized = window.is_maximized();
        self.maximized = maximized;
        let (w, h) = (viewport.width.as_f32(), viewport.height.as_f32());
        let title = self.topbar_title(cx);

        let sidebar_toggle = (frame.sidebar == 0.).then(|| {
            div()
                .id("topbar-left")
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(10.))
                .child(self.sidebar_toggle(cx))
                .child(
                    div()
                        .id("brand")
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Hermes"),
                )
        });
        let topbar = TitleBar::new()
            .maximized(maximized)
            .title(title)
            .right(self.panel_toggles(cx))
            .when_some(sidebar_toggle, |bar, left| bar.left(left));

        let right = (frame.right > 0.).then(|| {
            div()
                .id("right-panel-slot")
                .flex_shrink_0()
                .w(px(frame.right))
                .h_full()
                .child(
                    Panel::pane("right-panel", Edge::Left)
                        .title("Right panel")
                        .size_full()
                        .child(
                            div()
                                .id("right-body")
                                .p(px(12.))
                                .text_size(text::SM.size)
                                .text_color(palette.muted_foreground.hsla())
                                .child("Diff, files and terminal tabs arrive in Phase 6."),
                        ),
                )
        });
        let drawer = (frame.drawer > 0.).then(|| {
            div()
                .id("drawer-slot")
                .flex_shrink_0()
                .h(px(frame.drawer))
                .w_full()
                .child(
                    Panel::pane("drawer", Edge::Top)
                        .title("Terminal")
                        .size_full()
                        .child(
                            div()
                                .id("drawer-body")
                                .p(px(12.))
                                .text_size(text::SM.size)
                                .text_color(palette.muted_foreground.hsla())
                                .child("The terminal arrives in Phase 6."),
                        ),
                )
        });

        let toasts = (!self.toasts.is_empty()).then(|| {
            let this = cx.entity();
            div()
                .id("toasts")
                .absolute()
                .right(px(toast::INSET))
                .bottom(px(toast::INSET))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(toast::GAP))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    let now = this.now();
                    if *hovered {
                        this.toasts.hold(now);
                    } else {
                        this.toasts.release(now);
                    }
                    this.arm_toast_timer(cx);
                }))
                .children(toast::stack_elements(
                    &self.toasts,
                    {
                        let this = this.clone();
                        move |id| {
                            let this = this.clone();
                            std::rc::Rc::new(move |_, _, cx| {
                                this.update(cx, |this, cx| this.dismiss_toast(id, cx));
                            })
                        }
                    },
                    move |id| {
                        let this = this.clone();
                        std::rc::Rc::new(move |_, _, cx| {
                            this.update(cx, |this, cx| this.run_toast_action(id, cx));
                        })
                    },
                ))
                .probe()
        });

        let palette_layer = self.palette.clone().map(|p| {
            div()
                .id("palette-layer")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px((h * 0.12).max(24.)))
                // t3code's `dialog-backdrop`: the background at 60% (64% dark); the blur behind
                // it is not reproduced.
                .bg(palette
                    .background
                    .with_alpha(if cx.design_theme().appearance.is_dark() {
                        163
                    } else {
                        153
                    })
                    .hsla())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .id("palette-box")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(p),
                )
        });

        let handle_edge = px(HANDLE / 2.);
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(palette.background.hsla())
            .text_color(palette.foreground.hsla())
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _, cx| this.toggle_right_panel(cx)))
            .on_action(cx.listener(|this, _: &ToggleDrawer, _, cx| this.toggle_drawer(cx)))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.open_palette(Mode::All, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenActionPalette, window, cx| {
                this.open_palette(Mode::Actions, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                this.run_command("chat.new", window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.run_command("settings.open", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CycleTheme, _, cx| this.cycle_theme(cx)))
            .on_action(cx.listener(|_, _: &FocusNextRegion, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPreviousRegion, window, cx| window.focus_prev(cx)))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeDrag>, window, cx| {
                    let pane = event.drag(cx).0;
                    this.drag_border(pane, event.event.position, window.viewport_size(), cx);
                }),
            )
            .when(frame.sidebar > 0., |d| d.child(self.sidebar(frame.sidebar)))
            .child(
                div()
                    .id("content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(topbar)
                    .child(
                        div()
                            .id("body")
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("main-column")
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.main_area(cx))
                                    .when_some(drawer, |d, drawer| d.child(drawer))
                                    .probe(),
                            )
                            .when_some(right, |d, right| d.child(right))
                            .probe(),
                    )
                    .probe(),
            )
            .when(frame.sidebar > 0., |d| {
                d.child(
                    // Below the 40 px top row: up there the sidebar header is a drag zone, and
                    // Windows' hit test answers "caption" before any element sees the press.
                    self.handle("sidebar-handle", Pane::Sidebar, false, cx)
                        .top(px(titlebar::HEIGHT))
                        .bottom_0()
                        .left(px(frame.sidebar) - handle_edge)
                        .w(px(HANDLE))
                        .probe(),
                )
            })
            .when(frame.right > 0., |d| {
                d.child(
                    self.handle("right-handle", Pane::Right, false, cx)
                        .top(px(titlebar::HEIGHT))
                        .bottom_0()
                        .left(px(w - frame.right) - handle_edge)
                        .w(px(HANDLE))
                        .probe(),
                )
            })
            .when(frame.drawer > 0., |d| {
                d.child(
                    self.handle("drawer-handle", Pane::Drawer, true, cx)
                        .left(px(frame.sidebar))
                        .right(px(frame.right))
                        .top(px(h - frame.drawer) - handle_edge)
                        .h(px(HANDLE))
                        .probe(),
                )
            })
            .when_some(toasts, |d, t| d.child(t))
            .when_some(palette_layer, |d, p| d.child(p))
            .probe()
    }
}

/// The smallest window the frame is designed for.
pub fn min_window_size() -> Size<Pixels> {
    size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))
}

/// A one-shot helper for tests and the app: the action `id`'s command, boxed.
pub fn command_action(id: &str) -> Option<Box<dyn Action>> {
    keymap::find(id).map(|c| (c.make)())
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
