//! Sessions over a real runtime on a temporary data folder (no model is called).

use std::sync::Arc;

use hermes_llm::conversation::message::Message;
use hermes_runtime::{ProviderConfig, RuntimeConfig};

use super::*;
use crate::events::CoreEvent;

struct Fixture {
    core: Arc<Core>,
    events: tokio::sync::mpsc::UnboundedReceiver<CoreEvent>,
    work: hermes_testdir::TestDir,
    // Last: the runtime above holds files in it until it drops.
    _data: hermes_testdir::TestDir,
}

fn fixture() -> Fixture {
    let data = hermes_testdir::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = Core::new(config, tokio::runtime::Handle::current()).unwrap();
    let events = core.subscribe();
    Fixture {
        core,
        events,
        work: hermes_testdir::tempdir().unwrap(),
        _data: data,
    }
}

fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "mock".into(),
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key: None,
    }
}

fn new_session(f: &Fixture, seeded: Vec<Message>, parent: Option<&str>) -> String {
    let (record, _) = f
        .core
        .create_session(NewSession {
            provider: provider(),
            model: "mock".into(),
            cwd: f.work.path().to_path_buf(),
            title: Some("Fix the parser".into()),
            seeded,
            parent: parent.map(str::to_string),
        })
        .unwrap();
    record.id
}

fn conversation() -> Vec<Message> {
    vec![
        Message::user()
            .with_text("first")
            .with_generated_id_if_missing(),
        Message::assistant()
            .with_text("one")
            .with_generated_id_if_missing(),
        Message::user()
            .with_text("second")
            .with_generated_id_if_missing(),
        Message::assistant()
            .with_text("two")
            .with_generated_id_if_missing(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_created_session_is_listed_with_its_title_and_parent() {
    let f = fixture();
    let parent = new_session(&f, vec![], None);
    let child = new_session(&f, conversation(), Some(&parent));
    let all = f.core.sessions(archive::Filter::Include);
    let row = all.iter().find(|s| s.record.id == child).unwrap();
    assert_eq!(row.record.title.as_deref(), Some("Fix the parser"));
    assert_eq!(row.parent.as_deref(), Some(parent.as_str()));
    assert_eq!(row.message_count, 4);
    assert!(!row.running && !row.waiting && !row.archived);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rename_is_trimmed_stored_and_announced() {
    let mut f = fixture();
    let id = new_session(&f, vec![], None);
    f.core.rename(&id, "  New name ").unwrap();
    let record = f.core.runtime().store().session(&id).unwrap().unwrap();
    assert_eq!(record.title.as_deref(), Some("New name"));
    match f.events.try_recv().unwrap() {
        CoreEvent::TitleChanged { session_id, title } => {
            assert_eq!(
                (session_id.as_str(), title.as_str()),
                (id.as_str(), "New name")
            )
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_hides_unpins_and_deleting_forgets() {
    let f = fixture();
    let id = new_session(&f, vec![], None);
    f.core.runtime().store().set_pinned(&id, true).unwrap();
    assert!(f.core.set_archived(&id, true).unwrap());
    assert!(
        !f.core.set_archived("nope", true).unwrap(),
        "no such session"
    );
    let record = f.core.runtime().store().session(&id).unwrap().unwrap();
    assert!(!record.pinned, "an archived session leaves the pins");
    assert!(
        f.core
            .sessions(archive::Filter::Exclude)
            .iter()
            .all(|s| s.record.id != id)
    );
    assert!(
        f.core
            .sessions(archive::Filter::Only)
            .iter()
            .any(|s| s.record.id == id && s.archived)
    );
    f.core.delete_session(&id).await.unwrap();
    assert!(!f.core.archive().is_archived(&id));
    assert!(f.core.runtime().store().session(&id).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_cuts_where_asked_and_a_retry_gets_the_same_branch() {
    let f = fixture();
    let parent = new_session(&f, conversation(), None);
    let request = |point, key: Option<&str>| BranchRequest {
        parent: parent.clone(),
        point,
        cwd: None,
        name: None,
        idempotency_key: key.map(str::to_string),
    };
    let ok = || Ok::<_, ()>(provider());
    let made = f
        .core
        .branch(request(BranchPoint::ThroughRow(2), Some("k1")), ok)
        .unwrap();
    assert_eq!(made.kept.len(), 2);
    assert_eq!(made.parent.id, parent);
    let record = f
        .core
        .runtime()
        .store()
        .session(&made.child)
        .unwrap()
        .unwrap();
    assert_eq!(record.title.as_deref(), Some("Fix the parser (branch)"));
    assert_eq!(
        f.core.branches().parent(&made.child).as_deref(),
        Some(parent.as_str())
    );
    // The same key: the same branch, the provider not asked.
    let again = f
        .core
        .branch(request(BranchPoint::Whole, Some("k1")), || {
            Err::<ProviderConfig, _>("not asked")
        })
        .unwrap();
    assert_eq!(again.child, made.child);
    // Through the third visible entry: the second typed message's row.
    let third = f
        .core
        .branch(request(BranchPoint::ThroughEntry(3), None), ok)
        .unwrap();
    assert_eq!(third.kept.len(), 3);
    let second = f
        .core
        .runtime()
        .store()
        .session(&third.child)
        .unwrap()
        .unwrap();
    assert_eq!(second.title.as_deref(), Some("Fix the parser (branch 2)"));
    // Nothing to branch, an unknown parent, a provider that can't be found.
    assert!(matches!(
        f.core.branch(request(BranchPoint::ThroughRow(0), None), ok),
        Err(BranchError::Empty)
    ));
    let unknown = BranchRequest {
        parent: "nope".into(),
        ..request(BranchPoint::Whole, None)
    };
    assert!(matches!(
        f.core.branch(unknown, ok),
        Err(BranchError::UnknownSession(id)) if id == "nope"
    ));
    assert!(matches!(
        f.core.branch(
            request(BranchPoint::Whole, None),
            || Err::<ProviderConfig, _>(7)
        ),
        Err(BranchError::Provider(7))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rewind_drops_the_typed_message_and_what_followed() {
    let f = fixture();
    let id = new_session(&f, conversation(), None);
    let rewound = f.core.rewind_to_row(&id, 3).unwrap().unwrap();
    assert_eq!((rewound.removed, rewound.typed.as_str()), (2, "second"));
    assert_eq!(f.core.runtime().messages(&id).unwrap().len(), 2);
    assert!(f.core.rewind_to_row(&id, 9).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_to_a_missing_folder_is_refused() {
    let f = fixture();
    let id = new_session(&f, vec![], None);
    let missing = f.work.path().join("no-such-folder");
    assert!(matches!(
        f.core.move_session(&id, missing).await,
        Err(MoveError::InvalidFolder(_))
    ));
    let elsewhere = f.work.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let (cwd, branch, _) = f.core.move_session(&id, elsewhere).await.unwrap();
    assert!(cwd.ends_with("elsewhere"), "{cwd}");
    assert_eq!(branch, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_idle_sweep_archives_old_sessions_once() {
    let f = fixture();
    let id = new_session(&f, vec![], None);
    // Idle zero days: anything not touched this millisecond counts as idle.
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert_eq!(f.core.auto_archive(0.0), vec![id.clone()]);
    assert!(f.core.auto_archive(0.0).is_empty(), "once a day");
    f.core.reopened(&id);
    assert!(!f.core.archive().is_archived(&id), "used again: back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_keeps_its_primary_folder_first() {
    let f = fixture();
    let made = f
        .core
        .create_project(
            "Mine",
            vec!["C:/a".into(), "C:/b".into()],
            Some("C:/b".into()),
        )
        .unwrap();
    assert_eq!(made.folders, ["C:/b", "C:/a"]);
    let edit = crate::projects::ProjectEdit {
        name: Some(" Ours ".into()),
        folder: Some("C:/a".into()),
        primary: true,
    };
    let edited = f.core.update_project(&made.id, edit).unwrap().unwrap();
    assert_eq!(
        (edited.name.as_str(), edited.folders.clone()),
        ("Ours", vec!["C:/a".to_string(), "C:/b".to_string()])
    );
    assert_eq!(f.core.projects()[0].folders, ["C:/a", "C:/b"]);
    assert!(
        f.core
            .update_project("nope", Default::default())
            .unwrap()
            .is_none()
    );
    f.core.delete_project(&made.id).unwrap();
    assert!(f.core.projects().is_empty());
}
