//! Phase 13 CP13c: the tools that read or write the disk or the session database must not hold
//! an async worker (ground rule 7). Each test runs one of them on a runtime with one thread (so
//! a blocked worker is a blocked everything), makes it slow with the `slow` hook (a sleep where
//! the disk or database work happens), and watches a timer that ticks every 20 ms beside it.
//! If the tool ran its work on the worker, the timer would stall for the whole delay.
//!
//! The tolerance is generous (a 300 ms delay, a gap of at most 150 ms allowed) so a loaded
//! machine does not fail it; a worker that is really blocked shows a gap of 300 ms or more.
//! `a_blocked_worker_is_seen_by_the_timer` is the control: the same measurement over an inline
//! sleep must see the stall, or the other tests would prove nothing.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::tests::{TestBridge, tools_with_bridge};
use crate::{SessionTools, SessionToolsConfig, SkillStore, ToolOutput, detect_shell, slow};

const DELAY: Duration = Duration::from_millis(300);
const TICK: Duration = Duration::from_millis(20);
const ALLOWED_GAP: Duration = Duration::from_millis(150);

/// Runs `work` while a task on the same one-thread runtime ticks every [`TICK`]; returns what
/// `work` returned, how long it took, and the longest time between two ticks.
async fn with_timer<T>(work: impl Future<Output = T>) -> (T, Duration, Duration) {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let timer = tokio::spawn(async move {
        let mut longest = Duration::ZERO;
        let mut last = Instant::now();
        let mut ticks = tokio::time::interval(TICK);
        tokio::pin!(stopped);
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    longest = longest.max(last.elapsed());
                    last = Instant::now();
                }
                _ = &mut stopped => break,
            }
        }
        longest.max(last.elapsed())
    });
    // Lets the timer start before the work does.
    tokio::time::sleep(TICK * 2).await;
    let began = Instant::now();
    let value = work.await;
    let took = began.elapsed();
    let _ = stop.send(());
    (value, took, timer.await.unwrap())
}

fn assert_off_the_worker(what: &str, took: Duration, longest: Duration) {
    assert!(
        took >= DELAY,
        "{what}: the slow hook did not run inside the tool ({took:?})"
    );
    assert!(
        longest < ALLOWED_GAP,
        "{what}: the timer stalled for {longest:?} while the tool worked: it ran on the worker"
    );
}

/// Tools with a memory folder, a session database and a skills folder, all real.
fn full_tools(session_id: &str) -> (tempfile::TempDir, SessionTools) {
    let dir = tempfile::tempdir().unwrap();
    let skills = Arc::new(SkillStore::new(dir.path().join("skills"), Default::default()));
    let memory = Arc::new(hermes_memory::MemoryStore::new(
        dir.path().join("memories"),
        Default::default(),
    ));
    let history = Arc::new(hermes_store::Store::open_in_memory().unwrap());
    let tools = SessionTools::new(SessionToolsConfig {
        session_id: session_id.into(),
        cwd: dir.path().to_path_buf(),
        home: dir.path().to_path_buf(),
        data_dir: dir.path().join(".data"),
        scratch_dir: dir.path().join(".scratch"),
        shell: detect_shell(),
        bridge: Arc::new(TestBridge::default()),
        todos: Vec::new(),
        memory: Some(memory),
        history: Some(history),
        skills: Some(skills),
        approvals: None,
        checkpoints: None,
        web: None,
    });
    (dir, tools)
}

async fn timed(tools: &SessionTools, name: &str, args: Value) -> (ToolOutput, Duration, Duration) {
    with_timer(tools.dispatch(name, args, None)).await
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocked_worker_is_seen_by_the_timer() {
    let ((), took, longest) = with_timer(async {
        std::thread::sleep(DELAY);
    })
    .await;
    assert!(took >= DELAY);
    assert!(
        longest >= DELAY - TICK,
        "the control saw no stall ({longest:?}): the other tests prove nothing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_memory_tool_does_not_block_the_worker() {
    let (_dir, tools) = full_tools("ses_slow_memory");
    slow::set("ses_slow_memory", DELAY);
    let args = json!({"target": "memory", "operations": [{"action": "add", "content": "likes tea"}]});
    let (output, took, longest) = timed(&tools, "memory", args).await;
    assert_off_the_worker("memory", took, longest);
    assert!(!output.is_error, "{}", output.value);
}

#[tokio::test(flavor = "current_thread")]
async fn a_staged_memory_write_does_not_block_the_worker() {
    let (_dir, tools) = full_tools("ses_slow_staged");
    // A tainted session's memory writes wait for the user (`staging.rs`).
    tools.mark_tainted("a web page".into()).await;
    slow::set("ses_slow_staged", DELAY);
    let args = json!({"target": "memory", "operations": [{"action": "add", "content": "likes tea"}]});
    let (output, took, longest) = timed(&tools, "memory", args).await;
    assert_off_the_worker("staged memory", took, longest);
    assert!(!output.is_error, "{}", output.value);
}

#[tokio::test(flavor = "current_thread")]
async fn session_search_does_not_block_the_worker() {
    let (_dir, tools) = full_tools("ses_slow_search");
    slow::set("ses_slow_search", DELAY);
    let (output, took, longest) = timed(&tools, "session_search", json!({"query": "tea"})).await;
    assert_off_the_worker("session_search", took, longest);
    assert!(!output.is_error, "{}", output.value);
    // The browse shape reads the same database.
    let (output, took, longest) = timed(&tools, "session_search", json!({})).await;
    assert_off_the_worker("session_search (browse)", took, longest);
    assert!(!output.is_error, "{}", output.value);
}

#[tokio::test(flavor = "current_thread")]
async fn the_skill_tools_do_not_block_the_worker() {
    let (_dir, tools) = full_tools("ses_slow_skills");
    slow::set("ses_slow_skills", DELAY);
    let (output, took, longest) = timed(&tools, "skills_list", json!({})).await;
    assert_off_the_worker("skills_list", took, longest);
    assert!(!output.is_error, "{}", output.value);
    let create = json!({
        "action": "create", "name": "tea-making",
        "content": "---\nname: tea-making\ndescription: Make tea.\n---\n# Tea\nBoil water.\n",
    });
    let (output, took, longest) = timed(&tools, "skill_manage", create).await;
    assert_off_the_worker("skill_manage", took, longest);
    assert!(!output.is_error, "{}", output.value);
    let (output, took, longest) = timed(&tools, "skill_view", json!({"name": "tea-making"})).await;
    assert_off_the_worker("skill_view", took, longest);
    assert!(!output.is_error, "{}", output.value);
}

/// The turn's step as `agent/machine.rs` races it: Stop wins only when the call is still waiting.
async fn raced_against_stop<T>(
    call: impl Future<Output = T>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<T> {
    tokio::select! {
        biased;
        result = call => Some(result),
        _ = cancel.cancelled() => None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stop_during_a_follow_up_keeps_the_calls_result() {
    // Stop lands while the work is running on its thread: the result still comes back.
    let cancel = tokio_util::sync::CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stopper.cancel();
    });
    let work = crate::off_worker_unless_stopped(Some(&cancel), || {
        std::thread::sleep(Duration::from_millis(200));
        7
    });
    assert_eq!(raced_against_stop(work, &cancel).await, Some(Ok(7)));

    // The control: a plain blocking hop under the same race loses it.
    let cancel = tokio_util::sync::CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stopper.cancel();
    });
    let plain = tokio::task::spawn_blocking(|| {
        std::thread::sleep(Duration::from_millis(200));
        7
    });
    assert!(raced_against_stop(plain, &cancel).await.is_none());

    // After Stop it runs inline; without a token, on the blocking pool.
    assert_eq!(crate::off_worker_unless_stopped(Some(&cancel), || 8).await, Ok(8));
    assert_eq!(crate::off_worker_unless_stopped(None, || 9).await, Ok(9));
}

#[tokio::test(flavor = "current_thread")]
async fn a_follow_up_without_stop_does_not_block_the_worker() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let (value, took, longest) = with_timer(crate::off_worker_unless_stopped(Some(&cancel), || {
        std::thread::sleep(DELAY);
        1
    }))
    .await;
    assert_eq!(value, Ok(1));
    assert_off_the_worker("off_worker_unless_stopped", took, longest);
}

#[tokio::test(flavor = "current_thread")]
async fn the_hook_only_slows_the_session_it_names() {
    let (_dir, tools) = full_tools("ses_slow_other");
    slow::set("ses_slow_nobody", DELAY);
    let (output, took, _) = timed(&tools, "session_search", json!({})).await;
    assert!(took < DELAY, "{took:?}");
    assert!(!output.is_error, "{}", output.value);
    // (Without a database the tool answers at once.)
    let (_dir, bare) = tools_with_bridge(Arc::new(TestBridge::default()));
    let output = bare.dispatch("session_search", json!({}), None).await;
    assert!(output.is_error);
}
