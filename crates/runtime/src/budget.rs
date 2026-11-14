//! Budget profiles: one setting, `requests | balanced | tokens`, sets a bundle of knobs that
//! decide how many model requests, retries and tokens the backend spends.
//!
//! Not in Hermes (it has no profiles): `docs/backend/PHASES.md`, Phase 3. `balanced` is exactly
//! the behaviour before profiles existed (every value below is the constant it replaced, and a
//! unit test pins them), so a user can switch back to it. `requests` is the default profile
//! ([`BudgetProfile::default`]); `tokens` spends requests freely and saves tokens.
//!
//! The knobs are read **once per turn**: `Runtime::begin_turn` copies [`BudgetKnobs`] out of the
//! runtime (`Runtime::budget`, set from the config's `budget` by the desktop) and threads the copy
//! to every step it builds, so a change applies from the next turn. Anything that changes the
//! system prompt or the tool list (the budget wording, `skills_index`, `tool_schemas`) applies
//! from the next *session* (prompt caching, ground rule 8); a compaction or a model switch, which
//! rebuild the prompt anyway, take the profile the session runs under then.
//!
//! A knob whose `requests` or `tokens` value needs a later phase (deferred
//! tool schemas) holds its `balanced`
//! value until that phase lands: the enum has the variant, [`for_profile`] says which phase flips
//! it in a comment, and nothing consumes the later variant yet.

use serde::{Deserialize, Serialize};

/// The three bundles. `Requests` is the default (decision D1 of the plan): new installs and
/// configs without the `budget.profile` key get it ([`from_config`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetProfile {
    /// Fewest requests per task: tighter retry and iteration caps, bigger tool results, whole
    /// files per read, late compression.
    #[default]
    Requests,
    /// Today's behaviour, kept as the comparison point.
    Balanced,
    /// Fewest tokens per task: smaller results, early compression.
    Tokens,
}

impl BudgetProfile {
    pub const ALL: [BudgetProfile; 3] = [Self::Requests, Self::Balanced, Self::Tokens];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requests => "requests",
            Self::Balanced => "balanced",
            Self::Tokens => "tokens",
        }
    }

    /// `None` for anything that isn't a profile name (the caller picks the default).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "requests" => Some(Self::Requests),
            "balanced" => Some(Self::Balanced),
            "tokens" => Some(Self::Tokens),
            _ => None,
        }
    }
}

/// When the background review runs, and which one (Phase 7, `review_schedule.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// Hermes's counters (every 10 user turns for memory, every 10 model calls without a
    /// `skill_manage` for skills) start the agentic review (`review.rs`: the conversation replayed
    /// to a forked agent with tools), and so does a learning-signal score at the threshold. The
    /// `balanced` and `tokens` value.
    Nudges,
    /// The evidence review (`review_once.rs`: one request, no tools), once, when the session has
    /// been idle for `review_idle_minutes` or was closed or switched away from, if its signals
    /// reached the threshold. The `requests` value.
    IdleOnce,
    /// The review exactly as before Phase 7: the counters and the agentic review, no signals. Kept
    /// for comparison only (`budget.review_mode: agentic`); to be deleted a few weeks after Phase 7
    /// lands.
    Agentic,
}

impl ReviewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nudges => "nudges",
            Self::IdleOnce => "idle_once",
            Self::Agentic => "agentic",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "nudges" => Some(Self::Nudges),
            "idle_once" => Some(Self::IdleOnce),
            "agentic" => Some(Self::Agentic),
            _ => None,
        }
    }
}

/// How the curator consolidates (`curator.rs`): the agent loop of today, or (Phase 8) one request
/// per cluster of near-duplicate skills and per low performer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuratorMode {
    /// One agent loop over all candidates, up to `curator_max_iterations` calls (today).
    Loop,
    /// One request per cluster of near-duplicate skills or low-performing skill, the answer a
    /// proposal in the staging area (`curator/`). The `requests` value.
    PerCluster,
}

/// What the system prompt's skills block lists. Applies from the next session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsIndex {
    /// Every skill with its description (today).
    Full,
    /// Categories, with retrieval for the rest (Phase 5).
    CategoriesAndRetrieval,
    /// Names only.
    NamesOnly,
}

/// How session titles are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMode {
    /// A model request (today).
    Model,
    /// From the first message, no request ([`crate::titles::heuristic_title`]). Used only while no
    /// auxiliary title model is set; with one, the model's title is the upgrade.
    Heuristic,
}

/// Which tool schemas ride in the request. Applies from the next session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSchemas {
    /// Every tool's schema (today).
    All,
    /// Core tools plus a loader for the rest (Phase 11).
    Deferred,
}

/// Fractions of the context window at which the conversation is compressed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressAt {
    /// For windows under 512k tokens.
    pub small_window: f64,
    /// For windows of 512k tokens and more.
    pub large_window: f64,
}

/// Every knob of the table in the plan. `Copy`: a turn takes a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetKnobs {
    /// The profile these knobs started from (per-knob overrides keep it). The system prompt's
    /// budget wording follows it, chosen when a session starts.
    pub profile: BudgetProfile,
    /// Model calls per user message before the toolless summary call.
    pub max_iterations: u32,
    /// The inference step's own retries of an empty reply.
    pub empty_response_retries: usize,
    /// Nudges to continue an answer the output limit cut off.
    pub length_continuations: u32,
    /// Batches with only invented tool names before the turn gives up.
    pub invalid_tool_retries: u32,
    /// Re-asks of a tool call the output limit cut off.
    pub truncated_tool_retries: u32,
    /// Retries of a failed HTTP request (a daily-limit 429 is never retried).
    pub http_retries: usize,
    /// Compression thresholds.
    pub compress_at: CompressAt,
    /// A tool result's size limit as a percentage of the standard limit (100 = unchanged).
    pub result_size_pct: u32,
    /// `read_file`: lines per read, both the default and the ceiling.
    pub read_file_max_lines: usize,
    /// `search_files`: a requested context is multiplied by this (and the ceiling with it).
    pub search_context_factor: usize,
    /// `delegate_task`: subagents running in parallel.
    pub delegate_max_children: usize,
    /// `delegate_task`: each child's iteration budget.
    pub delegate_child_iterations: u32,
    pub review_mode: ReviewMode,
    /// The learning-signal score at which a session is worth a review (`signals.rs`; an explicit
    /// "remember this" or `/learn` always is).
    pub review_threshold: f32,
    /// `idle_once`: minutes without activity before the session's review runs.
    pub review_idle_minutes: f64,
    /// `idle_once`: evidence reviews the scheduler starts per hour, across all sessions (an app
    /// start with many sessions waiting can't send a burst of requests).
    pub review_per_hour: u32,
    /// Model calls one curator pass may make (when `curator_mode` is `Loop`).
    pub curator_max_iterations: u32,
    pub curator_mode: CuratorMode,
    /// `curator_mode` `PerCluster`: clusters and repairs one run may send a request for (each
    /// may take one more request to repair an unreadable reply).
    pub curator_max_requests: u32,
    pub skills_index: SkillsIndex,
    /// Skills attached to the user's message by the local index (Phase 5): the top k, 0 = none.
    pub skills_preload: u32,
    /// Memory entries attached the same way (top k), 0 = none.
    pub memory_preload: u32,
    /// The first user message of a session inside a git repository carries a repository overview
    /// (file tree, project docs, git state) in its `<context>` block (Phase 10, `context_pack.rs`).
    pub context_pack: bool,
    pub titles: TitleMode,
    pub tool_schemas: ToolSchemas,
}

impl BudgetKnobs {
    /// Whether a side job with no model of its own (the smart-approval judge) may borrow the
    /// session's main model. Under `requests` it may not: the main provider's quota is for the
    /// main turns (Phase 4), so the flagged command asks the user instead.
    pub fn side_jobs_may_use_main_model(&self) -> bool {
        self.profile != BudgetProfile::Requests
    }

    /// Today's behaviour. Each value is the constant it replaced (a test pins them).
    pub const BALANCED: BudgetKnobs = BudgetKnobs {
        profile: BudgetProfile::Balanced,
        max_iterations: 500,
        empty_response_retries: 3,
        length_continuations: 4,
        invalid_tool_retries: 3,
        truncated_tool_retries: 4,
        http_retries: 3,
        compress_at: CompressAt {
            small_window: 0.75,
            large_window: 0.50,
        },
        result_size_pct: 100,
        read_file_max_lines: 2000,
        search_context_factor: 1,
        delegate_max_children: 10,
        delegate_child_iterations: 250,
        review_mode: ReviewMode::Nudges,
        review_threshold: crate::signals::REVIEW_THRESHOLD,
        review_idle_minutes: 10.0,
        review_per_hour: 6,
        curator_max_iterations: 200,
        curator_mode: CuratorMode::Loop,
        curator_max_requests: 3,
        skills_index: SkillsIndex::Full,
        skills_preload: 0,
        memory_preload: 0,
        context_pack: false,
        titles: TitleMode::Model,
        tool_schemas: ToolSchemas::All,
    };

    pub fn for_profile(profile: BudgetProfile) -> Self {
        for_profile(profile)
    }
}

impl Default for BudgetKnobs {
    /// The default profile's knobs.
    fn default() -> Self {
        for_profile(BudgetProfile::default())
    }
}

/// The knob table as code. Rows marked "Phase N" keep the `balanced` value until that phase
/// lands (see the module doc).
pub fn for_profile(profile: BudgetProfile) -> BudgetKnobs {
    let balanced = BudgetKnobs::BALANCED;
    match profile {
        BudgetProfile::Balanced => balanced,
        BudgetProfile::Requests => BudgetKnobs {
            profile: BudgetProfile::Requests,
            max_iterations: 150,
            empty_response_retries: 1,
            length_continuations: 2,
            invalid_tool_retries: 2,
            truncated_tool_retries: 2,
            // 3 retries; a daily-limit 429 is not retried at all (the retry layer already
            // refuses those), so this is the balanced value.
            http_retries: 3,
            compress_at: CompressAt {
                small_window: 0.90,
                large_window: 0.85,
            },
            result_size_pct: 400,
            read_file_max_lines: 4000,
            search_context_factor: 2,
            delegate_max_children: 3,
            delegate_child_iterations: 60,
            // Phase 7: one evidence review when the session goes idle.
            review_mode: ReviewMode::IdleOnce,
            review_threshold: balanced.review_threshold,
            review_idle_minutes: balanced.review_idle_minutes,
            review_per_hour: balanced.review_per_hour,
            // Phase 8: one request per cluster or repair, up to 3 a run.
            curator_max_iterations: balanced.curator_max_iterations,
            curator_mode: CuratorMode::PerCluster,
            curator_max_requests: 3,
            // Phase 5: the system prompt lists categories; the top 3 skills and top 3 memory
            // entries for each message ride on it (the local index).
            skills_index: SkillsIndex::CategoriesAndRetrieval,
            skills_preload: 3,
            memory_preload: 3,
            // Phase 10: the first message of a session in a repository carries its overview.
            context_pack: true,
            // Phase 4: titles from the first message, no request.
            titles: TitleMode::Heuristic,
            tool_schemas: balanced.tool_schemas,
        },
        BudgetProfile::Tokens => BudgetKnobs {
            profile: BudgetProfile::Tokens,
            compress_at: CompressAt {
                small_window: 0.60,
                large_window: 0.40,
            },
            result_size_pct: 50,
            skills_index: SkillsIndex::NamesOnly,
            // Phase 11: ToolSchemas::Deferred.
            tool_schemas: balanced.tool_schemas,
            ..balanced
        },
    }
}

// --- Config ---------------------------------------------------------------------------------

/// What `budget.*` in the config can set: the profile, and each of these numbers on top of it
/// (`budget.max_iterations: 100`). Names are the knob's field names.
pub const OVERRIDE_KEYS: [&str; 23] = [
    "skills_preload",
    "memory_preload",
    "context_pack",
    "max_iterations",
    "empty_response_retries",
    "length_continuations",
    "invalid_tool_retries",
    "truncated_tool_retries",
    "http_retries",
    "compress_at_small_window",
    "compress_at_large_window",
    "result_size_pct",
    "read_file_max_lines",
    "search_context_factor",
    "delegate_max_children",
    "delegate_child_iterations",
    "curator_max_iterations",
    "curator_mode",
    "curator_max_requests",
    "review_mode",
    "review_threshold",
    "review_idle_minutes",
    "review_per_hour",
];

/// Checks one `budget.<key>` value and applies it to `knobs`. `null` clears an override (nothing
/// to apply). Out-of-range values are refused, not clamped: 0 for the result size, the
/// continuation and strike caps and the iteration budgets would persist every result to disk, or
/// end a turn on its first strike, so it is never a valid override (0 stays valid for the two
/// retry counts, where it means "don't retry").
pub fn apply_override(
    knobs: &mut BudgetKnobs,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    fn count(key: &str, value: &serde_json::Value, min: u64, max: u64) -> Result<u64, String> {
        value
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| format!("budget.{key} must be a whole number from {min} to {max}"))
    }
    fn fraction(key: &str, value: &serde_json::Value) -> Result<f64, String> {
        value
            .as_f64()
            .filter(|f| (0.2..=0.95).contains(f))
            .ok_or_else(|| format!("budget.{key} must be a number from 0.2 to 0.95"))
    }
    match key {
        "max_iterations" => knobs.max_iterations = count(key, value, 1, 10_000)? as u32,
        "empty_response_retries" => {
            knobs.empty_response_retries = count(key, value, 0, 20)? as usize
        }
        "length_continuations" => knobs.length_continuations = count(key, value, 1, 50)? as u32,
        "invalid_tool_retries" => knobs.invalid_tool_retries = count(key, value, 1, 50)? as u32,
        "truncated_tool_retries" => knobs.truncated_tool_retries = count(key, value, 1, 50)? as u32,
        "http_retries" => knobs.http_retries = count(key, value, 0, 20)? as usize,
        "compress_at_small_window" => knobs.compress_at.small_window = fraction(key, value)?,
        "compress_at_large_window" => knobs.compress_at.large_window = fraction(key, value)?,
        // At most `requests`' ×4: the base is 15% of the window, so ×4 is 60% of it already,
        // and more would let one tool result fill the context on its own.
        "result_size_pct" => knobs.result_size_pct = count(key, value, 1, 400)? as u32,
        "read_file_max_lines" => {
            knobs.read_file_max_lines = count(key, value, 1, 100_000)? as usize
        }
        "search_context_factor" => knobs.search_context_factor = count(key, value, 1, 10)? as usize,
        "delegate_max_children" => knobs.delegate_max_children = count(key, value, 1, 50)? as usize,
        "delegate_child_iterations" => {
            knobs.delegate_child_iterations = count(key, value, 1, 10_000)? as u32
        }
        "curator_max_iterations" => {
            knobs.curator_max_iterations = count(key, value, 1, 10_000)? as u32
        }
        // "" is the profile's own (no override).
        "curator_mode" if value.as_str().is_some_and(|v| v.trim().is_empty()) => {}
        "curator_mode" => {
            knobs.curator_mode = match value.as_str().map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("loop") => CuratorMode::Loop,
                Some("per_cluster") => CuratorMode::PerCluster,
                _ => return Err("budget.curator_mode must be loop or per_cluster".to_string()),
            }
        }
        "curator_max_requests" => knobs.curator_max_requests = count(key, value, 1, 20)? as u32,
        // "" is the Settings select's "the profile's own" (no override).
        "review_mode" if value.as_str().is_some_and(|v| v.trim().is_empty()) => {}
        "review_mode" => {
            knobs.review_mode = value
                .as_str()
                .and_then(ReviewMode::parse)
                .ok_or_else(|| "budget.review_mode must be idle_once, nudges or agentic".to_string())?
        }
        // Below 1 one correction would start a review; above 50 only an explicit request would.
        "review_threshold" => {
            knobs.review_threshold = value
                .as_f64()
                .filter(|f| (1.0..=50.0).contains(f))
                .ok_or_else(|| "budget.review_threshold must be a number from 1 to 50".to_string())?
                as f32
        }
        // Fractions are fine (0.05 = 3 seconds, for a live check); a day at most.
        "review_idle_minutes" => {
            knobs.review_idle_minutes = value
                .as_f64()
                .filter(|f| (0.05..=1440.0).contains(f))
                .ok_or_else(|| {
                    "budget.review_idle_minutes must be a number from 0.05 to 1440".to_string()
                })?
        }
        "review_per_hour" => knobs.review_per_hour = count(key, value, 1, 60)? as u32,
        // 0 switches the injection off (balanced's value): a valid choice.
        "skills_preload" => knobs.skills_preload = count(key, value, 0, 10)? as u32,
        "memory_preload" => knobs.memory_preload = count(key, value, 0, 20)? as u32,
        // true / false; "" is the Settings select's "the profile's own" (no override).
        "context_pack" if value.as_str().is_some_and(|v| v.trim().is_empty()) => {}
        "context_pack" => {
            knobs.context_pack = value
                .as_bool()
                .ok_or_else(|| "budget.context_pack must be true or false".to_string())?
        }
        _ => return Err(format!("budget.{key} is not a budget setting")),
    }
    Ok(())
}

/// `config.set budget.*`: whether the key and value are acceptable (`key` without `budget.`).
pub fn check_config_value(key: &str, value: &serde_json::Value) -> Result<(), String> {
    if key == "profile" {
        return value
            .as_str()
            .and_then(BudgetProfile::parse)
            .map(|_| ())
            .ok_or_else(|| "budget.profile must be requests, balanced or tokens".to_string());
    }
    let mut scratch = BudgetKnobs::BALANCED;
    apply_override(&mut scratch, key, value)
}

/// The knobs a config's `budget` object asks for: its profile (anything missing or unknown is
/// the default, `requests`: existing configs have no such key), then each valid override on
/// top. An invalid override is ignored, so a hand-edited file can't switch a safety cap off.
pub fn from_config(budget: &serde_json::Value) -> BudgetKnobs {
    let profile = budget["profile"]
        .as_str()
        .and_then(BudgetProfile::parse)
        .unwrap_or_default();
    let mut knobs = for_profile(profile);
    if let Some(object) = budget.as_object() {
        for key in OVERRIDE_KEYS {
            if let Some(value) = object.get(key) {
                let _ = apply_override(&mut knobs, key, value);
            }
        }
    }
    knobs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Switch back to today" stays exact: these are the constants the knobs replaced, written
    /// out (not read from `BALANCED`, which is what is under test).
    #[test]
    fn balanced_is_todays_constants() {
        let k = for_profile(BudgetProfile::Balanced);
        assert_eq!(k.max_iterations, 500); // RuntimeConfig::for_current_user
        assert_eq!(k.empty_response_retries, 3); // inference.rs MAX_EMPTY_RESPONSE_RETRIES
        assert_eq!(k.length_continuations, 4); // ops.rs MAX_LENGTH_CONTINUATIONS
        assert_eq!(k.invalid_tool_retries, 3); // ops.rs MAX_INVALID_TOOL_RETRIES
        assert_eq!(k.truncated_tool_retries, 4); // ops.rs MAX_TRUNCATED_TOOL_RETRIES
        assert_eq!(k.http_retries, 3); // retry.rs DEFAULT_MAX_RETRIES
        assert_eq!(k.compress_at.small_window, 0.75); // compress.rs
        assert_eq!(k.compress_at.large_window, 0.50);
        assert_eq!(k.result_size_pct, 100); // ops.rs result_size_limit
        assert_eq!(k.read_file_max_lines, 2000); // files.rs MAX_LINES
        assert_eq!(k.search_context_factor, 1); // search.rs: context as asked
        assert_eq!(k.delegate_max_children, 10); // delegate.rs MAX_CONCURRENT_CHILDREN (gone: the page is told the effective one)
        assert_eq!(k.delegate_child_iterations, 250); // delegate.rs CHILD_MAX_ITERATIONS
        // Phase 7: the counters as before (plus the signal score as one more trigger).
        assert_eq!(k.review_mode, ReviewMode::Nudges);
        assert_eq!(k.review_threshold, 3.0);
        assert_eq!(k.review_idle_minutes, 10.0);
        assert_eq!(k.review_per_hour, 6);
        assert_eq!(k.curator_max_iterations, 200); // curator.rs CURATOR_MAX_ITERATIONS
        assert_eq!(k.curator_mode, CuratorMode::Loop);
        assert_eq!(k.curator_max_requests, 3); // only read by `PerCluster`
        assert_eq!(k.skills_index, SkillsIndex::Full);
        assert_eq!(k.skills_preload, 0);
        assert_eq!(k.memory_preload, 0);
        assert!(!k.context_pack); // Phase 10: not in today's behaviour
        assert_eq!(k.titles, TitleMode::Model);
        assert_eq!(k.tool_schemas, ToolSchemas::All);
    }

    #[test]
    fn the_default_profile_is_requests() {
        assert_eq!(BudgetProfile::default(), BudgetProfile::Requests);
        assert_eq!(BudgetKnobs::default(), for_profile(BudgetProfile::Requests));
    }

    #[test]
    fn profile_names_round_trip() {
        for profile in BudgetProfile::ALL {
            assert_eq!(BudgetProfile::parse(profile.as_str()), Some(profile));
        }
        assert_eq!(
            BudgetProfile::parse(" Tokens "),
            Some(BudgetProfile::Tokens)
        );
        assert_eq!(BudgetProfile::parse("fast"), None);
    }

    /// The table's other columns, and the fallbacks for knobs that wait for a later phase.
    #[test]
    fn requests_and_tokens_follow_the_table() {
        let r = for_profile(BudgetProfile::Requests);
        assert_eq!(
            (
                r.max_iterations,
                r.empty_response_retries,
                r.length_continuations
            ),
            (150, 1, 2)
        );
        assert_eq!(
            (
                r.invalid_tool_retries,
                r.truncated_tool_retries,
                r.http_retries
            ),
            (2, 2, 3)
        );
        assert_eq!(
            (r.compress_at.small_window, r.compress_at.large_window),
            (0.90, 0.85)
        );
        assert_eq!(
            (
                r.result_size_pct,
                r.read_file_max_lines,
                r.search_context_factor
            ),
            (400, 4000, 2)
        );
        assert_eq!(
            (r.delegate_max_children, r.delegate_child_iterations),
            (3, 60)
        );
        // Phase 4: heuristic titles, and no borrowing the main model for side jobs.
        assert_eq!(r.titles, TitleMode::Heuristic);
        assert!(!r.side_jobs_may_use_main_model());
        assert!(BudgetKnobs::BALANCED.side_jobs_may_use_main_model());
        // Phase 5: categories in the prompt, the top 3 skills and memory entries on the message.
        assert_eq!(r.skills_index, SkillsIndex::CategoriesAndRetrieval);
        assert_eq!((r.skills_preload, r.memory_preload), (3, 3));
        // Phase 10: the first message of a session in a repository carries its overview.
        assert!(r.context_pack);
        assert!(!for_profile(BudgetProfile::Tokens).context_pack);
        // Phase 7: the evidence review at idle.
        assert_eq!(r.review_mode, ReviewMode::IdleOnce);
        // Phase 8: the curator asks once per cluster or repair, 3 a run at most.
        assert_eq!((r.curator_mode, r.curator_max_requests), (CuratorMode::PerCluster, 3));
        // Later phases: still balanced.
        let b = BudgetKnobs::BALANCED;
        assert_eq!(r.tool_schemas, b.tool_schemas);

        let t = for_profile(BudgetProfile::Tokens);
        assert_eq!(
            (t.compress_at.small_window, t.compress_at.large_window),
            (0.60, 0.40)
        );
        assert_eq!(t.result_size_pct, 50);
        assert_eq!(t.skills_index, SkillsIndex::NamesOnly);
        assert_eq!((t.skills_preload, t.memory_preload), (0, 0));
        // Everything else is today's.
        assert_eq!(
            BudgetKnobs {
                profile: BudgetProfile::Balanced,
                compress_at: b.compress_at,
                result_size_pct: 100,
                skills_index: SkillsIndex::Full,
                ..t
            },
            b
        );
    }

    #[test]
    fn the_preload_knobs_can_be_switched_off_or_raised() {
        use serde_json::json;
        assert!(check_config_value("skills_preload", &json!(0)).is_ok());
        assert!(check_config_value("skills_preload", &json!(11)).is_err());
        assert!(check_config_value("memory_preload", &json!(-1)).is_err());
        let knobs = from_config(&json!({"profile": "requests", "skills_preload": 0, "memory_preload": 5}));
        assert_eq!((knobs.skills_preload, knobs.memory_preload), (0, 5));
        let knobs = from_config(&json!({"profile": "balanced", "skills_preload": 2}));
        assert_eq!(knobs.skills_preload, 2);
    }

    #[test]
    fn the_context_pack_knob_is_a_validated_boolean_that_overrides_the_profile() {
        use serde_json::json;
        assert!(check_config_value("context_pack", &json!(true)).is_ok());
        assert!(check_config_value("context_pack", &json!(false)).is_ok());
        assert!(check_config_value("context_pack", &json!("")).is_ok());
        assert!(check_config_value("context_pack", &json!(null)).is_ok());
        for bad in [json!(1), json!("yes"), json!(0.5), json!([true])] {
            assert!(check_config_value("context_pack", &bad).is_err(), "{bad}");
        }
        assert!(!from_config(&json!({"profile": "requests", "context_pack": false})).context_pack);
        assert!(from_config(&json!({"profile": "balanced", "context_pack": true})).context_pack);
        assert!(from_config(&json!({"profile": "tokens", "context_pack": true})).context_pack);
        // A bad value in a hand-edited file leaves the profile's.
        assert!(from_config(&json!({"profile": "requests", "context_pack": "yes"})).context_pack);
        assert!(!from_config(&json!({"profile": "balanced", "context_pack": 1})).context_pack);
        assert!(from_config(&json!({"profile": "requests", "context_pack": ""})).context_pack);
    }

    #[test]
    fn the_review_knobs_are_validated() {
        use serde_json::json;
        for mode in ["idle_once", "nudges", "agentic", " Agentic "] {
            assert!(check_config_value("review_mode", &json!(mode)).is_ok(), "{mode}");
        }
        assert!(check_config_value("review_mode", &json!("never")).is_err());
        // The Settings select's empty choice: the profile's own mode.
        assert!(check_config_value("review_mode", &json!("")).is_ok());
        assert_eq!(
            from_config(&json!({"profile": "requests", "review_mode": ""})).review_mode,
            ReviewMode::IdleOnce
        );
        assert!(check_config_value("review_mode", &json!(1)).is_err());
        assert!(check_config_value("review_threshold", &json!(0.5)).is_err());
        assert!(check_config_value("review_threshold", &json!(4.5)).is_ok());
        assert!(check_config_value("review_idle_minutes", &json!(0)).is_err());
        assert!(check_config_value("review_idle_minutes", &json!(0.05)).is_ok());
        assert!(check_config_value("review_idle_minutes", &json!(2000)).is_err());
        assert!(check_config_value("review_per_hour", &json!(0)).is_err());
        let knobs = from_config(&json!({
            "profile": "requests",
            "review_mode": "agentic",
            "review_threshold": 5,
            "review_idle_minutes": 0.5,
            "review_per_hour": 2,
        }));
        assert_eq!(knobs.review_mode, ReviewMode::Agentic);
        assert_eq!(
            (knobs.review_threshold, knobs.review_idle_minutes, knobs.review_per_hour),
            (5.0, 0.5, 2)
        );
        // A bad value in a hand-edited file leaves the profile's.
        let knobs = from_config(&json!({"profile": "balanced", "review_mode": "never"}));
        assert_eq!(knobs.review_mode, ReviewMode::Nudges);
        assert_eq!(from_config(&json!({"profile": "tokens"})).review_mode, ReviewMode::Nudges);
    }

    #[test]
    fn the_curator_knobs_are_validated_and_can_opt_a_profile_in_or_out() {
        use serde_json::json;
        for mode in ["loop", "per_cluster", " Loop ", ""] {
            assert!(check_config_value("curator_mode", &json!(mode)).is_ok(), "{mode}");
        }
        assert!(check_config_value("curator_mode", &json!("agentic")).is_err());
        assert!(check_config_value("curator_mode", &json!(1)).is_err());
        assert!(check_config_value("curator_max_requests", &json!(0)).is_err());
        assert!(check_config_value("curator_max_requests", &json!(21)).is_err());
        assert!(check_config_value("curator_max_requests", &json!(5)).is_ok());
        // balanced and tokens keep the agent loop unless the user opts in; requests can opt out.
        for profile in ["balanced", "tokens"] {
            assert_eq!(from_config(&json!({"profile": profile})).curator_mode, CuratorMode::Loop);
            let knobs = from_config(&json!({
                "profile": profile, "curator_mode": "per_cluster", "curator_max_requests": 2
            }));
            assert_eq!((knobs.curator_mode, knobs.curator_max_requests), (CuratorMode::PerCluster, 2));
        }
        let knobs = from_config(&json!({"profile": "requests", "curator_mode": "loop"}));
        assert_eq!(knobs.curator_mode, CuratorMode::Loop);
        assert_eq!(
            from_config(&json!({"profile": "requests", "curator_mode": ""})).curator_mode,
            CuratorMode::PerCluster
        );
    }

    #[test]
    fn a_config_without_the_key_gets_requests() {
        use serde_json::json;
        for config in [
            json!(null),
            json!({}),
            json!({"profile": "fast"}),
            json!({"profile": 3}),
        ] {
            assert_eq!(from_config(&config), for_profile(BudgetProfile::Requests));
        }
        assert_eq!(
            from_config(&json!({"profile": "tokens"})),
            for_profile(BudgetProfile::Tokens)
        );
        assert_eq!(
            from_config(&json!({"profile": "balanced"})),
            BudgetKnobs::BALANCED
        );
    }

    #[test]
    fn overrides_sit_on_top_of_the_profile() {
        use serde_json::json;
        let knobs = from_config(&json!({
            "profile": "balanced",
            "max_iterations": 42,
            "compress_at_small_window": 0.8,
            "http_retries": 0,
        }));
        assert_eq!(knobs.profile, BudgetProfile::Balanced);
        assert_eq!(knobs.max_iterations, 42);
        assert_eq!(knobs.compress_at.small_window, 0.8);
        assert_eq!(knobs.compress_at.large_window, 0.50);
        assert_eq!(knobs.http_retries, 0);
        assert_eq!(knobs.read_file_max_lines, 2000);
    }

    #[test]
    fn zero_is_refused_for_the_caps_that_would_break_a_turn() {
        use serde_json::json;
        for key in [
            "max_iterations",
            "length_continuations",
            "invalid_tool_retries",
            "truncated_tool_retries",
            "result_size_pct",
            "read_file_max_lines",
            "search_context_factor",
            "delegate_max_children",
            "delegate_child_iterations",
            "curator_max_iterations",
            "curator_max_requests",
        ] {
            assert!(check_config_value(key, &json!(0)).is_err(), "{key}");
            // And from a hand-edited file: ignored, the profile value stays.
            let knobs = from_config(&json!({"profile": "balanced", key: 0}));
            assert_eq!(knobs, BudgetKnobs::BALANCED, "{key}");
        }
        // 0 means "don't retry" for these two.
        assert!(check_config_value("empty_response_retries", &json!(0)).is_ok());
        assert!(check_config_value("http_retries", &json!(0)).is_ok());
        // Other shapes: negative, fractional, text, a fraction out of range, an unknown key.
        assert!(check_config_value("max_iterations", &json!(-1)).is_err());
        assert!(check_config_value("max_iterations", &json!(1.5)).is_err());
        assert!(check_config_value("max_iterations", &json!("9")).is_err());
        assert!(check_config_value("compress_at_small_window", &json!(1.0)).is_err());
        // Ceilings: one result may not outgrow requests' ×4 (60% of the window).
        assert!(check_config_value("result_size_pct", &json!(400)).is_ok());
        assert!(check_config_value("result_size_pct", &json!(401)).is_err());
        assert!(check_config_value("nonsense", &json!(1)).is_err());
        assert!(check_config_value("profile", &json!("fast")).is_err());
        assert!(check_config_value("profile", &json!("tokens")).is_ok());
        // null clears an override.
        assert!(check_config_value("max_iterations", &json!(null)).is_ok());
    }
}
