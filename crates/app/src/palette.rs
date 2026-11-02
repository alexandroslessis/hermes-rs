//! Zed's command palette (`command_palette/src/command_palette.rs`, GPL-3.0-or-later) over a
//! picker-style list: Ctrl+Shift+P lists the window's actions and Hermes's slash commands,
//! Ctrl+P jumps to a session.

use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    ListAlignment, ListSizingBehavior, ListState, SharedString, Subscription, Window, list,
    prelude::*,
};
use ui::{Divider, KeyBinding, ListItem, ListItemSpacing, WithScrollbar, prelude::*};

use crate::model_picker::fuzzy_matches;
use crate::text_area::{TextArea, TextAreaEvent};

pub enum PaletteItem {
    /// A GPUI action, dispatched on whatever had focus before the palette opened.
    Action {
        label: SharedString,
        action: Box<dyn Action>,
    },
    /// A chat slash command; `run` sends it at once, else it goes to the composer for arguments.
    Command {
        name: &'static str,
        description: &'static str,
        run: bool,
    },
    Session {
        id: String,
        title: SharedString,
        detail: SharedString,
    },
}

impl PaletteItem {
    fn haystack(&self) -> String {
        match self {
            Self::Action { label, .. } => label.to_lowercase(),
            Self::Command {
                name, description, ..
            } => format!("/{name} {}", description.to_lowercase()),
            Self::Session { title, detail, .. } => {
                format!("{} {}", title.to_lowercase(), detail.to_lowercase())
            }
        }
    }
}

pub struct PalettePicked(pub PaletteItem);

/// `workspace::NewSession` → "workspace: new session" (Zed's `humanize_action_name`).
pub fn humanize_action_name(name: &str) -> String {
    let (namespace, action) = name.rsplit_once("::").unwrap_or(("", name));
    let mut words = String::new();
    for (index, c) in action.chars().enumerate() {
        if c.is_uppercase() && index > 0 {
            words.push(' ');
        }
        words.extend(c.to_lowercase());
    }
    if namespace.is_empty() {
        words
    } else {
        format!("{}: {words}", namespace.replace('_', " "))
    }
}

pub struct Palette {
    search: Entity<TextArea>,
    items: Vec<PaletteItem>,
    /// Indices into `items` that match the query, best first.
    matches: Vec<usize>,
    focused: usize,
    list: ListState,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for Palette {}
impl EventEmitter<PalettePicked> for Palette {}

impl Palette {
    pub fn new(
        placeholder: &str,
        items: Vec<PaletteItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextArea::new(placeholder, cx).single_line());
        let subscription = cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, event, window, cx| match event {
                TextAreaEvent::Changed => this.update_matches(cx),
                TextAreaEvent::Submit => this.confirm(window, cx),
                _ => {}
            },
        );
        let mut this = Self {
            search,
            items,
            matches: Vec::new(),
            focused: 0,
            list: ListState::new(0, ListAlignment::Top, px(1000.)),
            _subscription: subscription,
        };
        this.update_matches(cx);
        this
    }

    fn update_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text().trim().to_lowercase();
        let query = query.strip_prefix('/').unwrap_or(&query).to_string();
        // Substring matches before scattered ones; each keeps the list's order.
        let (mut exact, mut fuzzy) = (Vec::new(), Vec::new());
        for (index, item) in self.items.iter().enumerate() {
            let haystack = item.haystack();
            if query.is_empty() || haystack.contains(&query) {
                exact.push(index);
            } else if fuzzy_matches(&haystack, &query) {
                fuzzy.push(index);
            }
        }
        exact.extend(fuzzy);
        self.matches = exact;
        self.focused = 0;
        self.list.reset(self.matches.len());
        cx.notify();
    }

    fn move_focus(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.focused = if down {
            (self.focused + 1) % count
        } else {
            (self.focused + count - 1) % count
        };
        self.list.scroll_to_reveal_item(self.focused);
        cx.notify();
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.pick(self.focused, cx);
    }

    fn pick(&mut self, position: usize, cx: &mut Context<Self>) {
        let Some(&index) = self.matches.get(position) else {
            return;
        };
        let item = match &self.items[index] {
            PaletteItem::Action { label, action } => PaletteItem::Action {
                label: label.clone(),
                action: action.boxed_clone(),
            },
            PaletteItem::Command {
                name,
                description,
                run,
            } => PaletteItem::Command {
                name,
                description,
                run: *run,
            },
            PaletteItem::Session { id, title, detail } => PaletteItem::Session {
                id: id.clone(),
                title: title.clone(),
                detail: detail.clone(),
            },
        };
        cx.emit(PalettePicked(item));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => self.move_focus(false, cx),
            "down" => self.move_focus(true, cx),
            "escape" => cx.emit(DismissEvent),
            "enter" => self.confirm(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_item(&mut self, position: usize, cx: &mut Context<Self>) -> AnyElement {
        let item = &self.items[self.matches[position]];
        let row = ListItem::new(position)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(position == self.focused)
            .on_click(cx.listener(move |this, _, _, cx| this.pick(position, cx)));
        match item {
            PaletteItem::Action { label, action } => row
                .child(Label::new(label.clone()))
                .end_slot(div().pr_2().child(KeyBinding::for_action(action.as_ref(), cx))),
            PaletteItem::Command {
                name, description, ..
            } => row.child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(Label::new(format!("/{name}")))
                    .child(
                        Label::new(*description)
                            .color(Color::Muted)
                            .size(LabelSize::Small)
                            .truncate(),
                    ),
            ),
            PaletteItem::Session { title, detail, .. } => row.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(Label::new(title.clone()).truncate())
                    .child(
                        Label::new(detail.clone())
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    ),
            ),
        }
        .into_any_element()
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.matches.is_empty() {
            div()
                .px_2p5()
                .py_2()
                .child(Label::new("No matches").color(Color::Muted))
                .into_any_element()
        } else {
            let state = self.list.clone();
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    list(
                        state.clone(),
                        cx.processor(|this, index, _, cx| this.render_item(index, cx)),
                    )
                    .with_sizing_behavior(ListSizingBehavior::Infer)
                    .flex_1()
                    .py(DynamicSpacing::Base04.rems(cx)),
                )
                .vertical_scrollbar_for(&state, window, cx)
                .into_any_element()
        };
        v_flex()
            .key_context("Palette")
            .capture_key_down(cx.listener(Self::on_key_down))
            .w(rems(34.))
            .max_h(rems(26.))
            .elevation_3(cx)
            .overflow_hidden()
            // Clicks inside don't reach the backdrop that closes it.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .h_9()
                    .px_2p5()
                    .flex_none()
                    .child(div().flex_1().child(self.search.clone())),
            )
            .child(Divider::horizontal())
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_read_like_zed() {
        assert_eq!(
            humanize_action_name("workspace::NewSession"),
            "workspace: new session"
        );
        assert_eq!(humanize_action_name("chat::Find"), "chat: find");
        assert_eq!(
            humanize_action_name("text_area::SelectAll"),
            "text area: select all"
        );
    }
}
