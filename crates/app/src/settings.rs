//! Settings: appearance, the OpenAI-compatible providers (a list, each edited in a dialog), and
//! the default model.

use std::rc::Rc;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyDownEvent, Subscription, Window, prelude::*,
};
use ui::{
    Callout, Chip, ContextMenu, Divider, DropdownMenu, PopoverMenu, Switch, TintColor, ToggleState, Tooltip,
    prelude::*,
};

use crate::backend::backend;
use crate::config::{self, ProviderSettings};
use crate::model_picker::{ModelPicker, provider_icon};
use crate::state::{AppState, provider_ready};
use crate::text_area::{TextArea, TextAreaEvent};

/// The MCP editor's placeholder: one local and one remote server.
const MCP_EXAMPLE: &str = r#"{"time": {"command": "uvx", "args": ["mcp-server-time"]}, "remote": {"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer ..."}}}"#;

/// The add/edit provider dialog.
struct ProviderForm {
    /// The provider being edited; `None` adds a new one.
    editing: Option<String>,
    name: Entity<TextArea>,
    base_url: Entity<TextArea>,
    api_key: Entity<TextArea>,
    api_key_env: Entity<TextArea>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

pub struct SettingsView {
    picker: Entity<ModelPicker>,
    form: Option<ProviderForm>,
    scroll: gpui::ScrollHandle,
    /// The MCP servers JSON being edited, and why the last save was refused.
    mcp_editor: Option<Entity<TextArea>>,
    mcp_error: Option<String>,
    /// Re-renders while a server is still connecting.
    mcp_poll: Option<gpui::Task<()>>,
    /// What the last Import from Hermes did, and whether anything failed.
    import_report: Option<(String, bool)>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let selected = Self::selected_model(cx);
        let picker = cx.new(|cx| {
            ModelPicker::new(
                selected,
                Rc::new(|provider, model, _, cx| {
                    AppState::global(cx).update(cx, |state, cx| {
                        state.config.provider = Some(provider);
                        state.config.model = Some(model);
                        state.save(cx);
                    });
                }),
                window,
                cx,
            )
        });
        let subscriptions = vec![cx.observe(&state, |this, _, cx| {
            let selected = Self::selected_model(cx);
            this.picker
                .update(cx, |picker, cx| picker.set_selected(selected, cx));
        })];
        Self {
            picker,
            form: None,
            scroll: gpui::ScrollHandle::new(),
            mcp_editor: None,
            mcp_error: None,
            mcp_poll: None,
            import_report: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// (provider id, model id) of the default model.
    fn selected_model(cx: &App) -> Option<(String, String)> {
        let state = AppState::global(cx).read(cx);
        let model = state.config.model.clone()?;
        let provider = state.provider_for_model(&model)?.id.clone();
        Some((provider, model))
    }

    /// Opens the provider dialog: `editing` names the provider to edit, `None` adds one.
    fn open_form(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = editing.as_ref().and_then(|id| {
            AppState::global(cx)
                .read(cx)
                .config
                .providers
                .iter()
                .find(|p| &p.id == id)
                .cloned()
        });
        let field = |placeholder: &'static str, text: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut area = TextArea::new(placeholder, cx).single_line();
                area.set_text(text, cx);
                area
            })
        };
        let name = field(
            "e.g. OpenRouter, Ollama",
            existing
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            cx,
        );
        let base_url = field(
            "e.g. https://openrouter.ai/api/v1 or http://localhost:11434/v1",
            existing
                .as_ref()
                .map(|p| p.base_url.clone())
                .unwrap_or_default(),
            cx,
        );
        let api_key = field(
            if existing.is_some() {
                "Leave empty to keep the current key"
            } else {
                "Stored in Windows Credential Manager"
            },
            String::new(),
            cx,
        );
        let api_key_env = field(
            "Optional: read the key from this environment variable",
            existing
                .as_ref()
                .and_then(|p| p.api_key_env.clone())
                .unwrap_or_default(),
            cx,
        );
        let mut subscriptions = Vec::new();
        for input in [&name, &base_url, &api_key, &api_key_env] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this: &mut Self, _, event, _, cx| {
                    if let TextAreaEvent::Submit = event {
                        this.save_form(cx);
                    }
                },
            ));
        }
        name.focus_handle(cx).focus(window, cx);
        self.form = Some(ProviderForm {
            editing,
            name,
            base_url,
            api_key,
            api_key_env,
            error: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let text = |input: &Entity<TextArea>| input.read(cx).text().trim().to_string();
        let name = text(&form.name);
        let base_url = text(&form.base_url).trim_end_matches('/').to_string();
        let key = text(&form.api_key);
        let key_env = text(&form.api_key_env);
        if base_url.is_empty() {
            form.error = Some("The base URL is required.".into());
            cx.notify();
            return;
        }
        let editing = form.editing.clone();
        let result = AppState::global(cx).update(cx, |state, cx| {
            let providers = &mut state.config.providers;
            let id = editing
                .clone()
                .unwrap_or_else(|| unique_id(&name_or(&name, &base_url), providers));
            let provider = ProviderSettings {
                id: id.clone(),
                name: name_or(&name, &base_url),
                base_url,
                api_key_env: (!key_env.is_empty()).then_some(key_env),
            };
            match providers.iter_mut().find(|p| p.id == id) {
                Some(existing) => *existing = provider,
                None => providers.push(provider),
            }
            if state.config.provider.is_none() {
                state.config.provider = Some(id.clone());
            }
            let stored = if key.is_empty() {
                Ok(())
            } else {
                config::store_api_key(&id, &key)
            };
            state.save(cx);
            state.refresh_provider_models(&id, cx);
            stored
        });
        match result {
            Ok(()) => self.close_form(cx),
            Err(error) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(format!("Couldn't store the API key: {error:#}"));
                }
                cx.notify();
            }
        }
    }

    fn remove_provider(&mut self, id: String, cx: &mut Context<Self>) {
        AppState::global(cx).update(cx, |state, cx| {
            state.config.providers.retain(|p| p.id != id);
            state
                .config
                .favorite_models
                .retain(|favorite| favorite.provider != id);
            if state.config.provider.as_deref() == Some(id.as_str()) {
                state.config.provider = state.config.providers.first().map(|p| p.id.clone());
            }
            state.provider_models.remove(&id);
            // The key goes with it.
            let _ = config::store_api_key(&id, "");
            state.save(cx);
        });
        cx.notify();
    }

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
        DropdownMenu::new("theme-picker", current, menu)
    }

    /// This build's version, the daily check switch, and the newest release.
    fn render_updates(cx: &mut Context<Self>) -> impl IntoElement {
        use crate::update::{CURRENT_VERSION, UpdateStatus};
        let (status, auto) = {
            let state = AppState::global(cx).read(cx);
            (state.update.clone(), state.config.check_updates)
        };
        let (line, color): (String, Color) = match &status {
            UpdateStatus::Unknown => (String::new(), Color::Muted),
            UpdateStatus::Checking => ("Checking…".into(), Color::Muted),
            UpdateStatus::UpToDate => ("You have the latest version.".into(), Color::Muted),
            UpdateStatus::Available(release) => (
                format!("Version {} is available.", release.version),
                Color::Accent,
            ),
            UpdateStatus::Downloading(release) => (
                format!("Downloading {}…", release.version),
                Color::Muted,
            ),
            UpdateStatus::Failed(error) => (format!("Couldn't check: {error}"), Color::Error),
        };
        let action = match &status {
            UpdateStatus::Available(release) if release.installer.is_some() => {
                Button::new("install-update", "Download and Install")
                    .style(ButtonStyle::Filled)
                    .tooltip(Tooltip::text("Hermes closes while the installer runs"))
                    .on_click(|_, _, cx| {
                        AppState::global(cx).update(cx, |state, cx| state.install_update(cx))
                    })
                    .into_any_element()
            }
            UpdateStatus::Available(release) => {
                let page = release.page.clone();
                Button::new("open-release", "Open Release Page")
                    .on_click(move |_, _, cx| cx.open_url(&page))
                    .into_any_element()
            }
            _ => Button::new("check-updates", "Check Now")
                .style(ButtonStyle::Outlined)
                .disabled(matches!(
                    status,
                    UpdateStatus::Checking | UpdateStatus::Downloading(_)
                ))
                .on_click(|_, _, cx| {
                    AppState::global(cx).update(cx, |state, cx| state.check_for_updates(true, cx))
                })
                .into_any_element(),
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new("Updates").size(LabelSize::Large))
                            .child(
                                Label::new(format!("hermes-rs {CURRENT_VERSION}"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .when(!line.is_empty(), |this| {
                                this.child(Label::new(line).size(LabelSize::Small).color(color))
                            }),
                    )
                    .child(action),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Label::new("Check for updates once a day")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Switch::new("check-updates-switch", auto.into()).on_click(|state, _, cx| {
                            let on = *state == ToggleState::Selected;
                            AppState::global(cx).update(cx, |app, cx| {
                                app.config.check_updates = on;
                                app.save(cx);
                            });
                        }),
                    ),
            )
    }

    /// Import from Hermes: memory, skills and SOUL.md from `~/.hermes`.
    fn render_import(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let found = crate::hermes_import::hermes_home();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_4()
                    .child(Self::section(
                        "Import from Hermes",
                        Some("Copies Hermes's memory entries, the skills you made or installed, and SOUL.md (when you have none here). Hermes's files are left as they are."),
                    ))
                    .child(
                        Button::new("import-hermes-all", "Import")
                            .style(ButtonStyle::Outlined)
                            .disabled(found.is_none())
                            .tooltip(Tooltip::text(match &found {
                                Some(home) => format!("From {}", home.display()),
                                None => "No Hermes install found (~/.hermes or HERMES_HOME)".into(),
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_report = Some(crate::hermes_import::import_all(cx));
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.import_report.clone(), |this, (report, failed)| {
                this.child(
                    Callout::new()
                        .severity(if failed {
                            Severity::Warning
                        } else {
                            Severity::Success
                        })
                        .title(report),
                )
            })
    }

    /// The interface and code fonts: a family dropdown and a size stepper each.
    fn render_fonts(window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::appearance::{
            DEFAULT_CODE_FONT, DEFAULT_CODE_SIZE, DEFAULT_UI_FONT, DEFAULT_UI_SIZE, MAX_FONT_SIZE,
            MIN_FONT_SIZE,
        };
        let fonts = AppState::global(cx).read(cx).config.fonts.clone();
        // Families worth offering that are on this PC (plus the bundled ones).
        let installed = cx.text_system().all_font_names();
        let available = |candidates: &[&str], bundled: &str| -> Vec<String> {
            std::iter::once(bundled.to_string())
                .chain(
                    candidates
                        .iter()
                        .filter(|name| installed.iter().any(|i| i == *name))
                        .map(|name| name.to_string()),
                )
                .collect()
        };
        let ui_families = available(
            &["Segoe UI", "Segoe UI Variable", "Inter", "Arial", "Calibri", "Verdana", "Tahoma"],
            DEFAULT_UI_FONT,
        );
        let code_families = available(
            &[
                "Cascadia Code",
                "Cascadia Mono",
                "Consolas",
                "JetBrains Mono",
                "Fira Code",
                "Source Code Pro",
                "Courier New",
            ],
            DEFAULT_CODE_FONT,
        );
        type Set = fn(&mut config::FontSettings, Option<String>);
        type SetSize = fn(&mut config::FontSettings, Option<f32>);
        let family_picker = |id: &'static str,
                             current: Option<String>,
                             families: Vec<String>,
                             bundled: &'static str,
                             set: Set,
                             window: &mut Window,
                             cx: &mut Context<Self>| {
            let shown = |name: &str| {
                if name == bundled {
                    format!("{} (default)", &bundled[1..])
                } else {
                    name.to_string()
                }
            };
            let selected = current.clone().unwrap_or(bundled.to_string());
            let label = shown(&selected);
            let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
                for family in &families {
                    let value = (family != bundled).then(|| family.clone());
                    menu = menu.toggleable_entry(
                        shown(family),
                        *family == selected,
                        IconPosition::Start,
                        None,
                        move |_, cx| {
                            let value = value.clone();
                            AppState::global(cx).update(cx, |state, cx| {
                                let mut fonts = state.config.fonts.clone();
                                set(&mut fonts, value);
                                state.set_fonts(fonts, cx);
                            });
                        },
                    );
                }
                menu
            });
            DropdownMenu::new(id, label, menu)
        };
        let stepper = |id: &'static str, size: Option<f32>, default: f32, set: SetSize| {
            let current = size.unwrap_or(default);
            let step = move |delta: f32| {
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                    let next = (current + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                    AppState::global(cx).update(cx, |state, cx| {
                        let mut fonts = state.config.fonts.clone();
                        set(&mut fonts, (next != default).then_some(next));
                        state.set_fonts(fonts, cx);
                    });
                }
            };
            h_flex()
                .gap_1()
                .child(
                    IconButton::new(SharedString::from(format!("{id}-smaller")), IconName::Dash)
                        .icon_size(IconSize::Small)
                        .disabled(current <= MIN_FONT_SIZE)
                        .on_click(step(-1.)),
                )
                .child(
                    div()
                        .w(rems(2.5))
                        .flex()
                        .justify_center()
                        .child(Label::new(format!("{current}"))),
                )
                .child(
                    IconButton::new(SharedString::from(format!("{id}-larger")), IconName::Plus)
                        .icon_size(IconSize::Small)
                        .disabled(current >= MAX_FONT_SIZE)
                        .on_click(step(1.)),
                )
        };
        let ui_picker = family_picker(
            "ui-font",
            fonts.ui_family.clone(),
            ui_families,
            DEFAULT_UI_FONT,
            |fonts, value| fonts.ui_family = value,
            window,
            cx,
        );
        let code_picker = family_picker(
            "code-font",
            fonts.code_family.clone(),
            code_families,
            DEFAULT_CODE_FONT,
            |fonts, value| fonts.code_family = value,
            window,
            cx,
        );
        let row = |title: &'static str, picker: AnyElement, stepper: AnyElement| {
            h_flex()
                .justify_between()
                .child(Label::new(title))
                .child(h_flex().gap_3().child(picker).child(stepper))
        };
        v_flex()
            .gap_2()
            .child(Self::section(
                "Fonts",
                Some("The interface font, and the font for code, commands and tool output."),
            ))
            .child(row(
                "Interface",
                ui_picker.into_any_element(),
                stepper("ui-size", fonts.ui_size, DEFAULT_UI_SIZE, |f, v| f.ui_size = v)
                    .into_any_element(),
            ))
            .child(row(
                "Code",
                code_picker.into_any_element(),
                stepper("code-size", fonts.code_size, DEFAULT_CODE_SIZE, |f, v| {
                    f.code_size = v
                })
                .into_any_element(),
            ))
    }

    fn section(title: &'static str, description: Option<&'static str>) -> impl IntoElement {
        // Takes the room the controls beside it leave, so a long description wraps.
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(Label::new(title).size(LabelSize::Large))
            .when_some(description, |this, description| {
                this.child(
                    Label::new(description)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
    }

    /// Hermes's self-improvement switch: background reviews after turns.
    /// Hermes's `busy_input_mode`: what a message sent during a turn does.
    fn render_busy_input(cx: &mut Context<Self>) -> impl IntoElement {
        use crate::config::BusyInput;
        let current = AppState::global(cx).read(cx).config.busy_input_mode;
        let choice = |mode: BusyInput, label: &'static str, tip: &'static str| {
            Button::new(SharedString::from(format!("busy-{label}")), label)
                .style(if mode == current {
                    ButtonStyle::Filled
                } else {
                    ButtonStyle::Subtle
                })
                .tooltip(Tooltip::text(tip))
                .on_click(move |_, _, cx| {
                    AppState::global(cx).update(cx, |state, cx| {
                        state.config.busy_input_mode = mode;
                        state.save(cx);
                    });
                })
        };
        h_flex()
            .justify_between()
            .items_start()
            .gap_4()
            .child(Self::section(
                "While a Turn Runs",
                Some("What sending a message does while the agent is working. /queue and /steer work either way."),
            ))
            .child(
                h_flex()
                    .gap_1()
                    .flex_none()
                    .child(choice(BusyInput::Steer, "Steer", "Delivered to the agent after its current tool call"))
                    .child(choice(BusyInput::Queue, "Queue", "Sent when the running turn ends"))
                    .child(choice(BusyInput::Interrupt, "Interrupt", "Stops the running turn and sends this instead")),
            )
    }

    /// Hermes's `auxiliary.*`: the models side jobs run on, each defaulting to the chat's model.
    fn render_aux(cx: &mut Context<Self>) -> impl IntoElement {
        let aux = AppState::global(cx).read(cx).config.aux_models.clone();
        let row = |id: &'static str,
                   title: &'static str,
                   description: &'static str,
                   choice: Option<config::ModelChoice>,
                   set: fn(&mut config::AuxModelSettings, Option<config::ModelChoice>),
                   cx: &mut Context<Self>| {
            let name = choice
                .as_ref()
                .map(|choice| {
                    AppState::global(cx)
                        .read(cx)
                        .model_info(&choice.model)
                        .and_then(|m| m.name.clone())
                        .unwrap_or_else(|| choice.model.clone())
                })
                .unwrap_or_else(|| "Chat model".into());
            let selected = choice.clone().map(|c| (c.provider, c.model));
            let chosen = choice.is_some();
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    v_flex().min_w_0().child(Label::new(title)).child(
                        Label::new(description)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .flex_none()
                        .child(
                            PopoverMenu::new(SharedString::from(format!("aux-{id}")))
                                .menu(move |window, cx| {
                                    let on_select: crate::model_picker::OnSelect =
                                        Rc::new(move |provider, model, _, cx| {
                                            AppState::global(cx).update(cx, |state, cx| {
                                                set(
                                                    &mut state.config.aux_models,
                                                    Some(config::ModelChoice { provider, model }),
                                                );
                                                state.save(cx);
                                            });
                                        });
                                    let selected = selected.clone();
                                    Some(cx.new(|cx| {
                                        ModelPicker::new(selected, on_select, window, cx)
                                    }))
                                })
                                .trigger(
                                    Button::new(
                                        SharedString::from(format!("aux-button-{id}")),
                                        name,
                                    )
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .end_icon(
                                        Icon::new(IconName::ChevronDown)
                                            .size(IconSize::XSmall)
                                            .color(Color::Muted),
                                    ),
                                )
                                .anchor(gpui::Anchor::TopRight),
                        )
                        .when(chosen, |this| {
                            this.child(
                                IconButton::new(
                                    SharedString::from(format!("aux-reset-{id}")),
                                    IconName::Close,
                                )
                                .icon_size(IconSize::XSmall)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Use the Chat Model"))
                                .on_click(move |_, _, cx| {
                                    AppState::global(cx).update(cx, |state, cx| {
                                        set(&mut state.config.aux_models, None);
                                        state.save(cx);
                                    });
                                }),
                            )
                        }),
                )
        };
        v_flex()
            .gap_3()
            .child(Self::section(
                "Auxiliary Models",
                Some("Side jobs can run on a cheaper or faster model; each uses the chat's model unless one is chosen here."),
            ))
            .child(row(
                "compression",
                "Compression",
                "Writes the summary when the context is compacted.",
                aux.compression,
                |aux, choice| aux.compression = choice,
                cx,
            ))
            .child(row(
                "review",
                "Background review",
                "Reviews finished turns for memory and skills (and /refine).",
                aux.review,
                |aux, choice| aux.review = choice,
                cx,
            ))
            .child(row(
                "curator",
                "Curator",
                "Consolidates overlapping skills when the curator runs.",
                aux.curator,
                |aux, choice| aux.curator = choice,
                cx,
            ))
            .child(row(
                "approval",
                "Smart approvals",
                "Judges flagged commands before you are asked (when smart approvals are on).",
                aux.approval,
                |aux, choice| aux.approval = choice,
                cx,
            ))
            .child(row(
                "titles",
                "Session titles",
                "Names a session from its first message.",
                aux.titles,
                |aux, choice| aux.titles = choice,
                cx,
            ))
    }

    fn render_learning(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (enabled, consolidate) = {
            let config = &AppState::global(cx).read(cx).config;
            (config.background_review, config.curator_consolidate)
        };
        h_flex()
            .justify_between()
            .items_start()
            .gap_4()
            .child(Self::section(
                "Learning",
                Some("After some turns, the agent reviews the conversation in the background to save memories and improve its skills. /refine runs a review now; /learn turns anything into a skill. The curator archives skills the agent wrote that go unused for a month (Skills screen)."),
            ))
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Switch::new("background-review", enabled.into())
                            .label("Review in the background")
                            .on_click(cx.listener(|_, state: &ToggleState, _, cx| {
                                let enabled = *state == ToggleState::Selected;
                                AppState::global(cx).update(cx, |state, cx| {
                                    state.config.background_review = enabled;
                                    state.save(cx);
                                });
                                backend(cx).runtime.set_background_review(enabled);
                            })),
                    )
                    .child(
                        Switch::new("curator-consolidate", consolidate.into())
                            .label("Curator merges overlapping skills")
                            .on_click(cx.listener(|_, state: &ToggleState, _, cx| {
                                let on = *state == ToggleState::Selected;
                                AppState::global(cx).update(cx, |state, cx| {
                                    state.config.curator_consolidate = on;
                                    state.save(cx);
                                });
                            })),
                    ),
            )
    }

    /// Hermes's command approvals: whether to ask, and the patterns always allowed.
    /// Applies MCP servers: saved to the settings and (re)connected.
    fn apply_mcp(
        servers: std::collections::BTreeMap<String, hermes_runtime::mcp::McpServerConfig>,
        cx: &mut App,
    ) {
        AppState::global(cx).update(cx, |state, cx| {
            state.config.mcp_servers = servers.clone();
            state.save(cx);
        });
        let backend = backend(cx);
        let _guard = backend.tokio.enter();
        backend.runtime.mcp().configure(servers);
    }

    /// Hermes's `mcp_servers`: each server's state, an enable switch and Retry, and an editor for
    /// the JSON (the shape Claude Desktop and Cursor use pastes in as is).
    fn render_mcp(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use hermes_runtime::mcp::ServerStatus;
        let servers = AppState::global(cx).read(cx).config.mcp_servers.clone();
        let statuses: std::collections::HashMap<String, ServerStatus> =
            backend(cx).runtime.mcp().statuses().into_iter().collect();
        if self.mcp_poll.is_none() && statuses.values().any(|s| *s == ServerStatus::Connecting) {
            self.mcp_poll = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(700))
                    .await;
                this.update(cx, |this, cx| {
                    this.mcp_poll = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        let colors = cx.theme().colors();
        let editor = self.mcp_editor.clone();
        let editing = editor.is_some();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_4()
                    .child(Self::section(
                        "MCP Servers",
                        Some("Tools from Model Context Protocol servers, offered to the agent as mcp__<server>__<tool>. A server is a local command (stdio) or a URL (Streamable HTTP)."),
                    ))
                    .when(!editing, |this| {
                        this.child(
                            Button::new("edit-mcp", "Edit Servers")
                                .style(ButtonStyle::Outlined)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let servers =
                                        AppState::global(cx).read(cx).config.mcp_servers.clone();
                                    let text = if servers.is_empty() {
                                        String::new()
                                    } else {
                                        serde_json::to_string_pretty(&servers).unwrap_or_default()
                                    };
                                    let editor = cx.new(|cx| {
                                        let mut area = TextArea::new(MCP_EXAMPLE, cx).editor(16);
                                        area.set_text(text, cx);
                                        area
                                    });
                                    editor.focus_handle(cx).focus(window, cx);
                                    this.mcp_editor = Some(editor);
                                    this.mcp_error = None;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when_some(editor, |this, editor| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.editor_background)
                                .text_sm()
                                .child(editor.clone()),
                        )
                        .when_some(self.mcp_error.clone(), |this, error| {
                            this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("cancel-mcp", "Cancel").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.mcp_editor = None;
                                        this.mcp_error = None;
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    Button::new("save-mcp", "Save")
                                        .style(ButtonStyle::Filled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let text = editor.read(cx).text().trim().to_string();
                                            let text = if text.is_empty() {
                                                "{}".to_string()
                                            } else {
                                                text
                                            };
                                            match hermes_runtime::mcp::parse_servers(&text) {
                                                Ok(servers) => {
                                                    Self::apply_mcp(servers, cx);
                                                    this.mcp_editor = None;
                                                    this.mcp_error = None;
                                                }
                                                Err(error) => this.mcp_error = Some(error),
                                            }
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .when(servers.is_empty() && !editing, |this| {
                this.child(
                    Label::new(
                        "No MCP servers yet. Edit Servers to add one; a config from another app can be pasted in as is.",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
            })
            .children(servers.into_iter().map(|(name, server)| {
                let status = statuses
                    .get(&name)
                    .cloned()
                    .unwrap_or(ServerStatus::Disabled);
                let (status_text, status_color) = match &status {
                    ServerStatus::Disabled => ("Off".to_string(), Color::Muted),
                    ServerStatus::Connecting => ("Connecting…".to_string(), Color::Muted),
                    ServerStatus::Connected { tools } => (
                        format!(
                            "Connected · {tools} tool{}",
                            if *tools == 1 { "" } else { "s" }
                        ),
                        Color::Success,
                    ),
                    ServerStatus::Failed(error) => (format!("Failed: {error}"), Color::Error),
                };
                let target = match (&server.url, &server.command) {
                    (Some(url), _) => url.clone(),
                    (None, Some(command)) => std::iter::once(command.clone())
                        .chain(server.args.iter().cloned())
                        .collect::<Vec<_>>()
                        .join(" "),
                    (None, None) => String::new(),
                };
                let toggle_name = name.clone();
                let retry_name = name.clone();
                let failed = matches!(status, ServerStatus::Failed(_));
                h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(Label::new(name.clone()))
                            .child(
                                Label::new(target)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                            .child(
                                Label::new(status_text)
                                    .size(LabelSize::Small)
                                    .color(status_color)
                                    .truncate(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_none()
                            .when(failed, |this| {
                                this.child(
                                    Button::new(
                                        SharedString::from(format!("retry-mcp-{name}")),
                                        "Retry",
                                    )
                                    .style(ButtonStyle::Subtle)
                                    .on_click(move |_, _, cx| {
                                        let backend = backend(cx);
                                        let _guard = backend.tokio.enter();
                                        backend.runtime.mcp().reconnect(&retry_name);
                                    }),
                                )
                            })
                            .child(
                                Switch::new(
                                    SharedString::from(format!("mcp-{name}")),
                                    server.enabled.into(),
                                )
                                .on_click(cx.listener(
                                    move |_, state: &ToggleState, _, cx| {
                                        let mut servers = AppState::global(cx)
                                            .read(cx)
                                            .config
                                            .mcp_servers
                                            .clone();
                                        if let Some(server) = servers.get_mut(&toggle_name) {
                                            server.enabled = *state == ToggleState::Selected;
                                        }
                                        Self::apply_mcp(servers, cx);
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
            }))
    }

    fn render_safety(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ask = AppState::global(cx).read(cx).config.ask_dangerous_commands;
        let smart = AppState::global(cx).read(cx).config.smart_approvals;
        let checkpoints = AppState::global(cx).read(cx).config.checkpoints;
        let always = backend(cx).runtime.approvals().always_allowed();
        let colors = cx.theme().colors();
        let (border, background) = (colors.border_variant, colors.editor_background);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_4()
                    .child(Self::section(
                        "Safety",
                        Some("Commands that match a dangerous pattern (recursive deletes, force pushes, piping downloads into a shell, inline scripts…) wait for your approval on their card. Commands that could wreck the system never run through the agent. With smart approvals a model judges each flagged command first: clearly safe ones run, clearly dangerous ones are blocked, only uncertain ones are asked about. /yolo stops asking for one session."),
                    ))
                    .child(
                        v_flex()
                            .gap_2()
                            .items_end()
                            .child(
                        Switch::new("ask-dangerous", ask.into())
                            .label("Ask before dangerous commands")
                            .on_click(cx.listener(|_, state: &ToggleState, _, cx| {
                                let ask = *state == ToggleState::Selected;
                                AppState::global(cx).update(cx, |state, cx| {
                                    state.config.ask_dangerous_commands = ask;
                                    state.save(cx);
                                });
                                backend(cx).runtime.approvals().set_ask(ask);
                            })),
                            )
                            .child(
                                Switch::new("smart-approvals", smart.into())
                                    .label("Smart approvals")
                                    .disabled(!ask)
                                    .on_click(cx.listener(|_, state: &ToggleState, _, cx| {
                                        let smart = *state == ToggleState::Selected;
                                        AppState::global(cx).update(cx, |state, cx| {
                                            state.config.smart_approvals = smart;
                                            state.save(cx);
                                        });
                                        backend(cx).runtime.approvals().set_smart(smart);
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_4()
                    .child(Self::section(
                        "Checkpoints",
                        Some("Before Hermes first changes a project's files in a turn (writes, patches, deleting or moving commands), the project is snapshotted to a private store; its own .git is never touched. /rollback lists and restores snapshots, /diff session shows everything changed since the first."),
                    ))
                    .child(
                        Switch::new("checkpoints", checkpoints.into())
                            .label("Take checkpoints")
                            .on_click(cx.listener(|_, state: &ToggleState, _, cx| {
                                let enabled = *state == ToggleState::Selected;
                                AppState::global(cx).update(cx, |state, cx| {
                                    state.config.checkpoints = enabled;
                                    state.save(cx);
                                });
                                backend(cx).runtime.checkpoints().set_enabled(enabled);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("Always allowed")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        v_flex()
                            .p_1()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(background)
                            .when(always.is_empty(), |this| {
                                this.child(
                                    div().px_1p5().py_1().child(
                                        Label::new("Nothing yet. \"Always Allow\" on a command's card adds its pattern here.")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                                )
                            })
                            .children(always.into_iter().map(|pattern| {
                                let forget = pattern.clone();
                                h_flex()
                                    .px_1p5()
                                    .gap_2()
                                    .justify_between()
                                    .child(Label::new(pattern.clone()).size(LabelSize::Small))
                                    .child(
                                        IconButton::new(
                                            SharedString::from(format!("forget-{pattern}")),
                                            IconName::Close,
                                        )
                                        .icon_size(IconSize::XSmall)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text("Ask again for this pattern"))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            backend(cx).runtime.approvals().forget(&forget);
                                            cx.notify();
                                        })),
                                    )
                            })),
                    ),
            )
    }

    /// One provider in the list: icon, name, address and key status, with edit and remove.
    fn render_provider_row(
        &self,
        provider: &ProviderSettings,
        is_active: bool,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (key_icon, key_color, key_text) = key_status(provider);
        let edit_id = provider.id.clone();
        let remove_id = provider.id.clone();
        h_flex()
            .id(SharedString::from(format!("provider-{}", provider.id)))
            .group("provider-row")
            .w_full()
            .px_3()
            .py_2()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.editor_background)
            .hover(|style| style.border_color(colors.border))
            .child(
                Icon::new(provider_icon(provider))
                    .size(IconSize::Medium)
                    .color(if is_active {
                        Color::Accent
                    } else {
                        Color::Muted
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Label::new(provider.name.clone()))
                            .when(is_active, |this| {
                                this.child(Chip::new("Active").label_color(Color::Accent))
                            }),
                    )
                    .child(
                        Label::new(provider.base_url.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .buffer_font(cx)
                            .truncate(),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Icon::new(key_icon).size(IconSize::XSmall).color(key_color))
                            .child(
                                Label::new(key_text)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .when_some(error, |this, error| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::XCircle)
                                        .size(IconSize::XSmall)
                                        .color(Color::Error),
                                )
                                .child(
                                    Label::new(format!("Couldn't list models: {error}"))
                                        .size(LabelSize::XSmall)
                                        .color(Color::Error)
                                        .truncate(),
                                ),
                        )
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("edit-{}", provider.id)), "Edit")
                    .style(ButtonStyle::Outlined)
                    .label_size(LabelSize::Small)
                    .start_icon(Icon::new(IconName::Pencil).size(IconSize::XSmall))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_form(Some(edit_id.clone()), window, cx)
                    })),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("remove-{}", provider.id)),
                    IconName::Trash,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text("Remove Provider"))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.remove_provider(remove_id.clone(), cx)),
                ),
            )
    }

    fn render_form(&self, form: &ProviderForm, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let field = |label: &'static str, input: &Entity<TextArea>| {
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
        };
        let title = if form.editing.is_some() {
            "Edit Provider"
        } else {
            "Add Provider"
        };
        // A dimmed backdrop over the settings with the dialog centered on it; clicking the
        // backdrop or pressing Escape cancels.
        div()
            .id("provider-dialog-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.4))
            .occlude()
            // On press, not click: a drag that starts in a field and ends out here must not
            // close the dialog.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_form(cx)),
            )
            .child(
                v_flex()
                    .id("provider-dialog")
                    .w(rems(30.))
                    .max_w_full()
                    .p_4()
                    .gap_3()
                    .elevation_3(cx)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            this.close_form(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Headline::new(title).size(HeadlineSize::Small))
                            .child(
                                IconButton::new("close-dialog", IconName::Close)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Close"))
                                    .on_click(cx.listener(|this, _, _, cx| this.close_form(cx))),
                            ),
                    )
                    .child(
                        Label::new(
                            "Any OpenAI-compatible endpoint: OpenRouter, OpenAI, LM Studio, \
                             Ollama, vLLM… Servers on this machine need no key.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(field("Name", &form.name))
                    .child(field("Base URL", &form.base_url))
                    .child(field("API Key", &form.api_key))
                    .child(field("API Key Environment Variable", &form.api_key_env))
                    .when_some(form.error.clone(), |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    })
                    .child(
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("cancel-provider", "Cancel")
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|this, _, _, cx| this.close_form(cx))),
                            )
                            .child(
                                Button::new("save-provider", "Save Provider")
                                    .style(ButtonStyle::Tinted(TintColor::Accent))
                                    .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                            ),
                    ),
            )
    }
}

/// The provider's name, or one made from its address.
pub(crate) fn name_or(name: &str, base_url: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("Provider")
        .to_string()
}

/// A provider id from its name: lowercase letters, digits and dashes, unique in `providers`.
pub(crate) fn unique_id(name: &str, providers: &[ProviderSettings]) -> String {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "provider".into()
    } else {
        base
    };
    let mut id = base.clone();
    let mut n = 2;
    while providers.iter().any(|p| p.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// How the provider authenticates, for its row.
fn key_status(provider: &ProviderSettings) -> (IconName, Color, String) {
    if config::stored_api_key(&provider.id).is_some() {
        return (
            IconName::Check,
            Color::Success,
            "API key stored in Windows Credential Manager".into(),
        );
    }
    if let Some(var) = &provider.api_key_env {
        if provider_ready(provider) {
            return (
                IconName::Check,
                Color::Success,
                format!("API key read from the {var} environment variable"),
            );
        }
    }
    if provider.is_local() {
        return (
            IconName::Check,
            Color::Success,
            "No API key needed (a server on this machine)".into(),
        );
    }
    (IconName::Warning, Color::Warning, "No API key set".into())
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (panel_background, border_variant, editor_background) = {
            let colors = cx.theme().colors();
            (
                colors.panel_background,
                colors.border_variant,
                colors.editor_background,
            )
        };
        let (providers, active, errors, selected) = {
            let state = AppState::global(cx).read(cx);
            let errors: Vec<Option<String>> = state
                .config
                .providers
                .iter()
                .map(|p| {
                    state
                        .provider_models
                        .get(&p.id)
                        .and_then(|list| list.error.clone())
                })
                .collect();
            (
                state.config.providers.clone(),
                state.config.provider.clone(),
                errors,
                state.config.model.clone(),
            )
        };
        let rows: Vec<AnyElement> = providers
            .iter()
            .zip(errors)
            .map(|(provider, error)| {
                let is_active = active.as_deref() == Some(provider.id.as_str());
                self.render_provider_row(provider, is_active, error, cx)
                    .into_any_element()
            })
            .collect();

        let content = v_flex()
            .id("settings")
            .size_full()
            .p_6()
            .gap_5()
            .child(Headline::new("Settings").size(HeadlineSize::Large))
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .justify_between()
                    .child(Self::section("Appearance", Some("The color theme, from Zed's.")))
                    .child(Self::theme_picker(window, cx)),
            )
            .child(Self::render_fonts(window, cx))
            .child(Divider::horizontal())
            .child(Self::render_busy_input(cx))
            .child(Divider::horizontal())
            .child(self.render_learning(cx))
            .child(Divider::horizontal())
            .child(Self::render_aux(cx))
            .child(Divider::horizontal())
            .child(self.render_safety(cx))
            .child(Divider::horizontal())
            .child(self.render_mcp(cx))
            .child(Divider::horizontal())
            .child(self.render_import(cx))
            .child(Divider::horizontal())
            .child(Self::render_updates(cx))
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Self::section(
                                "Providers",
                                Some("OpenAI-compatible endpoints. Picking a model in the model selector makes its provider active."),
                            ))
                            .child(
                                Button::new("add-provider", "Add Provider")
                                    .style(ButtonStyle::Tinted(TintColor::Accent))
                                    .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_form(None, window, cx)
                                    })),
                            ),
                    )
                    .children(rows)
                    .when(providers.is_empty(), |this| {
                        this.child(
                            Label::new("No providers yet. Add one to start chatting.")
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Self::section(
                                "Default Model",
                                Some(match selected {
                                    Some(_) => "New sessions use it; each chat can switch from the model button under its message box.",
                                    None => "Pick the model new sessions use.",
                                }),
                            ))
                            .child(
                                Button::new("refresh-models", "Reload Models")
                                    .style(ButtonStyle::Outlined)
                                    .start_icon(Icon::new(IconName::ArrowCircle).size(IconSize::Small))
                                    .tooltip(Tooltip::text("List every provider's models again"))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        AppState::global(cx)
                                            .update(cx, |state, cx| state.refresh_models(cx));
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .h(px(420.))
                            .flex_none()
                            .rounded_md()
                            .border_1()
                            .border_color(border_variant)
                            .bg(editor_background)
                            .overflow_hidden()
                            // The list scrolls on its own; the wheel shouldn't also scroll the page.
                            .on_scroll(|_, _, cx| cx.stop_propagation())
                            .child(self.picker.clone()),
                    ),
            );
        let content =
            crate::scroll::with_scrollbar(div().size_full(), content, &self.scroll, window, cx);

        div()
            .relative()
            .size_full()
            .bg(panel_background)
            .child(content)
            .when_some(self.form.as_ref(), |this, form| {
                this.child(self.render_form(form, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_provider_ids_are_slugs_and_unique() {
        let existing = vec![ProviderSettings {
            id: "ollama".into(),
            name: "Ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key_env: None,
        }];
        assert_eq!(unique_id("LM Studio", &existing), "lm-studio");
        assert_eq!(unique_id("Ollama", &existing), "ollama-2");
        assert_eq!(unique_id("!!", &existing), "provider");
        assert_eq!(name_or("", "http://localhost:1234/v1"), "localhost:1234");
    }
}
