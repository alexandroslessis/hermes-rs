//! Persistent curated memory with Hermes's semantics: `MEMORY.md` (the agent's notes) and
//! `USER.md` (the user profile), the `memory` tool, the threat scan and write approvals.

mod db;
mod notes;
mod store;
mod sync;
#[cfg(test)]
mod sync_tests;
pub mod threats;
mod tiers;
#[cfg(test)]
mod tiers_tests;
mod tool;

pub use store::{
    ENTRY_DELIMITER, MemoryConfig, MemorySnapshot, MemoryStore, Operation, Target, TurnFailures,
};
pub use tiers::{
    Extras, PROJECT_LIMIT_DEFAULT, PROJECT_LIMIT_MAX, PROJECT_LIMIT_MIN, Tier,
};
pub use tool::{Parsed, PendingWrite, ReadCall};
