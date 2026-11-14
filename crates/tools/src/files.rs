//! read_file, write_file and patch, with Hermes's read-before-write guard.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{SessionTools, ToolOutput, display_path, parse_args};

const READ_CHAR_BUDGET: usize = 100_000;
const MAX_LINE_CHARS: usize = 2000;
const BINARY_SNIFF_BYTES: usize = 8000;
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "svgz",
];

/// What the session last saw of a file: recorded on a full read or a write, checked before
/// `write_file` overwrites it.
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    hash: u64,
}

tokio::task_local! {
    /// Set while a background review reads a file for itself: the read isn't the session's, so it
    /// leaves the session's read-before-write record alone. Otherwise a review running beside a
    /// turn (Phase 4) could satisfy the turn's guard for a file the turn's model never saw.
    pub(crate) static SIDE_READ: ();
}

#[derive(Default)]
pub struct FileState {
    seen: Mutex<HashMap<String, Stamp>>,
}

fn key(path: &Path) -> String {
    display_path(path).to_lowercase()
}

fn stamp_of(path: &Path, bytes: &[u8]) -> Stamp {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Stamp {
        len: bytes.len() as u64,
        modified: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        hash: hasher.finish(),
    }
}

impl FileState {
    fn record(&self, path: &Path, bytes: &[u8]) {
        self.seen.lock().insert(key(path), stamp_of(path, bytes));
    }

    /// Ok when the file on disk is exactly what this session last fully read or wrote.
    fn check_current(&self, path: &Path, on_disk: &[u8]) -> Result<(), String> {
        let seen = self.seen.lock();
        let Some(stamp) = seen.get(&key(path)) else {
            return Err(format!(
                "Refusing to overwrite {}: this session has no full read of it. Call read_file on it first, \
                 merge your changes into its current content, then retry. The file is untouched.",
                display_path(path)
            ));
        };
        let mut hasher = DefaultHasher::new();
        on_disk.hash(&mut hasher);
        if stamp.len != on_disk.len() as u64 || stamp.hash != hasher.finish() {
            return Err(format!(
                "Refusing to overwrite {}: it changed on disk since this session last read or wrote it. \
                 Call read_file again, merge, then retry. The file is untouched.",
                display_path(path)
            ));
        }
        Ok(())
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

/// Up to five names in the same directory that look like the missing file.
fn similar_files(path: &Path) -> Vec<String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let name = name.to_string_lossy().to_lowercase();
    let stem = Path::new(&name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut matches: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|candidate| {
            let lower = candidate.to_lowercase();
            lower != name && (!stem.is_empty() && (lower.contains(&stem) || stem.contains(&lower)))
        })
        .map(|candidate| display_path(&parent.join(candidate)))
        .collect();
    matches.sort();
    matches.truncate(5);
    matches
}

fn not_found(path: &Path) -> ToolOutput {
    let suggestions = similar_files(path);
    let mut extra = json!({"path": display_path(path)});
    if !suggestions.is_empty() {
        extra["similar_files"] = json!(suggestions);
    }
    ToolOutput::error_with(format!("File not found: {}", display_path(path)), extra)
}

fn split_line_ending(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

fn gutter(number: usize, line: &str) -> String {
    let count = line.chars().count();
    if count > MAX_LINE_CHARS {
        let clipped: String = line.chars().take(MAX_LINE_CHARS).collect();
        format!("{number}|{clipped}… [line truncated: {count} chars]")
    } else {
        format!("{number}|{line}")
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `read_file` with `paths` (Phase 10): several files in one call.
#[derive(Deserialize)]
struct ReadManyArgs {
    #[serde(default)]
    path: Option<String>,
    paths: Vec<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Files one `paths` call reads at most; the rest are answered with an error each.
const MAX_READ_PATHS: usize = 20;
/// Characters of file content one `paths` call returns in all (one file may use `READ_CHAR_BUDGET`).
const MULTI_CHAR_BUDGET: usize = 160_000;
/// Below this much budget left, the next file is not started: a few lines of it would cost a
/// second call anyway.
const MIN_FILE_BUDGET: usize = 2_000;

/// `read_file`: one file (`path`, the shape since the start: its results and errors are unchanged)
/// or several (`paths`, one entry per file, an error in one does not fail the others).
pub async fn read_file(tools: &SessionTools, args: Value) -> ToolOutput {
    if args.get("paths").is_some_and(|paths| !paths.is_null()) {
        return read_files(tools, args).await;
    }
    let args: ReadArgs = match parse_args("read_file", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    read_one(tools, &args.path, args.offset, args.limit, READ_CHAR_BUDGET).await
}

async fn read_files(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: ReadManyArgs = match parse_args("read_file", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let mut wanted: Vec<String> = Vec::new();
    for path in args.path.iter().chain(args.paths.iter()) {
        let path = path.trim();
        if !path.is_empty() && !wanted.iter().any(|seen| seen == path) {
            wanted.push(path.to_string());
        }
    }
    if wanted.is_empty() {
        return ToolOutput::error("read_file: paths is empty; give at least one file.");
    }
    let mut files = Vec::new();
    let (mut ok, mut failed, mut used) = (0usize, 0usize, 0usize);
    for (index, path) in wanted.iter().enumerate() {
        let left = MULTI_CHAR_BUDGET.saturating_sub(used);
        let refusal = if index >= MAX_READ_PATHS {
            Some(format!(
                "Not read: one call reads at most {MAX_READ_PATHS} files; ask again for this one."
            ))
        } else if index > 0 && left < MIN_FILE_BUDGET {
            Some(format!(
                "Not read: this call already returned {used} characters (the limit is {MULTI_CHAR_BUDGET}); ask again for this one."
            ))
        } else {
            None
        };
        let mut entry = match refusal {
            Some(message) => json!({"success": false, "error": message}),
            None => {
                let out = read_one(
                    tools,
                    path,
                    args.offset,
                    args.limit,
                    left.min(READ_CHAR_BUDGET),
                )
                .await;
                if let Some(content) = out.value.get("content").and_then(Value::as_str) {
                    used += content.len();
                }
                out.value
            }
        };
        let success = entry.get("success") != Some(&Value::Bool(false));
        if success {
            ok += 1;
        } else {
            failed += 1;
        }
        // Which file an entry is about: the path as the single read would name it.
        if entry.get("path").is_none() {
            entry["path"] = json!(display_path(&tools.resolve(path)));
        }
        files.push(entry);
    }
    // What a file read from outside the working folder brings in is somebody else's text: the
    // session turns careful, per file actually read, as for a single read (`taint_after_call`).
    // Not for a background review's own reads (they are not the session's).
    if SIDE_READ.try_with(|_| ()).is_err() {
        for (entry, path) in files.iter().zip(&wanted) {
            if entry.get("success") == Some(&Value::Bool(false)) {
                continue;
            }
            let resolved = tools.resolve(path);
            if tools.read_outside(resolved.clone()).await {
                tools
                    .mark_tainted(crate::taint::outside_file_source(&tools.shown(&resolved)))
                    .await;
            }
        }
    }
    ToolOutput {
        value: json!({"files": files, "files_read": ok, "files_failed": failed}),
        is_error: ok == 0,
    }
}

async fn read_one(
    tools: &SessionTools,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    char_budget: usize,
) -> ToolOutput {
    let path = tools.resolve(path);
    if path.is_dir() {
        return ToolOutput::error(format!(
            "{} is a directory. Use search_files with target='files' to list it.",
            display_path(&path)
        ));
    }
    if is_image(&path) {
        return ToolOutput::error(format!(
            "{} is an image; read_file only reads text.",
            display_path(&path)
        ));
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return not_found(&path),
        Err(error) => {
            return ToolOutput::error(format!("Could not read {}: {error}", display_path(&path)));
        }
    };
    if is_binary(&bytes) {
        return ToolOutput::error(format!(
            "{} is a binary file; read_file only reads text.",
            display_path(&path)
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.split_inclusive('\n').map(split_line_ending).collect();
    let total_lines = lines.len();
    let offset = offset.unwrap_or(1).max(1);
    let max_lines = tools.limits().read_file_max_lines.max(1);
    let limit = limit.unwrap_or(max_lines).clamp(1, max_lines);

    if offset > total_lines.max(1) {
        return ToolOutput::ok(json!({
            "content": "",
            "total_lines": total_lines,
            "file_size": bytes.len(),
            "hint": format!("offset {offset} is past the end of the file ({total_lines} lines)."),
        }));
    }

    let mut rendered = Vec::new();
    let mut chars = 0;
    let mut next = offset;
    for (index, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let line = gutter(index + 1, line);
        if chars + line.len() > char_budget && !rendered.is_empty() {
            break;
        }
        chars += line.len() + 1;
        rendered.push(line);
        next = index + 2;
    }

    let mut result = json!({
        "content": rendered.join("\n"),
        "total_lines": total_lines,
        "file_size": bytes.len(),
    });
    if next <= total_lines {
        result["truncated"] = json!(true);
        result["next_offset"] = json!(next);
    } else if offset == 1 && SIDE_READ.try_with(|_| ()).is_err() {
        tools.files.record(&path, &bytes);
    }
    ToolOutput::ok(result)
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

pub async fn write_file(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: WriteArgs = match parse_args("write_file", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let path = tools.resolve(&args.path);
    if path.is_dir() {
        return ToolOutput::error(format!("{} is a directory.", display_path(&path)));
    }
    let mut previous: Option<String> = None;
    if path.exists() {
        let on_disk = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Could not read {}: {error}",
                    display_path(&path)
                ));
            }
        };
        if let Err(message) = tools.files.check_current(&path, &on_disk) {
            return ToolOutput::error(message);
        }
        previous = String::from_utf8(on_disk).ok();
    }
    let dirs_created = match path.parent() {
        Some(parent) if !parent.exists() => {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!(
                    "Could not create {}: {error}",
                    display_path(parent)
                ));
            }
            true
        }
        _ => false,
    };
    write_verified(tools, &path, args.content.as_bytes())
        .await
        .map(|()| {
            let mut result = json!({
                "success": true,
                "path": tools.shown(&path),
                "bytes_written": args.content.len(),
                "verified": true,
            });
            if dirs_created {
                result["dirs_created"] = json!(true);
            }
            // Hermes's post-write syntax check.
            if let Some(lint) = crate::lint::delta(&path, previous.as_deref(), &args.content) {
                result["lint"] = lint;
            }
            ToolOutput::ok(result)
        })
        .unwrap_or_else(ToolOutput::error)
}

async fn write_verified(tools: &SessionTools, path: &Path, bytes: &[u8]) -> Result<(), String> {
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("Could not write {}: {error}", display_path(path)))?;
    let written = tokio::fs::read(path).await.map_err(|error| {
        format!(
            "Wrote {} but could not read it back: {error}",
            display_path(path)
        )
    })?;
    if written != bytes {
        return Err(format!(
            "Wrote {} but the content on disk differs.",
            display_path(path)
        ));
    }
    tools.files.record(path, bytes);
    Ok(())
}

// --- patch ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Exact,
    LineEndings,
    TrailingWhitespace,
    Indentation,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Strategy::Exact => "exact",
            Strategy::LineEndings => "line_endings",
            Strategy::TrailingWhitespace => "trailing_whitespace",
            Strategy::Indentation => "indentation",
        }
    }
}

/// A match of `old_string` in the file: a byte range and the text that replaces it.
struct Hit {
    range: std::ops::Range<usize>,
    replacement: String,
}

struct FileLine {
    start: usize,
    /// End of the line's text, before the line ending.
    end: usize,
    /// End including the line ending.
    end_with_newline: usize,
}

fn file_lines(content: &str) -> Vec<FileLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    for raw in content.split_inclusive('\n') {
        let text_len = split_line_ending(raw).len();
        lines.push(FileLine {
            start,
            end: start + text_len,
            end_with_newline: start + raw.len(),
        });
        start += raw.len();
    }
    lines
}

fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

fn exact_hits(content: &str, old: &str, new: &str) -> Vec<Hit> {
    content
        .match_indices(old)
        .map(|(start, _)| Hit {
            range: start..start + old.len(),
            replacement: new.to_string(),
        })
        .collect()
}

fn line_hits(content: &str, old: &str, new: &str, strategy: Strategy) -> Vec<Hit> {
    let crlf = content.contains("\r\n");
    let newline = if crlf { "\r\n" } else { "\n" };
    let old_ends_with_newline = old.ends_with('\n');
    let old_lines: Vec<&str> = old
        .trim_end_matches(['\n', '\r'])
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    let lines = file_lines(content);
    let compare = |file: &str, wanted: &str| match strategy {
        Strategy::TrailingWhitespace => file.trim_end() == wanted.trim_end(),
        Strategy::Indentation => file.trim() == wanted.trim(),
        Strategy::Exact | Strategy::LineEndings => file == wanted,
    };
    if old_lines.is_empty() || old_lines.len() > lines.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut index = 0;
    while index + old_lines.len() <= lines.len() {
        let window = &lines[index..index + old_lines.len()];
        if window
            .iter()
            .zip(&old_lines)
            .all(|(line, wanted)| compare(&content[line.start..line.end], wanted))
        {
            let last = &window[window.len() - 1];
            let end = if old_ends_with_newline {
                last.end_with_newline
            } else {
                last.end
            };
            let mut replacement = new.replace("\r\n", "\n");
            if strategy == Strategy::Indentation {
                replacement = reindent(
                    &replacement,
                    &old_lines,
                    &content[window[0].start..window[0].end],
                );
            }
            if crlf {
                replacement = replacement.replace('\n', newline);
            }
            hits.push(Hit {
                range: window[0].start..end,
                replacement,
            });
            index += old_lines.len();
        } else {
            index += 1;
        }
    }
    hits
}

/// Shifts `new` by the difference between the indentation the model wrote and the file's.
fn reindent(new: &str, old_lines: &[&str], first_file_line: &str) -> String {
    let Some(old_first) = old_lines.iter().find(|line| !line.trim().is_empty()) else {
        return new.to_string();
    };
    let old_indent = indentation(old_first);
    let file_indent = indentation(first_file_line);
    if old_indent == file_indent {
        return new.to_string();
    }
    new.split('\n')
        .map(|line| match line.strip_prefix(old_indent) {
            Some(rest) if !line.trim().is_empty() => format!("{file_indent}{rest}"),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_hits(content: &str, old: &str, new: &str) -> (Strategy, Vec<Hit>) {
    let exact = exact_hits(content, old, new);
    if !exact.is_empty() {
        return (Strategy::Exact, exact);
    }
    if content.contains("\r\n") && !old.contains("\r\n") {
        let hits = exact_hits(
            content,
            &old.replace('\n', "\r\n"),
            &new.replace('\n', "\r\n"),
        );
        if !hits.is_empty() {
            return (Strategy::LineEndings, hits);
        }
    }
    for strategy in [Strategy::TrailingWhitespace, Strategy::Indentation] {
        let hits = line_hits(content, old, new, strategy);
        if !hits.is_empty() {
            return (strategy, hits);
        }
    }
    (Strategy::Exact, Vec::new())
}

/// `patch`'s matching for other editors (skill_manage): the content with `old` replaced, and the
/// number of replacements. Errors name what to do next.
pub(crate) fn replace_in(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old == new {
        return Err("new_string is identical to old_string; nothing to change.".into());
    }
    let (_, hits) = find_hits(content, old, new);
    if hits.is_empty() {
        return Err("old_string was not found (also tried ignoring line endings, trailing whitespace and indentation). Copy the text exactly from the current file.".into());
    }
    if hits.len() > 1 && !replace_all {
        return Err(format!(
            "old_string matches {} places. Include more surrounding lines to make it unique, or set replace_all=true.",
            hits.len()
        ));
    }
    let mut updated = content.to_string();
    for hit in hits.iter().rev() {
        updated.replace_range(hit.range.clone(), &hit.replacement);
    }
    Ok((updated, hits.len()))
}

fn line_number(content: &str, byte: usize) -> usize {
    content[..byte].matches('\n').count() + 1
}

#[derive(Deserialize)]
struct PatchArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub async fn patch(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: PatchArgs = match parse_args("patch", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let path = tools.resolve(&args.path);
    if args.old_string.is_empty() {
        return ToolOutput::error("old_string is empty. To create a file use write_file.");
    }
    if args.old_string == args.new_string {
        return ToolOutput::error("new_string is identical to old_string; nothing to change.");
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return not_found(&path),
        Err(error) => {
            return ToolOutput::error(format!("Could not read {}: {error}", display_path(&path)));
        }
    };
    if is_binary(&bytes) {
        return ToolOutput::error(format!("{} is a binary file.", display_path(&path)));
    }
    let Ok(content) = String::from_utf8(bytes) else {
        return ToolOutput::error(format!(
            "{} is not valid UTF-8; patch only edits UTF-8 text.",
            display_path(&path)
        ));
    };

    let (strategy, hits) = find_hits(&content, &args.old_string, &args.new_string);
    if hits.is_empty() {
        if !args.new_string.is_empty() && find_hits(&content, &args.new_string, "").1.len() == 1 {
            return ToolOutput::ok(json!({
                "success": true,
                "no_change": true,
                "note": "old_string was not found but new_string is already present: the edit appears to be applied already. Nothing was written.",
                "files_modified": [],
            }));
        }
        return ToolOutput::error(format!(
            "old_string was not found in {} (also tried ignoring line endings, trailing whitespace and indentation). \
             Call read_file to see the current content and copy the text exactly.",
            display_path(&path)
        ));
    }
    if hits.len() > 1 && !args.replace_all {
        let lines: Vec<usize> = hits
            .iter()
            .map(|hit| line_number(&content, hit.range.start))
            .collect();
        return ToolOutput::error_with(
            format!(
                "old_string matches {} places in {}. Include more surrounding lines to make it unique, or set replace_all=true.",
                hits.len(),
                display_path(&path)
            ),
            json!({"match_lines": lines}),
        );
    }

    let mut updated = content.clone();
    for hit in hits.iter().rev() {
        updated.replace_range(hit.range.clone(), &hit.replacement);
    }
    if let Err(error) = write_verified(tools, &path, updated.as_bytes()).await {
        return ToolOutput::error(error);
    }
    let shown = tools.shown(&path);
    let diff = similar::TextDiff::from_lines(&content, &updated)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{shown}"), &format!("b/{shown}"))
        .to_string();
    let mut result = json!({
        "success": true,
        "diff": diff,
        "files_modified": [shown],
        "replacements": hits.len(),
    });
    if strategy != Strategy::Exact {
        result["match"] = json!(strategy.name());
    }
    if let Some(lint) = crate::lint::delta(&path, Some(&content), &updated) {
        result["lint"] = lint;
    }
    ToolOutput::ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::tools_in;

    fn text(output: &ToolOutput) -> &Value {
        &output.value
    }

    #[tokio::test]
    async fn read_numbers_lines_and_paginates() {
        let (dir, tools) = tools_in();
        let body: String = (1..=5).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let out = read_file(&tools, json!({"path": "a.txt", "offset": 2, "limit": 2})).await;
        assert_eq!(text(&out)["content"], "2|line 2\n3|line 3");
        assert_eq!(text(&out)["total_lines"], 5);
        assert_eq!(text(&out)["next_offset"], 4);
    }


    #[tokio::test]
    async fn read_default_lines_follow_the_budget_limits() {
        let (dir, tools) = tools_in();
        let body: String = (1..=3000).map(|n| format!("l{n}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), body).unwrap();
        // Balanced (the default): 2000 lines, then a next offset.
        let out = read_file(&tools, json!({"path": "big.txt"})).await;
        assert_eq!(text(&out)["next_offset"], 2001);
        // Requests: the whole file (up to 4000 lines).
        tools.set_limits(crate::ToolLimits {
            read_file_max_lines: 4000,
            ..Default::default()
        });
        let out = read_file(&tools, json!({"path": "big.txt"})).await;
        assert!(text(&out).get("next_offset").is_none(), "{:?}", out.value);
        assert!(text(&out)["content"].as_str().unwrap().ends_with("3000|l3000"));
    }

    #[tokio::test]
    async fn paths_read_several_files_one_entry_each_and_errors_stay_per_file() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "gamma\n").unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("config.toml"), "x").unwrap();
        let out = read_file(
            &tools,
            json!({"paths": ["a.txt", "nope.txt", "b.txt", "bin.dat", "sub", "config.tml", "a.txt"]}),
        )
        .await;
        assert!(!out.is_error);
        let v = text(&out);
        assert_eq!((v["files_read"].as_u64(), v["files_failed"].as_u64()), (Some(2), Some(4)));
        let files = v["files"].as_array().unwrap();
        // One entry per distinct path, in the order asked; the duplicate is read once.
        assert_eq!(files.len(), 6);
        assert_eq!(files[0]["content"], "1|alpha\n2|beta");
        assert_eq!(files[0]["total_lines"], 2);
        assert!(files[0]["path"].as_str().unwrap().ends_with("/a.txt"));
        assert_eq!(files[1]["success"], false);
        assert!(files[1]["error"].as_str().unwrap().starts_with("File not found:"));
        assert_eq!(files[2]["content"], "1|gamma");
        assert!(files[3]["error"].as_str().unwrap().contains("binary file"));
        assert!(files[4]["error"].as_str().unwrap().contains("is a directory"));
        assert!(files[5]["similar_files"][0].as_str().unwrap().ends_with("config.toml"));
        // A single entry is what a single read says (plus its path).
        let single = read_file(&tools, json!({"path": "a.txt"})).await;
        let mut first = files[0].clone();
        first.as_object_mut().unwrap().remove("path");
        assert_eq!(first, single.value);
        // All failed: an error for the call, the entries still say why.
        let none = read_file(&tools, json!({"paths": ["x1", "x2"]})).await;
        assert!(none.is_error);
        assert_eq!(text(&none)["files"].as_array().unwrap().len(), 2);
        // `path` and `paths` together: both are read, `path` first.
        let both = read_file(&tools, json!({"path": "b.txt", "paths": ["a.txt"]})).await;
        assert_eq!(text(&both)["files"][0]["content"], "1|gamma");
        assert_eq!(text(&both)["files"][1]["content"], "1|alpha\n2|beta");
        // Nothing to read, and a wrong shape.
        assert!(read_file(&tools, json!({"paths": []})).await.is_error);
        assert!(read_file(&tools, json!({"paths": [" "]})).await.is_error);
        let bad = read_file(&tools, json!({"paths": "a.txt"})).await;
        assert!(bad.is_error && text(&bad)["error"].as_str().unwrap().starts_with("Invalid arguments for read_file"));
    }

    /// Each file a `paths` call read whole counts for the read-before-write guard, as a single
    /// read would; one it did not read does not.
    #[tokio::test]
    async fn a_multi_path_read_satisfies_the_write_guard_per_file() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "c\n").unwrap();
        read_file(&tools, json!({"paths": ["a.txt", "b.txt"]})).await;
        let a = write_file(&tools, json!({"path": "a.txt", "content": "A\n"})).await;
        assert!(!a.is_error, "{}", a.value);
        let b = write_file(&tools, json!({"path": "b.txt", "content": "B\n"})).await;
        assert!(!b.is_error, "{}", b.value);
        let c = write_file(&tools, json!({"path": "c.txt", "content": "C\n"})).await;
        assert!(c.is_error, "c.txt was never read: {}", c.value);
        assert_eq!(std::fs::read_to_string(dir.path().join("c.txt")).unwrap(), "c\n");
    }

    #[tokio::test]
    async fn the_single_path_shape_and_its_errors_are_unchanged() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let out = read_file(&tools, json!({"path": "a.txt"})).await;
        assert_eq!(
            out.value,
            json!({"content": "1|one\n2|two", "total_lines": 2, "file_size": 8})
        );
        let missing = read_file(&tools, json!({"path": "nope.txt"})).await;
        assert!(missing.is_error);
        assert_eq!(missing.value["error"], "File not found: ".to_string() + &display_path(&tools.resolve("nope.txt")));
        // No path at all: the error it always was.
        let none = read_file(&tools, json!({})).await;
        assert_eq!(none.value["error"], "Invalid arguments for read_file: missing field `path`");
        // `paths: null` is as if absent.
        let null = read_file(&tools, json!({"path": "a.txt", "paths": null})).await;
        assert_eq!(null.value, out.value);
    }

    #[tokio::test]
    async fn paths_are_bounded_in_count_and_total_size() {
        let (dir, tools) = tools_in();
        let names: Vec<String> = (0..25).map(|i| format!("f{i}.txt")).collect();
        for name in &names {
            std::fs::write(dir.path().join(name), "x\n").unwrap();
        }
        let out = read_file(&tools, json!({"paths": names})).await;
        let files = out.value["files"].as_array().unwrap();
        assert_eq!(files.len(), 25);
        assert_eq!(out.value["files_read"], MAX_READ_PATHS);
        assert!(files[MAX_READ_PATHS]["error"].as_str().unwrap().starts_with("Not read: one call reads at most 20"));
        // Size: files of ~90 KB each, the total stops at the budget and the rest say so.
        let big: String = (0..1500).map(|n| format!("{n:0>58}\n")).collect();
        for i in 0..4 {
            std::fs::write(dir.path().join(format!("big{i}.txt")), &big).unwrap();
        }
        let out = read_file(
            &tools,
            json!({"paths": ["big0.txt", "big1.txt", "big2.txt", "big3.txt"]}),
        )
        .await;
        let total: usize = out.value["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["content"].as_str())
            .map(str::len)
            .sum();
        assert!(total <= MULTI_CHAR_BUDGET + 200, "{total}");
        let files = out.value["files"].as_array().unwrap();
        // The first is whole; the budget runs out inside the second or before the third.
        assert!(files[0].get("truncated").is_none());
        assert!(files.iter().skip(1).any(|f| f["truncated"] == true || f["success"] == false));
        let refused = files.iter().find(|f| f["success"] == false).unwrap();
        assert!(refused["error"].as_str().unwrap().starts_with("Not read: this call already returned"));
    }

    #[tokio::test]
    async fn paths_mask_secrets_per_file_and_taint_only_outside_files_that_were_read() {
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("o.txt");
        std::fs::write(&outside, "outside text").unwrap();
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join(".env"), "OPENAI_KEY=sk-abcdefghijklmnop1234\n").unwrap();
        std::fs::write(dir.path().join("mine.txt"), "mine").unwrap();
        let out = tools
            .dispatch("read_file", json!({"paths": [".env", "mine.txt"]}), None)
            .await;
        let text = out.value.to_string();
        assert!(!text.contains("sk-abcdefghij"), "{text}");
        assert_eq!(out.value["files"][1]["content"], "1|mine");
        // Inside the working folder, or failed: nothing is learned from a stranger.
        tools
            .taint_after_call("read_file", &json!({"paths": [".env", "mine.txt"]}), false)
            .await;
        assert!(!tools.taint().is_tainted());
        let missing = elsewhere.path().join("missing.txt").display().to_string();
        read_file(&tools, json!({"paths": ["mine.txt", missing]})).await;
        assert!(!tools.taint().is_tainted(), "a file that was not read taints nothing");
        // An outside file that was read does, as a single read does.
        let out = read_file(&tools, json!({"paths": ["mine.txt", outside.display().to_string()]})).await;
        assert_eq!(out.value["files_read"], 2);
        assert!(tools.taint().is_tainted());
        // A background review's own read is not the session's.
        let (_dir2, other) = tools_in();
        SIDE_READ
            .scope((), read_file(&other, json!({"paths": [outside.display().to_string()]})))
            .await;
        assert!(!other.taint().is_tainted());
    }

    #[tokio::test]
    async fn read_missing_file_suggests_similar_names() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("config.toml"), "x").unwrap();
        let out = read_file(&tools, json!({"path": "config.tml"})).await;
        assert!(out.is_error);
        assert!(
            text(&out)["similar_files"][0]
                .as_str()
                .unwrap()
                .ends_with("config.toml")
        );
    }

    #[tokio::test]
    async fn write_requires_a_current_full_read() {
        let (dir, tools) = tools_in();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "old").unwrap();

        let refused = write_file(&tools, json!({"path": "notes.md", "content": "new"})).await;
        assert!(refused.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");

        read_file(&tools, json!({"path": "notes.md"})).await;
        std::fs::write(&path, "changed by someone else").unwrap();
        let stale = write_file(&tools, json!({"path": "notes.md", "content": "new"})).await;
        assert!(stale.is_error);
        assert!(
            text(&stale)["error"]
                .as_str()
                .unwrap()
                .contains("changed on disk")
        );

        read_file(&tools, json!({"path": "notes.md"})).await;
        let ok = write_file(&tools, json!({"path": "notes.md", "content": "new"})).await;
        assert_eq!(text(&ok)["verified"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[tokio::test]
    async fn write_creates_new_files_and_parent_dirs_without_a_read() {
        let (dir, tools) = tools_in();
        let out = write_file(&tools, json!({"path": "deep/dir/new.txt", "content": "hi"})).await;
        assert_eq!(text(&out)["dirs_created"], true);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("deep/dir/new.txt")).unwrap(),
            "hi"
        );
    }

    #[tokio::test]
    async fn patch_exact_unique_and_diff() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let out = patch(
            &tools,
            json!({"path": "a.rs", "old_string": "fn b() {}", "new_string": "fn b() { todo() }"}),
        )
        .await;
        assert!(!out.is_error, "{:?}", out.value);
        assert!(
            text(&out)["diff"]
                .as_str()
                .unwrap()
                .contains("+fn b() { todo() }")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() {}\nfn b() { todo() }\n"
        );
    }

    #[tokio::test]
    async fn patch_rejects_ambiguous_matches_unless_replace_all() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.txt"), "x = 1\nx = 1\n").unwrap();
        let ambiguous = patch(
            &tools,
            json!({"path": "a.txt", "old_string": "x = 1", "new_string": "x = 2"}),
        )
        .await;
        assert!(ambiguous.is_error);
        assert_eq!(text(&ambiguous)["match_lines"], json!([1, 2]));
        let all = patch(&tools, json!({"path": "a.txt", "old_string": "x = 1", "new_string": "x = 2", "replace_all": true})).await;
        assert_eq!(text(&all)["replacements"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "x = 2\nx = 2\n"
        );
    }

    #[tokio::test]
    async fn patch_tolerates_crlf_files() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let out = patch(
            &tools,
            json!({"path": "w.txt", "old_string": "one\ntwo", "new_string": "one\n2"}),
        )
        .await;
        assert!(!out.is_error, "{:?}", out.value);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("w.txt")).unwrap(),
            "one\r\n2\r\nthree\r\n"
        );
    }

    #[tokio::test]
    async fn patch_reindents_when_indentation_differs() {
        let (dir, tools) = tools_in();
        std::fs::write(
            dir.path().join("p.py"),
            "def f():\n        x = 1\n        return x\n",
        )
        .unwrap();
        let out = patch(
            &tools,
            json!({"path": "p.py", "old_string": "    x = 1\n    return x", "new_string": "    x = 2\n    return x"}),
        )
        .await;
        assert_eq!(text(&out)["match"], "indentation");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("p.py")).unwrap(),
            "def f():\n        x = 2\n        return x\n"
        );
    }

    #[tokio::test]
    async fn patch_reports_an_already_applied_edit_as_no_change() {
        let (dir, tools) = tools_in();
        std::fs::write(dir.path().join("a.txt"), "value = 2\n").unwrap();
        let out = patch(
            &tools,
            json!({"path": "a.txt", "old_string": "value = 1", "new_string": "value = 2"}),
        )
        .await;
        assert!(!out.is_error);
        assert_eq!(text(&out)["no_change"], true);
    }
}
