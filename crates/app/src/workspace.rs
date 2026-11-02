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
    AgentThreadStatus, Callout, CommonAnimationExt, ContextMenu, Divider, PopoverMenu, Tab, TabBar,
    TabPosition, ThreadItem, TintColor, Tooltip, prelude::*, right_click_menu,
};

use crate::backend::{UiRequest, backend};
use crate::chat::{ChatEvent, ChatView};
use crate::memory_view::MemoryView;
use crate::notification::{AgentNotification, AgentNotificationEvent};
use crate::palette::{Palette, PaletteItem, PalettePicked};
use crate::review_pane::{ReviewEvent, ReviewPane};
use crate::project_dialog::{ProjectDialog, ProjectDialogEvent};
use crate::settings::SettingsView;
use crate::sidebar::{ProjectGroup, folder_name, group_sessions, project_root};
use crate::skills_view::SkillsView;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};
use crate::title_bar::TitleBar;

actions!(
    workspace,
    [
        NewSession,
        ToggleSidebar,
        CloseTab,
        NextTab,
        PreviousTab,
        OpenSettings,
        OpenMemory,
        OpenSkills,
        ToggleCommandPalette,
        GoToSession,
        ToggleReview
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Main {
    Chat,
    Settings,
    Memory,
    Skills,
    /// The first-run wizard.
    Onboarding,
}

/// The tab keys of the screens that open as tabs (session tabs are keyed by session id).
const SCREEN_TABS: [(Main, &str); 3] = [
    (Main::Settings, "screen:settings"),
    (Main::Memory, "screen:memory"),
    (Main::Skills, "screen:skills"),
];

fn screen_tab(screen: Main) -> Option<&'static str> {
    SCREEN_TABS
        .iter()
        .find(|(s, _)| *s == screen)
        .map(|(_, key)| *key)
}

fn tab_screen(key: &str) -> Option<Main> {
    SCREEN_TABS
        .iter()
        .find(|(_, k)| *k == key)
        .map(|(screen, _)| *screen)
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
    /// The first-run wizard, while it shows.
    onboarding: Option<(Entity<crate::onboarding::OnboardingView>, Subscription)>,
    /// Open sessions, in tab order.
    tabs: Vec<String>,
    active: Option<String>,
    main: Main,
    settings: Entity<SettingsView>,
    memory: Entity<MemoryView>,
    skills: Entity<SkillsView>,
    sidebar_open: bool,
    collapsed: HashSet<String>,
    hovered_session: Option<String>,
    /// The sidebar search box, and the sessions matching it (`None` when it's empty).
    search: Entity<TextArea>,
    search_hits: Option<HashSet<String>>,
    /// The session being renamed and its title editor.
    renaming: Option<(String, Entity<TextArea>, Subscription)>,
    project_roots: RefCell<HashMap<String, PathBuf>>,
    sidebar_scroll: ScrollHandle,
    project_dialog: Option<(Entity<ProjectDialog>, Vec<Subscription>)>,
    /// The open command palette or session switcher, and what had focus before it.
    palette: Option<(Entity<Palette>, Option<FocusHandle>, Vec<Subscription>)>,
    /// The review pane beside the chat, and whether it shows.
    review: Entity<ReviewPane>,
    review_open: bool,
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
        let review = cx.new(|_| ReviewPane::new());
        let review_subscription = cx.subscribe(&review, |this: &mut Self, _, event, cx| match event {
            ReviewEvent::Close => {
                this.review_open = false;
                cx.notify();
            }
        });
        let search = cx.new(|cx| TextArea::new("Search sessions…", cx).single_line());
        let search_subscription = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, TextAreaEvent::Changed) {
                this.update_search(cx);
            }
        });
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
            tabs: Vec::new(),
            onboarding: None,
            active: None,
            main: Main::Chat,
            settings,
            memory,
            skills,
            sidebar_open: true,
            collapsed: HashSet::new(),
            hovered_session: None,
            search,
            search_hits: None,
            renaming: None,
            project_roots: RefCell::default(),
            sidebar_scroll: ScrollHandle::new(),
            project_dialog: None,
            palette: None,
            review,
            review_open: false,
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
                search_subscription,
                review_subscription,
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
            // No provider that can be called, or no model: the wizard (it can hand over to Settings).
            let view = cx.new(crate::onboarding::OnboardingView::new);
            let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
                this.onboarding = None;
                match event {
                    crate::onboarding::OnboardingEvent::Done => {
                        this.main = Main::Chat;
                        this.new_session(window, cx);
                    }
                    crate::onboarding::OnboardingEvent::Skip => {
                        this.main = Main::Chat;
                        this.open_screen(Main::Settings, window, cx)
                    }
                }
                cx.notify();
            });
            this.onboarding = Some((view, subscription));
            this.main = Main::Onboarding;
        }
        this
    }

    /// Shows `screen`, or goes back to the chat when it is already showing.
    /// Settings, Memory and Skills open as tabs beside the sessions; asking again focuses the
    /// open tab instead of adding another.
    fn open_screen(&mut self, screen: Main, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = screen_tab(screen) else {
            return;
        };
        if !self.tabs.iter().any(|t| t == key) {
            let at = self
                .active
                .as_ref()
                .and_then(|active| self.tabs.iter().position(|t| t == active))
                .map_or(self.tabs.len(), |i| i + 1);
            self.tabs.insert(at, key.to_string());
        }
        self.active = Some(key.to_string());
        self.main = screen;
        match screen {
            Main::Memory => self.memory.update(cx, |memory, cx| memory.refresh(cx)),
            Main::Skills => self.skills.update(cx, |skills, cx| skills.refresh(cx)),
            _ => {}
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Shows a tab: a screen or a session.
    fn activate_tab(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        match tab_screen(&key) {
            Some(screen) => self.open_screen(screen, window, cx),
            None => self.open_session(key, window, cx),
        }
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

    /// Re-runs the sidebar search for the search box's text.
    fn update_search(&mut self, cx: &mut Context<Self>) {
        let text = self.search.read(cx).text().trim().to_string();
        self.search_hits = (!text.is_empty()).then(|| {
            backend(cx)
                .runtime
                .store()
                .sessions_matching(&text)
                .unwrap_or_default()
        });
        cx.notify();
    }

    /// Whether a session shows under the current search.
    fn shown(&self, id: &str) -> bool {
        self.search_hits
            .as_ref()
            .is_none_or(|hits| hits.contains(id))
    }

    fn set_pinned(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        if let Err(error) = backend(cx).runtime.store().set_pinned(id, pinned) {
            self.notice = Some(format!("{error:#}"));
        }
        self.reload_sessions(cx);
    }

    /// Replaces a session's title with an editor; Enter saves, Escape cancels.
    fn start_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut area = TextArea::new("Session name", cx).single_line();
            area.set_text(title, cx);
            area
        });
        let subscription = cx.subscribe(&editor, |this, editor, event, cx| {
            if matches!(event, TextAreaEvent::Submit) {
                let title = editor.read(cx).text().trim().to_string();
                this.finish_rename(Some(title), cx);
            }
        });
        editor.focus_handle(cx).focus(window, cx);
        self.renaming = Some((id, editor, subscription));
        cx.notify();
    }

    /// Saves the new title (`None` cancels; an empty title keeps the old one).
    fn finish_rename(&mut self, title: Option<String>, cx: &mut Context<Self>) {
        let Some((id, _, _)) = self.renaming.take() else {
            return;
        };
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            if let Err(error) = backend(cx).runtime.store().set_title(&id, &title) {
                self.notice = Some(format!("{error:#}"));
            }
            self.reload_sessions(cx);
        }
        cx.notify();
    }

    fn reload_projects(&mut self, cx: &mut Context<Self>) {
        self.projects = backend(cx)
            .runtime
            .store()
            .list_projects()
            .unwrap_or_default();
        cx.notify();
    }

    /// The project groups of the sessions shown (pinned ones are listed apart).
    fn groups(&self) -> Vec<ProjectGroup> {
        let sessions: Vec<SessionRecord> = self
            .sessions
            .iter()
            .filter(|s| !s.pinned && self.shown(&s.id))
            .cloned()
            .collect();
        group_sessions(
            &sessions,
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
                            // The turn may have edited files.
                            if this.review_open && this.active.as_ref() == Some(&id) {
                                this.review.update(cx, |review, cx| review.reload(cx));
                            }
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
        if !self.tabs.contains(&id) {
            // A new tab opens next to the current one.
            let at = self
                .active
                .as_ref()
                .and_then(|active| self.tabs.iter().position(|t| t == active))
                .map_or(self.tabs.len(), |i| i + 1);
            self.tabs.insert(at, id.clone());
        }
        self.active = Some(id.clone());
        self.main = Main::Chat;
        if self.review_open {
            let active = self.active.clone();
            self.review.update(cx, |review, cx| review.show(active, cx));
        }
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
                // A skill command or expanded references: title it with what was typed.
                let typed = crate::chat::shown_user_text(&first);
                let title = title_from(&typed);
                if !title.is_empty() {
                    let _ = backend.runtime.store().set_title(&session_id, &title);
                    // Hermes's titler then names it properly (the instant title is a placeholder).
                    self.generate_title(session_id.clone(), title, typed, cx);
                }
            }
        }
        self.reload_sessions(cx);
    }

    /// Asks the titles model (else the session's) for a title, and keeps it unless the session
    /// was renamed in the meantime.
    fn generate_title(
        &mut self,
        session_id: String,
        placeholder: String,
        opening: String,
        cx: &mut Context<Self>,
    ) {
        let (provider, model) = {
            let state = AppState::global(cx).read(cx);
            let chosen = state.aux_model(&state.config.aux_models.titles);
            match chosen {
                Some(aux) => (aux.provider, aux.model),
                None => {
                    let Some(model) = self
                        .sessions
                        .iter()
                        .find(|s| s.id == session_id)
                        .map(|s| s.model.clone())
                    else {
                        return;
                    };
                    let Some(provider) = state.provider_for_model(&model) else {
                        return;
                    };
                    (crate::config::runtime_provider(provider), model)
                }
            }
        };
        let request = backend(cx).tokio.spawn(async move {
            hermes_runtime::generate_title(&provider, &model, &opening).await
        });
        cx.spawn(async move |this, cx| {
            let title = match request.await {
                Ok(Ok(Some(title))) => title,
                Ok(Err(error)) => {
                    tracing::debug!("title generation failed: {error:#}");
                    return;
                }
                _ => return,
            };
            this.update(cx, |this, cx| {
                let backend = backend(cx);
                let store = backend.runtime.store();
                let unchanged = store
                    .session(&session_id)
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.title.as_deref() == Some(placeholder.as_str()));
                if unchanged {
                    let _ = store.set_title(&session_id, &title);
                    this.reload_sessions(cx);
                }
            })
            .ok();
        })
        .detach();
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
            self.open_screen(Main::Settings, window, cx);
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

    /// Ctrl+Shift+D: the review pane beside the chat.
    fn toggle_review(&mut self, cx: &mut Context<Self>) {
        self.review_open = !self.review_open;
        if self.review_open {
            let active = self.active.clone();
            self.review.update(cx, |review, cx| review.show(active, cx));
        }
        cx.notify();
    }

    /// Ctrl+Shift+P: the actions available where focus is, then the active chat's commands.
    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            cx.notify();
            return;
        }
        let mut actions: Vec<PaletteItem> = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| {
                let name = action.name();
                name.starts_with("workspace::") || name.starts_with("chat::")
            })
            .filter(|action| !action.as_any().is::<ToggleCommandPalette>())
            .map(|action| PaletteItem::Action {
                label: crate::palette::humanize_action_name(action.name()).into(),
                action,
            })
            .collect();
        actions.sort_by_key(|item| match item {
            PaletteItem::Action { label, .. } => label.clone(),
            _ => SharedString::default(),
        });
        let in_chat = matches!(self.main, Main::Chat) && self.active.is_some();
        if in_chat {
            actions.extend(crate::chat::BUILTIN_COMMANDS.iter().map(|(name, description)| {
                PaletteItem::Command {
                    name,
                    description,
                    // These need their arguments typed.
                    run: !matches!(*name, "learn" | "steer" | "queue"),
                }
            }));
        }
        self.open_palette("Execute a command…", actions, window, cx);
    }

    /// Ctrl+P: jump to a session by title or project.
    fn toggle_session_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            cx.notify();
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let items = self
            .sessions
            .iter()
            .map(|session| PaletteItem::Session {
                id: session.id.clone(),
                title: session
                    .title
                    .clone()
                    .unwrap_or_else(|| "New session".into())
                    .into(),
                detail: format!(
                    "{} · {}",
                    folder_name(Path::new(&session.cwd)),
                    crate::sidebar::relative_time(session.updated_at, now)
                )
                .into(),
            })
            .collect();
        self.open_palette("Go to a session…", items, window, cx);
    }

    fn open_palette(
        &mut self,
        placeholder: &str,
        items: Vec<PaletteItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = window.focused(cx);
        let palette = cx.new(|cx| Palette::new(placeholder, items, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&palette, window, |this, _, PalettePicked(item), window, cx| {
                this.palette_picked(item, window, cx)
            }),
            cx.subscribe_in(&palette, window, |this, _, _: &gpui::DismissEvent, window, cx| {
                this.close_palette(window, cx)
            }),
        ];
        palette.focus_handle(cx).focus(window, cx);
        self.palette = Some((palette, previous, subscriptions));
        cx.notify();
    }

    /// Closes the palette and gives focus back to what had it.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, previous, _)) = self.palette.take() {
            previous
                .unwrap_or_else(|| self.focus_handle.clone())
                .focus(window, cx);
        }
        cx.notify();
    }

    fn palette_picked(&mut self, item: &PaletteItem, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match item {
            // Dispatched where focus was, as if its key had been pressed there.
            PaletteItem::Action { action, .. } => window.dispatch_action(action.boxed_clone(), cx),
            PaletteItem::Command { name, run, .. } => {
                if let Some(chat) = self.active.as_ref().and_then(|id| self.chats.get(id)) {
                    chat.update(cx, |chat, cx| chat.run_command(name, *run, window, cx));
                }
            }
            PaletteItem::Session { id, .. } => self.open_session(id.clone(), window, cx),
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

    /// Closes a session's tab. A chat with a running turn stays alive (the turn keeps going and
    /// the sidebar shows it); the next tab to the right (else left) becomes active.
    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t == id) else {
            return;
        };
        self.tabs.remove(index);
        if tab_screen(id).is_none() && !backend(cx).runtime.is_running(id) {
            self.chats.remove(id);
        }
        if self.active.as_deref() == Some(id) {
            self.active = None;
            self.main = Main::Chat;
            let next = self
                .tabs
                .get(index)
                .or_else(|| index.checked_sub(1).and_then(|i| self.tabs.get(i)))
                .cloned();
            if let Some(next) = next {
                self.activate_tab(next, window, cx);
            }
        }
        cx.notify();
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab: the next or previous tab, wrapping around.
    fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        let current = self
            .active
            .as_ref()
            .and_then(|id| self.tabs.iter().position(|t| t == id))
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % self.tabs.len()
        } else {
            (current + self.tabs.len() - 1) % self.tabs.len()
        };
        let id = self.tabs[next].clone();
        self.activate_tab(id, window, cx);
    }

    /// Zed's tab bar over the chat: one tab per open session, with its status and a close button.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.tabs.len();
        let active = self
            .active
            .as_ref()
            .and_then(|id| self.tabs.iter().position(|t| t == id))
            .unwrap_or(0);
        TabBar::new("session-tabs").children(self.tabs.iter().enumerate().map(|(index, id)| {
            let screen = tab_screen(id);
            let title = match screen {
                Some(Main::Settings) => "Settings".to_string(),
                Some(Main::Memory) => "Memory".to_string(),
                Some(Main::Skills) => "Skills".to_string(),
                _ => self
                    .sessions
                    .iter()
                    .find(|s| &s.id == id)
                    .and_then(|s| s.title.clone())
                    .unwrap_or_else(|| "New session".into()),
            };
            let screen_icon = match screen {
                Some(Main::Settings) => Some(IconName::Settings),
                Some(Main::Memory) => Some(IconName::Notepad),
                Some(Main::Skills) => Some(IconName::Book),
                _ => None,
            };
            let middle_close_id = id.clone();
            let position = if index == 0 {
                TabPosition::First
            } else if index + 1 == count {
                TabPosition::Last
            } else {
                TabPosition::Middle(index.cmp(&active))
            };
            let running = matches!(self.session_status(id, cx), AgentThreadStatus::Running);
            let (open_id, close_id) = (id.clone(), id.clone());
            Tab::new(SharedString::from(format!("tab-{id}")))
                .position(position)
                .toggle_state(index == active)
                .start_slot::<AnyElement>(if running {
                    Some(
                        Icon::new(IconName::LoadCircle)
                            .size(IconSize::XSmall)
                            .color(Color::Muted)
                            .with_rotate_animation(2)
                            .into_any_element(),
                    )
                } else {
                    screen_icon.map(|icon| {
                        Icon::new(icon)
                            .size(IconSize::XSmall)
                            .color(Color::Muted)
                            .into_any_element()
                    })
                })
                // A middle click anywhere on the tab closes it, as in Zed and browsers.
                .on_mouse_down(
                    gpui::MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(&middle_close_id, window, cx)
                    }),
                )
                .end_slot(
                    IconButton::new(
                        SharedString::from(format!("close-tab-{id}")),
                        IconName::Close,
                    )
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Close Tab (Ctrl+W)"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(&close_id, window, cx)
                    })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_tab(open_id.clone(), window, cx)
                }))
                .child(
                    div()
                        .max_w(rems(14.))
                        .child(Label::new(title).size(LabelSize::Small).truncate()),
                )
        }))
    }

    fn delete_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chats.remove(&id);
        self.tabs.retain(|t| t != &id);
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
            .h(px(32.))
            .flex_none()
            .mt_1()
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
                            .size(LabelSize::Default)
                            .weight(gpui::FontWeight::SEMIBOLD)
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
        if let Some((_, editor, _)) = self
            .renaming
            .as_ref()
            .filter(|(id, _, _)| *id == session.id)
        {
            return div()
                .mx_2()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .border_1()
                .border_color(colors.border_focused)
                .bg(colors.editor_background)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.finish_rename(None, cx);
                    }
                }))
                .child(editor.clone())
                .into_any_element();
        }
        let (button_hover, button_active) = (colors.element_background, colors.element_active);
        let id = session.id.clone();
        let delete_id = session.id.clone();
        let hover_id = session.id.clone();
        let hovered = self.hovered_session.as_deref() == Some(session.id.as_str());
        let row = ThreadItem::new(
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
            let rename_id = session.id.clone();
            this.action_slot(
                h_flex()
                    .gap_0p5()
                    .child(
                        IconButton::new(
                            SharedString::from(format!("rename-{}", session.id)),
                            IconName::Pencil,
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .hover_background(button_hover)
                        .active_background(button_active)
                        .tooltip(Tooltip::text("Rename session"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.start_rename(rename_id.clone(), window, cx)
                        })),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("delete-{}", session.id)),
                            IconName::Trash,
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        // The row behind it is already hovered: Zed's row buttons use these to
                        // stand out.
                        .hover_background(button_hover)
                        .active_background(button_active)
                        .tooltip(Tooltip::text("Delete session"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.delete_session(delete_id.clone(), window, cx)
                        })),
                    ),
            )
        })
        .on_click(cx.listener(move |this, _, window, cx| this.open_session(id.clone(), window, cx)))
        .into_any_element();
        // Right-click: rename, pin, delete.
        let workspace = cx.entity().downgrade();
        let (menu_id, pinned) = (session.id.clone(), session.pinned);
        right_click_menu(SharedString::from(format!("session-menu-{}", session.id)))
            .trigger(move |_, _, _| div().w_full().child(row))
            .menu(move |window, cx| {
                let (workspace, id) = (workspace.clone(), menu_id.clone());
                ContextMenu::build(window, cx, move |menu, _, _| {
                    let (rename, pin, delete) =
                        (workspace.clone(), workspace.clone(), workspace.clone());
                    let (rename_id, pin_id, delete_id) = (id.clone(), id.clone(), id.clone());
                    menu.entry("Rename", None, move |window, cx| {
                        rename
                            .update(cx, |this, cx| {
                                this.start_rename(rename_id.clone(), window, cx)
                            })
                            .ok();
                    })
                    .entry(if pinned { "Unpin" } else { "Pin" }, None, move |_, cx| {
                        pin.update(cx, |this, cx| this.set_pinned(&pin_id, !pinned, cx))
                            .ok();
                    })
                    .separator()
                    .entry("Delete", None, move |window, cx| {
                        delete
                            .update(cx, |this, cx| {
                                this.delete_session(delete_id.clone(), window, cx)
                            })
                            .ok();
                    })
                })
            })
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
        // Pinned sessions first, whatever their project.
        let pinned: Vec<&SessionRecord> = self
            .sessions
            .iter()
            .filter(|s| s.pinned && self.shown(&s.id))
            .collect();
        if !pinned.is_empty() {
            rows.push(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::Pin)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new("Pinned")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
            for session in pinned {
                rows.push(self.render_session_row(session, now, cx));
            }
            rows.push(div().py_1().child(Divider::horizontal()).into_any_element());
        }
        let searching = self.search_hits.is_some();
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
            if group.sessions.is_empty() && !searching {
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
                                .style(ButtonStyle::Outlined)
                                .full_width()
                                .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                                .tooltip(Tooltip::text(new_session_tooltip))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                                ),
                        ),
                    ),
            )
            .child(
                div()
                    .mx_2()
                    .mb_2()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.editor_background)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::MagnifyingGlass)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(div().flex_1().child(self.search.clone())),
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
        window.set_rem_size(crate::appearance::rem_size(cx));
        let tab_bar = (!self.tabs.is_empty()).then(|| self.render_tabs(cx).into_any_element());
        let colors = cx.theme().colors();
        let content: AnyElement = match self.main {
            Main::Settings => self.settings.clone().into_any_element(),
            Main::Memory => self.memory.clone().into_any_element(),
            Main::Skills => self.skills.clone().into_any_element(),
            Main::Onboarding => match &self.onboarding {
                Some((view, _)) => view.clone().into_any_element(),
                None => self.settings.clone().into_any_element(),
            },
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
        // Every tab's content sits under the tab bar (the first-run wizard has none).
        let main: AnyElement = if matches!(self.main, Main::Onboarding) {
            content
        } else {
            v_flex()
                .size_full()
                .children(tab_bar)
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element()
        };
        let settings_open = matches!(self.main, Main::Settings);
        let memory_open = matches!(self.main, Main::Memory);
        let skills_open = matches!(self.main, Main::Skills);
        let dialog = self
            .project_dialog
            .as_ref()
            .map(|(dialog, _)| dialog.clone());
        let palette = self.palette.as_ref().map(|(palette, _, _)| palette.clone());
        let review_border = cx.theme().colors().border;
        let review_shown =
            self.review_open && matches!(self.main, Main::Chat) && self.active.is_some();
        v_flex()
            .relative()
            .size_full()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_session(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(id) = this.active.clone() {
                    this.close_tab(&id, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousTab, window, cx| this.cycle_tab(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_screen(Main::Settings, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMemory, window, cx| {
                this.open_screen(Main::Memory, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSkills, window, cx| {
                this.open_screen(Main::Skills, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &GoToSession, window, cx| {
                this.toggle_session_switcher(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleReview, _, cx| this.toggle_review(cx)))
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
                        IconButton::new("toggle-review", IconName::Diff)
                            .icon_color(if review_shown {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Review Changes (Ctrl+Shift+D)"))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_review(cx))),
                    )
                    // The review pane toggles beside the chat; the buttons after open tabs.
                    .right(div().mx_1().h_4().w_px().bg(review_border))
                    .right(
                        IconButton::new("open-skills", IconName::Book)
                            .icon_color(if skills_open {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Skills (Ctrl+Shift+K)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_screen(Main::Skills, window, cx)
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
                                this.open_screen(Main::Memory, window, cx)
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
                                this.open_screen(Main::Settings, window, cx)
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
                    )
                    .when(review_shown, |this| {
                        this.child(
                            div()
                                .w(px(480.))
                                .flex_none()
                                .h_full()
                                .border_l_1()
                                .border_color(review_border)
                                .child(self.review.clone()),
                        )
                    }),
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
            // Zed's modal layer: the palette near the top; a click elsewhere closes it.
            .when_some(palette, |this, palette| {
                this.child(
                    div()
                        .id("palette-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .pt(px(64.))
                        .occlude()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                        )
                        .child(palette),
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
