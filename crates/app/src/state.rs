//! App-wide state shared by every view: settings and every provider's discovered models.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{App, AppContext as _, Context, Entity, Global};
use hermes_llm::discovery::DiscoveredModel;

use crate::backend::backend;
use crate::config::{self, AppConfig, ProviderSettings};

pub struct AppState {
    pub config: AppConfig,
    /// Code-block languages, colored with the active theme's syntax colors.
    pub languages: std::sync::Arc<markdown::language::LanguageRegistry>,
    pub data_dir: PathBuf,
    /// Each provider's discovered models, by provider id.
    pub provider_models: HashMap<String, ProviderModels>,
}

#[derive(Default)]
pub struct ProviderModels {
    pub models: Vec<DiscoveredModel>,
    pub loading: bool,
    pub error: Option<String>,
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
                provider_models: HashMap::new(),
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

    /// The provider that serves `model`: the active one when it lists it, else the first that
    /// does (a session may use a model from a provider that's no longer active).
    pub fn provider_for_model(&self, model: &str) -> Option<&ProviderSettings> {
        let serves = |provider: &&ProviderSettings| {
            self.provider_models
                .get(&provider.id)
                .is_some_and(|list| list.models.iter().any(|m| m.id == model))
        };
        self.provider()
            .filter(serves)
            .or_else(|| self.config.providers.iter().find(serves))
            .or_else(|| self.provider())
    }

    /// Whether the active provider can be called: it has a key, or it's local and needs none.
    pub fn has_api_key(&self) -> bool {
        self.provider().is_some_and(provider_ready)
    }

    pub fn models_loading(&self) -> bool {
        self.provider_models.values().any(|list| list.loading)
    }

    pub fn model_info(&self, id: &str) -> Option<&DiscoveredModel> {
        let provider = self.provider_for_model(id)?;
        self.provider_models
            .get(&provider.id)?
            .models
            .iter()
            .find(|model| model.id == id)
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

    /// Re-lists every ready provider's models in the background (Zed's model selector shows
    /// them all, grouped by provider).
    pub fn refresh_models(&mut self, cx: &mut Context<Self>) {
        self.provider_models
            .retain(|id, _| self.config.providers.iter().any(|p| &p.id == id));
        let ids: Vec<String> = self
            .config
            .providers
            .iter()
            .filter(|p| provider_ready(p))
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            self.refresh_provider_models(&id, cx);
        }
    }

    /// Re-lists one provider's models.
    pub fn refresh_provider_models(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(provider) = self.config.providers.iter().find(|p| p.id == id) else {
            return;
        };
        let provider = config::runtime_provider(provider);
        let list = self.provider_models.entry(id.to_string()).or_default();
        list.loading = true;
        list.error = None;
        let id = id.to_string();
        let task = backend(cx).spawn(async move { provider.list_models().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let list = this.provider_models.entry(id).or_default();
                list.loading = false;
                match result {
                    Ok(Ok(models)) => list.models = models,
                    Ok(Err(error)) => list.error = Some(format!("{error:#}")),
                    Err(error) => list.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

/// A provider can be called when it has a key, or runs on this machine and needs none.
pub fn provider_ready(provider: &ProviderSettings) -> bool {
    provider.is_local() || config::api_key(provider).is_some()
}
