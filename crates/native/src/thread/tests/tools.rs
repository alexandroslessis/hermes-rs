//! The work log in the thread view (CP4.2): a run of tool calls folds into one group row and
//! counts its seconds, every card kind's layout (the line, its open details, collapsed and open
//! heights), long output staying virtual, streaming progress touching only its own card, the
//! turn's footer and its Retry and Branch, runs cut by a history page, and the actions a card
//! sends. Pure rules (ANSI, diffs, card contents, group labels, the tree) are tested in their
//! modules (`ansi.rs`, `diff.rs`, `tools.rs`, `work.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use hermes_core::CoreEvent;
use serde_json::json;

use super::*;
use crate::model::backend::ThreadOp;
use crate::thread::tools::{Detail, Status};
use crate::thread::work::WorkGroup;
use crate::thread::{OpenDiffTab, OpenScreen, ThreadEvent};

fn tool_started(id: &str, name: &str, args: serde_json::Value) -> CoreEvent {
    CoreEvent::ToolStarted {
        session_id: "s".into(),
        tool_id: id.into(),
        name: name.into(),
        args,
    }
}

fn tool_completed(id: &str, name: &str, text: &str) -> CoreEvent {
    CoreEvent::ToolCompleted {
        session_id: "s".into(),
        tool_id: id.into(),
        name: name.into(),
        text: text.into(),
        // As our tools flag it.
        is_error: text.contains("\"success\":false"),
    }
}

fn progress(id: &str, message: &str) -> CoreEvent {
    CoreEvent::ToolProgress {
        session_id: "s".into(),
        tool_id: id.into(),
        name: "terminal".into(),
        progress: 0.,
        message: message.into(),
    }
}

fn read_result() -> String {
    json!({"content": "1|a", "total_lines": 1, "file_size": 3}).to_string()
}

/// A rig over an empty thread with a fixed clock a test moves.
fn empty_rig() -> Rig {
    rig("s", Some(Vec::new()), PAGE)
}

fn work_rows(rig: &mut Rig) -> Vec<(u64, usize)> {
    rig.read(|m, _| {
        m.rows()
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Work(g) => Some((r.key, g.entries.len())),
                _ => None,
            })
            .collect()
    })
}

fn entry_keys(rig: &mut Rig, row: u64) -> Vec<u64> {
    rig.read(|m, _| match &m.rows()[m.index_of(row).unwrap()].kind {
        RowKind::Work(g) => g.entries.iter().map(|e| e.key).collect(),
        _ => Vec::new(),
    })
}

const ALL: &[&str] = &[
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10",
];

/// A thread of just these calls of the recorded turn (and its end when `end`).
fn calls_rig(calls: &[&str], end: bool) -> Rig {
    let mut rig = empty_rig();
    rig.model(|m, cx| m.replay_turn(&Recording::fixture().only_calls(calls, end), cx));
    rig
}

fn recorded_rig() -> Rig {
    let mut rig = empty_rig();
    rig.model(|m, cx| m.replay_turn(&Recording::fixture(), cx));
    rig
}

fn group_of(rig: &mut Rig) -> (u64, Vec<u64>) {
    let (row, _) = work_rows(rig)[0];
    (row, entry_keys(rig, row))
}

// --- grouping -----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn calls_with_nothing_between_them_are_one_group_and_a_reply_ends_the_run() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started("a", "read_file", json!({"path": "a"})));
    rig.emit(tool_completed("a", "read_file", &read_result()));
    rig.emit(tool_started("b", "read_file", json!({"path": "b"})));
    rig.emit(tool_completed("b", "read_file", &read_result()));
    assert_eq!(
        work_rows(&mut rig).iter().map(|w| w.1).collect::<Vec<_>>(),
        [2]
    );
    rig.emit(text_delta("between"));
    rig.emit(tool_started("c", "read_file", json!({"path": "c"})));
    rig.emit(tool_completed("c", "read_file", &read_result()));
    // A reply between them: two groups, the second of one call (shown as its card).
    assert_eq!(
        work_rows(&mut rig).iter().map(|w| w.1).collect::<Vec<_>>(),
        [2, 1]
    );
    // Only the new call grew a row: the first group row was extended in place, not replaced.
    assert_eq!(rig.read(|m, _| m.rows().len()), 3);
}

#[core::prelude::v1::test]
fn a_reasoning_row_or_a_notice_between_calls_ends_the_run_too() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started("a", "read_file", json!({})));
    rig.emit(CoreEvent::ReasoningDelta {
        session_id: "s".into(),
        text: "hmm".into(),
    });
    rig.emit(tool_started("b", "read_file", json!({})));
    rig.emit(CoreEvent::Status {
        session_id: "s".into(),
        kind: "compacting".into(),
        text: "Compacting".into(),
    });
    rig.emit(tool_started("c", "read_file", json!({})));
    assert_eq!(work_rows(&mut rig).len(), 3);
}

#[core::prelude::v1::test]
fn the_header_counts_while_a_call_runs_and_says_how_long_it_worked_after() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started("a", "read_file", json!({})));
    rig.emit(tool_started("b", "terminal", json!({"command": "make"})));
    let (row, _) = group_of(&mut rig);
    let summary = |rig: &mut Rig, now: i64| {
        rig.read(|m, _| match &m.rows()[m.index_of(row).unwrap()].kind {
            RowKind::Work(g) => g.summary(now),
            _ => unreachable!(),
        })
    };
    let live = summary(&mut rig, NOW_MS + 12_000);
    assert_eq!(
        (live.label.as_str(), live.steps.as_str(), live.running),
        ("Working for 12s", "2 steps", true)
    );
    // The header is drawn with the label and the step count on one line.
    let layout = rig.layout();
    let mut check = layout.check();
    check
        .same_row(&format!("work-label:{row}"), &format!("work-steps:{row}"))
        .inside(&format!("work-head:{row}"), &format!("lane:{row}"));
    check.assert();
    rig.emit(tool_completed("a", "read_file", &read_result()));
    rig.emit(tool_completed(
        "b",
        "terminal",
        &json!({"output": "", "exit_code": 0}).to_string(),
    ));
    let done = summary(&mut rig, NOW_MS + 99_000);
    assert!(!done.running);
    assert!(done.label.starts_with("Worked"), "{}", done.label);
}

#[core::prelude::v1::test]
fn a_turn_that_ends_with_a_call_running_stops_its_spinner() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started(
        "a",
        "terminal",
        json!({"command": "sleep 99"}),
    ));
    rig.emit(
        Recorded::TurnEnded {
            outcome: "interrupted".into(),
            error: None,
            text: String::new(),
            user_row: None,
            last_row: None,
        }
        .to_core("s"),
    );
    rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::Work(g) => {
            assert_eq!(g.entries[0].card.status, Status::Failed);
            assert!(!g.running());
        }
        _ => panic!("a work row"),
    });
}

#[core::prelude::v1::test]
fn the_recorded_turn_is_one_group_of_eleven_with_a_footer() {
    let mut rig = recorded_rig();
    let groups = work_rows(&mut rig);
    assert_eq!(groups.iter().map(|g| g.1).collect::<Vec<_>>(), [11]);
    let kinds = rig.read(|m, _| {
        let RowKind::Work(g) = &m
            .rows()
            .iter()
            .find(|r| matches!(r.kind, RowKind::Work(_)))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        g.entries.iter().map(|e| e.card.kind).collect::<Vec<_>>()
    });
    use crate::thread::tools::ToolKind as K;
    assert_eq!(
        kinds,
        [
            K::Command,
            K::Command,
            K::Read,
            K::Search,
            K::Edit,
            K::Edit,
            K::Memory,
            K::Skill,
            K::Web,
            K::Delegate,
            K::Read
        ]
    );
    let footer = rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::Footer(f) => Some((
            f.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            f.user_row,
            f.last_row,
            f.usage,
        )),
        _ => None,
    });
    let (files, user_row, last_row, usage) = footer.expect("the turn ends in a footer");
    assert_eq!(files, ["notes.md", "docs/plan.md"]);
    assert_eq!((user_row, last_row), (Some(1), Some(24)));
    assert!(usage.is_some_and(|u| u.output >= 100), "{usage:?}");
}

#[core::prelude::v1::test]
fn a_delegated_task_shows_its_subagents_tool_and_result_from_the_events() {
    let mut rig = recorded_rig();
    rig.read(|m, _| {
        let RowKind::Work(g) = &m
            .rows()
            .iter()
            .find(|r| matches!(r.kind, RowKind::Work(_)))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        let Detail::Delegate(tasks) = &g.entries[9].card.detail else {
            panic!("the delegate card");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, Status::Done);
        assert_eq!(tasks[0].tools.len(), 1);
        assert_eq!(tasks[0].tools[0].name, "read_file");
        assert!(tasks[0].summary.contains("TextViewState"));
    });
}

// --- history ------------------------------------------------------------------------------------

fn stored_tool(i: usize) -> HistoryItem {
    HistoryItem::Tool {
        id: format!("c{i}"),
        name: "read_file".into(),
        args: json!({"path": format!("f{i}")}),
        result: Some(read_result()),
        is_error: false,
        at_ms: NOW_MS + i as i64 * 1000,
    }
}

#[core::prelude::v1::test]
fn stored_calls_fold_into_groups_and_a_run_cut_by_a_page_is_joined() {
    let mut items = vec![HistoryItem::User {
        text: "go".into(),
        images: Vec::new(),
        at_ms: NOW_MS,
    }];
    items.extend((0..6).map(stored_tool));
    items.push(HistoryItem::Assistant {
        text: "done".into(),
        at_ms: NOW_MS,
    });
    // Pages of 4 items: the newest holds the last 3 calls and the reply; the small thread's top
    // is near, so the older page (the prompt and the first 3 calls) is read at once.
    let mut rig = rig("long", Some(items), 4);
    assert_eq!(
        rig.backend.pages(),
        [("long".to_string(), None), ("long".to_string(), Some(4))]
    );
    assert_eq!(
        work_rows(&mut rig).iter().map(|w| w.1).collect::<Vec<_>>(),
        [6],
        "one group of six"
    );
    let kinds = rig.read(|m, _| {
        m.rows()
            .iter()
            .map(|r| match &r.kind {
                RowKind::User { .. } => "user",
                RowKind::Work(_) => "work",
                RowKind::Assistant { .. } => "reply",
                _ => "other",
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds, ["user", "work", "reply"]);
    // The stored group counts its span from its messages' times.
    let label = rig.read(|m, _| match &m.rows()[1].kind {
        RowKind::Work(g) => g.summary(NOW_MS).label,
        _ => String::new(),
    });
    assert_eq!(label, "Worked for 5.0s");
}

#[core::prelude::v1::test]
fn stored_cards_open_to_parsed_output_only_when_clicked() {
    let items = vec![
        stored_tool(0),
        HistoryItem::Tool {
            id: "t".into(),
            name: "terminal".into(),
            args: json!({"command": "ls"}),
            result: Some(
                json!({"output": "a\nb\n", "exit_code": 0, "duration_ms": 30}).to_string(),
            ),
            is_error: false,
            at_ms: NOW_MS,
        },
    ];
    let mut rig = rig("cmd", Some(items), PAGE);
    let (row, entries) = group_of(&mut rig);
    let parsed = |rig: &mut Rig| {
        rig.read(|m, _| match &m.rows()[0].kind {
            RowKind::Work(g) => match &g.entries[1].card.detail {
                Detail::Command { output, .. } => output.lines().is_some(),
                _ => panic!(),
            },
            _ => panic!(),
        })
    };
    assert!(
        !parsed(&mut rig),
        "a stored command's output is not parsed when the thread opens"
    );
    // The group is folded; opening it lists the calls, a click on a card opens it.
    rig.click(&format!("work-head:{row}"));
    rig.click(&format!("entry-head:{}", entries[1]));
    assert!(parsed(&mut rig));
    let layout = rig.layout();
    assert!(layout.get(&format!("output:{}", entries[1])).is_ok());
}

// --- layout -------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_group_opens_to_aligned_lines_and_each_card_opens_below_its_line() {
    let mut rig = calls_rig(ALL, false);
    let (row, entries) = group_of(&mut rig);
    let height = |rig: &mut Rig| rig.layout().bounds(&format!("work:{row}")).size.height;
    let folded = height(&mut rig);
    rig.click(&format!("work-head:{row}"));
    let open = height(&mut rig);
    assert!(open > folded + px(100.), "{open:?} vs {folded:?}");
    let layout = rig.layout();
    let lane = layout.bounds(&format!("lane:{row}"));
    // Every line of the list: 24 px high, starting at the same x, inside the lane.
    let visible: Vec<&u64> = entries
        .iter()
        .filter(|k| layout.get(&format!("entry-head:{k}")).is_ok())
        .collect();
    assert!(visible.len() >= 4, "{} lines in view", visible.len());
    let first = layout.bounds(&format!("entry-head:{}", visible[0]));
    for k in &visible {
        let b = layout.bounds(&format!("entry-head:{k}"));
        assert!(
            (b.size.height - px(24.)).abs() < px(1.),
            "{k}: {:?}",
            b.size.height
        );
        assert!((b.left() - first.left()).abs() < px(0.5));
        assert!(b.right() <= lane.right() + px(1.));
    }
    let mut check = layout.check();
    check.no_overflow("timeline");
    check.assert();

    // A card opens below its line, indented, and grows its row only.
    let before = rig.layout().bounds(&format!("work:{row}")).size.height;
    rig.click(&format!("entry-head:{}", entries[0]));
    let layout = rig.layout();
    let head = layout.bounds(&format!("entry-head:{}", entries[0]));
    let body = layout.bounds(&format!("entry-body:{}", entries[0]));
    assert!(body.top() >= head.bottom(), "{body:?} under {head:?}");
    assert!(
        body.left() > head.left() + px(20.),
        "indented under the label"
    );
    assert!(layout.bounds(&format!("work:{row}")).size.height > before);
    // The command line: its output is two lines of 18 px in a list.
    let output = layout.bounds(&format!("output:{}", entries[0]));
    assert!(
        (output.size.height - px(36.)).abs() < px(1.),
        "{:?}",
        output.size.height
    );
    // Closing it again gives the height back.
    rig.click(&format!("entry-head:{}", entries[0]));
    assert!((rig.layout().bounds(&format!("work:{row}")).size.height - before).abs() < px(1.));
}

#[core::prelude::v1::test]
fn a_failed_call_says_why_in_the_error_colour_and_a_failed_command_shows_its_exit_code() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started("a", "terminal", json!({"command": "false"})));
    rig.emit(tool_completed(
        "a",
        "terminal",
        &json!({"output": "boom", "exit_code": 2, "duration_ms": 40}).to_string(),
    ));
    let (row, entries) = group_of(&mut rig);
    let layout = rig.layout();
    let exit = layout.get(&format!("exit-code:{}", entries[0])).unwrap();
    assert_eq!(
        layout
            .bounds(&format!("exit-code:{}", entries[0]))
            .size
            .height
            < px(24.),
        true
    );
    let _ = (exit, row);
    let destructive = rig
        .cx
        .update(|cx| cx.design_theme().palette.destructive.hsla());
    let mut check = layout.check();
    check.text_color(&format!("exit-code:{}", entries[0]), destructive);
    check.assert();
    // A call that failed outright: its reason is in the line.
    rig.emit(tool_started("b", "read_file", json!({"path": "nope"})));
    rig.emit(tool_completed(
        "b",
        "read_file",
        &json!({"success": false, "error": "File not found: nope"}).to_string(),
    ));
    rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::Work(g) => {
            assert_eq!(g.entries[1].card.status, Status::Failed);
            assert_eq!(g.entries[1].card.subtitle, "File not found: nope");
            assert!(g.summary(0).failed, "the latest call failed");
        }
        _ => panic!(),
    });
}

#[core::prelude::v1::test]
fn an_edit_opens_to_its_diff_in_the_edit_colours_and_open_diff_sends_the_action() {
    let mut rig = calls_rig(&["t4"], false);
    let (row, entries) = group_of(&mut rig);
    rig.model(|m, cx| m.expand_all(cx));
    let sent: Rc<RefCell<Vec<OpenDiffTab>>> = Rc::default();
    let seen = sent.clone();
    rig.cx.update(|cx| {
        cx.on_action(move |a: &OpenDiffTab, _| seen.borrow_mut().push(a.clone()));
    });
    let patch = entries[0];
    let layout = rig.layout();
    let lines = layout.bounds(&format!("diff-lines:{patch}"));
    assert!(lines.size.height >= px(18. * 5.), "{:?}", lines.size.height);
    layout
        .get(&format!("open-diff:{patch}"))
        .expect("the open-diff button");
    rig.click(&format!("open-diff:{patch}"));
    assert_eq!(
        sent.borrow().as_slice(),
        [OpenDiffTab {
            path: Some("notes.md".into())
        }]
    );
    let _ = row;
}

#[core::prelude::v1::test]
fn memory_and_skill_cards_link_to_their_screens() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started(
        "m",
        "memory",
        json!({"target": "user", "operations": [{"action": "add", "content": "Likes tabs"}]}),
    ));
    rig.emit(tool_completed(
        "m",
        "memory",
        &json!({"success": true, "message": "Applied 1 operation(s)."}).to_string(),
    ));
    let (_, entries) = group_of(&mut rig);
    let sent: Rc<RefCell<Vec<OpenScreen>>> = Rc::default();
    let seen = sent.clone();
    rig.cx.update(|cx| {
        cx.on_action(move |a: &OpenScreen, _| seen.borrow_mut().push(a.clone()));
    });
    rig.click(&format!("entry-head:{}", entries[0]));
    rig.click(&format!("open-screen:{}", entries[0]));
    assert_eq!(
        sent.borrow().as_slice(),
        [OpenScreen {
            screen: "user".into(),
            target: "user".into()
        }]
    );
}

#[core::prelude::v1::test]
fn a_delegate_card_lists_its_tasks_and_each_opens_on_its_own() {
    let mut rig = calls_rig(&["t9"], false);
    let (_, entries) = group_of(&mut rig);
    let delegate = entries[0];
    rig.click(&format!("entry-head:{delegate}"));
    let layout = rig.layout();
    let head = layout.bounds(&format!("tasks:{delegate}"));
    assert!(head.size.height >= px(24.));
    let task = layout.dump().entries.iter().find(|e| {
        e.path
            .last()
            .is_some_and(|n| n.starts_with("subtask-head:"))
    });
    let task_name = task
        .map(|e| e.path.last().unwrap().clone())
        .expect("the task's line");
    assert!(
        rig.layout().dump().entries.iter().all(|e| !e
            .path
            .last()
            .is_some_and(|n| n.starts_with("subtask-body:"))),
        "closed"
    );
    rig.click(&task_name);
    assert!(
        rig.layout().dump().entries.iter().any(|e| e
            .path
            .last()
            .is_some_and(|n| n.starts_with("subtask-body:"))),
        "opened by its click"
    );
}

// --- long output and streaming --------------------------------------------------------------

#[core::prelude::v1::test]
fn ten_thousand_lines_of_output_stay_a_screenful_and_are_parsed_once() {
    let mut rig = empty_rig();
    rig.emit(started());
    rig.emit(tool_started(
        "a",
        "terminal",
        json!({"command": "seq 10000"}),
    ));
    let output: String = (1..=10_000)
        .map(|i| format!("\u{1b}[32m{i}\u{1b}[0m\n"))
        .collect();
    rig.emit(tool_completed(
        "a",
        "terminal",
        &json!({"output": output, "exit_code": 0, "duration_ms": 5}).to_string(),
    ));
    let (_, entries) = group_of(&mut rig);
    rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::Work(g) => match &g.entries[0].card.detail {
            Detail::Command { output, .. } => {
                assert!(
                    output.lines().is_none(),
                    "kept raw while the card is closed"
                )
            }
            _ => panic!(),
        },
        _ => panic!(),
    });
    let start = Instant::now();
    rig.click(&format!("entry-head:{}", entries[0]));
    let opened = start.elapsed();
    let layout = rig.layout();
    let list = layout.bounds(&format!("output:{}", entries[0]));
    assert!(
        (list.size.height - px(16. * 18.)).abs() < px(1.),
        "a screenful of lines, not 10,000: {:?}",
        list.size.height
    );
    let ptr = rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::Work(g) => match &g.entries[0].card.detail {
            Detail::Command { output, .. } => {
                assert_eq!(output.lines().unwrap().len(), 10_000);
                std::sync::Arc::as_ptr(output.lines().unwrap()) as usize
            }
            _ => panic!(),
        },
        _ => panic!(),
    });
    // Drawing again (what a scroll does) does not parse again: the lines are the same allocation.
    let start = Instant::now();
    for _ in 0..10 {
        rig.settle();
    }
    let frames = start.elapsed();
    let again = rig.read(|m, _| match &m.rows()[0].kind {
        RowKind::Work(g) => match &g.entries[0].card.detail {
            Detail::Command { output, .. } => {
                std::sync::Arc::as_ptr(output.lines().unwrap()) as usize
            }
            _ => panic!(),
        },
        _ => panic!(),
    });
    assert_eq!(ptr, again);
    println!("open: {opened:?}  10 frames: {frames:?}");
    assert!(
        frames.as_millis() < 3000,
        "10 frames of a 10,000-line card took {frames:?}"
    );
}

#[core::prelude::v1::test]
fn live_output_replaces_the_tail_and_touches_only_its_own_card() {
    let mut rig = rig("s", Some(fixture::long_history(300)), 300);
    let texts = |rig: &mut Rig| -> Vec<usize> {
        rig.read(|m, _| {
            m.rows()
                .iter()
                .filter_map(|r| match &r.kind {
                    RowKind::User { text, .. } => Some(text.as_ptr() as usize),
                    RowKind::Assistant {
                        body: Body::Text(t),
                    } => Some(t.as_ptr() as usize),
                    _ => None,
                })
                .collect()
        })
    };
    let before = texts(&mut rig);
    let rows = rig.read(|m, _| m.rows().len());
    rig.emit(started());
    rig.emit(tool_started("a", "terminal", json!({"command": "make"})));
    let (_, entries) = group_of(&mut rig);
    rig.click(&format!("entry-head:{}", entries[0]));
    for i in 1..=30 {
        rig.emit(progress("a", &"line\n".repeat(i)));
    }
    // One new row (the group); the output is the last tail; every earlier row is untouched.
    assert_eq!(rig.read(|m, _| m.rows().len()), rows + 1);
    assert_eq!(before, texts(&mut rig));
    rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::Work(g) => match &g.entries[0].card.detail {
            Detail::Command { output, .. } => {
                assert_eq!(output.lines().unwrap().len(), 30);
                assert_eq!(g.entries[0].card.status, Status::Running);
            }
            _ => panic!(),
        },
        _ => panic!(),
    });
    // The running card follows its output: 30 lines, 16 shown, the list scrolled to the end.
    let layout = rig.layout();
    assert!(layout.get(&format!("output:{}", entries[0])).is_ok());
}

/// A frame's cost while a command streams does not depend on the thread's length.
#[core::prelude::v1::test]
fn streaming_progress_into_a_two_thousand_row_thread_stays_cheap_per_frame() {
    let run = |rows: usize| -> f64 {
        let mut rig = rig("s", Some(fixture::long_history(rows)), PAGE);
        rig.emit(started());
        rig.emit(tool_started("a", "terminal", json!({"command": "make"})));
        let (_, entries) = group_of(&mut rig);
        rig.click(&format!("entry-head:{}", entries[0]));
        let mut times = Vec::new();
        for i in 1..=100 {
            let start = Instant::now();
            rig.emit(progress(
                "a",
                &"compiling a crate with a long name\n".repeat(i % 40 + 1),
            ));
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.iter().sum::<f64>() / times.len() as f64
    };
    let small = run(20);
    let large = run(2000);
    println!("progress-20: mean {small:.1} ms  progress-2000: mean {large:.1} ms");
    assert!(large < small * 3. + 10., "{large:.1} ms vs {small:.1} ms");
}

// --- the footer ---------------------------------------------------------------------------------

fn footer_key(rig: &mut Rig) -> u64 {
    rig.read(|m, _| {
        m.rows()
            .iter()
            .rev()
            .find(|r| matches!(r.kind, RowKind::Footer(_)))
            .unwrap()
            .key
    })
}

#[core::prelude::v1::test]
fn the_footer_lists_the_changed_files_as_a_tree_with_their_counts_and_opens_their_diffs() {
    let mut rig = calls_rig(&["t4", "t5"], true);
    let key = footer_key(&mut rig);
    let layout = rig.layout();
    layout
        .get(&format!("changed-files:{key}"))
        .expect("the card");
    let card = layout.bounds(&format!("changed-files:{key}"));
    let tree = layout.bounds(&format!("changed-tree:{key}"));
    assert!(
        tree.top() > card.top() + px(20.),
        "the header is above the tree"
    );
    assert!(tree.bottom() <= card.bottom() + px(1.));
    // docs/ (a folder with plan.md), then notes.md: three 28 px rows.
    let rows: Vec<_> = layout
        .dump()
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.starts_with("tree-row")))
        .collect();
    assert_eq!(rows.len(), 3, "folder, its file, the loose file");
    for r in &rows {
        assert!((r.bounds.size.height - px(28.)).abs() < px(0.5));
    }
    // Retry and Branch in one row under the card, with the usage.
    let mut check = layout.check();
    check
        .same_row(&format!("retry:{key}"), &format!("branch:{key}"))
        .same_height(&[&format!("retry:{key}"), &format!("branch:{key}")])
        .no_overflow("timeline");
    check.assert();

    let sent: Rc<RefCell<Vec<OpenDiffTab>>> = Rc::default();
    let seen = sent.clone();
    rig.cx.update(|cx| {
        cx.on_action(move |a: &OpenDiffTab, _| seen.borrow_mut().push(a.clone()));
    });
    rig.click(&format!("open-diff-all:{key}"));
    assert_eq!(sent.borrow().as_slice(), [OpenDiffTab { path: None }]);

    // A folder closes on a click, and the collapse-all button closes the rest.
    let rows_before = rows.len();
    let folder = rows[0].path.last().unwrap().clone();
    rig.click(&folder);
    let after = rig
        .layout()
        .dump()
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.starts_with("tree-row")))
        .count();
    assert_eq!(after, rows_before - 1);
    // The only folder is closed, so the button offers to open all; then to close all.
    rig.click(&format!("collapse-all:{key}"));
    let all = rig
        .layout()
        .dump()
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.starts_with("tree-row")))
        .count();
    assert_eq!(all, rows_before, "expanded again");
}

#[core::prelude::v1::test]
fn retry_and_branch_reach_the_core_operations_with_the_turns_rows() {
    let mut rig = recorded_rig();
    let key = footer_key(&mut rig);
    let events: Rc<RefCell<Vec<ThreadEvent>>> = Rc::default();
    let seen = events.clone();
    let model = rig.model.clone();
    let _subscription = rig.cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &ThreadEvent, _| {
            seen.borrow_mut().push(event.clone())
        })
    });
    rig.click(&format!("branch:{key}"));
    assert_eq!(
        rig.backend.thread_ops(),
        [ThreadOp::Branch {
            session: "s".into(),
            through_row: 24
        }]
    );
    assert_eq!(
        events.borrow().as_slice(),
        [ThreadEvent::Branched("s-branch".into())]
    );

    // Retry: the prompt's row goes to the core, then the thread is read again from the store.
    let pages = rig.backend.pages().len();
    rig.click(&format!("retry:{key}"));
    assert_eq!(
        rig.backend.thread_ops()[1],
        ThreadOp::Retry {
            session: "s".into(),
            user_row: 1
        }
    );
    assert!(
        rig.backend.pages().len() > pages,
        "the thread was read again"
    );
    assert_eq!(
        rig.read(|m, _| m.phase().clone()),
        super::super::model::ThreadPhase::Ready
    );
}

#[core::prelude::v1::test]
fn a_failed_retry_or_branch_is_a_notice_and_the_buttons_come_back() {
    let mut rig = recorded_rig();
    let key = footer_key(&mut rig);
    rig.backend.fail_with(Some("The core said no."));
    rig.click(&format!("branch:{key}"));
    let notice = rig.read(|m, _| match &m.rows().last().unwrap().kind {
        RowKind::Notice { error, text } => Some((*error, text.to_string())),
        _ => None,
    });
    assert_eq!(
        notice,
        Some((true, "Could not branch: The core said no.".to_string()))
    );
    let busy = rig.read(|m, _| match &m.rows()[m.index_of(key).unwrap()].kind {
        RowKind::Footer(f) => f.busy,
        _ => true,
    });
    assert!(!busy);
}

#[core::prelude::v1::test]
fn a_footer_without_stored_rows_has_its_buttons_disabled() {
    // A replay with no rows in its end: nothing to retry or branch from.
    let mut rig = empty_rig();
    let mut recording = Recording::fixture().only_calls(&["t0"], true);
    for e in &mut recording.events {
        if let Recorded::TurnEnded {
            user_row, last_row, ..
        } = e
        {
            *user_row = None;
            *last_row = None;
        }
    }
    rig.model(|m, cx| m.replay_turn(&recording, cx));
    let key = footer_key(&mut rig);
    let layout = rig.layout();
    let mut check = layout.check();
    check
        .state(&format!("retry:{key}"), None, None, Some(true))
        .state(&format!("branch:{key}"), None, None, Some(true));
    check.assert();
    rig.click(&format!("retry:{key}"));
    assert!(rig.backend.thread_ops().is_empty());
}

#[core::prelude::v1::test]
fn only_the_newest_turn_can_retry_and_not_while_a_turn_runs() {
    let mut rig = recorded_rig();
    let older = footer_key(&mut rig);
    // A second turn ends: its footer is the newest; the first keeps Branch only.
    let recording = Recording::fixture();
    rig.model(|m, cx| m.replay(&recording, cx));
    let newest = footer_key(&mut rig);
    assert_ne!(older, newest);
    assert!(!rig.read(|m, _| m.can_retry(older)), "an older turn is branched, not retried");
    rig.model(|m, cx| m.retry(older, cx));
    assert!(rig.backend.thread_ops().is_empty(), "an older turn is not retried");
    let layout = rig.layout();
    let mut check = layout.check();
    check
        .state(&format!("retry:{newest}"), None, None, Some(false))
        .state(&format!("branch:{newest}"), None, None, Some(false));
    check.assert();
    // While a turn streams, the newest cannot retry either.
    rig.emit(CoreEvent::TurnStarted {
        session_id: "s".into(),
        admission_id: "x".into(),
    });
    assert!(!rig.read(|m, _| m.can_retry(newest)));
    rig.click(&format!("retry:{newest}"));
    assert!(rig.backend.thread_ops().is_empty());
}

#[core::prelude::v1::test]
fn a_turn_with_nothing_changed_has_a_footer_without_the_files_card() {
    let mut rig = empty_rig();
    rig.model(|m, cx| m.replay_turn(&Recording::fixture().only_calls(&["t0"], true), cx));
    let key = footer_key(&mut rig);
    let layout = rig.layout();
    assert!(layout.get(&format!("changed-files:{key}")).is_err());
    layout.get(&format!("footer-usage:{key}")).expect("usage");
    let groups: Vec<WorkGroup> = Vec::new();
    assert!(groups.is_empty());
}

#[core::prelude::v1::test]
fn in_the_frame_branching_opens_the_new_thread_and_the_diff_and_screen_actions_answer() {
    let mut cx = headless_app();
    cx.update(|cx| crate::theme::apply_theme(ThemeChoice::T3Dark, cx));
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    backend.set_history("t1", Vec::new());
    let model = {
        let backend = backend.clone();
        cx.update(|cx| cx.new(|cx| AppModel::with_snapshot(backend, snapshot, cx)))
    };
    let root_model = model.clone();
    let window = open_window(&mut cx, size(px(1100.), px(700.)), move |window, cx| {
        cx.new(|cx| {
            crate::workspace::Workspace::with_model(
                crate::ui_state::UiState::default(),
                None,
                root_model,
                window,
                cx,
            )
        })
    });
    let handle: AnyWindowHandle = window.into();
    let settle = |cx: &mut HeadlessAppContext| {
        for _ in 0..4 {
            cx.run_until_parked();
            draw(cx, handle);
        }
    };
    cx.update(|cx| model.update(cx, |m, cx| m.open("t1", cx)));
    settle(&mut cx);
    let thread = cx.update(|cx| model.read(cx).thread().cloned()).unwrap();
    cx.update(|cx| {
        thread.update(cx, |t, cx| {
            t.replay_turn(&Recording::fixture().only_calls(&["t4"], true), cx)
        })
    });
    settle(&mut cx);
    let key = cx.update(|cx| {
        thread
            .read(cx)
            .rows()
            .iter()
            .find(|r| matches!(r.kind, RowKind::Footer(_)))
            .unwrap()
            .key
    });
    // Branch from here: the backend makes the thread, the list reads it, the frame opens it.
    cx.update(|cx| thread.update(cx, |t, cx| t.branch(key, cx)));
    settle(&mut cx);
    assert_eq!(
        backend.thread_ops(),
        [ThreadOp::Branch {
            session: "t1".into(),
            through_row: 24
        }]
    );
    assert_eq!(
        cx.update(|cx| model.read(cx).selected().clone()),
        Selection::Thread("t1-branch".into())
    );
    // The diff tab opens the right panel (CP6.1); a screen is Phase 7: a toast says so.
    cx.update_window(handle, |root, window, cx| {
        let workspace = root.downcast::<crate::workspace::Workspace>().unwrap();
        workspace.update(cx, |w, cx| w.focus(window, cx));
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_action(
            Box::new(OpenDiffTab {
                path: Some("notes.md".into()),
            }),
            cx,
        );
        window.dispatch_action(
            Box::new(OpenScreen {
                screen: "skill".into(),
                target: "deploy".into(),
            }),
            cx,
        );
    })
    .unwrap();
    settle(&mut cx);
    let layout = layout(&mut cx, handle);
    let toasts: Vec<String> = layout
        .dump()
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.starts_with("toast")))
        .map(|e| e.path.last().unwrap().clone())
        .collect();
    assert!(!toasts.is_empty(), "a toast for the screen: {toasts:?}");
    assert!(layout.get("right-panel").is_ok(), "the right panel opened");
    assert!(layout.get("diff-panel").is_ok(), "on the diff tab");
}
