//! The file finder (`Ctrl+P`): one input over a list of the project's files, best match first, the
//! matching characters bold; Up and Down move, Enter opens, Escape closes. It floats over the
//! window like the command palette (the workspace places it) and answers
//! [`FinderEvent::Open`] with the path.
//!
//! **What keeps it fast on a large project:** the files come from the backend's index
//! ([`crate::model::Backend::project_index`]: a walk that leaves out what git ignores, run on a
//! background thread) in batches, and the finder answers as they arrive, so typing works while a
//! 100,000-file project is still being walked. A batch is an immutable chunk ([`Chunk`]) shared
//! with the background ranking, so the UI thread never copies the index and ranking never holds a
//! lock on it. Ranking ([`rank`]) runs on the background executor, one at a time: a keystroke or a
//! new batch that arrives meanwhile re-ranks once when it finishes, and an answer for an older
//! query is dropped. The work is split in two: a byte-level filter over the lower-cased paths
//! keeps the files that contain every word of the query in order, a cheap score picks the best
//! [`CANDIDATES`] of them, and only those are scored again with the command palette's matcher
//! ([`crate::palette::fuzzy`]), which also says which characters to draw bold. At most [`LIMIT`]
//! results are kept and drawn (a virtual list).
//!
//! t3code: `apps/web/src/components/files/ProjectFilePicker.tsx` (the dialog: "Search files...",
//! the matched characters strong, "Indexing workspace files..." / "No matching files." states, the
//! "Open file" action), `ProjectFilePicker.logic.ts` (`PROJECT_FILE_PICKER_RESULT_LIMIT`),
//! `CommandPaletteContent.tsx` (the shell). No screenshot of the picker exists. Gallery story:
//! `file-finder`.
//!
//! Not copied: the preview pane and the "Back" footer.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window,
    assets::IconName,
    component::{
        Icon, Sizable as _, Size as KitSize,
        input::{Input, InputEvent, InputState},
    },
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use super::fs::{IndexSummary, Refused};
use crate::design::tokens::{radius, text};
use crate::dev::ProbeExt as _;
use crate::model::AppModel;
use crate::palette::{Confirm, Dismiss, SelectNext, SelectPrevious, fuzzy, highlighted};
use crate::theme::DesignTheme as _;

/// At most this many results are kept and shown.
pub const LIMIT: usize = 100;
/// The cheap score keeps this many files for the matcher.
pub const CANDIDATES: usize = 400;
/// A result row's height.
pub const ROW_HEIGHT: f32 = 32.;
/// The dialog's width and its list's height (as the command palette's).
pub const WIDTH: f32 = 560.;
pub const LIST_HEIGHT: f32 = 340.;

/// The key context the dialog declares (bound in [`bind`]).
pub const CONTEXT: &str = "FileFinder";

// --- the index --------------------------------------------------------------------------------------

/// A batch of the index: paths, and each lower-cased for the filter.
#[derive(Debug)]
pub struct Chunk {
    pub paths: Vec<String>,
    lower: Vec<String>,
}

impl Chunk {
    pub fn new(paths: Vec<String>) -> Self {
        let lower = paths.iter().map(|p| p.to_lowercase()).collect();
        Self { paths, lower }
    }
}

/// A result: the path and which of its characters matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub score: i32,
    /// Indexes in characters of `path`.
    pub positions: Vec<usize>,
}

/// Whether `word` is in `hay` as a subsequence (both lower case).
fn contains_in_order(word: &[u8], hay: &[u8]) -> bool {
    let mut letters = hay.iter();
    word.iter().all(|w| letters.any(|h| h == w))
}

fn is_boundary(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\' | b'.' | b'_' | b'-' | b' ' | b':')
}

/// A quick score of the word in the path, or `None`: the letters found left to right, more for a
/// letter at the start of a name, for a run, and for a match inside the file's own name.
fn cheap_score(word: &[u8], hay: &[u8]) -> Option<i32> {
    let name_start = hay.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    let mut best: Option<i32> = None;
    // First inside the file's name, then anywhere.
    for (from, bonus) in [(name_start, 40), (0, 0)] {
        let mut at = from;
        let mut score = bonus;
        let mut last: Option<usize> = None;
        let mut found = true;
        for w in word {
            match hay[at..].iter().position(|h| h == w) {
                Some(offset) => {
                    let p = at + offset;
                    score += 10;
                    if p == 0 || is_boundary(hay[p - 1]) {
                        score += 12;
                    }
                    if last == Some(p.wrapping_sub(1)) {
                        score += 14;
                    }
                    last = Some(p);
                    at = p + 1;
                }
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            best = Some(best.map_or(score, |b| b.max(score)));
        }
        if from == 0 {
            break;
        }
    }
    // The word there whole beats the same letters spread out.
    let whole = if word.len() <= hay.len() && hay.windows(word.len()).any(|w| w == word) {
        40
    } else {
        0
    };
    best.map(|s| s + whole - (hay.len() / 8) as i32)
}

/// The best [`LIMIT`] files of `chunks` for `query` (words separated by spaces, each found in
/// order, `\` read as `/`), best first. An empty query gives the first files in index order.
pub fn rank(chunks: &[Arc<Chunk>], query: &str, limit: usize) -> Vec<Hit> {
    let query = query.replace('\\', "/");
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return chunks
            .iter()
            .flat_map(|c| c.paths.iter())
            .take(limit)
            .map(|path| Hit {
                path: path.clone(),
                score: 0,
                positions: Vec::new(),
            })
            .collect();
    }
    // Every file with all the words: a byte filter, then a cheap score.
    let mut kept: Vec<(i32, usize, usize)> = Vec::new();
    for (c, chunk) in chunks.iter().enumerate() {
        for (i, hay) in chunk.lower.iter().enumerate() {
            let hay = hay.as_bytes();
            let mut total = 0;
            let mut all = true;
            for word in &words {
                match contains_in_order(word.as_bytes(), hay)
                    .then(|| cheap_score(word.as_bytes(), hay))
                    .flatten()
                {
                    Some(score) => total += score,
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if all {
                kept.push((total, c, i));
            }
        }
    }
    let by_score = |a: &(i32, usize, usize), b: &(i32, usize, usize)| {
        b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
    };
    if kept.len() > CANDIDATES {
        kept.select_nth_unstable_by(CANDIDATES, by_score);
        kept.truncate(CANDIDATES);
    }
    // The matcher says how well and where, for the few that are left.
    let mut hits: Vec<Hit> = kept
        .into_iter()
        .filter_map(|(_, c, i)| {
            let path = &chunks[c].paths[i];
            let name_start = path
                .char_indices()
                .rev()
                .find(|(_, ch)| *ch == '/')
                .map_or(0, |(at, _)| path[..at].chars().count() + 1);
            let lower = &chunks[c].lower[i];
            let mut score = 0;
            let mut positions = Vec::new();
            for word in &words {
                let (s, p) = fuzzy(word, path)?;
                // Every letter in the file's own name counts for more.
                let in_name = !p.is_empty() && p.iter().all(|&at| at >= name_start);
                // A word that is there whole beats the same letters spread out.
                let whole = if lower.contains(word.as_str()) { 40 } else { 0 };
                score += s + whole + if in_name { 30 } else { 0 };
                positions.extend(p);
            }
            positions.sort_unstable();
            positions.dedup();
            Some(Hit {
                path: path.clone(),
                score,
                positions,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.path.len().cmp(&b.path.len()))
            .then(a.path.cmp(&b.path))
    });
    hits.truncate(limit);
    hits
}

// --- the dialog -------------------------------------------------------------------------------------

/// What the finder tells the workspace.
#[derive(Clone, Debug)]
pub enum FinderEvent {
    /// A file was picked.
    Open(String),
    /// Escape, a click outside.
    Dismiss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Indexing {
    /// The walk is going.
    Running,
    Done {
        truncated: bool,
    },
    Failed(String),
}

pub struct FileFinder {
    app: Entity<AppModel>,
    session: String,
    query: Entity<InputState>,
    chunks: Vec<Arc<Chunk>>,
    files: usize,
    indexing: Indexing,
    hits: Vec<Hit>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// A ranking is running on the background executor.
    ranking: bool,
    /// The query or the index changed while it ran.
    dirty: bool,
    /// Rankings started, for the tests.
    rankings: usize,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<FinderEvent> for FileFinder {}

impl Focusable for FileFinder {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl FileFinder {
    /// A finder over the files of thread `session`'s folder; the index starts at once.
    pub fn new(
        app: Entity<AppModel>,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut finder = Self::new_idle(app, window, cx);
        finder.session = session;
        finder.start_index(cx);
        finder
    }

    fn new_idle(app: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search files..."));
        let subscription =
            cx.subscribe_in(
                &query,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.requery(cx),
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        Self {
            app,
            session: String::new(),
            query,
            chunks: Vec::new(),
            files: 0,
            indexing: Indexing::Running,
            hits: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            ranking: false,
            dirty: false,
            rankings: 0,
            focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    /// A finder over files already known, with `query` typed and the results ranked now: no
    /// index is started (stories, which are drawn twice and cannot wait for a task).
    pub fn with_files(
        app: Entity<AppModel>,
        paths: Vec<String>,
        indexing: Indexing,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut finder = Self::new_idle(app, window, cx);
        finder.files = paths.len();
        finder.chunks = vec![Arc::new(Chunk::new(paths))];
        finder.indexing = indexing;
        finder
            .query
            .update(cx, |q, cx| q.set_value(query, window, cx));
        finder.hits = rank(&finder.chunks, query, LIMIT);
        finder
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.query.update(cx, |q, cx| q.focus(window, cx));
    }

    pub fn query(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |q, cx| q.set_value(text, window, cx));
        self.requery(cx);
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn files(&self) -> usize {
        self.files
    }

    pub fn indexing(&self) -> &Indexing {
        &self.indexing
    }

    pub fn rankings(&self) -> usize {
        self.rankings
    }

    /// Whether a ranking is running or waiting to run (the tests settle on it).
    pub fn busy(&self) -> bool {
        self.ranking || self.dirty
    }

    fn start_index(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.app.read(cx).backend() else {
            self.indexing = Indexing::Failed("Not connected to the core.".to_string());
            return;
        };
        let session = self.session.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<String>>();
        let walk = cx.background_spawn(async move {
            // The batches go over the channel; a closed channel (the finder went away) stops it.
            backend.project_index(&session, &mut |batch| tx.send(batch).is_ok())
        });
        cx.spawn(async move |this, cx| {
            while let Some(batch) = rx.recv().await {
                let Ok(()) = this.update(cx, |this, cx| this.add_batch(batch, cx)) else {
                    return;
                };
            }
            let done = walk.await;
            let _ = this.update(cx, |this, cx| this.index_done(done, cx));
        })
        .detach();
    }

    fn add_batch(&mut self, batch: Vec<String>, cx: &mut Context<Self>) {
        self.files += batch.len();
        // Chunking (lower-casing every path) is the walk's thread's work in a real run, but a
        // batch is only a couple of thousand short strings: cheap on this thread.
        self.chunks.push(Arc::new(Chunk::new(batch)));
        self.requery(cx);
    }

    fn index_done(&mut self, done: Result<IndexSummary, Refused>, cx: &mut Context<Self>) {
        self.indexing = match done {
            Ok(summary) => Indexing::Done {
                truncated: summary.truncated,
            },
            Err(refused) => Indexing::Failed(refused.message()),
        };
        cx.notify();
    }

    /// The query (or the index) changed: rank again, once the running ranking is over.
    fn requery(&mut self, cx: &mut Context<Self>) {
        if self.ranking {
            self.dirty = true;
            return;
        }
        self.rank_now(cx);
    }

    fn rank_now(&mut self, cx: &mut Context<Self>) {
        self.ranking = true;
        self.dirty = false;
        self.rankings += 1;
        let query = self.query(cx);
        let chunks = self.chunks.clone();
        let asked = query.clone();
        cx.spawn(async move |this, cx| {
            let hits = cx
                .background_spawn(async move { rank(&chunks, &asked, LIMIT) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ranking = false;
                // The query moved on while it ran: this answer is for an older one.
                if this.dirty || this.query(cx) != query {
                    this.rank_now(cx);
                    return;
                }
                this.hits = hits;
                this.selected = 0;
                this.scroll.scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
                cx.notify();
            });
        })
        .detach();
    }

    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as isize;
        self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        self.scroll
            .scroll_to_item(self.selected, gpui_kit::ScrollStrategy::Nearest);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(hit) = self.hits.get(self.selected) {
            cx.emit(FinderEvent::Open(hit.path.clone()));
        }
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let hit = &self.hits[ix];
        let selected = ix == self.selected;
        let chars: Vec<char> = hit.path.chars().collect();
        let split = chars.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
        let dir: String = chars[..split].iter().collect();
        let name: String = chars[split..].iter().collect();
        let dir_positions: Vec<usize> = hit
            .positions
            .iter()
            .copied()
            .filter(|&p| p < split)
            .collect();
        let name_positions: Vec<usize> = hit
            .positions
            .iter()
            .filter(|&&p| p >= split)
            .map(|p| p - split)
            .collect();
        div()
            .id(("found", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT))
            .px(px(10.))
            .rounded(radius::LG)
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .when(selected, |d| d.bg(palette.accent.hsla()))
            .hover(move |s| s.bg(palette.accent.hsla()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = ix;
                this.confirm(cx);
            }))
            .child(
                Icon::new(super::files::file_icon(&name))
                    .with_size(KitSize::Size(px(16.)))
                    .text_color(palette.muted_foreground.hsla()),
            )
            .child(
                div()
                    .id(("found-name", ix))
                    .flex_shrink_0()
                    .child(highlighted(&super::fs::shown_name(&name), &name_positions))
                    .probe(),
            )
            .when(!dir.is_empty(), |d| {
                d.child(
                    div()
                        .id("found-dir")
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(text::XS.size)
                        .text_color(palette.muted_foreground.hsla())
                        .child(highlighted(&super::fs::shown_name(&dir), &dir_positions))
                        .probe(),
                )
            })
            .probe()
            .into_any_element()
    }

    /// What the list says when it has no rows.
    fn empty_message(&self, cx: &App) -> String {
        let searching = !self.query(cx).trim().is_empty();
        match &self.indexing {
            Indexing::Failed(why) => why.clone(),
            Indexing::Running => {
                if searching {
                    "Searching the project's files...".to_string()
                } else {
                    "Indexing the project's files...".to_string()
                }
            }
            Indexing::Done { .. } => {
                if searching {
                    "No matching files.".to_string()
                } else {
                    "No files found.".to_string()
                }
            }
        }
    }

    /// The line under the list: how many files, and whether the walk is still going.
    fn status(&self) -> String {
        let files = format!("{} files", group_digits(self.files));
        match &self.indexing {
            Indexing::Running => format!("{files} so far, indexing..."),
            Indexing::Done { truncated: true } => {
                format!("The first {files} are searched; the project has more.")
            }
            Indexing::Done { truncated: false } => files,
            Indexing::Failed(_) => files,
        }
    }
}

/// `100000` as `100,000`.
pub fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Render for FileFinder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let count = self.hits.len();
        // The list gives way in a short window: the dialog sits 12% from the top (the workspace's
        // rule) and its input, status line and padding take 100 px.
        let h = window.viewport_size().height.as_f32();
        let room = (h - (h * 0.12).max(24.) - 100. - 12.).max(ROW_HEIGHT * 2.);
        let rows_height = (count as f32 * ROW_HEIGHT).min(LIST_HEIGHT).min(room);
        let message = (count == 0).then(|| self.empty_message(cx));
        let this = cx.entity();
        div()
            .id("finder")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(FinderEvent::Dismiss)))
            .flex()
            .flex_col()
            .w(px(WIDTH))
            .max_w_full()
            .rounded(radius::XL)
            .border_1()
            .border_color(palette.panel_edge.hsla())
            .bg(palette.panel_surface.over(palette.background).hsla())
            .text_color(palette.popover_foreground.hsla())
            .overflow_hidden()
            .child(
                div()
                    .id("finder-search")
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(48.))
                    .px(px(14.))
                    .border_b_1()
                    .border_color(palette.border.hsla())
                    .child(
                        Icon::new(IconName::Search)
                            .with_size(KitSize::Size(px(16.)))
                            .text_color(palette.muted_foreground.hsla()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.query).appearance(false)),
                    )
                    .probe(),
            )
            .child(match message {
                Some(message) => div()
                    .id("finder-none")
                    .py(px(24.))
                    .text_center()
                    .text_size(text::SM.size)
                    .text_color(palette.muted_foreground.hsla())
                    .child(SharedString::from(message))
                    .probe()
                    .into_any_element(),
                None => div()
                    .id("finder-results")
                    .p(px(6.))
                    .child(
                        uniform_list("finder-list", count, move |range: Range<usize>, _, cx| {
                            this.update(cx, |this, cx| {
                                range.map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
                            })
                        })
                        .track_scroll(&self.scroll)
                        .h(px(rows_height)),
                    )
                    .probe()
                    .into_any_element(),
            })
            .child(
                div()
                    .id("finder-status")
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(28.))
                    .px(px(14.))
                    .border_t_1()
                    .border_color(palette.border.hsla())
                    .text_size(text::XXS2.size)
                    .text_color(palette.muted_foreground.hsla())
                    .child(SharedString::from(self.status()))
                    .child("Enter opens, Esc closes")
                    .probe(),
            )
            .probe()
    }
}

/// Binds the finder's keys (in its own context, as the palette's).
pub fn bind(cx: &mut App) {
    use gpui_kit::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(paths: &[&str]) -> Vec<Arc<Chunk>> {
        vec![Arc::new(Chunk::new(
            paths.iter().map(|p| p.to_string()).collect(),
        ))]
    }

    fn top(chunks: &[Arc<Chunk>], query: &str) -> Vec<String> {
        rank(chunks, query, LIMIT)
            .into_iter()
            .map(|h| h.path)
            .collect()
    }

    #[core::prelude::v1::test]
    fn a_name_beats_a_path_and_a_prefix_beats_a_scatter() {
        let index = chunks(&[
            "src/main.rs",
            "src/main_menu/item.rs",
            "docs/domain.rs",
            "crates/app/src/mod.rs",
            "README.md",
            "Cargo.toml",
            "src/util/mean_ai_notes.rs",
        ]);
        // The file called main comes before the folder called main_menu and the scatter.
        assert_eq!(top(&index, "main")[0], "src/main.rs");
        assert_eq!(top(&index, "main").len(), 4);
        // A word that is only in the folders still finds its files.
        assert_eq!(top(&index, "menu item"), ["src/main_menu/item.rs"]);
        // Letters in order, case does not matter, `\` is `/`.
        assert_eq!(top(&index, "CARGO")[0], "Cargo.toml");
        assert_eq!(top(&index, "src\\main.rs")[0], "src/main.rs");
        assert_eq!(top(&index, "rdme"), ["README.md"]);
        // No match.
        assert!(top(&index, "zzz").is_empty());
        // Two words must both be there (any order across the path: each in order).
        assert_eq!(top(&index, "app mod"), ["crates/app/src/mod.rs"]);
    }

    #[core::prelude::v1::test]
    fn a_hit_says_which_characters_matched() {
        let index = chunks(&["src/main.rs", "docs/maintenance.md"]);
        let hits = rank(&index, "main", LIMIT);
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[0].positions, [4, 5, 6, 7]);
        let hits = rank(&index, "mr", LIMIT);
        assert_eq!(hits[0].path, "src/main.rs", "m then r in the name");
    }

    #[core::prelude::v1::test]
    fn an_empty_query_lists_the_first_files_and_the_result_is_capped() {
        let paths: Vec<String> = (0..1_000).map(|n| format!("dir/f{n:04}.rs")).collect();
        let index = vec![Arc::new(Chunk::new(paths))];
        let none = rank(&index, "  ", 100);
        assert_eq!(none.len(), 100);
        assert_eq!(none[0].path, "dir/f0000.rs");
        assert!(none[0].positions.is_empty());
        // 1,000 files match "f", only LIMIT come back, and the candidates cap does not drop the
        // best: the shortest, best-aligned name is first.
        let some = rank(&index, "f0999", 100);
        assert_eq!(some[0].path, "dir/f0999.rs");
        assert_eq!(rank(&index, "rs", 100).len(), 100);
    }

    #[core::prelude::v1::test]
    fn a_non_ascii_name_is_found_in_any_case() {
        let index = chunks(&["docs/Καλημέρα.md", "docs/cafe.md", "docs/café.md"]);
        assert_eq!(top(&index, "καλη"), ["docs/Καλημέρα.md"]);
        assert_eq!(top(&index, "CAFÉ"), ["docs/café.md"]);
        assert!(top(&index, "caf").contains(&"docs/cafe.md".to_string()));
    }

    #[core::prelude::v1::test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(100_000), "100,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[core::prelude::v1::test]
    fn ranking_a_hundred_thousand_paths_is_fast_enough_to_type_through() {
        // Paths shaped like the 100,000-file test tree.
        let paths: Vec<String> = (0..100_000)
            .map(|n| format!("pkg{:04}/src/file{n:06}.rs", n % 1_000))
            .collect();
        let index: Vec<Arc<Chunk>> = paths
            .chunks(2_000)
            .map(|c| Arc::new(Chunk::new(c.to_vec())))
            .collect();
        for query in ["f", "fi", "file0420", "pkg0420 file", "src rs", "zzzz"] {
            let started = std::time::Instant::now();
            let hits = rank(&index, query, LIMIT);
            let took = started.elapsed();
            println!(
                "rank {query:?} over 100,000 paths: {} hits in {took:?}",
                hits.len()
            );
            assert!(
                took < std::time::Duration::from_secs(3),
                "{query}: {took:?}"
            );
        }
        let hits = rank(&index, "file042000", LIMIT);
        assert_eq!(hits[0].path, "pkg0000/src/file042000.rs");
    }
}
