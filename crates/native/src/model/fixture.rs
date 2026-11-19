//! Fake projects and threads for the gallery, the snapshots, the tests and `hermes-native
//! --fixture`: the names are the ones in t3code's screenshots (structure, engine, hermes-rs; the
//! threads halo, ayo, hi), plus a project with no thread, an automatic repository, a "No project"
//! thread and enough threads in one project to show "Show more". The clock is fixed
//! ([`NOW_MS`]), so the relative times in a snapshot never change.
//!
//! The threads have histories ([`history`]): "halo" is the exchange of `app-thread-dark.png`,
//! "ayo" the primes code of `app-codeblock_closeup-dark.png` with a message carrying two images,
//! "Fix the sidebar width" every markdown kind the thread view extends (alerts, a wide table, file
//! links, code) plus a tool call and a failed turn; [`long_history`] makes a thread of any length.

use std::sync::Arc;

use crate::sidebar::data::{
    NO_PROJECT, NO_PROJECT_LABEL, ProjectItem, ProjectKind, Snapshot, ThreadItem,
};
use crate::thread::data::{Attachment, HistoryItem};

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

fn user(text: &str, ago_ms: i64) -> HistoryItem {
    HistoryItem::User {
        text: text.into(),
        images: Vec::new(),
        at_ms: NOW_MS - ago_ms,
    }
}

fn reply(text: &str, ago_ms: i64) -> HistoryItem {
    HistoryItem::Assistant {
        text: text.into(),
        at_ms: NOW_MS - ago_ms,
    }
}

/// Two small images (a gradient and a striped "screenshot") for thumbnails.
pub fn images() -> Vec<Attachment> {
    vec![
        Attachment {
            name: "sunset.png".into(),
            mime: "image/png".into(),
            bytes: Arc::new(include_bytes!("../../fixtures/thumb-sunset.png").to_vec()),
        },
        Attachment {
            name: "stripes.png".into(),
            mime: "image/png".into(),
            bytes: Arc::new(include_bytes!("../../fixtures/thumb-stripes.png").to_vec()),
        },
    ]
}

/// The primes code of `app-codeblock_closeup-dark.png`.
pub const PRIMES: &str = "```python\ndef primes_up_to(n: int) -> list[int]:\n    \"\"\"Return all primes <= n using a Sieve of Eratosthenes.\"\"\"\n    if n < 2:\n        return []\n    sieve = bytearray([1]) * (n + 1)\n    sieve[0:2] = b\"\\x00\\x00\"\n    for i in range(2, int(n**0.5) + 1):\n        if sieve[i]:\n            sieve[i * i :: i] = b\"\\x00\" * len(sieve[i * i :: i])\n    return [i for i, is_prime in enumerate(sieve) if is_prime]\n```";

/// Every markdown kind the thread view extends, in one reply.
pub const RICH: &str = "## Why the sidebar grows\n\nThe width is read from `native-ui.json` before the clamp, so a stale value wins. See [workspace.rs](crates/native/src/workspace.rs#L89) and [the t3code source](https://github.com/pingdotgg/t3code).\n\n> [!WARNING]\n> A width saved on a larger monitor is not clamped until the next drag.\n\n> [!TIP]\n> Double-click the border to reset it.\n\n| Window | Sidebar before | Sidebar after | Right panel before | Right panel after | Main area |\n|---|---|---|---|---|---|\n| 640 x 480 | 256 px (clamped late) | 208 px | hidden | hidden | 432 px |\n| 1100 x 700 | 300 px | 256 px | 420 px | 420 px | 424 px |\n\n```rust\npub fn frame_layout(window: Size<Pixels>, state: &UiState) -> FrameLayout {\n    let w = window.width.as_f32();\n    let sidebar = state.sidebar.size.clamp(MIN, MAX).min(w - MAIN_MIN_WIDTH);\n    FrameLayout { sidebar, ..Default::default() }\n}\n```\n\n1. Clamp on load.\n2. Clamp on every window resize.";

/// The history of a fixture thread (`id`): see the module docs.
pub fn history(id: &str) -> Vec<HistoryItem> {
    match id {
        "t1" => vec![
            user("halo", 5 * MIN),
            HistoryItem::Reasoning {
                text: "The user greets me in a playful way. I answer in kind and offer help."
                    .into(),
                duration_ms: Some(2_800),
            },
            reply("Halo! How can I help you today?", 5 * MIN),
        ],
        "t2" => vec![
            HistoryItem::User {
                text: "Write a primes function like the one in these screenshots.".into(),
                images: images(),
                at_ms: NOW_MS - 9 * MIN,
            },
            reply(
                &format!(
                    "{PRIMES}\n\n`primes_up_to(30)` \u{2192} `[2, 3, 5, 7, 11, 13, 17, 19, 23, 29]`"
                ),
                8 * MIN,
            ),
            user("ayo", 7 * MIN),
            reply("Ayo. Anything else?", 7 * MIN),
        ],
        "t4" => vec![
            user("Why does the sidebar get wider after a restart?", 4 * MIN),
            HistoryItem::Tool {
                name: "read_file".into(),
                summary: "crates/native/src/workspace.rs".into(),
            },
            reply(RICH, 3 * MIN),
            user("Try it on the small window.", 3 * MIN),
            HistoryItem::Failed {
                text: "Your request was not processed. Send it again if you still want me to carry it out.".into(),
            },
        ],
        _ => vec![
            user("hi", 20 * HOUR),
            reply("Hi. What are we working on?", 20 * HOUR),
        ],
    }
}

/// A thread of `count` items: user questions and replies of mixed lengths (some with a list or
/// code), oldest first, a minute apart.
pub fn long_history(count: usize) -> Vec<HistoryItem> {
    (0..count)
        .map(|i| {
            let ago = (count - i) as i64 * MIN;
            if i % 2 == 0 {
                user(&format!("Question {i}: what does step {i} change?"), ago)
            } else {
                let body = match i % 6 {
                    1 => format!("Reply {i}: one short line."),
                    3 => format!(
                        "Reply {i}: a longer answer that wraps over a few lines, because the \
                         timeline must measure rows of different heights and keep the scroll \
                         position steady while it does.\n\n- first point\n- second point"
                    ),
                    _ => {
                        format!("Reply {i}:\n\n```rust\nfn step_{i}() -> u32 {{\n    {i}\n}}\n```")
                    }
                };
                reply(&body, ago)
            }
        })
        .collect()
}
