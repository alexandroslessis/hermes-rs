//! The `/api/git/*` routes (the review pane and the coding rail's git actions) in Hermes's JSON.
//! What each does (git and `gh` runs, the porcelain parsing) is `hermes_core::git_review` and
//! `hermes_core::git`.
//!
//! Hermes: `hermes_cli/web_routers/git.py` (the routes), `hermes_cli/web_git.py` (the shapes).

use std::path::PathBuf;

use hermes_core::git_review::{self as review, WorktreeRequest};
use serde_json::{Value, json};

/// The `/api/git/*` routes (`hermes_cli/web_routers/git.py`): `(status, body)`, `None` for one
/// this backend doesn't serve. A failed change answers 400 with git's message. Blocking: call it
/// off the async workers.
pub fn route(method: &str, path: &str, query: Option<&str>, body: &[u8]) -> Option<(u16, Value)> {
    let route = path.strip_prefix("/api/git/")?;
    let param = |name: &str| crate::rest::query_param(query, name).filter(|v| !v.is_empty());
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let field = |name: &str| {
        body[name]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let folder = PathBuf::from(
        if method == "GET" {
            param("path")
        } else {
            field("path")
        }
        .unwrap_or_default(),
    );
    // A file named by the page, against the repository folder.
    let file =
        |name: Option<String>| crate::files::resolve(&name.unwrap_or_default(), Some(&folder));
    let credentials = || {
        Some((
            403,
            json!({"detail": "That file holds credentials and isn't shown."}),
        ))
    };
    let ok = |done: Result<(), String>| done.map(|()| json!({"ok": true}));
    let outcome: Result<Value, String> = match (method, route) {
        ("GET", "status") => Ok(crate::git::repo_status(&folder).unwrap_or(Value::Null)),
        ("GET", "review/rev-parse") => {
            Ok(json!({"sha": crate::git::rev_parse(&folder, param("ref").as_deref())}))
        }
        ("GET", "gh-auth") => {
            let auth = review::gh_auth();
            Ok(json!({"available": auth.available, "authenticated": auth.authenticated}))
        }
        ("GET", "worktrees") => Ok(json!({"worktrees": worktrees(&folder)})),
        ("GET", "branches") => Ok(json!({"branches": branches(&folder)})),
        ("GET", "base-branches") => Ok(json!({"branches": base_branches(&folder)})),
        ("GET", "review/list") => Ok(review_list(
            &folder,
            param("scope").as_deref().unwrap_or("uncommitted"),
            param("base").as_deref(),
        )),
        ("GET", "review/diff") => {
            let target = file(param("file"));
            if crate::files::is_sensitive(&target) {
                return credentials();
            }
            review::review_diff(
                &folder,
                &target,
                param("scope").as_deref().unwrap_or("uncommitted"),
                param("base").as_deref(),
                param("staged").as_deref() == Some("true"),
            )
            .map(|diff| json!({"diff": diff}))
        }
        ("GET", "file-diff") => {
            let target = file(param("file"));
            if crate::files::is_sensitive(&target) {
                return credentials();
            }
            review::file_diff(&folder, &target).map(|diff| json!({"diff": diff}))
        }
        ("GET", "review/commit-context") => {
            let context = review::commit_context(&folder);
            Ok(json!({"diff": context.diff, "recent": context.recent}))
        }
        ("GET", "review/ship-info") => Ok(ship_info(&folder)),
        ("POST", "review/stage") => ok(review::stage(&folder, field("file").as_deref())),
        ("POST", "review/unstage") => ok(review::unstage(&folder, field("file").as_deref())),
        ("POST", "review/revert") => ok(review::revert(&folder, field("file").as_deref())),
        ("POST", "review/commit") => ok(review::commit(
            &folder,
            body["message"].as_str().unwrap_or_default(),
            body["push"] == true,
        )),
        ("POST", "review/push") => ok(review::push(&folder)),
        ("POST", "review/create-pr") => review::create_pr(&folder).map(|url| json!({"url": url})),
        ("POST", "review/pr-list") => {
            let branches: Vec<String> = body["branches"]
                .as_array()
                .map(|b| {
                    b.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let numbers: Vec<u64> = body["numbers"]
                .as_array()
                .map(|n| n.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            Ok(pr_list(&folder, &branches, &numbers))
        }
        ("POST", "worktree/add") => {
            let text = |key: &str| body[key].as_str().unwrap_or_default().trim().to_string();
            let request = WorktreeRequest {
                name: text("name"),
                branch: text("branch"),
                base: text("base"),
                existing_branch: text("existingBranch"),
            };
            review::worktree_add(&folder, &request).map(|added| {
                json!({"path": added.path, "branch": added.branch, "repoRoot": added.repo_root})
            })
        }
        ("POST", "worktree/remove") => {
            let worktree = field("worktreePath").unwrap_or_default();
            review::worktree_remove(&folder, &worktree, body["force"] == true)
                .map(|()| json!({"removed": worktree}))
        }
        ("POST", "branch/switch") => {
            review::branch_switch(&folder, &field("branch").unwrap_or_default())
                .map(|branch| json!({"branch": branch}))
        }
        _ => return None,
    };
    Some(match outcome {
        Ok(value) => (200, value),
        Err(detail) => (400, json!({"detail": detail})),
    })
}

/// `GET /api/git/review/list`: the changed files (`{files, base}`).
fn review_list(folder: &std::path::Path, scope: &str, base: Option<&str>) -> Value {
    let list = review::review_list(folder, scope, base);
    let files: Vec<Value> = list
        .files
        .iter()
        .map(|f| {
            json!({"path": f.path, "added": f.added, "removed": f.removed,
                   "status": f.status, "staged": f.staged})
        })
        .collect();
    json!({"files": files, "base": list.base})
}

/// `GET /api/git/review/ship-info`: `{ghReady, pr}`.
fn ship_info(folder: &std::path::Path) -> Value {
    let info = review::ship_info(folder);
    let pr = info
        .pr
        .map(|pr| json!({"url": pr.url, "state": pr.state, "number": pr.number}));
    json!({"ghReady": info.gh_ready, "pr": pr})
}

/// `POST /api/git/review/pr-list`: `{ghReady, prs}`.
fn pr_list(folder: &std::path::Path, branches: &[String], numbers: &[u64]) -> Value {
    let found = review::pr_list(folder, branches, numbers);
    let prs: Vec<Value> = found
        .prs
        .iter()
        .map(|pr| {
            json!({"branch": pr.branch, "draft": pr.draft, "number": pr.number,
                   "state": pr.state, "title": pr.title, "url": pr.url})
        })
        .collect();
    json!({"ghReady": found.gh_ready, "prs": prs})
}

/// `GET /api/git/worktrees`: one entry per tree, the main one first.
fn worktrees(folder: &std::path::Path) -> Vec<Value> {
    review::worktrees(folder)
        .into_iter()
        .map(|t| {
            json!({"path": t.path, "branch": t.branch, "isMain": t.is_main,
                   "detached": t.detached, "locked": t.locked})
        })
        .collect()
}

/// `GET /api/git/branches`.
fn branches(folder: &std::path::Path) -> Vec<Value> {
    review::branches(folder)
        .into_iter()
        .map(|b| {
            json!({"name": b.name, "checkedOut": b.checked_out, "isDefault": b.is_default,
                   "isRemote": b.is_remote, "worktreePath": b.worktree_path})
        })
        .collect()
}

/// `GET /api/git/base-branches`.
fn base_branches(folder: &std::path::Path) -> Vec<Value> {
    review::base_branches(folder)
        .into_iter()
        .map(|b| json!({"name": b.name, "isRemote": b.is_remote, "isDefault": b.is_default}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &std::path::Path, args: &[&str]) {
        assert!(hermes_core::git::git(cwd, args).0, "git {args:?}");
    }

    #[test]
    fn the_routes_answer_hermes_shapes() {
        let dir = hermes_testdir::tempdir().unwrap();
        let cwd = dir.path();
        git(cwd, &["init", "-q", "-b", "main"]);
        git(cwd, &["config", "user.email", "test@example.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        std::fs::write(cwd.join("a.txt"), "one\n").unwrap();
        git(cwd, &["add", "-A"]);
        git(cwd, &["commit", "-q", "-m", "first"]);
        std::fs::write(cwd.join("a.txt"), "one\ntwo\n").unwrap();
        let path = cwd.display().to_string();
        let query = format!("path={path}");
        let get = |route: &str, query: &str| {
            route_get(route, query).unwrap_or_else(|| panic!("{route} is served"))
        };
        let (status, list) = get("review/list", &query);
        assert_eq!(status, 200);
        assert_eq!(
            list["files"][0],
            json!({"path": "a.txt", "added": 1, "removed": 0, "status": "M", "staged": false})
        );
        assert_eq!(list["base"], Value::Null);
        let (_, diff) = get("review/diff", &format!("{query}&file=a.txt"));
        assert!(diff["diff"].as_str().unwrap().contains("+two"));
        let (_, trees) = get("worktrees", &query);
        assert_eq!(trees["worktrees"][0]["isMain"], true);
        assert_eq!(trees["worktrees"][0]["branch"], "main");
        let (_, branches) = get("branches", &query);
        assert_eq!(branches["branches"][0]["isDefault"], true);
        assert!(branches["branches"][0]["worktreePath"].is_string());
        let (_, bases) = get("base-branches", &query);
        assert_eq!(bases["branches"][0]["isRemote"], false);
        let (_, context) = get("review/commit-context", &query);
        assert_eq!(context["recent"], "first");
        let (status, refused) = get("review/diff", &format!("{query}&file=.env"));
        assert_eq!(status, 403, "{refused}");
        let body = json!({"path": path, "message": "second"}).to_string();
        let (status, committed) =
            route("POST", "/api/git/review/commit", None, body.as_bytes()).unwrap();
        assert_eq!((status, committed), (200, json!({"ok": true})));
        let body = json!({"path": path, "branch": "  "}).to_string();
        let (status, refused) =
            route("POST", "/api/git/branch/switch", None, body.as_bytes()).unwrap();
        assert_eq!(status, 400);
        assert_eq!(refused["detail"], "Branch name is required.");
        assert!(route("GET", "/api/git/nothing", None, b"").is_none());
    }

    fn route_get(route_name: &str, query: &str) -> Option<(u16, Value)> {
        route("GET", &format!("/api/git/{route_name}"), Some(query), b"")
    }
}
