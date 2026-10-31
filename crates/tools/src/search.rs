//! search_files: file-name globs and regex content search over a gitignore-aware walk.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{SessionTools, ToolOutput, display_path, parse_args};

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MATCH_LINE_CHARS: usize = 500;

#[derive(Deserialize)]
struct SearchArgs {
    pattern: String,
    #[serde(default = "default_target")]
    target: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    file_glob: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    order: Option<String>,
    #[serde(default)]
    output_mode: Option<String>,
    #[serde(default)]
    context: Option<usize>,
}

fn default_target() -> String {
    "content".into()
}

/// Paths under the working directory are shown relative to it, others absolute.
fn shown(path: &Path, cwd: &Path) -> String {
    display_path(path.strip_prefix(cwd).unwrap_or(path))
}

fn walker(root: &Path, glob: Option<&str>) -> Result<ignore::Walk, String> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .parents(true);
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(root);
        overrides
            .case_insensitive(true)
            .map_err(|error| error.to_string())?;
        let pattern = if glob.contains('/') {
            glob.to_string()
        } else {
            format!("**/{glob}")
        };
        overrides
            .add(&pattern)
            .map_err(|error| format!("Invalid glob '{glob}': {error}"))?;
        builder.overrides(overrides.build().map_err(|error| error.to_string())?);
    }
    Ok(builder.build())
}

fn files_under(root: &Path, glob: Option<&str>) -> Result<impl Iterator<Item = PathBuf>, String> {
    Ok(walker(root, glob)?
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path()))
}

pub async fn search_files(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: SearchArgs = match parse_args("search_files", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let cwd = tools.cwd();
    let root = tools.resolve(args.path.as_deref().unwrap_or("."));
    if !root.exists() {
        return ToolOutput::error(format!("Path not found: {}", display_path(&root)));
    }
    let limit = args.limit.unwrap_or(50).clamp(1, 1000);
    let offset = args.offset.unwrap_or(0);
    let task = tokio::task::spawn_blocking(move || match args.target.as_str() {
        "files" => search_names(
            &root,
            &cwd,
            &args.pattern,
            args.order.as_deref(),
            offset,
            limit,
        ),
        "content" => search_content(
            &root,
            &cwd,
            &args.pattern,
            args.file_glob.as_deref(),
            args.output_mode.as_deref().unwrap_or("content"),
            args.context.unwrap_or(0).min(10),
            offset,
            limit,
        ),
        other => ToolOutput::error(format!(
            "Unknown target '{other}'; use 'content' or 'files'."
        )),
    });
    task.await
        .unwrap_or_else(|error| ToolOutput::error(format!("search failed: {error}")))
}

fn search_names(
    root: &Path,
    cwd: &Path,
    pattern: &str,
    order: Option<&str>,
    offset: usize,
    limit: usize,
) -> ToolOutput {
    let glob = if pattern.contains(['*', '?', '[']) {
        pattern.to_string()
    } else {
        format!("*{pattern}*")
    };
    let files = match files_under(root, Some(&glob)) {
        Ok(files) => files,
        Err(error) => return ToolOutput::error(error),
    };
    let (page, total, truncated): (Vec<PathBuf>, usize, bool) = if order == Some("modified") {
        let mut all: Vec<(SystemTime, PathBuf)> = files
            .map(|path| {
                let modified = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                (modified, path)
            })
            .collect();
        all.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        let total = all.len();
        (
            all.into_iter()
                .skip(offset)
                .take(limit)
                .map(|(_, path)| path)
                .collect(),
            total,
            total > offset + limit,
        )
    } else {
        let mut page = Vec::new();
        let mut seen = 0;
        let mut more = false;
        for path in files {
            if seen >= offset + limit {
                more = true;
                break;
            }
            if seen >= offset {
                page.push(path);
            }
            seen += 1;
        }
        let total = seen;
        (page, total, more)
    };
    let mut result = json!({
        "files": page.iter().map(|path| shown(path, cwd)).collect::<Vec<_>>(),
        "total_count": total,
    });
    if truncated {
        result["truncated"] = json!(true);
        result["next_offset"] = json!(offset + limit);
    }
    ToolOutput::ok(result)
}

#[allow(clippy::too_many_arguments)]
fn search_content(
    root: &Path,
    cwd: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    output_mode: &str,
    context: usize,
    offset: usize,
    limit: usize,
) -> ToolOutput {
    let regex = match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => return ToolOutput::error(format!("Invalid regex: {error}")),
    };
    let files = match files_under(root, file_glob) {
        Ok(files) => files,
        Err(error) => return ToolOutput::error(error),
    };

    let mut matches = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut matched_files = Vec::new();
    let mut total = 0usize;
    for path in files {
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_FILE_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes[..bytes.len().min(8000)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let mut file_count = 0;
        for (index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            file_count += 1;
            total += 1;
            if output_mode == "content" && total > offset && matches.len() < limit {
                let mut entry = json!({
                    "path": shown(&path, cwd),
                    "line": index + 1,
                    "content": clip(line),
                });
                if context > 0 {
                    let start = index.saturating_sub(context);
                    let end = (index + context + 1).min(lines.len());
                    entry["context"] = json!(
                        (start..end)
                            .map(|n| format!("{}|{}", n + 1, clip(lines[n])))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
                matches.push(entry);
            }
        }
        if file_count > 0 {
            counts.insert(shown(&path, cwd), json!(file_count));
            matched_files.push(shown(&path, cwd));
        }
    }

    let result = match output_mode {
        "files_only" => {
            let page: Vec<_> = matched_files
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect();
            let mut result = json!({"files": page, "total_count": matched_files.len()});
            if matched_files.len() > offset + limit {
                result["truncated"] = json!(true);
                result["next_offset"] = json!(offset + limit);
            }
            result
        }
        "count" => json!({"counts": counts, "total_count": total}),
        _ => {
            let mut result = json!({"matches": matches, "total_count": total});
            if total > offset + limit {
                result["truncated"] = json!(true);
                result["next_offset"] = json!(offset + limit);
            }
            result
        }
    };
    ToolOutput::ok(result)
}

fn clip(line: &str) -> String {
    if line.chars().count() > MAX_MATCH_LINE_CHARS {
        format!(
            "{}…",
            line.chars().take(MAX_MATCH_LINE_CHARS).collect::<String>()
        )
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::tools_in;

    fn setup() -> (tempfile::TempDir, SessionTools) {
        let (dir, tools) = tools_in();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(root.join("notes.txt"), "main idea\n").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        (dir, tools)
    }

    #[tokio::test]
    async fn content_search_respects_gitignore_and_globs() {
        let (_dir, tools) = setup();
        let out = search_files(&tools, json!({"pattern": "fn main", "file_glob": "*.rs"})).await;
        let matches = out.value["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "src/main.rs");
        assert_eq!(matches[0]["line"], 1);
    }

    #[tokio::test]
    async fn file_search_matches_names() {
        let (_dir, tools) = setup();
        let out = search_files(&tools, json!({"pattern": "*.rs", "target": "files"})).await;
        let mut files: Vec<_> = out.value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        files.sort();
        assert_eq!(files, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[tokio::test]
    async fn count_and_files_only_modes() {
        let (_dir, tools) = setup();
        let count = search_files(&tools, json!({"pattern": "main", "output_mode": "count"})).await;
        assert_eq!(count.value["total_count"], 2);
        let files = search_files(
            &tools,
            json!({"pattern": "main", "output_mode": "files_only"}),
        )
        .await;
        assert_eq!(files.value["total_count"], 2);
    }

    #[tokio::test]
    async fn invalid_regex_is_reported() {
        let (_dir, tools) = setup();
        let out = search_files(&tools, json!({"pattern": "fn (", "target": "content"})).await;
        assert!(out.is_error);
    }
}
