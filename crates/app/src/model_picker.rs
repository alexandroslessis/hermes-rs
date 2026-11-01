//! The model selector, after Zed's (`agent_ui/src/language_model_selector.rs` and
//! `ui/model_selector_components.rs`, GPL-3.0-or-later): a search field over every provider's
//! models, favorites first, then one group per provider; the current model is checked and any
//! model can be starred. The composer opens it as a popover; Settings embeds it.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, ListAlignment, ListSizingBehavior, ListState, SharedString, Subscription, Window,
    list, prelude::*,
};
use hermes_llm::discovery::DiscoveredModel;
use ui::{
    Chip, CommonAnimationExt, Divider, ElevationIndex, ListItem, ListItemSpacing, Tooltip,
    WithScrollbar, prelude::*,
};

use crate::config::{FavoriteModel, ProviderSettings};
use crate::state::{AppState, provider_ready};
use crate::text_area::{TextArea, TextAreaEvent};

/// Called with the provider id and the model id of the picked model.
pub type OnSelect = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
/// Opens the provider settings (the footer's Configure button).
pub type OnConfigure = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
struct ModelInfo {
    provider: String,
    icon: IconName,
    model: DiscoveredModel,
    is_favorite: bool,
}

enum Entry {
    Header(SharedString),
    Model(ModelInfo),
}

pub struct ModelPicker {
    search: Entity<TextArea>,
    /// Hide models the provider says can't call tools (the agent needs them).
    tools_only: bool,
    free_only: bool,
    /// (provider id, model id) of the current model.
    selected: Option<(String, String)>,
    entries: Vec<Entry>,
    /// The row the arrow keys are on.
    focused: usize,
    list: ListState,
    in_popover: bool,
    on_select: OnSelect,
    on_configure: Option<OnConfigure>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for ModelPicker {}

/// The icon Zed shows for a provider, guessed from its name and address.
pub fn provider_icon(provider: &ProviderSettings) -> IconName {
    let haystack = format!("{} {}", provider.name, provider.base_url).to_lowercase();
    if haystack.contains("openrouter") {
        IconName::AiOpenRouter
    } else if haystack.contains("ollama") || haystack.contains(":11434") {
        IconName::AiOllama
    } else if haystack.contains("lm studio") || haystack.contains(":1234") {
        IconName::AiLmStudio
    } else if haystack.contains("api.openai.com") {
        IconName::AiOpenAi
    } else {
        IconName::AiOpenAiCompat
    }
}

/// Zed's cost chip: dollars per million tokens in / out, or "Free".
fn cost_info(model: &DiscoveredModel) -> Option<SharedString> {
    let trim = |price: f64| {
        let text = format!("{price:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    match (model.input_price, model.output_price) {
        (Some(input), Some(output)) if input == 0. && output == 0. => Some("Free".into()),
        (Some(input), Some(output)) => Some(format!("${}/${}", trim(input), trim(output)).into()),
        _ => None,
    }
}

/// Whether `query`'s characters appear in `text` in order (Zed's fuzzy match, without scores).
fn fuzzy_matches(text: &str, query: &str) -> bool {
    let mut chars = text.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|q| chars.any(|c| c == q))
}

impl ModelPicker {
    pub fn new(
        selected: Option<(String, String)>,
        on_select: OnSelect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextArea::new("Select a model…", cx).single_line());
        let state = AppState::global(cx);
        let subscriptions = vec![
            cx.observe(&state, |this, _, cx| this.update_entries(cx)),
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self, _, event, window, cx| match event {
                    TextAreaEvent::Changed => this.update_entries(cx),
                    TextAreaEvent::Submit => this.confirm(window, cx),
                },
            ),
        ];
        let mut this = Self {
            search,
            tools_only: true,
            free_only: false,
            selected,
            entries: Vec::new(),
            focused: 0,
            list: ListState::new(0, ListAlignment::Top, px(1000.)),
            in_popover: false,
            on_select,
            on_configure: None,
            _subscriptions: subscriptions,
        };
        this.update_entries(cx);
        this.focus_selected();
        this
    }

    /// Renders as Zed's popover: elevated, with a Configure footer.
    pub fn in_popover(mut self, on_configure: OnConfigure) -> Self {
        self.in_popover = true;
        self.on_configure = Some(on_configure);
        self
    }

    pub fn set_selected(&mut self, selected: Option<(String, String)>, cx: &mut Context<Self>) {
        self.selected = selected;
        cx.notify();
    }

    fn update_entries(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text().to_lowercase();
        let state = AppState::global(cx).read(cx);
        let favorites = &state.config.favorite_models;
        let matches = |provider: &ProviderSettings, model: &DiscoveredModel| {
            if self.tools_only && model.supports_tools == Some(false) {
                return false;
            }
            if self.free_only && cost_info(model).as_deref() != Some("Free") {
                return false;
            }
            if query.is_empty() {
                return true;
            }
            let name = model.name.as_deref().unwrap_or(&model.id).to_lowercase();
            let haystack = format!("{}/{name} {}", provider.name.to_lowercase(), model.id);
            haystack.contains(&query) || fuzzy_matches(&haystack, &query)
        };
        let mut groups: Vec<(SharedString, Vec<ModelInfo>)> = Vec::new();
        for provider in state.config.providers.iter().filter(|p| provider_ready(p)) {
            let Some(list) = state.provider_models.get(&provider.id) else {
                continue;
            };
            let icon = provider_icon(provider);
            let models: Vec<ModelInfo> = list
                .models
                .iter()
                .filter(|model| matches(provider, model))
                .map(|model| ModelInfo {
                    provider: provider.id.clone(),
                    icon,
                    is_favorite: favorites
                        .iter()
                        .any(|f| f.provider == provider.id && f.model == model.id),
                    model: model.clone(),
                })
                .collect();
            if !models.is_empty() {
                groups.push((provider.name.clone().into(), models));
            }
        }
        let mut entries = Vec::new();
        let favorite: Vec<ModelInfo> = groups
            .iter()
            .flat_map(|(_, models)| models.iter().filter(|m| m.is_favorite).cloned())
            .collect();
        if !favorite.is_empty() {
            entries.push(Entry::Header("Favorite".into()));
            entries.extend(favorite.into_iter().map(Entry::Model));
        }
        for (name, models) in groups {
            entries.push(Entry::Header(name));
            entries.extend(models.into_iter().map(Entry::Model));
        }
        self.entries = entries;
        self.list.reset(self.entries.len());
        self.focused = self.first_model_from(0).unwrap_or(0);
        if query.is_empty() {
            self.focus_selected();
        }
        cx.notify();
    }

    /// Puts the arrow-key focus on the current model (Zed opens the selector there).
    fn focus_selected(&mut self) {
        let Some((provider, model)) = &self.selected else {
            return;
        };
        if let Some(index) = self.entries.iter().position(|entry| {
            matches!(entry, Entry::Model(info) if &info.provider == provider && &info.model.id == model)
        }) {
            self.focused = index;
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn first_model_from(&self, start: usize) -> Option<usize> {
        (start..self.entries.len()).find(|&i| matches!(self.entries[i], Entry::Model(_)))
    }

    fn move_focus(&mut self, down: bool, cx: &mut Context<Self>) {
        let models: Vec<usize> = (0..self.entries.len())
            .filter(|&i| matches!(self.entries[i], Entry::Model(_)))
            .collect();
        if models.is_empty() {
            return;
        }
        let position = models.iter().position(|&i| i == self.focused).unwrap_or(0);
        let next = if down {
            (position + 1) % models.len()
        } else {
            (position + models.len() - 1) % models.len()
        };
        self.focused = models[next];
        self.list.scroll_to_reveal_item(self.focused);
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Entry::Model(info)) = self.entries.get(self.focused) {
            let (provider, model) = (info.provider.clone(), info.model.id.clone());
            self.pick(provider, model, window, cx);
        }
    }

    fn pick(
        &mut self,
        provider: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some((provider.clone(), model.clone()));
        (self.on_select)(provider, model, window, cx);
        cx.emit(DismissEvent);
        cx.notify();
    }

    fn toggle_favorite(&mut self, provider: String, model: String, cx: &mut Context<Self>) {
        AppState::global(cx).update(cx, |state, cx| {
            let favorite = FavoriteModel { provider, model };
            let favorites = &mut state.config.favorite_models;
            match favorites.iter().position(|f| *f == favorite) {
                Some(index) => {
                    favorites.remove(index);
                }
                None => favorites.push(favorite),
            }
            state.save(cx);
        });
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

    fn render_entry(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.entries[index] {
            // Zed's `ModelSelectorHeader`.
            Entry::Header(title) => div()
                .px_2()
                .pb_1()
                .when(index > 1, |this| {
                    this.mt_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().colors().border_variant)
                })
                .child(
                    Label::new(title.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .into_any_element(),
            // Zed's `ModelSelectorListItem`.
            Entry::Model(info) => {
                let is_selected = self.selected.as_ref().is_some_and(|(provider, model)| {
                    provider == &info.provider && model == &info.model.id
                });
                let is_favorite = info.is_favorite;
                let title = info
                    .model
                    .name
                    .clone()
                    .unwrap_or_else(|| info.model.id.clone());
                let cost = cost_info(&info.model);
                let (provider, model) = (info.provider.clone(), info.model.id.clone());
                let (fav_provider, fav_model) = (provider.clone(), model.clone());
                let (star, star_color, star_tooltip) = if is_favorite {
                    (IconName::StarFilled, Color::Accent, "Unfavorite Model")
                } else {
                    (IconName::Star, Color::Default, "Favorite Model")
                };
                ListItem::new(index)
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(index == self.focused)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1p5()
                            .child(
                                Icon::new(info.icon)
                                    .color(if is_selected {
                                        Color::Accent
                                    } else {
                                        Color::Muted
                                    })
                                    .size(IconSize::Small),
                            )
                            .child(Label::new(title).truncate())
                            .when_some(cost, |this, cost| {
                                let tooltip = if cost.contains('$') {
                                    format!("Cost per Million Tokens: {cost}")
                                } else {
                                    format!("Cost: {cost}")
                                };
                                this.child(Chip::new(cost).tooltip(Tooltip::text(tooltip)))
                            }),
                    )
                    .end_slot(h_flex().pr_2().gap_1p5().when(is_selected, |this| {
                        this.child(Icon::new(IconName::Check).color(Color::Accent))
                    }))
                    .end_slot_on_hover(
                        div().pr_1p5().child(
                            IconButton::new(("toggle-favorite", index), star)
                                .layer(ElevationIndex::ElevatedSurface)
                                .icon_color(star_color)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text(star_tooltip))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_favorite(
                                        fav_provider.clone(),
                                        fav_model.clone(),
                                        cx,
                                    )
                                })),
                        ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(provider.clone(), model.clone(), window, cx)
                    }))
                    .into_any_element()
            }
        }
    }
}

impl Focusable for ModelPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl Render for ModelPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (loading, empty_label) = {
            let state = AppState::global(cx).read(cx);
            let loading = state.models_loading();
            let label = if loading && self.entries.is_empty() {
                "Loading models…"
            } else {
                "No matches"
            };
            (loading, label)
        };
        let filter_toggle = |id: &'static str, icon: IconName, on: bool, tooltip: &'static str| {
            IconButton::new(id, icon)
                .icon_size(IconSize::Small)
                .icon_color(if on { Color::Accent } else { Color::Muted })
                .toggle_state(on)
                .tooltip(Tooltip::text(tooltip))
        };
        let list_body = if self.entries.is_empty() {
            div()
                .px_2p5()
                .py_2()
                .child(Label::new(empty_label).color(Color::Muted))
                .into_any_element()
        } else {
            let list_state = self.list.clone();
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    list(
                        list_state.clone(),
                        cx.processor(|this, index, window, cx| {
                            this.render_entry(index, window, cx)
                        }),
                    )
                    .with_sizing_behavior(if self.in_popover {
                        ListSizingBehavior::Infer
                    } else {
                        ListSizingBehavior::Auto
                    })
                    .flex_1()
                    .py(DynamicSpacing::Base04.rems(cx)),
                )
                .vertical_scrollbar_for(&list_state, window, cx)
                .into_any_element()
        };
        v_flex()
            .key_context("ModelPicker")
            .capture_key_down(cx.listener(Self::on_key_down))
            .map(|this| {
                if self.in_popover {
                    this.w(rems(20.)).max_h(rems(24.)).elevation_3(cx)
                } else {
                    this.size_full()
                }
            })
            .overflow_hidden()
            // Zed's picker head: the query over a divider, filters at its trailing edge.
            .child(
                h_flex()
                    .h_9()
                    .px_2p5()
                    .flex_none()
                    .gap_1()
                    .overflow_hidden()
                    .child(div().flex_1().child(self.search.clone()))
                    .when(loading, |this| {
                        this.child(
                            Icon::new(IconName::LoadCircle)
                                .size(IconSize::Small)
                                .color(Color::Muted)
                                .with_rotate_animation(2),
                        )
                    })
                    .child(
                        Button::new("free-only", "Free")
                            .label_size(LabelSize::Small)
                            .color(if self.free_only {
                                Color::Accent
                            } else {
                                Color::Muted
                            })
                            .toggle_state(self.free_only)
                            .tooltip(Tooltip::text("Only Free Models"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.free_only = !this.free_only;
                                this.update_entries(cx);
                            })),
                    )
                    .child(
                        filter_toggle(
                            "tools-only",
                            IconName::ToolHammer,
                            self.tools_only,
                            "Only Models That Can Call Tools",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tools_only = !this.tools_only;
                            this.update_entries(cx);
                        })),
                    ),
            )
            .child(Divider::horizontal())
            .child(list_body)
            // Zed's `ModelSelectorFooter`.
            .when_some(self.on_configure.clone(), |this, on_configure| {
                this.child(
                    h_flex()
                        .w_full()
                        .p_1p5()
                        .border_t_1()
                        .border_color(cx.theme().colors().border_variant)
                        .child(
                            Button::new("configure", "Configure")
                                .full_width()
                                .style(ButtonStyle::Outlined)
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    cx.emit(DismissEvent);
                                    on_configure(window, cx);
                                })),
                        ),
                )
            })
    }
}
