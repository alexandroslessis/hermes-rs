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
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, ImageFormat, SharedString,
    component::{message_scroller::MessageScrollerState, text::TextViewState},
};
use hermes_core::{CoreEvent, Outcome};
use serde_json::Value;

use super::data::{Attachment, HistoryItem, HistoryPage, PAGE};
use super::header_data::{Exported, HeaderInfo};
use super::tools::{Detail, Status, SubTool, ToolCard, ToolKind, changed_files};
use super::work::{Footer, ToolEntry, Usage, WorkGroup};
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
    /// A run of tool calls (one call is shown as its card, more fold under a header).
    Work(WorkGroup),
    /// The end of a turn: changed files, usage, Retry and Branch.
    Footer(Footer),
    /// A line about the turn: a failure (`error`) or a status (compacting, stopped).
    Notice {
        error: bool,
        text: SharedString,
    },
    /// What a `/command` answered (a report on the thread): the command and the text, shown on
    /// the screen only.
    Command {
        command: SharedString,
        text: SharedString,
    },
}

/// One row of the timeline. `key` is stable for the row's life (element ids, toggles).
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
    /// The work group the next tool call joins: the last row, while nothing came between.
    work: Option<u64>,
    /// Every work group row of the turn, in order (its changed files, its running label).
    work_rows: Vec<u64>,
    /// The `delegate_task` call whose subagents' events are arriving.
    delegate: Option<String>,
    started_ms: i64,
    usage: Option<Usage>,
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
    /// What the header shows about the thread (folder, model, git), read in the background.
    header: HeaderRead,
    /// A save is being prepared (the menu's export entries wait).
    exporting: bool,
    /// Prompts the composer sent, by the core's admission id: the user's row is shown when the
    /// turn with that id starts (a queued prompt appears when its turn begins).
    expected: Vec<ExpectedPrompt>,
}

/// A prompt the composer sent: what its row shows once its turn starts.
struct ExpectedPrompt {
    id: String,
    text: String,
    images: Vec<Attachment>,
}

/// How long after one header read the window coming back to the front does not read again.
const HEADER_STALE: Duration = Duration::from_secs(2);

/// The header's data and the bookkeeping of reading it: one read at a time, a request made
/// meanwhile runs once more after it.
struct HeaderRead {
    stale_after: Duration,
    info: Option<HeaderInfo>,
    /// Why the last read failed (the header then shows no chips).
    error: Option<String>,
    reading: bool,
    again: bool,
    last: Option<Instant>,
}

impl Default for HeaderRead {
    fn default() -> Self {
        Self {
            stale_after: HEADER_STALE,
            info: None,
            error: None,
            reading: false,
            again: false,
            last: None,
        }
    }
}

/// What the model tells its view besides "something changed".
#[derive(Clone, Debug, PartialEq)]
pub enum ThreadEvent {
    /// Sending a prompt failed (why); the view shows it.
    SendFailed(String),
    /// "Branch from here" made this thread (the app model lists it and opens it).
    Branched(String),
    /// The thread's export is ready for the save dialog (the header asks for the file's place).
    ExportReady(Exported),
    /// The export could not be made (why).
    ExportFailed(String),
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
            header: HeaderRead::default(),
            exporting: false,
            expected: Vec::new(),
        };
        model.load_page(None, Load::First, cx);
        model.refresh_header(cx);
        model
    }

    // --- the header's data --------------------------------------------------------------------

    /// What the header shows: `None` until the first read lands (or when it failed: see
    /// [`Self::header_error`]).
    pub fn header(&self) -> Option<&HeaderInfo> {
        self.header.info.as_ref()
    }

    /// Why the header's data could not be read.
    pub fn header_error(&self) -> Option<&str> {
        self.header.error.as_deref()
    }

    /// Reads the thread's folder, model and git state again, in the background (a git probe is a
    /// process: never from a handler or `render`). Reads do not pile up: one runs at a time and a
    /// request made meanwhile runs once more after it. Called when the thread opens and when a
    /// turn ends; the header calls [`Self::refresh_header_if_stale`] when the window comes forward.
    pub fn refresh_header(&mut self, cx: &mut Context<Self>) {
        if self.header.reading {
            self.header.again = true;
            return;
        }
        self.header.reading = true;
        self.header.last = Some(Instant::now());
        let (backend, session) = (self.backend.clone(), self.session.clone());
        cx.spawn(async move |this, cx| {
            loop {
                let (reader, id) = (backend.clone(), session.clone());
                let read = cx
                    .background_spawn(async move { reader.header_info(&id) })
                    .await;
                let again = this.update(cx, |this, cx| {
                    match read {
                        Ok(info) => {
                            this.header.info = Some(info);
                            this.header.error = None;
                        }
                        Err(why) => this.header.error = Some(why),
                    }
                    let again = std::mem::take(&mut this.header.again);
                    if !again {
                        this.header.reading = false;
                    }
                    cx.notify();
                    again
                });
                match again {
                    Ok(true) => continue,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// [`Self::refresh_header`] unless a read is running or one finished just now (the window
    /// coming forward in quick succession reads once).
    pub fn refresh_header_if_stale(&mut self, cx: &mut Context<Self>) {
        let fresh = self
            .header
            .last
            .is_some_and(|last| last.elapsed() < self.header.stale_after);
        if !self.header.reading && !fresh {
            self.refresh_header(cx);
        }
    }

    /// How old the last header read must be for [`Self::refresh_header_if_stale`] to read again
    /// (tests lower it).
    pub fn set_header_stale(&mut self, after: Duration) {
        self.header.stale_after = after;
    }

    /// Whether the thread has a message of the user's or a reply (what "export" saves).
    pub fn has_messages(&self) -> bool {
        self.phase == ThreadPhase::Ready
            && self
                .rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::User { .. } | RowKind::Assistant { .. }))
    }

    /// The footer "branch from latest reply" uses: the newest turn's, when it is stored and
    /// nothing is running or being branched.
    fn branch_target(&self) -> Option<u64> {
        if self.live.is_some() {
            return None;
        }
        self.rows.iter().rev().find_map(|r| match &r.kind {
            RowKind::Footer(f) if f.last_row.is_some() && !f.busy => Some(r.key),
            _ => None,
        })
    }

    /// Whether the thread has a finished reply to branch from now.
    pub fn can_branch(&self) -> bool {
        self.branch_target().is_some()
    }

    /// Branch from the latest reply (the header's menu): the same operation as the turn footer's
    /// button on the newest turn.
    pub fn branch_latest(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.branch_target() {
            self.branch(key, cx);
        }
    }

    /// Whether a save is being prepared.
    pub fn exporting(&self) -> bool {
        self.exporting
    }

    /// Makes the thread's file in the background (`Backend::export`: the core's export over the
    /// stored messages) and tells the header ([`ThreadEvent::ExportReady`]), which asks where to
    /// save it.
    pub fn export(&mut self, format: hermes_core::export::Format, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        self.exporting = true;
        let (backend, session) = (self.backend.clone(), self.session.clone());
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(async move { backend.export(&session, format) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                cx.emit(match made {
                    Ok(file) => ThreadEvent::ExportReady(file),
                    Err(why) => ThreadEvent::ExportFailed(why),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
        let mut rows = self.rows_of(&page.items);
        self.start = page.start;
        self.loading_older = false;
        match load {
            Load::First | Load::Older => {
                // A run of tool calls cut by the page's edge is one group: the page's last group
                // joins the first group of the rows already here.
                let merged = matches!(rows.last().map(|r| &r.kind), Some(RowKind::Work(_)))
                    && matches!(self.rows.first().map(|r| &r.kind), Some(RowKind::Work(_)));
                if merged
                    && let Some(Row {
                        kind: RowKind::Work(earlier),
                        ..
                    }) = rows.pop()
                    && let Some(Row {
                        kind: RowKind::Work(later),
                        ..
                    }) = self.rows.first_mut()
                {
                    let mut entries = earlier.entries;
                    entries.append(&mut later.entries);
                    later.entries = entries;
                }
                let count = rows.len();
                self.rows.splice(0..0, rows);
                self.scroller.update(cx, |list, cx| {
                    list.prepend(count, cx);
                    if merged {
                        list.remeasure_items(count..count + 1, cx);
                    }
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

    /// The rows of a page of items: tool calls with nothing between them are one group row.
    fn rows_of(&mut self, items: &[HistoryItem]) -> Vec<Row> {
        let mut rows = Vec::with_capacity(items.len());
        let mut run: Vec<&HistoryItem> = Vec::new();
        for item in items {
            if matches!(item, HistoryItem::Tool { .. }) {
                run.push(item);
                continue;
            }
            if !run.is_empty() {
                let row = self.stored_work(&std::mem::take(&mut run));
                rows.push(row);
            }
            rows.push(self.row_of(item));
        }
        if !run.is_empty() {
            let row = self.stored_work(&run);
            rows.push(row);
        }
        rows
    }

    /// A group row of stored tool calls (all `HistoryItem::Tool`).
    fn stored_work(&mut self, calls: &[&HistoryItem]) -> Row {
        let mut group = WorkGroup::default();
        for call in calls {
            let HistoryItem::Tool {
                id,
                name,
                args,
                result,
                is_error,
                at_ms,
            } = call
            else {
                continue;
            };
            let card = ToolCard::from_stored(name, args, result.as_deref(), *is_error);
            let key = self.key();
            // What the store knows of the times: the message's, and a command's own duration.
            let mut entry = ToolEntry::new(key, id, card);
            entry.started_ms = Some(*at_ms);
            entry.ended_ms = Some(*at_ms + entry.card.duration_ms.unwrap_or(0) as i64);
            group.entries.push(entry);
        }
        Row {
            key: self.key(),
            kind: RowKind::Work(group),
            at_ms: None,
        }
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
            // Runs of calls are grouped by `rows_of`; one on its own is a group of one.
            HistoryItem::Tool { .. } => return self.stored_work(&[item]),
            HistoryItem::TurnFooter {
                files,
                user_row,
                last_row,
            } => (
                RowKind::Footer(Footer::new(files.clone(), None, None, *user_row, *last_row)),
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
        // Anything but a tool call between two calls ends the run.
        if !matches!(kind, RowKind::Work(_))
            && let Some(live) = &mut self.live
        {
            live.work = None;
        }
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

    /// The row `key` and its index, to change.
    fn row_mut(&mut self, key: u64) -> Option<(usize, &mut Row)> {
        let ix = self.index_of(key)?;
        Some((ix, &mut self.rows[ix]))
    }

    /// Opens or closes a group's list of calls.
    pub fn toggle_group(&mut self, key: u64, cx: &mut Context<Self>) {
        if let Some((ix, row)) = self.row_mut(key)
            && let RowKind::Work(group) = &mut row.kind
        {
            group.open = !group.open;
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Opens or closes one call's card (its output is parsed as it opens).
    pub fn toggle_entry(&mut self, key: u64, entry: u64, cx: &mut Context<Self>) {
        if let Some((ix, row)) = self.row_mut(key)
            && let RowKind::Work(group) = &mut row.kind
            && let Some(e) = group.entries.iter_mut().find(|e| e.key == entry)
        {
            e.open = !e.open;
            if e.open {
                e.card.prepare_open();
            }
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Opens or closes one subagent's task under a `delegate_task` card.
    pub fn toggle_subtask(&mut self, key: u64, entry: u64, task: usize, cx: &mut Context<Self>) {
        if let Some((ix, row)) = self.row_mut(key)
            && let RowKind::Work(group) = &mut row.kind
            && let Some(e) = group.entries.iter_mut().find(|e| e.key == entry)
            && let Detail::Delegate(tasks) = &mut e.card.detail
            && let Some(t) = tasks.get_mut(task)
        {
            t.open = !t.open;
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Opens or closes a folder of the footer's changed files.
    pub fn toggle_dir(&mut self, key: u64, path: &str, cx: &mut Context<Self>) {
        if let Some((ix, row)) = self.row_mut(key)
            && let RowKind::Footer(footer) = &mut row.kind
        {
            if !footer.closed.remove(path) {
                footer.closed.insert(path.to_string());
            }
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Opens every folder of the footer's changed files, or closes them all.
    pub fn toggle_all_dirs(&mut self, key: u64, cx: &mut Context<Self>) {
        if let Some((ix, row)) = self.row_mut(key)
            && let RowKind::Footer(footer) = &mut row.kind
        {
            footer.toggle_all();
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Opens every group, card and subagent task (stories, tests): what a person would see
    /// after clicking through.
    pub fn expand_all(&mut self, cx: &mut Context<Self>) {
        for row in &mut self.rows {
            if let RowKind::Work(group) = &mut row.kind {
                group.open = true;
                for entry in &mut group.entries {
                    entry.open = true;
                    entry.card.prepare_open();
                    if let Detail::Delegate(tasks) = &mut entry.card.detail {
                        tasks.iter_mut().for_each(|t| t.open = true);
                    }
                }
            }
        }
        self.scroller.update(cx, |list, cx| list.remeasure(cx));
        cx.notify();
    }

    /// Opens every group's list, the cards staying closed (stories, tests).
    pub fn open_groups(&mut self, cx: &mut Context<Self>) {
        for row in &mut self.rows {
            if let RowKind::Work(group) = &mut row.kind {
                group.open = true;
            }
        }
        self.scroller.update(cx, |list, cx| list.remeasure(cx));
        cx.notify();
    }

    /// Closes every folder of the footers' changed files (stories, tests).
    pub fn close_all_dirs(&mut self, cx: &mut Context<Self>) {
        for row in &mut self.rows {
            if let RowKind::Footer(footer) = &mut row.kind
                && !footer.all_closed()
            {
                footer.toggle_all();
            }
        }
        self.scroller.update(cx, |list, cx| list.remeasure(cx));
        cx.notify();
    }

    /// Marks the newest footer as waiting for a Retry or Branch (stories, tests).
    pub fn mark_footer_busy(&mut self, cx: &mut Context<Self>) {
        if let Some(RowKind::Footer(footer)) = self
            .rows
            .iter_mut()
            .rev()
            .map(|r| &mut r.kind)
            .find(|k| matches!(k, RowKind::Footer(_)))
        {
            footer.busy = true;
        }
        self.scroller.update(cx, |list, cx| list.remeasure(cx));
        cx.notify();
    }

    /// The time the durations on screen are counted to.
    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// Whether a work group of the running turn is running a call (its header counts seconds).
    pub fn counting(&self) -> bool {
        let Some(key) = self.live.as_ref().and_then(|l| l.work_rows.last().copied()) else {
            return false;
        };
        self.index_of(key)
            .is_some_and(|ix| matches!(&self.rows[ix].kind, RowKind::Work(g) if g.running()))
    }

    // --- retry and branch ---------------------------------------------------------------------

    /// Retry: the footer's turn is dropped and its prompt sent again (`Backend::retry`:
    /// `Core::rewind_to_row`, then `submit`); the thread is read again from the store, which no
    /// longer has the turn, and the new turn streams in.
    pub fn retry(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(row) = self.footer_rows(key, true) else {
            return;
        };
        let (backend, session) = (self.backend.clone(), self.session.clone());
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move { backend.retry(&session, row) })
                .await;
            let _ = this.update(cx, |this, cx| match done {
                Ok(()) => this.restart(cx),
                Err(why) => this.action_failed(key, "Could not retry", &why, cx),
            });
        })
        .detach();
    }

    /// Branch from here: a new thread with the conversation up to the footer's last reply
    /// (`Backend::branch`: `Core::branch`). The app model opens it.
    pub fn branch(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(row) = self.footer_rows(key, false) else {
            return;
        };
        let (backend, session) = (self.backend.clone(), self.session.clone());
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move { backend.branch(&session, row) })
                .await;
            let _ = this.update(cx, |this, cx| match done {
                Ok(id) => {
                    this.set_busy(key, false, cx);
                    cx.emit(ThreadEvent::Branched(id));
                }
                Err(why) => this.action_failed(key, "Could not branch", &why, cx),
            });
        })
        .detach();
    }

    /// The stored row a footer action names, marking the footer busy; `None` when there is
    /// nothing to do (no row, already busy, a turn is running, `retry` of a stored prompt only).
    fn footer_rows(&mut self, key: u64, retry: bool) -> Option<usize> {
        if retry && !self.can_retry(key) {
            return None;
        }
        let (_, row) = self.row_mut(key)?;
        let RowKind::Footer(footer) = &mut row.kind else {
            return None;
        };
        let target = if retry {
            footer.user_row
        } else {
            footer.last_row
        }?;
        if footer.busy {
            return None;
        }
        footer.busy = true;
        Some(target)
    }

    /// Whether the footer `key` may Retry: it is the newest turn's (a retry re-runs the last
    /// prompt, as Hermes's `/retry`; an older turn is branched from instead) and no turn runs.
    pub fn can_retry(&self, key: u64) -> bool {
        self.live.is_none()
            && self
                .rows
                .iter()
                .rev()
                .find(|r| matches!(r.kind, RowKind::Footer(_)))
                .is_some_and(|r| r.key == key)
    }

    fn set_busy(&mut self, key: u64, busy: bool, cx: &mut Context<Self>) {
        if let Some((_, row)) = self.row_mut(key)
            && let RowKind::Footer(footer) = &mut row.kind
        {
            footer.busy = busy;
            cx.notify();
        }
    }

    fn action_failed(&mut self, key: u64, what: &str, why: &str, cx: &mut Context<Self>) {
        self.set_busy(key, false, cx);
        self.push(
            RowKind::Notice {
                error: true,
                text: format!("{what}: {why}").into(),
            },
            None,
            cx,
        );
        self.scroll_to_end(cx);
    }

    /// Reads the thread from the store again, from its newest page: after a retry dropped rows (and
    /// `--dev-send`, to read a finished turn back).
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.rows.clear();
        self.live = None;
        self.start = 0;
        self.phase = ThreadPhase::Loading;
        self.loading_older = false;
        self.scroller.update(cx, |list, cx| list.reset(0, cx));
        self.load_page(None, Load::First, cx);
        cx.notify();
    }

    /// The text "copy message" puts on the clipboard: the message's markdown.
    pub fn copy_text(&self, key: u64, cx: &App) -> Option<String> {
        let row = &self.rows[self.index_of(key)?];
        match &row.kind {
            RowKind::User { text, .. } => Some(text.to_string()),
            RowKind::Assistant { body } | RowKind::Reasoning { body, .. } => {
                Some(body.text(cx).to_string())
            }
            RowKind::Notice { text, .. } | RowKind::Command { text, .. } => Some(text.to_string()),
            RowKind::Work(_) | RowKind::Footer(_) => None,
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

    // --- prompts from the composer -------------------------------------------------------------

    /// The composer is sending `text` as admission `id`: its row appears when that turn starts.
    pub fn expect_prompt(&mut self, id: &str, text: &str) {
        self.expect_prompt_with(id, text, Vec::new());
    }

    /// [`Self::expect_prompt`] with the images attached to the prompt (thumbnails in its row).
    pub fn expect_prompt_with(&mut self, id: &str, text: &str, images: Vec<Attachment>) {
        self.expected.push(ExpectedPrompt {
            id: id.to_string(),
            text: text.to_string(),
            images,
        });
    }

    /// The send of admission `id` failed or was withdrawn: no row will come for it.
    pub fn forget_prompt(&mut self, id: &str) {
        self.expected.retain(|expected| expected.id != id);
    }

    /// The prompts the thread waits to show (the composer's tests read it).
    pub fn expected_prompts(&self) -> usize {
        self.expected.len()
    }

    /// Shows `text` as the user's message now (a steer into the running turn has no turn of its
    /// own to start).
    pub fn show_user(&mut self, text: &str, now_ms: i64, cx: &mut Context<Self>) {
        self.show_user_with(text, Vec::new(), now_ms, cx);
    }

    /// [`Self::show_user`] with the images that went with the message.
    pub fn show_user_with(
        &mut self,
        text: &str,
        images: Vec<Attachment>,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        self.push(
            RowKind::User {
                text: text.to_string().into(),
                images: images.iter().filter_map(Thumb::from_attachment).collect(),
            },
            Some(now_ms),
            cx,
        );
        self.scroll_to_end(cx);
    }

    /// [`Self::show_user`] at the model's clock.
    pub fn show_user_now(&mut self, text: &str, cx: &mut Context<Self>) {
        let now = (self.clock)();
        self.show_user(text, now, cx);
    }

    /// What a command answered, shown in the timeline as a note: the `/command` typed and its
    /// text. It belongs to the screen only (the core does not store it, as the page's note is not
    /// stored): reading the thread again does not bring it back, and the model never sees it.
    pub fn show_command_note(&mut self, command: &str, text: &str, cx: &mut Context<Self>) {
        let now = (self.clock)();
        self.push(
            RowKind::Command {
                command: command.to_string().into(),
                text: text.to_string().into(),
            },
            Some(now),
            cx,
        );
        self.scroll_to_end(cx);
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
            CoreEvent::TurnStarted { admission_id, .. } => {
                // The composer's prompt: its row is shown now that its turn has begun (unless
                // the history is not read yet: the store has the prompt and the end reconciles).
                let prompt = self
                    .expected
                    .iter()
                    .position(|expected| expected.id == *admission_id)
                    .map(|at| self.expected.remove(at));
                // Before the history is read, the store may already hold part of this turn
                // (its prompt): the end reconciles.
                self.live = Some(LiveTurn {
                    // A replay is whole by definition (the gallery's thread is never "read").
                    partial: self.phase != ThreadPhase::Ready && !self.replaying,
                    started_ms: now_ms,
                    ..LiveTurn::default()
                });
                if let Some(prompt) = prompt
                    && self.phase == ThreadPhase::Ready
                {
                    self.show_user_with(&prompt.text, prompt.images, now_ms, cx);
                }
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
                self.tool_started(tool_id, name, args, now_ms, cx);
            }
            CoreEvent::ToolProgress {
                tool_id, message, ..
            } => {
                if let Some((ix, entry)) = self.entry_of(tool_id) {
                    self.with_entry(ix, entry, |e| {
                        let open = e.open;
                        e.card.progress(message, open);
                    });
                    self.remeasure(ix, cx);
                    cx.notify();
                }
            }
            CoreEvent::ToolCompleted {
                tool_id,
                text,
                is_error,
                ..
            } => {
                if let Some((ix, entry)) = self.entry_of(tool_id) {
                    self.with_entry(ix, entry, |e| {
                        e.card.finish(text, *is_error);
                        e.ended_ms = Some(now_ms);
                        if e.open {
                            e.card.prepare_open();
                        }
                    });
                    if let Some(live) = &mut self.live
                        && live.delegate.as_deref() == Some(tool_id.as_str())
                    {
                        live.delegate = None;
                    }
                    self.remeasure(ix, cx);
                    cx.notify();
                }
            }
            CoreEvent::Subagent { kind, payload, .. } => self.subagent(kind, payload, cx),
            CoreEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                if let Some(live) = &mut self.live {
                    let usage = live.usage.get_or_insert_with(Usage::default);
                    usage.input += input_tokens.unwrap_or(0).max(0);
                    usage.output += output_tokens.unwrap_or(0).max(0);
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
                let tools: Vec<(String, u64)> = self
                    .live
                    .as_ref()
                    .map(|live| live.tools.iter().map(|(c, k)| (c.clone(), *k)).collect())
                    .unwrap_or_default();
                for (call, key) in tools {
                    if let Some(ix) = self.index_of(key)
                        && let RowKind::Work(group) = &mut self.rows[ix].kind
                        && let Some(entry) = group.entries.iter_mut().rfind(|e| e.call_id == call)
                    {
                        entry.card.interrupt();
                        self.remeasure(ix, cx);
                    }
                }
                // The turn's footer (a turn read back from the store has its own).
                if !partial {
                    let footer = self.footer_of(end, now_ms);
                    self.push(RowKind::Footer(footer), None, cx);
                }
                // The streamed rows stay as they are: no copy, no parse.
                self.live = None;
                if partial && !self.replaying {
                    self.reload(keep_from, cx);
                }
                // The turn may have changed files and commits: the header's git state is read
                // again (a replay is in no folder).
                if !self.replaying {
                    self.refresh_header(cx);
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

    // --- tool calls ---------------------------------------------------------------------------

    /// A call started: it joins the group at the end of the rows when nothing came between, else
    /// it starts a group row. Only that row is touched.
    fn tool_started(
        &mut self,
        call_id: &str,
        name: &str,
        args: &Value,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        let mut entry = ToolEntry::new(self.key(), call_id, ToolCard::started(name, args));
        entry.started_ms = Some(now_ms);
        let live = self.live.get_or_insert_with(LiveTurn::partial);
        let extend = live.work;
        let joined = extend
            .and_then(|key| self.index_of(key))
            .filter(|ix| ix + 1 == self.rows.len());
        let row_key = match joined {
            Some(ix) => {
                let RowKind::Work(group) = &mut self.rows[ix].kind else {
                    unreachable!("the live group row is a work row");
                };
                group.entries.push(entry);
                self.remeasure(ix, cx);
                cx.notify();
                self.rows[ix].key
            }
            None => {
                let mut group = WorkGroup::default();
                group.entries.push(entry);
                let key = self.push(RowKind::Work(group), None, cx);
                let live = self.live.get_or_insert_with(LiveTurn::partial);
                live.work = Some(key);
                live.work_rows.push(key);
                key
            }
        };
        let live = self.live.get_or_insert_with(LiveTurn::partial);
        live.text = None;
        live.tools.insert(call_id.to_string(), row_key);
        if ToolKind::of(name) == ToolKind::Delegate {
            live.delegate = Some(call_id.to_string());
        }
    }

    /// The row index and the entry key of the live call `call_id`.
    fn entry_of(&self, call_id: &str) -> Option<(usize, u64)> {
        let key = *self.live.as_ref()?.tools.get(call_id)?;
        let ix = self.index_of(key)?;
        let RowKind::Work(group) = &self.rows[ix].kind else {
            return None;
        };
        let entry = group.entries.iter().rfind(|e| e.call_id == call_id)?;
        Some((ix, entry.key))
    }

    fn with_entry<R>(
        &mut self,
        ix: usize,
        entry: u64,
        f: impl FnOnce(&mut ToolEntry) -> R,
    ) -> Option<R> {
        let RowKind::Work(group) = &mut self.rows.get_mut(ix)?.kind else {
            return None;
        };
        group.entries.iter_mut().find(|e| e.key == entry).map(f)
    }

    /// A subagent's activity under the running `delegate_task` card (Hermes's `subagent.*`:
    /// `start`, `tool`, `complete`; its thinking and progress text are not shown).
    fn subagent(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) {
        let Some(call) = self.live.as_ref().and_then(|l| l.delegate.clone()) else {
            return;
        };
        let Some((ix, entry)) = self.entry_of(&call) else {
            return;
        };
        let id = payload.get("subagent_id").and_then(Value::as_str);
        let index = payload.get("task_index").and_then(Value::as_u64);
        let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or_default();
        let changed = self.with_entry(ix, entry, |e| {
            let Detail::Delegate(tasks) = &mut e.card.detail else {
                return false;
            };
            let at = tasks
                .iter()
                .position(|t| id.is_some() && t.id.as_deref() == id)
                .or_else(|| index.map(|i| i as usize).filter(|i| *i < tasks.len()));
            let Some(task) = at.and_then(|i| tasks.get_mut(i)) else {
                return false;
            };
            match kind {
                "subagent.start" => {
                    task.id = id.map(str::to_string);
                    task.status = Status::Running;
                    if task.goal.is_empty() {
                        task.goal = text("goal").to_string();
                    }
                }
                "subagent.tool" => task.tools.push(SubTool {
                    name: text("tool_name").to_string(),
                    preview: text("tool_preview").to_string(),
                    failed: false,
                }),
                "subagent.complete" => {
                    task.status = match text("status") {
                        "completed" | "complete" | "ok" | "success" => Status::Done,
                        _ => Status::Failed,
                    };
                    task.summary = text("summary").to_string();
                    task.duration_s = payload.get("duration_seconds").and_then(Value::as_f64);
                }
                _ => return false,
            }
            true
        });
        if changed == Some(true) {
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// The footer a live turn ends with: the files its edit cards changed, its usage and
    /// duration, and the stored rows its actions name.
    fn footer_of(&self, end: &hermes_core::TurnEnd, now_ms: i64) -> Footer {
        let live = self.live.as_ref();
        let mut cards: Vec<&ToolCard> = Vec::new();
        for key in live.map(|l| l.work_rows.as_slice()).unwrap_or_default() {
            if let Some(ix) = self.index_of(*key)
                && let RowKind::Work(group) = &self.rows[ix].kind
            {
                cards.extend(group.entries.iter().map(|e| &e.card));
            }
        }
        Footer::new(
            changed_files(cards),
            live.and_then(|l| l.usage),
            live.map(|l| now_ms.saturating_sub(l.started_ms).max(0) as u64),
            end.persisted.user_row_ids.first().copied(),
            end.persisted.final_assistant_row_id,
        )
    }

    /// Replays a recorded turn as if it arrived now, one event every
    /// [`super::recording::REPLAY_STEP`] (the gallery and the tests): the prompt's row, then
    /// every event.
    pub fn replay(&mut self, recording: &super::recording::Recording, cx: &mut Context<Self>) {
        let now = (self.clock)();
        self.push(
            RowKind::User {
                text: recording.prompt.clone().into(),
                images: Vec::new(),
            },
            Some(now),
            cx,
        );
        self.replay_turn(recording, cx);
    }

    /// [`Self::replay`] without the prompt's row: the events alone (stories of one card).
    pub fn replay_turn(&mut self, recording: &super::recording::Recording, cx: &mut Context<Self>) {
        let step = super::recording::REPLAY_STEP.as_millis() as i64;
        let mut now = (self.clock)();
        self.replaying = true;
        for event in recording.events_for(&self.session) {
            now += step;
            self.on_event_at(&event, now, cx);
        }
        self.replaying = false;
    }
}
