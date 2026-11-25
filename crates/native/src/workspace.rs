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
//! project yet, no open thread) or the open thread ([`crate::thread::ThreadView`], Phase 4); the
//! right panel is [`crate::panel::RightPanel`] (Phase 6: its tab strip and the diff tab; "Open
//! diff" and a file link, [`crate::thread::OpenDiffTab`] and [`crate::thread::OpenFileInPanel`],
//! open it); the drawer holds the open thread's terminals ([`crate::terminal::group`], CP6.3: a
//! hub of groups, one per thread, kept while other threads are shown and while the drawer is
//! closed). Opening the drawer gives the focus to its terminal; closing it while the terminal has
//! the focus gives the focus back to the frame.
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
    rc::Rc,
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
        Button, EmptyState, Panel, Toasts,
        button::{self, Variant, icon_button},
        panel::Edge,
        toast::{self, ToastId, ToastSpec},
        tokens::{Appearance, text},
    },
    dev::ProbeExt as _,
    keymap::{self, *},
    notify::{self, Attention, Notifier, Policy},
    model::{AppModel, ModelEvent, Selection, fixture},
    palette::{self, Item, Kind, Mode, Palette, PaletteEvent},
    panel::{RightPanel, RightPanelEvent, finder::{FileFinder, FinderEvent}},
    terminal::group::{GroupEvent, Surface, TerminalHub},
    sidebar::{Sidebar, SidebarEvent, data::Snapshot, empty},
    theme::{self, DesignTheme as _, Themes},
    thread::{
        ComposerView, ComposerViewEvent, HeaderEvent, OpenDiffTab, OpenFileInPanel, OpenScreen,
        ThreadHeader, ThreadView,
        composer::{Target, data::new_thread_key},
    },
    titlebar::{self, TitleBar},
    ui_state::{self, MIN_WINDOW, UiState},
};

/// The middle never gets narrower than this (docs/PITFALLS.md: the CP0.1 report).
pub const MAIN_MIN_WIDTH: f32 = 320.;
/// The main area never gets lower than this when the drawer is open.
pub const MAIN_MIN_HEIGHT: f32 = 160.;
/// How wide the invisible band that grabs a border is.
pub const HANDLE: f32 = 6.;
/// How often the frame looks for a click on a notification.
const CLICK_POLL: Duration = Duration::from_millis(250);
/// The title bar's right-hand toggles: two 28 px buttons, a 4 px gap and 8 px of padding.
const TOGGLES_WIDTH: f32 = 68.;
/// The title bar's left block while the sidebar is collapsed: padding, toggle, gap, "Hermes".
const BRAND_BLOCK: f32 = 100.;

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
    /// What had the focus when the finder opened: closing gives it back.
    finder_return: Option<FocusHandle>,
    /// The file finder (`Ctrl+P`) while open.
    finder: Option<Entity<FileFinder>>,
    finder_subscription: Option<Subscription>,
    focus: FocusHandle,
    save_task: Option<Task<()>>,
    model: Entity<AppModel>,
    sidebar: Entity<Sidebar>,
    /// The thread a toast's action opens.
    toast_threads: HashMap<ToastId, String>,
    /// The open thread's view (made when the model opens a thread).
    thread_view: Option<Entity<ThreadView>>,
    /// The open thread's header, in the title bar (made and dropped with the view).
    thread_header: Option<Entity<ThreadHeader>>,
    header_subscription: Option<Subscription>,
    /// The composer under the open thread, or in the new-thread screen.
    composer_view: Option<Entity<ComposerView>>,
    composer_subscription: Option<Subscription>,
    /// What tells the user a thread waits (CP5.3, [`crate::notify`]); none in a fixture.
    notifier: Option<Rc<dyn Notifier>>,
    notify_policy: Policy,
    /// Requests held back before they are announced, by thread.
    holding: HashMap<String, Task<()>>,
    /// Reads the clicks on a notification.
    clicks: Option<Task<()>>,
    /// Tests say whether the window is in front (the headless one has no answer).
    active_override: Option<bool>,
    /// The right panel: the tab strip and the diff (Phase 6).
    right_panel: Entity<RightPanel>,
    /// The drawer's terminals, per thread (CP6.3).
    drawer_terminals: Entity<TerminalHub>,
    /// The drawer was opened or closed since the last frame: the focus follows at the next one.
    drawer_toggled: bool,
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
        let notifications = state.notifications;
        let focus = cx.focus_handle();
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.window_moved(window, cx);
        });
        let sidebar = cx.new(|cx| Sidebar::new(model.clone(), state.sidebar_list.clone(), cx));
        let right_panel = cx.new(|cx| RightPanel::new(model.clone(), window, cx));
        let drawer_terminals = cx.new(|_| TerminalHub::new(model.clone(), Surface::Drawer));
        // The panel reads only while it is open: it is open from the start when it was left open.
        let open = state.right_panel.open;
        right_panel.update(cx, |panel, cx| panel.set_visible(open, cx));
        let subscriptions = vec![
            cx.subscribe_in(&right_panel, window, Self::on_panel_event),
            cx.subscribe(&drawer_terminals, |this, _, event: &GroupEvent, cx| {
                if let GroupEvent::Notice(spec) = event {
                    this.push_toast(spec.clone(), cx);
                }
            }),
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.subscribe_in(&model, window, Self::on_model_event),
            // The palette's list, the title and the open thread follow the model.
            cx.observe_in(&model, window, |this, _, window, cx| {
                this.sync_thread_view(window, cx);
                cx.notify();
            }),
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
            finder: None,
            finder_subscription: None,
            finder_return: None,
            focus,
            save_task: None,
            model,
            sidebar,
            toast_threads: HashMap::new(),
            thread_view: None,
            thread_header: None,
            header_subscription: None,
            composer_view: None,
            composer_subscription: None,
            notifier: None,
            notify_policy: Policy::new(notifications),
            holding: HashMap::new(),
            clicks: None,
            active_override: None,
            right_panel,
            drawer_terminals,
            drawer_toggled: false,
            _bounds: bounds,
            _subscriptions: subscriptions,
        }
    }

    /// The composer of the open thread or of the new-thread screen.
    pub fn composer_view(&self) -> Option<&Entity<ComposerView>> {
        self.composer_view.as_ref()
    }

    /// The open thread's view.
    pub fn thread_view(&self) -> Option<&Entity<ThreadView>> {
        self.thread_view.as_ref()
    }

    /// The right panel.
    pub fn right_panel(&self) -> &Entity<RightPanel> {
        &self.right_panel
    }

    /// What the right panel says: toasts, and a revert changing the folder's git state.
    fn on_panel_event(
        &mut self,
        _: &Entity<RightPanel>,
        event: &RightPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            RightPanelEvent::OpenFinder => self.open_finder(window, cx),
            RightPanelEvent::Notice(spec) => {
                self.push_toast(spec.clone(), cx);
            }
            RightPanelEvent::Reverted => {
                if let Some(thread) = self.model.read(cx).thread().cloned() {
                    thread.update(cx, |t, cx| t.refresh_header(cx));
                }
            }
        }
    }

    /// Makes the view of the model's open thread, or drops it when none is open.
    fn sync_thread_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prune_drafts(cx);
        let wanted_composer = self.model.read(cx).composer().cloned();
        let current_composer = self
            .composer_view
            .as_ref()
            .map(|v| v.read(cx).model().clone());
        if wanted_composer != current_composer {
            self.make_composer_view(wanted_composer, window, cx);
        }
        let thread = self.model.read(cx).thread().cloned();
        self.right_panel
            .update(cx, |panel, cx| panel.set_thread(thread.clone(), cx));
        let current = self
            .thread_view
            .as_ref()
            .map(|v| v.read(cx).model().clone());
        if thread == current {
            return;
        }
        let composer = self.composer_view.clone();
        self.thread_view = thread
            .clone()
            .map(|thread| cx.new(|cx| ThreadView::new(thread, composer, window, cx)));
        // The header is the title bar's middle while a thread is open.
        let app = self.model.clone();
        self.thread_header =
            thread.map(|thread| cx.new(|cx| ThreadHeader::new(app, thread, window, cx)));
        self.header_subscription = self.thread_header.as_ref().map(|header| {
            cx.subscribe_in(
                header,
                window,
                |this, _, event: &HeaderEvent, window, cx| match event {
                    HeaderEvent::Notice(spec) => {
                        this.push_toast(spec.clone(), cx);
                    }
                    HeaderEvent::RenameEnded => this.focus(window, cx),
                },
            )
        });
    }

    /// The open thread's header.
    /// Makes the composer's view over `model` (starting from the draft kept for it), or drops it.
    fn make_composer_view(
        &mut self,
        model: Option<Entity<crate::thread::ComposerModel>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_view = model.map(|model| {
            let key = match model.read(cx).target() {
                Target::Thread(id) => id.clone(),
                Target::Draft(project) => new_thread_key(project),
            };
            let draft = self.state.drafts.get(&key).map(str::to_string);
            cx.new(|cx| ComposerView::new(model, key, draft, window, cx))
        });
        self.composer_subscription = self.composer_view.as_ref().map(|view| {
            cx.subscribe_in(view, window, |this, _, event: &ComposerViewEvent, window, cx| {
                match event {
                    ComposerViewEvent::Draft { key, text } => this.set_draft(key, text, cx),
                    ComposerViewEvent::Notice(spec) => {
                        this.push_toast(spec.clone(), cx);
                    }
                    // A command of the app typed in the composer (`/new`, `/settings`): the same
                    // handler the palette and the shortcuts reach.
                    ComposerViewEvent::App(id) => this.run_command(id, window, cx),
                }
            })
        });
    }

    /// Keeps `text` as the draft of `key` and writes `native-ui.json` after a pause.
    fn set_draft(&mut self, key: &str, text: &str, cx: &mut Context<Self>) {
        if self
            .state
            .drafts
            .set(key, text, crate::model::system_now_ms())
        {
            self.changed(cx);
        }
    }

    /// Drops the drafts of threads and projects the list no longer has once they are a week old
    /// (a deleted thread's draft does not stay forever; an archived one keeps it a while).
    fn prune_drafts(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        if !matches!(model.phase(), crate::model::Phase::Ready) || self.state.drafts.is_empty() {
            return;
        }
        let snapshot = model.snapshot();
        let listed = |key: &str| match key.strip_prefix("new:") {
            Some(project) => snapshot.project(project).is_some(),
            None => snapshot.thread(key).is_some(),
        };
        if self
            .state
            .drafts
            .prune(listed, crate::model::system_now_ms())
        {
            self.changed(cx);
        }
    }

    pub fn thread_header(&self) -> Option<&Entity<ThreadHeader>> {
        self.thread_header.as_ref()
    }

    /// A file link was clicked: the right panel opens on the file (in the diff when it is
    /// changed, else the Files tab's placeholder until CP6.2).
    pub fn open_file_in_panel(
        &mut self,
        file: &OpenFileInPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_right_panel(cx);
        self.right_panel
            .update(cx, |panel, cx| panel.open_file(file, window, cx));
        cx.notify();
    }

    /// The file finder, while open.
    pub fn finder(&self) -> Option<&Entity<FileFinder>> {
        self.finder.as_ref()
    }

    /// Opens the finder over the open thread's folder (`Ctrl+P`), or closes it when open.
    pub fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finder.is_some() {
            self.close_finder(window, cx);
            return;
        }
        let Some(session) = self
            .model
            .read(cx)
            .thread()
            .map(|t| t.read(cx).session().to_string())
        else {
            self.push_toast(
                ToastSpec::new(toast::Kind::Info, "Open a thread to search its files."),
                cx,
            );
            return;
        };
        // Closing gives the focus back where it was (the composer, the tree).
        self.finder_return = window.focused(cx);
        let app = self.model.clone();
        let finder = cx.new(|cx| FileFinder::new(app, session, window, cx));
        self.finder_subscription = Some(cx.subscribe_in(
            &finder,
            window,
            |this, _, event: &FinderEvent, window, cx| match event {
                FinderEvent::Dismiss => this.close_finder(window, cx),
                FinderEvent::Open(path) => {
                    let file = OpenFileInPanel {
                        path: path.clone(),
                        line: None,
                    };
                    // The file view takes it from here.
                    this.finder_return = None;
                    this.close_finder(window, cx);
                    this.open_file_in_panel(&file, window, cx);
                }
            },
        ));
        finder.update(cx, |f, cx| f.focus_input(window, cx));
        self.finder = Some(finder);
        cx.notify();
    }

    pub fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        self.finder_subscription = None;
        match self.finder_return.take() {
            Some(back) => window.focus(&back, cx),
            None => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    /// A card or the turn footer asked for the diff tab: the right panel opens on it.
    pub fn open_diff_tab(&mut self, diff: &OpenDiffTab, cx: &mut Context<Self>) {
        self.open_right_panel(cx);
        self.right_panel
            .update(cx, |panel, cx| panel.open_diff(diff.path.as_deref(), cx));
        cx.notify();
    }

    /// Opens the right panel when it is closed.
    fn open_right_panel(&mut self, cx: &mut Context<Self>) {
        if !self.state.right_panel.open {
            self.state.right_panel.open = true;
            self.changed(cx);
        }
    }

    /// A memory or skill write card asked for its screen (Phase 7).
    pub fn open_screen(&mut self, screen: &OpenScreen, cx: &mut Context<Self>) {
        let what = match screen.screen.as_str() {
            "skill" => format!("Skill {}", screen.target),
            "user" => "Your profile".to_string(),
            _ => "Memory".to_string(),
        };
        self.push_toast(
            ToastSpec::new(toast::Kind::Info, what)
                .description("This screen arrives in Phase 7.".to_string()),
            cx,
        );
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

    fn on_model_event(
        &mut self,
        _: &Entity<AppModel>,
        event: &ModelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ModelEvent::Toast { spec, open } => {
                let id = self.push_toast(spec.clone(), cx);
                if let Some(thread) = open {
                    self.toast_threads.insert(id, thread.clone());
                }
            }
            ModelEvent::Waiting { session } => self.hold_announcement(session, window, cx),
            ModelEvent::Resumed { session } => {
                // Answered or settled before the hold ran out: nothing is announced; and the
                // thread's next wait is a new one.
                self.holding.remove(session);
                self.notify_policy.resumed(session);
            }
        }
    }

    // --- a thread waits for the user (CP5.3) ----------------------------------------------------

    /// Gives the frame what tells the user (the app's [`notify::OsNotifier`], or a test's), and
    /// the channel the notification's clicks arrive on.
    pub fn set_notifier(
        &mut self,
        notifier: Rc<dyn Notifier>,
        clicked: Option<std::sync::mpsc::Receiver<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notifier = Some(notifier);
        self.clicks = clicked.map(|clicked| {
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    match clicked.try_recv() {
                        Ok(session) => {
                            let done = this.update_in(cx, |this, window, cx| {
                                this.model.update(cx, |m, cx| m.open(&session, cx));
                                window.activate_window();
                            });
                            if done.is_err() {
                                break;
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            cx.background_executor().timer(CLICK_POLL).await;
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    }
                }
            })
        });
    }

    /// Makes the frame believe the window is in front (`Some(true)`) or behind (`Some(false)`):
    /// for tests, since a headless window has no focus to ask.
    pub fn force_window_active(&mut self, active: Option<bool>) {
        self.active_override = active;
    }

    /// Turns announcing on or off (the `notifications` setting).
    pub fn set_notifications(&mut self, on: bool) {
        self.state.notifications = on;
        self.notify_policy.set_enabled(on);
    }

    /// A thread began to wait: announce it after [`notify::HOLD`] unless it was answered by then.
    fn hold_announcement(&mut self, session: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.notifier.is_none() || self.holding.contains_key(session) {
            return;
        }
        let key = session.to_string();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(notify::HOLD).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.holding.remove(&key);
                this.announce(&key, window, cx);
            });
        });
        self.holding.insert(session.to_string(), task);
    }

    /// Flashes the taskbar and shows a notification for `session`, if it still waits and the user
    /// is not looking at it ([`Policy`]).
    fn announce(&mut self, session: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(notifier) = self.notifier.clone() else {
            return;
        };
        let attention = {
            let model = self.model.read(cx);
            let title = model.thread_title(session);
            Attention::of(model.asks(), session, &title)
        };
        let Some(attention) = attention else {
            return;
        };
        let active = self
            .active_override
            .unwrap_or_else(|| window.is_window_active());
        let open = self.model.read(cx).open_thread().map(str::to_string);
        if !self
            .notify_policy
            .should_announce(session, open.as_deref(), active)
        {
            return;
        }
        // The taskbar button of a window that is in front does not need to flash.
        if !active {
            notifier.flash(window);
        }
        notifier.toast(&attention);
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
        // The right panel reads only while it is open.
        let open = self.state.right_panel.open;
        self.right_panel
            .update(cx, |panel, cx| panel.set_visible(open, cx));
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

    /// Opens or closes the bottom drawer (the focus goes to its terminal, or back to the frame,
    /// at the next frame).
    pub fn toggle_drawer(&mut self, cx: &mut Context<Self>) {
        self.state.drawer.open = !self.state.drawer.open;
        self.drawer_toggled = true;
        self.changed(cx);
    }

    /// The drawer's terminal hub.
    pub fn drawer_terminals(&self) -> &Entity<TerminalHub> {
        &self.drawer_terminals
    }

    /// The open thread's id, when a thread (not a draft) is open.
    fn open_thread_id(&self, cx: &App) -> Option<String> {
        match self.model.read(cx).selected() {
            Selection::Thread(id) => Some(id.clone()),
            _ => None,
        }
    }

    /// The drawer's content: the open thread's terminals (the first starts now), or a note.
    fn drawer_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let toggled = std::mem::take(&mut self.drawer_toggled);
        let Some(session) = self.open_thread_id(cx) else {
            let palette = cx.design_theme().palette;
            return div()
                .id("drawer-body")
                .flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_size(text::SM.size)
                .text_color(palette.muted_foreground.hsla())
                .child("Open a thread to use its terminal.")
                .probe()
                .into_any_element();
        };
        let group = self
            .drawer_terminals
            .update(cx, |hub, cx| hub.group(&session, cx));
        group.update(cx, |g, cx| {
            g.ensure_started(window, cx);
            if toggled {
                g.focus_active(window, cx);
            }
        });
        div()
            .id("drawer-body")
            .size_full()
            .child(group)
            .probe()
            .into_any_element()
    }

    /// The drawer was just closed: if one of its terminals had the focus, the frame takes it.
    fn drawer_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.drawer_toggled) {
            return;
        }
        let focused_terminal = self.drawer_terminals.read(cx).groups().any(|(_, group)| {
            group
                .read(cx)
                .terminals()
                .iter()
                .any(|t| t.read(cx).focus_handle(cx).is_focused(window))
        });
        if focused_terminal {
            window.focus(&self.focus, cx);
        }
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
            "files.find" => self.open_finder(window, cx),
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
                    keymap::tooltip("terminal.toggle", "Toggle terminal drawer"),
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
                    keymap::tooltip("rightPanel.toggle", "Toggle right panel"),
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
            keymap::tooltip("sidebar.toggle", "Toggle sidebar"),
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

    /// What the main area is about: the sidebar's empty states, or the open thread.
    fn main_area(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let model = self.model.read(cx);
        let snapshot = model.snapshot();
        let starting = matches!(model.phase(), crate::model::Phase::Starting);
        let body: AnyElement = match model.selected() {
            Selection::Thread(_) => match &self.thread_view {
                Some(view) => div()
                    .id("thread-slot")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(view.clone())
                    .into_any_element(),
                // The model opens it; the view follows on the next notification.
                None => div().into_any_element(),
            },
            Selection::Draft(project) => {
                let name = snapshot
                    .project(project)
                    .map_or("this project".to_string(), |p| p.name.clone());
                let without = (project != crate::sidebar::data::NO_PROJECT).then(|| {
                    let model = self.model.clone();
                    Button::new("start-without-project", "or start without a project")
                        .variant(Variant::GhostMuted)
                        .size(button::Size::Sm)
                        .on_click(move |_, _, cx| {
                            model.update(cx, |m, cx| {
                                m.new_thread(crate::sidebar::data::NO_PROJECT, cx)
                            })
                        })
                        .into_any_element()
                });
                crate::thread::composer::view::hero(
                    &palette,
                    format!("What should we build in {name}?"),
                    without,
                    self.composer_view.clone(),
                )
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
        // With a thread open the bar's middle is its header (interactive: the title edits, a
        // menu opens), told how much room it has; otherwise the text title in the drag zone.
        let room = (w - frame.sidebar)
            - if frame.sidebar == 0. { BRAND_BLOCK } else { 0. }
            - TOGGLES_WIDTH
            - 3. * titlebar::CAPTION_WIDTH
            - titlebar::DRAG_MIN;
        let header = self
            .thread_header
            .clone()
            .filter(|_| matches!(self.model.read(cx).selected(), Selection::Thread(_)));
        if let Some(header) = &header {
            let colors = &self.state.sidebar_list.colors;
            header.update(cx, |header, _| {
                header.set_room(room);
                header.set_colors(colors);
            });
        }
        let topbar = TitleBar::new()
            .maximized(maximized)
            .right(self.panel_toggles(cx))
            .when_some(sidebar_toggle, |bar, left| bar.left(left))
            .map(|bar| match header {
                Some(header) => bar.header(header),
                None => bar.title(title),
            });

        let right = (frame.right > 0.).then(|| {
            div()
                .id("right-panel-slot")
                .flex_shrink_0()
                .w(px(frame.right))
                .h_full()
                .child(
                    Panel::pane("right-panel", Edge::Left)
                        .size_full()
                        .child(self.right_panel.clone()),
                )
        });
        let drawer = if frame.drawer > 0. {
            let body = self.drawer_body(window, cx);
            Some(
                div()
                    .id("drawer-slot")
                    .flex_shrink_0()
                    .h(px(frame.drawer))
                    .w_full()
                    .child(Panel::pane("drawer", Edge::Top).size_full().child(body)),
            )
        } else {
            self.drawer_closed(window, cx);
            None
        };

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

        let finder_layer = self.finder.clone().map(|f| {
            div()
                .id("finder-layer")
                .probe()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px((h * 0.12).max(24.)))
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
                    cx.listener(|this, _, window, cx| this.close_finder(window, cx)),
                )
                .child(
                    div()
                        .id("finder-box")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(f),
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
            .on_action(cx.listener(|this, _: &OpenFileFinder, window, cx| {
                this.open_finder(window, cx)
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
            .on_action(
                cx.listener(|this, file: &OpenFileInPanel, window, cx| {
                    this.open_file_in_panel(file, window, cx)
                }),
            )
            .on_action(cx.listener(|this, diff: &OpenDiffTab, _, cx| this.open_diff_tab(diff, cx)))
            .on_action(cx.listener(|this, screen: &OpenScreen, _, cx| this.open_screen(screen, cx)))
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
            .when_some(finder_layer, |d, p| d.child(p))
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
