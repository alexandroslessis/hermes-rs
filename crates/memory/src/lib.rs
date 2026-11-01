//! Persistent curated memory with Hermes's semantics: `MEMORY.md` (the agent's notes) and
//! `USER.md` (the user profile), the `memory` tool, the threat scan and write approvals.

mod store;
pub mod threats;
mod tool;

pub use store::{
    ENTRY_DELIMITER, MemoryConfig, MemorySnapshot, MemoryStore, Operation, Target, TurnFailures,
};
pub use tool::PendingWrite;
