//! The session actor's contract: order, answers on a stopped or panicking actor, no
//! re-entrancy, lifecycle (Phase 13 CP13d).

use super::*;
use crate::session::testing::open_session;

use std::time::{Duration, Instant};

fn handle() -> SessionHandle {
    SessionHandle::spawn("ses_test").unwrap()
}

/// Polls `done` for up to five seconds.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn the_first_session_attached_is_kept_and_answered_to_both() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, first) = open_session(dir.path());
    let (_, second) = open_session(dir.path());
    let handle = handle();
    let kept = handle.attach(first.clone(), Default::default()).unwrap();
    let again = handle.attach(second, Default::default()).unwrap();
    assert!(Arc::ptr_eq(&kept, &first));
    assert!(Arc::ptr_eq(&again, &first), "a second load keeps the first");
    assert!(Arc::ptr_eq(&handle.open().unwrap(), &first));
}

#[test]
fn an_answered_command_is_in_the_status_the_caller_reads_next() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let handle = handle();
    for _ in 0..200 {
        handle.attach(open.clone(), Default::default()).unwrap();
        assert!(handle.open().is_some());
        handle.detach(false).unwrap().unwrap();
        assert!(handle.open().is_none());
    }
}

#[test]
fn commands_from_one_sender_are_handled_in_the_order_sent() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let handle = handle();
    // Fire-and-forget attach, detach, attach: whatever the timing, the last one wins.
    for _ in 0..50 {
        assert!(handle.tell(Command::Attach {
            open: open.clone(),
            nudges: Default::default(),
            reply: Reply::Ignore
        }));
        assert!(handle.tell(Command::Detach {
            only_idle: false,
            reply: Reply::Ignore
        }));
        assert!(handle.tell(Command::Attach {
            open: open.clone(),
            nudges: Default::default(),
            reply: Reply::Ignore
        }));
        handle.barrier().unwrap();
        assert!(
            handle.open().is_some(),
            "the barrier saw every earlier command"
        );
        handle.detach(false).unwrap().unwrap();
    }
}

#[test]
fn many_threads_each_keep_their_own_order() {
    let handle = handle();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let handle = handle.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    handle.barrier().unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn a_panicking_handler_answers_an_error_and_the_actor_goes_on() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let handle = handle();
    handle.attach(open.clone(), Default::default()).unwrap();
    assert_eq!(
        handle.ask(|reply| Command::Panic { reply }),
        Err(Gone::Stopped)
    );
    // The state is whole and the next command is answered.
    assert!(Arc::ptr_eq(&handle.open().unwrap(), &open));
    handle.barrier().unwrap();
}

#[tokio::test]
async fn an_async_caller_gets_an_error_from_a_panicking_handler_too() {
    let handle = handle();
    let answer = handle.ask_async(|reply| Command::Panic { reply }).await;
    assert_eq!(answer, Err(Gone::Stopped));
    handle
        .ask_async(|reply| Command::Barrier { reply })
        .await
        .unwrap();
}

#[test]
fn a_stopped_actor_answers_an_error_instead_of_hanging() {
    // An actor whose thread is gone: its inbox is dropped, as when the thread ends.
    let (commands, inbox) = mpsc::channel::<Command>();
    drop(inbox);
    let handle = SessionHandle {
        inner: Arc::new(Inner {
            id: "ses_gone".into(),
            commands,
            status: Arc::default(),
            alive: Arc::new(AtomicBool::new(true)),
        }),
    };
    let began = Instant::now();
    assert_eq!(handle.barrier(), Err(Gone::Stopped));
    assert!(!handle.tell(Command::Barrier {
        reply: Reply::Ignore
    }));
    assert!(began.elapsed() < Duration::from_secs(1));
}

#[test]
fn commands_queued_when_the_thread_ends_are_answered_with_an_error() {
    // The actor's thread stops (here: it reads one command, then drops its inbox) with a
    // command still queued behind: its sender is dropped with the inbox, so the waiter wakes.
    let (commands, inbox) = mpsc::channel::<Command>();
    let handle = SessionHandle {
        inner: Arc::new(Inner {
            id: "ses_ending".into(),
            commands,
            status: Arc::default(),
            alive: Arc::new(AtomicBool::new(true)),
        }),
    };
    let (first_sent, first_seen) = mpsc::channel();
    let actor = std::thread::spawn(move || {
        let first = inbox.recv().unwrap();
        first_sent.send(()).unwrap();
        // Wait until the second is queued, then end without answering either.
        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        drop(inbox);
    });
    let waiter = {
        let handle = handle.clone();
        std::thread::spawn(move || handle.barrier())
    };
    first_seen.recv().unwrap();
    let second = handle.barrier();
    assert_eq!(second, Err(Gone::Stopped));
    assert_eq!(waiter.join().unwrap(), Err(Gone::Stopped));
    actor.join().unwrap();
}

#[test]
fn a_call_from_the_actors_own_thread_is_refused_not_deadlocked() {
    let handle = handle();
    let inner = handle.clone();
    let (seen, answer) = mpsc::channel();
    let work: Box<dyn FnOnce() + Send> = Box::new(move || {
        // Only in release builds does the refusal return (debug builds assert first); catch
        // either so the test runs in both.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.barrier()));
        let _ = seen.send(match result {
            Ok(answer) => answer,
            Err(_) => Err(Gone::Reentrant),
        });
    });
    handle
        .ask(|reply| Command::OnActor { work, reply })
        .unwrap();
    assert_eq!(
        answer.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(Gone::Reentrant)
    );
}

#[test]
fn the_actor_ends_when_the_last_handle_is_dropped() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let weak = Arc::downgrade(&open);
    let handle = handle();
    handle.attach(open, Default::default()).unwrap();
    let held = handle.clone();
    drop(handle);
    // Another holder (a turn still ending) keeps it going.
    held.barrier().unwrap();
    assert!(weak.upgrade().is_some());
    drop(held);
    assert!(
        eventually(|| weak.upgrade().is_none()),
        "the actor's state (and the session it held) is dropped once no handle is left"
    );
}

#[test]
fn the_registry_starts_one_actor_per_id_and_reads_never_start_one() {
    let registry = SessionRegistry::default();
    assert!(registry.get("a").is_none());
    let a = registry.get_or_spawn("a").unwrap();
    let again = registry.get_or_spawn("a").unwrap();
    assert!(Arc::ptr_eq(&a.inner, &again.inner));
    assert!(registry.get("b").is_none());
    assert_eq!(registry.all().len(), 1);
    let removed = registry.remove("a").unwrap();
    assert!(Arc::ptr_eq(&removed.inner, &a.inner));
    assert!(registry.get("a").is_none());
    // A fresh actor for a reused id.
    let fresh = registry.get_or_spawn("a").unwrap();
    assert!(!Arc::ptr_eq(&fresh.inner, &a.inner));
}

#[tokio::test(flavor = "current_thread")]
async fn a_waiting_call_on_a_one_thread_runtime_does_not_deadlock() {
    // The runtime's only thread waits for an answer: the actor has its own thread, so it answers
    // (this is why the actor is a thread, not a task: `Runtime::start_turn` is synchronous and
    // the `turns` tests call it from a one-thread runtime).
    let handle = handle();
    let ticker = tokio::spawn(async {});
    for _ in 0..100 {
        handle.barrier().unwrap();
    }
    ticker.await.unwrap();
}

// ---- the run slot and the review slot (stage 2) ----

use crate::session::RunGuard;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

#[test]
fn one_run_claim_at_a_time_and_dropping_the_guard_frees_it_at_once() {
    let handle = handle();
    let first = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(handle.is_running());
    assert!(matches!(
        RunGuard::claim(&handle, CancellationToken::new(), None).unwrap(),
        Err(Busy)
    ));
    drop(first);
    // No barrier: the drop waited for the release, so the status already shows it.
    assert!(!handle.is_running());
    let second = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(handle.is_running());
    drop(second);
}

#[test]
fn a_stale_release_does_not_free_a_newer_claim() {
    let handle = handle();
    let first = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    let old = handle.status().run.as_ref().unwrap().claim;
    drop(first);
    let _second = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    handle
        .ask(|reply| Command::Release { claim: old, reply })
        .unwrap();
    assert!(
        handle.is_running(),
        "the old claim's number is not the slot's"
    );
}

#[test]
fn cancel_reaches_the_turn_while_the_actor_is_held_up() {
    let handle = handle();
    let token = CancellationToken::new();
    let _run = RunGuard::claim(&handle, token.clone(), None)
        .unwrap()
        .unwrap();
    // Hold the actor's thread on a command, with more commands queued behind it.
    let (release, held) = mpsc::channel::<()>();
    let work: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = held.recv_timeout(Duration::from_secs(10));
    });
    assert!(handle.tell(Command::OnActor {
        work,
        reply: Reply::Ignore
    }));
    for _ in 0..10 {
        assert!(handle.tell(Command::Barrier {
            reply: Reply::Ignore
        }));
    }
    let began = Instant::now();
    handle.cancel();
    assert!(token.is_cancelled(), "Stop does not wait behind the queue");
    assert!(began.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    handle.barrier().unwrap();
}

#[test]
fn the_turn_state_is_published_with_the_claim_and_gone_with_the_release() {
    let handle = handle();
    assert!(handle.turn_state().is_none());
    let state: crate::ops::SharedTurnState = Arc::default();
    let run = RunGuard::claim(&handle, CancellationToken::new(), Some(state.clone()))
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&handle.turn_state().unwrap(), &state));
    drop(run);
    assert!(handle.turn_state().is_none());
}

#[test]
fn detaching_an_idle_only_session_is_refused_while_a_turn_runs() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let handle = handle();
    handle.attach(open, Default::default()).unwrap();
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(matches!(handle.detach(true).unwrap(), Err(Busy)));
    assert!(handle.open().is_some());
    drop(run);
    assert!(handle.detach(true).unwrap().unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_turn_task_frees_the_slot_before_its_join_handle_resolves() {
    let handle = handle();
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    let task = tokio::spawn(async move {
        let _run = run;
        panic!("a tool bug");
    });
    assert!(task.await.unwrap_err().is_panic());
    assert!(!handle.is_running(), "the guard was dropped with the task");
}

#[test]
fn a_paused_review_confirms_only_if_no_turn_came_between() {
    let handle = handle();
    let started = Arc::new(AtomicBool::new(false));
    let confirm = |epoch| {
        let started = started.clone();
        handle
            .ask(|reply| Command::ConfirmIdle {
                epoch,
                started,
                reply,
            })
            .unwrap()
    };
    let epoch = handle
        .ask(|reply| Command::IdleEpoch { reply })
        .unwrap()
        .expect("idle");
    // A turn claims and ends between the check and the confirmation: the copy is stale.
    drop(
        RunGuard::claim(&handle, CancellationToken::new(), None)
            .unwrap()
            .unwrap(),
    );
    assert!(!confirm(epoch));
    assert!(!started.load(Ordering::SeqCst));
    // Running now: no epoch at all.
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle.ask(|reply| Command::IdleEpoch { reply }).unwrap(),
        None
    );
    drop(run);
    let epoch = handle
        .ask(|reply| Command::IdleEpoch { reply })
        .unwrap()
        .unwrap();
    assert!(confirm(epoch));
    assert!(started.load(Ordering::SeqCst));
}

fn claim_review(
    handle: &SessionHandle,
    memory: bool,
    skills: bool,
    requested: bool,
    busy: bool,
) -> ReviewClaim {
    handle
        .ask(|reply| Command::ClaimReview {
            memory,
            skills,
            requested,
            evidence_busy: Box::new(move || busy),
            reply,
        })
        .unwrap()
}

#[test]
fn the_review_slot_keeps_the_rules_of_review_setup_start() {
    let handle = handle();
    // An evidence review holds the session: nothing starts.
    assert!(matches!(
        claim_review(&handle, true, false, false, true),
        ReviewClaim::Skipped("an evidence review of the session is running")
    ));
    assert!(!handle.reviewing());
    // A paused due review is replaced by the next one, which takes over its job.
    let ReviewClaim::Start(first) = claim_review(&handle, true, false, false, false) else {
        panic!("the first review starts");
    };
    let ReviewClaim::Start(second) = claim_review(&handle, false, true, false, false) else {
        panic!("a paused due review is replaced");
    };
    assert!(first.cancel.is_cancelled());
    assert!(second.memory && second.skills);
    // Once started (its snapshot taken), a due review is skipped; /refine replaces it.
    second.started.store(true, Ordering::SeqCst);
    assert!(matches!(
        claim_review(&handle, true, true, false, false),
        ReviewClaim::Skipped("one is still running")
    ));
    let ReviewClaim::Start(refine) = claim_review(&handle, true, true, true, false) else {
        panic!("/refine replaces a started review");
    };
    assert!(second.cancel.is_cancelled());
    // A paused /refine stays.
    assert!(matches!(
        claim_review(&handle, true, false, false, false),
        ReviewClaim::Skipped("a /refine is waiting to run")
    ));
    // The replaced review's end leaves the slot to the newer one; its own end frees it.
    let stale = second.started;
    handle
        .ask(|reply| Command::ReleaseReview {
            started: stale,
            reply,
        })
        .unwrap();
    assert!(handle.reviewing());
    assert!(
        handle
            .ask(|reply| Command::ReviewSlotTaken { reply })
            .unwrap()
    );
    let own = refine.started;
    handle
        .ask(|reply| Command::ReleaseReview {
            started: own,
            reply,
        })
        .unwrap();
    assert!(!handle.reviewing());
    assert!(
        !handle
            .ask(|reply| Command::ReviewSlotTaken { reply })
            .unwrap()
    );
}

#[test]
fn the_review_claim_and_the_schedulers_claim_always_see_each_other() {
    // The scheduler sets its busy flag, then asks `ReviewSlotTaken`; the agentic side checks the
    // flag inside `ClaimReview`. Raced many times: never both go ahead, and one always does.
    for _ in 0..300 {
        let handle = handle();
        let busy = Arc::new(AtomicBool::new(false));
        let agentic = {
            let (handle, busy) = (handle.clone(), busy.clone());
            std::thread::spawn(move || {
                let check = Box::new(move || busy.load(Ordering::SeqCst));
                matches!(
                    handle
                        .ask(|reply| Command::ClaimReview {
                            memory: true,
                            skills: false,
                            requested: false,
                            evidence_busy: check,
                            reply,
                        })
                        .unwrap(),
                    ReviewClaim::Start(_)
                )
            })
        };
        busy.store(true, Ordering::SeqCst);
        let evidence = !handle
            .ask(|reply| Command::ReviewSlotTaken { reply })
            .unwrap();
        let agentic = agentic.join().unwrap();
        assert!(!(agentic && evidence), "both reviews started");
        assert!(agentic || evidence, "one of the two goes ahead");
    }
}

// ---- the review counters (stage 3) ----

#[test]
fn the_review_counters_start_with_the_load_and_count_turns() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let (_, other) = open_session(dir.path());
    let handle = handle();
    // Nine turns behind it: the tenth is the memory review's.
    handle
        .attach(open, crate::review::Nudges::resumed(9))
        .unwrap();
    // A second load that lost the race does not reset the counters.
    handle
        .attach(other, crate::review::Nudges::resumed(0))
        .unwrap();
    assert!(handle.ask(|reply| Command::TickMemory { reply }).unwrap());
    assert!(!handle.ask(|reply| Command::TickMemory { reply }).unwrap());
    assert!(
        !handle
            .ask(|reply| Command::FinishTurn {
                turn: Vec::new(),
                reply
            })
            .unwrap()
    );
}

// ---- the page's state (stages 4-9) ----

fn prompt(admission: &str) -> serde_json::Value {
    serde_json::json!({"submission_id": admission, "text": admission})
}

fn queued(handle: &SessionHandle) -> Vec<String> {
    handle
        .page()
        .queue
        .iter()
        .map(|params| params["submission_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_queue_keeps_back_and_front_order_and_takes_by_admission() {
    let handle = handle();
    let _run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    for admission in ["a", "b", "c"] {
        assert!(handle.enqueue(prompt(admission), false, true).unwrap());
    }
    // A redirect / a steer the turn missed goes first.
    assert!(handle.enqueue(prompt("r"), true, false).unwrap());
    assert_eq!(queued(&handle), ["r", "a", "b", "c"]);
    assert_eq!(handle.dequeue("b").unwrap().unwrap()["submission_id"], "b");
    assert!(handle.dequeue("b").unwrap().is_none(), "already taken");
    assert_eq!(handle.take_next().unwrap().unwrap()["submission_id"], "r");
    assert_eq!(queued(&handle), ["a", "c"]);
}

#[test]
fn a_prompt_is_queued_while_running_only_if_a_turn_still_holds_the_session() {
    let handle = handle();
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(handle.enqueue(prompt("a"), false, true).unwrap());
    drop(run);
    // The turn ended between the caller's check and its queueing: not queued (it starts now
    // instead), so nothing waits for a turn end that already happened.
    assert!(!handle.enqueue(prompt("b"), false, true).unwrap());
    assert_eq!(queued(&handle), ["a"]);
}

#[test]
fn queued_prompts_from_many_threads_keep_each_threads_order() {
    let handle = handle();
    let _run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let handle = handle.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    assert!(
                        handle
                            .enqueue(prompt(&format!("{t}-{i}")), false, true)
                            .unwrap()
                    );
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let all = queued(&handle);
    assert_eq!(all.len(), 200);
    for t in 0..4 {
        let mine: Vec<usize> = all
            .iter()
            .filter_map(|a| a.strip_prefix(&format!("{t}-")))
            .map(|i| i.parse().unwrap())
            .collect();
        assert_eq!(mine, (0..50).collect::<Vec<_>>());
    }
}

#[test]
fn live_is_a_running_turn_or_the_pages_hold() {
    let handle = handle();
    assert!(!handle.is_live());
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(handle.is_live());
    handle.set_live(true).unwrap();
    drop(run);
    assert!(
        handle.is_live(),
        "held live between a turn and its queued follow-up"
    );
    handle.set_live(false).unwrap();
    assert!(!handle.is_live());
}

#[test]
fn a_model_pick_is_kept_only_while_a_turn_runs_and_taken_once() {
    let handle = handle();
    let pick = Some(("p".to_string(), "m".to_string()));
    assert!(
        !handle.set_pending_route(pick.clone()).unwrap(),
        "idle: switch now"
    );
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    assert!(handle.set_pending_route(pick.clone()).unwrap());
    assert_eq!(handle.page().pending_route, pick);
    drop(run);
    assert_eq!(handle.take_pending_route().unwrap(), pick);
    assert_eq!(handle.take_pending_route().unwrap(), None);
}

#[test]
fn effort_revision_and_images_are_kept_per_session() {
    let handle = handle();
    assert_eq!(handle.page().revision, 1);
    assert_eq!(handle.bump_revision().unwrap(), 2);
    assert_eq!(handle.bump_revision().unwrap(), 3);
    handle.set_effort(Some("high".into())).unwrap();
    assert_eq!(handle.page().effort.as_deref(), Some("high"));
    handle.set_effort(None).unwrap();
    assert_eq!(handle.page().effort, None);
    assert_eq!(handle.attach_image("a.png").unwrap(), 1);
    assert_eq!(handle.attach_image("b.png").unwrap(), 2);
    assert_eq!(handle.detach_image("a.png").unwrap(), (true, 1));
    assert_eq!(handle.detach_image("a.png").unwrap(), (false, 1));
    assert_eq!(handle.take_images().unwrap(), ["b.png"]);
    assert!(handle.take_images().unwrap().is_empty());
}

#[test]
fn the_pages_state_outlives_a_close_but_not_a_delete() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, open) = open_session(dir.path());
    let registry = SessionRegistry::default();
    let handle = registry.get_or_spawn("s").unwrap();
    handle.attach(open, Default::default()).unwrap();
    handle.set_effort(Some("low".into())).unwrap();
    // A close (or a workspace move) forgets the loaded data only.
    handle.detach(false).unwrap().unwrap();
    let again = registry.get("s").unwrap();
    assert_eq!(again.page().effort.as_deref(), Some("low"));
    // A delete removes the actor: the id starts afresh.
    registry.remove("s");
    drop((handle, again));
    let fresh = registry.get_or_spawn("s").unwrap();
    assert_eq!(fresh.page().effort, None);
}

// ---- review fixes (CP13d review) ----

#[test]
fn a_losing_load_gets_its_copy_back_to_drop_on_its_own_thread() {
    let dir = hermes_testdir::tempdir().unwrap();
    let (_, first) = open_session(dir.path());
    let (_, second) = open_session(dir.path());
    let handle = handle();
    handle.attach(first.clone(), Default::default()).unwrap();
    let (kept, refused) = handle
        .ask(|reply| Command::Attach {
            open: second.clone(),
            nudges: Default::default(),
            reply,
        })
        .unwrap();
    assert!(Arc::ptr_eq(&kept, &first));
    assert!(
        refused.is_some_and(|refused| Arc::ptr_eq(&refused, &second)),
        "the actor hands the copy back instead of dropping its tools itself"
    );
}

#[test]
fn a_model_pick_after_the_turn_ended_drops_the_pick_it_left_behind() {
    let handle = handle();
    let run = RunGuard::claim(&handle, CancellationToken::new(), None)
        .unwrap()
        .unwrap();
    let during = Some(("p".to_string(), "during".to_string()));
    assert!(handle.set_pending_route(during).unwrap());
    // The turn ends; before its end takes the pick, the user picks again: switched now, and the
    // older pick must not be applied after it by the turn's end.
    drop(run);
    let after = Some(("p".to_string(), "after".to_string()));
    assert!(!handle.set_pending_route(after).unwrap());
    assert_eq!(handle.take_pending_route().unwrap(), None);
}

#[test]
fn a_dead_actor_is_replaced_and_its_published_turn_stopped() {
    let registry = SessionRegistry::default();
    let old = registry.get_or_spawn("s").unwrap();
    old.set_effort(Some("high".into())).unwrap();
    let token = CancellationToken::new();
    let run = RunGuard::claim(&old, token.clone(), None).unwrap().unwrap();
    // The loop dies (a bug outside a handler): nothing answers any more.
    assert!(old.tell(Command::Exit));
    assert!(eventually(|| !old.is_alive()));
    assert_eq!(old.barrier(), Err(Gone::Stopped));
    assert!(old.is_running(), "its last status still names the turn");
    // Reads don't see it, and the orphaned turn is stopped (Stop can't reach it any more).
    assert!(registry.get("s").is_none());
    assert!(token.is_cancelled());
    assert!(registry.all().is_empty());
    // The next use gets a working actor.
    let fresh = registry.get_or_spawn("s").unwrap();
    assert!(fresh.is_alive());
    fresh.barrier().unwrap();
    assert!(!fresh.is_running());
    assert_eq!(
        fresh.page().effort,
        None,
        "the dead actor's page state is gone"
    );
    // The orphaned turn's guard still drops without hanging (its release is refused, logged).
    drop(run);
    // get_or_spawn replaces a dead actor too (not only get).
    assert!(fresh.tell(Command::Exit));
    assert!(eventually(|| !fresh.is_alive()));
    let third = registry.get_or_spawn("s").unwrap();
    assert!(!Arc::ptr_eq(&third.inner, &fresh.inner));
    third.barrier().unwrap();
}
