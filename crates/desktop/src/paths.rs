//! The composer's `@` path completions (`complete.path`; Hermes's `tui_gateway/methods_complete.py`
//! for a local folder): a bare `@` offers the reference kinds, `@file:…` / `@folder:…` and plain
//! paths list a directory, and a bare `@name` searches the project's files and folders by name.
//! The texts are the references the runtime expands when the message is sent (`@file:src/a.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// At most this many completions (Hermes's 30).
const MAX_ITEMS: usize = 30;
/// Files a name search looks at (`_FUZZY_CACHE_MAX_FILES`), and how long a listing is reused.
const MAX_FILES: usize = 20_000;
const LISTING_TTL: Duration = Duration::from_secs(5);
/// Folders a name search never descends into (`_FUZZY_FALLBACK_EXCLUDES`).
const EXCLUDED: [&str; 15] = [
    ".git",
    ".hg",
    ".svn",
    ".next",
    ".cache",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    "dist",
    "build",
    "target",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];
/// A bare `@` (`_AT_DIRECTIVE_HINTS`).
const HINTS: [(&str, &str); 6] = [
    ("@diff", "git diff"),
    ("@staged", "staged diff"),
    ("@file:", "attach file"),
    ("@folder:", "attach folder"),
    ("@url:", "fetch url"),
    ("@git:", "git log"),
];

/// `/api/fs/list` (Hermes's `fs_list`): a folder's entries, folders first, for the file browser
/// and the web folder picker. Paths use `/`, which the picker walks up by: `/` lists the drives,
/// and `~` (or nothing) is the home folder.
pub fn list_dir(path: &str) -> Value {
    let path = path.trim();
    if path == "/" {
        let drives: Vec<Value> = (b'A'..=b'Z')
            .map(|letter| format!("{}:/", letter as char))
            .filter(|root| Path::new(root).is_dir())
            .map(|root| json!({"name": root.trim_end_matches('/'), "path": root, "isDirectory": true}))
            .collect();
        return json!({"entries": drives});
    }
    let folder = match path.strip_prefix('~') {
        Some(rest) => dirs::home_dir()
            .unwrap_or_default()
            .join(rest.trim_start_matches(['/', '\\'])),
        None if path.is_empty() => dirs::home_dir().unwrap_or_default(),
        // `C:` alone is the drive's current folder to Windows; the page means its root.
        None if path.len() == 2 && path.ends_with(':') => PathBuf::from(format!("{path}/")),
        None => PathBuf::from(path),
    };
    let read = match std::fs::read_dir(&folder) {
        Ok(read) => read,
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => "ENOENT",
                std::io::ErrorKind::PermissionDenied => "EACCES",
                std::io::ErrorKind::NotADirectory => "ENOTDIR",
                _ => "read-error",
            };
            return json!({"entries": [], "error": code});
        }
    };
    let base = folder.display().to_string().replace('\\', "/");
    let base = base.trim_end_matches('/');
    let mut entries: Vec<(bool, String)> = read
        .flatten()
        .map(|entry| {
            (
                entry.path().is_dir(),
                entry.file_name().to_string_lossy().into_owned(),
            )
        })
        .collect();
    entries.sort_by(|a, b| (!a.0, a.1.to_lowercase()).cmp(&(!b.0, b.1.to_lowercase())));
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(is_dir, name)| json!({"name": name, "path": format!("{base}/{name}"), "isDirectory": is_dir}))
        .collect();
    json!({"entries": entries})
}

fn item(text: String, meta: &str, display: String) -> Value {
    json!({"text": text, "display": display, "meta": meta})
}

/// `complete.path` for `word` in `root`.
pub fn complete(word: &str, root: &Path) -> Vec<Value> {
    if word.is_empty() {
        return Vec::new();
    }
    let is_context = word.starts_with('@');
    let query = if is_context { &word[1..] } else { word };
    if is_context && query.is_empty() {
        return HINTS
            .iter()
            .map(|(text, meta)| item(text.to_string(), meta, text.to_string()))
            .collect();
    }
    let (tag, path_part) = match query.split_once(':') {
        Some((tag @ ("file" | "folder"), rest)) if is_context => (tag, rest),
        _ if is_context && (query == "file" || query == "folder") => (query, ""),
        _ => ("", query),
    };
    let bare_word = is_context && !path_part.is_empty() && !path_part.contains(['/', '\\']);
    if bare_word && path_part.trim().chars().count() >= 2 && tag != "folder" {
        name_search(root, path_part, tag)
    } else {
        listing(root, word, path_part, tag, is_context)
    }
}

/// The entries of the directory `path_part` names, those starting with its last part.
fn listing(root: &Path, word: &str, path_part: &str, tag: &str, is_context: bool) -> Vec<Value> {
    let typed = path_part.replace('\\', "/");
    // The folder part keeps its trailing `/`: entries are named `{dir}{name}`.
    let (dir, prefix) = match typed.rfind('/') {
        Some(slash) => typed.split_at(slash + 1),
        None => ("", typed.as_str()),
    };
    let home = dirs::home_dir().unwrap_or_default();
    let search = if let Some(rest) = dir.strip_prefix('~') {
        home.join(rest.trim_start_matches('/'))
    } else if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        root.join(dir)
    };
    let Ok(read) = std::fs::read_dir(&search) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, bool)> = read
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().is_dir(),
            )
        })
        .collect();
    entries.sort();
    let mut items = Vec::new();
    for (name, is_dir) in entries {
        if !prefix.is_empty() && !name.to_lowercase().starts_with(&prefix.to_lowercase()) {
            continue;
        }
        if is_context
            && (EXCLUDED.contains(&name.as_str()) || (tag.is_empty() && name.starts_with('.')))
        {
            continue;
        }
        // An explicit `@folder:` / `@file:` lists only its own kind.
        if !tag.is_empty() && (tag == "folder") != is_dir {
            continue;
        }
        let slash = if is_dir { "/" } else { "" };
        let relative = format!("{dir}{name}{slash}");
        let text = if is_context {
            let kind = if tag.is_empty() {
                if is_dir { "folder" } else { "file" }
            } else {
                tag
            };
            format!("@{kind}:{relative}")
        } else if word.starts_with("./") && !relative.starts_with("./") {
            format!("./{relative}")
        } else {
            relative
        };
        items.push(item(
            text,
            if is_dir { "dir" } else { "" },
            format!("{name}{slash}"),
        ));
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    items
}

/// How well `name` matches `query` (`_fuzzy_basename_rank`): exact, prefix, a word boundary,
/// anywhere, then its letters in order; `None` when it doesn't match.
fn rank(name: &str, query: &str) -> Option<u8> {
    let (name, query) = (name.to_lowercase(), query.to_lowercase());
    if name == query {
        return Some(0);
    }
    if name.starts_with(&query) {
        return Some(1);
    }
    if name
        .match_indices(&query)
        .any(|(at, _)| name[..at].ends_with(['-', '_', '.', ' ']))
    {
        return Some(2);
    }
    if name.contains(&query) {
        return Some(3);
    }
    let mut letters = name.chars();
    query
        .chars()
        .all(|wanted| letters.any(|c| c == wanted))
        .then_some(4)
}

/// The project's files relative to `root`: git's list (tracked and untracked, ignores applied),
/// else a walk that skips the usual build and tool folders. Reused for a few seconds per folder.
fn project_files(root: &Path) -> Vec<String> {
    static CACHE: Mutex<Option<HashMap<PathBuf, (Instant, Vec<String>)>>> = Mutex::new(None);
    if let Ok(mut cache) = CACHE.lock() {
        let cache = cache.get_or_insert_with(HashMap::new);
        cache.retain(|_, (at, _)| at.elapsed() < LISTING_TTL);
        if let Some((_, files)) = cache.get(root) {
            return files.clone();
        }
    }
    let (ok, out) = crate::git::git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    );
    let mut files: Vec<String> = if ok {
        out.split('\0')
            .filter(|f| !f.is_empty())
            .take(MAX_FILES)
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    if files.is_empty() {
        walk(root, root, &mut files);
    }
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(HashMap::new)
            .insert(root.to_path_buf(), (Instant::now(), files.clone()));
    }
    files
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if files.len() >= MAX_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            if !EXCLUDED.contains(&name.as_str()) && !name.starts_with('.') {
                walk(root, &path, files);
            }
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// A bare `@name`: the project's files and folders whose name matches, best first, folders first
/// within a tier (`_fuzzy_basename_items`).
fn name_search(root: &Path, query: &str, tag: &str) -> Vec<Value> {
    let want_hidden = query.starts_with('.');
    let mut seen = std::collections::HashSet::new();
    let mut ranked: Vec<(u8, bool, String, String)> = Vec::new();
    let mut consider = |relative: &str, is_dir: bool| {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        if (name.starts_with('.') && !want_hidden) || !seen.insert(relative.to_string()) {
            return;
        }
        if let Some(tier) = rank(name, query) {
            ranked.push((tier, !is_dir, relative.to_string(), name.to_string()));
        }
    };
    // The folder's own entries first: a capped file list can miss them.
    if let Ok(read) = std::fs::read_dir(root) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !EXCLUDED.contains(&name.as_str()) {
                consider(&name, entry.path().is_dir());
            }
        }
    }
    for file in project_files(root) {
        consider(&file, false);
        // Each folder on the way, so a folder whose files don't match is still found.
        let mut parent = file.as_str();
        while let Some((up, _)) = parent.rsplit_once('/') {
            consider(up, true);
            parent = up;
        }
    }
    ranked.sort_by(|a, b| (a.0, a.1, a.2.len(), &a.2).cmp(&(b.0, b.1, b.2.len(), &b.2)));
    ranked
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, is_file, relative, name)| {
            if is_file {
                let kind = if tag.is_empty() { "file" } else { tag };
                let folder = relative
                    .rsplit_once('/')
                    .map_or("", |(dir, _)| dir)
                    .to_string();
                item(format!("@{kind}:{relative}"), &folder, name)
            } else {
                item(format!("@folder:{relative}/"), "dir", format!("{name}/"))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(items: &[Value]) -> Vec<String> {
        items
            .iter()
            .map(|i| i["text"].as_str().unwrap().to_string())
            .collect()
    }

    fn project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("hermes-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src/app")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("src/app/main.rs"), "").unwrap();
        std::fs::write(root.join("src/app_chrome.rs"), "").unwrap();
        std::fs::write(root.join("node_modules/x/main.js"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        root
    }

    #[test]
    fn a_bare_at_offers_the_reference_kinds() {
        assert_eq!(texts(&complete("@", Path::new(".")))[2], "@file:");
    }

    #[test]
    fn listings_and_name_search_name_references() {
        let root = project();
        assert_eq!(
            texts(&complete("@file:src/", &root)),
            ["@file:src/app_chrome.rs"]
        );
        assert_eq!(
            texts(&complete("@folder:src/", &root)),
            ["@folder:src/app/"]
        );
        // A partly typed name under a folder keeps the folder's slash (Tab descends there).
        assert_eq!(
            texts(&complete("@src/ap", &root)),
            ["@folder:src/app/", "@file:src/app_chrome.rs"]
        );
        assert_eq!(
            texts(&complete("@src/app/ma", &root)),
            ["@file:src/app/main.rs"]
        );
        // A name search finds files deep down and the folders on the way, skipping node_modules.
        let found = texts(&complete("@main", &root));
        assert_eq!(found, ["@file:src/app/main.rs"]);
        let found = texts(&complete("@app", &root));
        assert_eq!(found[0], "@folder:src/app/");
        assert!(found.contains(&"@file:src/app_chrome.rs".to_string()));
        assert_eq!(rank("appChrome.tsx", "chrome"), Some(3));
        assert_eq!(rank("app_chrome.rs", "chrome"), Some(2));
        let _ = std::fs::remove_dir_all(root);
    }
}
