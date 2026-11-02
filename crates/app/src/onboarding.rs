//! The first-run wizard: pick a provider (a preset or any OpenAI-compatible address), give its
//! key, and pick a model. Shown when no provider can be called yet; Settings stays the place
//! for everything after.

use std::rc::Rc;

use gpui::{Context, Entity, EventEmitter, Focusable, Subscription, Window, prelude::*};
use ui::{Divider, prelude::*};

use crate::config::{self, ProviderSettings};
use crate::model_picker::ModelPicker;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

/// (name, base URL, needs a key, where to get one).
const PRESETS: &[(&str, &str, bool, &str)] = &[
    (
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        true,
        "openrouter.ai/keys — many models, some free",
    ),
    (
        "Ollama on this PC",
        "http://localhost:11434/v1",
        false,
        "ollama.com — run models locally, no key",
    ),
    (
        "LM Studio on this PC",
        "http://localhost:1234/v1",
        false,
        "lmstudio.ai — local server, no key",
    ),
    (
        "OpenAI",
        "https://api.openai.com/v1",
        true,
        "platform.openai.com/api-keys",
    ),
    (
        "Other OpenAI-compatible",
        "",
        true,
        "any /v1/chat/completions endpoint",
    ),
];

pub enum OnboardingEvent {
    /// A provider and model are set: start chatting.
    Done,
    /// Go to Settings instead.
    Skip,
}

enum Step {
    Provider,
    Connect,
    Model,
}

pub struct OnboardingView {
    step: Step,
    preset: usize,
    base_url: Entity<TextArea>,
    api_key: Entity<TextArea>,
    provider_id: Option<String>,
    picker: Option<Entity<ModelPicker>>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OnboardingEvent> for OnboardingView {}

impl OnboardingView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let base_url = cx.new(|cx| TextArea::new("https://…/v1", cx).single_line());
        let api_key = cx.new(|cx| TextArea::new("API key", cx).single_line());
        let submit = |this: &mut Self,
                      _: Entity<TextArea>,
                      event: &TextAreaEvent,
                      cx: &mut Context<Self>| {
            if matches!(event, TextAreaEvent::Submit) {
                this.connect(cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe(&base_url, submit),
            cx.subscribe(&api_key, submit),
            // The model list arrives after connecting.
            cx.observe(&AppState::global(cx), |_, _, cx| cx.notify()),
        ];
        Self {
            step: Step::Provider,
            preset: 0,
            base_url,
            api_key,
            provider_id: None,
            picker: None,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn choose(&mut self, preset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.preset = preset;
        let (_, url, needs_key, _) = PRESETS[preset];
        self.base_url.update(cx, |area, cx| area.set_text(url, cx));
        self.step = Step::Connect;
        self.error = None;
        let focus = if url.is_empty() || !needs_key {
            &self.base_url
        } else {
            &self.api_key
        };
        focus.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Saves the provider (key in Credential Manager) and lists its models.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let base_url = self
            .base_url
            .read(cx)
            .text()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let key = self.api_key.read(cx).text().trim().to_string();
        let (preset_name, _, needs_key, _) = PRESETS[self.preset];
        if base_url.is_empty() {
            self.error = Some("The address is required.".into());
            cx.notify();
            return;
        }
        let local = ProviderSettings {
            id: String::new(),
            name: String::new(),
            base_url: base_url.clone(),
            api_key_env: None,
        }
        .is_local();
        if needs_key && key.is_empty() && !local {
            self.error = Some("This provider needs an API key.".into());
            cx.notify();
            return;
        }
        let name = if preset_name.starts_with("Other") {
            crate::settings::name_or("", &base_url)
        } else {
            preset_name.trim_end_matches(" on this PC").to_string()
        };
        let result = AppState::global(cx).update(cx, |state, cx| {
            // The same address again (the default OpenRouter entry, a retry): that provider.
            let existing = state
                .config
                .providers
                .iter()
                .find(|p| {
                    p.base_url
                        .trim_end_matches('/')
                        .eq_ignore_ascii_case(&base_url)
                })
                .map(|p| p.id.clone());
            let id = match existing {
                Some(id) => id,
                None => {
                    let id = crate::settings::unique_id(&name, &state.config.providers);
                    state.config.providers.push(ProviderSettings {
                        id: id.clone(),
                        name,
                        base_url,
                        api_key_env: None,
                    });
                    id
                }
            };
            state.config.provider = Some(id.clone());
            let stored = if key.is_empty() {
                Ok(())
            } else {
                config::store_api_key(&id, &key)
            };
            state.save(cx);
            state.refresh_provider_models(&id, cx);
            stored.map(|()| id)
        });
        match result {
            Ok(id) => {
                self.provider_id = Some(id);
                self.step = Step::Model;
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Couldn't store the API key: {error:#}")),
        }
        cx.notify();
    }

    fn picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ModelPicker> {
        if let Some(picker) = &self.picker {
            return picker.clone();
        }
        let view = cx.entity().downgrade();
        let provider = self.provider_id.clone().unwrap_or_default();
        let picker = cx.new(|cx| {
            ModelPicker::new(
                None,
                Rc::new(move |provider, model, _, cx| {
                    AppState::global(cx).update(cx, |state, cx| {
                        state.config.provider = Some(provider);
                        state.config.model = Some(model);
                        state.save(cx);
                    });
                    view.update(cx, |_, cx| cx.emit(OnboardingEvent::Done)).ok();
                }),
                window,
                cx,
            )
            .only_provider(provider, cx)
        });
        self.picker = Some(picker.clone());
        picker
    }

    fn field(
        label: &'static str,
        input: &Entity<TextArea>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
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

impl Render for OnboardingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = matches!(self.step, Step::Model).then(|| self.picker(window, cx));
        let colors = cx.theme().colors();
        let body = match self.step {
            Step::Provider => v_flex()
                .gap_2()
                .child(Label::new("Where should Hermes get its model from?").color(Color::Muted))
                .children(
                    PRESETS
                        .iter()
                        .enumerate()
                        .map(|(index, (name, _, _, hint))| {
                            h_flex()
                                .id(("preset", index))
                                .p_2()
                                .gap_3()
                                .rounded_md()
                                .border_1()
                                .border_color(colors.border)
                                .cursor_pointer()
                                .hover(|style| style.bg(colors.ghost_element_hover))
                                .child(Icon::new(IconName::Server).color(Color::Muted))
                                .child(v_flex().child(Label::new(*name)).child(
                                    Label::new(*hint).size(LabelSize::Small).color(Color::Muted),
                                ))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose(index, window, cx)
                                }))
                        }),
                )
                .into_any_element(),
            Step::Connect => {
                let (name, _, needs_key, hint) = PRESETS[self.preset];
                v_flex()
                    .gap_3()
                    .child(Label::new(name))
                    .child(Label::new(hint).size(LabelSize::Small).color(Color::Muted))
                    .child(Self::field("Address", &self.base_url, cx))
                    .when(needs_key, |this| {
                        this.child(Self::field(
                            "API key (kept in Windows Credential Manager)",
                            &self.api_key,
                            cx,
                        ))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("back", "Back").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.step = Step::Provider;
                                    cx.notify();
                                },
                            )))
                            .child(
                                Button::new("connect", "Connect")
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                            ),
                    )
                    .into_any_element()
            }
            Step::Model => {
                let state = AppState::global(cx).read(cx);
                let list = self
                    .provider_id
                    .as_ref()
                    .and_then(|id| state.provider_models.get(id));
                let loading = list.is_none_or(|l| l.loading);
                let error = list.and_then(|l| l.error.clone());
                let empty = list.is_some_and(|l| !l.loading && l.models.is_empty());
                let mut column = v_flex().gap_2();
                if loading {
                    column = column
                        .child(Label::new("Connecting and listing models…").color(Color::Muted));
                } else if let Some(error) = error {
                    column = column
                        .child(
                            Label::new(format!("Couldn't list models: {error}"))
                                .color(Color::Error),
                        )
                        .child(
                            Button::new("retry", "Change Settings").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.step = Step::Connect;
                                    cx.notify();
                                },
                            )),
                        );
                } else if empty {
                    column = column.child(
                        Label::new("The provider answered but lists no models.")
                            .color(Color::Warning),
                    );
                } else {
                    column = column
                        .child(
                            Label::new("Pick the model to chat with (you can switch any time).")
                                .color(Color::Muted),
                        )
                        .children(picker.map(|picker| div().h(px(360.)).child(picker)));
                }
                column.into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors.background)
            .child(
                v_flex()
                    .w(px(520.))
                    .p_6()
                    .gap_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.panel_background)
                    .shadow_lg()
                    .child(Headline::new("Welcome to Hermes").size(HeadlineSize::Large))
                    .child(Divider::horizontal())
                    .child(body)
                    .when_some(self.error.clone(), |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    })
                    .child(
                        h_flex().justify_end().child(
                            Button::new("skip", "Set up in Settings instead")
                                .style(ButtonStyle::Subtle)
                                .label_size(LabelSize::Small)
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(OnboardingEvent::Skip)),
                                ),
                        ),
                    ),
            )
    }
}
