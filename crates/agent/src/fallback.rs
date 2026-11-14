//! The provider fallback chain a turn's [`crate::inference::InferenceRunner`] walks (hermes-rs,
//! backend v2 Phase 12; not from Hermes, whose chain is `agent/agent_init_fallback.py`).
//!
//! The chain is the primary provider (index 0, the runner's own) followed by [`FallbackTarget`]s
//! in the order the user listed them. The runner moves a request to the next target when
//! [`hermes_llm::fallback::classify`] says the failure is about the provider (quota, outage,
//! unknown model), when the [`FallbackGate`] says the provider's own daily limit is used up, and
//! only when the target is allowed by its gate and the request fits its window. Everything the
//! host knows (the governor, the ledger, token estimates) comes through [`FallbackGate`], so this
//! crate stays free of the runtime's types.
//!
//! Rules (documented again in `docs/ARCHITECTURE.md`):
//! - One request moves, never the session: the chain position is kept for the rest of the TURN
//!   (the runner lives one turn), and the next turn starts at the primary again, unless the gate
//!   says the primary is known to be out of requests until its reset.
//! - A switch happens only before anything of the response was delivered. A response that breaks
//!   half way keeps today's behaviour (what arrived stays, the error follows); it never goes to
//!   another provider, which would answer the question twice.
//! - A target the request does not fit is asked to compress first (the turn's compress step reads
//!   [`FallbackShared`]); if it still does not fit, the target is skipped.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use hermes_llm::base::Provider;
use hermes_llm::conversation::message::Message;
use hermes_llm::errors::ProviderError;
use hermes_llm::fallback::Trigger;
use hermes_llm::maybe_send::{MaybeSend, MaybeSync};
use hermes_llm::model::ModelConfig;

/// One entry of the chain after the primary.
pub struct FallbackTarget {
    /// The provider's id, as the user and the notice know it.
    pub label: String,
    pub provider: Arc<dyn Provider>,
    /// The model to ask for (its context length set when known).
    pub model_config: ModelConfig,
    /// The model's context window in tokens, when known.
    pub window: Option<usize>,
    /// The largest request, in estimated tokens, sent to this target without compressing first
    /// (the compression threshold of its window). `None` = unknown window: not checked.
    pub fit_limit: Option<usize>,
}

/// What the host decides about each position of the chain (`0` is the primary).
pub trait FallbackGate: MaybeSend + MaybeSync {
    /// May a request go to this provider now (the governor, an "exhausted until reset" mark)?
    /// `Err` is why not, in words for the user.
    fn allow(&self, index: usize) -> Result<(), String>;

    /// A request to this provider failed in a way that switches (the host records a used-up
    /// quota so later turns skip the provider until its reset).
    fn failed(&self, index: usize, trigger: Trigger, error: &ProviderError);

    /// Estimated tokens of the request about to be sent.
    fn estimate(
        &self,
        system_prompt: &str,
        messages: &[Message],
        tools: &[rmcp::model::Tool],
    ) -> usize;
}

/// What a turn's compress step needs to know about the chain: which window the turn runs against
/// now. Set when the runner moves to a target (so later requests of the turn compress for that
/// window) or asks for compression before moving.
#[derive(Default)]
pub struct FallbackShared {
    bound: Mutex<Option<Bound>>,
}

/// The target the turn is bound to: its provider (a compression summary can go there when the
/// primary is the one that is out of requests) and its window.
#[derive(Clone)]
pub struct Bound {
    pub label: String,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub window: Option<usize>,
}

impl FallbackShared {
    pub fn bound(&self) -> Option<Bound> {
        self.bound.lock().ok().and_then(|bound| bound.clone())
    }

    pub(crate) fn bind(&self, bound: Option<Bound>) {
        if let Ok(mut slot) = self.bound.lock() {
            *slot = bound;
        }
    }
}

pub struct FallbackChain {
    pub targets: Vec<FallbackTarget>,
    pub gate: Arc<dyn FallbackGate>,
    pub shared: Arc<FallbackShared>,
}

/// Where the runner is in the chain, across the requests of one turn.
#[derive(Default)]
pub(crate) struct ChainRun {
    /// The position serving the turn now (0 = the primary).
    pub current: usize,
    /// A target compression was asked for, to be tried again once the conversation is smaller.
    pub pending: Option<Pending>,
    /// Targets compression was already asked for in this turn.
    pub compress_asked: HashSet<usize>,
}

pub(crate) struct Pending {
    /// The target compression was asked for.
    pub index: usize,
    /// The position the request left, and the notice's words for why.
    pub from: usize,
    pub reason: String,
    /// The provider it left is out of requests until its reset.
    pub quota: bool,
    /// The error that started the move (the one shown if the whole chain fails).
    pub first_error: Option<ProviderError>,
    /// What happened at each position so far, for the final error's summary.
    pub trail: Vec<String>,
}

/// The notice the page shows when a request moves to another provider. `reason` is a sentence
/// naming the provider that could not serve ("a has used up its request quota"); `primary_out`:
/// the primary is out of requests until its reset (the next turns skip it).
pub fn switch_notice(
    reason: &str,
    to: &str,
    model: &str,
    primary: &str,
    primary_out: bool,
) -> String {
    let next = if to == primary {
        String::new()
    } else if primary_out {
        format!(
            " {primary} is out of requests until it resets, so the next messages go to {to} until then."
        )
    } else {
        format!(" The next message tries {primary} again.")
    };
    format!(
        "{reason}: switched to {to} ({model}) for the rest of this turn. Your conversation is now sent to {to}, and its prompt cache starts empty there, so the next request reads the whole conversation again.{next}"
    )
}
