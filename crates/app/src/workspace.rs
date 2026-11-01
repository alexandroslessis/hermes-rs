//! The main window: title bar, session sidebar (grouped by project), the active chat or another
//! screen, and a status bar.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, ScrollHandle, SharedString, Subscription, Task,
    Window, WindowHandle, actions, prelude::*,
};
use hermes_store::{Project, SessionRecord};
use tokio::sync::mpsc;
use ui::{
    AgentThreadStatus, Callout, CommonAnimationExt, ContextMenu, Divider, PopoverMenu, ThreadItem,
    TintColor, Tooltip, prelude::*,
};

use crate::backend::{UiRequest, backend};
use crate::chat::{ChatEvent, ChatView};
use crate::memory_view::MemoryView;
use crate::notification::{AgentNotification, AgentNotificationEvent};
use crate::project_dialog::{ProjectDialog, ProjectDialogEvent};
use crate::settings::SettingsView;
use crate::sidebar::{ProjectGroup, folder_name, group_sessions, project_root};
use crate::skills_view::SkillsView;
use crate::state::AppState;
use crate::title_bar::TitleBar;

actions!(
    workspace,
    [
        NewSession,
        ToggleSidebar,
        OpenSettings,
        OpenMemory,
        OpenSkills
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Main {
    Chat,
    Settings,
    Memory,
    Skills,
}

/// An open notification pop-up and the session it's about.
struct Notification {
    session_id: String,
    window: WindowHandle<AgentNotification>,
    _subscription: Subscription,
}

pub struct Workspace {
    sessions: Vec<SessionRecord>,
    projects: Vec<Project>,
    chats: HashMap<String, Entity<ChatView>>,
    active: Option<String>,
    main: Main,
    settings: Entity<SettingsView>,
    memory: Entity<MemoryView>,
    skills: Entity<SkillsView>,
    sidebar_open: bool,
    collapsed: HashSet<String>,
    hovered_session: Option<String>,
    project_roots: RefCell<HashMap<String, PathBuf>>,
    sidebar_scroll: ScrollHandle,
    project_dialog: Option<(Entity<ProjectDialog>, Vec<Subscription>)>,
    /// Sessions whose turn finished while you weren't looking (Zed's blue dot).
    unseen: HashSet<String>,
    notifications: Vec<Notification>,
    notice: Option<String>,
    focus_handle: FocusHandle,
    _requests: Task<()>,
    /// Checks every few minutes whether the curator is due (Hermes runs it on idle ticks).
    _curator: Task<()>,
    _subscriptions: Vec<Subscription>,
}

fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(57).collect::<String>())
    } else {
        line.to_string()
    }
}

impl Workspace {
    pub fn new(
        mut requests: mpsc::UnboundedReceiver<UiRequest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = cx.new(|cx| SettingsView::new(window, cx));
        let memory = cx.new(|cx| MemoryView::new(window, cx));
        let skills = cx.new(SkillsView::new);
        let state = AppState::global(cx);
        let requests_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(request) = requests.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_request(request, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            sessions: Vec::new(),
            projects: Vec::new(),
            chats: HashMap::new(),
            active: None,
            main: Main::Chat,
            settings,
            memory,
            skills,
            sidebar_open: true,
            collapsed: HashSet::new(),
            hovered_session: None,
            project_roots: RefCell::default(),
            sidebar_scroll: ScrollHandle::new(),
            project_dialog: None,
            unseen: HashSet::new(),
            notifications: Vec::new(),
            notice: None,
            focus_handle: cx.focus_handle(),
            _requests: requests_task,
            _curator: cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(15 * 60))
                        .await;
                    if this
                        .update(cx, |this, cx| this.maybe_run_curator(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
            _subscriptions: vec![
                cx.observe(&state, |_, _, cx| cx.notify()),
                // Coming back to the window counts as seeing the session on screen.
                cx.observe_window_activation(window, |this, window, cx| {
                    if window.is_window_active() {
                        this.mark_active_seen(cx);
                    }
                }),
            ],
        };
        this.reload_projects(cx);
        this.reload_sessions(cx);
        if let Some(first) = this.sessions.first().map(|s| s.id.clone()) {
            this.open_session(first, window, cx);
        }
        let needs_setup = {
            let state = state.read(cx);
            !state.has_api_key() || state.config.model.is_none()
        };
        if needs_setup {
            this.main = Main::Settings;
        }
        this
    }

    /// Shows `screen`, or goes back to the chat when it is already showing.
    fn toggle_main(&mut self, screen: Main, window: &mut Window, cx: &mut Context<Self>) {
        self.main = if self.main == screen {
            Main::Chat
        } else {
            screen
        };
        match self.main {
            Main::Memory => self.memory.update(cx, |memory, cx| memory.refresh(cx)),
            Main::Skills => self.skills.update(cx, |skills, cx| skills.refresh(cx)),
            Main::Chat => {
                if let Some(chat) = self.active.as_ref().and_then(|id| self.chats.get(id)) {
                    let composer = chat.read(cx).composer();
                    composer.focus_handle(cx).focus(window, cx);
                }
                self.mark_active_seen(cx);
            }
            Main::Settings => {}
        }
        cx.notify();
    }

    fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions = backend(cx)
            .runtime
            .store()
            .list_sessions(200)
            .unwrap_or_default();
        cx.notify();
    }

    /// Hermes's `maybe_run_curator`: a scheduled run when it's due and the app is idle.
    fn maybe_run_curator(&mut self, cx: &mut Context<Self>) {
        if !backend(cx).runtime.curator_due() {
            return;
        }
        let model = AppState::global(cx).read(cx).curator_model();
        crate::state::run_curator(model, cx, |this: &mut Self, outcome, cx| {
            if let Ok(outcome) = outcome {
                tracing::info!("curator: {}", outcome.summary);
            }
            this.skills.update(cx, |skills, cx| skills.refresh(cx));
        });
    }

    fn reload_projects(&mut self, cx: &mut Context<Self>) {
        self.projects = backend(cx)
            .runtime
            .store()
            .list_projects()
            .unwrap_or_default();
        cx.notify();
    }

    fn groups(&self) -> Vec<ProjectGroup> {
        group_sessions(
            &self.sessions,
            &self.projects,
            &mut self.project_roots.borrow_mut(),
        )
    }

    /// The sidebar group a folder belongs to: its project, else its automatic group.
    fn group_key_for(&self, cwd: &str) -> String {
        match hermes_store::project_for_path(&self.projects, cwd) {
            Some(project) => project.id.clone(),
            None => format!("auto:{}", project_root(Path::new(cwd)).display()),
        }
    }

    /// The selected group: the one last picked, else the active session's, else the first.
    fn selected_group<'a>(&self, groups: &'a [ProjectGroup], cx: &App) -> Option<&'a ProjectGroup> {
        let picked = AppState::global(cx)
            .read(cx)
            .config
            .selected_project
            .clone();
        let active = self
            .active
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
            .map(|session| self.group_key_for(&session.cwd));
        picked
            .and_then(|key| groups.iter().find(|g| g.key == key))
            .or_else(|| active.and_then(|key| groups.iter().find(|g| g.key == key)))
            .or_else(|| groups.first())
    }

    fn select_group(&mut self, key: String, cx: &mut Context<Self>) {
        AppState::global(cx).update(cx, |state, cx| {
            if state.config.selected_project.as_deref() != Some(key.as_str()) {
                state.config.selected_project = Some(key);
                state.save(cx);
            }
        });
        cx.notify();
    }

    fn handle_request(&mut self, request: UiRequest, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            UiRequest::Clarify {
                session_id,
                request,
                reply,
            } => match self.chats.get(&session_id) {
                Some(chat) => chat.update(cx, |chat, cx| chat.show_clarify(request, reply, cx)),
                None => {
                    let _ = reply.send(hermes_runtime::ClarifyOutcome::Undelivered);
                }
            },
            UiRequest::ProcessFinished { session_id, notice } => {
                if let Some(chat) = self.chats.get(&session_id) {
                    chat.update(cx, |chat, cx| chat.process_finished(notice, window, cx));
                }
            }
            UiRequest::Approval {
                session_id,
                request,
                reply,
            } => match self.chats.get(&session_id) {
                Some(chat) => chat.update(cx, |chat, cx| chat.show_approval(request, reply, cx)),
                None => {
                    let _ = reply.send(hermes_tools::approval::ApprovalChoice::Deny);
                }
            },
            UiRequest::ReviewFinished {
                session_id,
                actions,
                requested,
            } => {
                if let Some(chat) = self.chats.get(&session_id) {
                    chat.update(cx, |chat, cx| chat.review_finished(actions, requested, cx));
                }
            }
        }
    }

    fn open_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.chats.contains_key(&id) {
            let Some(record) = self.sessions.iter().find(|s| s.id == id).cloned() else {
                return;
            };
            let chat =
                cx.new(|cx| ChatView::new(record.id.clone(), record.model.clone(), window, cx));
            let subscription =
                cx.subscribe_in(&chat, window, |this: &mut Self, chat, event, window, cx| {
                    match event {
                        ChatEvent::Updated => this.chat_updated(chat.clone(), cx),
                        // Zed's captions for a finished turn.
                        ChatEvent::TurnFinished { failed, used_tools } => {
                            let (caption, icon) = if *failed {
                                ("Agent stopped because of an error", IconName::Warning)
                            } else if *used_tools {
                                ("Finished running tools", IconName::Sparkle)
                            } else {
                                ("New message", IconName::Sparkle)
                            };
                            let id = chat.read(cx).session_id.clone();
                            this.session_needs_attention(id, caption, icon, window, cx);
                        }
                        ChatEvent::NeedsInput => {
                            let id = chat.read(cx).session_id.clone();
                            this.session_needs_attention(
                                id,
                                "Waiting for input",
                                IconName::Info,
                                window,
                                cx,
                            );
                        }
                        // Zed's caption for a tool waiting on the user.
                        ChatEvent::NeedsApproval => {
                            let id = chat.read(cx).session_id.clone();
                            this.session_needs_attention(
                                id,
                                "Waiting for tool confirmation",
                                IconName::Info,
                                window,
                                cx,
                            );
                        }
                    }
                });
            self._subscriptions.push(subscription);
            self.chats.insert(id.clone(), chat);
        }
        self.active = Some(id.clone());
        self.main = Main::Chat;
        if let Some(record) = self.sessions.iter().find(|s| s.id == id) {
            let key = self.group_key_for(&record.cwd);
            self.select_group(key, cx);
        }
        self.mark_active_seen(cx);
        if let Some(chat) = self.chats.get(&id) {
            let composer = chat.read(cx).composer();
            composer.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// The active session is on screen: drop its dot and its notifications.
    fn mark_active_seen(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        if self.main != Main::Chat {
            return;
        }
        self.unseen.remove(&active);
        self.dismiss_notifications(Some(&active), cx);
        cx.notify();
    }

    /// Zed's `show_notification`: when the session isn't on screen (another session shows, or
    /// the window is in the background), mark it unseen and pop up a notification.
    fn session_needs_attention(
        &mut self,
        session_id: String,
        caption: &'static str,
        icon: IconName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = window.is_window_active()
            && self.main == Main::Chat
            && self.active.as_deref() == Some(session_id.as_str());
        if visible {
            return;
        }
        self.unseen.insert(session_id.clone());
        cx.notify();
        // One pop-up per session at a time; the newest event replaces it.
        self.dismiss_notifications(Some(&session_id), cx);
        window.request_attention();
        let Some(screen) = cx.primary_display() else {
            return;
        };
        let record = self.sessions.iter().find(|s| s.id == session_id);
        let title = record
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "New session".into());
        let project = record.map(|s| {
            let key = self.group_key_for(&s.cwd);
            self.groups()
                .into_iter()
                .find(|g| g.key == key)
                .map(|g| g.name)
                .unwrap_or_else(|| folder_name(Path::new(&s.cwd)))
        });
        let options = AgentNotification::window_options(screen, cx);
        let caption = SharedString::from(caption);
        let Ok(handle) = cx.open_window(options, |_, cx| {
            cx.new(|_| AgentNotification::new(title, Some(caption), icon, project))
        }) else {
            return;
        };
        let Ok(pop_up) = handle.entity(cx) else {
            return;
        };
        let id = session_id.clone();
        let subscription = cx.subscribe_in(
            &pop_up,
            window,
            move |this: &mut Self, _, event, window, cx| match event {
                AgentNotificationEvent::Accepted => {
                    cx.activate(true);
                    window.activate_window();
                    this.open_session(id.clone(), window, cx);
                    this.dismiss_notifications(Some(&id), cx);
                }
                AgentNotificationEvent::Dismissed => this.dismiss_notifications(Some(&id), cx),
            },
        );
        self.notifications.push(Notification {
            session_id,
            window: handle,
            _subscription: subscription,
        });
    }

    /// Closes the pop-ups about `session` (all of them for `None`).
    fn dismiss_notifications(&mut self, session: Option<&str>, cx: &mut Context<Self>) {
        let (close, keep): (Vec<_>, Vec<_>) = self
            .notifications
            .drain(..)
            .partition(|n| session.is_none_or(|id| n.session_id == id));
        self.notifications = keep;
        for notification in close {
            notification
                .window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    }

    /// Titles a session from its first message, then refreshes the sidebar order.
    fn chat_updated(&mut self, chat: Entity<ChatView>, cx: &mut Context<Self>) {
        let session_id = chat.read(cx).session_id.clone();
        let needs_title = self
            .sessions
            .iter()
            .any(|s| s.id == session_id && s.title.is_none());
        if needs_title {
            let backend = backend(cx);
            if let Some(first) = backend
                .runtime
                .messages(&session_id)
                .ok()
                .and_then(|messages| {
                    messages
                        .into_iter()
                        .find(|m| m.role == rmcp::model::Role::User)
                })
            {
                let text = first.as_concat_text();
                // A skill command's message is the whole skill: title it with what was typed.
                let title = title_from(&crate::chat::typed_command(&text).unwrap_or(text));
                if !title.is_empty() {
                    let _ = backend.runtime.store().set_title(&session_id, &title);
                }
            }
        }
        self.reload_sessions(cx);
    }

    /// The folder for sessions outside any group: the last one used, else home.
    fn cwd(&self, cx: &App) -> PathBuf {
        AppState::global(cx)
            .read(cx)
            .config
            .last_cwd
            .clone()
            .filter(|path| path.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("C:/"))
    }

    /// Hermes's new session: it starts in the selected project's primary folder.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let groups = self.groups();
        let folder = self
            .selected_group(&groups, cx)
            .map(|group| group.root.clone())
            .filter(|root| root.is_dir());
        self.new_session_in(folder, window, cx);
    }

    /// Starts a session in `folder`, or in the folder chosen for new sessions.
    fn new_session_in(
        &mut self,
        folder: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = AppState::global(cx);
        let (provider, model) = {
            let state = state.read(cx);
            let model = state.config.model.clone();
            let provider = model
                .as_deref()
                .and_then(|model| state.provider_for_model(model))
                .map(crate::config::runtime_provider);
            (provider, model)
        };
        let (Some(provider), Some(model)) = (provider, model) else {
            self.notice = Some("Pick a provider and a model in Settings first.".into());
            self.main = Main::Settings;
            cx.notify();
            return;
        };
        let context_length = state.read(cx).context_length(&model);
        let cwd = folder.unwrap_or_else(|| self.cwd(cx));
        match backend(cx)
            .runtime
            .create_session(&provider, &model, context_length, &cwd)
        {
            Ok(record) => {
                self.notice = None;
                AppState::global(cx).update(cx, |state, cx| {
                    state.config.last_cwd = Some(cwd.clone());
                    state.save(cx);
                });
                self.reload_sessions(cx);
                self.open_session(record.id, window, cx);
            }
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    /// Opens the project dialog: for `project`, or a new one seeded with `name` and `folders`.
    fn open_project_dialog(
        &mut self,
        project: Option<Project>,
        name: String,
        folders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| ProjectDialog::new(project.as_ref(), name, folders, window, cx));
        let subscriptions = vec![
            cx.subscribe(&dialog, |this: &mut Self, _, event, cx| match event {
                ProjectDialogEvent::Saved(id) => {
                    this.reload_projects(cx);
                    this.select_group(id.clone(), cx);
                }
            }),
            cx.subscribe(&dialog, |this: &mut Self, _, _: &gpui::DismissEvent, cx| {
                this.project_dialog = None;
                cx.notify();
            }),
        ];
        self.project_dialog = Some((dialog, subscriptions));
        cx.notify();
    }

    fn delete_project(&mut self, id: String, cx: &mut Context<Self>) {
        let _ = backend(cx).runtime.store().delete_project(&id);
        self.reload_projects(cx);
    }

    fn delete_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chats.remove(&id);
        self.unseen.remove(&id);
        self.dismiss_notifications(Some(&id), cx);
        let backend = backend(cx);
        let runtime = backend.runtime.clone();
        let task = backend.spawn(async move { runtime.delete_session(&id).await });
        cx.spawn_in(window, async move |this, cx| {
            let _ = task.await;
            this.update_in(cx, |this, window, cx| {
                this.reload_sessions(cx);
                if this
                    .active
                    .as_ref()
                    .is_some_and(|active| !this.sessions.iter().any(|s| &s.id == active))
                {
                    this.active = None;
                    if let Some(first) = this.sessions.first().map(|s| s.id.clone()) {
                        this.open_session(first, window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// A session row's status icon, as Zed's thread list shows it.
    fn session_status(&self, id: &str, cx: &App) -> AgentThreadStatus {
        match self.chats.get(id).map(|chat| chat.read(cx)) {
            Some(chat) if chat.needs_input() => AgentThreadStatus::WaitingForConfirmation,
            Some(chat) if chat.is_running() => AgentThreadStatus::Running,
            Some(chat) if chat.has_error() => AgentThreadStatus::Error,
            _ => AgentThreadStatus::Completed,
        }
    }

    /// A project's header row: its name (highlighted when selected), and on hover a chevron,
    /// new session, and its menu.
    fn render_group_header(
        &self,
        group: &ProjectGroup,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        // Zed's header buttons: their hover must differ from the hovered header behind them.
        let (button_hover, button_active) = (colors.element_background, colors.element_active);
        let key = group.key.clone();
        let collapsed = self.collapsed.contains(&key);
        let running = group.sessions.iter().any(|s| {
            self.chats
                .get(&s.id)
                .is_some_and(|chat| chat.read(cx).is_running())
        });
        let unseen = group.sessions.iter().any(|s| self.unseen.contains(&s.id));
        let header_group = SharedString::from(format!("project-{key}"));
        let toggle_key = key.clone();
        let select_key = key.clone();
        let new_root = group.root.clone();
        let tooltip = match &group.project {
            Some(project) => project.folders.join("\n"),
            None => group.root.display().to_string(),
        };
        let menu_project = group.project.clone();
        let (menu_name, menu_root) = (group.name.clone(), group.root.display().to_string());
        let workspace = cx.entity().downgrade();
        h_flex()
            .id(SharedString::from(format!("project-header-{key}")))
            .group(header_group.clone())
            .cursor_pointer()
            .h(px(28.))
            .w_full()
            .px_1p5()
            .justify_between()
            .when(selected, |this| this.bg(colors.ghost_element_selected))
            .hover(|s| s.bg(colors.ghost_element_hover))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .child(
                        Icon::new(if group.project.is_some() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        })
                        .size(IconSize::Small)
                        .color(if selected {
                            Color::Accent
                        } else {
                            Color::Muted
                        }),
                    )
                    .child(
                        Label::new(group.name.clone())
                            .size(LabelSize::Small)
                            .color(if selected {
                                Color::Default
                            } else {
                                Color::Muted
                            })
                            .truncate(),
                    )
                    .when(collapsed && running, |this| {
                        this.child(
                            Icon::new(IconName::LoadCircle)
                                .size(IconSize::XSmall)
                                .color(Color::Muted)
                                .with_keyed_rotate_animation(
                                    SharedString::from(format!("group-spin-{key}")),
                                    2,
                                ),
                        )
                    })
                    .when(collapsed && unseen && !running, |this| {
                        this.child(
                            Icon::new(IconName::Circle)
                                .size(IconSize::XSmall)
                                .color(Color::Accent),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .visible_on_hover(header_group.clone())
                    .child(
                        IconButton::new(
                            SharedString::from(format!("collapse-{key}")),
                            if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            },
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .hover_background(button_hover)
                        .active_background(button_active)
                        .tooltip(Tooltip::text(if collapsed { "Expand" } else { "Collapse" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !this.collapsed.remove(&toggle_key) {
                                this.collapsed.insert(toggle_key.clone());
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("new-in-{key}")),
                            IconName::Plus,
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .hover_background(button_hover)
                        .active_background(button_active)
                        .tooltip(Tooltip::text(format!("New Session in {}", group.name)))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.new_session_in(Some(new_root.clone()), window, cx)
                            },
                        )),
                    )
                    .child(
                        PopoverMenu::new(SharedString::from(format!("project-menu-{key}")))
                            .trigger(
                                IconButton::new(
                                    SharedString::from(format!("project-menu-button-{key}")),
                                    IconName::Ellipsis,
                                )
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .hover_background(button_hover)
                                .active_background(button_active),
                            )
                            .anchor(gpui::Anchor::TopRight)
                            .menu(move |window, cx| {
                                let workspace = workspace.clone();
                                let project = menu_project.clone();
                                let (name, root) = (menu_name.clone(), menu_root.clone());
                                Some(ContextMenu::build(
                                    window,
                                    cx,
                                    move |menu, _, _| match project.clone() {
                                        Some(project) => {
                                            let edit = workspace.clone();
                                            let delete = workspace.clone();
                                            let edited = project.clone();
                                            let id = project.id.clone();
                                            menu.entry(
                                                "Edit Project…",
                                                None,
                                                move |window, cx| {
                                                    edit.update(cx, |this, cx| {
                                                        this.open_project_dialog(
                                                            Some(edited.clone()),
                                                            String::new(),
                                                            Vec::new(),
                                                            window,
                                                            cx,
                                                        )
                                                    })
                                                    .ok();
                                                },
                                            )
                                            .separator()
                                            .entry(
                                                "Delete Project",
                                                None,
                                                move |_, cx| {
                                                    delete
                                                        .update(cx, |this, cx| {
                                                            this.delete_project(id.clone(), cx)
                                                        })
                                                        .ok();
                                                },
                                            )
                                        }
                                        None => {
                                            let make = workspace.clone();
                                            let (name, root) = (name.clone(), root.clone());
                                            menu.entry(
                                                "Make Project…",
                                                None,
                                                move |window, cx| {
                                                    make.update(cx, |this, cx| {
                                                        this.open_project_dialog(
                                                            None,
                                                            name.clone(),
                                                            vec![root.clone()],
                                                            window,
                                                            cx,
                                                        )
                                                    })
                                                    .ok();
                                                },
                                            )
                                        }
                                    },
                                ))
                            }),
                    ),
            )
            .tooltip(Tooltip::text(tooltip))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Clicking the selected project folds it; clicking another selects it.
                let already = AppState::global(cx)
                    .read(cx)
                    .config
                    .selected_project
                    .as_deref()
                    == Some(select_key.as_str());
                if already {
                    if !this.collapsed.remove(&select_key) {
                        this.collapsed.insert(select_key.clone());
                    }
                    cx.notify();
                } else {
                    this.collapsed.remove(&select_key);
                    this.select_group(select_key.clone(), cx);
                }
            }))
            .into_any_element()
    }

    fn render_session_row(
        &self,
        session: &SessionRecord,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let (button_hover, button_active) = (colors.element_background, colors.element_active);
        let id = session.id.clone();
        let delete_id = session.id.clone();
        let hover_id = session.id.clone();
        let hovered = self.hovered_session.as_deref() == Some(session.id.as_str());
        ThreadItem::new(
            SharedString::from(session.id.clone()),
            session
                .title
                .clone()
                .unwrap_or_else(|| "New session".into()),
        )
        .base_bg(colors.panel_background)
        .icon(IconName::Chat)
        .status(self.session_status(&session.id, cx))
        .notified(self.unseen.contains(&session.id))
        .timestamp(crate::sidebar::relative_time(session.updated_at, now))
        .selected(self.active.as_deref() == Some(session.id.as_str()))
        .hovered(hovered)
        .on_hover(cx.listener(move |this, is_hovered: &bool, _, cx| {
            if *is_hovered {
                this.hovered_session = Some(hover_id.clone());
            } else if this.hovered_session.as_deref() == Some(hover_id.as_str()) {
                this.hovered_session = None;
            }
            cx.notify();
        }))
        .when(hovered, |this| {
            this.action_slot(
                IconButton::new(
                    SharedString::from(format!("delete-{}", session.id)),
                    IconName::Trash,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                // The row behind it is already hovered: Zed's row buttons use these to stand out.
                .hover_background(button_hover)
                .active_background(button_active)
                .tooltip(Tooltip::text("Delete session"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.delete_session(delete_id.clone(), window, cx)
                })),
            )
        })
        .on_click(cx.listener(move |this, _, window, cx| this.open_session(id.clone(), window, cx)))
        .into_any_element()
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = chrono::Utc::now().timestamp_millis();
        let groups = self.groups();
        let selected_key = self.selected_group(&groups, cx).map(|g| g.key.clone());
        let selected_name = groups
            .iter()
            .find(|g| Some(&g.key) == selected_key.as_ref())
            .map(|g| g.name.clone());
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            // A line between projects.
            if index > 0 {
                rows.push(div().py_1().child(Divider::horizontal()).into_any_element());
            }
            let selected = selected_key.as_deref() == Some(group.key.as_str());
            rows.push(self.render_group_header(group, selected, cx));
            if self.collapsed.contains(&group.key) {
                continue;
            }
            if group.sessions.is_empty() {
                rows.push(
                    div()
                        .px_3()
                        .py_1()
                        .child(
                            Label::new("No sessions yet")
                                .size(LabelSize::Small)
                                .color(Color::Disabled),
                        )
                        .into_any_element(),
                );
            }
            for session in &group.sessions {
                rows.push(self.render_session_row(session, now, cx));
            }
        }

        let colors = cx.theme().colors();
        let new_session_tooltip = match selected_name {
            Some(name) => format!("New Session in {name} (Ctrl+N)"),
            None => "New Session (Ctrl+N)".to_string(),
        };
        let width = sidebar_width(cx);
        v_flex()
            .relative()
            .w(width)
            .h_full()
            .flex_none()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border)
            // Zed's panel resize handle: a thin strip over the border; drag it to resize, double-click
            // to restore the default width.
            .child(
                div()
                    .id("sidebar-resize-handle")
                    .absolute()
                    .top_0()
                    .right(px(-3.))
                    .h_full()
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_drag(DraggedSidebar, |dragged, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| dragged.clone())
                    })
                    .on_click(cx.listener(|_, event: &gpui::ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            set_sidebar_width(None, cx);
                        }
                    })),
            )
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    // Each takes half the row (two full-width buttons alone would push one out).
                    .child(
                        div().flex_1().child(
                            Button::new("new-project", "New Project")
                                .style(ButtonStyle::Outlined)
                                .full_width()
                                .start_icon(Icon::new(IconName::FolderAdd).size(IconSize::Small))
                                .tooltip(Tooltip::text("Group folders into a project"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_project_dialog(
                                        None,
                                        String::new(),
                                        Vec::new(),
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            Button::new("new-session", "New Session")
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .full_width()
                                .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                                .tooltip(Tooltip::text(new_session_tooltip))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                                ),
                        ),
                    ),
            )
            .child(Divider::horizontal())
            .child(crate::scroll::with_scrollbar(
                div().flex_1().min_h_0(),
                v_flex().id("sessions").pt_1().pb_2().children(rows),
                &self.sidebar_scroll,
                window,
                cx,
            ))
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let main: AnyElement = match self.main {
            Main::Settings => self.settings.clone().into_any_element(),
            Main::Memory => self.memory.clone().into_any_element(),
            Main::Skills => self.skills.clone().into_any_element(),
            Main::Chat => match self.active.as_ref().and_then(|id| self.chats.get(id)) {
                Some(chat) => chat.clone().into_any_element(),
                None => v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .bg(colors.panel_background)
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::XLarge)
                            .color(Color::Accent),
                    )
                    .child(Headline::new("Hermes").size(HeadlineSize::Large))
                    .child(
                        Button::new("empty-new-session", "Start a New Session")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                            ),
                    )
                    .into_any_element(),
            },
        };
        let settings_open = matches!(self.main, Main::Settings);
        let memory_open = matches!(self.main, Main::Memory);
        let skills_open = matches!(self.main, Main::Skills);
        let dialog = self
            .project_dialog
            .as_ref()
            .map(|(dialog, _)| dialog.clone());
        v_flex()
            .relative()
            .size_full()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_session(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.main = Main::Settings;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenMemory, window, cx| {
                this.toggle_main(Main::Memory, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSkills, window, cx| {
                this.toggle_main(Main::Skills, window, cx)
            }))
            .font(theme::theme_settings(cx).ui_font(cx).clone())
            .text_color(colors.text)
            .bg(colors.background)
            .child(
                TitleBar::new()
                    .left(
                        IconButton::new(
                            "toggle-sidebar",
                            if self.sidebar_open {
                                IconName::ThreadsSidebarLeftOpen
                            } else {
                                IconName::ThreadsSidebarLeftClosed
                            },
                        )
                        .icon_color(Color::Muted)
                        .tooltip(Tooltip::text("Toggle sidebar (Ctrl+B)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        })),
                    )
                    .left(
                        Label::new("Hermes")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .right(
                        IconButton::new("open-skills", IconName::Book)
                            .icon_color(if skills_open {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Skills (Ctrl+Shift+K)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_main(Main::Skills, window, cx)
                            })),
                    )
                    .right(
                        IconButton::new("open-memory", IconName::Notepad)
                            .icon_color(if memory_open {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Memory (Ctrl+Shift+M)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_main(Main::Memory, window, cx)
                            })),
                    )
                    .right(
                        IconButton::new("open-settings", IconName::Settings)
                            .icon_color(if settings_open {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Settings (Ctrl+,)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_main(Main::Settings, window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .on_drag_move(cx.listener(
                        |_, event: &gpui::DragMoveEvent<DraggedSidebar>, _, cx| {
                            let width = event.event.position.x - event.bounds.left();
                            let width = width.clamp(px(MIN_SIDEBAR_WIDTH), px(MAX_SIDEBAR_WIDTH));
                            // Kept in memory while dragging; written to disk on release.
                            AppState::global(cx).update(cx, |state, cx| {
                                state.config.sidebar_width = Some(f32::from(width));
                                cx.notify();
                            });
                        },
                    ))
                    .on_drop(cx.listener(|_, _: &DraggedSidebar, _, cx| {
                        AppState::global(cx).update(cx, |state, cx| state.save(cx));
                    }))
                    .when(self.sidebar_open, |this| {
                        this.child(self.render_sidebar(window, cx))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when_some(self.notice.clone(), |this, notice| {
                                this.child(div().p_2().child(
                                    Callout::new().severity(Severity::Warning).title(notice),
                                ))
                            })
                            .child(main),
                    ),
            )
            // The project dialog over a dimmed window; clicking outside it cancels.
            .when_some(dialog, |this, dialog| {
                this.child(
                    div()
                        .id("project-dialog-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::black().opacity(0.4))
                        .occlude()
                        // On press, not click: a drag that starts in a field and ends out
                        // here must not close the dialog.
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.project_dialog = None;
                                cx.notify();
                            }),
                        )
                        .child(dialog),
                )
            })
    }
}

const DEFAULT_SIDEBAR_WIDTH: f32 = 280.;
const MIN_SIDEBAR_WIDTH: f32 = 200.;
const MAX_SIDEBAR_WIDTH: f32 = 600.;

/// What's dragged when resizing the sidebar (Zed drags a marker like this for its panels).
#[derive(Clone)]
struct DraggedSidebar;

impl Render for DraggedSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

fn sidebar_width(cx: &App) -> Pixels {
    let width = AppState::global(cx)
        .read(cx)
        .config
        .sidebar_width
        .unwrap_or(DEFAULT_SIDEBAR_WIDTH);
    px(width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH))
}

/// Sets and saves the sidebar width; `None` restores the default.
fn set_sidebar_width(width: Option<f32>, cx: &mut App) {
    AppState::global(cx).update(cx, |state, cx| {
        state.config.sidebar_width = width;
        state.save(cx);
    });
}
