//! The system prompt, built once per session and then frozen (the prompt-cache invariant: only
//! compaction may rebuild it). Ported from Hermes's `agent/system_prompt.py` and
//! `agent/prompt_builder.py`: three tiers ordered stable → context → volatile so providers with
//! longest-prefix caching reuse as much as possible across sessions.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::text;

const CONTEXT_FILE_MAX_CHARS: usize = 20_000;
const CONTEXT_FILE_WINDOW_FRACTION: f64 = 0.06;
const CONTEXT_FILE_MAX_CEILING: usize = 500_000;
const CHARS_PER_TOKEN: f64 = 4.0;
const TRUNCATE_HEAD_RATIO: f64 = 0.7;
const TRUNCATE_TAIL_RATIO: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    GitBash,
    PowerShell,
}

pub struct SystemPromptInput<'a> {
    pub model: &'a str,
    pub provider: &'a str,
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub scratch_dir: &'a Path,
    pub shell: Shell,
    /// e.g. "11 Pro 10.0.26200".
    pub windows_version: &'a str,
    pub tool_names: &'a [&'a str],
    /// The user's SOUL.md, which replaces the default identity when present.
    pub soul: Option<&'a str>,
    /// Rendered MEMORY.md / USER.md blocks (M3) and skills index (M4).
    pub memory: Option<&'a str>,
    pub skills_index: Option<&'a str>,
    pub context_length: Option<usize>,
    pub now: DateTime<Local>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrompt {
    pub stable: String,
    pub context: String,
    pub volatile: String,
}

impl SystemPrompt {
    pub fn full(&self) -> String {
        [&self.stable, &self.context, &self.volatile]
            .into_iter()
            .filter(|tier| !tier.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

fn join_tier(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn model_matches(model: &str, families: &[&str]) -> bool {
    let model = model.to_lowercase();
    families.iter().any(|family| model.contains(family))
}

fn execution_guidance(tool_names: &[&str]) -> String {
    let mut guidance = text::OPENAI_MODEL_EXECUTION_GUIDANCE.to_string();
    for (line, tools) in text::EXECUTION_GUIDANCE_LINE_TOOLS {
        if !tools.iter().any(|tool| tool_names.contains(tool)) {
            guidance = guidance.replace(line, "");
        }
    }
    guidance
}

fn guidance_parts(input: &SystemPromptInput) -> Vec<Option<String>> {
    let mut parts = Vec::new();
    if input.tool_names.is_empty() {
        return parts;
    }
    parts.push(Some(text::TASK_COMPLETION_GUIDANCE.to_string()));
    parts.push(Some(text::PARALLEL_TOOL_CALL_GUIDANCE.to_string()));
    parts.push(Some(text::STEER_CHANNEL_NOTE.to_string()));
    if model_matches(input.model, text::TOOL_USE_ENFORCEMENT_MODELS) {
        parts.push(Some(text::TOOL_USE_ENFORCEMENT_GUIDANCE.to_string()));
        if model_matches(input.model, &["gemini", "gemma"]) {
            parts.push(Some(text::GOOGLE_MODEL_OPERATIONAL_GUIDANCE.to_string()));
        }
    }
    if model_matches(input.model, text::EXECUTION_GUIDANCE_MODELS) {
        parts.push(Some(execution_guidance(input.tool_names)));
    }
    parts
}

pub fn build_system_prompt(input: &SystemPromptInput) -> SystemPrompt {
    let identity = input
        .soul
        .map(str::trim)
        .filter(|soul| !soul.is_empty())
        .unwrap_or(text::IDENTITY);
    let mut stable = vec![Some(identity.to_string())];
    stable.extend(guidance_parts(input));

    let context = vec![context_files_section(input.cwd, input.context_length)];

    let volatile = vec![
        input.skills_index.map(str::to_string),
        input.memory.map(str::to_string),
        Some(timestamp_line(input)),
        Some(environment_block(input)),
    ];

    SystemPrompt {
        stable: join_tier(&stable),
        context: join_tier(&context),
        volatile: join_tier(&volatile),
    }
}

/// Date only (not time), so the prompt stays byte-stable for the whole day.
fn timestamp_line(input: &SystemPromptInput) -> String {
    format!(
        "Conversation started: {} (UTC{})\nModel: {}\nProvider: {}",
        input.now.format("%A, %B %d, %Y"),
        input.now.format("%:z"),
        input.model,
        input.provider,
    )
}

fn environment_block(input: &SystemPromptInput) -> String {
    let host = [
        format!("Host: Windows ({})", input.windows_version),
        format!("User home directory: {}", input.home.display()),
        format!("Current working directory: {}", input.cwd.display()),
        format!(
            "Scratch directory: {} (TMPDIR points here; write temporary files and probes there, never under the system temp dir)",
            input.scratch_dir.display()
        ),
        "Note: on Windows, the machine hostname (e.g. from `hostname` or uname) is NOT the username. \
         Use the 'User home directory' above to construct paths under C:\\Users\\<user>\\, never the hostname."
            .to_string(),
    ]
    .join("\n");
    let shell = match input.shell {
        Shell::GitBash => text::WINDOWS_BASH_SHELL_HINT,
        Shell::PowerShell => text::WINDOWS_POWERSHELL_SHELL_HINT,
    };
    format!(
        "# Hermes runtime environment\n\n{host}\n\n{shell}\n\n<!-- End Hermes runtime environment -->"
    )
}

// --- Project context files -----------------------------------------------------------------

fn context_file_max_chars(context_length: Option<usize>) -> usize {
    match context_length {
        Some(tokens) if tokens > 0 => {
            let budget = (tokens as f64 * CHARS_PER_TOKEN * CONTEXT_FILE_WINDOW_FRACTION) as usize;
            budget.clamp(CONTEXT_FILE_MAX_CHARS, CONTEXT_FILE_MAX_CEILING)
        }
        _ => CONTEXT_FILE_MAX_CHARS,
    }
}

fn truncate(content: &str, label: &str, path: &Path, max_chars: usize) -> String {
    let total = content.chars().count();
    if total <= max_chars {
        return content.to_string();
    }
    let head_chars = (max_chars as f64 * TRUNCATE_HEAD_RATIO) as usize;
    let tail_chars = (max_chars as f64 * TRUNCATE_TAIL_RATIO) as usize;
    let head: String = content.chars().take(head_chars).collect();
    let tail: String = content.chars().skip(total - tail_chars).collect();
    format!(
        "{head}\n\n[...truncated {label}: kept {head_chars}+{tail_chars} of {total} chars. The middle is omitted. \
         If you need the full instructions, read the complete file with the read_file tool: {}]\n\n{tail}",
        path.display()
    )
}

fn read_trimmed(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let content = content.trim();
    (!content.is_empty()).then(|| content.to_string())
}

fn strip_yaml_frontmatter(content: &str) -> &str {
    content
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("\n---"))
        .map(|(_, body)| body.trim_start_matches(|c| c == '-').trim_start())
        .unwrap_or(content)
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Directories from the git root (or just cwd outside a repo) down to cwd.
fn chain_to_cwd(cwd: &Path) -> Vec<PathBuf> {
    let Some(root) = git_root(cwd) else {
        return vec![cwd.to_path_buf()];
    };
    let mut chain: Vec<PathBuf> = cwd
        .ancestors()
        .take_while(|dir| dir.starts_with(&root))
        .map(Path::to_path_buf)
        .collect();
    chain.reverse();
    chain
}

fn section(label: &str, content: &str, path: &Path, max_chars: usize) -> String {
    truncate(&format!("## {label}\n\n{content}"), label, path, max_chars)
}

fn load_hermes_md(cwd: &Path, max_chars: usize) -> Option<String> {
    let stop = git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    for dir in cwd.ancestors() {
        for name in [".hermes.md", "HERMES.md"] {
            let path = dir.join(name);
            if let Some(content) = read_trimmed(&path) {
                return Some(section(
                    name,
                    strip_yaml_frontmatter(&content),
                    &path,
                    max_chars,
                ));
            }
        }
        if dir == stop {
            break;
        }
    }
    None
}

fn load_agents_md(cwd: &Path, max_chars: usize) -> Option<String> {
    let mut sections = Vec::new();
    let mut seen = Vec::new();
    for dir in chain_to_cwd(cwd) {
        let found = ["AGENTS.override.md", "AGENTS.md", "agents.md"]
            .iter()
            .find_map(|name| {
                read_trimmed(&dir.join(name)).map(|content| (dir.join(name), content))
            });
        if let Some((path, content)) = found {
            if seen.contains(&content) {
                continue;
            }
            let label = path
                .strip_prefix(cwd)
                .ok()
                .filter(|relative| relative.components().count() == 1)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|| path.display().to_string());
            sections.push(section(&label, &content, &path, max_chars));
            seen.push(content);
        }
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn load_claude_md(cwd: &Path, max_chars: usize) -> Option<String> {
    ["CLAUDE.md", "claude.md"].iter().find_map(|name| {
        let path = cwd.join(name);
        read_trimmed(&path).map(|content| section(name, &content, &path, max_chars))
    })
}

fn load_cursorrules(cwd: &Path, max_chars: usize) -> Option<String> {
    let mut content = String::new();
    if let Some(rules) = read_trimmed(&cwd.join(".cursorrules")) {
        content.push_str(&format!("## .cursorrules\n\n{rules}\n\n"));
    }
    if let Ok(entries) = std::fs::read_dir(cwd.join(".cursor").join("rules")) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "mdc"))
            .collect();
        files.sort();
        for path in files {
            if let Some(rules) = read_trimmed(&path) {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                content.push_str(&format!("## .cursor/rules/{name}\n\n{rules}\n\n"));
            }
        }
    }
    (!content.is_empty()).then(|| {
        truncate(
            &content,
            ".cursorrules",
            &cwd.join(".cursorrules"),
            max_chars,
        )
    })
}

/// Only one project context type loads, first found wins: .hermes.md → AGENTS.md chain →
/// CLAUDE.md → .cursorrules.
fn context_files_section(cwd: &Path, context_length: Option<usize>) -> Option<String> {
    let max_chars = context_file_max_chars(context_length);
    let content = load_hermes_md(cwd, max_chars)
        .or_else(|| load_agents_md(cwd, max_chars))
        .or_else(|| load_claude_md(cwd, max_chars))
        .or_else(|| load_cursorrules(cwd, max_chars))?;
    Some(format!(
        "# Project Context\n\nThe following project context files have been loaded and should be followed:\n\n{content}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn input<'a>(cwd: &'a Path, model: &'a str, tools: &'a [&'a str]) -> SystemPromptInput<'a> {
        SystemPromptInput {
            model,
            provider: "openrouter",
            cwd,
            home: Path::new("C:/Users/test"),
            scratch_dir: Path::new("C:/Users/test/AppData/Roaming/hermes-rs/scratch"),
            shell: Shell::GitBash,
            windows_version: "11 Pro 10.0.26200",
            tool_names: tools,
            soul: None,
            memory: None,
            skills_index: None,
            context_length: None,
            now: Local.with_ymd_and_hms(2026, 10, 31, 15, 0, 0).unwrap(),
        }
    }

    #[test]
    fn prompt_is_byte_stable_for_the_same_day() {
        let dir = tempfile::tempdir().unwrap();
        let tools = ["terminal", "read_file"];
        let mut morning = input(dir.path(), "openai/gpt-5", &tools);
        let first = build_system_prompt(&morning).full();
        morning.now = Local.with_ymd_and_hms(2026, 10, 31, 23, 59, 0).unwrap();
        assert_eq!(first, build_system_prompt(&morning).full());
    }

    #[test]
    fn tiers_are_ordered_stable_context_volatile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Use tabs.").unwrap();
        let tools = ["terminal"];
        let prompt = build_system_prompt(&input(dir.path(), "anthropic/claude", &tools));
        let full = prompt.full();
        let identity = full.find("You are Hermes").unwrap();
        let context = full.find("# Project Context").unwrap();
        let environment = full.find("# Hermes runtime environment").unwrap();
        assert!(identity < context && context < environment);
        assert!(prompt.context.contains("Use tabs."));
    }

    #[test]
    fn model_family_guidance_is_gated() {
        let dir = tempfile::tempdir().unwrap();
        let tools = ["terminal"];
        let gpt = build_system_prompt(&input(dir.path(), "openai/gpt-5", &tools)).full();
        let claude =
            build_system_prompt(&input(dir.path(), "anthropic/claude-opus", &tools)).full();
        assert!(gpt.contains("# Tool-use enforcement"));
        assert!(!claude.contains("# Tool-use enforcement"));
    }

    #[test]
    fn execution_guidance_drops_lines_for_missing_tools() {
        let without_terminal = execution_guidance(&["read_file"]);
        assert!(!without_terminal.contains("run `date`"));
        assert!(without_terminal.contains("use read_file, search_files, or terminal"));
        assert!(execution_guidance(&["terminal"]).contains("run `date`"));
    }

    #[test]
    fn hermes_md_wins_over_agents_md_and_strips_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "agents").unwrap();
        std::fs::write(
            dir.path().join(".hermes.md"),
            "---\nkey: v\n---\nhermes rules",
        )
        .unwrap();
        let section = context_files_section(dir.path(), None).unwrap();
        assert!(section.contains("hermes rules"));
        assert!(!section.contains("key: v"));
        assert!(!section.contains("agents"));
    }

    #[test]
    fn agents_md_chain_merges_from_git_root_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let sub = dir.path().join("crates").join("app");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(sub.join("AGENTS.md"), "app rules").unwrap();
        let section = context_files_section(&sub, None).unwrap();
        let root = section.find("root rules").unwrap();
        let app = section.find("app rules").unwrap();
        assert!(root < app);
    }

    #[test]
    fn long_context_files_keep_head_and_tail() {
        let content = format!("HEAD{}TAIL", "x".repeat(30_000));
        let truncated = truncate(&content, "AGENTS.md", Path::new("AGENTS.md"), 20_000);
        assert!(truncated.starts_with("HEAD"));
        assert!(truncated.ends_with("TAIL"));
        assert!(truncated.contains("[...truncated AGENTS.md"));
        assert!(truncated.len() < 20_500);
    }

    #[test]
    fn soul_replaces_default_identity() {
        let dir = tempfile::tempdir().unwrap();
        let tools = ["terminal"];
        let mut with_soul = input(dir.path(), "m", &tools);
        with_soul.soul = Some("You are Ada.");
        let prompt = build_system_prompt(&with_soul).full();
        assert!(prompt.starts_with("You are Ada."));
        assert!(!prompt.contains("You are Hermes"));
    }
}
