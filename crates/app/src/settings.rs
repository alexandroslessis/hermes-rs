//! Settings: the OpenAI-compatible provider, its API key, and the model picker.

use gpui::{App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, UniformListScrollHandle, Window, prelude::*, uniform_list};
use hermes_llm::discovery::DiscoveredModel;
use ui::{Callout, Divider, Tooltip, prelude::*};

use crate::config::{self, ProviderSettings};
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

pub struct SettingsView {
    name: Entity<TextArea>,
    base_url: Entity<TextArea>,
    api_key: Entity<TextArea>,
    search: Entity<TextArea>,
    free_only: bool,
    tools_only: bool,
    notice: Option<(Severity, String)>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let provider = state.read(cx).provider().cloned();
        let field = |placeholder: &'static str, text: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut area = TextArea::new(placeholder, cx).single_line();
                area.set_text(text, cx);
                area
            })
        };
        let name = field("Provider name", provider.as_ref().map(|p| p.name.clone()).unwrap_or_default(), cx);
        let base_url = field(
            "Base URL, e.g. https://openrouter.ai/api/v1",
            provider.as_ref().map(|p| p.base_url.clone()).unwrap_or_default(),
            cx,
        );
        let api_key = field("Paste an API key to store it in Windows Credential Manager", String::new(), cx);
        let search = field("Search models…", String::new(), cx);
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&search, window, |_, _, event, _, cx| {
                if let TextAreaEvent::Changed = event {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&api_key, window, |this: &mut Self, _, event, _, cx| {
                if let TextAreaEvent::Submit = event {
                    this.save_provider(cx);
                }
            }),
        ];
        Self {
            name,
            base_url,
            api_key,
            search,
            free_only: false,
            tools_only: true,
            notice: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn save_provider(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_string();
        let base_url = self.base_url.read(cx).text().trim().trim_end_matches('/').to_string();
        let key = self.api_key.read(cx).text().trim().to_string();
        if base_url.is_empty() {
            self.notice = Some((Severity::Error, "The base URL is required.".into()));
            cx.notify();
            return;
        }
        let state = AppState::global(cx);
        let result = state.update(cx, |state, cx| {
            let id = state.provider().map(|p| p.id.clone()).unwrap_or_else(|| "provider".into());
            let provider = ProviderSettings {
                id: id.clone(),
                name: if name.is_empty() { "Provider".into() } else { name },
                base_url,
                api_key_env: state.provider().and_then(|p| p.api_key_env.clone()),
            };
            match state.config.providers.iter_mut().find(|p| p.id == id) {
                Some(existing) => *existing = provider,
                None => state.config.providers.push(provider),
            }
            state.config.provider = Some(id.clone());
            let stored = if key.is_empty() { Ok(()) } else { config::store_api_key(&id, &key) };
            state.save(cx);
            state.refresh_models(cx);
            stored
        });
        self.notice = Some(match result {
            Ok(()) if key.is_empty() => (Severity::Success, "Provider saved.".into()),
            Ok(()) => (Severity::Success, "Provider saved and API key stored in Windows Credential Manager.".into()),
            Err(error) => (Severity::Error, format!("{error:#}")),
        });
        self.api_key.update(cx, |field, cx| field.set_text("", cx));
        cx.notify();
    }

    fn filtered_models(&self, cx: &App) -> Vec<DiscoveredModel> {
        let query = self.search.read(cx).text().to_lowercase();
        AppState::global(cx)
            .read(cx)
            .models
            .iter()
            .filter(|model| !self.free_only || model.id.ends_with(":free") || model.input_price == Some(0.) && model.output_price == Some(0.))
            .filter(|model| !self.tools_only || model.supports_tools != Some(false))
            .filter(|model| {
                query.is_empty()
                    || model.id.to_lowercase().contains(&query)
                    || model.name.as_deref().is_some_and(|name| name.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    fn select_model(&mut self, id: String, cx: &mut Context<Self>) {
        AppState::global(cx).update(cx, |state, cx| {
            state.config.model = Some(id);
            state.save(cx);
        });
        cx.notify();
    }

    fn field(label: &'static str, input: &Entity<TextArea>, cx: &App) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .gap_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.editor_background)
                    .child(input.clone()),
            )
    }
}

fn price(value: Option<f64>) -> String {
    match value {
        Some(0.0) => "free".into(),
        Some(price) => format!("${price:.2}"),
        None => "?".into(),
    }
}

fn context(value: Option<usize>) -> String {
    match value {
        Some(tokens) if tokens >= 1_000_000 => format!("{}M", tokens / 1_000_000),
        Some(tokens) => format!("{}K", tokens / 1000),
        None => "?".into(),
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let state = AppState::global(cx);
        let (has_key, key_source, selected, loading, error, total) = {
            let state = state.read(cx);
            let provider = state.provider();
            let stored = provider.and_then(|p| config::stored_api_key(&p.id)).is_some();
            let source = match (stored, provider.and_then(|p| p.api_key_env.clone())) {
                (true, _) => "stored in Windows Credential Manager".to_string(),
                (false, Some(var)) if state.has_api_key() => format!("read from the {var} environment variable"),
                _ => "not set".to_string(),
            };
            (
                state.has_api_key(),
                source,
                state.config.model.clone(),
                state.models_loading,
                state.models_error.clone(),
                state.models.len(),
            )
        };
        let models = self.filtered_models(cx);
        let count = models.len();
        let selected_for_list = selected.clone();

        v_flex()
            .id("settings")
            .size_full()
            .p_6()
            .gap_4()
            .bg(colors.panel_background)
            .overflow_y_scroll()
            .child(Headline::new("Settings").size(HeadlineSize::Large))
            .child(Label::new("Provider").size(LabelSize::Large))
            .child(Label::new("Any OpenAI-compatible endpoint: OpenRouter, OpenAI, LM Studio, Ollama, vLLM…").color(Color::Muted))
            .child(Self::field("Name", &self.name, cx))
            .child(Self::field("Base URL", &self.base_url, cx))
            .child(Self::field("API key", &self.api_key, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(if has_key { IconName::Check } else { IconName::Warning }).color(if has_key { Color::Success } else { Color::Warning }))
                    .child(Label::new(format!("API key {key_source}")).size(LabelSize::Small).color(Color::Muted)),
            )
            .child(
                h_flex().child(
                    Button::new("save-provider", "Save provider")
                        .style(ButtonStyle::Filled)
                        .on_click(cx.listener(|this, _, _, cx| this.save_provider(cx))),
                ),
            )
            .when_some(self.notice.clone(), |this, (severity, text)| {
                this.child(Callout::new().severity(severity).title(text))
            })
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .justify_between()
                    .child(Label::new("Model").size(LabelSize::Large))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("free-only", "Free only")
                                    .toggle_state(self.free_only)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.free_only = !this.free_only;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("tools-only", "Tool calling")
                                    .toggle_state(self.tools_only)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tools_only = !this.tools_only;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                IconButton::new("refresh-models", IconName::ArrowCircle)
                                    .tooltip(Tooltip::text("Reload the provider's model list"))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        AppState::global(cx).update(cx, |state, cx| state.refresh_models(cx));
                                    })),
                            ),
                    ),
            )
            .child(
                Label::new(match (&selected, loading) {
                    (_, true) => "Loading models…".to_string(),
                    (Some(model), _) => format!("New sessions use {model}. {count} of {total} models shown."),
                    (None, _) => format!("Pick the model new sessions use. {count} of {total} models shown."),
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .when_some(error, |this, error| this.child(Callout::new().severity(Severity::Error).title("Could not list models").description(error)))
            .child(Self::field("Search", &self.search, cx))
            .child(
                div()
                    .flex_none()
                    .h(px(360.))
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.editor_background)
                    .child(
                        uniform_list("models", count, cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            let colors = cx.theme().colors();
                            range
                                .map(|index| {
                                    let model = models[index].clone();
                                    let is_selected = selected_for_list.as_deref() == Some(model.id.as_str());
                                    let id = model.id.clone();
                                    h_flex()
                                        .id(SharedString::from(format!("model-{index}")))
                                        .w_full()
                                        .px_2()
                                        .h(px(28.))
                                        .gap_2()
                                        .cursor_pointer()
                                        .when(is_selected, |row| row.bg(colors.element_selected))
                                        .hover(|row| row.bg(colors.ghost_element_hover))
                                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().child(model.id.clone()))
                                        .child(Label::new(context(model.context_length)).size(LabelSize::XSmall).color(Color::Muted))
                                        .child(
                                            Label::new(format!("{} / {}", price(model.input_price), price(model.output_price)))
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                        .when(model.supports_reasoning == Some(true), |row| {
                                            row.child(Icon::new(IconName::ToolThink).size(IconSize::XSmall).color(Color::Muted))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| this.select_model(id.clone(), cx)))
                                })
                                .collect()
                        }))
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
            )
            .child(Label::new("Prices are USD per million input / output tokens.").size(LabelSize::XSmall).color(Color::Muted))
    }
}
