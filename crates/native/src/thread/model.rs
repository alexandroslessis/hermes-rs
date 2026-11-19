//! The open thread's state: its timeline rows, the history pages loaded so far, the turn that is
//! streaming into it, and the virtual list's bookkeeping. One [`ThreadModel`] per open thread,
//! made by the app model ([`crate::model::AppModel::open`]); the view reads it and asks it to do
//! things, it alone talks to the backend (history pages, the dev-only send) and takes the core's
//! events for its session (the app model forwards them).
//!
//! How streaming stays cheap, whatever the length of the thread:
//! - **Deltas touch the last row only.** A text delta is pushed onto the streaming row's
//!   `TextViewState` (Kit parses only its last block again with the new text) and only that row is
//!   marked for measuring again; no other row is looked at.
//! - **No copy when a message finishes.** The rows are a plain `Vec` the model owns (no shared
//!   `Rc<Vec>` a renderer holds, which `make_mut` had to copy whole in the CP0.1 spike), and a
//!   finished streamed row keeps its `TextViewState`: the text is not moved, copied or parsed
//!   again when the turn ends.
//! - **One copy of the streamed text.** The row holds no `String` of its own beside the state's
//!   (the spike kept both); "copy message" reads the state's source ([`Body::text`]).
//! - **History rows are text only** (a `SharedString`); Kit parses them when they scroll into view.
//!
//! History loads newest first: the last [`PAGE`] items when the thread opens, an older page each
//! time the list's top comes near ([`ThreadModel::load_older`]); a page is put in front of the
//! rows and the list keeps its place (`MessageScrollerState::prepend`).
//!
//! A turn the model did not see whole (the thread was opened while it ran, or it started before
//! the history was read) is read back from the store when it ends: its streamed rows began in the
//! middle, or the history may hold part of it. A turn seen whole stays as it streamed.
//!
//! t3code: `apps/web/src/components/ChatView.tsx` (the open thread, its live turn),
//! `chat/MessagesTimeline.tsx` and `MessagesTimeline.logic.ts` (rows from items and the live
//! turn, the reasoning row, the older-history loading).

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, ImageFormat, SharedString,
    component::{message_scroller::MessageScrollerState, text::TextViewState},
};
use hermes_core::{CoreEvent, Outcome};

use super::data::{Attachment, HistoryItem, HistoryPage, PAGE};
use crate::model::Backend;

/// A row's markdown: text from history, or the `TextViewState` a turn streamed into.
#[derive(Clone)]
pub enum Body {
    Text(SharedString),
    Live(Entity<TextViewState>),
}

impl Body {
    /// The markdown source.
    pub fn text(&self, cx: &App) -> SharedString {
        match self {
            Body::Text(text) => text.clone(),
            // Kit's own copy of the source: the only one (doc-hidden in Kit, public).
            Body::Live(state) => {
                gpui_kit::base::text::Text::from(gpui_kit::base::text::TextView::new(state))
                    .get_text(cx)
            }
        }
    }
}

/// An image attached to a user message, ready to draw.
#[derive(Clone)]
pub struct Thumb {
    pub name: SharedString,
    pub image: Arc<gpui_kit::Image>,
}

impl Thumb {
    fn from_attachment(attachment: &Attachment) -> Option<Self> {
        let format = match attachment.mime.as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
            "image/gif" => ImageFormat::Gif,
            "image/webp" => ImageFormat::Webp,
            "image/bmp" => ImageFormat::Bmp,
            "image/svg+xml" => ImageFormat::Svg,
            _ => return None,
        };
        Some(Self {
            name: attachment.name.clone().into(),
            image: Arc::new(gpui_kit::Image::from_bytes(
                format,
                attachment.bytes.as_ref().clone(),
            )),
        })
    }
}

/// What a row shows.
#[derive(Clone)]
pub enum RowKind {
    User {
        text: SharedString,
        images: Vec<Thumb>,
    },
    Assistant {
        body: Body,
    },
    /// Collapsed unless `open`; `live` while it streams; `duration_ms` once measured.
    Reasoning {
        body: Body,
        duration_ms: Option<u64>,
        open: bool,
        live: bool,
    },
    /// A tool call's placeholder line (the cards are CP4.2).
    Tool {
        name: SharedString,
        summary: SharedString,
        done: bool,
    },
    /// A line about the turn: a failure (`error`) or a status (compacting, stopped).
    Notice {
        error: bool,
        text: SharedString,
    },
}

/// One row of the timeline. `key` is stable for the row's life (element ids, toggles).
#[derive(Clone)]
pub struct Row {
    pub key: u64,
    pub kind: RowKind,
    /// When it was written (user messages and replies), for the hover time.
    pub at_ms: Option<i64>,
}

/// Where the thread is in loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadPhase {
    /// The first page is being read (local: no spinner is shown for it).
    Loading,
    Ready,
    /// The history could not be read: why.
    Failed(String),
}

/// The streaming turn's rows.
#[derive(Default)]
struct LiveTurn {
    /// The turn was not seen whole: it was running when the thread opened (its first events went
    /// by before this model existed), or it started before the history was read (the history may
    /// hold part of it). Its streamed rows are then replaced by the store's when it ends.
    partial: bool,
    /// The reasoning row being streamed into and when it started.
    reasoning: Option<(u64, i64)>,
    /// The reply row being streamed into.
    text: Option<u64>,
    /// Tool call ids to their rows.
    tools: HashMap<String, u64>,
}

impl LiveTurn {
    /// A turn whose events start in the middle (no `TurnStarted` was seen).
    fn partial() -> Self {
        Self {
            partial: true,
            ..Self::default()
        }
    }
}

/// Which history read a page answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// The newest page, when the thread opens.
    First,
    /// The page before the loaded rows.
    Older,
    /// The newest page again, replacing the rows before index `keep_from` (a partial turn ended:
    /// the store has it whole).
    Reload { keep_from: usize },
}

/// The open thread.
pub struct ThreadModel {
    session: String,
    backend: Arc<dyn Backend>,
    rows: Vec<Row>,
    scroller: Entity<MessageScrollerState>,
    phase: ThreadPhase,
    /// Where the loaded history starts in the whole history (0: all of it is loaded).
    start: usize,
    loading_older: bool,
    page_size: usize,
    live: Option<LiveTurn>,
    next_key: u64,
    clock: fn() -> i64,
    /// History's reasoning rows start open (the gallery's "reasoning open" state).
    open_reasoning: bool,
    /// Bumped by a reload: a page read before it is dropped when it arrives.
    generation: u64,
    /// A replayed turn (the gallery) is in no store: its end reconciles nothing.
    replaying: bool,
}

/// What the model tells its view besides "something changed".
#[derive(Clone, Debug, PartialEq)]
pub enum ThreadEvent {
    /// Sending a prompt failed (why); the view shows it.
    SendFailed(String),
}

impl EventEmitter<ThreadEvent> for ThreadModel {}

impl ThreadModel {
    /// The thread `session`, its newest page being read from `backend`.
    pub fn new(session: &str, backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        Self::with_page_size(session, backend, PAGE, cx)
    }

    /// Like [`Self::new`] with pages of `page_size` items (tests).
    pub fn with_page_size(
        session: &str,
        backend: Arc<dyn Backend>,
        page_size: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let mut model = Self {
            session: session.to_string(),
            backend,
            rows: Vec::new(),
            scroller,
            phase: ThreadPhase::Loading,
            start: 0,
            loading_older: false,
            page_size: page_size.max(1),
            live: None,
            next_key: 0,
            clock: crate::model::system_now_ms,
            open_reasoning: false,
            generation: 0,
            replaying: false,
        };
        model.load_page(None, Load::First, cx);
        model
    }

    /// The clock the durations use.
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// History's reasoning rows start open (the gallery).
    pub fn with_reasoning_open(mut self) -> Self {
        self.open_reasoning = true;
        self
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn phase(&self) -> &ThreadPhase {
        &self.phase
    }

    /// The virtual list's state.
    pub fn scroller(&self) -> &Entity<MessageScrollerState> {
        &self.scroller
    }

    /// Whether older history can still be loaded.
    pub fn has_older(&self) -> bool {
        self.phase == ThreadPhase::Ready && self.start > 0
    }

    /// Whether an older page is being read.
    pub fn loading_older(&self) -> bool {
        self.loading_older
    }

    /// Whether a turn is streaming into the thread.
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    /// The row with `key`, looked for from the end (the rows a turn touches are the last ones).
    pub fn index_of(&self, key: u64) -> Option<usize> {
        self.rows.iter().rposition(|row| row.key == key)
    }

    // --- history ------------------------------------------------------------------------------

    fn load_page(&mut self, before: Option<usize>, load: Load, cx: &mut Context<Self>) {
        let (backend, session, limit) =
            (self.backend.clone(), self.session.clone(), self.page_size);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let page = cx
                .background_spawn(async move { backend.history(&session, before, limit) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match page {
                    Ok(page) => this.install(page, load, cx),
                    Err(why) => {
                        this.loading_older = false;
                        if load == Load::First {
                            this.phase = ThreadPhase::Failed(why);
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Puts a page in front of the rows (the first page goes before any row a turn streamed
    /// meanwhile), or, for a reload, in place of the rows before `keep_from`.
    fn install(&mut self, page: HistoryPage, load: Load, cx: &mut Context<Self>) {
        let rows: Vec<Row> = page.items.iter().map(|item| self.row_of(item)).collect();
        let count = rows.len();
        self.start = page.start;
        self.loading_older = false;
        match load {
            Load::First | Load::Older => {
                self.rows.splice(0..0, rows);
                self.scroller.update(cx, |list, cx| {
                    list.prepend(count, cx);
                    if load == Load::First {
                        list.scroll_to_end(cx);
                    }
                });
            }
            Load::Reload { keep_from } => {
                let kept = self.rows.split_off(keep_from.min(self.rows.len()));
                self.rows = rows;
                self.rows.extend(kept);
                let total = self.rows.len();
                self.scroller.update(cx, |list, cx| {
                    list.reset(total, cx);
                    list.scroll_to_end(cx);
                });
            }
        }
        self.phase = ThreadPhase::Ready;
        cx.notify();
    }

    /// Reads the newest page again and puts it in place of the rows before `keep_from` (those
    /// after it stay: rows added since). A page of older history still being read is dropped, and
    /// none is asked for until the reload lands (it would move `keep_from`).
    fn reload(&mut self, keep_from: usize, cx: &mut Context<Self>) {
        self.generation += 1;
        self.loading_older = true;
        self.load_page(None, Load::Reload { keep_from }, cx);
    }

    fn row_of(&mut self, item: &HistoryItem) -> Row {
        let key = self.key();
        let (kind, at_ms) = match item {
            HistoryItem::User {
                text,
                images,
                at_ms,
            } => (
                RowKind::User {
                    text: text.clone().into(),
                    images: images.iter().filter_map(Thumb::from_attachment).collect(),
                },
                Some(*at_ms),
            ),
            HistoryItem::Assistant { text, at_ms } => (
                RowKind::Assistant {
                    body: Body::Text(text.clone().into()),
                },
                Some(*at_ms),
            ),
            HistoryItem::Reasoning { text, duration_ms } => (
                RowKind::Reasoning {
                    body: Body::Text(text.clone().into()),
                    duration_ms: *duration_ms,
                    open: self.open_reasoning,
                    live: false,
                },
                None,
            ),
            HistoryItem::Tool { name, summary } => (
                RowKind::Tool {
                    name: name.clone().into(),
                    summary: summary.clone().into(),
                    done: true,
                },
                None,
            ),
            HistoryItem::Failed { text } => (
                RowKind::Notice {
                    error: true,
                    text: text.clone().into(),
                },
                None,
            ),
        };
        Row { key, kind, at_ms }
    }

    /// Reads the page before the loaded history, if there is one and none is being read.
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.loading_older || !self.has_older() {
            return;
        }
        self.loading_older = true;
        self.load_page(Some(self.start), Load::Older, cx);
    }

    // --- rows ---------------------------------------------------------------------------------

    fn push(&mut self, kind: RowKind, at_ms: Option<i64>, cx: &mut Context<Self>) -> u64 {
        let key = self.key();
        self.rows.push(Row { key, kind, at_ms });
        self.scroller.update(cx, |list, cx| {
            list.append(1, cx);
        });
        cx.notify();
        key
    }

    /// Marks row `ix` for measuring again (its height may have changed).
    fn remeasure(&self, ix: usize, cx: &mut Context<Self>) {
        self.scroller.update(cx, |list, cx| {
            list.remeasure_items(ix..ix + 1, cx);
        });
    }

    /// Opens or closes a reasoning row.
    pub fn toggle_reasoning(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(key) else {
            return;
        };
        if let RowKind::Reasoning { open, .. } = &mut self.rows[ix].kind {
            *open = !*open;
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// The text "copy message" puts on the clipboard: the message's markdown.
    pub fn copy_text(&self, key: u64, cx: &App) -> Option<String> {
        let row = &self.rows[self.index_of(key)?];
        match &row.kind {
            RowKind::User { text, .. } => Some(text.to_string()),
            RowKind::Assistant { body } | RowKind::Reasoning { body, .. } => {
                Some(body.text(cx).to_string())
            }
            RowKind::Notice { text, .. } => Some(text.to_string()),
            RowKind::Tool { .. } => None,
        }
    }

    /// Back to the newest row, following it again.
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.scroller.update(cx, |list, cx| list.scroll_to_end(cx));
    }

    // --- sending (dev only until Phase 5) -----------------------------------------------------

    /// Sends `text` as a prompt: its row shows at once, the backend sends it in the background
    /// and the turn's events stream in; a failure is a notice row and [`ThreadEvent::SendFailed`].
    pub fn submit(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let now = (self.clock)();
        self.push(
            RowKind::User {
                text: text.clone().into(),
                images: Vec::new(),
            },
            Some(now),
            cx,
        );
        self.scroll_to_end(cx);
        let (backend, session) = (self.backend.clone(), self.session.clone());
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_spawn(async move { backend.submit(&session, &text) })
                .await;
            if let Err(why) = sent {
                let _ = this.update(cx, |this, cx| {
                    this.push(
                        RowKind::Notice {
                            error: true,
                            text: format!("Could not send: {why}").into(),
                        },
                        None,
                        cx,
                    );
                    cx.emit(ThreadEvent::SendFailed(why));
                });
            }
        })
        .detach();
    }

    // --- the live turn ------------------------------------------------------------------------

    /// Takes in one of the core's events for this thread (others are ignored).
    pub fn on_event(&mut self, event: &CoreEvent, cx: &mut Context<Self>) {
        let now = (self.clock)();
        self.on_event_at(event, now, cx);
    }

    /// [`Self::on_event`] at `now_ms` (a replay gives its own times).
    pub fn on_event_at(&mut self, event: &CoreEvent, now_ms: i64, cx: &mut Context<Self>) {
        if event.session_id() != self.session {
            return;
        }
        match event {
            CoreEvent::TurnStarted { .. } => {
                // Before the history is read, the store may already hold part of this turn
                // (its prompt): the end reconciles.
                self.live = Some(LiveTurn {
                    partial: self.phase != ThreadPhase::Ready,
                    ..LiveTurn::default()
                });
            }
            CoreEvent::ReasoningDelta { text, .. } => self.reasoning_delta(text, now_ms, cx),
            CoreEvent::TextDelta { text, .. } => {
                self.end_reasoning(now_ms, cx);
                self.text_delta(text, now_ms, cx);
            }
            CoreEvent::ToolStarted {
                tool_id,
                name,
                args,
                ..
            } => {
                self.end_reasoning(now_ms, cx);
                let summary = super::data::tool_summary(name, args);
                let key = self.push(
                    RowKind::Tool {
                        name: name.clone().into(),
                        summary: summary.into(),
                        done: false,
                    },
                    None,
                    cx,
                );
                let live = self.live.get_or_insert_with(LiveTurn::partial);
                live.text = None;
                live.tools.insert(tool_id.clone(), key);
            }
            CoreEvent::ToolCompleted { tool_id, .. } => {
                let key = self
                    .live
                    .as_ref()
                    .and_then(|live| live.tools.get(tool_id).copied());
                if let Some(ix) = key.and_then(|key| self.index_of(key))
                    && let RowKind::Tool { done, .. } = &mut self.rows[ix].kind
                {
                    *done = true;
                    cx.notify();
                }
            }
            CoreEvent::Status { text, .. } => {
                self.end_reasoning(now_ms, cx);
                self.push(
                    RowKind::Notice {
                        error: false,
                        text: text.clone().into(),
                    },
                    None,
                    cx,
                );
                if let Some(live) = &mut self.live {
                    live.text = None;
                }
            }
            CoreEvent::TurnEnded { end, .. } => {
                self.end_reasoning(now_ms, cx);
                let streamed = self.live.as_ref().is_some_and(|live| live.text.is_some());
                // A turn not seen whole (or not seen at all: its end alone) is read back from
                // the store; the notice pushed below stays after it.
                let partial = self.live.as_ref().is_none_or(|live| live.partial);
                let keep_from = self.rows.len();
                match end.outcome {
                    Outcome::Error => {
                        // The account of the error: the runtime's text, else the turn's text
                        // when nothing streamed (it carries the error's account then).
                        let text = end
                            .error
                            .clone()
                            .filter(|e| !e.is_empty())
                            .or_else(|| (!streamed).then(|| end.text.clone()))
                            .unwrap_or_else(|| "The turn failed.".to_string());
                        self.push(
                            RowKind::Notice {
                                error: true,
                                text: text.into(),
                            },
                            None,
                            cx,
                        );
                    }
                    Outcome::Interrupted => {
                        self.push(
                            RowKind::Notice {
                                error: false,
                                text: "Stopped.".into(),
                            },
                            None,
                            cx,
                        );
                    }
                    Outcome::Complete => {}
                }
                // A tool call the turn never finished stops spinning with it.
                let tools: Vec<u64> = self
                    .live
                    .as_ref()
                    .map(|live| live.tools.values().copied().collect())
                    .unwrap_or_default();
                for key in tools {
                    if let Some(ix) = self.index_of(key)
                        && let RowKind::Tool { done, .. } = &mut self.rows[ix].kind
                    {
                        *done = true;
                    }
                }
                // The streamed rows stay as they are: no copy, no parse.
                self.live = None;
                if partial && !self.replaying {
                    self.reload(keep_from, cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn reasoning_delta(&mut self, text: &str, now_ms: i64, cx: &mut Context<Self>) {
        let current = self
            .live
            .as_ref()
            .and_then(|live| live.reasoning)
            .and_then(|(key, _)| self.index_of(key))
            .filter(|ix| ix + 1 == self.rows.len());
        if let Some(ix) = current {
            if let RowKind::Reasoning {
                body: Body::Live(state),
                ..
            } = &self.rows[ix].kind
            {
                state.update(cx, |state, cx| state.push_str(text, cx));
                self.remeasure(ix, cx);
            }
            return;
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
        let key = self.push(
            RowKind::Reasoning {
                body: Body::Live(state),
                duration_ms: None,
                open: false,
                live: true,
            },
            None,
            cx,
        );
        let live = self.live.get_or_insert_with(LiveTurn::partial);
        live.reasoning = Some((key, now_ms));
        live.text = None;
    }

    /// The reasoning row stops streaming: its duration is measured.
    fn end_reasoning(&mut self, now_ms: i64, cx: &mut Context<Self>) {
        let Some((key, started)) = self.live.as_mut().and_then(|live| live.reasoning.take()) else {
            return;
        };
        if let Some(ix) = self.index_of(key)
            && let RowKind::Reasoning {
                duration_ms, live, ..
            } = &mut self.rows[ix].kind
        {
            *duration_ms = Some(now_ms.saturating_sub(started).max(0) as u64);
            *live = false;
            cx.notify();
        }
    }

    fn text_delta(&mut self, text: &str, now_ms: i64, cx: &mut Context<Self>) {
        let current = self
            .live
            .as_ref()
            .and_then(|live| live.text)
            .and_then(|key| self.index_of(key))
            .filter(|ix| ix + 1 == self.rows.len());
        if let Some(ix) = current {
            if let RowKind::Assistant {
                body: Body::Live(state),
            } = &self.rows[ix].kind
            {
                state.update(cx, |state, cx| state.push_str(text, cx));
                self.remeasure(ix, cx);
            }
            return;
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
        let key = self.push(
            RowKind::Assistant {
                body: Body::Live(state),
            },
            Some(now_ms),
            cx,
        );
        self.live.get_or_insert_with(LiveTurn::partial).text = Some(key);
    }

    /// Replays a recorded turn as if it arrived now, one event every
    /// [`super::recording::REPLAY_STEP`] (the gallery and the tests): the prompt's row, then
    /// every event.
    pub fn replay(&mut self, recording: &super::recording::Recording, cx: &mut Context<Self>) {
        let step = super::recording::REPLAY_STEP.as_millis() as i64;
        let mut now = (self.clock)();
        self.push(
            RowKind::User {
                text: recording.prompt.clone().into(),
                images: Vec::new(),
            },
            Some(now),
            cx,
        );
        self.replaying = true;
        for event in recording.events_for(&self.session) {
            now += step;
            self.on_event_at(&event, now, cx);
        }
        self.replaying = false;
    }
}
