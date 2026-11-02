//! One-way import from a Hermes install (`HERMES_HOME`, else `~/.hermes`): its memory entries,
//! the skills the user made or installed, and `SOUL.md`. Nothing there is changed.

use std::path::{Path, PathBuf};

use gpui::App;
use hermes_memory::{ENTRY_DELIMITER, MemoryStore, Operation, Target, TurnFailures};

use crate::backend::backend;

/// Hermes's home folder, when there is one.
pub fn hermes_home() -> Option<PathBuf> {
    let home = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".hermes")))?;
    home.is_dir().then_some(home)
}

/// Adds the entries of Hermes's `MEMORY.md` / `USER.md` that aren't here yet. Returns how many
/// were added, or what went wrong per file.
pub fn import_memory(home: &Path, store: &MemoryStore) -> Result<usize, Vec<String>> {
    let dir = home.join("memories");
    let mut added = 0;
    let mut problems = Vec::new();
    for target in [Target::User, Target::Memory] {
        let Ok(raw) = std::fs::read_to_string(dir.join(target.file_name())) else {
            continue;
        };
        let existing = store.entries(target);
        let ops: Vec<Operation> = raw
            .trim_start_matches('\u{feff}')
            .split(ENTRY_DELIMITER)
            .map(str::trim)
            .filter(|entry| !entry.is_empty() && !existing.iter().any(|e| e == entry))
            .map(|entry| Operation {
                action: "add".into(),
                content: Some(entry.to_string()),
                ..Default::default()
            })
            .collect();
        if ops.is_empty() {
            continue;
        }
        let result = store.apply(target, &ops, &mut TurnFailures::default());
        if result["success"] == serde_json::json!(true) {
            added += ops.len();
        } else {
            problems.push(format!(
                "{}: {}",
                target.file_name(),
                result["error"].as_str().unwrap_or("failed")
            ));
        }
    }
    if problems.is_empty() {
        Ok(added)
    } else {
        Err(problems)
    }
}

/// Copies Hermes's `SOUL.md` when this app has none. `Ok(true)` when copied.
pub fn import_soul(home: &Path, data_dir: &Path) -> Result<bool, String> {
    let source = home.join("SOUL.md");
    let target = data_dir.join("SOUL.md");
    let Ok(text) = std::fs::read_to_string(&source) else {
        return Ok(false);
    };
    if target.exists() || text.trim().is_empty() {
        return Ok(false);
    }
    if let Some(threat) =
        hermes_memory::threats::first_threat_message(&text, hermes_memory::threats::Scope::All)
    {
        return Err(format!("SOUL.md was not copied: {threat}"));
    }
    std::fs::write(&target, text)
        .map(|()| true)
        .map_err(|e| format!("SOUL.md: {e}"))
}

/// Everything at once (Settings → Import from Hermes): a one-paragraph report, and whether
/// anything went wrong.
pub fn import_all(cx: &App) -> (String, bool) {
    let Some(home) = hermes_home() else {
        return ("No Hermes install found (~/.hermes or HERMES_HOME).".into(), true);
    };
    let backend = backend(cx);
    let mut lines = Vec::new();
    let mut failed = false;
    match import_memory(&home, &backend.runtime.memory()) {
        Ok(added) => lines.push(format!("Memory: {added} new entries.")),
        Err(problems) => {
            failed = true;
            lines.push(format!("Memory: {}", problems.join("; ")));
        }
    }
    let skills = backend.runtime.skills().import_from(&home.join("skills"));
    let mut line = format!("Skills: {} imported", skills.imported.len());
    if !skills.existing.is_empty() {
        line += &format!(", {} already here", skills.existing.len());
    }
    line.push('.');
    for (name, why) in &skills.failed {
        failed = true;
        line += &format!(" {name} skipped — {why}");
    }
    lines.push(line);
    match import_soul(&home, &backend.data_dir) {
        Ok(true) => lines.push("SOUL.md copied.".into()),
        Ok(false) => {}
        Err(error) => {
            failed = true;
            lines.push(error);
        }
    }
    (lines.join("\n"), failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soul_is_copied_only_when_missing() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        assert_eq!(import_soul(home.path(), data.path()), Ok(false));
        std::fs::write(home.path().join("SOUL.md"), "You are terse.").unwrap();
        assert_eq!(import_soul(home.path(), data.path()), Ok(true));
        assert_eq!(
            std::fs::read_to_string(data.path().join("SOUL.md")).unwrap(),
            "You are terse."
        );
        std::fs::write(home.path().join("SOUL.md"), "Something else.").unwrap();
        assert_eq!(import_soul(home.path(), data.path()), Ok(false));
    }
}
