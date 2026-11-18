//! Running system `git` without a window, a prompt or stdin (the workspace probe here; the coding
//! rail's reads in `crates/desktop/src/git.rs` until CP1.2 moves them). A folder that isn't a
//! repository answers empty.
//!
//! Hermes: `hermes_cli/web_git.py` (`_run_git`).

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// `CREATE_NO_WINDOW`: no console flashes for a child process.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `(succeeded, stdout)` of `git args` in `cwd`.
pub fn git(cwd: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        Err(_) => (false, String::new()),
    }
}

/// `git args`' output, trimmed; empty when it failed.
pub fn git_line(cwd: &Path, args: &[&str]) -> String {
    let (ok, out) = git(cwd, args);
    if ok {
        out.trim().to_string()
    } else {
        String::new()
    }
}
