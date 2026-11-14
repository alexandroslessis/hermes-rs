//! Prompt text, embedded at compile time. Hermes originals live in `prompts/hermes/` (MIT,
//! see `licenses/MIT-hermes-agent.txt`); our adapted texts live directly in `prompts/`.

macro_rules! hermes {
    ($module:literal, $name:literal) => {
        include_str!(concat!(
            "../../../prompts/hermes/",
            $module,
            "/",
            $name,
            ".txt"
        ))
    };
}

/// Hermes's default identity with the "built by Nous Research" line adapted to this app.
pub const IDENTITY: &str = include_str!("../../../prompts/identity.md");

pub const TASK_COMPLETION_GUIDANCE: &str =
    hermes!("agent.prompt_builder", "TASK_COMPLETION_GUIDANCE");
pub const PARALLEL_TOOL_CALL_GUIDANCE: &str =
    hermes!("agent.prompt_builder", "PARALLEL_TOOL_CALL_GUIDANCE");
/// Not in Hermes (it has no script tool): added after Hermes's parallel-calls text when `run_script`
/// is offered (Phase 10). In every profile: it carries no number and is frozen with the prompt.
pub const BATCHING_GUIDANCE: &str = "When a task needs several independent reads, searches or commands, issue them as parallel tool calls in one response, or do them in one `run_script` that prints only what you need. `read_file` takes several files in one call (`paths`).";
pub const SKILLS_GUIDANCE: &str =
    "When you work out a non-trivial workflow, record it with skill_manage for future reuse.";
pub const SESSION_SEARCH_GUIDANCE: &str = "When the user references something from a past conversation or you suspect relevant cross-session context exists, use session_search to recall it before asking them to repeat themselves.";
/// Explains the `/steer` marker; steering lands after tool results, so only sessions with tools
/// get it.
pub const STEER_CHANNEL_NOTE: &str = hermes!("agent.prompt_builder", "STEER_CHANNEL_NOTE");
pub const STEER_MARKER_OPEN: &str = hermes!("agent.prompt_builder", "STEER_MARKER_OPEN");
pub const STEER_MARKER_CLOSE: &str = "[/OUT-OF-BAND USER MESSAGE]";
pub const TOOL_USE_ENFORCEMENT_GUIDANCE: &str =
    hermes!("agent.prompt_builder", "TOOL_USE_ENFORCEMENT_GUIDANCE");
pub const GOOGLE_MODEL_OPERATIONAL_GUIDANCE: &str =
    hermes!("agent.prompt_builder", "GOOGLE_MODEL_OPERATIONAL_GUIDANCE");
pub const OPENAI_MODEL_EXECUTION_GUIDANCE: &str =
    hermes!("agent.prompt_builder", "OPENAI_MODEL_EXECUTION_GUIDANCE");
pub const WINDOWS_BASH_SHELL_HINT: &str =
    hermes!("agent.prompt_builder", "WINDOWS_BASH_SHELL_HINT");

/// Models that get `TOOL_USE_ENFORCEMENT_GUIDANCE` (substring match on the model id).
pub const TOOL_USE_ENFORCEMENT_MODELS: &[&str] = &[
    "gpt", "codex", "gemini", "gemma", "grok", "glm", "qwen", "deepseek", "muse",
];

/// Models that get `OPENAI_MODEL_EXECUTION_GUIDANCE` (substring match on the model id).
pub const EXECUTION_GUIDANCE_MODELS: &[&str] = &[
    "gpt", "codex", "grok", "deepseek", "kimi", "qwen", "glm", "minimax", "mimo", "mistral", "muse",
];

/// Lines of `OPENAI_MODEL_EXECUTION_GUIDANCE` that point at specific tools; a line is dropped
/// when none of its tools is available, so the prompt never steers toward a missing tool.
pub const EXECUTION_GUIDANCE_LINE_TOOLS: &[(&str, &[&str])] = &[
    (
        "- Arithmetic, math, calculations → use terminal or execute_code\n",
        &["terminal", "execute_code"],
    ),
    (
        "- Hashes, encodings, checksums → use terminal (e.g. sha256sum, base64)\n",
        &["terminal"],
    ),
    (
        "- Current time, date, timezone → use terminal (e.g. date)\n",
        &["terminal"],
    ),
    (
        "- System state: OS, CPU, memory, disk, ports, processes → use terminal\n",
        &["terminal"],
    ),
    (
        "- File contents, sizes, line counts → use read_file, search_files, or terminal\n",
        &["read_file", "search_files", "terminal"],
    ),
    (
        "- Git history, branches, diffs → use terminal\n",
        &["terminal"],
    ),
    (
        "- 'What time is it?' → run `date` (don't guess)\n",
        &["terminal"],
    ),
];

/// Shell hint when Git Bash isn't installed and the terminal tool falls back to PowerShell.
pub const WINDOWS_POWERSHELL_SHELL_HINT: &str = "Shell: on this Windows host your `terminal` tool \
runs commands through Windows PowerShell 5.1, not bash. Use PowerShell syntax: `$env:FOO` for \
environment variables, `;` to chain commands (`&&` and `||` are not available), `Get-ChildItem`, \
`Select-String`. Each command runs in a fresh PowerShell: the working directory carries over \
between calls, but variables and `$env:` changes do not, so set them in the same command that \
needs them. Prefer the read_file, search_files, patch and write_file tools over shell commands \
for files.";
