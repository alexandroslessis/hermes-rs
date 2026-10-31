//! The main window: title bar, session sidebar, the active chat or settings, and a status bar.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, PathPromptOptions, SharedString, Subscription,
    Task, Window, actions, prelude::*,
};
use hermes_store::SessionRecord;
use tokio::sync::mpsc;
use ui::{
    AgentThreadStatus, Callout, CommonAnimationExt, Divider, ThreadItem, Tooltip, prelude::*,
};

use crate::backend::{UiRequest, backend};
use crate::chat::{ChatEvent, ChatView};
use crate::settings::SettingsView;
use crate::state::AppState;
use crate::title_bar::TitleBar;

actions!(workspace, [NewSession, ToggleSidebar, OpenSettings]);

enum Main {
    Chat,
    Settings,
}

pub struct Workspace {
    sessions: Vec<SessionRecord>,
    chats: HashMap<String, Entity<ChatView>>,
    active: Option<String>,
    main: Main,
    settings: Entity<SettingsView>,
    sidebar_open: bool,
    collapsed: HashSet<String>,
    hovered_session: Option<String>,
    project_roots: RefCell<HashMap<String, PathBuf>>,
    notice: Option<String>,
    focus_handle: FocusHandle,
    _requests: Task<()>,
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
            chats: HashMap::new(),
            active: None,
            main: Main::Chat,
            settings,
            sidebar_open: true,
            collapsed: HashSet::new(),
            hovered_session: None,
            project_roots: RefCell::default(),
            notice: None,
            focus_handle: cx.focus_handle(),
            _requests: requests_task,
            _subscriptions: vec![cx.observe(&state, |_, _, cx| cx.notify())],
        };
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

    fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions = backend(cx)
            .runtime
            .store()
            .list_sessions(200)
            .unwrap_or_default();
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
                cx.subscribe(&chat, |this: &mut Self, chat, event, cx| match event {
                    ChatEvent::Updated => this.chat_updated(chat, cx),
                });
            self._subscriptions.push(subscription);
            self.chats.insert(id.clone(), chat);
        }
        self.active = Some(id.clone());
        self.main = Main::Chat;
        if let Some(chat) = self.chats.get(&id) {
            let composer = chat.read(cx).composer();
            composer.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
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
                let title = title_from(&first.as_concat_text());
                if !title.is_empty() {
                    let _ = backend.runtime.store().set_title(&session_id, &title);
                }
            }
        }
        self.reload_sessions(cx);
    }

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

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_in(None, window, cx);
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
            (state.runtime_provider(), state.config.model.clone())
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
                self.reload_sessions(cx);
                self.open_session(record.id, window, cx);
            }
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the working folder for new sessions".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                AppState::global(cx).update(cx, |state, cx| {
                    state.config.last_cwd = Some(path);
                    state.save(cx);
                });
                this.new_session(window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn delete_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chats.remove(&id);
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

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let now = chrono::Utc::now().timestamp_millis();
        let groups =
            crate::sidebar::group_sessions(&self.sessions, &mut self.project_roots.borrow_mut());
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in groups {
            let key = group.root.display().to_string();
            let collapsed = self.collapsed.contains(&key);
            let running = group.sessions.iter().any(|s| {
                self.chats
                    .get(&s.id)
                    .is_some_and(|chat| chat.read(cx).is_running())
            });
            let header_group = SharedString::from(format!("project-{key}"));
            let toggle_key = key.clone();
            let new_root = group.root.clone();
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("project-header-{key}")))
                    .group(header_group.clone())
                    .cursor_pointer()
                    .h(px(28.))
                    .w_full()
                    .pl_2()
                    .pr_1()
                    .mt_1()
                    .justify_between()
                    .rounded_sm()
                    .hover(|s| s.bg(colors.ghost_element_hover))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                Label::new(group.name.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
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
                            .child(
                                div().visible_on_hover(header_group.clone()).child(
                                    Icon::new(if collapsed {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                                ),
                            ),
                    )
                    .child(
                        div().visible_on_hover(header_group.clone()).child(
                            IconButton::new(
                                SharedString::from(format!("new-in-{key}")),
                                IconName::Plus,
                            )
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text(format!(
                                "New session in {}",
                                group.root.display()
                            )))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.new_session_in(Some(new_root.clone()), window, cx)
                                },
                            )),
                        ),
                    )
                    .tooltip(Tooltip::text(key.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&toggle_key) {
                            this.collapsed.insert(toggle_key.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            for session in group.sessions {
                let id = session.id.clone();
                let delete_id = session.id.clone();
                let hover_id = session.id.clone();
                let running = self
                    .chats
                    .get(&session.id)
                    .is_some_and(|chat| chat.read(cx).is_running());
                let hovered = self.hovered_session.as_deref() == Some(session.id.as_str());
                rows.push(
                    ThreadItem::new(
                        SharedString::from(session.id.clone()),
                        session
                            .title
                            .clone()
                            .unwrap_or_else(|| "New session".into()),
                    )
                    .base_bg(colors.panel_background)
                    .icon(IconName::Chat)
                    .status(if running {
                        AgentThreadStatus::Running
                    } else {
                        AgentThreadStatus::Completed
                    })
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
                            .tooltip(Tooltip::text("Delete session"))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.delete_session(delete_id.clone(), window, cx)
                                },
                            )),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_session(id.clone(), window, cx)
                    }))
                    .into_any_element(),
                );
            }
        }

        let cwd = self.cwd(cx);
        let folder = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        v_flex()
            .w(px(280.))
            .h_full()
            .flex_none()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border)
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .child(
                        Button::new("new-session", "New session")
                            .style(ButtonStyle::Filled)
                            .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                            .full_width()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("choose-folder", format!("in {folder}"))
                            .start_icon(
                                Icon::new(IconName::Folder)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .label_size(LabelSize::Small)
                            .color(Color::Muted)
                            .full_width()
                            .tooltip(Tooltip::text(format!(
                                "{}\nClick to start a session in another folder",
                                cwd.display()
                            )))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    ),
            )
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .id("sessions")
                    .flex_1()
                    .px_1()
                    .pb_2()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let state = AppState::global(cx).read(cx);
        let chat = self
            .active
            .as_ref()
            .and_then(|id| self.chats.get(id))
            .map(|chat| chat.read(cx));
        let mut items: Vec<AnyElement> = Vec::new();
        if let Some(chat) = chat {
            let context = state.context_length(&chat.model);
            items.push(
                Label::new(chat.model.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .into_any_element(),
            );
            if let (Some(context), true) = (context, chat.usage.last_input_tokens > 0) {
                let percent = chat.usage.last_input_tokens as f64 * 100. / context as f64;
                items.push(
                    Label::new(format!("context {percent:.1}% of {}K", context / 1000))
                        .size(LabelSize::XSmall)
                        .color(if percent > 80. {
                            Color::Warning
                        } else {
                            Color::Muted
                        })
                        .into_any_element(),
                );
            }
            items.push(
                Label::new(format!(
                    "{} in / {} out tokens",
                    chat.usage.total_input_tokens, chat.usage.total_output_tokens
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .into_any_element(),
            );
            if chat.usage.cost > 0. {
                items.push(
                    Label::new(format!("${:.4}", chat.usage.cost))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .into_any_element(),
                );
            }
        }
        if let Some(record) = self
            .active
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
        {
            items.push(
                Label::new(record.cwd.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .into_any_element(),
            );
        }
        h_flex()
            .h(px(24.))
            .px_2()
            .gap_4()
            .flex_none()
            .bg(colors.status_bar_background)
            .border_t_1()
            .border_color(colors.border)
            .children(items)
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
                        Button::new("empty-new-session", "Start a new session")
                            .style(ButtonStyle::Filled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_session(window, cx)),
                            ),
                    )
                    .into_any_element(),
            },
        };
        let settings_open = matches!(self.main, Main::Settings);
        let _ = window;
        v_flex()
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
                        IconButton::new("open-settings", IconName::Settings)
                            .icon_color(if settings_open {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .tooltip(Tooltip::text("Settings (Ctrl+,)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.main = match this.main {
                                    Main::Settings => Main::Chat,
                                    Main::Chat => Main::Settings,
                                };
                                if let (Main::Chat, Some(chat)) = (
                                    &this.main,
                                    this.active.as_ref().and_then(|id| this.chats.get(id)),
                                ) {
                                    let composer = chat.read(cx).composer();
                                    composer.focus_handle(cx).focus(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |this| {
                        this.child(self.render_sidebar(cx))
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
            .child(self.render_status_bar(cx))
    }
}
