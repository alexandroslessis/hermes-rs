//! Prompt text and tool schemas embedded from `prompts/`, and the system-prompt builder.

pub mod coding;
pub mod compress;
pub mod review;
pub mod side;
pub mod system;
pub mod text;
pub mod titles;
pub mod tools;

pub use system::{
    BUDGET_GUIDANCE_REQUESTS, BUDGET_GUIDANCE_TOKENS, Shell, SystemPrompt, SystemPromptInput,
    build_system_prompt,
};
