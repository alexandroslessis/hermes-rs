//! The runtime side of the provider fallback chain (backend v2 Phase 12; not from Hermes, whose
//! chain is `agent/agent_init_fallback.py`). The agent crate's `InferenceRunner` walks the chain
//! (`hermes_agent::fallback`); this module gives it what only the runtime knows: whether the
//! governor lets a request go to a provider, what to remember when a provider says its quota is
//! gone, and how big a request is.
//!
//! What is decided here, for the user's sake:
//! - The chain holds only providers the user configured (the host resolves ids to
//!   [`ProviderConfig`]s; no key is ever read from the chain's entries).
//! - Each provider in the chain is governed by its own limit: a request to a fallback goes through
//!   [`Governor::check`] for that provider like any other, so a switch cannot spend a quota the
//!   user capped.
//! - The ledger counts a request on the provider that served it (every client is labelled with
//!   its provider id), so the daily counts stay right after a switch.
//! - A provider that said "daily quota used up" is refused by the governor until its reset
//!   (`Ledger::mark_exhausted`, in memory), so the next turn goes straight to the fallback
//!   instead of spending a request to learn it again.
//! - Only the main turn uses the chain. The review, curator, title, compression summary, approval
//!   judge and `delegate_task` children keep their own providers (the governor already defers
//!   background jobs when quota is low); a chain for them would send background work, which the
//!   user did not see, to another provider.

use std::sync::Arc;

use hermes_agent::fallback::{FallbackChain, FallbackGate, FallbackShared, FallbackTarget};
use hermes_llm::conversation::message::Message;
use hermes_llm::errors::ProviderError;
use hermes_llm::fallback::{Trigger, daily_limit_in};
use hermes_llm::model::ModelConfig;
use hermes_llm::observer::Purpose;

use crate::ProviderConfig;
use crate::budget::BudgetKnobs;
use crate::compress::{Budget, estimate_messages, estimate_text};
use crate::governor::{Governor, Verdict};
use crate::ledger::Ledger;

/// The most providers a chain may hold after the primary (each one is somewhere the conversation
/// can be sent; more than three is a configuration mistake).
pub const MAX_FALLBACKS: usize = 3;

/// One entry of the chain, resolved by the host from the user's configured providers.
#[derive(Debug, Clone)]
pub struct FallbackModel {
    pub provider: ProviderConfig,
    pub model: String,
    /// The model's context window, when known (decides whether a request fits, `None` = not
    /// checked).
    pub context_length: Option<usize>,
    /// The model takes images.
    pub supports_vision: bool,
}

struct RuntimeGate {
    governor: Governor,
    ledger: Ledger,
    /// The provider id at each position (0 = the primary).
    ids: Vec<String>,
}

impl FallbackGate for RuntimeGate {
    fn allow(&self, index: usize) -> Result<(), String> {
        match self.governor.check(&self.ids[index], Purpose::Main) {
            Verdict::Deny(why) => Err(why),
            _ => Ok(()),
        }
    }

    fn failed(&self, index: usize, trigger: Trigger, error: &ProviderError) {
        if trigger != Trigger::DailyQuota {
            return;
        }
        let provider = &self.ids[index];
        // What the provider says its limit is teaches the ledger (as a failed turn does), and
        // the day's quota is marked used up until the provider's reset.
        if let Some(limit) = daily_limit_in(&error.to_string()) {
            self.ledger.learn_limit(provider, limit);
        }
        self.ledger
            .mark_exhausted(provider, self.ledger.usage(provider).resets_at);
    }

    fn estimate(
        &self,
        system_prompt: &str,
        messages: &[Arc<Message>],
        tools: &[rmcp::model::Tool],
    ) -> usize {
        let tools: usize = tools
            .iter()
            .map(|tool| {
                tool.name.len()
                    + tool.description.as_deref().map_or(0, str::len)
                    + serde_json::to_string(tool.input_schema.as_ref()).map_or(0, |s| s.len())
            })
            .sum();
        estimate_text(system_prompt) + estimate_messages(messages) + tools.div_ceil(4)
    }
}

/// The chain for a turn on `primary`, or `None` when no entry is usable. An entry whose provider
/// cannot be built is left out (logged), never a failure of the turn.
pub(crate) fn build(
    primary: &str,
    models: &[FallbackModel],
    budget: &BudgetKnobs,
    ledger: &Ledger,
) -> Option<FallbackChain> {
    let mut ids = vec![primary.to_string()];
    let mut targets = Vec::new();
    for model in models.iter().take(MAX_FALLBACKS) {
        let provider = match model.provider.provider_with(budget.http_retries) {
            Ok(provider) => provider,
            Err(error) => {
                tracing::warn!(provider = %model.provider.id, "fallback entry skipped: {error:#}");
                continue;
            }
        };
        ids.push(model.provider.id.clone());
        targets.push(FallbackTarget {
            label: model.provider.id.clone(),
            provider,
            model_config: ModelConfig::new(&model.model)
                .with_context_limit(model.context_length)
                .with_vision_support(model.supports_vision),
            window: model.context_length,
            fit_limit: model
                .context_length
                .filter(|&n| n > 0)
                .map(|n| Budget::with_compress_at(Some(n), budget.compress_at).threshold),
        });
    }
    if targets.is_empty() {
        return None;
    }
    Some(FallbackChain {
        targets,
        gate: Arc::new(RuntimeGate {
            governor: Governor::new(ledger.clone()),
            ledger: ledger.clone(),
            ids,
        }),
        shared: Arc::new(FallbackShared::default()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (hermes_testdir::TestDir, Ledger) {
        let dir = hermes_testdir::tempdir().unwrap();
        let ledger = Ledger::open(dir.path().join("requests.json"));
        (dir, ledger)
    }

    fn model(id: &str, window: Option<usize>) -> FallbackModel {
        FallbackModel {
            provider: ProviderConfig {
                id: id.into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: None,
            },
            model: "m".into(),
            context_length: window,
            supports_vision: false,
        }
    }

    #[test]
    fn the_chain_is_capped_and_empty_without_entries() {
        let (_dir, ledger) = ledger();
        let budget = BudgetKnobs::BALANCED;
        assert!(build("a", &[], &budget, &ledger).is_none());
        let many: Vec<_> = (0..6).map(|n| model(&format!("p{n}"), None)).collect();
        let chain = build("a", &many, &budget, &ledger).unwrap();
        assert_eq!(chain.targets.len(), MAX_FALLBACKS);
    }

    #[test]
    fn the_fit_limit_is_the_compression_threshold_of_the_window() {
        let (_dir, ledger) = ledger();
        let budget = BudgetKnobs::BALANCED;
        let chain = build(
            "a",
            &[model("b", Some(8_192)), model("c", None)],
            &budget,
            &ledger,
        )
        .unwrap();
        let expected = Budget::with_compress_at(Some(8_192), budget.compress_at).threshold;
        assert_eq!(chain.targets[0].fit_limit, Some(expected));
        assert!(expected < 8_192);
        // An unknown window is not checked.
        assert_eq!(chain.targets[1].fit_limit, None);
    }

    #[test]
    fn the_gate_applies_each_providers_own_limit() {
        let (_dir, ledger) = ledger();
        ledger.set_limit("a", Some(2));
        ledger.set_limit("b", Some(1000));
        for _ in 0..2 {
            ledger.record("a", Purpose::Main);
        }
        // The ledger thread applies counts asynchronously: wait for it.
        for _ in 0..200 {
            if ledger.usage("a").used >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let chain = build("a", &[model("b", None)], &BudgetKnobs::BALANCED, &ledger).unwrap();
        assert!(chain.gate.allow(0).is_err(), "the primary is used up");
        assert!(
            chain.gate.allow(1).is_ok(),
            "the fallback has its own budget"
        );
    }

    #[test]
    fn a_daily_quota_answer_marks_the_provider_used_up_and_teaches_the_limit() {
        let (_dir, ledger) = ledger();
        let chain = build("a", &[model("b", None)], &BudgetKnobs::BALANCED, &ledger).unwrap();
        assert!(chain.gate.allow(0).is_ok());
        let error = ProviderError::RateLimitExceeded {
            details: "user daily request limit reached (800 of 800).".into(),
            retry_delay: None,
        };
        // Other triggers (an outage, an unknown model) leave the provider alone.
        chain.gate.failed(0, Trigger::ServerError, &error);
        assert!(chain.gate.allow(0).is_ok());
        chain.gate.failed(0, Trigger::DailyQuota, &error);
        assert!(chain.gate.allow(0).is_err());
        assert!(chain.gate.allow(1).is_ok());
        for _ in 0..200 {
            if ledger.usage("a").limit == Some(800) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ledger.usage("a").limit, Some(800));
    }
}
