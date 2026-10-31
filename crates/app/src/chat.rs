//! One session's conversation: the message list, the composer, and the running turn.
//! Layout and card styling follow Zed's `agent_ui` thread view (GPL-3.0-or-later).

use std::collections::{HashMap, HashSet};

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Entity, FocusHandle, Focusable,
    FollowMode, ListAlignment, ListState, SharedString, Subscription, Task, Window, list,
    prelude::*, pulsating_between, px,
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
use ui::{Callout, CommonAnimationExt, Disclosure, Divider, PopoverMenu, Tooltip, prelude::*};

use crate::backend::backend;
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
    conversation: Conversation,
    /// Indices into the conversation of the messages that render as list entries.
    entries: Vec<usize>,
    list_state: ListState,
    composer: Entity<TextArea>,
    running: Option<CancellationToken>,
    turn_task: Option<Task<()>>,
    expanded: HashSet<String>,
    clarify: Option<ClarifyForm>,
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
        let composer = cx.new(|cx| {
            TextArea::new(
                "Message Hermes… (Enter to send, Shift+Enter for a new line)",
                cx,
            )
        });
        let subscription = cx.subscribe_in(
            &composer,
            window,
            |this: &mut Self, _, event, window, cx| {
                if let TextAreaEvent::Submit = event {
                    this.submit(window, cx);
                }
            },
        );
        let list_state = ListState::new(0, ListAlignment::Top, px(2000.));
        list_state.set_follow_mode(FollowMode::Tail);
        let mut this = Self {
            session_id,
            model,
            conversation: Conversation::new_unvalidated(messages),
            entries: Vec::new(),
            list_state,
            composer,
            running: None,
            turn_task: None,
            expanded: HashSet::new(),
            clarify: None,
            usage: TurnUsage {
                last_input_tokens: 0,
                total_input_tokens: totals.input_tokens,
                total_output_tokens: totals.output_tokens,
                cost: totals.cost.unwrap_or(0.),
            },
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![subscription],
        };
        this.rebuild_entries();
        this.list_state.reset(this.entries.len());
        this.list_state.scroll_to_end();
        this
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn composer(&self) -> Entity<TextArea> {
        self.composer.clone()
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
        self.list_state.splice(start..old, new - start);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_string();
        if text.is_empty() || self.running.is_some() {
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.set_text("", cx));
        self.start_turn(text, window, cx);
    }

    pub fn start_turn(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let (provider, context_length) = {
            let state = state.read(cx);
            (state.runtime_provider(), state.context_length(&self.model))
        };
        let Some(provider) = provider else {
            self.error = Some("No provider is configured. Open Settings to add one.".into());
            cx.notify();
            return;
        };
        let backend = backend(cx);
        let turn = {
            let _guard = backend.tokio.enter();
            backend
                .runtime
                .start_turn(&self.session_id, &provider, context_length, &text)
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

    /// Switches this chat's model; it also becomes the default for new chats (Hermes's
    /// composer picker is sticky the same way).
    fn switch_model(&mut self, model: String, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let context_length = state.read(cx).context_length(&model);
        state.update(cx, |state, cx| {
            state.config.model = Some(model.clone());
            state.save(cx);
        });
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
                self.conversation.push(message);
                self.entries_changed(cx);
            }
            RuntimeEvent::Agent(AgentEvent::HistoryReplaced(conversation)) => {
                self.conversation = conversation;
                self.rebuild_entries();
                self.list_state.reset(self.entries.len());
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
            RuntimeEvent::TurnFinished(status) => {
                self.running = None;
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
                    Ok(result) => (
                        content.as_tool_response_text().unwrap_or_default(),
                        result.is_error.unwrap_or(false),
                    ),
                    Err(error) => (error.message.to_string(), true),
                };
                Some((response.id.as_str(), (text, is_error)))
            })
            .collect()
    }

    fn render_entry(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(&message_index) = self.entries.get(index) else {
            return div().into_any_element();
        };
        let message = self.conversation.messages()[message_index].clone();
        let content = match message.role {
            Role::User => self.render_user_message(&message, cx),
            Role::Assistant => self.render_assistant_message(&message, index, cx),
        };
        div()
            .w_full()
            .px_4()
            .py_2()
            .child(content)
            .into_any_element()
    }

    fn render_user_message(&self, message: &Message, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let text = message.as_concat_text();
        h_flex()
            .w_full()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(colors.editor_background)
                    .border_1()
                    .border_color(colors.border)
                    .text_color(colors.text)
                    .child(text),
            )
            .into_any_element()
    }

    fn render_assistant_message(
        &mut self,
        message: &Message,
        entry: usize,
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
                    blocks.push(
                        div()
                            .w_full()
                            .child(text.text.trim().to_string())
                            .into_any_element(),
                    );
                }
                MessageContent::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                    let active = streaming_chunk == Some(chunk);
                    blocks.push(self.render_thinking(entry, chunk, &thinking.thinking, active, cx));
                }
                MessageContent::ToolRequest(request) => {
                    let response = responses.get(&request.id).cloned();
                    blocks.push(self.render_tool_call(request, response, cx));
                }
                MessageContent::Error(error) => {
                    blocks.push(
                        Callout::new()
                            .severity(Severity::Error)
                            .icon(IconName::XCircle)
                            .title("The provider returned an error")
                            .description(error.message.clone())
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }
        v_flex()
            .w_full()
            .gap_2()
            .children(blocks)
            .into_any_element()
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        // Expanding changes the entry's height.
        self.list_state.reset(self.entries.len());
        cx.notify();
    }

    fn render_thinking(
        &self,
        entry: usize,
        chunk: usize,
        text: &str,
        active: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = format!("thinking-{entry}-{chunk}");
        let open = self.expanded.contains(&key);
        let colors = cx.theme().colors();
        let toggle_key = key.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{key}-header")))
                    .gap_1p5()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconName::ToolThink)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(if active {
                        Label::new("Thinking")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .with_animation(
                                SharedString::from(format!("{key}-pulse")),
                                Animation::new(Duration::from_secs(2))
                                    .repeat()
                                    .with_easing(pulsating_between(0.3, 1.0)),
                                |label, delta| label.alpha(delta),
                            )
                            .into_any_element()
                    } else {
                        Label::new("Thinking")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .into_any_element()
                    })
                    .child(Disclosure::new(
                        SharedString::from(format!("{key}-disclosure")),
                        open,
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle(toggle_key.clone(), cx)),
                    ),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .ml_1p5()
                        .pl_3p5()
                        .border_l_1()
                        .border_color(colors.border_variant)
                        .text_color(colors.text_muted)
                        .text_sm()
                        .child(text.trim().to_string()),
                )
            })
            .into_any_element()
    }

    fn render_tool_call(
        &self,
        request: &hermes_llm::conversation::message::ToolRequest,
        response: Option<(String, bool)>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
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
        let key = format!("tool-{}", request.id);
        let open = self.expanded.contains(&key);
        let toggle_key = key.clone();
        let status: AnyElement = match (&response, self.running.is_some()) {
            (Some((_, true)), _) => Icon::new(IconName::XCircle)
                .size(IconSize::Small)
                .color(Color::Error)
                .into_any_element(),
            (Some(_), _) => Icon::new(IconName::Check)
                .size(IconSize::Small)
                .color(Color::Success)
                .into_any_element(),
            (None, true) => Icon::new(IconName::LoadCircle)
                .size(IconSize::Small)
                .color(Color::Accent)
                .with_keyed_rotate_animation(SharedString::from(format!("{key}-spin")), 2)
                .into_any_element(),
            (None, false) => Icon::new(IconName::Stop)
                .size(IconSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        };
        let summary = tool_summary(&name, &args);
        let mono = theme::theme_settings(cx).buffer_font(cx).clone();

        v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.editor_background)
            .overflow_hidden()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{key}-header")))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .child(
                        Icon::new(tool_icon(&name))
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(name.clone())
                            .size(LabelSize::Small)
                            .color(Color::Default),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .text_color(colors.text_muted)
                            .font(mono.clone())
                            .child(summary),
                    )
                    .child(status)
                    .child(Disclosure::new(
                        SharedString::from(format!("{key}-disclosure")),
                        open,
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle(toggle_key.clone(), cx)),
                    ),
            )
            .when(open, |this| {
                let args_text = serde_json::to_string_pretty(&args).unwrap_or_default();
                let result = match response.as_ref() {
                    Some((text, _)) => render_result(text, cx),
                    None => div().child("(running…)").into_any_element(),
                };
                this.child(Divider::horizontal()).child(
                    v_flex()
                        .p_2()
                        .gap_2()
                        .font(mono)
                        .text_xs()
                        .child(
                            Label::new("Arguments")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(div().text_color(colors.text).child(clip(&args_text)))
                        .child(
                            Label::new("Result")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(div().text_color(colors.text).child(result)),
                )
            })
            .into_any_element()
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

    fn render_model_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let chat = cx.entity().downgrade();
        let selected = self.model.clone();
        let short = self
            .model
            .rsplit('/')
            .next()
            .unwrap_or(&self.model)
            .to_string();
        let running = self.running.is_some();
        PopoverMenu::new("model-selector")
            .menu(move |window, cx| {
                let chat = chat.clone();
                let on_select: crate::model_picker::OnSelect =
                    std::rc::Rc::new(move |id, _, cx| {
                        chat.update(cx, |chat, cx| chat.switch_model(id, cx)).ok();
                    });
                let selected = selected.clone();
                Some(cx.new(|cx| {
                    ModelPicker::new(Some(selected), px(320.), on_select, window, cx).in_popover()
                }))
            })
            .trigger(
                Button::new("model-button", short)
                    .label_size(LabelSize::Small)
                    .color(Color::Muted)
                    .disabled(running)
                    .end_icon(
                        Icon::new(IconName::ChevronDown)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .anchor(gpui::Anchor::BottomLeft)
    }

    fn render_composer(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let running = self.running.is_some();
        v_flex()
            .mx_4()
            .mb_3()
            .p_2()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.editor_background)
            .child(self.composer.clone())
            .child(
                h_flex()
                    .justify_between()
                    .child(self.render_model_button(cx))
                    .child(if running {
                        IconButton::new("stop", IconName::Stop)
                            .icon_color(Color::Error)
                            .tooltip(Tooltip::text("Stop"))
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                            .into_any_element()
                    } else {
                        IconButton::new("send", IconName::Send)
                            .icon_color(Color::Accent)
                            .tooltip(Tooltip::text("Send (Enter)"))
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                            .into_any_element()
                    }),
            )
    }
}

fn tool_icon(name: &str) -> IconName {
    match name {
        "terminal" | "process_manage" => IconName::ToolTerminal,
        "read_file" => IconName::FileTextOutlined,
        "write_file" | "patch" => IconName::ToolPencil,
        "search_files" => IconName::ToolSearch,
        "todo_list" => IconName::ListTodo,
        "clarify" => IconName::Info,
        _ => IconName::ToolHammer,
    }
}

/// The one-line summary shown on a tool card.
fn tool_summary(name: &str, args: &serde_json::Value) -> String {
    let field = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let summary = match name {
        "terminal" => field("command"),
        "read_file" | "write_file" | "patch" => field("path"),
        "search_files" => field("pattern")
            .map(|p| format!("{p}  in {}", field("path").unwrap_or_else(|| ".".into()))),
        "process_manage" => Some(format!(
            "{} {}",
            field("action").unwrap_or_default(),
            field("session_id").unwrap_or_default()
        )),
        "todo_list" => Some(match args.get("todos").and_then(|t| t.as_array()) {
            Some(todos) => format!("{} items", todos.len()),
            None => "read".into(),
        }),
        "clarify" => args
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(|q| q.as_str())
            .map(str::to_string),
        _ => None,
    };
    summary.unwrap_or_default().replace('\n', " ⏎ ")
}

/// A tool result: JSON fields one per line, multi-line strings as real text, and a `diff`
/// field colored like a diff view.
fn render_result(text: &str, cx: &App) -> AnyElement {
    let colors = cx.theme().colors();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return div().child(clip(text)).into_any_element();
    };
    let mut lines: Vec<AnyElement> = Vec::new();
    for (key, value) in &map {
        match value {
            serde_json::Value::String(diff) if key == "diff" => {
                lines.push(
                    div()
                        .text_color(colors.text_muted)
                        .child("diff:")
                        .into_any_element(),
                );
                for line in clip(diff).lines() {
                    let color = if line.starts_with('+') && !line.starts_with("+++") {
                        Some(colors.version_control_added)
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        Some(colors.version_control_deleted)
                    } else {
                        None
                    };
                    let row = div().child(if line.is_empty() {
                        " ".to_string()
                    } else {
                        line.to_string()
                    });
                    lines.push(match color {
                        Some(color) => row.bg(color.opacity(0.2)).into_any_element(),
                        None => row.into_any_element(),
                    });
                }
            }
            serde_json::Value::String(text) if text.contains('\n') => {
                lines.push(
                    div()
                        .text_color(colors.text_muted)
                        .child(format!("{key}:"))
                        .into_any_element(),
                );
                lines.push(div().child(clip(text)).into_any_element());
            }
            other => {
                let shown = match other {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                lines.push(
                    div()
                        .child(format!("{key}: {}", clip(&shown)))
                        .into_any_element(),
                );
            }
        }
    }
    v_flex().children(lines).into_any_element()
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
                    cx.processor(|this, index: usize, _window, cx| this.render_entry(index, cx)),
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
            .child(self.render_composer(cx))
    }
}
