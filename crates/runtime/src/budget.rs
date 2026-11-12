//! Budget profiles: one setting, `requests | balanced | tokens`, sets a bundle of knobs that
//! decide how many model requests, retries and tokens the backend spends.
//!
//! Not in Hermes (it has no profiles): `docs/backend/PHASES.md`, Phase 3. `balanced` is exactly
//! the behaviour before profiles existed (every value below is the constant it replaced, and a
//! unit test pins them), so a user can switch back to it. `requests` is the default profile
//! ([`BudgetProfile::default`]); `tokens` spends requests freely and saves tokens.
//!
//! The knobs are read **once per turn**: `Runtime::begin_turn` copies [`BudgetKnobs`] out of the
//! runtime's config and threads the copy to every step it builds, so a change applies from the
//! next turn. Anything that changes the system prompt or the tool list (`skills_index`,
//! `tool_schemas`) applies from the next *session* (prompt caching, ground rule 8).
//!
//! A knob whose `requests` or `tokens` value needs a later phase (review `idle_once`, skills
//! preload, heuristic titles, per-cluster curator, deferred tool schemas) holds its `balanced`
//! value until that phase lands: the enum has the variant, [`for_profile`] says which phase flips
//! it in a comment, and nothing consumes the later variant yet.

use serde::{Deserialize, Serialize};

/// The three bundles. `Requests` is the default (decision D1 of the plan): new installs and
/// configs without the key get it, once the config key exists (CP3.2).
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

/// When the background review runs. Phase 7 adds `idle_once` as the `requests` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// Counter-based nudges (today).
    Nudges,
    /// One review when the session goes idle (Phase 7).
    IdleOnce,
}

/// How the curator consolidates. Phase 8 adds one-shot-per-cluster as the `requests` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuratorMode {
    /// One agent loop over all candidates, up to `curator_max_iterations` calls (today).
    Loop,
    /// One request per cluster of related skills (Phase 8).
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

/// How session titles are made. Phase 4 adds the heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMode {
    /// A model request (today).
    Model,
    /// From the first message, no request (Phase 4).
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
    /// Model calls one curator pass may make (when `curator_mode` is `Loop`).
    pub curator_max_iterations: u32,
    pub curator_mode: CuratorMode,
    pub skills_index: SkillsIndex,
    /// Skills loaded before the first model call (Phase 5); 0 = none.
    pub skills_preload: u32,
    pub titles: TitleMode,
    pub tool_schemas: ToolSchemas,
}

impl BudgetKnobs {
    /// Today's behaviour. Each value is the constant it replaced (a test pins them).
    pub const BALANCED: BudgetKnobs = BudgetKnobs {
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
        curator_max_iterations: 200,
        curator_mode: CuratorMode::Loop,
        skills_index: SkillsIndex::Full,
        skills_preload: 0,
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
            // Phase 7: ReviewMode::IdleOnce.
            review_mode: balanced.review_mode,
            // Phase 8: CuratorMode::PerCluster.
            curator_max_iterations: balanced.curator_max_iterations,
            curator_mode: balanced.curator_mode,
            // Phase 5: SkillsIndex::CategoriesAndRetrieval, preload 3.
            skills_index: balanced.skills_index,
            skills_preload: balanced.skills_preload,
            // Phase 4: TitleMode::Heuristic.
            titles: balanced.titles,
            tool_schemas: balanced.tool_schemas,
        },
        BudgetProfile::Tokens => BudgetKnobs {
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
        assert_eq!(k.delegate_max_children, 10); // delegate.rs MAX_CONCURRENT_CHILDREN
        assert_eq!(k.delegate_child_iterations, 250); // delegate.rs CHILD_MAX_ITERATIONS
        assert_eq!(k.review_mode, ReviewMode::Nudges);
        assert_eq!(k.curator_max_iterations, 200); // curator.rs CURATOR_MAX_ITERATIONS
        assert_eq!(k.curator_mode, CuratorMode::Loop);
        assert_eq!(k.skills_index, SkillsIndex::Full);
        assert_eq!(k.skills_preload, 0);
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
        // Later phases: still balanced.
        let b = BudgetKnobs::BALANCED;
        assert_eq!(
            (
                r.review_mode,
                r.curator_mode,
                r.skills_index,
                r.skills_preload,
                r.titles,
                r.tool_schemas
            ),
            (
                b.review_mode,
                b.curator_mode,
                b.skills_index,
                b.skills_preload,
                b.titles,
                b.tool_schemas
            )
        );

        let t = for_profile(BudgetProfile::Tokens);
        assert_eq!(
            (t.compress_at.small_window, t.compress_at.large_window),
            (0.60, 0.40)
        );
        assert_eq!(t.result_size_pct, 50);
        assert_eq!(t.skills_index, SkillsIndex::NamesOnly);
        // Everything else is today's.
        assert_eq!(
            BudgetKnobs {
                compress_at: b.compress_at,
                result_size_pct: 100,
                skills_index: SkillsIndex::Full,
                ..t
            },
            b
        );
    }
}
