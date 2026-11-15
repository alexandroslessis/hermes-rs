//! The writer's rules (module doc of `writer.rs`): order, batches, read-after-write, flush,
//! errors reported once and to the right session, a failed write not taking the others down,
//! shutdown draining the queue, and a killed process keeping every flushed write.

use super::*;
use crate::{NewSession, Store};
use hermes_llm::conversation::token_usage::Usage;

fn new_session(store: &Store) -> String {
    store
        .create_session(NewSession {
            cwd: "C:/work".into(),
            provider: "p".into(),
            model: "m".into(),
            system_prompt: "frozen".into(),
        })
        .unwrap()
        .id
}

fn text(id: &str, text: &str) -> Arc<Message> {
    Arc::new(Message::user().with_text(text).with_id(id))
}

fn upsert(session: &str, seq: usize, message: Arc<Message>) -> StoreOp {
    StoreOp::UpsertMessage {
        session_id: session.to_string(),
        seq,
        message,
    }
}

fn texts(store: &Store, session: &str) -> Vec<String> {
    store
        .load_messages(session)
        .unwrap()
        .iter()
        .map(|m| m.as_concat_text())
        .collect()
}

/// A queued write the database refuses: a message of a session that doesn't exist (the
/// `messages.session_id` foreign key).
fn refused(seq: usize) -> StoreOp {
    upsert("ses_missing", seq, text("m", "lost"))
}

#[test]
fn queued_writes_are_applied_in_the_order_they_were_sent() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    store.queue(upsert(&s, 0, text("a", "first"))).unwrap();
    store.queue(upsert(&s, 1, text("b", "second"))).unwrap();
    // A replace in the middle wins over what came before it, not over what comes after.
    store
        .queue(StoreOp::ReplaceMessages {
            session_id: s.clone(),
            messages: vec![text("x", "replaced")],
        })
        .unwrap();
    store.queue(upsert(&s, 1, text("c", "after"))).unwrap();
    store.flush().unwrap();
    assert_eq!(texts(&store, &s), ["replaced", "after"]);
}

#[test]
fn a_waiting_write_never_overtakes_a_queued_one() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    store.queue(upsert(&s, 0, text("a", "queued"))).unwrap();
    // The title is a waiting write: when it returns, the queued message is committed too.
    store.set_title(&s, "Titled").unwrap();
    assert_eq!(
        store.writer.shared.state.lock().done,
        store.writer.queue.lock().enqueued
    );
    // A delete after queued writes of the same session: they land first, then the cascade.
    store.queue(upsert(&s, 1, text("b", "queued too"))).unwrap();
    store.delete_session(&s).unwrap();
    assert!(store.load_messages(&s).unwrap().is_empty());
    store.flush().unwrap();
}

#[test]
fn every_read_sees_what_was_queued_before_it_without_a_flush() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("sessions.db")).unwrap());
    let s = new_session(&store);
    for seq in 0..20 {
        store
            .queue(upsert(
                &s,
                seq,
                text(&format!("m{seq}"), &format!("text {seq}")),
            ))
            .unwrap();
        // Read right away, through the other connection: the queued write is there.
        assert_eq!(store.message_count(&s).unwrap(), seq + 1);
    }
    store
        .queue(StoreOp::RecordUsage {
            session_id: s.clone(),
            usage: ProviderUsage::new("m".into(), Usage::new(Some(7), Some(3), Some(10))),
        })
        .unwrap();
    assert_eq!(store.usage_totals(&s).unwrap().input_tokens, 7);
    // From another thread too: the read waits for writes any thread sent before it.
    store
        .queue(upsert(&s, 20, text("m20", "searchable zebra")))
        .unwrap();
    let hits = std::thread::spawn(move || store.sessions_matching("zebra").unwrap())
        .join()
        .unwrap();
    assert!(hits.contains(&s));
}

#[test]
fn another_connection_sees_queued_writes_once_flushed() {
    // The GPUI app (another process) and a second `Store` on the same file read committed rows
    // only: a flush is what makes the queue visible to them.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Store::open(&path).unwrap();
    let other = Store::open(&path).unwrap();
    let s = new_session(&store);
    for seq in 0..50 {
        store
            .queue(upsert(&s, seq, text(&format!("m{seq}"), "x")))
            .unwrap();
    }
    store.flush_session(&s).unwrap();
    assert_eq!(other.message_count(&s).unwrap(), 50);
}

#[test]
fn a_burst_of_queued_writes_is_committed_in_few_transactions() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    let (before, _) = store.writer.stats();
    // 200 different messages, sent faster than the 5 ms window: a handful of batches (on a
    // loaded machine the window can close early more often; never one commit per write).
    for seq in 0..200 {
        store
            .queue(upsert(&s, seq, text(&format!("m{seq}"), "x")))
            .unwrap();
    }
    store.flush().unwrap();
    let (after, _) = store.writer.stats();
    assert_eq!(store.message_count(&s).unwrap(), 200);
    let batches = after - before;
    assert!(
        batches >= 1 && batches < 50,
        "{batches} batches for 200 writes"
    );
}

#[test]
fn a_batch_never_holds_more_than_max_batch_writes() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    // Hold the batch open: the writer can't start while the connection is locked, so everything
    // below is waiting in the channel when it does.
    let (before, _) = store.writer.stats();
    {
        let _hold = store.writer.write.lock();
        // The writer takes the first job and then blocks on the connection; the rest queue up.
        for seq in 0..(MAX_BATCH * 2 + 10) {
            store
                .queue(upsert(&s, seq, text(&format!("m{seq}"), "x")))
                .unwrap();
        }
    }
    store.flush().unwrap();
    let (after, _) = store.writer.stats();
    assert!(after - before >= 3, "{} batches", after - before);
    assert_eq!(store.message_count(&s).unwrap(), MAX_BATCH * 2 + 10);
}

#[test]
fn a_streamed_message_upserted_many_times_in_a_batch_is_written_once() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    let (_, skipped_before) = store.writer.stats();
    {
        // Every chunk below is in the channel before the writer can start the batch.
        let _hold = store.writer.write.lock();
        store.queue(upsert(&s, 0, text("u", "question"))).unwrap();
        let mut reply = String::new();
        for chunk in 0..30 {
            reply.push_str(&format!("{chunk} "));
            store.queue(upsert(&s, 1, text("r", &reply))).unwrap();
        }
    }
    store.flush().unwrap();
    let (_, skipped_after) = store.writer.stats();
    let reply: String = (0..30).map(|chunk| format!("{chunk} ")).collect();
    assert_eq!(texts(&store, &s), ["question".to_string(), reply]);
    // The first job may have been taken before the lock; the rest were superseded.
    assert!(
        skipped_after - skipped_before >= 28,
        "{}",
        skipped_after - skipped_before
    );
}

#[test]
fn an_upsert_is_not_skipped_across_a_replace_of_its_session() {
    let jobs = |works: Vec<Work>| -> Vec<Job> {
        works
            .into_iter()
            .enumerate()
            .map(|(i, work)| Job {
                seq: i as u64 + 1,
                session: Some(match &work {
                    Work::Op(op) => op.session_id().to_string(),
                    Work::Call(_) => "s".into(),
                }),
                work,
                reply: None,
            })
            .collect()
    };
    let replace = || {
        Work::Op(StoreOp::ReplaceMessages {
            session_id: "s".into(),
            messages: vec![],
        })
    };
    // upsert(0), replace, upsert(0): the first is kept (the replace runs between them).
    let batch = jobs(vec![
        Work::Op(upsert("s", 0, text("a", "a"))),
        replace(),
        Work::Op(upsert("s", 0, text("b", "b"))),
    ]);
    assert_eq!(superseded(&batch), [false, false, false]);
    // Other sessions' writes and other messages of the same session don't matter.
    let batch = jobs(vec![
        Work::Op(upsert("s", 0, text("a", "a"))),
        Work::Op(upsert("t", 0, text("t", "t"))),
        Work::Op(upsert("s", 1, text("c", "c"))),
        Work::Op(upsert("s", 0, text("b", "b"))),
    ]);
    assert_eq!(superseded(&batch), [true, false, false, false]);
    // A write of unknown scope (a closure) keeps it, and so does a waiting upsert.
    let mut batch = jobs(vec![
        Work::Op(upsert("s", 0, text("a", "a"))),
        Work::Call(Box::new(|_| Ok(()))),
        Work::Op(upsert("s", 0, text("b", "b"))),
    ]);
    assert_eq!(superseded(&batch), [false, false, false]);
    batch.remove(1);
    let (reply, _answer) = mpsc::channel();
    batch[0].reply = Some(reply);
    assert_eq!(superseded(&batch), [false, false]);
}

#[test]
fn a_failed_write_is_reported_once_to_its_own_session_and_spares_the_others() {
    let store = Store::open_in_memory().unwrap();
    let a = new_session(&store);
    let b = new_session(&store);
    {
        // All in one batch: good, refused, good.
        let _hold = store.writer.write.lock();
        store.queue(upsert(&a, 0, text("a0", "kept"))).unwrap();
        store.queue(refused(0)).unwrap();
        store.queue(upsert(&b, 0, text("b0", "kept too"))).unwrap();
    }
    // The failure belongs to "ses_missing": the other sessions' flushes are clean...
    store.flush_session(&a).unwrap();
    store.flush_session(&b).unwrap();
    assert_eq!(texts(&store, &a), ["kept"]);
    assert_eq!(texts(&store, &b), ["kept too"]);
    // ...and it is reported to that session's flush, once.
    let error = store.flush_session("ses_missing").unwrap_err().to_string();
    assert!(
        error.contains("1 write to the session database failed"),
        "{error}"
    );
    assert!(error.contains("FOREIGN KEY"), "{error}");
    store.flush_session("ses_missing").unwrap();
    store.flush().unwrap();
}

#[test]
fn many_failures_of_one_session_never_hide_another_sessions_failure() {
    let store = Store::open_in_memory().unwrap();
    for seq in 0..(MAX_FAILURES + 30) {
        store
            .writer
            .submit(Some("ses_noisy".into()), Work::Op(refused(seq)), false)
            .unwrap();
    }
    store
        .queue(upsert("ses_other_missing", 0, text("m", "x")))
        .unwrap();
    let _ = store.list_sessions(1).unwrap();
    let other = store
        .flush_session("ses_other_missing")
        .unwrap_err()
        .to_string();
    assert!(other.starts_with("1 write "), "{other}");
    // The noisy session's are all counted, though only MAX_FAILURES are kept.
    let noisy = store.flush_session("ses_noisy").unwrap_err().to_string();
    assert!(
        noisy.starts_with(&format!("{} writes ", MAX_FAILURES + 30)),
        "{noisy}"
    );
    store.flush().unwrap();
}

#[test]
fn the_next_queued_write_of_a_session_reports_an_earlier_failure() {
    let store = Store::open_in_memory().unwrap();
    store.queue(refused(0)).unwrap();
    // Settle without taking the error (a read does that).
    let _ = store.list_sessions(1).unwrap();
    // The turn's next write hears of it (as it did when every write was synchronous).
    let error = store.queue(refused(1)).unwrap_err().to_string();
    assert!(error.contains("FOREIGN KEY"), "{error}");
    // `flush` collects every session's failures (the second refused write).
    assert!(store.flush().is_err());
    store.flush().unwrap();
}

#[test]
fn a_message_without_an_id_is_refused_before_it_is_queued() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    let error = store
        .queue(upsert(&s, 0, Arc::new(Message::user().with_text("no id"))))
        .unwrap_err();
    assert!(error.to_string().contains("must have an id"));
    assert!(
        store
            .upsert_message(&s, 0, &Message::user().with_text("no id"))
            .is_err()
    );
    store.flush().unwrap();
}

#[test]
fn a_waiting_write_gets_its_own_error_and_leaves_none_behind() {
    let store = Store::open_in_memory().unwrap();
    let error = store
        .upsert_message(
            "ses_missing",
            0,
            &Message::user().with_text("x").with_id("m"),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("FOREIGN KEY"));
    assert!(store.update_project("prj_none", "x", &[]).is_err());
    // Reported to the caller already: nothing waits for a flush.
    store.flush().unwrap();
}

#[test]
fn a_batch_whose_commit_fails_is_retried_write_by_write() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    // A write whose statement passes but whose COMMIT fails: a deferred foreign key.
    let deferred: Call = Box::new(|conn| {
        conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        conn.execute(
            "INSERT INTO usage (session_id, at, model, input_tokens, output_tokens,
                                cache_read_tokens, cache_write_tokens, cost)
             VALUES ('ses_missing', 0, 'm', 0, 0, 0, 0, NULL)",
            [],
        )?;
        Ok(())
    });
    {
        let _hold = store.writer.write.lock();
        store.queue(upsert(&s, 0, text("a", "before"))).unwrap();
        store
            .writer
            .submit(Some("ses_missing".into()), Work::Call(deferred), false)
            .unwrap();
        store.queue(upsert(&s, 1, text("b", "after"))).unwrap();
    }
    // The good writes are saved, alone; the bad one is reported.
    store.flush_session(&s).unwrap();
    assert!(RETRIED_BATCHES.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert_eq!(texts(&store, &s), ["before", "after"]);
    let error = store.flush_session("ses_missing").unwrap_err();
    assert!(format!("{error:#}").contains("FOREIGN KEY"), "{error:#}");
}

#[test]
fn a_write_that_panics_is_reported_and_the_writer_keeps_going() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    store
        .writer
        .submit(
            Some(s.clone()),
            Work::Call(Box::new(|_| panic!("a bug in a write"))),
            false,
        )
        .unwrap();
    let error = store.flush_session(&s).unwrap_err();
    assert!(error.to_string().contains("internal error"), "{error}");
    // The writer still works, and the panicked batch left no transaction open.
    store
        .queue(upsert(&s, 0, text("a", "still saved")))
        .unwrap();
    store.flush_session(&s).unwrap();
    assert_eq!(texts(&store, &s), ["still saved"]);
}

#[test]
fn dropping_the_store_commits_everything_queued() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let s = {
        let store = Store::open(&path).unwrap();
        let s = new_session(&store);
        for seq in 0..500 {
            store
                .queue(upsert(&s, seq, text(&format!("m{seq}"), &format!("{seq}"))))
                .unwrap();
        }
        // No flush: the drop drains the queue.
        s
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(store.message_count(&s).unwrap(), 500);
    assert_eq!(
        store.load_messages(&s).unwrap()[499].as_concat_text(),
        "499"
    );
}

#[test]
fn writes_after_shutdown_still_work_on_the_callers_thread() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    store.queue(upsert(&s, 0, text("a", "queued"))).unwrap();
    store.shutdown().unwrap();
    assert_eq!(texts(&store, &s), ["queued"]);
    store.queue(upsert(&s, 1, text("b", "inline"))).unwrap();
    store.set_title(&s, "late").unwrap();
    assert_eq!(texts(&store, &s), ["queued", "inline"]);
    // A failure inline is the caller's at once.
    assert!(store.queue(refused(0)).is_err());
    store.shutdown().unwrap();
    store.flush().unwrap();
}

#[test]
fn writes_from_many_threads_keep_each_threads_order() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions: Vec<String> = (0..4).map(|_| new_session(&store)).collect();
    let threads: Vec<_> = sessions
        .iter()
        .cloned()
        .map(|s| {
            let store = store.clone();
            std::thread::spawn(move || {
                for step in 0..100 {
                    // The same row rewritten: only the last version may remain.
                    store
                        .queue(upsert(
                            &s,
                            step % 5,
                            text(&format!("m{step}"), &format!("{step}")),
                        ))
                        .unwrap();
                }
                store.flush_session(&s).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    for s in &sessions {
        assert_eq!(texts(&store, s), ["95", "96", "97", "98", "99"]);
    }
}

/// The policy for a crash: every write before a flush (a turn boundary) is in the database after
/// the process dies; what was queued after it may or may not be, and the database is intact.
/// The process is this test binary run again (`HERMES_STORE_CRASH_DB` set): it writes, flushes,
/// queues more and exits at once, without running any destructor (as a kill would).
#[test]
fn a_killed_process_keeps_every_flushed_write() {
    const CHILD: &str = "HERMES_STORE_CRASH_DB";
    if let Some(path) = std::env::var_os(CHILD) {
        let store = Store::open(std::path::Path::new(&path)).unwrap();
        let s = new_session(&store);
        store.set_title(&s, "crash").unwrap();
        for seq in 0..100 {
            store
                .queue(upsert(&s, seq, text(&format!("m{seq}"), "flushed")))
                .unwrap();
        }
        store.flush_session(&s).unwrap();
        for seq in 100..400 {
            store
                .queue(upsert(&s, seq, text(&format!("m{seq}"), "queued")))
                .unwrap();
        }
        std::process::exit(42);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "writer::tests::a_killed_process_keeps_every_flushed_write",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42), "the child process did not run");
    let store = Store::open(&path).unwrap();
    let s = store
        .session_by_title("crash")
        .unwrap()
        .expect("the session");
    let stored = texts(&store, &s);
    assert!(
        stored.len() >= 100 && stored.len() <= 400,
        "{}",
        stored.len()
    );
    assert!(stored[..100].iter().all(|t| t == "flushed"));
    // What did land of the queued part is a prefix (batches commit in order).
    assert!(stored[100..].iter().all(|t| t == "queued"));
    let ids: Vec<String> = store
        .load_messages(&s)
        .unwrap()
        .iter()
        .map(|m| m.id.clone().unwrap())
        .collect();
    let prefix: Vec<String> = (0..ids.len()).map(|seq| format!("m{seq}")).collect();
    assert_eq!(
        ids, prefix,
        "the rows that landed are positions 0..n, no gap"
    );
    let check: String = store
        .read()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
}

// Review (CP13b) tests.

fn one_call(session: &str) -> StoreOp {
    StoreOp::RecordUsage {
        session_id: session.to_string(),
        usage: ProviderUsage::new("m".into(), Usage::new(Some(1), Some(1), Some(2))),
    }
}

/// A write whose error ends the whole transaction (SQLite does that itself on a full disk or an
/// I/O error; here the write rolls back) must not leave the rest of the batch running outside a
/// transaction: each write would commit on its own and then run again in the retry, so a usage
/// row would be counted twice.
#[test]
fn a_write_that_ends_the_transaction_never_runs_the_others_twice() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    let ends_it: Call = Box::new(|conn| {
        conn.execute_batch("ROLLBACK")?;
        Err(anyhow!("disk I/O error (simulated)"))
    });
    {
        let _hold = store.writer.write.lock();
        store.queue(one_call(&s)).unwrap();
        store
            .writer
            .submit(Some("ses_broken".into()), Work::Call(ends_it), false)
            .unwrap();
        store.queue(one_call(&s)).unwrap();
        store.queue(upsert(&s, 0, text("a", "kept"))).unwrap();
    }
    store.flush_session(&s).unwrap();
    // Both calls once, the message once: nothing lost, nothing doubled.
    assert_eq!(store.usage_totals(&s).unwrap().calls, 2);
    assert_eq!(texts(&store, &s), ["kept"]);
    let error = store.flush_session("ses_broken").unwrap_err().to_string();
    assert!(error.contains("simulated"), "{error}");
    // And the writer goes on in a clean state.
    store.queue(one_call(&s)).unwrap();
    store.flush().unwrap();
    assert_eq!(store.usage_totals(&s).unwrap().calls, 3);
}

/// Deleting a session drops that session's failures only: a failure of the writer itself is
/// still reported to the next caller.
#[test]
fn deleting_a_session_keeps_a_failure_of_the_writer_itself() {
    let store = Store::open_in_memory().unwrap();
    let s = new_session(&store);
    store.queue(upsert("ses_gone", 0, text("m", "x"))).unwrap();
    store.writer.shared.state.lock().record(Failure {
        session: None,
        message: "the session database writer stopped; queued writes were lost".into(),
        more: 0,
    });
    let _ = store.list_sessions(1).unwrap();
    store.delete_session("ses_gone").unwrap();
    store.delete_session(&s).unwrap();
    let error = store.flush().unwrap_err().to_string();
    assert!(
        error.starts_with("1 write ") && error.contains("writer stopped"),
        "{error}"
    );
}

/// A write that arrives while a shutdown on another thread is still draining the queue runs
/// after everything queued before it (it was refused by the closed channel, but must not run
/// inline ahead of the thread's last batch), and a second `shutdown` returns only once the
/// queue is drained.
#[test]
fn a_write_during_a_shutdown_lands_after_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let s = new_session(&store);
    let hold = store.writer.write.clone();
    let held = hold.lock();
    for seq in 0..50 {
        store
            .queue(upsert(&s, seq, text(&format!("m{seq}"), "early")))
            .unwrap();
    }
    // Shutdown on another thread: it closes the channel and waits for the (held) writer.
    let closing = {
        let store = store.clone();
        std::thread::spawn(move || store.shutdown())
    };
    while store.writer.queue.lock().sender.is_some() {
        std::thread::yield_now();
    }
    // A late write and a second shutdown, both while the writer still holds 50 writes.
    let late = {
        let store = store.clone();
        let s = s.clone();
        std::thread::spawn(move || store.queue(upsert(&s, 0, text("m0", "late"))))
    };
    let second = {
        let store = store.clone();
        std::thread::spawn(move || store.shutdown())
    };
    std::thread::sleep(Duration::from_millis(50));
    drop(held);
    late.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    closing.join().unwrap().unwrap();
    let other = rusqlite::Connection::open(&path).unwrap();
    let first: String = other
        .query_row(
            "SELECT json FROM messages WHERE session_id = ?1 AND seq = 0",
            [&s],
            |row| row.get(0),
        )
        .unwrap();
    assert!(first.contains("late"), "{first}");
    let count: i64 = other
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            [&s],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 50);
}
