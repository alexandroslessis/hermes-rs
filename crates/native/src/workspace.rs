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
//! The content of the regions is placeholder (fake threads in the sidebar, an empty-state in the
//! main area): the sidebar is Phase 3, the thread view Phase 4, the panel Phase 6.
//!
//! t3code: `apps/web/src/components/AppSidebarLayout.tsx` (the frame, the sidebar width limits:
//! `threadSidebarWidth.ts`), `components/ui/sidebar.tsx` (`Sidebar`, `SidebarRail`: the drag
//! handle), `RightPanelTabs.tsx` / `rightPanelLayout.ts` (the right panel), `ThreadTerminalDrawer.tsx`
//! (the drawer), `shortcuts/*` (what the toggles are bound to). Reference: `app-empty-dark.png`
//! (the frame: a 254 px sidebar with its toggle, brand, search row and thread rows, the topbar
//! across the rest), `app-sidebar_collapsed.png` (the sidebar collapsed: the toggle moves to the
//! topbar), `app-terminal-open.png` (the drawer), `sidebar-right.png` (the right panel).

use std::{
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
        Button, EmptyState, ListRow, Panel, SectionHeader, ThreadRow, Toasts,
        button::{self, Variant, icon_button},
        list_row::Status,
        panel::Edge,
        toast::{self, ToastId, ToastSpec},
        tokens::text,
    },
    dev::ProbeExt as _,
    fake,
    keymap::{self, *},
    palette::{self, Item, Kind, Mode, Palette, PaletteEvent},
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
    selected_thread: usize,
    notice: usize,
    _bounds: Subscription,
}

impl Workspace {
    /// The frame with `state`; `data_dir` is where `native-ui.json` is written (`None`: not saved).
    pub fn new(
        state: UiState,
        data_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.window_moved(window, cx);
        });
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
            selected_thread: 0,
            notice: 0,
            _bounds: bounds,
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

    fn dismiss_toast(&mut self, id: ToastId, cx: &mut Context<Self>) {
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

    pub fn palette_items() -> Vec<Item> {
        let mut items = palette::action_items();
        for (id, title, project, branch) in fake::palette_threads() {
            items.push(Item {
                kind: Kind::Thread,
                id: id.to_string(),
                title: title.to_string(),
                subtitle: Some(format!("{project} · {branch}")),
                shortcut: None,
            });
        }
        for (path, name) in fake::palette_projects() {
            items.push(Item {
                kind: Kind::Project,
                id: path.to_string(),
                title: name.to_string(),
                subtitle: Some(path.to_string()),
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
        let palette = cx.new(|cx| Palette::new(mode, Self::palette_items(), window, cx));
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
                if let Some(ix) = fake::palette_threads().iter().position(|t| t.0 == item.id) {
                    self.selected_thread = ix;
                }
                self.push_toast(
                    ToastSpec::new(toast::Kind::Info, format!("Opened “{}”", item.title))
                        .description("The thread view arrives in Phase 4."),
                    cx,
                );
            }
            Kind::Project => {
                self.push_toast(
                    ToastSpec::new(toast::Kind::Info, format!("Project {}", item.title))
                        .description("Projects arrive with the sidebar, Phase 3."),
                    cx,
                );
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
            "chat.new" => {
                self.push_toast(
                    ToastSpec::new(toast::Kind::Info, "New thread")
                        .description("Threads arrive with the sidebar, Phase 3."),
                    cx,
                );
            }
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

    fn sidebar(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let threads = fake::palette_threads();
        let statuses = [None, Some(Status::Done), None, Some(Status::Working), None];
        let times = ["now", "", "20h", "3m", "1d"];
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(width))
            .h_full()
            .bg(palette.sidebar.hsla())
            .text_color(palette.sidebar_foreground.hsla())
            .border_r_1()
            .border_color(palette.sidebar_border.hsla())
            .child(
                div()
                    .id("sidebar-header")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(8.))
                    .h(px(titlebar::HEIGHT))
                    .px(px(10.))
                    .child(self.sidebar_toggle(cx))
                    .child(
                        div()
                            .id("brand")
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Hermes"),
                    )
                    .child(
                        div()
                            .id("sidebar-drag")
                            .flex_1()
                            .h_full()
                            .window_control_area(gpui_kit::WindowControlArea::Drag),
                    )
                    .probe(),
            )
            .child(
                div()
                    .id("sidebar-search")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(4.))
                    .px(px(8.))
                    .pb(px(8.))
                    .child(div().flex_1().min_w_0().child({
                        let this = cx.entity();
                        ListRow::new("search", "Search")
                            .icon(IconName::Search)
                            .meta("Ctrl+K")
                            .on_click(move |window, cx| {
                                this.update(cx, |t, cx| t.open_palette(Mode::All, window, cx));
                            })
                    }))
                    .child(
                        icon_button("new-thread", IconName::SquarePen, "New thread (Ctrl+N)")
                            .variant(Variant::GhostMuted)
                            .size(button::Size::Sm)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_command("chat.new", window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("threads")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(4.))
                    .px(px(8.))
                    .overflow_y_scroll()
                    .child(
                        SectionHeader::new("threads-header", "Threads")
                            .meta(threads.len().to_string()),
                    )
                    .children(threads.iter().enumerate().map(
                        |(ix, (_, title, project, branch))| {
                            let mut row = ThreadRow::new(("thread", ix), *project, *title)
                                .branch(*branch)
                                .selected(ix == self.selected_thread);
                            match statuses[ix % statuses.len()] {
                                Some(status) => row = row.status(status),
                                None if !times[ix % times.len()].is_empty() => {
                                    row = row.time(times[ix % times.len()])
                                }
                                None => {}
                            }
                            div()
                                .id(("thread-click", ix))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected_thread = ix;
                                    cx.notify();
                                }))
                                .child(row)
                        },
                    ))
                    .probe(),
            )
            .child(
                div()
                    .id("sidebar-footer")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(4.))
                    .h(px(titlebar::HEIGHT))
                    .px(px(10.))
                    .child(
                        icon_button("settings", IconName::Settings, "Settings (Ctrl+,)")
                            .variant(Variant::GhostMuted)
                            .size(button::Size::Sm)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_command("settings.open", window, cx)
                            })),
                    )
                    .child(
                        icon_button("theme", IconName::Palette, "Cycle theme (Ctrl+Alt+Shift+A)")
                            .variant(Variant::GhostMuted)
                            .size(button::Size::Sm)
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_theme(cx))),
                    )
                    .probe(),
            )
            .probe()
            .into_any_element()
    }

    fn main_area(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let thread = fake::palette_threads()[self.selected_thread];
        div()
            .id("main")
            .flex()
            .flex_1()
            .min_w(px(MAIN_MIN_WIDTH))
            .min_h_0()
            .overflow_hidden()
            .bg(palette.background.hsla())
            .child(
                EmptyState::new("empty", format!("What should we build in {}?", thread.2))
                    .icon(IconName::MessageSquare)
                    .description(
                        "The thread view arrives in Phase 4. Press Ctrl+K for the palette.",
                    )
                    .size(crate::design::empty_state::Size::Hero)
                    .child(
                        Button::new("open-palette", "Command palette")
                            .variant(Variant::Outline)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_palette(Mode::All, window, cx)
                            })),
                    )
                    .child(
                        Button::new("show-notice", "Show a notice")
                            .variant(Variant::Ghost)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let notices = fake::notices();
                                let spec = notices[this.notice % notices.len()].clone();
                                this.notice += 1;
                                this.push_toast(spec, cx);
                            })),
                    ),
            )
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
        let thread = fake::palette_threads()[self.selected_thread];

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
            .title(format!("{}  /  {}", thread.2, thread.1))
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
                .children(toast::stack_elements(&self.toasts, move |id| {
                    let this = this.clone();
                    std::rc::Rc::new(move |_, _, cx| {
                        this.update(cx, |this, cx| this.dismiss_toast(id, cx));
                    })
                }))
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
            .when(frame.sidebar > 0., |d| {
                d.child(self.sidebar(frame.sidebar, cx))
            })
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
