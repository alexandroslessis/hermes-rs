//! The coding rail's git reads in Hermes's JSON: `/api/git/status` (`HermesRepoStatus`) and
//! `/api/git/review/rev-parse`. Reading the repository is `hermes_core::git`.
//!
//! Hermes: `hermes_cli/web_git.py` (`repo_status`).

use std::path::Path;

use serde_json::{Value, json};

pub use hermes_core::git::rev_parse;

/// The coding rail's status (`repo_status`, `HermesRepoStatus`); `None` outside a repository.
pub fn repo_status(cwd: &Path) -> Option<Value> {
    let status = hermes_core::git::repo_status(cwd)?;
    let count = |flag: fn(&hermes_core::git::FileStatus) -> bool| status.count(flag);
    let files: Vec<Value> = status
        .files
        .iter()
        .take(hermes_core::git::RepoStatus::FILE_CAP)
        .map(|f| {
            json!({
                "path": f.path,
                "staged": f.staged,
                "unstaged": f.unstaged,
                "untracked": f.untracked,
                "conflicted": f.conflicted,
            })
        })
        .collect();
    Some(json!({
        "branch": status.branch,
        "defaultBranch": status.default_branch,
        "detached": status.detached,
        "ahead": status.ahead,
        "behind": status.behind,
        "staged": count(|f| f.staged),
        "unstaged": count(|f| f.unstaged),
        "untracked": count(|f| f.untracked),
        "conflicted": count(|f| f.conflicted),
        "changed": status.files.len(),
        "added": status.added,
        "removed": status.removed,
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_repository_reports_its_status_in_hermes_fields() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let status = repo_status(repo).expect("the crate lives in a git repository");
        assert!(status["branch"].is_string() || status["detached"] == true);
        for field in [
            "defaultBranch",
            "ahead",
            "behind",
            "changed",
            "added",
            "files",
        ] {
            assert!(status.get(field).is_some(), "{field}");
        }
        assert!(repo_status(&std::env::temp_dir().join("hermes-no-such-dir")).is_none());
    }
}
