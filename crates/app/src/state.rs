//! App-wide state shared by every view: settings and the active provider's discovered models.

use std::path::PathBuf;

use gpui::{App, AppContext as _, Context, Entity, Global};
use hermes_llm::discovery::DiscoveredModel;
use hermes_runtime::ProviderConfig;

use crate::backend::backend;
use crate::config::{self, AppConfig, ProviderSettings};

pub struct AppState {
    pub config: AppConfig,
    /// Code-block languages, colored with the active theme's syntax colors.
    pub languages: std::sync::Arc<markdown::language::LanguageRegistry>,
    pub data_dir: PathBuf,
    pub models: Vec<DiscoveredModel>,
    pub models_loading: bool,
    pub models_error: Option<String>,
}

struct AppStateGlobal(Entity<AppState>);

impl Global for AppStateGlobal {}

impl AppState {
    pub fn init(data_dir: PathBuf, cx: &mut App) -> Entity<Self> {
        let config = AppConfig::load(&data_dir);
        let memory = backend(cx).runtime.memory();
        memory.set_config(hermes_memory::MemoryConfig {
            write_approval: config.memory_write_approval,
            ..memory.config()
        });
        let skills = backend(cx).runtime.skills();
        skills.set_config(hermes_tools::SkillsConfig {
            write_approval: config.skill_write_approval,
            ..skills.config()
        });
        let registry = theme::ThemeRegistry::global(cx);
        hermes_themes::load_bundled_themes(&registry);
        let name = config.theme.as_deref().unwrap_or(theme::DEFAULT_DARK_THEME);
        if let Ok(theme) = registry
            .get(name)
            .or_else(|_| registry.get(theme::DEFAULT_DARK_THEME))
        {
            theme::GlobalTheme::update_theme(cx, theme);
        }
        let languages = hermes_highlight::language_registry(theme::ActiveTheme::theme(cx).syntax());
        let state = cx.new(|cx| {
            let mut state = Self {
                config,
                languages,
                data_dir,
                models: Vec::new(),
                models_loading: false,
                models_error: None,
            };
            state.refresh_models(cx);
            state
        });
        cx.set_global(AppStateGlobal(state.clone()));
        state
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<AppStateGlobal>().0.clone()
    }

    pub fn provider(&self) -> Option<&ProviderSettings> {
        self.config.active_provider()
    }

    pub fn runtime_provider(&self) -> Option<ProviderConfig> {
        self.provider().map(config::runtime_provider)
    }

    /// Whether the active provider can be called: it has a key, or it's local and needs none.
    pub fn has_api_key(&self) -> bool {
        self.provider()
            .is_some_and(|p| p.is_local() || config::api_key(p).is_some())
    }

    pub fn model_info(&self, id: &str) -> Option<&DiscoveredModel> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn context_length(&self, model: &str) -> Option<usize> {
        self.model_info(model)
            .and_then(|model| model.context_length)
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save(&self.data_dir) {
            tracing::error!("saving settings: {error:#}");
        }
        cx.notify();
    }

    /// Switches to the named theme, recolors code blocks to match, and remembers the choice.
    pub fn set_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        let Ok(theme) = theme::ThemeRegistry::global(cx).get(name) else {
            return;
        };
        self.languages = hermes_highlight::language_registry(theme.syntax());
        theme::GlobalTheme::update_theme(cx, theme);
        self.config.theme = Some(name.to_string());
        self.save(cx);
        cx.refresh_windows();
    }

    /// Re-lists the active provider's models in the background.
    pub fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.runtime_provider() else {
            return;
        };
        self.models_loading = true;
        self.models_error = None;
        let task = backend(cx).spawn(async move { provider.list_models().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.models_loading = false;
                match result {
                    Ok(Ok(models)) => this.models = models,
                    Ok(Err(error)) => this.models_error = Some(format!("{error:#}")),
                    Err(error) => this.models_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
