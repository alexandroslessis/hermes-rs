//! Settings: the OpenAI-compatible provider, its API key, and the default model.

use std::rc::Rc;

use gpui::{App, Context, Entity, FocusHandle, Focusable, Subscription, Window, prelude::*};
use ui::{Callout, ContextMenu, Divider, DropdownMenu, Tooltip, prelude::*};

use crate::config::{self, ProviderSettings};
use crate::model_picker::ModelPicker;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

pub struct SettingsView {
    name: Entity<TextArea>,
    base_url: Entity<TextArea>,
    api_key: Entity<TextArea>,
    picker: Entity<ModelPicker>,
    notice: Option<(Severity, String)>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let (provider, model) = {
            let state = state.read(cx);
            (state.provider().cloned(), state.config.model.clone())
        };
        let field = |placeholder: &'static str, text: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut area = TextArea::new(placeholder, cx).single_line();
                area.set_text(text, cx);
                area
            })
        };
        let name = field(
            "Provider name",
            provider
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            cx,
        );
        let base_url = field(
            "Base URL, e.g. https://openrouter.ai/api/v1",
            provider
                .as_ref()
                .map(|p| p.base_url.clone())
                .unwrap_or_default(),
            cx,
        );
        let api_key = field(
            "Paste an API key to store it in Windows Credential Manager",
            String::new(),
            cx,
        );
        let picker = cx.new(|cx| {
            ModelPicker::new(
                model,
                px(360.),
                Rc::new(|id, _, cx| {
                    AppState::global(cx).update(cx, |state, cx| {
                        state.config.model = Some(id);
                        state.save(cx);
                    });
                }),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&state, |this, state, cx| {
                let model = state.read(cx).config.model.clone();
                this.picker
                    .update(cx, |picker, cx| picker.set_selected(model, cx));
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
            picker,
            notice: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn save_provider(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_string();
        let base_url = self
            .base_url
            .read(cx)
            .text()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let key = self.api_key.read(cx).text().trim().to_string();
        if base_url.is_empty() {
            self.notice = Some((Severity::Error, "The base URL is required.".into()));
            cx.notify();
            return;
        }
        let state = AppState::global(cx);
        let result = state.update(cx, |state, cx| {
            let id = state
                .provider()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| "provider".into());
            let provider = ProviderSettings {
                id: id.clone(),
                name: if name.is_empty() {
                    "Provider".into()
                } else {
                    name
                },
                base_url,
                api_key_env: state.provider().and_then(|p| p.api_key_env.clone()),
            };
            match state.config.providers.iter_mut().find(|p| p.id == id) {
                Some(existing) => *existing = provider,
                None => state.config.providers.push(provider),
            }
            state.config.provider = Some(id.clone());
            let stored = if key.is_empty() {
                Ok(())
            } else {
                config::store_api_key(&id, &key)
            };
            state.save(cx);
            state.refresh_models(cx);
            stored
        });
        self.notice = Some(match result {
            Ok(()) if key.is_empty() => (Severity::Success, "Provider saved.".into()),
            Ok(()) => (
                Severity::Success,
                "Provider saved and API key stored in Windows Credential Manager.".into(),
            ),
            Err(error) => (Severity::Error, format!("{error:#}")),
        });
        self.api_key.update(cx, |field, cx| field.set_text("", cx));
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

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    /// Zed's bundled themes, light and dark, in a dropdown.
    fn theme_picker(window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = cx.theme().name.clone();
        let mut themes = theme::ThemeRegistry::global(cx).list();
        themes.sort_by(|a, b| {
            a.appearance
                .is_light()
                .cmp(&b.appearance.is_light())
                .then(a.name.cmp(&b.name))
        });
        let selected = current.clone();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            let mut light = None;
            for meta in themes {
                if light != Some(meta.appearance.is_light()) {
                    light = Some(meta.appearance.is_light());
                    menu = menu.header(if meta.appearance.is_light() {
                        "Light"
                    } else {
                        "Dark"
                    });
                }
                let name = meta.name.clone();
                menu = menu.toggleable_entry(
                    meta.name.clone(),
                    meta.name == selected,
                    IconPosition::Start,
                    None,
                    move |_, cx| {
                        AppState::global(cx).update(cx, |state, cx| state.set_theme(&name, cx));
                    },
                );
            }
            menu
        });
        h_flex()
            .gap_2()
            .child(Label::new("Theme").color(Color::Muted))
            .child(DropdownMenu::new("theme-picker", current, menu))
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let state = AppState::global(cx);
        let (has_key, key_source, selected, error) = {
            let state = state.read(cx);
            let provider = state.provider();
            let stored = provider
                .and_then(|p| config::stored_api_key(&p.id))
                .is_some();
            let source = match (stored, provider.and_then(|p| p.api_key_env.clone())) {
                (true, _) => "stored in Windows Credential Manager".to_string(),
                _ if provider.is_some_and(|p| p.is_local()) => {
                    "not needed (a server on this machine)".to_string()
                }
                (false, Some(var)) if state.has_api_key() => {
                    format!("read from the {var} environment variable")
                }
                _ => "not set".to_string(),
            };
            (
                state.has_api_key(),
                source,
                state.config.model.clone(),
                state.models_error.clone(),
            )
        };

        v_flex()
            .id("settings")
            .size_full()
            .p_6()
            .gap_4()
            .bg(colors.panel_background)
            .overflow_y_scroll()
            .child(Headline::new("Settings").size(HeadlineSize::Large))
            .child(Label::new("Appearance").size(LabelSize::Large))
            .child(Self::theme_picker(window, cx))
            .child(Divider::horizontal())
            .child(Label::new("Provider").size(LabelSize::Large))
            .child(Label::new("Any OpenAI-compatible endpoint: OpenRouter, OpenAI, LM Studio, Ollama, vLLM…").color(Color::Muted))
            .child(Self::field("Name", &self.name, cx))
            .child(Self::field("Base URL", &self.base_url, cx))
            .child(Self::field("API key", &self.api_key, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(if has_key { IconName::Check } else { IconName::Warning })
                            .color(if has_key { Color::Success } else { Color::Warning }),
                    )
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
                    .child(Label::new("Default model").size(LabelSize::Large))
                    .child(
                        IconButton::new("refresh-models", IconName::ArrowCircle)
                            .tooltip(Tooltip::text("Reload the provider's model list"))
                            .on_click(cx.listener(|_, _, _, cx| {
                                AppState::global(cx).update(cx, |state, cx| state.refresh_models(cx));
                            })),
                    ),
            )
            .child(
                Label::new(match &selected {
                    Some(model) => format!("New sessions use {model}. Each chat can switch from the model button under its message box."),
                    None => "Pick the model new sessions use.".to_string(),
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .when_some(error, |this, error| {
                this.child(Callout::new().severity(Severity::Error).title("Could not list models").description(error))
            })
            .child(self.picker.clone())
    }
}
