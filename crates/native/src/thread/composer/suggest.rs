//! The `@` and `/` menus of the composer: the drawer above the card that lists files, folders and
//! commands as the user types, how the keys move in it (arrows, Enter, Tab, Escape), and what a
//! pick does to the input (a mention becomes an atomic chip, a command is completed, a folder is
//! walked into). What the menus list comes from [`ComposerModel`] (the core's completion and
//! catalog, asked off the UI thread); where in the text the user is typing is
//! [`mention::trigger`].
//!
//! The drawer is t3code's: a panel above the card with the entries as rows (an icon, the name,
//! what it is in muted text, a badge for a skill), the highlighted one filled, at most 288 px
//! tall and scrolling, "No matching files or folders." / "No matching command." when nothing
//! fits. t3code anchors it to the composer (not to the caret), over the card's width; so do we.
//! Picking a file or folder with Enter or a click makes a chip (the text under it is the
//! reference the runtime reads, `@file:src/a.rs`); Tab on a folder walks into it (`@src/`), so
//! its own entries follow; `@file:` and the other prefixes are typed on. A command with no
//! arguments runs on Enter; with arguments, Enter and Tab complete it and the hint under the
//! input says what comes next.
//!
//! t3code: `components/chat/ComposerCommandMenu.tsx` and `ComposerCommandMenuLayer.tsx` (the
//! drawer, its rows, the empty texts), `composer-logic.ts` (`detectComposerTrigger`),
//! `useComposerMenuState.ts` (the highlight, Enter, Tab and Escape). Reference screenshots:
//! none of the menu is in `docs/native-ui/reference/t3code/` (see the README there for the
//! crops made for CP5.2); the card is `app-thread-dark.png`.

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{Sizable as _, Size as KitSize, input::InlineToken},
    div,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::mention::{
    self, Chip, ChipKind, CommandOrigin, CommandSpec, MentionKind, Suggestion, Trigger,
};
use super::view::ComposerView;
use crate::design::{
    Badge, badge,
    icons::icon,
    tokens::{Palette, radius, text},
};
use crate::dev::ProbeExt as _;

/// The drawer's tallest (`max-h-72`).
pub const MAX_HEIGHT: f32 = 288.;
/// An entry's height.
pub const ROW_HEIGHT: f32 = 32.;
/// The sides of the card the drawer leaves free (`--chat-composer-drawer-inset`).
pub const INSET: f32 = 12.;

/// A key the menu takes while it is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    /// Enter: pick (a command without arguments runs).
    Enter,
    /// Tab: complete (a folder is walked into).
    Tab,
    Escape,
}

/// One row of the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Mention(Suggestion),
    Command(CommandSpec),
}

impl ComposerView {
    /// The byte ranges of the chips in the input.
    pub(super) fn token_ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.input
            .read(cx)
            .tokens()
            .iter()
            .map(|span| span.range())
            .collect()
    }

    /// Reads where the caret is and asks what that calls for: the `@` entries of the word before
    /// it, the commands, or nothing.
    pub(super) fn sync_trigger(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let tokens = self.token_ranges(cx);
        let next = mention::trigger(&text, cursor, &tokens);
        if next == self.trigger {
            return;
        }
        let changed_word = match (&self.trigger, &next) {
            (Some(Trigger::Mention { word: a, .. }), Some(Trigger::Mention { word: b, .. })) => {
                a != b
            }
            (Some(Trigger::Command { query: a }), Some(Trigger::Command { query: b })) => a != b,
            _ => true,
        };
        if changed_word {
            self.highlight = 0;
        }
        if self
            .dismissed
            .is_some_and(|d| Some(d) != next.as_ref().map(Trigger::key))
        {
            self.dismissed = None;
        }
        match &next {
            Some(Trigger::Mention { word, .. }) => {
                let word = word.clone();
                self.model.update(cx, |m, cx| m.request_mentions(&word, cx));
            }
            Some(Trigger::Command { .. }) | Some(Trigger::Arguments { .. }) => {
                self.model.update(cx, |m, cx| {
                    m.clear_mentions(cx);
                    m.load_commands(cx);
                });
            }
            None => self.model.update(cx, |m, cx| m.clear_mentions(cx)),
        }
        self.trigger = next;
        cx.notify();
    }

    /// The menu the caret asks for, unless Escape put it away.
    pub fn suggest_trigger(&self) -> Option<&Trigger> {
        let trigger = self.trigger.as_ref()?;
        if self.dismissed == Some(trigger.key()) {
            return None;
        }
        Some(trigger)
    }

    /// The rows the drawer lists now.
    pub fn entries(&self, cx: &App) -> Vec<Entry> {
        match self.suggest_trigger() {
            Some(Trigger::Mention { .. }) => self
                .model
                .read(cx)
                .mentions()
                .map(|m| m.items.iter().cloned().map(Entry::Mention).collect())
                .unwrap_or_default(),
            Some(Trigger::Command { query }) => {
                let all = self.model.read(cx).commands();
                mention::filter_commands(&all, query)
                    .into_iter()
                    .cloned()
                    .map(Entry::Command)
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The highlighted row's index.
    pub fn highlighted(&self, cx: &App) -> usize {
        self.highlight.min(self.entries(cx).len().saturating_sub(1))
    }

    /// Whether the drawer shows (rows, a loading line, an empty line, or an argument hint).
    pub fn suggest_open(&self, cx: &App) -> bool {
        match self.suggest_trigger() {
            Some(Trigger::Mention { .. }) | Some(Trigger::Command { .. }) => true,
            Some(Trigger::Arguments { name }) => self.known_command(name, cx).is_some(),
            None => false,
        }
    }

    /// The usage line shown while a command's arguments are typed (`/diff [git diff arguments]`).
    pub fn argument_usage(&self, cx: &App) -> Option<String> {
        match self.suggest_trigger() {
            Some(Trigger::Arguments { name }) => self.known_command(name, cx).map(|c| c.usage()),
            _ => None,
        }
    }

    fn known_command(&self, name: &str, cx: &App) -> Option<CommandSpec> {
        self.model
            .read(cx)
            .commands()
            .into_iter()
            .find(|c| c.name == name)
    }

    /// A key while the menu is open: true when the menu took it (the input must not see it).
    pub fn suggest_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.suggest_open(cx) {
            return false;
        }
        let entries = self.entries(cx);
        match key {
            Key::Escape => {
                if let Some(trigger) = self.trigger.as_ref() {
                    self.dismissed = Some(trigger.key());
                }
                cx.notify();
                true
            }
            // The argument hint takes no keys but Escape.
            _ if entries.is_empty() => false,
            Key::Up | Key::Down => {
                let at = self.highlighted(cx);
                let last = entries.len() - 1;
                self.highlight = match key {
                    Key::Up => at.checked_sub(1).unwrap_or(last),
                    _ if at == last => 0,
                    _ => at + 1,
                };
                self.list_scroll.scroll_to_item(self.highlight);
                cx.notify();
                true
            }
            Key::Enter | Key::Tab => {
                let at = self.highlighted(cx);
                self.accept(at, key == Key::Tab, window, cx);
                true
            }
        }
    }

    /// Picks row `ix` (`tab`: Tab was pressed, which walks into a folder and only completes a
    /// command).
    pub fn accept(&mut self, ix: usize, tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.entries(cx);
        let Some(entry) = entries.get(ix).cloned() else {
            return;
        };
        match (entry, self.suggest_trigger().cloned()) {
            (Entry::Mention(item), Some(Trigger::Mention { range, .. })) => {
                self.accept_mention(&item, range, tab, window, cx)
            }
            (Entry::Command(spec), Some(Trigger::Command { .. })) => {
                self.accept_command(&spec, tab, window, cx)
            }
            _ => {}
        }
        self.focus_input(window, cx);
    }

    fn accept_mention(
        &mut self,
        item: &Suggestion,
        range: Range<usize>,
        tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A prefix is typed on; Tab on a folder walks into it.
        let typed = match (item.kind, tab) {
            (MentionKind::Prefix, _) => Some(item.text.clone()),
            (MentionKind::Folder, true) => item.drill_text(),
            _ => None,
        };
        if let Some(text) = typed {
            self.replace_range(range, &text, window, cx);
            return;
        }
        let Some(chip) = item.chip() else {
            return;
        };
        let replaced = self.input.update(cx, |input, cx| {
            input
                .replace_range_with_token(range.clone(), token_of(&chip), window, cx)
                .is_ok()
        });
        if !replaced {
            // The input would not hold a chip here (an IME composition, say): the text is the
            // same reference.
            self.replace_range(range, &chip.text, window, cx);
        }
        // A space after it, so the next word starts apart (t3code does the same).
        let next_is_space = {
            let input = self.input.read(cx);
            input
                .value()
                .get(input.cursor()..)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        };
        if !next_is_space {
            self.input
                .update(cx, |input, cx| input.insert(" ", window, cx));
        }
        self.edited(cx);
    }

    fn accept_command(
        &mut self,
        spec: &CommandSpec,
        tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No arguments to type and not a skill: Enter runs it.
        if !tab && !spec.takes_arguments() {
            self.set_text(&format!("/{}", spec.name), window, cx);
            self.submit(false, window, cx);
            return;
        }
        let end = {
            let text = self.input.read(cx).value().to_string();
            text.find(char::is_whitespace).unwrap_or(text.len())
        };
        let rest_has_space = self
            .input
            .read(cx)
            .value()
            .get(end..)
            .is_some_and(|r| r.starts_with(char::is_whitespace));
        let completed = if rest_has_space {
            format!("/{}", spec.name)
        } else {
            format!("/{} ", spec.name)
        };
        self.replace_range(0..end, &completed, window, cx);
    }

    /// Replaces a byte range of the input with plain text, the caret after it.
    fn replace_range(
        &mut self,
        range: Range<usize>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_selected_range(range, cx);
            input.replace(text.to_string(), window, cx);
        });
        self.edited(cx);
    }

    // --- drawing --------------------------------------------------------------------------------

    /// The drawer above the card (`None` while no menu is called for).
    pub(super) fn suggest_drawer(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.suggest_open(cx) {
            return None;
        }
        let entries = self.entries(cx);
        let highlighted = self.highlighted(cx);
        // Never taller than what is above the card: the window less the card and its margins.
        let room = (window.viewport_size().height.as_f32() - 220.).max(96.);
        let max_height = MAX_HEIGHT.min(room);
        let body: AnyElement = match self.suggest_trigger() {
            Some(Trigger::Arguments { name }) => self.argument_hint(name, palette, cx),
            Some(trigger) if entries.is_empty() => self.empty_line(trigger, palette, cx),
            _ => self.entry_list(&entries, highlighted, max_height, palette, cx),
        };
        Some(
            div()
                .id("composer-suggest")
                .occlude()
                .absolute()
                .bottom(relative(1.))
                .left(px(INSET))
                .right(px(INSET))
                .mb(px(6.))
                .flex()
                .flex_col()
                .rounded(radius::XL)
                .border_1()
                // t3code's popovers are `dropdown-glass` (the panel tokens).
                .border_color(palette.panel_edge.hsla())
                .bg(palette.panel_surface.over(palette.background).hsla())
                .text_color(palette.popover_foreground.hsla())
                .overflow_hidden()
                .child(body)
                .probe()
                .into_any_element(),
        )
    }

    fn entry_list(
        &self,
        entries: &[Entry],
        highlighted: usize,
        max_height: f32,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = entries.iter().enumerate().map(|(ix, entry)| {
            let (pick, hover) = (cx.entity(), cx.entity());
            let active = ix == highlighted;
            let (glyph, label, description, badge) = entry_parts(entry);
            div()
                .id(("suggest-item", ix))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(8.))
                .h(px(ROW_HEIGHT))
                .px(px(10.))
                .rounded(radius::LG)
                .cursor_pointer()
                .when(active, |d| d.bg(palette.accent.hsla()))
                .on_hover(move |hovered, _, cx| {
                    if *hovered {
                        hover.update(cx, |this, cx| {
                            if this.highlight != ix {
                                this.highlight = ix;
                                cx.notify();
                            }
                        });
                    }
                })
                .on_click(move |_, window, cx| {
                    pick.update(cx, |this, cx| this.accept(ix, false, window, cx))
                })
                .child(
                    div().flex_shrink_0().child(
                        icon(glyph)
                            .with_size(KitSize::Size(px(14.)))
                            .text_color(palette.muted_foreground.hsla()),
                    ),
                )
                .child(
                    div()
                        .id(("suggest-label", ix))
                        .min_w_0()
                        .max_w(relative(0.45))
                        .flex_shrink_0()
                        .truncate()
                        .text_size(text::XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .child(label.clone())
                        .probe_text(label),
                )
                .child(
                    div()
                        .id(("suggest-description", ix))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(text::XS.size)
                        .text_color(palette.secondary_label.hsla())
                        .child(description)
                        .probe(),
                )
                .when_some(badge, |d, badge| {
                    d.child(
                        Badge::new(("suggest-badge", ix), badge)
                            .variant(badge::Variant::Secondary)
                            .size(badge::Size::Sm),
                    )
                })
                .probe()
        });
        div()
            .id("suggest-list")
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .max_h(px(max_height))
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .children(rows)
            .probe()
            .into_any_element()
    }

    fn empty_line(
        &self,
        trigger: &Trigger,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model = self.model.read(cx);
        let words: &str = match trigger {
            Trigger::Mention { .. } => match model.mentions() {
                Some(m) if m.loading => "Searching workspace files...",
                Some(m) if m.error.is_some() => "Could not search the files.",
                _ => "No matching files or folders.",
            },
            _ if model.commands_loading() => "Reading the commands...",
            _ => "No matching command.",
        };
        div()
            .id("suggest-empty")
            .px(px(20.))
            .py(px(14.))
            .text_size(text::XS.size)
            .text_color(palette.secondary_label.hsla())
            .child(words)
            .probe()
            .into_any_element()
    }

    /// What follows a command's name, as the hint t3code gives for a slash command's argument.
    fn argument_hint(&self, name: &str, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let spec = self.known_command(name, cx);
        let (usage, description) = spec.map(|c| (c.usage(), c.description)).unwrap_or_default();
        div()
            .id("suggest-arguments")
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT + 8.))
            .px(px(14.))
            .text_size(text::XS.size)
            .child(
                div()
                    .id("suggest-usage")
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .child(usage.clone())
                    .probe_text(usage),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.secondary_label.hsla())
                    .child(description),
            )
            .probe()
            .into_any_element()
    }
}

/// What a row shows: its icon, name, what it is, and a badge.
fn entry_parts(entry: &Entry) -> (IconName, String, String, Option<&'static str>) {
    match entry {
        Entry::Mention(item) => {
            let glyph = match item.kind {
                MentionKind::File => IconName::File,
                MentionKind::Folder => IconName::Folder,
                MentionKind::Directive => IconName::GitBranch,
                MentionKind::Prefix => IconName::Command,
            };
            (glyph, item.label.clone(), item.description.clone(), None)
        }
        Entry::Command(spec) => {
            let glyph = match spec.origin {
                CommandOrigin::Skill => IconName::Wrench,
                CommandOrigin::Report => IconName::ChartColumn,
                CommandOrigin::App => IconName::Command,
                CommandOrigin::Builtin => IconName::Terminal,
            };
            (
                glyph,
                spec.usage(),
                spec.description.clone(),
                spec.origin.badge(),
            )
        }
    }
}

/// The input's token for a chip.
pub fn token_of(chip: &Chip) -> InlineToken {
    InlineToken::new(chip.id(), chip.text.clone()).with_label(chip.label.clone())
}

/// The glyph of a chip.
pub fn chip_icon(kind: ChipKind) -> IconName {
    match kind {
        ChipKind::File => IconName::File,
        ChipKind::Folder => IconName::Folder,
        ChipKind::Other => IconName::GitBranch,
    }
}
