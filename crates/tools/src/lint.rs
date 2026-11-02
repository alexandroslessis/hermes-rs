//! Hermes's post-write syntax lint (`tools/file_operations_lint.py`, MIT): after `write_file` /
//! `patch`, JSON, YAML and TOML are parsed in-process and Python and JavaScript go through
//! `python -m py_compile` / `node --check` when those are installed. When the file was already
//! broken, only the errors this edit introduced are reported.

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lint {
    Ok,
    Error(String),
    /// No linter for the file type, or it couldn't run.
    Skipped(String),
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// `LINTERS_INPROC`.
fn inproc(extension: &str, content: &str) -> Option<Result<(), String>> {
    Some(match extension {
        "json" => serde_json::from_str::<Value>(content)
            .map(|_| ())
            .map_err(|e| format!("JSONDecodeError: {e}")),
        "yaml" | "yml" => serde_yaml::Deserializer::from_str(content)
            .try_for_each(|document| {
                <serde_yaml::Value as serde::Deserialize>::deserialize(document).map(|_| ())
            })
            .map_err(|e| format!("YAMLError: {e}")),
        "toml" => toml::from_str::<toml::Table>(content)
            .map(|_| ())
            .map_err(|e| format!("TOMLDecodeError: {}", e.to_string().trim())),
        _ => return None,
    })
}

/// Runs a linter on a file; `None` when the program isn't installed or couldn't run.
fn run(program: &str, args: &[&str], file: &Path) -> Option<(bool, String)> {
    use std::os::windows::process::CommandExt as _;
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x0800_0000)
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    };
    let mut output = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut output);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut output);
    }
    // The Windows Store's `python` stub isn't a Python.
    if output.contains("Python was not found") || status.code() == Some(9009) {
        return None;
    }
    Some((status.success(), output.trim().to_string()))
}

/// `_check_lint`: the file's syntax as it is on disk (`content` is what was written).
pub fn check(path: &Path, content: &str) -> Lint {
    let extension = extension(path);
    if let Some(result) = inproc(&extension, content) {
        return match result {
            Ok(()) => Lint::Ok,
            Err(error) => Lint::Error(error),
        };
    }
    let outcome = match extension.as_str() {
        "py" => run("python", &["-m", "py_compile"], path),
        "js" | "mjs" | "cjs" => run("node", &["--check"], path),
        _ => return Lint::Skipped(format!("No linter for .{extension} files")),
    };
    match outcome {
        Some((true, _)) => Lint::Ok,
        Some((false, output)) => Lint::Error(output),
        None => Lint::Skipped("linter not available".into()),
    }
}

/// `_check_lint_delta` as the tool result's `lint` field: `None` when the file type has no
/// linter. With the old content known, errors that were already there are filtered out.
pub fn delta(path: &Path, pre: Option<&str>, post: &str) -> Option<Value> {
    let after = check(path, post);
    let Lint::Error(post_output) = after else {
        return match after {
            Lint::Ok => Some(json!({"status": "ok", "output": ""})),
            _ => None,
        };
    };
    let pre_output = pre.and_then(|pre| {
        let extension = extension(path);
        let before = match inproc(&extension, pre) {
            Some(result) => result.err(),
            None => {
                // A shell linter needs a file: the old content in a temporary twin.
                let dir = tempfile::tempdir().ok()?;
                let twin = dir.path().join(path.file_name()?);
                std::fs::write(&twin, pre).ok()?;
                match check(&twin, pre) {
                    Lint::Error(output) => Some(
                        output.replace(&twin.display().to_string(), &path.display().to_string()),
                    ),
                    _ => None,
                }
            }
        };
        before
    });
    let Some(pre_output) = pre_output else {
        return Some(json!({"status": "error", "output": post_output}));
    };
    let pre_lines: std::collections::HashSet<&str> = pre_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let new: Vec<&str> = post_output
        .lines()
        .filter(|l| !l.trim().is_empty() && !pre_lines.contains(l.trim()))
        .collect();
    Some(if new.is_empty() {
        json!({"status": "error", "output": post_output,
               "message": "Pre-existing lint errors — this edit didn't introduce new ones but the file is still broken."})
    } else {
        json!({"status": "error", "output": format!(
            "New lint errors introduced by this edit (pre-existing errors filtered out):\n{}",
            new.join("\n")
        )})
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_formats_are_checked_in_process() {
        let path = Path::new("c.json");
        assert_eq!(
            delta(path, None, "{\"a\": 1}"),
            Some(json!({"status": "ok", "output": ""}))
        );
        let broken = delta(path, None, "{\"a\": }").unwrap();
        assert_eq!(broken["status"], "error");
        assert!(
            broken["output"]
                .as_str()
                .unwrap()
                .starts_with("JSONDecodeError")
        );
        assert!(matches!(
            check(Path::new("c.toml"), "a = \n"),
            Lint::Error(_)
        ));
        assert!(matches!(
            check(Path::new("c.yaml"), "a: [1, 2\n"),
            Lint::Error(_)
        ));
        assert!(delta(Path::new("c.md"), None, "# hi").is_none());
        // Still broken the same way: nothing new.
        let again = delta(path, Some("{\"a\": }"), "{\"a\": }").unwrap();
        assert!(
            again["message"]
                .as_str()
                .unwrap()
                .starts_with("Pre-existing")
        );
    }
}
