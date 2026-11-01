//! Hermes's progressive subdirectory hints (`agent/subdirectory_hints.py`, MIT): when a tool call
//! first touches a directory below the working folder, the directory's AGENTS.md / CLAUDE.md /
//! .cursorrules is appended to the tool result. Context arrives without touching the system
//! prompt, so the prompt cache stays valid. The working folder's own file is in the system prompt
//! already.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Priority order, first match per directory (`_HINT_FILENAMES`).
const HINT_FILENAMES: &[&str] = &[
    "AGENTS.override.md",
    "AGENTS.md",
    "agents.md",
    "CLAUDE.md",
    "claude.md",
    ".cursorrules",
];
/// Per-file ceiling (`_MAX_HINT_CHARS`, Codex's `project_doc_max_bytes`).
const MAX_HINT_CHARS: usize = 32_000;
const PATH_ARG_KEYS: &[&str] = &["path", "file_path", "workdir"];
/// Ancestor levels walked per path (`_MAX_ANCESTOR_WALK`).
const MAX_ANCESTOR_WALK: usize = 5;
/// Dependency, build and cache trees hold copies of context files, never authoritative ones
/// (`SEARCH_PRUNE_DIR_NAMES`).
pub(crate) const EXCLUDED_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "venv",
    ".venv",
    "site-packages",
    "dist-packages",
    "vendor",
    "third_party",
    "build",
    "dist",
    "target",
    "out",
    "coverage",
    ".next",
    ".turbo",
    ".parcel-cache",
    ".nuxt",
    ".svelte-kit",
    ".archive",
    "__pycache__",
    ".cache",
    ".Trash",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".npm",
    ".yarn",
    ".pnpm-store",
    ".gradle",
    ".m2",
    ".nuget",
    "backups",
    "backup",
    ".backups",
];
/// Commands whose operand is a directory even without a `/` or `.` in it.
const NAV_COMMANDS: &[&str] = &["cd", "pushd", "set-location", "sl", "chdir"];
const SHELL_OPERATORS: &[&str] = &["&&", "||", "|", ";", "&", "(", ")"];

/// The canonical form of an existing path, without Windows's `\\?\` prefix.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(path).ok()?;
    let text = resolved.to_string_lossy();
    Some(match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => resolved,
    })
}

/// Whether `path` is `root` or below it (Windows paths compare case-insensitively).
pub(crate) fn is_under(path: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    let (path, root) = (norm(path), norm(root));
    path == root || path.starts_with(&format!("{root}/"))
}

fn digest(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Splits a command into words, keeping quoted strings whole and shell operators apart.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    let flush = |current: &mut String, words: &mut Vec<String>| {
        if !current.is_empty() {
            words.push(std::mem::take(current));
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => flush(&mut current, &mut words),
            None if matches!(c, ';' | '|' | '&' | '(' | ')') => {
                flush(&mut current, &mut words);
                let mut op = c.to_string();
                if let Some(&next) = chars.peek()
                    && (c == '&' || c == '|')
                    && next == c
                {
                    op.push(next);
                    chars.next();
                }
                words.push(op);
            }
            None => current.push(c),
        }
    }
    flush(&mut current, &mut words);
    words
}

/// Operands of `cd`/`pushd` (and PowerShell's `Set-Location`) that begin a shell segment.
fn nav_targets(words: &[String]) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut segment_start = true;
    for (index, word) in words.iter().enumerate() {
        if SHELL_OPERATORS.contains(&word.as_str()) {
            segment_start = true;
            continue;
        }
        if segment_start
            && NAV_COMMANDS.contains(&word.to_lowercase().as_str())
            && let Some(operand) = words[index + 1..]
                .iter()
                .find(|w| SHELL_OPERATORS.contains(&w.as_str()) || !w.starts_with('-'))
            && !SHELL_OPERATORS.contains(&operand.as_str())
        {
            targets.push(operand.as_str());
        }
        segment_start = false;
    }
    targets
}

/// Tracks which directories the agent has visited and returns their hint files on first visit.
pub struct HintTracker {
    working_dir: PathBuf,
    home: PathBuf,
    /// The working folder is home (no project chosen): every file under home would qualify.
    disabled: bool,
    loaded_dirs: HashSet<String>,
    /// The same file reached through links or copies is sent once.
    loaded_digests: HashSet<u64>,
}

impl HintTracker {
    pub fn new(working_dir: &Path, home: &Path) -> Self {
        let working_dir = canonical(working_dir).unwrap_or_else(|| working_dir.to_path_buf());
        let disabled = canonical(home).is_some_and(|home| is_under(&home, &working_dir));
        let mut tracker = Self {
            loaded_dirs: HashSet::from([key(&working_dir)]),
            loaded_digests: HashSet::new(),
            working_dir,
            home: home.to_path_buf(),
            disabled,
        };
        // The system prompt already carries the working folder's file.
        if let Some((_, content)) = tracker.first_hint_file(&tracker.working_dir.clone()) {
            tracker.loaded_digests.insert(digest(&content));
        }
        tracker
    }

    /// The hints for directories this call touches for the first time, to append to its result.
    pub fn check(&mut self, tool: &str, args: &Value, shell_cwd: &Path) -> Option<String> {
        if self.disabled {
            return None;
        }
        let mut candidates = Vec::new();
        for key in PATH_ARG_KEYS {
            if let Some(path) = args.get(key).and_then(Value::as_str)
                && !path.trim().is_empty()
            {
                self.add_candidate(path, shell_cwd, &mut candidates);
            }
        }
        if tool == "terminal"
            && let Some(command) = args.get("command").and_then(Value::as_str)
        {
            let words = words(command);
            for target in nav_targets(&words) {
                self.add_candidate(target, shell_cwd, &mut candidates);
            }
            for word in &words {
                // Path-like words only: something with a separator or a dot, not a flag or URL.
                let path_like = word.contains(['/', '\\', '.']);
                if !path_like
                    || word.starts_with('-')
                    || ["http://", "https://", "git@"]
                        .iter()
                        .any(|scheme| word.starts_with(scheme))
                {
                    continue;
                }
                self.add_candidate(word, shell_cwd, &mut candidates);
            }
        }
        let hints: Vec<String> = candidates
            .into_iter()
            .filter_map(|dir| self.load(&dir))
            .collect();
        (!hints.is_empty()).then(|| format!("\n\n{}", hints.join("\n\n")))
    }

    /// The path's directory and its ancestors (up to five, stopping at a loaded one), so reading
    /// `project/src/main.rs` also finds `project/AGENTS.md`.
    fn add_candidate(&self, raw: &str, shell_cwd: &Path, candidates: &mut Vec<PathBuf>) {
        let path = crate::paths::resolve(raw, shell_cwd, &self.home);
        let Some(mut dir) = canonical(&path) else {
            return;
        };
        if dir.is_file() {
            let Some(parent) = dir.parent() else {
                return;
            };
            dir = parent.to_path_buf();
        }
        for _ in 0..MAX_ANCESTOR_WALK {
            if self.loaded_dirs.contains(&key(&dir)) {
                break;
            }
            if self.valid_subdir(&dir) && !candidates.contains(&dir) {
                candidates.push(dir.clone());
            }
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => break,
            }
        }
    }

    fn valid_subdir(&self, dir: &Path) -> bool {
        if !dir.is_dir() || !is_under(dir, &self.working_dir) {
            return false;
        }
        // An excluded segment below the working folder (working inside `vendor/` itself is fine).
        let relative = dir
            .strip_prefix(&self.working_dir)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        !relative.components().any(|part| {
            let name = part.as_os_str().to_string_lossy().to_lowercase();
            EXCLUDED_DIR_NAMES.iter().any(|excluded| *excluded == name)
        })
    }

    /// The first non-empty hint file in `dir` that resolves inside the working folder and isn't
    /// a secret (a checked-in `AGENTS.md` symlink to a credentials file must not load).
    fn first_hint_file(&self, dir: &Path) -> Option<(PathBuf, String)> {
        HINT_FILENAMES.iter().find_map(|name| {
            let path = dir.join(name);
            if !path.is_file() {
                return None;
            }
            let target = canonical(&path)?;
            if !is_under(&target, &self.working_dir) || crate::redact::is_secret_file(&target) {
                return None;
            }
            let content = std::fs::read_to_string(&target).ok()?;
            let content = content.trim_start_matches('\u{feff}').trim();
            (!content.is_empty()).then(|| (path, content.to_string()))
        })
    }

    fn load(&mut self, dir: &Path) -> Option<String> {
        self.loaded_dirs.insert(key(dir));
        let (path, content) = self.first_hint_file(dir)?;
        if !self.loaded_digests.insert(digest(&content)) {
            return None;
        }
        let shown = path
            .strip_prefix(&self.working_dir)
            .map(crate::display_path)
            .unwrap_or_else(|_| crate::display_path(&path));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Same injection scan as the startup context files.
        let content = hermes_prompts::system::scan_context(&content, &name);
        let content =
            hermes_prompts::system::truncate(&content, &name, Path::new(&shown), MAX_HINT_CHARS);
        Some(format!(
            "[Subdirectory context discovered: {shown}]\n{content}"
        ))
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::write(dir.path().join("app/AGENTS.md"), "app rules").unwrap();
        std::fs::write(dir.path().join("app/src/main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join("node_modules/x/AGENTS.md"), "copy").unwrap();
        dir
    }

    #[test]
    fn a_file_in_a_subdirectory_loads_its_hint_once() {
        let dir = project();
        let home = tempfile::tempdir().unwrap();
        let mut tracker = HintTracker::new(dir.path(), home.path());
        let args = json!({"path": "app/src/main.rs"});
        let hint = tracker.check("read_file", &args, dir.path()).unwrap();
        assert!(hint.contains("[Subdirectory context discovered: app/AGENTS.md]\napp rules"));
        assert!(!hint.contains("root rules"));
        assert!(tracker.check("read_file", &args, dir.path()).is_none());
    }

    #[test]
    fn cd_operands_and_excluded_trees() {
        let dir = project();
        let home = tempfile::tempdir().unwrap();
        let mut tracker = HintTracker::new(dir.path(), home.path());
        let args = json!({"command": "cd node_modules/x && ls"});
        assert!(tracker.check("terminal", &args, dir.path()).is_none());
        let args = json!({"command": "cd app; cargo build"});
        assert!(
            tracker
                .check("terminal", &args, dir.path())
                .unwrap()
                .contains("app rules")
        );
    }

    #[test]
    fn home_as_the_working_folder_loads_nothing() {
        let dir = project();
        let mut tracker = HintTracker::new(dir.path(), dir.path());
        let args = json!({"path": "app/src/main.rs"});
        assert!(tracker.check("read_file", &args, dir.path()).is_none());
    }
}
