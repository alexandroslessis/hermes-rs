//! One session's conversation: the message list, the composer, and the running turn.
//! Layout and card styling follow Zed's `agent_ui` thread view (GPL-3.0-or-later).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Empty, Entity, FocusHandle, Focusable, FollowMode,
    ListAlignment, ListOffset, ListState, SharedString, Subscription, Task, Window,
    linear_color_stop, linear_gradient, list, prelude::*, px,
};
use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_runtime::{
    ClarifyAnswer, ClarifyOutcome, ClarifyQuestion, ClarifyRequest, ClarifyResponse, ProcessNotice,
    RuntimeEvent, TurnStatus,
};
use rmcp::model::Role;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use ui::{
    ButtonLike, Callout, CircularProgress, CommonAnimationExt, ContextMenu, ContextMenuEntry,
    CopyButton, Disclosure, Divider, LoadingLabel, PopoverMenu, PopoverMenuHandle, SpinnerVariant,
    SplitButton, SplitButtonStyle, TintColor, Tooltip, prelude::*, right_click_menu,
};

use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownOptions, MarkdownStyle};

use hermes_tools::{TodoItem, TodoStatus};

use crate::backend::backend;
use crate::chat_ui;
use crate::config;
use crate::model_picker::ModelPicker;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

const MAX_RESULT_CHARS: usize = 6000;

pub enum ChatEvent {
    /// The session's stored state changed (new messages, title-worthy first message).
    Updated,
}

struct ClarifyForm {
    questions: Vec<ClarifyQuestion>,
    selected: Vec<Vec<bool>>,
    other: Vec<Entity<TextArea>>,
    reply: Option<oneshot::Sender<ClarifyOutcome>>,
}

#[derive(Default, Clone, Copy)]
pub struct TurnUsage {
    pub last_input_tokens: i32,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub cost: f64,
}

pub struct ChatView {
    pub session_id: String,
    pub model: String,
    /// The session's working folder; tool labels show paths inside it relative to it.
    cwd: Option<String>,
    conversation: Conversation,
    /// Indices into the conversation of the messages that render as list entries.
    entries: Vec<usize>,
    list_state: ListState,
    composer: Entity<TextArea>,
    running: Option<CancellationToken>,
    turn_task: Option<Task<()>>,
    expanded: HashSet<String>,
    /// Rendered markdown per assistant text block, keyed by (message index, block index).
    markdown: HashMap<MarkdownKey, Entity<Markdown>>,
    /// The code-block languages `markdown` was built with.
    languages: Arc<markdown::language::LanguageRegistry>,
    clarify: Option<ClarifyForm>,
    /// `/steer` text waiting for the agent's next tool call to finish.
    steering: Option<String>,
    /// The user message being edited in the composer; sending rewinds the conversation there.
    editing: Option<usize>,
    /// Skills matching a `/command` being typed: (name, short description).
    slash_matches: Vec<(String, String)>,
    /// A steer the turn ended before delivering: Hermes sends it as the next message.
    next_message: Option<String>,
    /// Thinking blocks the user opened or closed by hand (Zed's auto mode leaves the rest).
    thinking_toggled: HashSet<String>,
    composer_expanded: bool,
    effort_menu: PopoverMenuHandle<ContextMenu>,
    model_menu: PopoverMenuHandle<ModelPicker>,
    pub usage: TurnUsage,
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl gpui::EventEmitter<ChatEvent> for ChatView {}

impl ChatView {
    pub fn new(
        session_id: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let backend = backend(cx);
        let messages = backend.runtime.messages(&session_id).unwrap_or_default();
        let totals = backend
            .runtime
            .store()
            .usage_totals(&session_id)
            .unwrap_or_default();
        let cwd = backend
            .runtime
            .store()
            .session(&session_id)
            .ok()
            .flatten()
            .map(|session| session.cwd);
        let composer = cx.new(|cx| TextArea::new(COMPOSER_PLACEHOLDER, cx));
        let subscription = cx.subscribe_in(
            &composer,
            window,
            |this: &mut Self, _, event, window, cx| match event {
                TextAreaEvent::Submit => this.submit(window, cx),
                TextAreaEvent::Changed => {
                    this.update_slash_matches(cx);
                    // The send button follows whether the composer is empty.
                    cx.notify();
                }
            },
        );
        let list_state = ListState::new(0, ListAlignment::Top, px(2000.));
        list_state.set_follow_mode(FollowMode::Tail);
        let mut this = Self {
            session_id,
            model,
            cwd,
            conversation: Conversation::new_unvalidated(messages),
            entries: Vec::new(),
            list_state,
            composer,
            running: None,
            turn_task: None,
            expanded: HashSet::new(),
            markdown: HashMap::new(),
            clarify: None,
            steering: None,
            editing: None,
            slash_matches: Vec::new(),
            next_message: None,
            thinking_toggled: HashSet::new(),
            composer_expanded: false,
            effort_menu: PopoverMenuHandle::default(),
            model_menu: PopoverMenuHandle::default(),
            usage: TurnUsage {
                last_input_tokens: i32::try_from(totals.last_input_tokens).unwrap_or(i32::MAX),
                total_input_tokens: totals.input_tokens,
                total_output_tokens: totals.output_tokens,
                cost: totals.cost.unwrap_or(0.),
            },
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![
                subscription,
                // A theme change recolors code blocks, so cached markdown is rebuilt.
                cx.observe(&AppState::global(cx), |this: &mut Self, state, cx| {
                    let languages = &state.read(cx).languages;
                    if !Arc::ptr_eq(languages, &this.languages) {
                        this.languages = languages.clone();
                        this.markdown.clear();
                        cx.notify();
                    }
                }),
            ],
            languages: AppState::global(cx).read(cx).languages.clone(),
        };
        this.rebuild_entries();
        this.list_state.reset(this.list_len());
        this.list_state.scroll_to_end();
        this
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn composer(&self) -> Entity<TextArea> {
        self.composer.clone()
    }

    /// The list's rows: one per entry, plus the trailer that shows the turn running.
    fn list_len(&self) -> usize {
        self.entries.len() + 1
    }

    /// Re-renders the trailer row (the generating indicator).
    fn trailer_changed(&mut self, cx: &mut Context<Self>) {
        let trailer = self.entries.len();
        self.list_state.splice(trailer..trailer + 1, 1);
        cx.notify();
    }

    fn rebuild_entries(&mut self) {
        self.entries = self
            .conversation
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, message)| message.is_user_visible())
            .filter(|(_, message)| {
                message
                    .content
                    .iter()
                    .any(|content| !matches!(content, MessageContent::ToolResponse(_)))
            })
            .map(|(index, _)| index)
            .collect();
    }

    /// Re-renders after the conversation changed; the last entry is remeasured (it may be streaming).
    fn entries_changed(&mut self, cx: &mut Context<Self>) {
        let old = self.entries.len();
        self.rebuild_entries();
        let new = self.entries.len();
        let start = old.saturating_sub(1).min(new);
        // The trailer row after the entries (the generating indicator) moves with them.
        self.list_state.splice(start..old + 1, new + 1 - start);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.running.is_none() {
            if let Some(index) = self.editing.take() {
                self.composer
                    .update(cx, |composer, cx| composer.set_text("", cx));
                self.resend(index, Some(text), cx);
                return;
            }
        }
        // Mid-turn, Enter steers the running turn (Hermes's `/steer`).
        if self.running.is_some() {
            if backend(cx).runtime.steer(&self.session_id, &text) {
                self.composer
                    .update(cx, |composer, cx| composer.set_text("", cx));
                self.steering = Some(match self.steering.take() {
                    Some(earlier) => format!("{earlier} / {text}"),
                    None => text,
                });
                cx.notify();
            }
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.set_text("", cx));
        self.start_turn(text, window, cx);
    }

    pub fn start_turn(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.begin(Some(text), cx);
    }

    /// While the composer holds a bare `/word`, the skills whose name contains it.
    fn update_slash_matches(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().to_string();
        let query = text
            .strip_prefix('/')
            .filter(|q| !q.contains(char::is_whitespace))
            .map(str::to_lowercase);
        let matches = match query {
            Some(query) => backend(cx)
                .runtime
                .skills()
                .catalog()
                .into_iter()
                .filter(|s| s.name.contains(&query))
                .take(6)
                .map(|s| {
                    (
                        s.name,
                        hermes_tools::skills::clip_description(&s.description),
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        if matches != self.slash_matches {
            self.slash_matches = matches;
            cx.notify();
        }
    }

    /// Hermes's edit-and-resend: the conversation rewinds to just before user message
    /// `message_index`, then that message (or `text`, its edited version) is sent again.
    fn resend(&mut self, message_index: usize, text: Option<String>, cx: &mut Context<Self>) {
        let text =
            text.unwrap_or_else(|| self.conversation.messages()[message_index].as_concat_text());
        match backend(cx).runtime.rewind(&self.session_id, message_index) {
            Ok(messages) => {
                self.conversation = Conversation::new_unvalidated(messages);
                self.markdown.clear();
                self.rebuild_entries();
                self.list_state.reset(self.list_len());
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        }
        self.begin(Some(text), cx);
    }

    /// Puts a user message in the composer; sending it rewinds the conversation there.
    fn edit_message(&mut self, message_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.conversation.messages()[message_index].as_concat_text();
        self.composer
            .update(cx, |composer, cx| composer.set_text(text, cx));
        self.composer.focus_handle(cx).focus(window, cx);
        self.editing = Some(message_index);
        cx.notify();
    }

    /// Hermes's `/retry`: run the failed turn again.
    fn retry(&mut self, cx: &mut Context<Self>) {
        self.begin(None, cx);
    }

    /// Starts a turn with a new message, or retries the failed one (`text` is `None`).
    fn begin(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let (provider, options) = {
            let state = state.read(cx);
            (
                state
                    .provider_for_model(&self.model)
                    .map(config::runtime_provider),
                hermes_runtime::TurnOptions {
                    context_length: state.context_length(&self.model),
                    effort: state.config.efforts.get(&self.model).cloned(),
                },
            )
        };
        let Some(provider) = provider else {
            self.error = Some("No provider is configured. Open Settings to add one.".into());
            cx.notify();
            return;
        };
        let backend = backend(cx);
        // `/skill-name instruction` sends the message that loads the skill.
        let text = match text {
            Some(text) if text.starts_with('/') => match backend.runtime.skill_command(&text) {
                Some(message) => Some(message),
                None => {
                    let name = text[1..]
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    self.error = Some(format!(
                        "There is no skill named '{name}'. Open Skills (Ctrl+Shift+K) to see them."
                    ));
                    cx.notify();
                    return;
                }
            },
            other => other,
        };
        let turn = {
            let _guard = backend.tokio.enter();
            match &text {
                Some(text) => {
                    backend
                        .runtime
                        .start_turn(&self.session_id, &provider, options, text)
                }
                None => backend
                    .runtime
                    .retry_turn(&self.session_id, &provider, options),
            }
        };
        let mut turn = match turn {
            Ok(turn) => turn,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.running = Some(turn.cancel.clone());
        self.composer.update(cx, |composer, cx| {
            composer.set_placeholder(STEER_PLACEHOLDER, cx)
        });
        self.trailer_changed(cx);
        self.list_state.scroll_to_end();
        self.turn_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = turn.events.recv().await {
                let finished = matches!(event, RuntimeEvent::TurnFinished(_));
                if this
                    .update(cx, |this, cx| this.handle_event(event, cx))
                    .is_err()
                    || finished
                {
                    break;
                }
            }
        }));
        cx.emit(ChatEvent::Updated);
        cx.notify();
    }

    /// Switches this chat's model (and the active provider to the one serving it); it becomes the default for new chats (Hermes's
    /// composer picker is sticky the same way).
    fn switch_model(&mut self, provider: String, model: String, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        state.update(cx, |state, cx| {
            state.config.provider = Some(provider);
            state.config.model = Some(model.clone());
            state.save(cx);
        });
        let context_length = state.read(cx).context_length(&model);
        if model == self.model {
            return;
        }
        match backend(cx)
            .runtime
            .set_session_model(&self.session_id, &model, context_length)
        {
            Ok(()) => {
                self.model = model;
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.emit(ChatEvent::Updated);
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.running {
            cancel.cancel();
        }
        if let Some(mut form) = self.clarify.take() {
            if let Some(reply) = form.reply.take() {
                let _ = reply.send(ClarifyOutcome::Cancelled);
            }
        }
        cx.notify();
    }

    fn handle_event(&mut self, event: RuntimeEvent, cx: &mut Context<Self>) {
        match event {
            RuntimeEvent::Agent(AgentEvent::Message(message)) => {
                // The chat's copy of a delivered steer: shown to the user, not sent to the model.
                if message.role == Role::User && !message.is_agent_visible() {
                    self.steering = None;
                }
                self.conversation.push(message);
                self.entries_changed(cx);
            }
            RuntimeEvent::Agent(AgentEvent::HistoryReplaced(conversation)) => {
                self.conversation = conversation;
                self.rebuild_entries();
                self.list_state.reset(self.list_len());
                cx.notify();
            }
            RuntimeEvent::Agent(AgentEvent::Usage(usage)) => {
                self.usage.last_input_tokens = usage.usage.input_tokens.unwrap_or(0);
                self.usage.total_input_tokens += i64::from(usage.usage.input_tokens.unwrap_or(0));
                self.usage.total_output_tokens += i64::from(usage.usage.output_tokens.unwrap_or(0));
                self.usage.cost += usage.cost.unwrap_or(0.);
                cx.notify();
            }
            RuntimeEvent::Agent(_) => {}
            RuntimeEvent::SteerUndelivered(text) => {
                self.steering = None;
                self.next_message = Some(text);
            }
            RuntimeEvent::TurnFinished(status) => {
                self.running = None;
                self.composer.update(cx, |composer, cx| {
                    composer.set_placeholder(COMPOSER_PLACEHOLDER, cx)
                });
                if let Some(text) = self.next_message.take() {
                    cx.spawn(async move |this, cx| {
                        this.update(cx, |this, cx| this.begin(Some(text), cx)).ok();
                    })
                    .detach();
                }
                if let TurnStatus::Failed(error) = status {
                    self.error = Some(error);
                }
                // Reload from the runtime: it holds the canonical merged conversation.
                if let Ok(messages) = backend(cx).runtime.messages(&self.session_id) {
                    self.conversation = Conversation::new_unvalidated(messages);
                    self.entries_changed(cx);
                }
                cx.emit(ChatEvent::Updated);
                cx.notify();
            }
        }
    }

    pub fn show_clarify(
        &mut self,
        request: ClarifyRequest,
        reply: oneshot::Sender<ClarifyOutcome>,
        cx: &mut Context<Self>,
    ) {
        let other = request
            .questions
            .iter()
            .map(|question| {
                let placeholder = if question.choices.is_empty() {
                    "Your answer"
                } else {
                    "Other…"
                };
                cx.new(|cx| TextArea::new(placeholder, cx))
            })
            .collect();
        self.clarify = Some(ClarifyForm {
            selected: request
                .questions
                .iter()
                .map(|q| vec![false; q.choices.len()])
                .collect(),
            questions: request.questions,
            other,
            reply: Some(reply),
        });
        cx.notify();
    }

    fn answer_clarify(&mut self, submit: bool, cx: &mut Context<Self>) {
        let Some(mut form) = self.clarify.take() else {
            return;
        };
        let Some(reply) = form.reply.take() else {
            return;
        };
        if !submit {
            let _ = reply.send(ClarifyOutcome::Cancelled);
            cx.notify();
            return;
        }
        let responses = form
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let picked: Vec<String> = question
                    .choices
                    .iter()
                    .zip(&form.selected[index])
                    .filter(|(_, selected)| **selected)
                    .map(|(choice, _)| choice.clone())
                    .collect();
                let other = form.other[index].read(cx).text().trim().to_string();
                match (picked.is_empty(), other.is_empty(), question.multi_select) {
                    (true, true, _) => ClarifyResponse::Skipped,
                    (true, false, _) => ClarifyResponse::Answered(ClarifyAnswer::Text(other)),
                    (false, _, true) => {
                        let mut all = picked;
                        if !other.is_empty() {
                            all.push(other);
                        }
                        ClarifyResponse::Answered(ClarifyAnswer::Choices(all))
                    }
                    (false, _, false) => {
                        ClarifyResponse::Answered(ClarifyAnswer::Text(picked[0].clone()))
                    }
                }
            })
            .collect();
        let _ = reply.send(ClarifyOutcome::Submitted(responses));
        cx.notify();
    }

    /// A background process the agent started with notify=true finished: tell the agent.
    pub fn process_finished(
        &mut self,
        notice: ProcessNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = format!(
            "[Background process {} finished with exit code {}]\nCommand: {}\nLast output:\n{}",
            notice.process_id,
            notice
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into()),
            notice.command,
            notice.tail
        );
        if self.running.is_none() {
            self.start_turn(text, window, cx);
        }
    }

    // --- rendering -------------------------------------------------------------------------

    fn tool_responses(&self) -> HashMap<&str, (String, bool)> {
        self.conversation
            .messages()
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| {
                let MessageContent::ToolResponse(response) = content else {
                    return None;
                };
                let (text, is_error) = match &response.tool_result {
                    Ok(result) => (tool_output(content), result.is_error.unwrap_or(false)),
                    Err(error) => (error.message.to_string(), true),
                };
                Some((response.id.as_str(), (text, is_error)))
            })
            .collect()
    }

    /// The whole thread as plain text: each message under its role, tool calls with their
    /// input and output. Selection stops at a block's edge, so this is how to copy it all.
    fn thread_text(&self) -> String {
        let responses = self.tool_responses();
        let mut parts = Vec::new();
        for &index in &self.entries {
            let message = &self.conversation.messages()[index];
            let mut body = Vec::new();
            for content in &message.content {
                match content {
                    MessageContent::Text(text) => body.push(text.text.clone()),
                    MessageContent::Thinking(thinking) => {
                        body.push(format!("[Thinking]\n{}", thinking.thinking))
                    }
                    MessageContent::ToolRequest(request) => {
                        let Ok(call) = &request.tool_call else {
                            continue;
                        };
                        let args = call
                            .arguments
                            .clone()
                            .map(serde_json::Value::Object)
                            .unwrap_or_default();
                        let mut text = format!(
                            "[Tool: {}]\n{}",
                            call.name,
                            serde_json::to_string_pretty(&args).unwrap_or_default()
                        );
                        if let Some((output, _)) = responses.get(request.id.as_str()) {
                            text.push_str("\n[Output]\n");
                            text.push_str(output);
                        }
                        body.push(text);
                    }
                    _ => {}
                }
            }
            let role = match message.role {
                Role::User => "User",
                Role::Assistant => "Hermes",
            };
            parts.push(format!("## {role}\n\n{}", body.join("\n\n")));
        }
        parts.join("\n\n")
    }

    fn render_entry(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The row after the last message: Zed's generating indicator while a turn runs.
        let Some(&message_index) = self.entries.get(index) else {
            return if self.running.is_some() {
                self.render_generating().into_any_element()
            } else {
                Empty.into_any_element()
            };
        };
        let message = self.conversation.messages()[message_index].clone();
        let is_last = index + 1 == self.entries.len();
        let content = match message.role {
            Role::User => self.render_user_message(&message, message_index, window, cx),
            Role::Assistant => {
                let body =
                    self.render_assistant_message(&message, message_index, index, window, cx);
                v_flex()
                    .px_5()
                    .py_1p5()
                    .when(is_last, |this| this.pb_4())
                    .w_full()
                    .text_ui(cx)
                    .child(body)
                    .into_any_element()
            }
        };
        // Zed's thread controls close every finished turn: the last agent message before the
        // next user message (or the end).
        let turn_end = message.role == Role::Assistant
            && self
                .entries
                .get(index + 1)
                .is_none_or(|&next| self.conversation.messages()[next].role == Role::User)
            && !(is_last && self.running.is_some());
        let content = if turn_end {
            v_flex()
                .w_full()
                .child(content)
                .child(self.render_thread_controls(index, is_last, cx))
                .into_any_element()
        } else {
            content
        };
        let chat = cx.entity().downgrade();
        let whole = message.as_concat_text();
        right_click_menu(SharedString::from(format!("entry-menu-{message_index}")))
            .trigger(move |_, _, _| div().w_full().child(content))
            .menu(move |window, cx| {
                // The markdown blocks of this message saw the right-click first and recorded
                // what was selected in them.
                let selected = chat.upgrade().and_then(|chat| {
                    chat.read(cx)
                        .markdown
                        .iter()
                        .filter(|(key, _)| key.0 == message_index)
                        .find_map(|(_, markdown)| {
                            let markdown = markdown.read(cx);
                            Some((
                                markdown.context_menu_selected_text()?.clone(),
                                markdown.context_menu_selected_markdown().cloned(),
                            ))
                        })
                });
                let whole = whole.clone();
                let chat = chat.clone();
                ContextMenu::build(window, cx, move |menu, _, _| {
                    let menu = match selected.clone() {
                        Some((text, markdown)) => {
                            let menu = menu.entry("Copy", None, move |_, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()))
                            });
                            match markdown {
                                Some(markdown) => {
                                    menu.entry("Copy as Markdown", None, move |_, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            markdown.to_string(),
                                        ))
                                    })
                                }
                                None => menu,
                            }
                            .separator()
                        }
                        None => menu,
                    };
                    let whole = whole.clone();
                    let chat = chat.clone();
                    menu.entry("Copy Message", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(whole.clone()))
                    })
                    .entry("Copy Thread", None, move |_, cx| {
                        if let Some(chat) = chat.upgrade() {
                            let text = chat.read(cx).thread_text();
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    })
                })
            })
            .into_any_element()
    }

    /// Zed's user message: a full-width card with the editor background, a shadow and a focus
    /// border on hover; its actions float over the top-right corner.
    fn render_user_message(
        &mut self,
        message: &Message,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = message.as_concat_text();
        // A skill command: show what was typed, not the skill it loaded.
        let shown = match hermes_tools::skills::describe_invocation(&text) {
            Some((skill, instruction)) => format!("/{skill} {instruction}").trim_end().to_string(),
            None => text.clone(),
        };
        let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        let body = self.markdown_block((message_index, 0, MarkdownKind::User), &shown, style, cx);
        let colors = cx.theme().colors();
        let focus_border = colors.border_focused;
        let running = self.running.is_some();
        let group = SharedString::from(format!("user-message-{message_index}"));
        let action = |id: &str, icon: IconName, tooltip: &'static str| {
            IconButton::new(SharedString::from(format!("{id}-{message_index}")), icon)
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text(tooltip))
        };
        let copy_text = shown.clone();
        v_flex()
            .id(("user_message", message_index))
            .pt_2()
            .pb_3()
            .px_2()
            .gap_1p5()
            .w_full()
            .child(
                div()
                    .relative()
                    .group(group.clone())
                    .child(
                        div()
                            .py_3()
                            .px_2()
                            .rounded_md()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .shadow_md()
                            .hover(|style| style.border_color(focus_border.opacity(0.8)))
                            .child(body),
                    )
                    .child(
                        h_flex()
                            .absolute()
                            .top_neg_3p5()
                            .right_3()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .overflow_hidden()
                            .visible_on_hover(group)
                            .child(action("copy-message", IconName::Copy, "Copy").on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_text.clone(),
                                    ))
                                },
                            ))
                            .child(
                                action("edit-message", IconName::Pencil, "Edit and Resend")
                                    .disabled(running)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.edit_message(message_index, window, cx)
                                    })),
                            )
                            .child(
                                action(
                                    "resend-message",
                                    IconName::Return,
                                    "Resend: restarts the thread from this point",
                                )
                                .disabled(running)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.resend(message_index, None, cx),
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The markdown entity for one text block, created or updated to `text` as it streams.
    fn markdown_for(
        &mut self,
        key: MarkdownKey,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Entity<Markdown> {
        if let Some(markdown) = self.markdown.get(&key) {
            let markdown = markdown.clone();
            if markdown.read(cx).source() != text {
                let text = text.to_string();
                markdown.update(cx, |markdown, cx| markdown.replace(text, cx));
            }
            return markdown;
        }
        let languages = AppState::global(cx).read(cx).languages.clone();
        let options = MarkdownOptions {
            render_mermaid_diagrams: true,
            ..Default::default()
        };
        let markdown = cx.new(|cx| match key.2 {
            // What the user typed and what tools took and returned show as written.
            MarkdownKind::User
            | MarkdownKind::ToolInput
            | MarkdownKind::ToolOutput
            | MarkdownKind::TerminalCommand
            | MarkdownKind::TerminalOutput => Markdown::new_text(text.to_string().into(), cx),
            _ => Markdown::new_with_options(
                text.to_string().into(),
                Some(languages),
                None,
                options,
                cx,
            ),
        });
        self.markdown.insert(key, markdown.clone());
        markdown
    }

    /// Selectable text (drag to select, Ctrl+C or right-click to copy). Each block gets its own
    /// element id: the markdown renderer keys code blocks, tables and checkboxes by offset or
    /// text, which repeat across messages, and duplicate ids crash the accessibility tree.
    fn markdown_block(
        &mut self,
        key: MarkdownKey,
        text: &str,
        style: MarkdownStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let markdown = self.markdown_for(key, text, cx);
        div()
            .id(SharedString::from(format!(
                "markdown-{}-{}-{:?}",
                key.0, key.1, key.2
            )))
            .w_full()
            .child(MarkdownElement::new(markdown, style))
            .into_any_element()
    }

    fn render_assistant_message(
        &mut self,
        message: &Message,
        message_index: usize,
        entry: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let responses = self.tool_responses();
        let responses: HashMap<String, (String, bool)> = responses
            .into_iter()
            .map(|(id, value)| (id.to_string(), value))
            .collect();
        let mut blocks: Vec<AnyElement> = Vec::new();
        // The block being streamed right now: last content of the last entry while running.
        let streaming_chunk = (self.running.is_some() && entry + 1 == self.entries.len())
            .then(|| message.content.len().saturating_sub(1));
        for (chunk, content) in message.content.iter().enumerate() {
            match content {
                MessageContent::Text(text) if !text.text.trim().is_empty() => {
                    let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
                    blocks.push(self.markdown_block(
                        (message_index, chunk, MarkdownKind::Text),
                        &text.text,
                        style,
                        cx,
                    ));
                }
                MessageContent::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                    let active = streaming_chunk == Some(chunk);
                    blocks.push(self.render_thinking(
                        message_index,
                        chunk,
                        &thinking.thinking,
                        active,
                        window,
                        cx,
                    ));
                }
                MessageContent::ToolRequest(request) => {
                    let response = responses.get(&request.id).cloned();
                    blocks.push(self.render_tool_call(
                        message_index,
                        chunk,
                        request,
                        response,
                        window,
                        cx,
                    ));
                }
                MessageContent::Error(error) => {
                    let can_retry = self.running.is_none() && entry + 1 == self.entries.len();
                    blocks.push(
                        Callout::new()
                            .severity(Severity::Error)
                            .icon(IconName::XCircle)
                            .title("The provider returned an error")
                            .description(error.message.clone())
                            .when(can_retry, |callout| {
                                callout.actions_slot(
                                    Button::new(
                                        SharedString::from(format!("retry-{entry}")),
                                        "Retry",
                                    )
                                    .style(ButtonStyle::Filled)
                                    .start_icon(Icon::new(IconName::Rerun).size(IconSize::Small))
                                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                                )
                            })
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }
        v_flex()
            .w_full()
            .gap_3()
            .children(blocks)
            .into_any_element()
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        // Expanding changes the entry's height.
        self.list_state.reset(self.list_len());
        cx.notify();
    }

    /// Zed's thinking block in its default "auto" display: a plain "Thinking" header (the
    /// chevron shows on hover), opened while the thought streams and closed when it ends,
    /// unless the user opened or closed it by hand.
    fn render_thinking(
        &mut self,
        message_index: usize,
        chunk: usize,
        text: &str,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("thinking-{message_index}-{chunk}");
        let open = if self.thinking_toggled.contains(&key) {
            self.expanded.contains(&key)
        } else {
            active
        };
        let body = open.then(|| {
            let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
            self.markdown_block(
                (message_index, chunk, MarkdownKind::Thinking),
                text.trim(),
                style,
                cx,
            )
        });
        let colors = cx.theme().colors();
        let card_header = SharedString::from(format!("{key}-card-header"));
        let toggle = {
            let key = key.clone();
            move |this: &mut Self, cx: &mut Context<Self>| {
                this.thinking_toggled.insert(key.clone());
                if open {
                    this.expanded.remove(&key);
                } else {
                    this.expanded.insert(key.clone());
                }
                this.list_state.reset(this.list_len());
                cx.notify();
            }
        };
        let header_toggle = toggle.clone();
        v_flex()
            .id(SharedString::from(format!("thinking-block-{key}")))
            .gap_1()
            .child(
                h_flex()
                    .id(SharedString::from(format!("thinking-block-header-{key}")))
                    .group(card_header.clone())
                    .relative()
                    .w_full()
                    .pr_1()
                    .justify_between()
                    .child(
                        h_flex()
                            .h(window.line_height() - px(2.))
                            .gap_1p5()
                            .overflow_hidden()
                            .child(
                                Icon::new(IconName::ToolThink)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                div()
                                    .text_size(rems_from_px(13_f32))
                                    .text_color(colors.text_muted)
                                    .child("Thinking"),
                            ),
                    )
                    .child(
                        Disclosure::new(SharedString::from(format!("{key}-expand")), open)
                            .opened_icon(IconName::ChevronUp)
                            .closed_icon(IconName::ChevronDown)
                            .visible_on_hover(card_header)
                            .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| header_toggle(this, cx))),
            )
            .when_some(body, |this, body| {
                this.child(
                    div()
                        .ml_1p5()
                        .pl_3p5()
                        .border_l_1()
                        .border_color(tool_card_border_color(cx))
                        .overflow_hidden()
                        .child(body),
                )
            })
            .into_any_element()
    }

    /// Zed's tool call: a plain row (icon, muted label, a fade at the right edge) that expands
    /// into its input and output; edits get a card, and terminal commands Zed's terminal card.
    fn render_tool_call(
        &mut self,
        message_index: usize,
        chunk: usize,
        request: &hermes_llm::conversation::message::ToolRequest,
        response: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (name, args) = match &request.tool_call {
            Ok(call) => (
                call.name.to_string(),
                call.arguments
                    .clone()
                    .map(serde_json::Value::Object)
                    .unwrap_or_default(),
            ),
            Err(error) => (
                "invalid tool call".to_string(),
                serde_json::json!({"error": error.message}),
            ),
        };
        if name == "terminal" {
            return self.render_terminal_tool_call(
                message_index,
                chunk,
                request,
                &args,
                response,
                window,
                cx,
            );
        }
        let key = format!("tool-{}", request.id);
        let open = self.expanded.contains(&key);
        let failed = response.as_ref().is_some_and(|(_, failed)| *failed);
        let is_edit = matches!(name.as_str(), "write_file" | "patch");
        let use_card_layout = is_edit;
        let card_header = SharedString::from(format!("inner-tool-call-header-{}", request.id));
        let label_text = chat_ui::tool_label(&name, &args, self.cwd.as_deref());
        let label_style = MarkdownStyle {
            prevent_mouse_interaction: true,
            ..MarkdownStyle::themed(MarkdownFont::Agent, window, cx).with_muted_text(cx)
        };
        let label = self.markdown_block(
            (message_index, chunk, MarkdownKind::ToolLabel),
            &label_text,
            label_style,
            cx,
        );
        let header_bg = tool_card_header_bg(cx);
        let panel_bg = cx.theme().colors().panel_background;
        let fade_bg = if use_card_layout { header_bg } else { panel_bg };
        let gradient_overlay =
            div()
                .absolute()
                .top_0()
                .right_0()
                .w_12()
                .h_full()
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(fade_bg, 1.),
                    linear_color_stop(fade_bg.opacity(0.2), 0.),
                ));
        let label_row = h_flex()
            .relative()
            .w_full()
            .h(window.line_height() - px(2.))
            .text_size(rems_from_px(13_f32))
            .gap_1p5()
            .when(use_card_layout, |this| this.px_1())
            .overflow_hidden()
            .child(
                div().flex_none().child(
                    Icon::new(tool_icon(&name))
                        .size(IconSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(h_flex().w_full().whitespace_nowrap().child(label))
            .when(!is_edit, |this| this.child(gradient_overlay));
        let toggle_key = key.clone();
        let header = h_flex()
            .id(SharedString::from(format!("{key}-header")))
            .group(card_header.clone())
            .relative()
            .w_full()
            .justify_between()
            .cursor_pointer()
            .when(use_card_layout, |this| {
                this.p_0p5().rounded_t(rems_from_px(5_f32)).bg(header_bg)
            })
            .child(label_row)
            .child(
                h_flex()
                    .pr_0p5()
                    .gap_1()
                    .when(response.is_some(), |this| {
                        this.child(
                            Disclosure::new(SharedString::from(format!("{key}-expand")), open)
                                .opened_icon(IconName::ChevronUp)
                                .closed_icon(IconName::ChevronDown)
                                .visible_on_hover(card_header.clone()),
                        )
                    })
                    .when(failed, |this| {
                        this.child(
                            Icon::new(IconName::Close)
                                .color(Color::Error)
                                .size(IconSize::Small),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle_key.clone(), cx)));

        let border_color = tool_card_border_color(cx);
        let output = match open.then_some(response.as_ref()).flatten() {
            Some((text, _)) => {
                let (fields, diff) = result_parts(text);
                let mono = mono_style(window, cx);
                v_flex()
                    .gap_1()
                    .when(!fields.is_empty(), |this| {
                        this.child(self.markdown_block(
                            (message_index, chunk, MarkdownKind::ToolOutput),
                            &fields,
                            mono.clone(),
                            cx,
                        ))
                    })
                    .when_some(diff, |this, diff| {
                        this.child(self.markdown_block(
                            (message_index, chunk, MarkdownKind::ToolDiff),
                            &fenced("diff", &diff),
                            mono,
                            cx,
                        ))
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        };
        let input = open.then(|| {
            let args_text = clip(&serde_json::to_string_pretty(&args).unwrap_or_default());
            let style = mono_style(window, cx);
            self.markdown_block(
                (message_index, chunk, MarkdownKind::ToolInput),
                &args_text,
                style,
                cx,
            )
        });
        let input_output_header = |text: &'static str| {
            Label::new(text)
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .buffer_font(cx)
        };
        let mono = theme::theme_settings(cx).buffer_font(cx).clone();
        let collapse_key = key.clone();
        let content = open.then(|| {
            if use_card_layout {
                v_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(border_color)
                    .font(mono.clone())
                    .text_xs()
                    .child(output)
                    .into_any_element()
            } else {
                v_flex()
                    .mt_1p5()
                    .w_full()
                    .child(
                        v_flex()
                            .ml(rems(0.4))
                            .px_3p5()
                            .pb_1()
                            .gap_1()
                            .border_l_1()
                            .border_color(border_color)
                            .child(input_output_header("Raw Input:"))
                            .children(input)
                            .child(input_output_header("Output:"))
                            .child(output),
                    )
                    .child(
                        div()
                            .ml(rems(0.4))
                            .px_3p5()
                            .pt_2()
                            .border_l_1()
                            .border_color(border_color)
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("tool_output-collapse-{key}")),
                                    IconName::ChevronUp,
                                )
                                .full_width()
                                .style(ButtonStyle::Outlined)
                                .icon_color(Color::Muted)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.toggle(collapse_key.clone(), cx),
                                )),
                            ),
                    )
                    .into_any_element()
            }
        });

        v_flex()
            .map(|this| {
                if use_card_layout {
                    this.my_1p5()
                        .rounded_md()
                        .border_1()
                        .when(failed, |this| this.border_dashed())
                        .border_color(border_color)
                        .bg(cx.theme().colors().editor_background)
                        .overflow_hidden()
                } else {
                    this.my_1()
                }
            })
            .child(header)
            .children(content)
            .into_any_element()
    }

    /// Zed's terminal card (`TerminalToolHeader` + `render_collapsible_command`): the working
    /// directory over the command, a spinner while it runs, its output when expanded.
    fn render_terminal_tool_call(
        &mut self,
        message_index: usize,
        chunk: usize,
        request: &hermes_llm::conversation::message::ToolRequest,
        args: &serde_json::Value,
        response: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = request.id.clone();
        let key = format!("tool-{id}");
        let is_expanded = self.expanded.contains(&key);
        let running = response.is_none() && self.running.is_some();
        let result: Option<serde_json::Value> = response
            .as_ref()
            .and_then(|(text, _)| serde_json::from_str(text).ok());
        let exit_code = result
            .as_ref()
            .and_then(|r| r.get("exit_code"))
            .and_then(|c| c.as_i64());
        let failed = response.as_ref().is_some_and(|(_, failed)| *failed)
            || exit_code.is_some_and(|code| code != 0);
        let command = args
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let working_dir = args
            .get("workdir")
            .and_then(|w| w.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "terminal".into());
        let border_color = tool_card_border_color(cx);
        let header_bg = tool_card_header_bg(cx);
        let group = SharedString::from(format!("terminal-tool-header-group-{id}"));
        let child_id = |name: &str| SharedString::from(format!("terminal-tool-{name}-{id}"));
        let toggle_key = key.clone();

        let header_row = h_flex()
            .id(child_id("header"))
            .pt_1()
            .pl_1p5()
            .pr_1()
            .flex_none()
            .gap_1()
            .justify_between()
            .rounded_t_md()
            .cursor_pointer()
            .child(
                div().w_full().min_w_0().overflow_hidden().child(
                    Label::new(working_dir)
                        .buffer_font(cx)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate_start(),
                ),
            )
            .child(
                Disclosure::new(child_id("disclosure"), is_expanded)
                    .opened_icon(IconName::ChevronUp)
                    .closed_icon(IconName::ChevronDown)
                    .visible_on_hover(group.clone()),
            )
            .when(running, |header| {
                header
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::LoadCircle)
                            .size(IconSize::Small)
                            .color(Color::Muted)
                            .with_keyed_rotate_animation(child_id("spinner"), 2),
                    )
                    .child(Divider::vertical().color(ui::DividerColor::Border).ml_1())
                    .child(
                        IconButton::new(child_id("stop"), IconName::Stop)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Error)
                            .tooltip(Tooltip::text("Stop This Command"))
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    )
            })
            .when(failed, |header| {
                header.child(
                    IconButton::new(child_id("failed"), IconName::Close)
                        .style(ButtonStyle::Transparent)
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Error)
                        .when_some(exit_code, |this, code| {
                            this.tooltip(Tooltip::text(format!("Exited with code {code}")))
                        }),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle_key.clone(), cx)));

        let command_style = {
            let mut style = mono_style(window, cx);
            style.base_text_style.font_size = rems_from_px(12_f32).into();
            style.base_text_style.line_height = rems_from_px(17_f32).into();
            style
        };
        let command_text = self.markdown_block(
            (message_index, chunk, MarkdownKind::TerminalCommand),
            &command,
            command_style,
            cx,
        );
        let command_slot = v_flex()
            .group(group.clone())
            .relative()
            .p_1p5()
            .bg(header_bg)
            .child(command_text)
            .child(
                div().absolute().top_1().right_1().child(
                    CopyButton::new(child_id("copy-command"), command)
                        .tooltip_label("Copy Command")
                        .visible_on_hover(group.clone()),
                ),
            );

        let output = result
            .as_ref()
            .and_then(|r| r.get("output"))
            .and_then(|o| o.as_str())
            .map(str::to_string)
            .or_else(|| response.as_ref().map(|(text, _)| text.clone()))
            .unwrap_or_default();
        let output = (is_expanded && response.is_some()).then(|| {
            let style = mono_style(window, cx);
            self.markdown_block(
                (message_index, chunk, MarkdownKind::TerminalOutput),
                &clip(output.trim_end()),
                style,
                cx,
            )
        });
        v_flex()
            .my_1p5()
            .border_1()
            .when(failed, |card| card.border_dashed())
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden()
            .child(
                v_flex()
                    .group(group)
                    .text_xs()
                    .bg(header_bg)
                    .child(header_row)
                    .child(command_slot),
            )
            .when_some(output, |this, output| {
                this.child(
                    div()
                        .id(child_id("output"))
                        .border_t_1()
                        .when(failed, |card| card.border_dashed())
                        .border_color(border_color)
                        .rounded_b_md()
                        .max_h(rems(18.))
                        .overflow_y_scroll()
                        .p_2()
                        .child(output),
                )
            })
            .into_any_element()
    }

    /// Zed's `render_generating`: the spinner under the thread while a turn runs (sand and
    /// "Awaiting Confirmation" while a question waits). The context ring in the composer shows
    /// the token count, so the elapsed-time and token labels Zed adds here are left out.
    fn render_generating(&self) -> impl IntoElement {
        let confirmation = self.clarify.is_some();
        h_flex()
            .id("generating-spinner")
            .py_2()
            .px(rems_from_px(22_f32))
            .gap_2()
            .map(|this| {
                if confirmation {
                    this.child(
                        h_flex()
                            .w_2()
                            .justify_center()
                            .child(chat_ui::GeneratingSpinnerElement::new(SpinnerVariant::Sand)),
                    )
                    .child(
                        div().min_w(rems(8.)).child(
                            LoadingLabel::new("Awaiting Confirmation")
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .single_line(),
                        ),
                    )
                } else {
                    this.child(
                        h_flex()
                            .w_2()
                            .justify_center()
                            .child(chat_ui::GeneratingSpinnerElement::new(SpinnerVariant::Dots)),
                    )
                }
            })
    }

    /// Zed's thread controls under a finished turn: copy the response, scroll to the user's
    /// message, and (at the bottom) scroll to top. Token use shows in the context ring only.
    fn render_thread_controls(
        &self,
        index: usize,
        is_thread_bottom: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        // The turn: the agent entries since the previous user message.
        let user_entry = self.entries[..index]
            .iter()
            .rposition(|&m| self.conversation.messages()[m].role == Role::User);
        let response: Vec<usize> = self.entries[user_entry.map_or(0, |u| u + 1)..=index].to_vec();
        h_flex()
            .w_full()
            .py_1p5()
            .px_4()
            .justify_end()
            .opacity(0.4)
            .hover(|s| s.opacity(1.))
            .child(
                IconButton::new(("copy_agent_response", index), IconName::Copy)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Copy This Agent Response"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let text = response
                            .iter()
                            .map(|&m| this.conversation.messages()[m].as_concat_text())
                            .filter(|t| !t.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    })),
            )
            .child(
                IconButton::new(
                    ("scroll_to_recent_user_prompt", index),
                    IconName::UserArrowUp,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text("Scroll to User Message"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(user_entry) = user_entry {
                        this.list_state.scroll_to(ListOffset {
                            item_ix: user_entry,
                            offset_in_item: px(0.),
                        });
                        cx.notify();
                    }
                })),
            )
            .when(is_thread_bottom, |this| {
                this.child(
                    IconButton::new(("scroll_to_top", index), IconName::ArrowUp)
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .tooltip(Tooltip::text("Scroll to Top"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.list_state.scroll_to(ListOffset {
                                item_ix: 0,
                                offset_in_item: px(0.),
                            });
                            cx.notify();
                        })),
                )
            })
    }

    fn render_clarify(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let form = self.clarify.as_ref()?;
        let colors = cx.theme().colors();
        let questions = form.questions.iter().enumerate().map(|(q, question)| {
            let choices = question.choices.iter().enumerate().map(|(c, choice)| {
                let selected = form.selected[q][c];
                let label = if c == 0 {
                    format!("{choice} (Recommended)")
                } else {
                    choice.clone()
                };
                Button::new(SharedString::from(format!("clarify-{q}-{c}")), label)
                    .style(ButtonStyle::Outlined)
                    .toggle_state(selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = this.clarify.as_mut() {
                            let multi = form.questions[q].multi_select;
                            if !multi {
                                form.selected[q].iter_mut().for_each(|s| *s = false);
                            }
                            form.selected[q][c] = !selected || !multi;
                        }
                        cx.notify();
                    }))
            });
            v_flex()
                .gap_1()
                .child(Label::new(question.question.clone()))
                .child(h_flex().flex_wrap().gap_1().children(choices))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.editor_background)
                        .child(form.other[q].clone()),
                )
        });
        Some(
            v_flex()
                .mx_4()
                .mb_2()
                .p_3()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(colors.border_focused)
                .bg(colors.panel_background)
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::Info).color(Color::Accent))
                        .child(Label::new("Hermes has a question")),
                )
                .children(questions)
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("clarify-skip", "Skip").on_click(
                                cx.listener(|this, _, _, cx| this.answer_clarify(false, cx)),
                            ),
                        )
                        .child(
                            Button::new("clarify-submit", "Submit")
                                .style(ButtonStyle::Filled)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer_clarify(true, cx)),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Zed's thinking control: a toggle (thinking on/off) split with the effort selector.
    fn render_thinking_control(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let state = AppState::global(cx).read(cx);
        let supports = state
            .model_info(&self.model)
            .and_then(|model| model.supports_reasoning);
        if supports != Some(true) {
            return None;
        }
        let current = state.config.efforts.get(&self.model).cloned();
        let thinking = current.as_deref() != Some("none");
        let (tooltip_label, icon, color) = if thinking {
            (
                "Disable Thinking Mode",
                IconName::ThinkingMode,
                Color::Accent,
            )
        } else {
            (
                "Enable Thinking Mode",
                IconName::ThinkingModeOff,
                Color::Custom(cx.theme().colors().icon_disabled.opacity(0.8)),
            )
        };
        let model = self.model.clone();
        let thinking_toggle = IconButton::new("thinking-mode", icon)
            .icon_size(IconSize::Small)
            .icon_color(color)
            .tooltip(Tooltip::text(tooltip_label))
            .on_click(move |_, _, cx| {
                set_effort(&model, if thinking { Some("none") } else { None }, cx)
            });
        if !thinking {
            return Some(thinking_toggle.into_any_element());
        }

        let label = EFFORTS
            .iter()
            .find(|(_, wire)| current.as_deref() == Some(*wire))
            .map(|(label, _)| *label)
            .unwrap_or("Auto");
        let (label_color, chevron) = if self.effort_menu.is_deployed() {
            (Color::Accent, IconName::ChevronUp)
        } else {
            (Color::Muted, IconName::ChevronDown)
        };
        let model = self.model.clone();
        let selector = PopoverMenu::new("effort-selector")
            .trigger_with_tooltip(
                ButtonLike::new_rounded_right("effort-selector-trigger")
                    .child(Label::new(label).size(LabelSize::Small).color(label_color))
                    .child(
                        Icon::new(chevron)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .selected_style(ButtonStyle::Tinted(TintColor::Accent)),
                Tooltip::text("Change Thinking Effort"),
            )
            .menu(move |window, cx| {
                let model = model.clone();
                let current = current.clone();
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    menu = menu.header("Change Thinking Effort");
                    let levels = std::iter::once(("Auto", None)).chain(
                        EFFORTS
                            .iter()
                            .filter(|(_, wire)| *wire != "none")
                            .map(|(label, wire)| (*label, Some(*wire))),
                    );
                    for (label, wire) in levels {
                        let model = model.clone();
                        let selected = current.as_deref() == wire;
                        menu.push_item(
                            ContextMenuEntry::new(label)
                                .toggleable(IconPosition::End, selected)
                                .handler(move |_, cx| set_effort(&model, wire, cx)),
                        );
                    }
                    menu
                }))
            })
            .with_handle(self.effort_menu.clone())
            .offset(gpui::Point {
                x: px(0.),
                y: px(-2.),
            })
            .anchor(gpui::Anchor::BottomLeft);
        Some(
            SplitButton::new(thinking_toggle, selector.into_any_element())
                .style(SplitButtonStyle::Transparent)
                .into_any_element(),
        )
    }

    /// Zed's model selector trigger: the model's name and a chevron; the picker opens upward.
    fn render_model_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let chat = cx.entity().downgrade();
        let selected = AppState::global(cx)
            .read(cx)
            .provider_for_model(&self.model)
            .map(|provider| (provider.id.clone(), self.model.clone()));
        let name = AppState::global(cx)
            .read(cx)
            .model_info(&self.model)
            .and_then(|model| model.name.clone())
            .unwrap_or_else(|| self.model.clone());
        let (color, chevron) = if self.model_menu.is_deployed() {
            (Color::Accent, IconName::ChevronUp)
        } else {
            (Color::Muted, IconName::ChevronDown)
        };
        let running = self.running.is_some();
        PopoverMenu::new("model-selector")
            .menu(move |window, cx| {
                let chat = chat.clone();
                let on_select: crate::model_picker::OnSelect =
                    std::rc::Rc::new(move |provider, id, _, cx| {
                        chat.update(cx, |chat, cx| chat.switch_model(provider, id, cx))
                            .ok();
                    });
                let on_configure: crate::model_picker::OnConfigure =
                    std::rc::Rc::new(|window, cx| {
                        window.dispatch_action(Box::new(crate::workspace::OpenSettings), cx)
                    });
                let selected = selected.clone();
                Some(cx.new(|cx| {
                    ModelPicker::new(selected, on_select, window, cx).in_popover(on_configure)
                }))
            })
            .trigger_with_tooltip(
                Button::new("active-model", name)
                    .label_size(LabelSize::Small)
                    .color(color)
                    .disabled(running)
                    .end_icon(
                        Icon::new(chevron)
                            .color(Color::Muted)
                            .size(IconSize::XSmall),
                    ),
                Tooltip::text("Change Model"),
            )
            .with_handle(self.model_menu.clone())
            .anchor(gpui::Anchor::BottomRight)
    }

    /// Zed's add-context button. Mentions of files, sessions and images arrive in M5; skills
    /// work now (they insert the `/skill` command).
    fn render_add_context_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let chat = cx.entity().downgrade();
        PopoverMenu::new("add-context-menu")
            .trigger_with_tooltip(
                IconButton::new("add-context", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted),
                Tooltip::text("Add Context"),
            )
            .anchor(gpui::Anchor::BottomLeft)
            .offset(gpui::Point {
                x: px(0.),
                y: px(-2.),
            })
            .menu(move |window, cx| {
                let skills: Vec<(String, String)> = backend(cx)
                    .runtime
                    .skills()
                    .catalog()
                    .into_iter()
                    .map(|s| {
                        let source = s.category.unwrap_or_else(|| "general".into());
                        (s.name, source)
                    })
                    .collect();
                let chat = chat.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    let insert = |text: &'static str| {
                        let chat = chat.clone();
                        move |window: &mut Window, cx: &mut App| {
                            chat.update(cx, |chat, cx| chat.insert_into_composer(text, window, cx))
                                .ok();
                        }
                    };
                    let soon = "Coming with context mentions (M5)";
                    menu.key_context("AddContextMenu")
                        .item(
                            ContextMenuEntry::new("Files & Directories")
                                .icon(IconName::File)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .documentation_aside(ui::DocumentationSide::Right, move |_| {
                                    Label::new(soon).into_any_element()
                                })
                                .handler(insert("@file:")),
                        )
                        .item(
                            ContextMenuEntry::new("Sessions")
                                .icon(IconName::Thread)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .documentation_aside(ui::DocumentationSide::Right, move |_| {
                                    Label::new(soon).into_any_element()
                                })
                                .handler(insert("@session:")),
                        )
                        .when(!skills.is_empty(), |this| {
                            let chat = chat.clone();
                            let skills = skills.clone();
                            this.submenu_with_colored_icon(
                                "Skills",
                                IconName::Sparkle,
                                Color::Muted,
                                move |mut menu, _, _| {
                                    for (name, source) in &skills {
                                        let chat = chat.clone();
                                        let command = format!("/{name} ");
                                        menu = menu.item(
                                            ContextMenuEntry::new(format!("{name} ({source})"))
                                                .icon(IconName::Sparkle)
                                                .icon_color(Color::Muted)
                                                .icon_size(IconSize::XSmall)
                                                .handler(move |window, cx| {
                                                    let command = command.clone();
                                                    chat.update(cx, |chat, cx| {
                                                        chat.composer.update(cx, |c, cx| {
                                                            c.set_text(command, cx)
                                                        });
                                                        chat.composer
                                                            .focus_handle(cx)
                                                            .focus(window, cx);
                                                    })
                                                    .ok();
                                                }),
                                        );
                                    }
                                    menu
                                },
                            )
                        })
                        .item(
                            ContextMenuEntry::new("Image")
                                .icon(IconName::Image)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .disabled(true),
                        )
                        .item(
                            ContextMenuEntry::new("URL")
                                .icon(IconName::ToolWeb)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .documentation_aside(ui::DocumentationSide::Right, move |_| {
                                    Label::new(soon).into_any_element()
                                })
                                .handler(insert("@url:")),
                        )
                }))
            })
    }

    /// Appends a mention prefix to the composer and focuses it.
    fn insert_into_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.composer.read(cx).text().to_string();
        let separator = if current.is_empty() || current.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        let updated = format!("{current}{separator}{text}");
        self.composer.update(cx, |c, cx| c.set_text(updated, cx));
        self.composer.focus_handle(cx).focus(window, cx);
    }

    /// Zed's context indicator: a ring that fills as the context window does (warning color at
    /// 85%), with the numbers and the session's cost on hover.
    fn render_token_usage(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let max = AppState::global(cx).read(cx).context_length(&self.model)? as u64;
        let used = self.usage.last_input_tokens.max(0) as u64;
        if max == 0 {
            return None;
        }
        let ratio = used as f32 / max as f32;
        // Zed turns the ring to the warning color at 85%; past 95% it's about to overflow.
        let progress_color = if ratio >= 0.95 {
            cx.theme().status().error
        } else if ratio >= 0.85 {
            cx.theme().status().warning
        } else {
            cx.theme().colors().text_muted
        };
        let percentage = SharedString::from(format!("{}%", (ratio * 100.).round() as u32));
        let used_label = SharedString::from(chat_ui::humanize_token_count(used));
        let max_label = SharedString::from(chat_ui::humanize_token_count(max));
        let cost = (self.usage.cost > 0.).then(|| {
            let precision = if self.usage.cost < 0.01 { 4 } else { 2 };
            SharedString::from(format!("{:.precision$} USD", self.usage.cost))
        });
        Some(
            h_flex()
                .id("circular_progress_tokens")
                .mt_px()
                .mr_1()
                .child(
                    CircularProgress::new(used as f32, max as f32, px(16.), cx)
                        .stroke_width(px(2.))
                        .progress_color(progress_color),
                )
                .hoverable_tooltip(move |_, cx| {
                    let (percentage, used, max, cost) = (
                        percentage.clone(),
                        used_label.clone(),
                        max_label.clone(),
                        cost.clone(),
                    );
                    cx.new(|_| chat_ui::TokenUsageTooltip {
                        percentage,
                        used,
                        max,
                        cost,
                    })
                    .into()
                }),
        )
    }

    /// Zed's send button: Stop while a turn runs and the box is empty; with text during a turn
    /// it steers (Zed queues); disabled when there's nothing to send.
    fn render_send_button(&self, cx: &Context<Self>) -> AnyElement {
        let is_editor_empty = self.composer.read(cx).text().trim().is_empty();
        let is_generating = self.running.is_some();
        if is_generating && is_editor_empty {
            return IconButton::new("stop-generation", IconName::Stop)
                .icon_color(Color::Error)
                .style(ButtonStyle::Tinted(TintColor::Error))
                .tooltip(Tooltip::text("Stop Generation"))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                .into_any_element();
        }
        let icon = if is_generating {
            IconName::QueueMessage
        } else {
            IconName::Send
        };
        IconButton::new("send-message", icon)
            .style(ButtonStyle::Filled)
            .map(|this| {
                if is_editor_empty && !is_generating {
                    this.disabled(true).icon_color(Color::Muted)
                } else {
                    this.icon_color(Color::Accent)
                }
            })
            .tooltip(Tooltip::text(if is_editor_empty && !is_generating {
                "Type to Send"
            } else if is_generating {
                "Steer the Agent (delivered after its current tool call)"
            } else {
                "Send Message"
            }))
            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
            .into_any_element()
    }

    /// The agent's task list: the latest successful `todo_list` result in the conversation.
    fn current_todos(&self) -> Vec<TodoItem> {
        let todo_calls: HashSet<&str> = self
            .conversation
            .messages()
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(MessageContent::as_tool_request)
            .filter(|request| {
                request
                    .tool_call
                    .as_ref()
                    .is_ok_and(|call| call.name == "todo_list")
            })
            .map(|request| request.id.as_str())
            .collect();
        self.conversation
            .messages()
            .iter()
            .rev()
            .flat_map(|message| message.content.iter().rev())
            .filter_map(|content| {
                let response = content.as_tool_response()?;
                todo_calls
                    .contains(response.id.as_str())
                    .then(|| Some(tool_output(content)))?
            })
            .find_map(|text| {
                let value: serde_json::Value = serde_json::from_str(&text).ok()?;
                serde_json::from_value(value.get("todos")?.clone()).ok()
            })
            .unwrap_or_default()
    }

    /// Hermes's "Tasks" strip above the composer: progress, expandable to every item.
    fn render_todos(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let todos = self.current_todos();
        let done = todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let open = todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count();
        if todos.is_empty() || (open == 0 && self.running.is_none()) {
            return None;
        }
        let colors = cx.theme().colors();
        let expanded = self.expanded.contains("todos");
        let active = todos
            .iter()
            .find(|t| t.status == TodoStatus::InProgress)
            .map(|t| t.content.clone());
        Some(
            v_flex()
                .mx_4()
                .mb_1()
                .px_2()
                .py_1()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(colors.border_variant)
                .bg(colors.editor_background)
                .child(
                    h_flex()
                        .id("todos-header")
                        .gap_2()
                        .cursor_pointer()
                        .child(
                            Icon::new(IconName::ListTodo)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(format!("Tasks {done}/{}", todos.len()))
                                .size(LabelSize::Small),
                        )
                        .when_some(active.filter(|_| !expanded), |this, active| {
                            this.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_sm()
                                    .text_color(colors.text_muted)
                                    .child(active),
                            )
                        })
                        .child(div().flex_1())
                        .child(Disclosure::new("todos-disclosure", expanded))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle("todos".into(), cx))),
                )
                .when(expanded, |this| {
                    this.child(
                        v_flex()
                            .id("todo-items")
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .children(todos.iter().map(|todo| {
                                let (icon, color) = match todo.status {
                                    TodoStatus::Completed => {
                                        (IconName::TodoComplete, Color::Success)
                                    }
                                    TodoStatus::InProgress => {
                                        (IconName::TodoProgress, Color::Accent)
                                    }
                                    TodoStatus::Pending => (IconName::TodoPending, Color::Muted),
                                    TodoStatus::Cancelled => (IconName::XCircle, Color::Disabled),
                                };
                                h_flex()
                                    .gap_2()
                                    .py_0p5()
                                    .when(todo.parent.is_some(), |row| row.pl_5())
                                    .child(Icon::new(icon).size(IconSize::Small).color(color))
                                    .child(
                                        Label::new(todo.content.clone())
                                            .size(LabelSize::Small)
                                            .when(todo.status == TodoStatus::Cancelled, |l| {
                                                l.strikethrough()
                                            })
                                            .when(todo.status == TodoStatus::Completed, |l| {
                                                l.color(Color::Muted)
                                            }),
                                    )
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// Zed's message editor: a strip across the bottom on the editor background, the input with
    /// an expand toggle, and the toolbar under it (context and thinking on the left; context
    /// usage, model and send on the right).
    fn render_composer(&self, has_messages: bool, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (expand_icon, expand_tooltip) = if self.composer_expanded {
            (IconName::Minimize, "Minimize Message Editor")
        } else {
            (IconName::Maximize, "Expand Message Editor")
        };
        h_flex()
            .py_2()
            .bg(colors.editor_background)
            .justify_center()
            .when(has_messages, |this| {
                this.border_t_1().border_color(colors.border)
            })
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .gap_2()
                    .children((!self.slash_matches.is_empty()).then(|| {
                        v_flex().children(self.slash_matches.iter().map(|(name, description)| {
                            let command = format!("/{name} ");
                            h_flex()
                                .id(SharedString::from(format!("slash-{name}")))
                                .gap_2()
                                .px_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(colors.ghost_element_hover))
                                .child(Label::new(format!("/{name}")).size(LabelSize::Small))
                                .child(
                                    Label::new(description.clone())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.composer
                                        .update(cx, |c, cx| c.set_text(command.clone(), cx));
                                    this.composer.focus_handle(cx).focus(window, cx);
                                    this.slash_matches.clear();
                                    cx.notify();
                                }))
                        }))
                    }))
                    .when(self.editing.is_some(), |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::Pencil)
                                        .size(IconSize::XSmall)
                                        .color(Color::Accent),
                                )
                                .child(
                                    Label::new(
                                        "Editing a message: sending restarts the thread from it",
                                    )
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                                )
                                .child(div().flex_1())
                                .child(
                                    IconButton::new("cancel-edit", IconName::Close)
                                        .icon_color(Color::Error)
                                        .icon_size(IconSize::XSmall)
                                        .tooltip(Tooltip::text("Cancel Editing"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.editing = None;
                                            this.composer.update(cx, |c, cx| c.set_text("", cx));
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .when_some(self.steering.clone(), |this, steering| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::QueueMessage)
                                        .size(IconSize::XSmall)
                                        .color(Color::Accent),
                                )
                                .child(
                                    Label::new(format!("Steering: {steering}"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .truncate(),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .relative()
                            .w_full()
                            .min_h_0()
                            .pt_1()
                            .pr_2p5()
                            .child(self.composer.clone())
                            .when(has_messages, |this| {
                                this.child(
                                    h_flex()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .opacity(0.5)
                                        .hover(|s| s.opacity(1.0))
                                        .child(
                                            IconButton::new("toggle-height", expand_icon)
                                                .icon_size(IconSize::Small)
                                                .icon_color(Color::Muted)
                                                .tooltip(Tooltip::text(expand_tooltip))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.composer_expanded =
                                                        !this.composer_expanded;
                                                    let lines = if this.composer_expanded {
                                                        32
                                                    } else {
                                                        12
                                                    };
                                                    this.composer.update(cx, |c, cx| {
                                                        c.set_max_lines(lines, cx)
                                                    });
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_none()
                            .flex_wrap()
                            .justify_between()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .flex_wrap()
                                    .gap_0p5()
                                    .child(self.render_add_context_button(cx))
                                    .children(self.render_thinking_control(cx)),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(self.render_token_usage(cx))
                                    .child(self.render_model_button(cx))
                                    .child(self.render_send_button(cx)),
                            ),
                    ),
            )
    }
}

const COMPOSER_PLACEHOLDER: &str =
    "Message Hermes… (Enter to send, Shift+Enter for a new line, / for skills)";
const STEER_PLACEHOLDER: &str = "Steer the agent… (delivered after its current tool call)";

/// Effort levels: label and the value sent to the provider.
const EFFORTS: &[(&str, &str)] = &[
    ("Off", "none"),
    ("Minimal", "minimal"),
    ("Low", "low"),
    ("Medium", "medium"),
    ("High", "high"),
    ("Max", "xhigh"),
];

fn set_effort(model: &str, effort: Option<&str>, cx: &mut App) {
    AppState::global(cx).update(cx, |state, cx| {
        match effort {
            Some(effort) => state
                .config
                .efforts
                .insert(model.to_string(), effort.to_string()),
            None => state.config.efforts.remove(model),
        };
        state.save(cx);
    });
}

/// Zed's tool icons by kind: reads and searches, edits, execution.
fn tool_icon(name: &str) -> IconName {
    match name {
        "read_file" | "search_files" | "session_search" | "skills_list" | "skill_view" => {
            IconName::ToolSearch
        }
        "write_file" | "patch" | "skill_manage" | "memory" | "todo_list" => IconName::ToolPencil,
        "terminal" | "process_manage" => IconName::ToolTerminal,
        _ => IconName::ToolHammer,
    }
}

/// Zed's `tool_card_header_bg`.
fn tool_card_header_bg(cx: &App) -> gpui::Hsla {
    cx.theme()
        .colors()
        .element_background
        .blend(cx.theme().colors().editor_foreground.opacity(0.025))
}

/// Zed's `tool_card_border_color`.
fn tool_card_border_color(cx: &App) -> gpui::Hsla {
    cx.theme().colors().border.opacity(0.8)
}

/// A tool result as selectable text: JSON fields one per line (multi-line strings under their
/// key), and a `diff` field apart so it can be colored like a diff view.
fn result_parts(text: &str) -> (String, Option<String>) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return (clip(text), None);
    };
    let mut lines = Vec::new();
    let mut diff = None;
    for (key, value) in &map {
        match value {
            serde_json::Value::String(text) if key == "diff" => diff = Some(clip(text)),
            serde_json::Value::String(text) if text.contains('\n') => {
                lines.push(format!("{key}:"));
                lines.push(clip(text));
            }
            serde_json::Value::String(text) => lines.push(format!("{key}: {}", clip(text))),
            other => lines.push(format!("{key}: {}", clip(&other.to_string()))),
        }
    }
    (lines.join("\n"), diff)
}

/// `text` in a fenced code block tagged `language`; the fence outgrows any backtick run inside.
fn fenced(language: &str, text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!(
        "{fence}{language}\n{}\n{fence}",
        text.trim_end_matches('\n')
    )
}

/// Tool input and output: the agent's code font at Zed's extra-small size, selectable.
fn mono_style(window: &Window, cx: &App) -> MarkdownStyle {
    let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
    let settings = theme::theme_settings(cx);
    style.base_text_style.font_family = settings.buffer_font(cx).family.clone();
    style.base_text_style.font_size = ui::TextSize::XSmall.rems(cx).into();
    style.base_text_style.color = cx.theme().colors().text;
    style.code_block_overflow_x_scroll = true;
    style
}

fn clip(text: &str) -> String {
    if text.chars().count() > MAX_RESULT_CHARS {
        format!(
            "{}\n… ({} more characters)",
            text.chars().take(MAX_RESULT_CHARS).collect::<String>(),
            text.chars().count() - MAX_RESULT_CHARS
        )
    } else {
        text.to_string()
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let empty = self.entries.is_empty();
        v_flex()
            .size_full()
            .bg(colors.panel_background)
            .child(if empty {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::XLarge)
                            .color(Color::Accent),
                    )
                    .child(Headline::new("What should we work on?").size(HeadlineSize::Small))
                    .into_any_element()
            } else {
                list(
                    self.list_state.clone(),
                    cx.processor(|this, index: usize, window, cx| {
                        this.render_entry(index, window, cx)
                    }),
                )
                .flex_1()
                .size_full()
                .into_any_element()
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div().mx_4().mb_2().child(
                        Callout::new()
                            .severity(Severity::Error)
                            .icon(IconName::Warning)
                            .title("Something went wrong")
                            .description(error),
                    ),
                )
            })
            .children(self.render_clarify(cx))
            .children(self.render_todos(cx))
            .child(self.render_composer(!empty, cx))
    }
}

/// What a tool returned, before the runtime's loop guardrails added notes for the model.
fn tool_output(content: &MessageContent) -> String {
    let structured = content
        .as_tool_response()
        .and_then(|response| response.tool_result.as_ref().ok())
        .and_then(|result| result.structured_content.as_ref());
    match structured {
        Some(value) => value.to_string(),
        None => content.as_tool_response_text().unwrap_or_default(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MarkdownKind {
    Text,
    Thinking,
    User,
    ToolLabel,
    /// A tool call's arguments, its result, a diff it made, and a terminal command and output.
    ToolInput,
    ToolOutput,
    ToolDiff,
    TerminalCommand,
    TerminalOutput,
}

/// (message index, content block, kind) of a rendered markdown block.
type MarkdownKey = (usize, usize, MarkdownKind);
