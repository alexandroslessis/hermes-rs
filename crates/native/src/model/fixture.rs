//! Fake projects and threads for the gallery, the snapshots, the tests and `hermes-native
//! --fixture`: the names are the ones in t3code's screenshots (structure, engine, hermes-rs; the
//! threads halo, ayo, hi), plus a project with no thread, an automatic repository, a "No project"
//! thread and enough threads in one project to show "Show more". The clock is fixed
//! ([`NOW_MS`]), so the relative times in a snapshot never change.

use crate::sidebar::data::{
    NO_PROJECT, NO_PROJECT_LABEL, ProjectItem, ProjectKind, Snapshot, ThreadItem,
};

/// The fixed "now" of every fixture and snapshot (an arbitrary instant).
pub const NOW_MS: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn project(id: &str, name: &str, path: &str, kind: ProjectKind) -> ProjectItem {
    ProjectItem {
        id: id.into(),
        name: name.into(),
        path: Some(path.into()),
        kind,
    }
}

fn thread(id: &str, title: &str, project: &ProjectItem, ago_ms: i64) -> ThreadItem {
    ThreadItem {
        id: id.into(),
        title: title.into(),
        project: project.id.clone(),
        cwd: project.path.clone().unwrap_or_default(),
        pinned: false,
        updated_ms: NOW_MS - ago_ms,
        working: false,
        waiting: false,
        branch: None,
    }
}

/// The projects of the screenshots, with their threads.
pub fn snapshot() -> Snapshot {
    let structure = project(
        "prj_structure",
        "structure",
        r"C:\work\structure",
        ProjectKind::User,
    );
    let engine = project("prj_engine", "engine", r"C:\work\engine", ProjectKind::User);
    let hermes = project(
        "prj_hermes",
        "hermes-rs",
        r"C:\Users\me\repos\hermes-rs",
        ProjectKind::User,
    );
    let docs = project("prj_docs", "docs", r"C:\work\docs", ProjectKind::User);
    let auto = project(
        r"C:\repos\scratch-repo",
        "scratch-repo",
        r"C:\repos\scratch-repo",
        ProjectKind::Auto,
    );
    let threads = vec![
        thread("t1", "halo", &structure, 5 * MIN),
        thread("t2", "ayo", &engine, 7 * MIN),
        thread("t3", "hi", &hermes, 20 * HOUR),
        thread("t4", "Fix the sidebar width", &hermes, 3 * MIN),
        thread("t5", "Port the command palette", &hermes, DAY + HOUR),
        thread("t7", "Try the repository", &auto, 3 * DAY),
    ];
    let mut threads = threads;
    threads[3].branch = Some("native/p2-design".into());
    threads[0].branch = Some("master".into());
    let none = ProjectItem {
        id: NO_PROJECT.into(),
        name: NO_PROJECT_LABEL.into(),
        path: None,
        kind: ProjectKind::None,
    };
    let mut scratch = thread("t6", "Quick question about regex", &none, 30_000);
    scratch.cwd = r"C:\Users\me".into();
    threads.push(scratch);
    Snapshot {
        projects: vec![structure, engine, hermes, docs, auto, none],
        threads,
    }
}

/// A project with `count` threads (for "Show more").
pub fn many(count: usize) -> Snapshot {
    let big = project(
        "prj_big",
        "big-project",
        r"C:\work\big-project",
        ProjectKind::User,
    );
    let threads = (0..count)
        .map(|i| {
            thread(
                &format!("m{i}"),
                &format!("Thread number {}", count - i),
                &big,
                (i as i64 + 1) * HOUR,
            )
        })
        .collect();
    Snapshot {
        projects: vec![big],
        threads,
    }
}

/// An empty app: no project, no thread.
pub fn empty() -> Snapshot {
    Snapshot::default()
}
