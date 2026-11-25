//! The diff over a real `hermes_core::Core` and temporary git repositories (made with `git init`
//! in `hermes_testdir` folders, a local identity, no network, nothing of the user's): the three
//! scopes' patches parse into the right files (modified, renamed, deleted, binary, untracked),
//! a revert really restores or removes the file on disk and only after its confirmation, a
//! thread in a subfolder of the repository, the missing-file and no-repository answers; and the
//! whole path the user takes: the mock provider's agent edits files in a repository (an edit
//! card, a new file, a change made by a terminal command), the turn footer lists them from git,
//! and the diff tab shows them.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, Entity, HeadlessAppContext, px, size};
use hermes_core::Core;
use hermes_runtime::RuntimeConfig;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::data::{DiffRequest, Revert, Scope, TurnContext, parse_patch};
use crate::model::{AppModel, Backend, CoreBackend, Selection};
use crate::testing::{draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::thread::model::RowKind;
use crate::ui_state::UiState;
use crate::workspace::Workspace;

fn git(cwd: &Path, args: &[&str]) -> String {
    let (ok, out) = hermes_core::git::git(cwd, args);
    assert!(ok, "git {args:?} failed in {}", cwd.display());
    out
}

fn lines(n: usize) -> String {
    (1..=n).map(|n| format!("line {n}\n")).collect()
}

/// A repository with one commit of five files and a subfolder.
fn make_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "core.autocrlf", "false"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join("b.txt"), "b1\n").unwrap();
    std::fs::write(repo.join("old.txt"), lines(20)).unwrap();
    std::fs::write(repo.join("gone.txt"), "bye\n").unwrap();
    std::fs::write(repo.join("logo.bin"), [0u8, 1, 2, 3]).unwrap();
    std::fs::create_dir(repo.join("sub")).unwrap();
    std::fs::write(repo.join("sub").join("x.txt"), "x\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "first"]);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// A core on a temporary data folder with a session whose folder is `cwd`.
struct CoreWorld {
    backend: Arc<CoreBackend>,
    core: Arc<Core>,
    session: String,
    // Drop order: the core, then the runtime that ran it, then the folders.
    _runtime: tokio::runtime::Runtime,
    _data: hermes_testdir::TestDir,
}

fn core_world(cwd: &Path, base_url: &str) -> CoreWorld {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let data = hermes_testdir::tempdir().unwrap();
    std::fs::write(
        data.path().join("config.json"),
        json!({
            "providers": [{
                "id": "mock", "name": "Mock endpoint",
                "base_url": base_url,
                "models": ["mock"], "discover_models": false
            }],
            "provider": "mock", "model": "mock"
        })
        .to_string(),
    )
    .unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = runtime
        .block_on(async { Core::new(config, tokio::runtime::Handle::current()) })
        .unwrap();
    let provider = hermes_runtime::ProviderConfig {
        id: "mock".into(),
        base_url: base_url.to_string(),
        api_key: Some("key".into()),
    };
    let (record, _) = core
        .create_session(hermes_core::NewSession {
            provider,
            model: "mock".into(),
            cwd: cwd.to_path_buf(),
            title: Some("Diff".into()),
            seeded: vec![],
            parent: None,
        })
        .unwrap();
    CoreWorld {
        backend: Arc::new(CoreBackend::new(core.clone())),
        core,
        session: record.id,
        _runtime: runtime,
        _data: data,
    }
}

fn request(world: &CoreWorld, scope: Scope, base: Option<&str>, files: &[&str]) -> DiffRequest {
    DiffRequest {
        session: world.session.clone(),
        scope,
        turn: TurnContext {
            base: base.map(str::to_string),
            files: files.iter().map(|f| f.to_string()).collect(),
        },
    }
}

fn letters(patch: &str) -> Vec<(String, char)> {
    parse_patch(patch)
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.status.letter()))
        .collect()
}

#[core::prelude::v1::test]
fn the_three_scopes_read_a_real_repository() {
    let repo = hermes_testdir::tempdir().unwrap();
    make_repo(repo.path());
    let w = core_world(repo.path(), "http://127.0.0.1:9/v1");
    let before = w.backend.head_commit(&w.session).expect("a commit");
    assert_eq!(before, git(repo.path(), &["rev-parse", "HEAD"]).trim());

    // Working-tree edits of every kind.
    git(repo.path(), &["mv", "old.txt", "new name.txt"]);
    std::fs::write(
        repo.path().join("new name.txt"),
        lines(20).replace("line 3\n", "line three\n"),
    )
    .unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    std::fs::write(repo.path().join("logo.bin"), [0u8, 9, 9, 9]).unwrap();
    std::fs::write(repo.path().join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.path().join("brand new.txt"), "x\ny\n").unwrap();

    let tree = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    assert_eq!(tree.revert, Revert::ToHead);
    assert!(!tree.narrowed && tree.note.is_none());
    let parsed = parse_patch(&tree.patch);
    assert!(parsed.unreadable.is_empty(), "{:?}", parsed.unreadable);
    let kinds = letters(&tree.patch);
    assert_eq!(
        kinds,
        vec![
            ("a.txt".to_string(), 'M'),
            ("brand new.txt".to_string(), 'A'),
            ("gone.txt".to_string(), 'D'),
            ("logo.bin".to_string(), 'M'),
            ("new name.txt".to_string(), 'R'),
        ]
    );
    let entry = |name: &str| {
        parsed
            .entries
            .iter()
            .find(|e| e.path == name)
            .unwrap()
            .clone()
    };
    assert_eq!((entry("a.txt").added, entry("a.txt").removed), (1, 0));
    assert!(entry("logo.bin").binary);
    assert_eq!(entry("new name.txt").original.as_deref(), Some("old.txt"));
    assert_eq!(entry("brand new.txt").added, 2);

    // This turn, from the commit it began at: the same files, nothing committed since.
    let turn = w
        .backend
        .diff(&request(&w, Scope::Turn, Some(&before), &[]))
        .unwrap();
    assert_eq!(turn.revert, Revert::None, "no revert from the turn's diff");
    assert_eq!(letters(&turn.patch), kinds);
    // The turn commits and edits once more: its diff keeps the commit, the tree has only the edit.
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "turn"]);
    std::fs::write(repo.path().join("b.txt"), "b1\nb2\n").unwrap();
    let turn = w
        .backend
        .diff(&request(&w, Scope::Turn, Some(&before), &[]))
        .unwrap();
    assert_eq!(letters(&turn.patch).len(), 6, "{:?}", letters(&turn.patch));
    let tree = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    assert_eq!(letters(&tree.patch), [("b.txt".to_string(), 'M')]);

    // A turn whose base is not known: the working tree, narrowed to what the turn's cards named;
    // with nothing named, no file and a note.
    let narrowed = w
        .backend
        .diff(&request(&w, Scope::Turn, None, &["b.txt"]))
        .unwrap();
    assert!(narrowed.narrowed && narrowed.revert == Revert::None);
    assert_eq!(letters(&narrowed.patch), [("b.txt".to_string(), 'M')]);
    let nothing = w
        .backend
        .diff(&request(&w, Scope::Turn, None, &[]))
        .unwrap();
    assert!(nothing.patch.is_empty() && nothing.note.is_some());
    // A base that is gone (history rewritten): the narrowed working tree, not an error.
    let gone = w
        .backend
        .diff(&request(
            &w,
            Scope::Turn,
            Some("deadbeefdeadbeef"),
            &["b.txt"],
        ))
        .unwrap();
    assert!(gone.narrowed);

    // This thread: it has not edited anything, so the runtime says so (no file, a note).
    let thread = w
        .backend
        .diff(&request(&w, Scope::Thread, None, &[]))
        .unwrap();
    assert!(thread.patch.is_empty());
    assert!(
        thread
            .note
            .as_deref()
            .is_some_and(|n| n.contains("hasn't edited")),
        "{:?}",
        thread.note
    );
    assert_eq!(thread.revert, Revert::None);
}

#[core::prelude::v1::test]
fn a_folder_that_is_not_a_repository_or_is_gone_answers_with_a_note() {
    let plain = hermes_testdir::tempdir().unwrap();
    std::fs::write(plain.path().join("a.txt"), "x").unwrap();
    let w = core_world(plain.path(), "http://127.0.0.1:9/v1");
    let data = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    assert!(data.patch.is_empty());
    assert_eq!(
        data.note.as_deref(),
        Some("This folder is not a git repository.")
    );
    assert_eq!(w.backend.head_commit(&w.session), None);
    // Open in editor: a file that is not there is refused before anything starts.
    let error = w
        .backend
        .open_in_editor(&w.session, "missing.txt", Some(3))
        .unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    // Nothing outside the repository: no absolute path, no `..`.
    for outside in [r"..\a.txt", r"C:\Windows\notepad.exe", "/etc/x", "sub/../../a.txt"] {
        let error = w
            .backend
            .open_in_editor(&w.session, outside, None)
            .unwrap_err();
        assert!(error.contains("not a file of this repository"), "{outside}: {error}");
    }

    let gone = hermes_testdir::tempdir().unwrap();
    let w = core_world(gone.path(), "http://127.0.0.1:9/v1");
    let path = gone.path().to_path_buf();
    drop(gone);
    assert!(!path.exists());
    let data = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    assert!(
        data.note
            .as_deref()
            .is_some_and(|n| n.contains("no longer exists"))
    );
    assert_eq!(w.backend.head_commit(&w.session), None);
}

#[core::prelude::v1::test]
fn a_revert_restores_or_removes_the_file_on_disk() {
    let repo = hermes_testdir::tempdir().unwrap();
    make_repo(repo.path());
    let w = core_world(repo.path(), "http://127.0.0.1:9/v1");
    // As the panel does it: with the blob the diff it showed names for the file.
    let revert = |path: &str| {
        let tree = request(&w, Scope::WorkingTree, None, &[]);
        let shown = parse_patch(&w.backend.diff(&tree).unwrap().patch);
        let seen = shown.blobs.get(path).cloned();
        assert!(seen.is_some(), "{path} has a blob in the diff");
        w.backend.revert_file(&tree, path, seen.as_deref())
    };

    std::fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    std::fs::write(repo.path().join("stray.txt"), "x\n").unwrap();
    std::fs::write(repo.path().join("sub").join("x.txt"), "edited\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "keep my edit\n").unwrap();
    // A staged change goes back to HEAD too.
    std::fs::write(repo.path().join("old.txt"), "staged\n").unwrap();
    git(repo.path(), &["add", "old.txt"]);

    revert("a.txt").unwrap();
    revert("old.txt").unwrap();
    assert_eq!(read(&repo.path().join("old.txt")), lines(20));
    assert_eq!(read(&repo.path().join("a.txt")), "one\n");
    revert("gone.txt").unwrap();
    assert_eq!(
        read(&repo.path().join("gone.txt")),
        "bye\n",
        "a deleted file comes back"
    );
    revert("stray.txt").unwrap();
    assert!(
        !repo.path().join("stray.txt").exists(),
        "an untracked file is deleted"
    );
    revert("sub/x.txt").unwrap();
    assert_eq!(read(&repo.path().join("sub").join("x.txt")), "x\n");
    // Only what was asked for: b.txt keeps its edit.
    assert_eq!(read(&repo.path().join("b.txt")), "keep my edit\n");
    assert_eq!(
        letters(
            &w.backend
                .diff(&request(&w, Scope::WorkingTree, None, &[]))
                .unwrap()
                .patch
        ),
        [("b.txt".to_string(), 'M')]
    );
    // A file with no change left (the list was out of date), or a name that is no file of the
    // repository, is not "reverted": the answer says nothing was done.
    let tree = request(&w, Scope::WorkingTree, None, &[]);
    for path in ["a.txt", "no such file.txt", "\"b.txt\""] {
        let error = w.backend.revert_file(&tree, path, None).unwrap_err();
        assert!(error.contains("nothing was done"), "{path}: {error}");
    }
    // A turn cannot.
    let error = w
        .backend
        .revert_file(&request(&w, Scope::Turn, Some("abc"), &[]), "b.txt", None)
        .unwrap_err();
    assert!(error.contains("cannot be reverted"), "{error}");
    assert_eq!(read(&repo.path().join("b.txt")), "keep my edit\n");
}

#[core::prelude::v1::test]
fn a_file_changed_after_the_diff_was_read_is_not_reverted() {
    let repo = hermes_testdir::tempdir().unwrap();
    make_repo(repo.path());
    let w = core_world(repo.path(), "http://127.0.0.1:9/v1");
    let tree = request(&w, Scope::WorkingTree, None, &[]);
    std::fs::write(repo.path().join("a.txt"), "seen\n").unwrap();
    std::fs::write(repo.path().join("new file (1).txt"), "seen\n").unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    let shown = parse_patch(&w.backend.diff(&tree).unwrap().patch);
    let seen = |path: &str| shown.blobs.get(path).cloned().unwrap();
    assert_eq!(seen("gone.txt"), "0000000");

    // The agent (or an editor) writes again before the user confirms: refused, file untouched.
    std::fs::write(repo.path().join("a.txt"), "written later\n").unwrap();
    std::fs::write(repo.path().join("new file (1).txt"), "written later\n").unwrap();
    std::fs::write(repo.path().join("gone.txt"), "recreated\n").unwrap();
    for path in ["a.txt", "new file (1).txt", "gone.txt"] {
        let error = w
            .backend
            .revert_file(&tree, path, Some(&seen(path)))
            .unwrap_err();
        assert!(error.contains("changed after the diff was read"), "{path}: {error}");
    }
    assert_eq!(read(&repo.path().join("a.txt")), "written later\n");
    assert_eq!(read(&repo.path().join("new file (1).txt")), "written later\n");
    assert_eq!(read(&repo.path().join("gone.txt")), "recreated\n");

    // Put back as shown: the same blobs, so the reverts go through (a name with spaces and
    // parentheses is one literal path to git).
    std::fs::write(repo.path().join("a.txt"), "seen\n").unwrap();
    std::fs::write(repo.path().join("new file (1).txt"), "seen\n").unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    for path in ["a.txt", "new file (1).txt", "gone.txt"] {
        w.backend
            .revert_file(&tree, path, Some(&seen(path)))
            .unwrap();
    }
    assert_eq!(read(&repo.path().join("a.txt")), "one\n");
    assert!(!repo.path().join("new file (1).txt").exists());
    assert_eq!(read(&repo.path().join("gone.txt")), "bye\n");
}

#[core::prelude::v1::test]
fn a_thread_in_a_subfolder_names_files_from_the_repository_root() {
    let repo = hermes_testdir::tempdir().unwrap();
    make_repo(repo.path());
    let sub = repo.path().join("sub");
    let w = core_world(&sub, "http://127.0.0.1:9/v1");
    std::fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    std::fs::write(sub.join("x.txt"), "edited\n").unwrap();
    let data = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    // `git diff` names files from the repository's root, whatever the thread's folder is.
    assert_eq!(
        letters(&data.patch),
        [("a.txt".to_string(), 'M'), ("sub/x.txt".to_string(), 'M')]
    );
    let blobs = parse_patch(&data.patch).blobs;
    w.backend
        .revert_file(
            &request(&w, Scope::WorkingTree, None, &[]),
            "a.txt",
            blobs.get("a.txt").map(String::as_str),
        )
        .unwrap();
    assert_eq!(read(&repo.path().join("a.txt")), "one\n");
    w.backend
        .revert_file(
            &request(&w, Scope::WorkingTree, None, &[]),
            "sub/x.txt",
            blobs.get("sub/x.txt").map(String::as_str),
        )
        .unwrap();
    assert_eq!(read(&sub.join("x.txt")), "x\n");

    // An untracked file is named from the top as well, so the revert deletes that file and
    // not a namesake at the top (which is tracked and keeps its edit).
    std::fs::write(sub.join("b.txt"), "loose in sub\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "top edit\n").unwrap();
    let data = w
        .backend
        .diff(&request(&w, Scope::WorkingTree, None, &[]))
        .unwrap();
    assert_eq!(
        letters(&data.patch),
        [("b.txt".to_string(), 'M'), ("sub/b.txt".to_string(), 'A')]
    );
    let blobs = parse_patch(&data.patch).blobs;
    w.backend
        .revert_file(
            &request(&w, Scope::WorkingTree, None, &[]),
            "sub/b.txt",
            blobs.get("sub/b.txt").map(String::as_str),
        )
        .unwrap();
    assert!(!sub.join("b.txt").exists());
    assert_eq!(read(&repo.path().join("b.txt")), "top edit\n");
}

// --- the whole path: an agent edits a repository --------------------------------------------------------

/// The mock model: request 0 patches `a.txt`, 1 writes `agent.txt`, 2 appends to `b.txt` from a
/// terminal command (no edit card for it), then it answers.
struct Agent {
    count: AtomicUsize,
}

impl Respond for Agent {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        let chunk = |delta: Value, finish: Option<&str>| {
            json!({"id": format!("r{n}"), "object": "chat.completion.chunk", "model": "mock",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        };
        let call = |name: &str, args: Value| {
            chunk(
                json!({"role": "assistant", "tool_calls": [{"index": 0, "id": format!("call_{n}"),
                    "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}),
                Some("tool_calls"),
            )
        };
        let first = match n {
            0 => call(
                "patch",
                json!({"path": "a.txt", "old_string": "one", "new_string": "two"}),
            ),
            1 => call(
                "write_file",
                json!({"path": "agent.txt", "content": "hello\nagent\n"}),
            ),
            2 => call("terminal", json!({"command": "echo more >> b.txt"})),
            _ => chunk(
                json!({"role": "assistant", "content": "Done."}),
                Some("stop"),
            ),
        };
        let usage = json!({"id": format!("r{n}"), "object": "chat.completion.chunk",
            "model": "mock", "choices": [],
            "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}});
        let body = format!("data: {first}\n\ndata: {usage}\n\ndata: [DONE]\n\n");
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }
}

fn wait(
    cx: &mut HeadlessAppContext,
    what: &str,
    mut done: impl FnMut(&mut HeadlessAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[core::prelude::v1::test]
fn an_agents_edits_show_in_the_turn_footer_and_in_the_diff_tab_from_git() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Agent {
                count: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        server
    });
    let repo = hermes_testdir::tempdir().unwrap();
    make_repo(repo.path());
    // Untracked clutter that was there before the turn: not the turn's.
    std::fs::write(repo.path().join("scratch.txt"), "old clutter\n").unwrap();
    let world = core_world(repo.path(), &format!("{}/v1", server.uri()));
    // The agent's commands and edits run without asking (nobody is there to answer).
    world
        .core
        .runtime()
        .set_approval_mode(&world.session, hermes_tools::ApprovalMode::Yolo)
        .unwrap();

    let mut cx = headless_app();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        crate::keymap::bind(cx);
    });
    let model = {
        let backend = world.backend.clone();
        cx.update(|cx| cx.new(|cx| AppModel::with_backend(backend, cx)))
    };
    let root = model.clone();
    let mut state = UiState::default();
    state.sidebar.open = false;
    state.right_panel.open = true;
    let handle = open_window(&mut cx, size(px(1100.), px(700.)), move |window, cx| {
        cx.new(|cx| Workspace::with_model(state, None, root, window, cx))
    });
    let window: gpui_kit::AnyWindowHandle = handle.into();
    let workspace: Entity<Workspace> = cx
        .update_window(window, |root, _, _| root.downcast().unwrap())
        .unwrap();
    let session = world.session.clone();
    {
        let (model, id) = (model.clone(), session.clone());
        wait(&mut cx, "the list", |cx| {
            cx.update(|cx| model.read(cx).snapshot().thread(&id).is_some())
        });
        cx.update(|cx| model.update(cx, |m, cx| m.open(&id, cx)));
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|cx| model.read(cx).selected().clone()),
        Selection::Thread(session)
    );
    let thread = cx.update(|cx| model.read(cx).thread().cloned()).unwrap();
    // The panel read the working tree when the thread opened: the clutter is the only change.
    let panel = cx.update(|cx| workspace.read(cx).right_panel().read(cx).diff().clone());
    {
        let panel = panel.clone();
        wait(&mut cx, "the first read", |cx| {
            cx.update(|cx| !panel.read(cx).entries().is_empty())
        });
    }
    assert_eq!(
        cx.update(|cx| panel
            .read(cx)
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()),
        ["scratch.txt"]
    );

    // The agent works.
    cx.update(|cx| thread.update(cx, |t, cx| t.submit("edit the files", cx)));
    {
        let thread = thread.clone();
        wait(&mut cx, "the turn's footer", |cx| {
            cx.update(|cx| {
                thread
                    .read(cx)
                    .rows()
                    .iter()
                    .any(|r| matches!(&r.kind, RowKind::Footer(f) if !f.files.is_empty()))
            })
        });
    }
    // The footer names the files git says the turn changed, which includes the one a terminal
    // command changed (no edit card) and leaves out the clutter that was already there.
    let footer_files = |cx: &mut HeadlessAppContext| -> Vec<(String, u32, u32)> {
        cx.update(|cx| {
            thread
                .read(cx)
                .rows()
                .iter()
                .find_map(|r| match &r.kind {
                    RowKind::Footer(f) => Some(
                        f.files
                            .iter()
                            .map(|c| (c.path.clone(), c.additions, c.deletions))
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default()
        })
    };
    {
        // The footer is first the cards' list; git's account replaces it a moment later.
        let mut seen = Vec::new();
        wait(&mut cx, "git's account of the turn", |cx| {
            seen = footer_files(cx);
            seen.iter().any(|f| f.0 == "b.txt")
        });
        let names: Vec<&str> = seen.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, ["a.txt", "agent.txt", "b.txt"], "{seen:?}");
        let counts = |name: &str| {
            seen.iter()
                .find(|f| f.0 == name)
                .map(|f| (f.1, f.2))
                .unwrap()
        };
        assert_eq!(counts("a.txt"), (1, 1), "git's counts for the patched file");
        assert_eq!(counts("agent.txt"), (2, 0));
        assert_eq!(
            counts("b.txt"),
            (1, 0),
            "changed by a command: only git knows"
        );
    }
    // The files are really on disk.
    assert_eq!(read(&repo.path().join("a.txt")), "two\n");
    assert_eq!(read(&repo.path().join("agent.txt")), "hello\nagent\n");
    assert!(read(&repo.path().join("b.txt")).ends_with("more\n"));

    // Open diff on one of them: the turn's changes, scrolled to it; the clutter is not among them.
    cx.update_window(window, |_, window, cx| {
        workspace.update(cx, |w, cx| w.focus(window, cx));
        window.dispatch_action(
            Box::new(crate::thread::OpenDiffTab {
                path: Some("agent.txt".into()),
            }),
            cx,
        );
    })
    .unwrap();
    {
        let panel = panel.clone();
        wait(&mut cx, "the turn's diff", |cx| {
            draw(cx, window);
            cx.update(|cx| {
                panel.read(cx).scope() == Scope::Turn && panel.read(cx).entries().len() == 3
            })
        });
    }
    let entries: Vec<(String, char)> = cx.update(|cx| {
        panel
            .read(cx)
            .entries()
            .iter()
            .map(|e| (e.path.clone(), e.status.letter()))
            .collect()
    });
    assert_eq!(
        entries,
        [
            ("a.txt".to_string(), 'M'),
            ("agent.txt".to_string(), 'A'),
            ("b.txt".to_string(), 'M')
        ]
    );
    let l = layout(&mut cx, window);
    assert!(l.get("diff-panel").is_ok() && l.get("diff-row:2").is_ok());
    // The working tree shows the same three and the clutter.
    cx.update(|cx| panel.update(cx, |p, cx| p.set_scope(Scope::WorkingTree, cx)));
    {
        let panel = panel.clone();
        wait(&mut cx, "the working tree", |cx| {
            cx.update(|cx| panel.read(cx).entries().len() == 4)
        });
    }
    // Reverting a.txt from the panel asks first; the file changes only after the confirmation.
    cx.update(|cx| panel.update(cx, |p, cx| p.ask_revert("a.txt", cx)));
    assert_eq!(
        read(&repo.path().join("a.txt")),
        "two\n",
        "asking reverts nothing"
    );
    cx.update(|cx| panel.update(cx, |p, cx| p.confirm_revert(cx)));
    wait(&mut cx, "the revert", |_| {
        read(&repo.path().join("a.txt")) == "one\n"
    });
    {
        let panel = panel.clone();
        wait(&mut cx, "the list without a.txt", |cx| {
            cx.update(|cx| panel.read(cx).entries().iter().all(|e| e.path != "a.txt"))
        });
    }
    // Teardown in the order the other real-core tests use.
    drop(workspace);
    drop(thread);
    drop(model);
    drop(cx);
    drop(world);
    drop(server);
    drop(runtime);
}
