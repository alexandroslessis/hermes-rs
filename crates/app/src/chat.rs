//! One session's conversation: the message list, the composer, and the running turn.
//! Layout and card styling follow Zed's `agent_ui` thread view (GPL-3.0-or-later).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, Empty, Entity,
    FocusHandle, Focusable, FollowMode, ListAlignment, ListOffset, ListState, SharedString,
    Subscription, Task, Window, linear_color_stop, linear_gradient, list, prelude::*, px,
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
    SplitButton, SplitButtonStyle, TintColor, Tooltip, WithScrollbar, prelude::*, right_click_menu,
};

use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownOptions, MarkdownStyle};

use hermes_tools::{TodoItem, TodoStatus};

use crate::backend::backend;
use crate::chat_ui;
use crate::config;
use crate::model_picker::ModelPicker;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

gpui::actions!(chat, [Find]);

const MAX_RESULT_CHARS: usize = 6000;

pub enum ChatEvent {
    /// The session's stored state changed (new messages, title-worthy first message).
    Updated,
    /// A turn ended on its own (not stopped by the user, and no steer is about to start the
    /// next one): Zed notifies when this happens out of sight.
    TurnFinished { failed: bool, used_tools: bool },
    /// The agent asked a question and waits for the answer.
    NeedsInput,
    /// A command waits for the user's approval.
    NeedsApproval,
}

/// Zed's buffer search bar over the transcript (Ctrl+F).
struct FindBar {
    query: Entity<TextArea>,
    /// (list entry, matches in its message) for each entry with a match.
    hits: Vec<(usize, usize)>,
    total: usize,
    /// The match Enter moves from, counting across the whole transcript.
    current: usize,
    /// The conversation length the hits were found in.
    seen: usize,
    _subscription: Subscription,
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
    find: Option<FindBar>,
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
    /// A sent message being edited where it is (Zed's editing_message), or a waiting one in the
    /// queue panel.
    inline_edit: Option<InlineEdit>,
    /// Skills matching a `/command` being typed: (name, short description).
    slash_matches: Vec<Suggestion>,
    /// The suggestion Tab (or Enter) takes; ↑/↓ move it.
    suggestion_selected: usize,
    /// Commands, skills and bundles by name, for the composer's `/command` chips (filled when a
    /// `/` is first typed): name → what clicking the chip opens.
    known_commands: Rc<RefCell<HashMap<String, String>>>,
    /// Which earlier message ↑ recalled (0 = the latest), while the composer shows it.
    history_index: Option<usize>,
    /// Prompts waiting for the running turn to end (Hermes's `/queue`).
    queued: Vec<String>,
    /// Whether the queue panel over the composer is unfolded.
    queue_expanded: bool,
    /// Images waiting to go with the next message (pasted, dropped or picked).
    attachments: Vec<Arc<gpui::Image>>,
    /// Each running `delegate_task` call'"'"'s subagents and what they last started, by tool call id.
    subagent_progress: HashMap<String, std::collections::BTreeMap<usize, String>>,
    /// Decoded images of sent messages, by (message id, content block).
    message_images: HashMap<(String, usize), Arc<gpui::Image>>,
    /// The working folder's files, listed when an `@file:` / `@folder:` mention starts.
    file_index: Option<Vec<(String, bool)>>,
    /// A steer the turn ended before delivering: Hermes sends it as the next message.
    next_message: Option<String>,
    /// A flagged command waiting for the user's decision, and where the answer goes.
    approval: Option<(
        hermes_tools::approval::CommandApproval,
        Option<oneshot::Sender<hermes_tools::approval::ApprovalChoice>>,
    )>,
    /// Background-review notes shown under the thread until the next turn (Hermes prints them).
    notices: Vec<String>,
    /// A context compaction is running (the generating row says so).
    compacting: bool,
    /// The running "turn" is a `/compress`, which ends without the turn-finished notification.
    manual_compress: bool,
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
        let known_commands: Rc<RefCell<HashMap<String, String>>> = Rc::default();
        let composer = cx.new(|cx| {
            TextArea::new(COMPOSER_PLACEHOLDER, cx)
                .with_decorations(composer_decorations(known_commands.clone()))
        });
        let subscription = cx.subscribe_in(
            &composer,
            window,
            |this: &mut Self, _, event, window, cx| match event {
                TextAreaEvent::Submit => {
                    if !this.enter_takes_suggestion(window, cx) {
                        this.submit(window, cx)
                    }
                }
                TextAreaEvent::PastedImage(image) => this.attach_bytes(image.bytes.clone(), cx),
                TextAreaEvent::PastedPaths(paths) => this.attach_paths(paths.clone(), cx),
                TextAreaEvent::HistoryPrev => {
                    if !this.move_suggestion(false, cx) {
                        this.step_history(true, cx)
                    }
                }
                TextAreaEvent::HistoryNext => {
                    if !this.move_suggestion(true, cx) {
                        this.step_history(false, cx)
                    }
                }
                TextAreaEvent::MentionClicked(target) => this.open_mention(target, cx),
                TextAreaEvent::Changed => {
                    // Typing over a recalled message leaves the history.
                    if let Some(index) = this.history_index {
                        let text = this.composer.read(cx).text().to_string();
                        if this.history().get(index) != Some(&text) {
                            this.history_index = None;
                        }
                    }
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
            find: None,
            list_state,
            composer,
            running: None,
            turn_task: None,
            expanded: HashSet::new(),
            markdown: HashMap::new(),
            clarify: None,
            steering: None,
            inline_edit: None,
            slash_matches: Vec::new(),
            suggestion_selected: 0,
            known_commands,
            file_index: None,
            attachments: Vec::new(),
            queued: Vec::new(),
            queue_expanded: true,
            history_index: None,
            message_images: HashMap::new(),
            subagent_progress: HashMap::new(),
            next_message: None,
            notices: Vec::new(),
            compacting: false,
            manual_compress: false,
            approval: None,
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

    /// A slash command from the command palette: sent at once when `run`, else left in the
    /// composer for its arguments.
    pub fn run_command(&mut self, name: &str, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, cx| composer.set_text(format!("/{name} "), cx));
        self.composer.focus_handle(cx).focus(window, cx);
        if run {
            self.submit(window, cx);
        }
    }

    /// Ctrl+F: opens the find bar (or puts focus back in it).
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            let query = cx.new(|cx| TextArea::new("Find in conversation…", cx).single_line());
            let subscription = cx.subscribe_in(
                &query,
                window,
                |this: &mut Self, _, event, _, cx| match event {
                    TextAreaEvent::Changed => {
                        if let Some(find) = &mut this.find {
                            find.current = 0;
                        }
                        this.update_find(cx);
                        this.reveal_match(cx);
                    }
                    TextAreaEvent::Submit => this.step_find(true, cx),
                    _ => {}
                },
            );
            self.find = Some(FindBar {
                query,
                hits: Vec::new(),
                total: 0,
                current: 0,
                seen: 0,
                _subscription: subscription,
            });
        }
        if let Some(find) = &self.find {
            find.query.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        for markdown in self.markdown.values() {
            markdown.update(cx, |markdown, cx| markdown.clear_search_highlights(cx));
        }
        self.composer.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The query as matched: case-insensitive (ASCII, so byte offsets stay put).
    fn find_query(&self, cx: &App) -> Option<String> {
        let find = self.find.as_ref()?;
        let query = find.query.read(cx).text().to_ascii_lowercase();
        (!query.is_empty()).then_some(query)
    }

    /// Finds the matches again: per message, in the order the list shows them.
    fn update_find(&mut self, cx: &mut Context<Self>) {
        let query = self.find_query(cx);
        let messages = self.conversation.messages();
        let Some(find) = &mut self.find else {
            return;
        };
        find.hits = match &query {
            Some(query) => self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(entry, &message)| {
                    let count = find_ranges(&shown_user_text(&messages[message]), query).len();
                    (count > 0).then_some((entry, count))
                })
                .collect(),
            None => Vec::new(),
        };
        find.total = find.hits.iter().map(|(_, count)| count).sum();
        find.current = find.current.min(find.total.saturating_sub(1));
        find.seen = messages.len();
        cx.notify();
    }

    /// The list entry of the current match and which match it is within its message.
    fn current_match(&self) -> Option<(usize, usize)> {
        let find = self.find.as_ref()?;
        let mut left = find.current;
        for &(entry, count) in &find.hits {
            if left < count {
                return Some((entry, left));
            }
            left -= count;
        }
        None
    }

    fn reveal_match(&mut self, cx: &mut Context<Self>) {
        if let Some((entry, _)) = self.current_match() {
            self.list_state.scroll_to_reveal_item(entry);
        }
        cx.notify();
    }

    /// Enter / Shift+Enter: the next or previous match, wrapping around.
    fn step_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.total == 0 {
            return;
        }
        find.current = if forward {
            (find.current + 1) % find.total
        } else {
            (find.current + find.total - 1) % find.total
        };
        self.reveal_match(cx);
    }

    /// Highlights the query in a markdown block as it renders; the current match is the one
    /// whose position in its message (counting this message's earlier blocks) is `current`.
    fn highlight_matches(&self, key: MarkdownKey, markdown: &Entity<Markdown>, cx: &mut App) {
        let Some(query) = self.find_query(cx) else {
            markdown.update(cx, |markdown, cx| markdown.clear_search_highlights(cx));
            return;
        };
        let ranges = find_ranges(markdown.read(cx).source(), &query);
        let active = self
            .current_match()
            .filter(|(entry, _)| self.entries.get(*entry) == Some(&key.0))
            .and_then(|(_, ordinal)| {
                let before: usize = self
                    .markdown
                    .iter()
                    .filter(|(k, _)| k.0 == key.0 && (k.1, k.2) < (key.1, key.2))
                    .map(|(_, m)| find_ranges(m.read(cx).source(), &query).len())
                    .sum();
                ordinal.checked_sub(before)
            });
        markdown.update(cx, |markdown, cx| {
            // Only on change: setting notifies, and notifying re-renders this.
            if markdown.search_highlights() != ranges.as_slice() {
                markdown.set_search_highlights(ranges, active, cx);
            } else {
                markdown.set_active_search_highlight(active, cx);
            }
        });
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let find = self.find.as_ref()?;
        let colors = cx.theme().colors();
        let has_query = !find.query.read(cx).text().is_empty();
        let count = if find.total == 0 {
            if has_query { "No results".to_string() } else { String::new() }
        } else {
            format!("{} of {}", find.current + 1, find.total)
        };
        Some(
            h_flex()
                .key_context("FindBar")
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let keystroke = &event.keystroke;
                    match keystroke.key.as_str() {
                        "escape" => this.close_find(window, cx),
                        "enter" if keystroke.modifiers.shift => this.step_find(false, cx),
                        "f3" => this.step_find(!keystroke.modifiers.shift, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .px_3()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(colors.border_variant)
                .bg(colors.toolbar_background)
                .child(Icon::new(IconName::MagnifyingGlass).size(IconSize::Small).color(Color::Muted))
                .child(
                    div()
                        .flex_1()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(if has_query && find.total == 0 {
                            colors.border_disabled
                        } else {
                            colors.border
                        })
                        .bg(colors.editor_background)
                        .child(find.query.clone()),
                )
                .child(
                    Label::new(count)
                        .size(LabelSize::Small)
                        .color(if has_query && find.total == 0 {
                            Color::Error
                        } else {
                            Color::Muted
                        }),
                )
                .child(
                    IconButton::new("find-previous", IconName::ChevronUp)
                        .icon_size(IconSize::Small)
                        .disabled(find.total == 0)
                        .tooltip(Tooltip::text("Previous Match (Shift+Enter)"))
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(false, cx))),
                )
                .child(
                    IconButton::new("find-next", IconName::ChevronDown)
                        .icon_size(IconSize::Small)
                        .disabled(find.total == 0)
                        .tooltip(Tooltip::text("Next Match (Enter)"))
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(true, cx))),
                )
                .child(
                    IconButton::new("find-close", IconName::Close)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Close (Escape)"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
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
        if text.is_empty() && (self.attachments.is_empty() || self.running.is_some()) {
            return;
        }
        // Mid-turn, Enter steers the running turn (Hermes's `/steer`), queues the message for after
        // it, or stops it and sends this instead (Settings, Hermes's `busy_input_mode`).
        // `/steer <text>` steers whatever the setting says; with nothing running it's a message.
        let (text, force_steer) = match command_arg(&text, "steer") {
            Some(steer) if !steer.is_empty() => (steer.to_string(), true),
            _ => (text, false),
        };
        if self.running.is_some() {
            let mode = if force_steer {
                config::BusyInput::Steer
            } else {
                AppState::global(cx).read(cx).config.busy_input_mode
            };
            match mode {
                config::BusyInput::Queue => {
                    self.composer
                        .update(cx, |composer, cx| composer.set_text("", cx));
                    self.queued.push(text);
                    cx.notify();
                    return;
                }
                config::BusyInput::Interrupt => {
                    self.composer
                        .update(cx, |composer, cx| composer.set_text("", cx));
                    self.next_message = Some(text);
                    self.stop(cx);
                    return;
                }
                config::BusyInput::Steer => {}
            }
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

    /// Hermes's `/queue`: `<prompt>` queues it for after the running turn (runs it now when
    /// nothing runs); `list`, `edit N <prompt>`, `rm N`, `move FROM TO` and `clear` manage the queue.
    /// Returns what to tell the user, or the prompt to send now.
    fn queue_command(&mut self, args: &str) -> Result<String, String> {
        let preview = |text: &str| {
            let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if line.chars().count() > 60 {
                format!("{}…", line.chars().take(60).collect::<String>())
            } else {
                line
            }
        };
        let (sub, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let rest = rest.trim();
        let index = |n: &str, size: usize| -> Result<usize, String> {
            n.parse::<usize>()
                .ok()
                .filter(|&i| (1..=size).contains(&i))
                .map(|i| i - 1)
                .ok_or_else(|| format!("Queue item {n} not found. Current size: {size}"))
        };
        Ok(match sub {
            "" => QUEUE_USAGE.into(),
            "list" | "ls" => {
                if self.queued.is_empty() {
                    "Queue is empty.".into()
                } else {
                    let mut lines = vec![format!("Queue ({} pending):", self.queued.len())];
                    lines.extend(
                        self.queued
                            .iter()
                            .enumerate()
                            .map(|(i, text)| format!("{}. {}", i + 1, preview(text))),
                    );
                    lines.join("\n")
                }
            }
            "clear" => {
                let count = std::mem::take(&mut self.queued).len();
                format!(
                    "Cleared {count} queued prompt{}.",
                    if count == 1 { "" } else { "s" }
                )
            }
            "rm" | "remove" => match index(rest, self.queued.len()) {
                Ok(i) => {
                    let removed = self.queued.remove(i);
                    format!("Removed queue item {}: {}", i + 1, preview(&removed))
                }
                Err(error) => error,
            },
            "edit" => {
                let (n, prompt) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                if prompt.trim().is_empty() {
                    return Ok("Usage: /queue edit <number> <new prompt>".into());
                }
                match index(n, self.queued.len()) {
                    Ok(i) => {
                        self.queued[i] = prompt.trim().to_string();
                        format!("Edited queue item {}: {}", i + 1, preview(prompt))
                    }
                    Err(error) => error,
                }
            }
            "move" | "mv" => {
                let mut numbers = rest.split_whitespace();
                let size = self.queued.len();
                match (
                    numbers.next().map(|n| index(n, size)),
                    numbers.next().map(|n| index(n, size)),
                ) {
                    (Some(Ok(from)), Some(Ok(to))) => {
                        let item = self.queued.remove(from);
                        self.queued.insert(to, item);
                        format!("Moved queue item {} to {}.", from + 1, to + 1)
                    }
                    (Some(Err(error)), _) | (_, Some(Err(error))) => error,
                    _ => QUEUE_USAGE.into(),
                }
            }
            _ => {
                let prompt = args.trim().to_string();
                if self.running.is_none() {
                    return Err(prompt);
                }
                let shown = preview(&prompt);
                self.queued.push(prompt);
                format!("Queued: {shown}")
            }
        })
    }

    /// Takes a waiting message out of the queue (or withdraws the undelivered steer).
    fn take_pending(&mut self, entry: PendingEntry, cx: &mut Context<Self>) -> Option<String> {
        self.drop_pending_edit();
        match entry {
            PendingEntry::Steer => {
                let text = self.steering.take()?;
                backend(cx).runtime.set_steer(&self.session_id, None);
                Some(text)
            }
            PendingEntry::Queued(index) => {
                (index < self.queued.len()).then(|| self.queued.remove(index))
            }
        }
    }

    /// Zed's queue editing: the waiting message turns into an editor where it is; Enter saves.
    fn edit_pending(&mut self, entry: PendingEntry, window: &mut Window, cx: &mut Context<Self>) {
        let text = match entry {
            PendingEntry::Steer => self.steering.clone(),
            PendingEntry::Queued(index) => self.queued.get(index).cloned(),
        };
        if let Some(text) = text {
            self.start_inline_edit(EditTarget::Pending(entry), text, window, cx);
        }
    }

    /// The queue's positions shift when an entry leaves it: an open edit of one is dropped.
    fn drop_pending_edit(&mut self) {
        if self
            .inline_edit
            .as_ref()
            .is_some_and(|e| matches!(e.target, EditTarget::Pending(_)))
        {
            self.inline_edit = None;
        }
    }

    /// Zed's Steer toggle: a queued message is delivered after the running tool call instead of
    /// after the turn, or the other way round.
    fn toggle_steer(&mut self, entry: PendingEntry, cx: &mut Context<Self>) {
        match entry {
            PendingEntry::Steer => {
                if let Some(text) = self.take_pending(entry, cx) {
                    self.queued.insert(0, text);
                }
            }
            PendingEntry::Queued(index) => {
                let Some(text) = self.queued.get(index).cloned() else {
                    return;
                };
                if backend(cx).runtime.steer(&self.session_id, &text) {
                    self.drop_pending_edit();
                    self.queued.remove(index);
                    self.steering = Some(match self.steering.take() {
                        Some(earlier) => format!("{earlier}\n\n{text}"),
                        None => text,
                    });
                }
            }
        }
        cx.notify();
    }

    /// Zed's Send Now: stops the running turn and sends this message instead.
    fn send_pending_now(&mut self, entry: PendingEntry, cx: &mut Context<Self>) {
        let Some(text) = self.take_pending(entry, cx) else {
            return;
        };
        if self.running.is_some() {
            self.next_message = Some(text);
            self.stop(cx);
        } else {
            self.begin(Some(text), cx);
        }
        cx.notify();
    }

    /// Zed's message queue over the composer: a summary that folds the list, then each waiting
    /// message (the undelivered steer first) with Remove, Edit, Steer and Send Now.
    fn render_queue(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let entries: Vec<(PendingEntry, String)> = self
            .steering
            .iter()
            .map(|text| (PendingEntry::Steer, text.clone()))
            .chain(
                self.queued
                    .iter()
                    .enumerate()
                    .map(|(index, text)| (PendingEntry::Queued(index), text.clone())),
            )
            .collect();
        let count = entries.len();
        let title = if count == 1 {
            "1 Queued Message".to_string()
        } else {
            format!("{count} Queued Messages")
        };
        v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.editor_background)
            .overflow_hidden()
            .child(
                h_flex()
                    .p_1()
                    .w_full()
                    .gap_1()
                    .justify_between()
                    .when(self.queue_expanded, |this| {
                        this.border_b_1().border_color(colors.border)
                    })
                    .child(
                        h_flex()
                            .id("queue_summary")
                            .gap_1()
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(colors.ghost_element_hover))
                            .child(Disclosure::new("queue_disclosure", self.queue_expanded))
                            .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queue_expanded = !this.queue_expanded;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("clear_queue", "Clear All")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queued.clear();
                                this.take_pending(PendingEntry::Steer, cx);
                                cx.notify();
                            })),
                    ),
            )
            .when(self.queue_expanded, |this| {
                this.child(
                    v_flex()
                        .id("message_queue_list")
                        .max_h_40()
                        .overflow_y_scroll()
                        .children(entries.into_iter().enumerate().map(|(row, (entry, text))| {
                            let is_next = row == 0;
                            let steer_on = entry == PendingEntry::Steer;
                            let line: String =
                                text.split_whitespace().collect::<Vec<_>>().join(" ");
                            let (icon_color, tooltip) = if steer_on {
                                (Color::Accent, "Delivered after the running tool call")
                            } else if is_next {
                                (Color::Accent, "Next in Queue")
                            } else {
                                (Color::Muted, "In Queue")
                            };
                            h_flex()
                                .group("queue_entry")
                                .w_full()
                                .p_1p5()
                                .gap_1()
                                .when(row + 1 < count, |this| {
                                    this.border_b_1().border_color(colors.border_variant)
                                })
                                .child(
                                    div()
                                        .id(("next_in_queue", row))
                                        .flex_none()
                                        .child(
                                            Icon::new(IconName::Circle)
                                                .size(IconSize::Small)
                                                .color(icon_color),
                                        )
                                        .tooltip(Tooltip::text(tooltip)),
                                )
                                // Zed's queue entries are editors: click the text to change it,
                                // Enter saves, Escape (or leaving it unchanged) cancels.
                                .child(match self.inline_editor(EditTarget::Pending(entry)) {
                                    Some(editor) => div()
                                        .flex_1()
                                        .min_w_0()
                                        .px_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.border_focused)
                                        .text_sm()
                                        .on_key_down(cx.listener(
                                            |this, event: &gpui::KeyDownEvent, window, cx| {
                                                if event.keystroke.key == "escape" {
                                                    this.finish_inline_edit(false, window, cx);
                                                    cx.stop_propagation();
                                                }
                                            },
                                        ))
                                        .child(editor)
                                        .into_any_element(),
                                    None => div()
                                        .id(("queued-text", row))
                                        .flex_1()
                                        .min_w_0()
                                        .cursor_text()
                                        .child(Label::new(line).size(LabelSize::Small).truncate())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.edit_pending(entry, window, cx)
                                        }))
                                        .into_any_element(),
                                })
                                .child(
                                    h_flex()
                                        .when(!is_next, |this| this.visible_on_hover("queue_entry"))
                                        .gap_1()
                                        .flex_none()
                                        .child(
                                            IconButton::new(("delete", row), IconName::Trash)
                                                .icon_size(IconSize::Small)
                                                .tooltip(Tooltip::text("Remove Message from Queue"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.take_pending(entry, cx);
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            IconButton::new(("edit", row), IconName::Pencil)
                                                .icon_size(IconSize::Small)
                                                .tooltip(Tooltip::text("Edit"))
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.edit_pending(entry, window, cx)
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(("steer", row), "Steer")
                                                .label_size(LabelSize::Small)
                                                .toggle_state(steer_on)
                                                .selected_style(ButtonStyle::Tinted(
                                                    TintColor::Accent,
                                                ))
                                                .disabled(self.running.is_none())
                                                .tooltip(Tooltip::text(
                                                    "Deliver this message right after the agent's current tool call. When off, it waits for the turn to finish.",
                                                ))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_steer(entry, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(("send_now", row), "Send Now")
                                                .label_size(LabelSize::Small)
                                                .style(ButtonStyle::Outlined)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.send_pending_now(entry, cx)
                                                })),
                                        ),
                                )
                        })),
                )
            })
    }

    pub fn start_turn(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.begin(Some(text), cx);
    }

    /// What the user sent in this session, newest first (as typed), without repeats in a row.
    fn history(&self) -> Vec<String> {
        let mut sent: Vec<String> = self
            .conversation
            .messages()
            .iter()
            .rev()
            .filter(|m| m.role == Role::User && m.is_user_visible() && !m.is_tool_response())
            .map(shown_user_text)
            .filter(|text| !text.trim().is_empty())
            .collect();
        sent.dedup();
        sent
    }

    /// ↑ / ↓ at the composer's edge: steps through earlier messages when the composer is empty
    /// or already shows one; ↓ past the newest clears it again.
    fn step_history(&mut self, back: bool, cx: &mut Context<Self>) {
        let empty = self.composer.read(cx).text().trim().is_empty();
        if self.history_index.is_none() && !empty {
            return;
        }
        let history = self.history();
        let next = match (self.history_index, back) {
            (None, true) => Some(0),
            (None, false) => return,
            (Some(i), true) => Some((i + 1).min(history.len().saturating_sub(1))),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        let text = next
            .and_then(|i| history.get(i))
            .cloned()
            .unwrap_or_default();
        if next.is_some() && text.is_empty() {
            return;
        }
        self.history_index = next;
        self.composer.update(cx, |c, cx| c.set_text(text, cx));
        cx.notify();
    }

    /// Adds an image to the next message; anything that isn't an image Hermes can read is refused.
    fn attach_bytes(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        match image_format(&bytes) {
            Some(format) => {
                self.attachments
                    .push(Arc::new(gpui::Image::from_bytes(format, bytes)));
                self.error = None;
            }
            None => {
                self.error = Some(
                    "That isn't an image Hermes can attach (PNG, JPEG, GIF, WebP, BMP or TIFF)."
                        .into(),
                )
            }
        }
        cx.notify();
    }

    /// Attaches the image files among `paths` (dropped, pasted or picked); other files and folders
    /// become `@file:` / `@folder:` mentions (relative to the session folder when inside it).
    fn attach_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let mut mentions = Vec::new();
        for path in paths {
            if !hermes_tools::vision::is_image_path(&path) {
                let shown = self
                    .cwd
                    .as_deref()
                    .and_then(|cwd| path.strip_prefix(cwd).ok())
                    .map(|relative| relative.display().to_string())
                    .unwrap_or_else(|| path.display().to_string())
                    .replace('\\', "/");
                let kind = if path.is_dir() { "folder" } else { "file" };
                mentions.push(format!(
                    "@{kind}:{}",
                    hermes_tools::references::format_value(&shown)
                ));
                continue;
            }
            match std::fs::read(&path) {
                Ok(bytes) => self.attach_bytes(bytes, cx),
                Err(error) => {
                    self.error = Some(format!("Could not read {}: {error}", path.display()));
                    cx.notify();
                }
            }
        }
        if !mentions.is_empty() {
            let current = self.composer.read(cx).text().to_string();
            let separator = if current.is_empty() || current.ends_with(char::is_whitespace) {
                ""
            } else {
                " "
            };
            let text = format!("{current}{separator}{} ", mentions.join(" "));
            self.composer.update(cx, |c, cx| c.set_text(text, cx));
            cx.notify();
        }
    }

    /// The `+` menu's Image entry: a file picker for images.
    fn pick_images(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| this.attach_paths(paths, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// While the composer ends in an `@` mention: the reference kinds it could start, or the
    /// files (folders) whose path contains what follows `@file:` (`@folder:`). `None` otherwise.
    fn mention_matches(&mut self, text: &str) -> Option<Vec<Suggestion>> {
        let start = text.rfind(char::is_whitespace).map_or(0, |index| {
            index + text[index..].chars().next().map_or(1, char::len_utf8)
        });
        let (before, token) = text.split_at(start);
        if !token.starts_with('@') {
            return None;
        }
        // `@file:x` / `@folder:x` look among that kind; a bare `@x` among folders, then files,
        // then Hermes's other reference kinds.
        let (only, partial) = match token.split_once(':') {
            Some(("@file", partial)) => (Some(false), partial),
            Some(("@folder", partial)) => (Some(true), partial),
            Some(_) => return Some(Vec::new()),
            None => (None, &token[1..]),
        };
        let query = partial.trim_start_matches(['`', '"', '\'']).to_lowercase();
        let mut suggestions: Vec<Suggestion> = match self.cwd.clone() {
            Some(cwd) => {
                let index = self.file_index.get_or_insert_with(|| {
                    hermes_tools::references::workspace_entries(std::path::Path::new(&cwd), 20_000)
                });
                rank_entries(index, &query, only)
                    .into_iter()
                    .take(10)
                    .map(|(path, is_dir)| {
                        let kind = if is_dir { "@folder" } else { "@file" };
                        Suggestion {
                            label: path.clone(),
                            description: String::new(),
                            text: format!(
                                "{before}{kind}:{} ",
                                hermes_tools::references::format_value(&path)
                            ),
                            icon: Some(if is_dir {
                                IconName::Folder
                            } else {
                                IconName::File
                            }),
                        }
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        if only.is_none() {
            suggestions.extend(
                MENTION_KINDS
                    .iter()
                    .filter(|(name, _)| !matches!(*name, "@file:" | "@folder:"))
                    .filter(|(name, _)| name[1..].starts_with(&query))
                    .map(|(name, description)| Suggestion {
                        label: name.to_string(),
                        description: description.to_string(),
                        text: if name.ends_with(':') {
                            format!("{before}{name}")
                        } else {
                            format!("{before}{name} ")
                        },
                        icon: Some(IconName::AtSign),
                    }),
            );
        }
        Some(suggestions)
    }

    /// Opens what a composer chip names: a file or folder with Windows, a skill's SKILL.md.
    fn open_mention(&mut self, target: &str, cx: &mut Context<Self>) {
        let path = match target.split_once(':') {
            Some(("file" | "folder", path)) => {
                let path = std::path::PathBuf::from(path);
                match &self.cwd {
                    Some(cwd) if path.is_relative() => std::path::Path::new(cwd).join(path),
                    _ => path,
                }
            }
            Some(("skill", name)) => match backend(cx).runtime.skills().find(name) {
                Some(skill) => skill.skill_md(),
                None => return,
            },
            _ => return,
        };
        if path.exists() {
            cx.open_with_system(&path);
        } else {
            self.error = Some(format!("{} doesn't exist", path.display()));
            cx.notify();
        }
    }

    /// Tab (or Enter on an `@` mention): the highlighted suggestion goes into the composer.
    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggestion) = self.slash_matches.get(self.suggestion_selected).cloned() else {
            return;
        };
        self.composer.update(cx, |composer, cx| {
            let len = composer.text().len();
            composer.replace_range(0..len, &suggestion.text, cx);
        });
        self.composer.focus_handle(cx).focus(window, cx);
        // A kind like `@file:` opens the next list; anything finished closes it.
        self.update_slash_matches(cx);
        if suggestion.text.ends_with(' ') {
            self.slash_matches.clear();
        }
        cx.notify();
    }

    /// Keys while suggestions show: ↑/↓ pick, Tab takes, Escape closes; Enter takes an `@`
    /// mention or an unfinished command (a finished one is sent as typed).
    fn suggestion_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Up, Down and Enter are the text area's own bindings, which run before key listeners;
        // they arrive as its HistoryPrev / HistoryNext / Submit events instead.
        if self.slash_matches.is_empty() || event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "tab" => self.accept_suggestion(window, cx),
            "escape" => self.slash_matches.clear(),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// ↑ / ↓ while suggestions show: the highlight moves (wrapping). `false` when none show.
    fn move_suggestion(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let count = self.slash_matches.len();
        if count == 0 {
            return false;
        }
        self.suggestion_selected = if down {
            (self.suggestion_selected + 1) % count
        } else {
            (self.suggestion_selected + count - 1) % count
        };
        cx.notify();
        true
    }

    /// Enter while suggestions show takes the highlighted one, except on a finished command
    /// (`/usage` typed in full is sent). `false` when Enter should send.
    fn enter_takes_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(chosen) = self.slash_matches.get(self.suggestion_selected) else {
            return false;
        };
        let text = self.composer.read(cx).text().trim().to_string();
        if text.starts_with('/') && chosen.label == text {
            return false;
        }
        self.accept_suggestion(window, cx);
        true
    }

    /// While the composer holds a bare `/word`, the skills whose name contains it.
    fn update_slash_matches(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().to_string();
        let query = text
            .strip_prefix('/')
            .filter(|q| !q.contains(char::is_whitespace))
            .map(str::to_lowercase);
        if let Some(mentions) = self.mention_matches(&text) {
            if mentions != self.slash_matches {
                self.slash_matches = mentions;
                self.suggestion_selected = 0;
                cx.notify();
            }
            return;
        }
        self.file_index = None;
        if text.starts_with('/') && self.known_commands.borrow().is_empty() {
            let skills = backend(cx).runtime.skills();
            let mut known = self.known_commands.borrow_mut();
            for (name, _) in BUILTIN_COMMANDS {
                known.insert(name.to_string(), format!("command:{name}"));
            }
            for bundle in skills.bundles() {
                known.insert(bundle.slug.clone(), format!("bundle:{}", bundle.slug));
            }
            for skill in skills.catalog() {
                known.insert(skill.name.clone(), format!("skill:{}", skill.name));
            }
        }
        let matches: Vec<Suggestion> = match query {
            Some(query) => BUILTIN_COMMANDS
                .iter()
                .filter(|(name, _)| name.contains(query.as_str()))
                .map(|(name, description)| (name.to_string(), description.to_string()))
                .chain(
                    backend(cx)
                        .runtime
                        .skills()
                        .bundles()
                        .into_iter()
                        .filter(|b| b.slug.contains(&query))
                        .map(|b| (b.slug, format!("Bundle: {}", b.description))),
                )
                .chain(
                    backend(cx)
                        .runtime
                        .skills()
                        .catalog()
                        .into_iter()
                        .filter(|s| s.name.contains(&query))
                        .map(|s| {
                            (
                                s.name,
                                hermes_tools::skills::clip_description(&s.description),
                            )
                        }),
                )
                .take(6)
                .map(|(name, description)| Suggestion {
                    label: format!("/{name}"),
                    description,
                    text: format!("/{name} "),
                    icon: None,
                })
                .collect(),
            None => Vec::new(),
        };
        if matches != self.slash_matches {
            self.slash_matches = matches;
            self.suggestion_selected = 0;
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

    /// Zed's in-place editing: the sent message becomes an editor where it is.
    fn edit_message(&mut self, message_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let text = shown_user_text(&self.conversation.messages()[message_index]);
        self.start_inline_edit(EditTarget::Sent(message_index), text, window, cx);
    }

    /// Opens an in-place editor on a sent or waiting message, the caret at the end.
    fn start_inline_edit(
        &mut self,
        target: EditTarget,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inline_edit.as_ref().is_some_and(|e| e.target == target) {
            if let Some(edit) = &self.inline_edit {
                edit.editor.focus_handle(cx).focus(window, cx);
            }
            return;
        }
        let editor = cx.new(|cx| {
            let mut editor = TextArea::new("", cx)
                .with_decorations(composer_decorations(self.known_commands.clone()));
            editor.set_text(text.clone(), cx);
            editor
        });
        let focus = editor.focus_handle(cx);
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                TextAreaEvent::Submit => this.finish_inline_edit(true, window, cx),
                TextAreaEvent::MentionClicked(target) => this.open_mention(target, cx),
                _ => {
                    // The editor grows as it's typed in: its list row is measured again.
                    if let Some(EditTarget::Sent(index)) = this.inline_edit.as_ref().map(|e| e.target)
                    {
                        if let Some(row) = this.entries.iter().position(|&m| m == index) {
                            this.list_state.remeasure_items(row..row + 1);
                        }
                    }
                    cx.notify()
                }
            }),
            // Zed: leaving an unchanged message ends its editing; a changed one stays open.
            cx.on_focus_out(&focus, window, |this, _, window, cx| {
                let unchanged = this
                    .inline_edit
                    .as_ref()
                    .is_some_and(|e| e.editor.read(cx).text() == e.original);
                if unchanged {
                    this.finish_inline_edit(false, window, cx);
                } else {
                    cx.notify();
                }
            }),
        ];
        focus.focus(window, cx);
        self.inline_edit = Some(InlineEdit {
            target,
            editor,
            original: text,
            _subscriptions: subscriptions,
        });
        self.list_state.remeasure_items(0..self.list_len());
        cx.notify();
    }

    /// Ends in-place editing: `apply` sends the edited message again (a sent one restarts the
    /// thread from it) or saves a waiting one; otherwise the edit is dropped.
    fn finish_inline_edit(&mut self, apply: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.inline_edit.take() else {
            return;
        };
        let text = edit.editor.read(cx).text().trim().to_string();
        self.list_state.remeasure_items(0..self.list_len());
        if apply && !text.is_empty() {
            match edit.target {
                EditTarget::Sent(index) => {
                    if self.running.is_none() {
                        self.resend(index, Some(text), cx);
                    }
                }
                EditTarget::Pending(PendingEntry::Steer) => {
                    if self.steering.is_some()
                        && backend(cx).runtime.set_steer(&self.session_id, Some(&text))
                    {
                        self.steering = Some(text);
                    }
                }
                EditTarget::Pending(PendingEntry::Queued(index)) => {
                    if let Some(queued) = self.queued.get_mut(index) {
                        *queued = text;
                    }
                }
            }
        }
        self.composer.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The in-place editor, when it's open on `target`.
    fn inline_editor(&self, target: EditTarget) -> Option<Entity<TextArea>> {
        self.inline_edit
            .as_ref()
            .filter(|e| e.target == target)
            .map(|e| e.editor.clone())
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
                    supports_vision: state.supports_vision(&self.model),
                    aux: state.aux_models(),
                },
            )
        };
        let Some(provider) = provider else {
            self.error = Some("No provider is configured. Open Settings to add one.".into());
            cx.notify();
            return;
        };
        let backend = backend(cx);
        // Hermes's `/refine [focus]` reviews the conversation in the background instead of
        // starting a turn; `/learn <request>` starts one with the learn prompt.
        // Hermes's `/yolo`: approval prompts off (or back on) for this session.
        if text
            .as_deref()
            .and_then(|t| command_arg(t, "yolo"))
            .is_some()
        {
            match backend.runtime.toggle_yolo(&self.session_id) {
                Ok(true) => self
                    .notices
                    .push("⚡ YOLO mode ON — all commands auto-approved. Use with caution.".into()),
                Ok(false) => self
                    .notices
                    .push("⚠ YOLO mode OFF — dangerous commands will require approval.".into()),
                Err(error) => self.error = Some(format!("{error:#}")),
            }
            self.trailer_changed(cx);
            return;
        }
        // Hermes's `/queue` manages prompts waiting for the running turn.
        let text = match text.as_deref().and_then(|t| command_arg(t, "queue")) {
            Some(args) => match self.queue_command(args) {
                Ok(notice) => {
                    self.notices.push(notice);
                    self.trailer_changed(cx);
                    return;
                }
                Err(prompt) => Some(prompt),
            },
            None => text,
        };
        // Hermes's `/rollback` and `/diff` report in the chat instead of starting a turn.
        let local = text.as_deref().and_then(|t| {
            ["rollback", "diff", "undo", "usage", "insights"]
                .into_iter()
                .find_map(|name| command_arg(t, name).map(|args| (name, args)))
        });
        if let Some((command, args)) = local {
            let runtime = backend.runtime.clone();
            let context_length = AppState::global(cx).read(cx).context_length(&self.model);
            let mut restore = None;
            let result = match command {
                "rollback" => runtime.rollback(&self.session_id, args),
                "undo" => runtime
                    .undo(&self.session_id, args)
                    .map(|(messages, typed)| {
                        restore = typed;
                        messages
                    }),
                "usage" => runtime.usage(&self.session_id, context_length),
                "insights" => runtime.insights(&self.session_id, args),
                _ => runtime.diff(&self.session_id, args),
            };
            // `/undo` puts the undone message back in the box.
            if let Some(typed) = restore {
                self.composer.update(cx, |c, cx| c.set_text(typed, cx));
            }
            match result {
                Ok(messages) => {
                    self.conversation = Conversation::new_unvalidated(messages);
                    self.error = None;
                    self.rebuild_entries();
                    self.list_state.reset(self.list_len());
                    self.list_state.scroll_to_end();
                    cx.emit(ChatEvent::Updated);
                }
                Err(error) => self.error = Some(format!("{error:#}")),
            }
            cx.notify();
            return;
        }
        if let Some(focus) = text.as_deref().and_then(|t| command_arg(t, "refine")) {
            let focus = (!focus.is_empty()).then(|| focus.to_string());
            let started = {
                let _guard = backend.tokio.enter();
                backend
                    .runtime
                    .refine(&self.session_id, &provider, options, focus.clone())
            };
            match started {
                Ok(()) => {
                    let focus = focus.map(|f| format!(" (focus: {f})")).unwrap_or_default();
                    self.notices.push(format!(
                        "Reviewing this conversation in the background{focus} — any memory/skill updates will be reported when done."
                    ));
                    self.error = None;
                    self.trailer_changed(cx);
                }
                Err(error) => {
                    self.error = Some(format!("{error:#}"));
                    cx.notify();
                }
            }
            return;
        }
        // Hermes's `/compress [focus]` summarizes the conversation now instead of starting a turn.
        let compress = text
            .as_deref()
            .and_then(|t| command_arg(t, "compress"))
            .map(|focus| (!focus.is_empty()).then(|| focus.to_string()));
        let text = match text {
            _ if compress.is_some() => None,
            Some(text) if command_arg(&text, "learn").is_some() => {
                let request = command_arg(&text, "learn").unwrap_or_default();
                Some(hermes_prompts::review::learn_message(request))
            }
            // `/skill-name instruction` sends the message that loads the skill.
            Some(text) if text.starts_with('/') => match backend.runtime.skill_command(&text) {
                Some(message) => Some(message),
                None => {
                    let name = text[1..]
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    self.error = Some(format!(
                        "There is no skill or bundle named '{name}'. Open Skills (Ctrl+Shift+K) to see them."
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
                _ if compress.is_some() => backend.runtime.compress(
                    &self.session_id,
                    &provider,
                    options,
                    compress.clone().flatten(),
                ),
                Some(text) => {
                    let images: Vec<Vec<u8>> = std::mem::take(&mut self.attachments)
                        .iter()
                        .map(|image| image.bytes.clone())
                        .collect();
                    backend.runtime.start_turn_with_images(
                        &self.session_id,
                        &provider,
                        options,
                        text,
                        images,
                    )
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
        self.notices.clear();
        self.running = Some(turn.cancel.clone());
        self.manual_compress = compress.is_some();
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
        // Stopping denies a command waiting for approval.
        if let Some((_, Some(reply))) = self.approval.take() {
            let _ = reply.send(hermes_tools::approval::ApprovalChoice::Deny);
        }
        cx.notify();
    }

    /// Zed's permission buttons (`render_permission_buttons`) under a flagged command: why it was
    /// flagged, then Allow Once / for This Session / Always and Deny (Hermes's four choices).
    fn render_permission(&self, description: &str, id: &str, cx: &Context<Self>) -> AnyElement {
        use hermes_tools::approval::ApprovalChoice;
        let option =
            |label: String, key: &str, icon: IconName, color: Color, choice: ApprovalChoice| {
                Button::new(SharedString::from(format!("{key}-{id}")), label)
                    .full_width()
                    .start_icon(Icon::new(icon).size(IconSize::XSmall).color(color))
                    .on_click(cx.listener(move |this, _, _, cx| this.answer_approval(choice, cx)))
            };
        v_flex()
            .p_1()
            .gap_0p5()
            .w_full()
            .border_t_1()
            .border_color(tool_card_border_color(cx))
            .child(
                h_flex()
                    .px_1p5()
                    .py_1()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::XSmall)
                            .color(Color::Warning),
                    )
                    .child(
                        Label::new(format!("Needs your approval: {description}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(option(
                "Allow Once".into(),
                "allow-once",
                IconName::Check,
                Color::Success,
                ApprovalChoice::Once,
            ))
            .child(option(
                "Allow for This Session".into(),
                "allow-session",
                IconName::Check,
                Color::Success,
                ApprovalChoice::Session,
            ))
            .child(option(
                format!("Always Allow: {description}"),
                "allow-always",
                IconName::CheckDouble,
                Color::Success,
                ApprovalChoice::Always,
            ))
            .child(option(
                "Deny".into(),
                "deny",
                IconName::Close,
                Color::Error,
                ApprovalChoice::Deny,
            ))
            .into_any_element()
    }

    /// A command matched a dangerous pattern: its card shows the approval buttons.
    pub fn show_approval(
        &mut self,
        request: hermes_tools::approval::CommandApproval,
        reply: oneshot::Sender<hermes_tools::approval::ApprovalChoice>,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, Some(previous))) = self.approval.replace((request, Some(reply))) {
            let _ = previous.send(hermes_tools::approval::ApprovalChoice::Deny);
        }
        self.list_state.remeasure_items(0..self.list_len());
        cx.emit(ChatEvent::NeedsApproval);
        cx.notify();
    }

    fn answer_approval(
        &mut self,
        choice: hermes_tools::approval::ApprovalChoice,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, Some(reply))) = self.approval.take() {
            let _ = reply.send(choice);
        }
        self.list_state.remeasure_items(0..self.list_len());
        cx.notify();
    }

    fn handle_event(&mut self, event: RuntimeEvent, cx: &mut Context<Self>) {
        match event {
            RuntimeEvent::Agent(AgentEvent::Message(message)) => {
                // The chat's copy of a delivered steer: shown to the user, not sent to the model.
                if message.role == Role::User && !message.is_agent_visible() {
                    self.steering = None;
                    self.drop_pending_edit();
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
            RuntimeEvent::Agent(AgentEvent::SubagentProgress {
                tool_call_id,
                task,
                text,
            }) => {
                self.subagent_progress
                    .entry(tool_call_id)
                    .or_default()
                    .insert(task, text);
                self.list_state.remeasure_items(0..self.list_len());
                cx.notify();
            }
            RuntimeEvent::Agent(AgentEvent::Compacting) => {
                self.compacting = true;
                cx.notify();
            }
            RuntimeEvent::Agent(AgentEvent::Compacted { tokens }) => {
                self.compacting = false;
                self.usage.last_input_tokens = i32::try_from(tokens).unwrap_or(i32::MAX);
                cx.notify();
            }
            RuntimeEvent::Agent(AgentEvent::CompactionFailed(error)) => {
                self.compacting = false;
                if let Some(error) = error {
                    self.notices
                        .push(format!("Context compression failed: {error}"));
                }
                cx.notify();
            }
            RuntimeEvent::Agent(_) => {}
            RuntimeEvent::SteerUndelivered(text) => {
                self.drop_pending_edit();
                self.steering = None;
                self.next_message = Some(text);
            }
            RuntimeEvent::TurnFinished(status) => {
                self.running = None;
                self.compacting = false;
                let manual_compress = std::mem::take(&mut self.manual_compress);
                self.composer.update(cx, |composer, cx| {
                    composer.set_placeholder(COMPOSER_PLACEHOLDER, cx)
                });
                // A finished turn hands over to the first queued prompt (not after a stop or an error).
                let queued = (self.next_message.is_none()
                    && status == TurnStatus::Completed
                    && !self.queued.is_empty())
                .then(|| self.queued.remove(0));
                if queued.is_some() {
                    self.drop_pending_edit();
                    self.next_message = queued;
                }
                let continues = self.next_message.is_some();
                let cancelled = matches!(status, TurnStatus::Cancelled);
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
                if !continues && !cancelled && !manual_compress {
                    cx.emit(ChatEvent::TurnFinished {
                        failed: self.error.is_some(),
                        used_tools: self.used_tools_since_last_user_message(),
                    });
                }
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
        cx.emit(ChatEvent::NeedsInput);
        cx.notify();
    }

    /// Hermes's "Self-improvement review" line: what a background review (or `/refine`) changed.
    pub fn review_finished(
        &mut self,
        actions: Vec<String>,
        requested: bool,
        cx: &mut Context<Self>,
    ) {
        let summary = if actions.is_empty() {
            debug_assert!(requested);
            "nothing to save.".to_string()
        } else {
            actions.join(" · ")
        };
        self.notices
            .push(format!("Self-improvement review: {summary}"));
        self.trailer_changed(cx);
    }

    /// Whether the agent waits on a question for the user (Zed's "waiting for confirmation").
    pub fn needs_input(&self) -> bool {
        self.clarify.is_some() || self.approval.is_some()
    }

    /// Whether the last turn failed (shown with Zed's error status in the sidebar).
    pub fn has_error(&self) -> bool {
        self.error.is_some() && self.running.is_none()
    }

    /// Whether the agent called tools since the last user message: Zed says "Finished running
    /// tools" then, "New message" otherwise.
    fn used_tools_since_last_user_message(&self) -> bool {
        self.conversation
            .messages()
            .iter()
            .rev()
            // Tool results are user-role messages too; the user's own message has text.
            .take_while(|message| {
                message.role != Role::User
                    || !message
                        .content
                        .iter()
                        .any(|content| matches!(content, MessageContent::Text(_)))
            })
            .any(|message| {
                message
                    .content
                    .iter()
                    .any(|content| matches!(content, MessageContent::ToolRequest(_)))
            })
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
            // A compaction stub is an agent-only copy; the chat shows the original output.
            .filter(|message| message.is_user_visible())
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
            } else if !self.notices.is_empty() {
                self.render_notices().into_any_element()
            } else {
                Empty.into_any_element()
            };
        };
        let message = self.conversation.messages()[message_index].clone();
        let is_last = index + 1 == self.entries.len();
        let compaction = hermes_runtime::compress::notice(&message);
        let content = match (&compaction, &message.role) {
            (Some(notice), _) => v_flex()
                .px_5()
                .py_1p5()
                .w_full()
                .text_ui(cx)
                .child(self.render_compaction(notice, &message, message_index, window, cx))
                .into_any_element(),
            (None, Role::User) => self.render_user_message(&message, message_index, window, cx),
            (None, Role::Assistant) => {
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
        // A compaction notice is neither: it closes the turn before it like a user message.
        let turn_end = message.role == Role::Assistant
            && compaction.is_none()
            && !is_command_output(&message)
            && self.entries.get(index + 1).is_none_or(|&next| {
                let next = &self.conversation.messages()[next];
                next.role == Role::User
                    || hermes_runtime::compress::notice(next).is_some()
                    || is_command_output(next)
            })
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
    /// The images a message carries, decoded once and kept.
    fn message_images(&mut self, message: &Message, message_index: usize) -> Vec<Arc<gpui::Image>> {
        use base64::Engine as _;
        message
            .content
            .iter()
            .enumerate()
            .filter_map(|(block, content)| {
                let MessageContent::Image(image) = content else {
                    return None;
                };
                let key = (
                    message
                        .id
                        .clone()
                        .unwrap_or_else(|| message_index.to_string()),
                    block,
                );
                if let Some(cached) = self.message_images.get(&key) {
                    return Some(cached.clone());
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(image.data.as_bytes())
                    .ok()?;
                let decoded = Arc::new(gpui::Image::from_bytes(image_format(&bytes)?, bytes));
                self.message_images.insert(key, decoded.clone());
                Some(decoded)
            })
            .collect()
    }

    fn render_user_message(
        &mut self,
        message: &Message,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // What was typed, not the skill it loaded or the context its references attached.
        let shown = shown_user_text(message);
        let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        let body = self.markdown_block((message_index, 0, MarkdownKind::User), &shown, style, cx);
        let images = self.message_images(message, message_index);
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
        let editor = self.inline_editor(EditTarget::Sent(message_index));
        let editor_focused = editor
            .as_ref()
            .is_some_and(|e| e.focus_handle(cx).is_focused(window));
        let editing = editor.is_some();
        // Zed's user message: the bubble is the editor; clicking it starts editing there.
        let content = match editor {
            // The message's own text size, so editing doesn't change how it looks.
            Some(editor) => div()
                .text_size(theme::theme_settings(cx).ui_font_size(cx))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.finish_inline_edit(false, window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(editor)
                .into_any_element(),
            None => body,
        };
        // The first message leaves room above it for the toolbar that sits on its top edge.
        let first = self.entries.first() == Some(&message_index);
        v_flex()
            .id(("user_message", message_index))
            .map(|this| if first { this.pt_4() } else { this.pt_2() })
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
                            .id(("user_message_bubble", message_index))
                            .py_3()
                            .px_2()
                            .rounded_md()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .map(|this| {
                                if editing && editor_focused {
                                    this.border_color(focus_border)
                                } else if editing {
                                    this.border_dashed()
                                } else if running {
                                    this.shadow_md()
                                } else {
                                    this.shadow_md()
                                        .cursor_text()
                                        .hover(|style| {
                                            style.border_color(focus_border.opacity(0.8))
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.edit_message(message_index, window, cx)
                                        }))
                                }
                            })
                            .child(content)
                            .when(!images.is_empty(), |this| {
                                this.child(h_flex().pt_2().gap_2().flex_wrap().children(
                                    images.into_iter().map(|image| {
                                        div()
                                            .rounded_md()
                                            .overflow_hidden()
                                            .border_1()
                                            .border_color(colors.border)
                                            .child(
                                                gpui::img(image)
                                                    .max_w(px(240.))
                                                    .max_h(px(180.))
                                                    .object_fit(gpui::ObjectFit::Contain),
                                            )
                                    }),
                                ))
                            }),
                    )
                    // Zed's editing toolbar: cancel, or send the edit (restarting from here).
                    .when(editing, |this| {
                        this.child(
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
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("cancel-edit-{message_index}")),
                                        IconName::Close,
                                    )
                                    .icon_color(Color::Error)
                                    .icon_size(IconSize::XSmall)
                                    .tooltip(Tooltip::text("Cancel Editing (Escape)"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.finish_inline_edit(false, window, cx)
                                    })),
                                )
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("regenerate-{message_index}")),
                                        IconName::Return,
                                    )
                                    .icon_color(Color::Muted)
                                    .icon_size(IconSize::XSmall)
                                    .tooltip(Tooltip::text(
                                        "Editing will restart the thread from this point.",
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.finish_inline_edit(true, window, cx)
                                    })),
                                ),
                        )
                    })
                    .when(!editing, |this| this.child(
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
                    )),
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
        self.highlight_matches(key, &markdown, cx);
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
        // Expanding changes the entry's height: remeasure, keeping the scroll position (a reset
        // would drop it and jump to the top).
        self.list_state.remeasure_items(0..self.list_len());
        cx.notify();
    }

    /// Zed's thinking block in its default "auto" display: a plain "Thinking" header (the
    /// chevron shows on hover), opened while the thought streams and closed when it ends,
    /// unless the user opened or closed it by hand.
    /// Where the context was compacted: a muted row like Thinking's ("Context compacted · …"),
    /// opening into the summary the model now reads in place of the earlier messages.
    fn render_compaction(
        &mut self,
        notice: &hermes_runtime::compress::CompactionNotice,
        message: &Message,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("compaction-{message_index}");
        let open = self.expanded.contains(&key);
        let body = open.then(|| {
            let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
            self.markdown_block(
                (message_index, 0, MarkdownKind::Text),
                message.as_concat_text().trim(),
                style,
                cx,
            )
        });
        let tokens = |n: usize| chat_ui::humanize_token_count(n as u64);
        let mut label = format!(
            "Context compacted · {} message{} summarized · {} → {} tokens",
            notice.summarized,
            if notice.summarized == 1 { "" } else { "s" },
            tokens(notice.tokens_before),
            tokens(notice.tokens_after),
        );
        if let Some(focus) = &notice.focus {
            label.push_str(&format!(" · focus: {focus}"));
        }
        let colors = cx.theme().colors();
        let header_group = SharedString::from(format!("{key}-header"));
        let toggle = {
            let key = key.clone();
            move |this: &mut Self, cx: &mut Context<Self>| {
                if open {
                    this.expanded.remove(&key);
                } else {
                    this.expanded.insert(key.clone());
                }
                this.list_state.remeasure_items(0..this.list_len());
                cx.notify();
            }
        };
        let header_toggle = toggle.clone();
        v_flex()
            .id(SharedString::from(format!("{key}-block")))
            .gap_1()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{key}-row")))
                    .group(header_group.clone())
                    .w_full()
                    .pr_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .child(
                        h_flex()
                            .h(window.line_height() - px(2.))
                            .gap_1p5()
                            .overflow_hidden()
                            .child(
                                Icon::new(IconName::FoldVertical)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                div()
                                    .text_size(rems_from_px(13_f32))
                                    .text_color(colors.text_muted)
                                    .child(label),
                            )
                            .child(
                                Disclosure::new(SharedString::from(format!("{key}-expand")), open)
                                    .opened_icon(IconName::ChevronUp)
                                    .closed_icon(IconName::ChevronDown)
                                    .visible_on_hover(header_group)
                                    .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx))),
                            ),
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
        // Closed until opened; the label shimmers while the thought streams.
        let open = self.expanded.contains(&key);
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
                if open {
                    this.expanded.remove(&key);
                } else {
                    this.expanded.insert(key.clone());
                }
                this.list_state.remeasure_items(0..this.list_len());
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
                    .rounded_sm()
                    .cursor_pointer()
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
                            .child(if active {
                                shimmer(
                                    SharedString::from(format!("{key}-shimmer")),
                                    "Thinking",
                                    cx,
                                )
                            } else {
                                div()
                                    .text_size(rems_from_px(13_f32))
                                    .text_color(colors.text_muted)
                                    .child("Thinking")
                                    .into_any_element()
                            })
                            .child(
                                Disclosure::new(SharedString::from(format!("{key}-expand")), open)
                                    .opened_icon(IconName::ChevronUp)
                                    .closed_icon(IconName::ChevronDown)
                                    .visible_on_hover(card_header)
                                    .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx))),
                            ),
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
            .child(
                h_flex()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(label),
            )
            // Zed puts the chevron right after the label.
            .when(response.is_some(), |this| {
                this.child(
                    div().flex_none().child(
                        Disclosure::new(SharedString::from(format!("{key}-expand")), open)
                            .opened_icon(IconName::ChevronUp)
                            .closed_icon(IconName::ChevronDown)
                            .visible_on_hover(card_header.clone()),
                    ),
                )
            })
            .child(div().flex_1())
            .when(!is_edit, |this| this.child(gradient_overlay));
        let toggle_key = key.clone();
        let header = h_flex()
            .id(SharedString::from(format!("{key}-header")))
            .group(card_header.clone())
            .relative()
            .w_full()
            .justify_between()
            .cursor_pointer()
            .rounded_sm()
            .when(use_card_layout, |this| {
                this.p_0p5().rounded_t(rems_from_px(5_f32)).bg(header_bg)
            })
            .child(label_row)
            .child(
                h_flex()
                    .pr_0p5()
                    .gap_1()
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
        // A running `delegate_task`: what each subagent is doing now.
        let progress = (name == "delegate_task" && response.is_none())
            .then(|| self.subagent_progress.get(&request.id))
            .flatten()
            .map(|tasks| {
                v_flex()
                    .ml(rems(0.4))
                    .px_3p5()
                    .py_1()
                    .gap_0p5()
                    .border_l_1()
                    .border_color(border_color)
                    .children(tasks.iter().map(|(task, line)| {
                        Label::new(format!("Subagent {} · {line}", task + 1))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate()
                    }))
            });
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
            .when_some(progress, |this, progress| this.child(progress))
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
        // Zed's permission buttons, on the card of the command waiting for approval.
        let permission = self
            .approval
            .as_ref()
            .filter(|(request, _)| response.is_none() && request.command == command)
            .map(|(request, _)| self.render_permission(&request.description, &id, cx));
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
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div().min_w_0().overflow_hidden().child(
                            Label::new(working_dir)
                                .buffer_font(cx)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate_start(),
                        ),
                    )
                    .child(
                        div().flex_none().child(
                            Disclosure::new(child_id("disclosure"), is_expanded)
                                .opened_icon(IconName::ChevronUp)
                                .closed_icon(IconName::ChevronDown)
                                .visible_on_hover(group.clone()),
                        ),
                    ),
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
            .children(permission)
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

    /// Background-review notes, under the thread like Hermes's review lines.
    fn render_notices(&self) -> impl IntoElement {
        v_flex()
            .px(rems_from_px(22_f32))
            .py_2()
            .gap_1()
            .children(self.notices.iter().map(|notice| {
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(notice.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
            }))
    }

    /// Zed's `render_generating`: the spinner under the thread while a turn runs (sand and
    /// "Awaiting Confirmation" while a question waits). The context ring in the composer shows
    /// the token count, so the elapsed-time and token labels Zed adds here are left out.
    fn render_generating(&self) -> impl IntoElement {
        let confirmation = self.clarify.is_some() || self.approval.is_some();
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
                    .when(self.compacting, |this| {
                        this.child(
                            div().min_w(rems(8.)).child(
                                LoadingLabel::new("Compressing context")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .single_line(),
                            ),
                        )
                    })
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
                    menu.key_context("AddContextMenu")
                        .item(
                            ContextMenuEntry::new("File")
                                .icon(IconName::File)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .handler(insert("@file:")),
                        )
                        .item(
                            ContextMenuEntry::new("Folder")
                                .icon(IconName::Folder)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .handler(insert("@folder:")),
                        )
                        .item(
                            ContextMenuEntry::new("Git Changes")
                                .icon(IconName::GitBranch)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
                                .handler(insert("@diff ")),
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
                                .handler({
                                    let chat = chat.clone();
                                    move |_, cx| {
                                        chat.update(cx, |chat, cx| chat.pick_images(cx)).ok();
                                    }
                                }),
                        )
                        .item(
                            ContextMenuEntry::new("URL")
                                .icon(IconName::ToolWeb)
                                .icon_color(Color::Muted)
                                .icon_size(IconSize::XSmall)
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
        if max == 0 {
            return None;
        }
        let runtime = backend(cx).runtime.clone();
        let session_id = self.session_id.clone();
        // No request measured yet (a reopened or just-compacted session): the estimate.
        let used = match self.usage.last_input_tokens {
            n if n > 0 => n as u64,
            _ => runtime
                .context_breakdown(&session_id)
                .map_or(0, |b| b.total() as u64),
        };
        let compress_at = SharedString::from(chat_ui::humanize_token_count(
            hermes_runtime::compress::Budget::new(Some(max as usize)).threshold as u64,
        ));
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
        let ring = CircularProgress::new(used as f32, max as f32, px(16.), cx)
            .stroke_width(px(2.))
            .progress_color(progress_color);
        Some(
            PopoverMenu::new("token-usage")
                .trigger(
                    ButtonLike::new("circular_progress_tokens")
                        .child(div().mt_px().child(ring)),
                )
                .anchor(gpui::Anchor::BottomRight)
                .offset(gpui::Point {
                    x: px(0.),
                    y: px(-4.),
                })
                .menu(move |_, cx| {
                    let (percentage, used, max, cost) = (
                        percentage.clone(),
                        used_label.clone(),
                        max_label.clone(),
                        cost.clone(),
                    );
                    // Estimated on hover: it walks the whole conversation.
                    let breakdown = runtime
                        .context_breakdown(&session_id)
                        .map(|b| {
                            [
                                ("System prompt", b.system_prompt),
                                ("Tool definitions", b.tool_definitions),
                                ("Skills", b.skills),
                                ("Memory", b.memory),
                                ("Conversation", b.conversation),
                            ]
                            .into_iter()
                            .filter(|&(part, tokens)| tokens > 0 || part == "Conversation")
                            .map(|(part, tokens)| {
                                (
                                    SharedString::from(part),
                                    SharedString::from(chat_ui::humanize_token_count(
                                        tokens as u64,
                                    )),
                                )
                            })
                            .collect()
                        })
                        .unwrap_or_default();
                    Some(cx.new(|cx| chat_ui::TokenUsageTooltip {
                        percentage,
                        used,
                        max,
                        cost,
                        breakdown,
                        compress_at: Some(compress_at.clone()),
                        focus_handle: cx.focus_handle(),
                    }))
                }),
        )
    }

    /// Zed's send button: Stop while a turn runs and the box is empty; with text during a turn
    /// it steers (Zed queues); disabled when there's nothing to send.
    fn render_send_button(&self, cx: &Context<Self>) -> AnyElement {
        let is_editor_empty = self.composer.read(cx).text().trim().is_empty();
        let is_generating = self.running.is_some();
        // Pending images alone can be sent (between turns).
        let nothing_to_send = is_editor_empty && self.attachments.is_empty();
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
                if nothing_to_send && !is_generating {
                    this.disabled(true).icon_color(Color::Muted)
                } else {
                    this.icon_color(Color::Accent)
                }
            })
            .tooltip(Tooltip::text(if nothing_to_send && !is_generating {
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
                        .rounded_sm()
                        .child(
                            Icon::new(IconName::ListTodo)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(format!("Tasks {done}/{}", todos.len()))
                                .size(LabelSize::Small),
                        )
                        .child(Disclosure::new("todos-disclosure", expanded))
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
    /// Thumbnails of the images going with the next message, each with a remove button; a note
    /// when the model can't see them.
    fn render_attachments(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let sees_images = AppState::global(cx).read(cx).supports_vision(&self.model);
        v_flex()
            .gap_1()
            .child(h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(
                |(index, image)| {
                    let group = SharedString::from(format!("attachment-{index}"));
                    div()
                        .id(group.clone())
                        .group(group.clone())
                        .relative()
                        .size(px(56.))
                        .rounded_md()
                        .overflow_hidden()
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            gpui::img(image.clone())
                                .size_full()
                                .object_fit(gpui::ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0p5()
                                .right_0p5()
                                .visible_on_hover(group)
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("remove-attachment-{index}")),
                                        IconName::Close,
                                    )
                                    .icon_size(IconSize::XSmall)
                                    .style(ButtonStyle::Filled)
                                    .tooltip(Tooltip::text("Remove Image"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if index < this.attachments.len() {
                                            this.attachments.remove(index);
                                        }
                                        cx.notify();
                                    })),
                                ),
                        )
                },
            )))
            .when(!sees_images, |this| {
                this.child(
                    Label::new(
                        "This model can't see images: it gets the saved file paths only. Pick a model with image input to have them looked at.",
                    )
                    .size(LabelSize::XSmall)
                    .color(Color::Warning),
                )
            })
    }

    fn render_composer(&self, has_messages: bool, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        h_flex()
            .py_2()
            .bg(colors.editor_background)
            .justify_center()
            .capture_key_down(cx.listener(Self::suggestion_key))
            .when(has_messages, |this| {
                this.border_t_1().border_color(colors.border)
            })
            // Dropped image files attach to the next message.
            .drag_over::<gpui::ExternalPaths>(|style, _, _, cx| {
                style.bg(cx.theme().colors().drop_target_background)
            })
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                this.attach_paths(paths.paths().to_vec(), cx);
            }))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .gap_2()
                    .when(!self.queued.is_empty() || self.steering.is_some(), |this| {
                        this.child(self.render_queue(cx))
                    })
                    .when(!self.attachments.is_empty(), |this| {
                        this.child(self.render_attachments(cx))
                    })
                    .children((!self.slash_matches.is_empty()).then(|| {
                        v_flex().children(self.slash_matches.iter().enumerate().map(
                            |(index, suggestion)| {
                                let selected = index == self.suggestion_selected;
                                h_flex()
                                    .id(SharedString::from(format!(
                                        "suggestion-{}",
                                        suggestion.label
                                    )))
                                    .gap_2()
                                    .px_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(selected, |this| {
                                        this.bg(colors.ghost_element_selected)
                                    })
                                    .hover(|style| style.bg(colors.ghost_element_hover))
                                    .when_some(suggestion.icon, |this, icon| {
                                        this.child(
                                            Icon::new(icon)
                                                .size(IconSize::Small)
                                                .color(Color::Muted),
                                        )
                                    })
                                    .child(
                                        Label::new(suggestion.label.clone())
                                            .size(LabelSize::Small),
                                    )
                                    .child(
                                        Label::new(suggestion.description.clone())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .when(selected, |this| {
                                        this.child(div().flex_1()).child(
                                            Label::new("Tab")
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.suggestion_selected = index;
                                        this.accept_suggestion(window, cx);
                                    }))
                            },
                        ))
                    }))
                    .child(
                        v_flex()
                            .relative()
                            .w_full()
                            .min_h_0()
                            .pt_1()
                            .pr_2p5()
                            .child(self.composer.clone()),
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
                                    .when(backend(cx).runtime.yolo(&self.session_id), |this| {
                                        this.child(
                                            div()
                                                .id("yolo")
                                                .mr_1()
                                                .tooltip(Tooltip::text(
                                                    "YOLO: commands run without asking (hardline ones never do). /yolo turns it off.",
                                                ))
                                                .child(
                                                    Label::new("YOLO")
                                                        .size(LabelSize::XSmall)
                                                        .color(Color::Warning),
                                                ),
                                        )
                                    })
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.entries.is_empty();
        let list_state = self.list_state.clone();
        // New messages since the matches were counted.
        if self
            .find
            .as_ref()
            .is_some_and(|find| find.seen != self.conversation.messages().len())
        {
            self.update_find(cx);
        }
        let find_bar = self.render_find_bar(cx);
        let colors = cx.theme().colors();
        v_flex()
            .key_context("Chat")
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .size_full()
            .bg(colors.panel_background)
            .children(find_bar)
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
                // Zed's thread: the list with its scrollbar along the right edge.
                v_flex()
                    .flex_1()
                    .size_full()
                    .child(
                        list(
                            list_state.clone(),
                            cx.processor(|this, index: usize, window, cx| {
                                this.render_entry(index, window, cx)
                            }),
                        )
                        .flex_1()
                        .size_full(),
                    )
                    .vertical_scrollbar_for(&list_state, window, cx)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// A label with a bright band sweeping across it, left to right: the "working" shimmer.
fn shimmer(id: SharedString, text: &'static str, cx: &App) -> AnyElement {
    let base = Color::Muted.color(cx);
    let bright = cx.theme().colors().text;
    let size = rems_from_px(13_f32);
    let chars = text.chars().count() as f32;
    // Half-width of the band, in characters; it starts and ends fully off the text.
    const BAND: f32 = 3.0;
    div()
        .text_size(size)
        .text_color(base)
        .child(text)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1800)).repeat(),
            move |_, delta| {
                let center = -BAND + delta * (chars + 2. * BAND);
                let runs = text.char_indices().enumerate().map(|(n, (i, c))| {
                    let distance = (n as f32 + 0.5 - center).abs();
                    let strength = (1. - distance / BAND).clamp(0., 1.);
                    let color = base.blend(bright.opacity(strength * strength));
                    (
                        i..i + c.len_utf8(),
                        gpui::HighlightStyle {
                            color: Some(color),
                            ..Default::default()
                        },
                    )
                });
                div()
                    .text_size(size)
                    .text_color(base)
                    .child(gpui::StyledText::new(text).with_highlights(runs))
            },
        )
        .into_any_element()
}

/// The argument of a built-in slash command (`/name arg`), or `None` when `text` isn't one.
/// How the composer styles what's typed: `code` and "quotes" as markdown would, and `@file:` /
/// `@folder:` references and a finished `/command` as chips that open what they name.
fn composer_decorations(known: Rc<RefCell<HashMap<String, String>>>) -> crate::text_area::Decorate {
    use crate::text_area::{Span, SpanKind};
    Rc::new(move |text: &str| {
        let mut spans = crate::text_area::markdown_spans(text);
        for (range, reference) in hermes_tools::references::spans(text) {
            let target = match reference.kind.as_str() {
                "file" | "folder" => format!("{}:{}", reference.kind, reference.target),
                kind => format!("ref:{kind}"),
            };
            spans.push(Span {
                range,
                kind: SpanKind::Mention(target),
            });
        }
        if let Some(rest) = text.strip_prefix('/') {
            if let Some(len) = rest.find(char::is_whitespace) {
                if let Some(target) = known.borrow().get(&rest[..len]) {
                    spans.push(Span {
                        range: 0..len + 1,
                        kind: SpanKind::Mention(target.clone()),
                    });
                }
            }
        }
        spans
    })
}

/// Where `query` (already lowercase) occurs in `text`, ignoring ASCII case, as byte ranges.
fn find_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    text.to_ascii_lowercase()
        .match_indices(query)
        .map(|(start, found)| start..start + found.len())
        .collect()
}

fn command_arg<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let rest = text.trim().strip_prefix('/')?.strip_prefix(name)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// Hermes's built-in commands offered in the slash suggestions, before skills.
pub(crate) const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    (
        "refine",
        "Review this conversation now and save lessons to memory/skills",
    ),
    (
        "learn",
        "Learn a reusable skill from anything you describe (dirs, URLs, this chat, notes)",
    ),
    (
        "compress",
        "Compress the conversation context now (optional focus topic)",
    ),
    (
        "steer",
        "Send guidance to the running turn, delivered after its current tool call",
    ),
    (
        "undo",
        "Remove the last exchange (/undo N for more) and put the message back in the box",
    ),
    (
        "usage",
        "Token use, cost and context breakdown for this session",
    ),
    (
        "insights",
        "Usage across sessions: /insights --days N (default 30)",
    ),
    (
        "yolo",
        "Run flagged commands without asking in this session (hardline ones never run)",
    ),
    (
        "queue",
        "Queue a prompt for after the running turn, or list/edit/rm/move/clear queued prompts",
    ),
    (
        "rollback",
        "List file checkpoints; /rollback N restores one, /rollback diff N previews",
    ),
    (
        "diff",
        "Show git changes: working, staged, all, or session (since the first checkpoint)",
    ),
];

/// A user message as the user wrote it: the typed text of one whose `@` references were
/// expanded, the command of one that loads a skill, else its text.
pub fn shown_user_text(message: &Message) -> String {
    if let Some(typed) = hermes_tools::references::typed_text(message) {
        return typed.to_string();
    }
    let text = message.as_concat_text();
    typed_command(&text).unwrap_or(text)
}

/// What the user typed to produce a message that loads a skill, a bundle or the learn prompt
/// (`/name instruction`); `None` for an ordinary message. Those messages are long, so the chat
/// shows (and titles) them by the command.
pub fn typed_command(text: &str) -> Option<String> {
    let (command, argument) = hermes_tools::describe_bundle_invocation(text)
        .or_else(|| hermes_tools::skills::describe_invocation(text))
        .or_else(|| {
            hermes_prompts::review::learn_request(text).map(|request| ("learn".into(), request))
        })?;
    Some(format!("/{command} {argument}").trim_end().to_string())
}

/// One row under the composer: a `/command` or an `@` mention, and the composer text that
/// accepting it leaves.
#[derive(Clone, PartialEq)]
struct Suggestion {
    label: String,
    description: String,
    text: String,
    icon: Option<IconName>,
}

/// The project's entries for an `@` mention, best first: the name starting with what's typed,
/// then containing it, then the path containing it, then its letters in order; folders before
/// files, then shallower and shorter paths. `only` keeps folders (true) or files (false).
fn rank_entries(entries: &[(String, bool)], query: &str, only: Option<bool>) -> Vec<(String, bool)> {
    let mut ranked: Vec<(u8, bool, usize, usize, &String)> = entries
        .iter()
        .filter(|(_, is_dir)| only.is_none_or(|folders| *is_dir == folders))
        .filter_map(|(path, is_dir)| {
            let lower = path.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let rank = if query.is_empty() || name.starts_with(query) {
                0
            } else if name.contains(query) {
                1
            } else if lower.contains(query) {
                2
            } else if fuzzy_subsequence(&lower, query) {
                3
            } else {
                return None;
            };
            let depth = path.matches('/').count();
            // Shorter paths are closer matches; with nothing typed, the list is alphabetical.
            let len = if query.is_empty() { 0 } else { path.len() };
            Some((rank, !*is_dir, depth, len, path))
        })
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .map(|(_, is_file, _, _, path)| (path.clone(), !is_file))
        .collect()
}

/// Whether `query`'s characters appear in `text` in order.
fn fuzzy_subsequence(text: &str, query: &str) -> bool {
    let mut chars = text.chars();
    query.chars().all(|q| chars.any(|c| c == q))
}

/// `queue.usage`.
const QUEUE_USAGE: &str = "Usage: /queue <prompt> | /queue list | /queue edit N <prompt> | /queue rm N | /queue move FROM TO | /queue clear";

/// Hermes's built-in `@` reference kinds, offered as an `@` mention is typed.
const MENTION_KINDS: &[(&str, &str)] = &[
    ("@file:", "Attach a file (add :start-end for lines)"),
    ("@folder:", "Attach a folder's file listing"),
    ("@diff", "Attach the unstaged git changes"),
    ("@staged", "Attach the staged git changes"),
    (
        "@git:",
        "Attach the last N commits with patches, e.g. @git:3",
    ),
    ("@url:", "Attach a web page's text"),
];

/// A `/rollback` or `/diff` report: shown like a reply, but no turn ends with it.
fn is_command_output(message: &Message) -> bool {
    message
        .metadata
        .operation_note(hermes_runtime::COMMAND_NOTE, "command")
        .is_some()
}

/// The image format of attachable bytes, by their signature.
fn image_format(bytes: &[u8]) -> Option<gpui::ImageFormat> {
    let starts = |prefix: &[u8]| bytes.starts_with(prefix);
    Some(if starts(b"\x89PNG") {
        gpui::ImageFormat::Png
    } else if starts(&[0xff, 0xd8, 0xff]) {
        gpui::ImageFormat::Jpeg
    } else if starts(b"GIF8") {
        gpui::ImageFormat::Gif
    } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        gpui::ImageFormat::Webp
    } else if starts(b"BM") {
        gpui::ImageFormat::Bmp
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        gpui::ImageFormat::Tiff
    } else {
        return None;
    })
}

/// A message waiting over the composer: the undelivered steer, or a queued prompt by index.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingEntry {
    Steer,
    Queued(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_list_folders_first_then_better_matches() {
        let entries: Vec<(String, bool)> = [
            ("src", true),
            ("src/main.rs", false),
            ("docs", true),
            ("docs/main-notes.md", false),
            ("README.md", false),
            ("src/maintenance", true),
        ]
        .into_iter()
        .map(|(p, d)| (p.to_string(), d))
        .collect();
        // Nothing typed: folders, shallow first, then files.
        let all = rank_entries(&entries, "", None);
        assert_eq!(all[0], ("docs".to_string(), true));
        assert_eq!(all[1], ("src".to_string(), true));
        assert!(all.iter().position(|e| !e.1).unwrap() > 2);
        // "main": names starting with it, the folder first.
        let main = rank_entries(&entries, "main", None);
        assert_eq!(main[0], ("src/maintenance".to_string(), true));
        assert_eq!(main[1], ("src/main.rs".to_string(), false));
        // Letters in order still find it.
        assert!(rank_entries(&entries, "rdme", Some(false))
            .iter()
            .any(|e| e.0 == "README.md"));
    }

    #[test]
    fn the_composer_styles_code_quotes_and_references() {
        use crate::text_area::SpanKind;
        let known: Rc<RefCell<HashMap<String, String>>> = Rc::default();
        known
            .borrow_mut()
            .insert("usage".into(), "command:usage".into());
        let decorate = composer_decorations(known);
        let text = "/usage then `cargo test` on \"x y\" and @file:src/main.rs";
        let kinds: Vec<(String, SpanKind)> = decorate(text)
            .into_iter()
            .map(|s| (text[s.range].to_string(), s.kind))
            .collect();
        assert!(kinds.contains(&("`cargo test`".into(), SpanKind::Code)));
        assert!(kinds.contains(&("\"x y\"".into(), SpanKind::Quote)));
        assert!(kinds.contains(&(
            "@file:src/main.rs".into(),
            SpanKind::Mention("file:src/main.rs".into())
        )));
        assert!(kinds.contains(&("/usage".into(), SpanKind::Mention("command:usage".into()))));
    }
}

/// What an in-place editor is changing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditTarget {
    /// A sent user message, by conversation index.
    Sent(usize),
    /// A message waiting in the queue panel.
    Pending(PendingEntry),
}

struct InlineEdit {
    target: EditTarget,
    editor: Entity<TextArea>,
    /// The text before editing: leaving it unchanged ends the edit.
    original: String,
    _subscriptions: Vec<Subscription>,
}
