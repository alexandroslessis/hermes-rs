//! The evidence review: one request, no tools, a JSON answer applied deterministically (Phase 7
//! of `docs/backend/PHASES.md`, CP7.2). It replaces replaying the conversation to a forked agent
//! (`review.rs`, Hermes's `agent/background_review.py`), which costs up to 16 requests and lets
//! the model write through tools as it goes.
//!
//! [`run_review_once`] takes a session's unreviewed learning signals (`signals.rs`) and:
//! 1. builds the **evidence packet** (`packet.rs`): the signals with their excerpts, the skills
//!    the session used and the top related ones from the local index in full, and the current
//!    MEMORY.md / USER.md, within a token budget, secrets masked;
//! 2. sends **one request** with the packet and the review prompt
//!    (`hermes_prompts::review::evidence_review_prompt`), asking for the reply schema through
//!    `response_format: json_schema` (`contract.rs`). A server that refuses that field with a 400
//!    or 422 gets the same request once more with the format stated in text, and, when that one
//!    goes through, is remembered (per endpoint and model, for the life of the process) so later
//!    reviews ask in text at once. Each request has a wall-clock limit ([`REQUEST_TIMEOUT`]); the
//!    packet's budget leaves room in the model's window for the prompt, the schema, the repair
//!    turn and the reply ([`reserved_tokens`]);
//! 3. reads the reply: a reply that isn't one JSON object gets **one repair request** (the bad
//!    reply and why it was refused), and when that fails too the review gives up, logged. It
//!    never loops: at most three requests in all (schema refused, text, repair), normally one;
//! 4. validates each operation on its own (`contract.rs`) and **applies** the valid ones through
//!    the stores' own write paths, or stages them for the user when the session is tainted or
//!    `review.require_approval` is on (`apply.rs`); a memory replace or remove is always staged
//!    (nobody watches a background review; Hermes's `_background_delete_gate`, which the agentic
//!    review keeps too). Every refused operation is logged with why.
//!
//! The request is a `review` request in the ledger (the caller sets the request context) on the
//! auxiliary review model when one is set; the governor is asked first. The model's output is
//! untrusted: nothing it says is executed; it can only propose the memory and skill operations
//! the contract allows, and every one of them meets the same limits and threat scans as a direct
//! write. The caller marks the signals reviewed only when a reply was read (no ops included).
//!
//! Scheduling (when this runs) is CP7.3; until then the nudge-driven agentic review in
//! `review.rs` stays the active one and nothing calls this but tests (and `Runtime::review_once`).
//!
//! Not from Hermes; `PORTING.md` marks the review `diverged`.

mod apply;
pub mod contract;
pub mod packet;
mod udiff;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use hermes_llm::base::Provider;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::errors::ProviderError;
use hermes_llm::model::ModelConfig;
use hermes_memory::{MemoryStore, Target};
use hermes_store::Store;
use hermes_tools::index::{IndexQuery, Limits, LocalIndex};
use hermes_tools::skills::SkillStore;
use parking_lot::Mutex;
use rmcp::model::Role;
use serde_json::Value;

pub use apply::{ApplyContext, Hold, OpReport, OpResult};
pub use packet::{MemoryFile, PACKET_MAX_TOKENS, PacketInput, SkillText};

use crate::signals::Signal;

/// How the reply format is asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplyFormat {
    /// `json_schema`, unless this endpoint and model refused it before (then text).
    #[default]
    Auto,
    /// Always `response_format: json_schema` (still falls back once on a refusal).
    JsonSchema,
    /// Always the JSON-in-text contract.
    Text,
}

/// The review's settings (knobs; CP7.3 reads them from the config).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewOnceSettings {
    /// The packet's token budget ([`PACKET_MAX_TOKENS`]).
    pub packet_tokens: usize,
    /// `review.require_approval`: every write goes to the staging area.
    pub require_approval: bool,
    pub format: ReplyFormat,
}

impl Default for ReviewOnceSettings {
    fn default() -> Self {
        Self {
            packet_tokens: PACKET_MAX_TOKENS,
            require_approval: false,
            format: ReplyFormat::Auto,
        }
    }
}

/// How a review ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewStatus {
    /// A reply was read and its operations applied, staged or refused (possibly none).
    Done,
    /// No unreviewed signals: nothing was sent.
    NothingToReview,
    /// Not run (the quota governor said no); nothing was sent.
    Skipped(String),
    /// The request failed or the reply stayed unreadable after the repair; nothing applied.
    Failed(String),
}

/// What [`run_review_once`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewOutcome {
    pub status: ReviewStatus,
    /// Requests sent (each one counted by the ledger as `review`).
    pub requests: usize,
    /// Whether the request that was answered asked for `json_schema`.
    pub json_schema: bool,
    /// Every operation of the reply and what became of it (refused ones included).
    pub ops: Vec<OpReport>,
    /// Set when a reply was read: the signals up to here are reviewed.
    pub reviewed_seq: Option<u64>,
    /// The packet's size (estimated tokens) and budget.
    pub packet_tokens: usize,
    pub budget_tokens: usize,
}

impl ReviewOutcome {
    fn ended(status: ReviewStatus) -> Self {
        Self {
            status,
            requests: 0,
            json_schema: false,
            ops: Vec::new(),
            reviewed_seq: None,
            packet_tokens: 0,
            budget_tokens: 0,
        }
    }

    /// Nothing to review.
    pub fn nothing() -> Self {
        Self::ended(ReviewStatus::NothingToReview)
    }

    /// Not run, and why.
    pub fn skipped(why: impl Into<String>) -> Self {
        Self::ended(ReviewStatus::Skipped(why.into()))
    }

    /// One line per change for the app (Hermes's review summary wording), without repeats.
    pub fn actions(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for line in self.ops.iter().filter_map(OpReport::action_line) {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        lines
    }
}

/// The model the review asks.
pub(crate) struct ReviewModel {
    pub llm: Arc<dyn Provider>,
    pub model: String,
    /// The endpoint, which keys the "refused json_schema" memory.
    pub base_url: String,
    pub context_length: Option<usize>,
    pub effort_params: Option<HashMap<String, Value>>,
}

/// Everything one review needs besides the signals.
pub(crate) struct OnceJob {
    pub model: ReviewModel,
    pub settings: ReviewOnceSettings,
    /// The packet's skills and memory (its signals are the evidence).
    pub input: PacketInput,
    pub apply: ApplyContext,
    /// Where the requests' token usage is recorded (as the agentic review does).
    pub store: Arc<Store>,
}

/// Endpoints (base URL, model) that refused `response_format: json_schema` in this process: only
/// those whose text request then went through (so a 400 for another reason, which the text
/// request gets too, doesn't switch the endpoint to text for good).
static NO_JSON_SCHEMA: LazyLock<Mutex<HashSet<(String, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Room kept in the review model's window for its reply (no `max_tokens` is sent: a cap the
/// model doesn't take is a 400, and the main turns send none either; a window's quarter at most).
pub const REPLY_RESERVE_TOKENS: usize = 16_384;
/// How much of an unreadable reply the repair request quotes back, in (estimated) tokens.
pub const REPAIR_QUOTE_TOKENS: usize = 1_000;
/// One request's wall-clock limit, retries included: a provider that hangs or trickles can't hold
/// the review (and CP7.3's one-review-at-a-time slot) forever.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Tokens the packet's budget leaves for everything else in the window: the longer (text) system
/// prompt, the response schema, the repair turn (the quoted reply and the repair text) and the
/// reply itself.
fn reserved_tokens(memory: bool, user: bool, context_length: Option<usize>) -> usize {
    let system = packet::estimate_tokens(&hermes_prompts::review::evidence_review_prompt(
        memory, user, false,
    ));
    let schema = packet::estimate_tokens(&contract::response_format().to_string());
    let repair = packet::estimate_tokens(&hermes_prompts::review::evidence_review_repair(
        &"x".repeat(MAX_REPAIR_REASON),
    )) + REPAIR_QUOTE_TOKENS;
    let reply = match context_length {
        Some(window) if window > 0 => REPLY_RESERVE_TOKENS.min(window / 4),
        _ => REPLY_RESERVE_TOKENS,
    };
    system + schema + repair + reply
}

/// The longest "why" a repair request quotes (characters).
const MAX_REPAIR_REASON: usize = 300;

/// Whether `error` is a server turning the request down as malformed (most likely the
/// `response_format` field), as opposed to a failure that would hit a text request too.
fn refuses_request(error: &ProviderError) -> bool {
    match error {
        ProviderError::RequestFailed(message) => {
            message.starts_with("Bad request (400)") || message.contains("status 422")
        }
        _ => false,
    }
}

/// The reply's text (thinking left out).
fn reply_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Runs one review of `evidence` (a session's unreviewed signals, oldest first). Never panics on
/// a bad reply; see the module doc for what it sends.
pub(crate) async fn run_review_once(job: OnceJob, evidence: Vec<Signal>) -> ReviewOutcome {
    if evidence.is_empty() {
        return ReviewOutcome::nothing();
    }
    let OnceJob {
        model,
        settings,
        mut input,
        mut apply,
        store,
    } = job;
    input.signals = evidence;
    let enabled = |target: Target| {
        input
            .memory
            .iter()
            .any(|file| file.target == target && file.enabled)
    };
    let (memory_on, user_on) = (enabled(Target::Memory), enabled(Target::User));
    let budget = packet::budget_for(
        settings.packet_tokens,
        model.context_length,
        reserved_tokens(memory_on, user_on, model.context_length),
    );
    let packet = packet::build(&input, budget);
    let mut outcome = ReviewOutcome::ended(ReviewStatus::Done);
    outcome.packet_tokens = packet::estimate_tokens(&packet.text);
    outcome.budget_tokens = budget;
    if packet.signals_shown == 0 {
        outcome.status =
            ReviewStatus::Failed("the packet budget has no room for a single signal".into());
        return outcome;
    }
    apply.shown = packet.skills.clone();
    let key = (model.base_url.clone(), model.model.clone());
    let mut json_schema = match settings.format {
        ReplyFormat::Auto => !NO_JSON_SCHEMA.lock().contains(&key),
        ReplyFormat::JsonSchema => true,
        ReplyFormat::Text => false,
    };
    let mut messages = vec![Message::user().with_text(packet.text.clone())];
    let (mut fell_back, mut repaired) = (false, false);
    let parsed = loop {
        let system =
            hermes_prompts::review::evidence_review_prompt(memory_on, user_on, json_schema);
        let mut config = ModelConfig::new(&model.model).with_context_limit(model.context_length);
        let mut params = model.effort_params.clone().unwrap_or_default();
        if json_schema {
            params.insert("response_format".into(), contract::response_format());
        }
        if !params.is_empty() {
            config = config.with_merged_request_params(params);
        }
        outcome.requests += 1;
        outcome.json_schema = json_schema;
        let sent = tokio::time::timeout(
            REQUEST_TIMEOUT,
            model.llm.complete(&config, &system, &messages, &[]),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ProviderError::RequestFailed(format!(
                "no answer within {} minutes",
                REQUEST_TIMEOUT.as_secs() / 60
            )))
        });
        let reply = match sent {
            Ok((reply, usage)) => {
                record_usage(&store, &apply.session_id, usage).await;
                if fell_back && !json_schema {
                    // The text request went through where the schema one was refused: it was
                    // the schema. Later reviews on this endpoint ask in text at once.
                    NO_JSON_SCHEMA.lock().insert(key.clone());
                }
                reply
            }
            Err(error) if json_schema && !fell_back && refuses_request(&error) => {
                tracing::info!(
                    "review: {} refused the request with response_format json_schema ({error}); asking in text",
                    model.model
                );
                json_schema = false;
                fell_back = true;
                continue;
            }
            Err(error) => {
                tracing::warn!("review request failed: {error}");
                outcome.status =
                    ReviewStatus::Failed(format!("the review request failed: {error}"));
                return outcome;
            }
        };
        let text = reply_text(&reply);
        match contract::parse(&text) {
            Ok(parsed) => break parsed,
            Err(why) if !repaired => {
                tracing::info!("review: reply unreadable ({why}); one repair request");
                repaired = true;
                let shown = packet::cut_to_tokens(&text, REPAIR_QUOTE_TOKENS);
                messages.push(Message::assistant().with_text(if shown.trim().is_empty() {
                    "(empty reply)".to_string()
                } else {
                    shown.to_string()
                }));
                let why: String = why.chars().take(MAX_REPAIR_REASON).collect();
                messages.push(
                    Message::user().with_text(hermes_prompts::review::evidence_review_repair(&why)),
                );
            }
            Err(why) => {
                tracing::warn!(
                    "review: reply unreadable after the repair request ({why}); giving up"
                );
                outcome.status =
                    ReviewStatus::Failed(format!("the reply was unreadable twice: {why}"));
                return outcome;
            }
        }
    };
    let reports = tokio::task::spawn_blocking(move || apply::apply_all(&apply, &parsed))
        .await
        .unwrap_or_else(|e| {
            vec![OpReport {
                section: "reply",
                index: 0,
                what: String::new(),
                result: OpResult::Rejected(format!("applying stopped: {e}")),
            }]
        });
    for report in &reports {
        match &report.result {
            OpResult::Rejected(reason) => tracing::info!(
                "review: {}[{}] {} refused: {reason}",
                report.section,
                report.index,
                report.what
            ),
            OpResult::Staged(_) => tracing::info!("review: {} staged for the user", report.what),
            OpResult::Applied(_) => tracing::info!("review: {} applied", report.what),
        }
    }
    outcome.ops = reports;
    outcome.reviewed_seq = Some(packet.max_seq);
    outcome
}

async fn record_usage(
    store: &Arc<Store>,
    session_id: &str,
    usage: hermes_llm::conversation::token_usage::ProviderUsage,
) {
    let (store, id) = (store.clone(), session_id.to_string());
    let result = tokio::task::spawn_blocking(move || store.record_usage(&id, &usage)).await;
    if let Ok(Err(error)) = result {
        tracing::warn!("review: could not record usage: {error:#}");
    }
}

// ---- gathering the packet's input --------------------------------------------------------

/// The skills a session used, in the order first used: loaded with `skill_view` (SKILL.md, not a
/// supporting file), attached by pre-injection (the `preinject` note), or invoked as
/// `/skill-name`. Compacted-away messages count too: the session used them all the same.
pub fn used_skills(messages: &[Message]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    for message in messages {
        if message.role == Role::User {
            if let Some(list) = message
                .metadata
                .operation_note(crate::preinject::NOTE, "skills")
                .and_then(Value::as_array)
            {
                list.iter().filter_map(Value::as_str).for_each(&mut add);
            }
            if let Some((name, _)) =
                hermes_tools::skills::describe_invocation(&message.as_concat_text())
            {
                add(&name);
            }
            continue;
        }
        for request in message
            .content
            .iter()
            .filter_map(MessageContent::as_tool_request)
        {
            let Ok(call) = &request.tool_call else {
                continue;
            };
            if call.name != "skill_view" {
                continue;
            }
            let args = call.arguments.clone().unwrap_or_default();
            let file = args
                .get("file_path")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if let (Some(name), true) = (args.get("name").and_then(Value::as_str), file.is_empty())
            {
                add(name);
            }
        }
    }
    names
}

/// What the related-skills lookup asks the index: the user's words in the signals (what the
/// lessons are about) and the session's first message, as one query.
pub fn topic(messages: &[Message], evidence: &[Signal]) -> String {
    let mut parts: Vec<String> = evidence
        .iter()
        .flat_map(|s| &s.evidence)
        .filter(|e| e.role == "user")
        .map(|e| e.text.clone())
        .collect();
    if let Some(first) = messages
        .iter()
        .find(|m| m.role == Role::User && !m.as_concat_text().trim().is_empty())
    {
        parts.push(crate::preinject::user_text(first));
    }
    let mut seen = HashSet::new();
    parts.retain(|p| seen.insert(p.clone()));
    parts.join("\n").chars().take(2_000).collect()
}

fn read_skill(skills: &SkillStore, name: &str) -> Option<SkillText> {
    let entry = skills.find_any(name)?;
    let bytes = std::fs::read(entry.skill_md()).ok()?;
    let fingerprint = hermes_tools::skill_manage::fingerprint(&bytes);
    let text = String::from_utf8(bytes).ok()?;
    Some(SkillText {
        name: entry.name,
        text,
        fingerprint,
    })
}

/// The packet's skills and memory, read from disk and the index. Blocking.
pub(crate) fn gather(
    messages: &[Message],
    memory: &MemoryStore,
    skills: &SkillStore,
    index: Option<&LocalIndex>,
    query: IndexQuery,
) -> PacketInput {
    let loaded: Vec<SkillText> = used_skills(messages)
        .iter()
        .filter_map(|name| read_skill(skills, name))
        .collect();
    let mut related = Vec::new();
    if let Some(index) = index {
        let limits = Limits {
            skills: packet::RELATED_SKILLS + loaded.len(),
            memory: 0,
            ..Limits::default()
        };
        match index.query(&query, &limits) {
            Ok(hits) => {
                related = hits
                    .skills
                    .iter()
                    .filter(|hit| !loaded.iter().any(|s| s.name == hit.name))
                    .filter_map(|hit| read_skill(skills, &hit.name))
                    .take(packet::RELATED_SKILLS)
                    .collect();
            }
            Err(error) => tracing::warn!("review: related skills lookup failed: {error:#}"),
        }
    }
    let memory = [Target::Memory, Target::User]
        .into_iter()
        .map(|target| MemoryFile {
            target,
            enabled: memory.enabled(target),
            entries: memory.entries(target),
            usage: memory.usage(target),
        })
        .collect();
    PacketInput {
        signals: Vec::new(),
        loaded,
        related,
        memory,
    }
}
