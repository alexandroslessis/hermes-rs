//! The composer's `@` path completions (`complete.path`) and the file browser's folder listing
//! (`/api/fs/list`) in Hermes's JSON. What they find is `hermes_core::paths`.
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.path`), `hermes_cli/web_routers/files.py`
//! (`fs_list`).

use std::path::Path;

use serde_json::{Value, json};

/// `/api/fs/list`: a folder's entries, folders first (`{entries, error?}`).
pub fn list_dir(path: &str) -> Value {
    let listing = hermes_core::paths::list_dir(path);
    let entries: Vec<Value> = listing
        .entries
        .into_iter()
        .map(|entry| json!({"name": entry.name, "path": entry.path, "isDirectory": entry.is_dir}))
        .collect();
    match listing.error {
        Some(code) => json!({"entries": entries, "error": code}),
        None => json!({"entries": entries}),
    }
}

/// `complete.path` for `word` in `root`: the items.
pub fn complete(word: &str, root: &Path) -> Vec<Value> {
    hermes_core::paths::complete(word, root)
        .into_iter()
        .map(|item| json!({"text": item.text, "display": item.display, "meta": item.meta}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_and_listings_are_hermes_shapes() {
        let items = complete("@", Path::new("."));
        assert_eq!(
            items[2],
            json!({"text": "@file:", "display": "@file:", "meta": "attach file"})
        );
        let dir = hermes_testdir::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let listed = list_dir(&dir.path().display().to_string());
        assert_eq!(listed["entries"][0]["name"], "sub");
        assert_eq!(listed["entries"][0]["isDirectory"], true);
        assert!(listed.get("error").is_none());
        assert_eq!(list_dir("C:/no/such/folder")["error"], "ENOENT");
    }
}
