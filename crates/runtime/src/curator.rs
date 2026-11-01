//! Hermes's skill curator (`agent/curator.py`, MIT): background maintenance of the skills the
//! agent wrote itself. Each run first applies the deterministic transitions (idle skills go stale,
//! then to the archive), then, when consolidation is on, has the model merge overlapping
//! agent-created skills into class-level umbrellas, and writes a report. It runs at most once an
//! interval, only after the app has been idle a while, or when the user asks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context as _, Result};
use chrono::Utc;
use hermes_agent::inference::InferenceRunner;
use hermes_agent::machine::{StateMachine, Step};
use hermes_agent::operation::Emitter;
use hermes_agent::tool::{ToolOperation, ToolProvider};
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::model::ModelConfig;
use hermes_store::SessionRecord;
use hermes_tools::curator::{MIN_IDLE_HOURS, TransitionCounts};
use hermes_tools::{ReviewTools, SessionTools, SessionToolsConfig};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session::{HermesEffect, OpenSession, TurnRuntime, TurnSession};
use crate::{ProviderConfig, Runtime, TurnOptions, effort_request_params, ops};

/// Model calls one consolidation pass may make (Hermes allows many: umbrella-building over a
/// large library takes 50-100).
const CURATOR_MAX_ITERATIONS: u32 = 200;

/// The model the consolidation pass runs on.
pub struct CuratorModel {
    pub provider: ProviderConfig,
    pub model: String,
    pub options: TurnOptions,
}

/// What a curator run did.
#[derive(Debug, Clone)]
pub struct CuratorOutcome {
    pub summary: String,
    pub report: Option<PathBuf>,
}

impl Runtime {
    /// Whether the scheduled curator should run now (Hermes's `maybe_run_curator` gates): its
    /// interval has passed, nothing is running, and the app has been idle for two hours.
    pub fn curator_due(&self) -> bool {
        if self.curator_running.load(Ordering::Relaxed) || self.anything_running() {
            return false;
        }
        let idle = self.last_activity.lock().elapsed();
        idle >= std::time::Duration::from_secs(MIN_IDLE_HOURS as u64 * 3600)
            && self.skills.curator_due(Utc::now())
    }

    fn anything_running(&self) -> bool {
        self.sessions
            .lock()
            .values()
            .any(|open| open.running.lock().is_some() || open.review.lock().is_some())
    }

    /// Runs the curator now. `consolidate` names the model for the consolidation pass; without
    /// it only the deterministic transitions run (Hermes's default).
    pub async fn run_curator(&self, consolidate: Option<CuratorModel>) -> Result<CuratorOutcome> {
        anyhow::ensure!(
            !self.curator_running.swap(true, Ordering::SeqCst),
            "the curator is already running"
        );
        let result = self.curate(consolidate).await;
        self.curator_running.store(false, Ordering::SeqCst);
        result
    }

    async fn curate(&self, consolidate: Option<CuratorModel>) -> Result<CuratorOutcome> {
        let skills = self.skills.clone();
        let start = Utc::now();
        // A rollback point: every curator change lands in the skills history after this.
        skills.commit_outside_changes();
        let before: Vec<String> = skills.curated().into_iter().map(|s| s.name).collect();
        let counts = skills.apply_automatic_transitions(start);
        let auto = counts.summary();
        let mut state = skills.curator_state();
        state.last_run_at = Some(start.to_rfc3339());
        state.run_count += 1;
        state.last_run_summary = Some(format!("auto: {auto}"));
        skills.save_curator_state(&state);

        let candidates = skills.curator_candidates();
        let (llm, final_text) = match consolidate {
            None => ("skipped (consolidation off)".to_string(), String::new()),
            Some(_) if candidates.starts_with("No agent-created skills") => {
                ("skipped (no candidates)".to_string(), String::new())
            }
            Some(model) => match self.consolidate(&model, &candidates).await {
                Ok(text) => {
                    let short: String = text.chars().take(240).collect();
                    let short = if text.chars().count() > 240 {
                        format!("{short}…")
                    } else if short.is_empty() {
                        "no change".into()
                    } else {
                        short
                    };
                    (short, text)
                }
                Err(error) => (format!("error ({error:#})"), String::new()),
            },
        };
        let summary = format!("auto: {auto}; llm: {llm}");
        let after: Vec<String> = skills.curated().into_iter().map(|s| s.name).collect();
        let report = self
            .write_curator_report(start, &counts, &summary, &before, &after, &final_text)
            .map_err(|error| tracing::warn!("writing the curator report failed: {error:#}"))
            .ok();
        let mut state = skills.curator_state();
        state.last_run_summary = Some(summary.clone());
        state.last_report_path = report.as_ref().map(|p| p.display().to_string());
        skills.save_curator_state(&state);
        Ok(CuratorOutcome { summary, report })
    }

    /// The consolidation pass: a fresh agent with only the skill tools works through the
    /// candidates. Returns its final message.
    async fn consolidate(&self, model: &CuratorModel, candidates: &str) -> Result<String> {
        let skills_root = self.skills.root().to_path_buf();
        let scratch = self.config.data_dir.join("scratch").join("curator");
        let tools = Arc::new(SessionTools::new(SessionToolsConfig {
            session_id: "curator".into(),
            cwd: skills_root.clone(),
            home: dirs::home_dir().context("no home directory")?,
            scratch_dir: scratch.clone(),
            shell: self.config.shell.clone(),
            bridge: self.bridge.clone(),
            todos: Vec::new(),
            memory: None,
            history: None,
            skills: Some(self.skills.clone()),
            approvals: None,
            checkpoints: None,
            web: None,
        }));
        let system_prompt = self.system_prompt_with(
            &model.provider.id,
            &model.model,
            model.options.context_length,
            &skills_root,
            false,
        )?;
        let message = Message::user()
            .with_text(hermes_prompts::review::curator_message(candidates))
            .with_generated_id_if_missing();
        let session = Arc::new(OpenSession {
            record: Mutex::new(SessionRecord {
                id: "curator".into(),
                title: None,
                cwd: skills_root.display().to_string(),
                provider: model.provider.id.clone(),
                model: model.model.clone(),
                system_prompt,
                created_at: 0,
                updated_at: 0,
                pinned: false,
            }),
            conversation: Mutex::new(Conversation::new_unvalidated(vec![message])),
            tools: tools.clone(),
            running: Mutex::new(None),
            turn_state: Mutex::new(None),
            review: Mutex::new(None),
            nudges: Mutex::new(Default::default()),
            context: Mutex::new(None),
        });
        let state: ops::SharedTurnState = Arc::default();
        let runtime = TurnRuntime {
            session: session.clone(),
            store: self.store.clone(),
            state: state.clone(),
            persist: false,
            record_usage: false,
        };
        let cancel = CancellationToken::new();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
        let emit = Emitter::new(events_tx, cancel.clone());
        let tool_names: Vec<String> = ["skills_list", "skill_view", "skill_manage"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let provider_tools: Arc<dyn ToolProvider<TurnSession>> = Arc::new(ops::GuardedTools {
            inner: Arc::new(ReviewTools::curator(tools.clone())),
            scratch_dir: scratch,
            state: state.clone(),
            result_limit: ops::result_size_limit(model.options.context_length),
        });
        let mut model_config =
            ModelConfig::new(&model.model).with_context_limit(model.options.context_length);
        if let Some(effort) = model.options.effort.as_deref() {
            model_config = model_config.with_merged_request_params(effort_request_params(
                &model.provider.base_url,
                effort,
            ));
        }
        let steps: Vec<Step<'_, TurnSession, HermesEffect>> = vec![
            Step::Operation(Arc::new(ops::FrozenPrompt)),
            Step::Operation(Arc::new(ops::IterationBudget {
                state: state.clone(),
                max_iterations: CURATOR_MAX_ITERATIONS,
            })),
            Step::Operation(Arc::new(ops::ValidateToolCalls {
                state: state.clone(),
                tool_names: tool_names.clone(),
            })),
            Step::Operation(Arc::new(ToolOperation::new().with_provider(provider_tools))),
            Step::Operation(Arc::new(ops::UnhandledToolCalls { tool_names })),
            Step::Operation(Arc::new(ops::LengthContinuation {
                state: state.clone(),
            })),
            Step::Operation(Arc::new(ops::EmptyResponseRecovery {
                state: state.clone(),
            })),
            Step::Operation(Arc::new(ops::ExitOnError)),
            Step::Inference(Arc::new(InferenceRunner::new(
                model.provider.provider()?,
                model_config,
            ))),
        ];
        let machine = StateMachine::new(steps, cancel);
        let result = machine.run(&runtime, "curator", &emit).await;
        drop(machine);
        drop(emit);
        let _ = drain.await;
        tools.shutdown().await;
        result?;
        let conversation = session.conversation.lock().clone();
        Ok(conversation
            .messages()
            .iter()
            .rev()
            .find(|m| m.role == rmcp::model::Role::Assistant && m.is_agent_visible())
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|c| match c {
                        MessageContent::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default())
    }

    /// `logs/curator/<YYYYMMDD-HHMMSS>/REPORT.md`, like Hermes's per-run report.
    fn write_curator_report(
        &self,
        start: chrono::DateTime<Utc>,
        counts: &TransitionCounts,
        summary: &str,
        before: &[String],
        after: &[String],
        model_text: &str,
    ) -> Result<PathBuf> {
        let dir = self
            .config
            .data_dir
            .join("logs")
            .join("curator")
            .join(start.format("%Y%m%d-%H%M%S").to_string());
        std::fs::create_dir_all(&dir)?;
        let gone: Vec<&String> = before.iter().filter(|n| !after.contains(n)).collect();
        let new: Vec<&String> = after.iter().filter(|n| !before.contains(n)).collect();
        let list = |names: &[&String]| {
            if names.is_empty() {
                "- (none)".to_string()
            } else {
                names
                    .iter()
                    .map(|n| format!("- {n}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let mut report = format!(
            "# Curator run {}\n\n{summary}\n\n## Automatic transitions\n\n\
             - checked: {}\n- marked stale: {}\n- archived: {}\n- reactivated: {}\n\n\
             ## Curator-managed skills\n\n{} before, {} after.\n\nArchived or merged away:\n{}\n\nNew:\n{}\n",
            start.to_rfc3339(),
            counts.checked,
            counts.marked_stale,
            counts.archived,
            counts.reactivated,
            before.len(),
            after.len(),
            list(&gone),
            list(&new),
        );
        if !model_text.is_empty() {
            report.push_str(&format!("\n## Consolidation pass\n\n{model_text}\n"));
        }
        let path = dir.join("REPORT.md");
        std::fs::write(&path, report)?;
        Ok(path)
    }
}
