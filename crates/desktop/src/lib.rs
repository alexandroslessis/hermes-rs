//! The hermes-rs desktop backend as a library: the page's server, the gateway, the ACP agent.
//! Two executables use it: `hermes-desktop` (the Tauri window) and `hermes-acp` (a console program
//! for editors, `src/bin/hermes-acp.rs`).

pub mod acp;
pub mod archive;
pub mod attachments;
pub mod aux_models;
pub mod branch;
pub mod commands;
pub mod endpoints;
pub mod export;
pub mod files;
pub mod gateway;
pub mod git;
pub mod git_review;
pub mod hermes_import;
pub mod hub;
pub mod learning;
pub mod maintenance;
pub mod open_file;
pub mod paths;
pub mod profiles;
pub mod projects;
pub mod providers;
pub mod rest;
pub mod server;
pub mod skills;
pub mod terminal;
pub mod toolsets;
pub mod transcript;
pub mod workspace;

/// Whether Settings' config record asks for Git Bash (`terminal.shell: "bash"`).
pub fn wants_git_bash(data_dir: &std::path::Path) -> bool {
    std::fs::read(data_dir.join("desktop-config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|config| config["terminal"]["shell"] == "bash")
}
