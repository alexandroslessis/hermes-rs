//! The command palette: `Ctrl+K` opens it over actions, threads and projects, `Ctrl+Shift+P`
//! over the actions only. One input at the top, grouped results under it, Up and Down to move,
//! Enter to run, Esc to close; the matching characters are drawn bold. For now the threads and
//! projects are fake ([`crate::fake::palette_threads`]); Phase 3 feeds it from the core.
//!
//! Two parts. The **search** ([`fuzzy`], [`search`]) is plain functions: a query is split on
//! spaces and every word must be found, in order, inside the title (or its subtitle); scores
//! favour a match at the start, at word starts (after a space, `-`, `_`, `/` or `.`) and in an
//! unbroken run, and punish gaps. It is our own matcher (about sixty lines) rather than a crate:
//! nothing in `Cargo.lock` does this and the lists are a few hundred entries. The **view**
//! ([`Palette`]) is a GPUI entity that emits [`PaletteEvent`]s; the workspace decides what
//! running an item does.
//!
//! t3code: `apps/web/src/components/CommandPalette.tsx`, `CommandPalette.logic.ts`,
//! `CommandPaletteResults.tsx` (groups, the active row, the kbd hints), `shortcuts/
//! commandPaletteCommands.ts`. No reference screenshot of the palette exists and t3code was not
//! run to take one (progress note); gallery story: `palette`. Not copied: the thread preview
//! pane, the theme and add-project sub-pages (later phases).

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, actions,
    assets::IconName,
    component::{
        Icon, Sizable as _, Size as KitSize,
        input::{Input, InputEvent, InputState},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::design::{
    KbdGroup,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::keymap;
use crate::theme::DesignTheme as _;

actions!(
    command_palette,
    [SelectNext, SelectPrevious, Confirm, Dismiss]
);

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A command from [`keymap::COMMANDS`].
    Action,
    /// A thread.
    Thread,
    /// A project.
    Project,
}

impl Kind {
    /// The group heading.
    pub fn heading(self) -> &'static str {
        match self {
            Kind::Action => "Actions",
            Kind::Thread => "Threads",
            Kind::Project => "Projects",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Kind::Action => IconName::Command,
            Kind::Thread => IconName::MessageSquare,
            Kind::Project => IconName::Folder,
        }
    }
}

/// One entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// What it is.
    pub kind: Kind,
    /// Its id (a command id, a thread id, a project path).
    pub id: String,
    /// The title searched and shown.
    pub title: String,
    /// A note under or beside the title (a project name, a folder), searched too.
    pub subtitle: Option<String>,
    /// The shortcut label (`Ctrl+B`).
    pub shortcut: Option<String>,
}

/// Which entries the palette covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Actions, threads and projects (`Ctrl+K`).
    All,
    /// Only actions (`Ctrl+Shift+P`).
    Actions,
}

/// The commands as entries.
pub fn action_items() -> Vec<Item> {
    keymap::COMMANDS
        .iter()
        .map(|c| Item {
            kind: Kind::Action,
            id: c.id.to_string(),
            title: c.title.to_string(),
            subtitle: None,
            shortcut: keymap::shortcut_label(c),
        })
        .collect()
}

fn is_boundary(c: char) -> bool {
    matches!(c, ' ' | '-' | '_' | '/' | '\\' | '.' | ':')
}

/// Matches `word` as a subsequence of `text`, ignoring case; the score (higher is better) and the
/// matched character indexes. Tries every place the first letter occurs and keeps the best.
pub fn fuzzy(word: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let word: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    if word.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut best: Option<(i32, Vec<usize>)> = None;
    for start in (0..lower.len()).filter(|i| lower[*i] == word[0]) {
        let mut positions = vec![start];
        let mut at = start + 1;
        for letter in &word[1..] {
            match lower[at.min(lower.len())..]
                .iter()
                .position(|c| c == letter)
            {
                Some(offset) => {
                    positions.push(at + offset);
                    at += offset + 1;
                }
                None => break,
            }
        }
        if positions.len() != word.len() {
            continue;
        }
        let mut score = 0;
        for (n, &p) in positions.iter().enumerate() {
            let boundary = p == 0 || is_boundary(chars[p - 1]);
            score += 10;
            if boundary {
                score += 12;
            }
            if p == 0 {
                score += 8;
            }
            if n > 0 {
                let gap = p - positions[n - 1] - 1;
                score += if gap == 0 { 14 } else { -(gap.min(8) as i32) };
            }
        }
        // Shorter texts win ties: "New thread" over "New thread in a project".
        score -= (chars.len() / 8) as i32;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, positions));
        }
    }
    best
}

/// A match: which entry, how well, and which characters of its title matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Index into the searched list.
    pub index: usize,
    /// The score.
    pub score: i32,
    /// Indexes (in characters) of the title that matched, for highlighting.
    pub positions: Vec<usize>,
}

/// The entries of `items` matching `query`, best first. An empty query gives every entry in list
/// order. Each space-separated word must match the title or the subtitle; the groups keep their
/// order (actions, threads, projects) and results are sorted within a group.
pub fn search(items: &[Item], query: &str) -> Vec<Hit> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut hits = Vec::new();
    'items: for (index, item) in items.iter().enumerate() {
        let mut score = 0;
        let mut positions = Vec::new();
        for word in &words {
            if let Some((s, p)) = fuzzy(word, &item.title) {
                score += s;
                positions.extend(p);
            } else if let Some((s, _)) = item.subtitle.as_deref().and_then(|t| fuzzy(word, t)) {
                // Found in the note: counts, but less than a title match.
                score += s - 10;
            } else {
                continue 'items;
            }
        }
        positions.sort_unstable();
        positions.dedup();
        hits.push(Hit {
            index,
            score,
            positions,
        });
    }
    if !words.is_empty() {
        hits.sort_by(|a, b| {
            items[a.index]
                .kind
                .cmp(&items[b.index].kind)
                .then(b.score.cmp(&a.score))
                .then(a.index.cmp(&b.index))
        });
    }
    hits
}

/// What the palette tells the workspace.
#[derive(Clone, Debug)]
pub enum PaletteEvent {
    /// The user picked an entry.
    Run(Item),
    /// The palette should close (Esc, a click outside).
    Dismiss,
}

/// The palette view.
pub struct Palette {
    mode: Mode,
    all: Vec<Item>,
    hits: Vec<Hit>,
    selected: usize,
    query: Entity<InputState>,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

/// The widest the palette gets.
pub const WIDTH: f32 = 560.;
/// The tallest the result list gets.
pub const LIST_HEIGHT: f32 = 340.;
/// A result row's height.
pub const ROW_HEIGHT: f32 = 32.;
/// The most results listed (best first; type more to narrow).
pub const MAX_RESULTS: usize = 200;

impl Palette {
    /// A palette over `items` (the commands first, then threads and projects), in `mode`.
    pub fn new(mode: Mode, items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(match mode {
                Mode::All => "Search actions, threads and projects",
                Mode::Actions => "Search actions",
            })
        });
        let subscription =
            cx.subscribe_in(
                &query,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.refresh(cx),
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        let mut palette = Self {
            mode,
            all: Vec::new(),
            hits: Vec::new(),
            selected: 0,
            query,
            focus: cx.focus_handle(),
            _subscription: subscription,
        };
        palette.set_items(items, cx);
        palette
    }

    /// Replaces the entries (a thread list that changed).
    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.all = items
            .into_iter()
            .filter(|i| self.mode == Mode::All || i.kind == Kind::Action)
            .collect();
        self.refresh(cx);
    }

    /// Focuses the input.
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.query.update(cx, |q, cx| q.focus(window, cx));
    }

    /// The text typed so far.
    pub fn query(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    /// Sets the text (a test, or "Theme: " prefilled by a command).
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |q, cx| q.set_value(text, window, cx));
        self.refresh(cx);
    }

    /// The entries shown, best first, with the one selected.
    pub fn results(&self) -> Vec<&Item> {
        self.hits.iter().map(|h| &self.all[h.index]).collect()
    }

    /// The selected row.
    pub fn selected(&self) -> usize {
        self.selected
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.hits = search(&self.all, &query);
        // The list is not virtual: a few hundred rows are drawn at most (thousands of threads
        // would otherwise all be laid out on every keystroke).
        self.hits.truncate(MAX_RESULTS);
        self.selected = 0;
        cx.notify();
    }

    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as isize;
        self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(hit) = self.hits.get(self.selected) {
            cx.emit(PaletteEvent::Run(self.all[hit.index].clone()));
        }
    }

    fn row(&self, ix: usize, hit: &Hit, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let item = &self.all[hit.index];
        let selected = ix == self.selected;
        let title = highlighted(&item.title, &hit.positions);
        div()
            .id(("result", ix))
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
                Icon::new(item.kind.icon())
                    .with_size(KitSize::Size(px(16.)))
                    .text_color(palette.muted_foreground.hsla()),
            )
            .child(
                div()
                    .id("title")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title)
                    .probe(),
            )
            .when_some(item.subtitle.clone(), |d, note| {
                d.child(
                    div()
                        .id("note")
                        .flex_shrink_0()
                        .text_size(text::XS.size)
                        .text_color(palette.muted_foreground.hsla())
                        .child(note),
                )
            })
            .when_some(item.shortcut.clone(), |d, shortcut| {
                d.child(KbdGroup::new(&shortcut))
            })
            .probe()
            .into_any_element()
    }
}

/// The title as styled text: matched characters bold.
pub(crate) fn highlighted(title: &str, positions: &[usize]) -> AnyElement {
    let mut row = div().flex();
    let mut run = String::new();
    let mut bold = false;
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (ix, c) in title.chars().enumerate() {
        let is = positions.contains(&ix);
        if is != bold && !run.is_empty() {
            runs.push((std::mem::take(&mut run), bold));
        }
        bold = is;
        run.push(c);
    }
    if !run.is_empty() {
        runs.push((run, bold));
    }
    for (text, bold) in runs {
        row = row.child(
            div()
                .whitespace_nowrap()
                .when(bold, |d| d.font_weight(FontWeight::BOLD))
                .child(SharedString::from(text)),
        );
    }
    row.into_any_element()
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        // Group headings where the kind changes.
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut last: Option<Kind> = None;
        for (ix, hit) in self.hits.clone().iter().enumerate() {
            let kind = self.all[hit.index].kind;
            if last != Some(kind) {
                rows.push(
                    div()
                        .id(("heading", ix))
                        .px(px(10.))
                        .pt(px(8.))
                        .pb(px(4.))
                        .text_size(text::XS.size)
                        .line_height(text::XS.line_height)
                        .text_color(palette.muted_foreground.hsla())
                        .child(kind.heading())
                        .into_any_element(),
                );
                last = Some(kind);
            }
            rows.push(self.row(ix, hit, cx));
        }
        let empty = self.hits.is_empty();
        let _ = window;
        div()
            .id("palette")
            .key_context("CommandPalette")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PaletteEvent::Dismiss)))
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
                    .id("search")
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
            .child(
                div()
                    .id("results")
                    .flex()
                    .flex_col()
                    .p(px(6.))
                    .max_h(px(LIST_HEIGHT))
                    .overflow_y_scroll()
                    .when(empty, |d| {
                        d.child(
                            div()
                                .id("none")
                                .py(px(24.))
                                .text_center()
                                .text_size(text::SM.size)
                                .text_color(palette.muted_foreground.hsla())
                                .child("No matches"),
                        )
                    })
                    .children(rows)
                    .probe(),
            )
            .probe()
    }
}

/// Binds the palette's keys (in its own context, so they do not leak).
pub fn bind(cx: &mut App) {
    use gpui_kit::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some("CommandPalette")),
        KeyBinding::new("up", SelectPrevious, Some("CommandPalette")),
        KeyBinding::new("ctrl-n", SelectNext, Some("CommandPalette")),
        KeyBinding::new("ctrl-p", SelectPrevious, Some("CommandPalette")),
        KeyBinding::new("enter", Confirm, Some("CommandPalette")),
        KeyBinding::new("escape", Dismiss, Some("CommandPalette")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: Kind, title: &str) -> Item {
        Item {
            kind,
            id: title.to_lowercase(),
            title: title.into(),
            subtitle: None,
            shortcut: None,
        }
    }

    #[core::prelude::v1::test]
    fn a_word_matches_as_a_case_insensitive_subsequence() {
        assert!(fuzzy("tgl", "Toggle sidebar").is_some());
        assert!(fuzzy("TOGGLE", "Toggle sidebar").is_some());
        assert!(fuzzy("xyz", "Toggle sidebar").is_none());
        // In order: "ba" is not in "ab".
        assert!(fuzzy("ba", "ab").is_none());
        assert_eq!(fuzzy("", "anything"), Some((0, vec![])));
    }

    #[core::prelude::v1::test]
    fn matched_positions_are_reported() {
        let (_, positions) = fuzzy("ts", "Toggle sidebar").unwrap();
        assert_eq!(positions, vec![0, 7], "the T and the s of sidebar");
    }

    #[core::prelude::v1::test]
    fn word_starts_and_runs_beat_scattered_letters() {
        let score = |q, t| fuzzy(q, t).unwrap().0;
        // "side" as a run at a word start beats the same letters spread out.
        assert!(score("side", "Toggle sidebar") > score("side", "Slide in the editor"));
        // A prefix beats a middle match.
        assert!(score("new", "New thread") > score("new", "Open a new thread"));
        // Shorter wins a tie.
        assert!(score("new", "New thread") > score("new", "New thread in another project"));
    }

    #[core::prelude::v1::test]
    fn every_word_of_the_query_must_match_in_any_order() {
        let items = vec![
            item(Kind::Action, "Toggle right panel"),
            item(Kind::Action, "Toggle sidebar"),
        ];
        let titles = |q: &str| -> Vec<String> {
            search(&items, q)
                .iter()
                .map(|h| items[h.index].title.clone())
                .collect()
        };
        assert_eq!(titles("panel toggle"), vec!["Toggle right panel"]);
        assert_eq!(titles("tog").len(), 2);
        assert!(titles("toggle zzz").is_empty());
    }

    #[core::prelude::v1::test]
    fn the_subtitle_is_searched_but_ranks_below_a_title_match() {
        let mut thread = item(Kind::Thread, "Fix the sidebar");
        thread.subtitle = Some("hermes-rs".into());
        let other = item(Kind::Thread, "Hermes notes");
        let items = vec![thread, other];
        let hits = search(&items, "hermes");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            items[hits[0].index].title, "Hermes notes",
            "the title match first"
        );
    }

    #[core::prelude::v1::test]
    fn results_keep_actions_then_threads_then_projects() {
        let items = vec![
            item(Kind::Project, "native"),
            item(Kind::Thread, "native ui"),
            item(Kind::Action, "Native settings"),
        ];
        let kinds: Vec<_> = search(&items, "native")
            .iter()
            .map(|h| items[h.index].kind)
            .collect();
        assert_eq!(kinds, vec![Kind::Action, Kind::Thread, Kind::Project]);
        // An empty query lists everything in list order.
        assert_eq!(search(&items, "").len(), 3);
        assert_eq!(search(&items, "  ").len(), 3);
    }

    #[core::prelude::v1::test]
    fn unicode_titles_match_by_character_and_highlight_the_right_ones() {
        // Greek, case-insensitive; positions count characters, not bytes.
        let (_, positions) = fuzzy("καλη", "Καλημέρα κόσμε").unwrap();
        assert_eq!(positions, vec![0, 1, 2, 3]);
        assert!(fuzzy("ΚΣΜ", "Καλημέρα κόσμε").is_some());
        // Accents count: "ο" is not "ό" (no accent folding yet).
        assert!(fuzzy("κοσ", "Καλημέρα κόσμε").is_none());
        // Emoji and accents neither panic nor match by accident.
        assert!(fuzzy("x", "🚀 launch").is_none());
        assert!(fuzzy("🚀", "🚀 launch").is_some());
        assert!(fuzzy("e", "é").is_none());
    }

    #[core::prelude::v1::test]
    fn ten_thousand_entries_are_searched_quickly() {
        let items: Vec<Item> = (0..10_000)
            .map(|i| {
                item(
                    Kind::Thread,
                    &format!("Thread number {i} about the sidebar"),
                )
            })
            .collect();
        let started = std::time::Instant::now();
        let hits = search(&items, "side 99");
        assert!(!hits.is_empty());
        assert!(
            hits.iter()
                .all(|h| items[h.index].title.matches('9').count() >= 2)
        );
        // Generous (debug build, a busy machine); a quadratic slip would take seconds.
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[core::prelude::v1::test]
    fn the_command_list_has_every_command_with_its_shortcut() {
        let items = action_items();
        assert_eq!(items.len(), keymap::COMMANDS.len());
        let sidebar = items.iter().find(|i| i.id == "sidebar.toggle").unwrap();
        assert_eq!(sidebar.shortcut.as_deref(), Some("Ctrl+B"));
    }

    /// Every action, in the palette's order (a new command once pushed "Toggle right panel" out of
    /// the first five rows, which the gallery story shows, and so out of its "tog" search).
    #[core::prelude::v1::test]
    fn the_actions_are_listed_in_their_order_and_tog_finds_both_panel_toggles() {
        let items = action_items();
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Open command palette",
                "Show all actions",
                "New thread",
                "Toggle sidebar",
                "Toggle right panel",
                "Toggle terminal drawer",
                "Go to file",
                "Cycle theme",
                "Focus next region",
                "Focus previous region",
                "Open settings",
            ]
        );
        let first_five: Vec<Item> = items.into_iter().take(5).collect();
        let found: Vec<&str> = search(&first_five, "tog")
            .iter()
            .map(|h| first_five[h.index].title.as_str())
            .collect();
        assert_eq!(found, ["Toggle sidebar", "Toggle right panel"]);
    }
}
