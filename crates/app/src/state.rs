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
    /// Whether a newer release is out (Settings → Updates).
    pub update: crate::update::UpdateStatus,
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
        backend(cx)
            .runtime
            .set_background_review(config.background_review);
        backend(cx)
            .runtime
            .approvals()
            .set_ask(config.ask_dangerous_commands);
        backend(cx)
            .runtime
            .approvals()
            .set_smart(config.smart_approvals);
        backend(cx)
            .runtime
            .checkpoints()
            .set_enabled(config.checkpoints);
        {
            let backend = backend(cx);
            let _guard = backend.tokio.enter();
            backend.runtime.mcp().configure(config.mcp_servers.clone());
        }
        theme::set_theme_settings_provider(
            Box::new(crate::appearance::Appearance::new(&config.fonts)),
            cx,
        );
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
                update: Default::default(),
            };
            state.refresh_models(cx);
            state.check_for_updates(false, cx);
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

    /// Whether the model accepts images (its discovered input modalities; unknown counts as no).
    pub fn supports_vision(&self, model: &str) -> bool {
        self.model_info(model)
            .and_then(|model| model.supports_vision)
            .unwrap_or(false)
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

    /// Saves the font settings and redraws with them.
    pub fn set_fonts(&mut self, fonts: config::FontSettings, cx: &mut Context<Self>) {
        theme::set_theme_settings_provider(
            Box::new(crate::appearance::Appearance::new(&fonts)),
            cx,
        );
        self.config.fonts = fonts;
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

    /// Looks for a newer release: when asked (`manual`), else at most once a day while
    /// automatic checks are on.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        use crate::update::{CHECK_INTERVAL_MS, UpdateStatus};
        if matches!(self.update, UpdateStatus::Checking | UpdateStatus::Downloading(_)) {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let due = self
            .config
            .last_update_check
            .is_none_or(|last| now - last >= CHECK_INTERVAL_MS);
        if !manual && !(self.config.check_updates && due) {
            return;
        }
        self.update = UpdateStatus::Checking;
        let task = backend(cx).spawn(crate::update::check());
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.update = match result {
                    Ok(Ok(Some(release))) => UpdateStatus::Available(release),
                    Ok(Ok(None)) => UpdateStatus::UpToDate,
                    Ok(Err(error)) => UpdateStatus::Failed(format!("{error:#}")),
                    Err(error) => UpdateStatus::Failed(error.to_string()),
                };
                this.config.last_update_check = Some(now);
                this.save(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Downloads the available release's installer, starts it and quits.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        use crate::update::UpdateStatus;
        let UpdateStatus::Available(release) = self.update.clone() else {
            return;
        };
        self.update = UpdateStatus::Downloading(release.clone());
        let download = release.clone();
        let task = backend(cx).spawn(async move { crate::update::download(&download).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let started = match result {
                Ok(Ok(path)) => crate::update::run_installer(&path),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(anyhow::anyhow!(error.to_string())),
            };
            this.update(cx, |this, cx| match started {
                Ok(()) => cx.quit(),
                Err(error) => {
                    this.update = UpdateStatus::Failed(format!("{error:#}"));
                    cx.notify();
                }
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

impl AppState {
    /// A side job's model on its provider, when one is chosen and the provider still exists.
    pub fn aux_model(
        &self,
        choice: &Option<config::ModelChoice>,
    ) -> Option<hermes_runtime::AuxModel> {
        let choice = choice.as_ref()?;
        let provider = self
            .config
            .providers
            .iter()
            .find(|p| p.id == choice.provider)?;
        Some(hermes_runtime::AuxModel {
            provider: config::runtime_provider(provider),
            model: choice.model.clone(),
            context_length: self
                .provider_models
                .get(&provider.id)
                .and_then(|list| list.models.iter().find(|m| m.id == choice.model))
                .and_then(|m| m.context_length),
            effort: None,
        })
    }

    /// The turn's side-job models (compression and background review).
    pub fn aux_models(&self) -> hermes_runtime::AuxModels {
        hermes_runtime::AuxModels {
            compression: self.aux_model(&self.config.aux_models.compression),
            review: self.aux_model(&self.config.aux_models.review),
            approval: self.aux_model(&self.config.aux_models.approval),
        }
    }

    /// The model the curator's consolidation pass runs on: the default model, when consolidation
    /// is on and a provider serves it.
    pub fn curator_model(&self) -> Option<hermes_runtime::CuratorModel> {
        if !self.config.curator_consolidate {
            return None;
        }
        // The auxiliary curator model, else the default model.
        let (model, provider) = match &self.config.aux_models.curator {
            Some(choice) => (
                choice.model.clone(),
                self.config
                    .providers
                    .iter()
                    .find(|p| p.id == choice.provider)?,
            ),
            None => {
                let model = self.config.model.clone()?;
                let provider = self.provider_for_model(&model)?;
                (model, provider)
            }
        };
        Some(hermes_runtime::CuratorModel {
            provider: config::runtime_provider(provider),
            options: hermes_runtime::TurnOptions {
                context_length: self.context_length(&model),
                effort: self.config.efforts.get(&model).cloned(),
                supports_vision: self.supports_vision(&model),
                aux: Default::default(),
            },
            model,
        })
    }
}

/// Runs the curator in the background; `done` gets the outcome on the UI thread.
pub fn run_curator<T: 'static>(
    model: Option<hermes_runtime::CuratorModel>,
    cx: &mut gpui::Context<T>,
    done: impl FnOnce(&mut T, Result<hermes_runtime::CuratorOutcome, String>, &mut gpui::Context<T>)
    + 'static,
) {
    let runtime = backend(cx).runtime.clone();
    let task = backend(cx).spawn(async move { runtime.run_curator(model).await });
    cx.spawn(async move |this, cx| {
        let outcome = match task.await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = &outcome {
            tracing::warn!("curator run failed: {error}");
        }
        this.update(cx, |this, cx| done(this, outcome, cx)).ok();
    })
    .detach();
}
