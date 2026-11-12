//! Signal detection on a corpus of real-looking messages and turns, persistence, caps, and each
//! non-textual signal.

use super::*;
use hermes_llm::conversation::message::Message;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::json;

/// After an ordinary answer (the usual place for a correction).
const AFTER: TextContext = TextContext {
    first: false,
    after_question: false,
};
/// The session's first message.
const FIRST: TextContext = TextContext {
    first: true,
    after_question: false,
};
/// Answering the assistant's question.
const ANSWER: TextContext = TextContext {
    first: false,
    after_question: true,
};

#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Scores nothing.
    Zero,
    /// Scores something, but at least `min` and below the threshold.
    AtLeast(f32),
    /// An explicit "remember this".
    Explicit,
}
use Expect::*;

/// The corpus: (message, where it stands, what it must give).
fn corpus() -> Vec<(&'static str, TextContext, Expect)> {
    vec![
        // ---- corrections and rules (true positives) ----
        ("No, use pnpm, not npm.", AFTER, AtLeast(1.0)),
        (
            "That's wrong — the config lives in src/config.rs.",
            AFTER,
            AtLeast(1.25),
        ),
        (
            "From now on, always run cargo fmt before committing.",
            AFTER,
            AtLeast(2.0),
        ),
        (
            "I prefer tabs over spaces in this repo.",
            AFTER,
            AtLeast(2.0),
        ),
        ("Don't use unwrap in library code.", AFTER, AtLeast(1.0)),
        ("Never commit directly to main.", AFTER, AtLeast(1.5)),
        ("Why did you delete the tests?", AFTER, AtLeast(1.25)),
        ("You forgot to update the changelog.", AFTER, AtLeast(1.25)),
        (
            "Not what I asked. I wanted a summary, not a rewrite.",
            AFTER,
            AtLeast(1.25),
        ),
        ("Stop adding comments to every line.", AFTER, AtLeast(1.25)),
        ("Use the existing helper instead.", AFTER, AtLeast(0.75)),
        (
            "No. The function should return a Result instead.",
            AFTER,
            AtLeast(1.75),
        ),
        (
            "Next time, ask before running migrations.",
            AFTER,
            AtLeast(2.0),
        ),
        (
            "I told you to keep the API unchanged.",
            AFTER,
            AtLeast(1.25),
        ),
        (
            "Okay, but don't touch the generated files.",
            AFTER,
            AtLeast(1.0),
        ),
        (
            "I'd rather you didn't reformat the whole file.",
            AFTER,
            AtLeast(2.0),
        ),
        (
            "As I said, the tests live under crates/runtime/tests.",
            AFTER,
            AtLeast(1.25),
        ),
        ("nope, wrong file", AFTER, AtLeast(2.0)),
        (
            "You should always check git status first.",
            AFTER,
            AtLeast(1.5),
        ),
        ("No, no, no. Use the release branch.", AFTER, AtLeast(1.0)),
        (
            "Fix the import, but don't change the public API.",
            AFTER,
            AtLeast(0.75),
        ),
        (
            "Hmm, that's not right, the port is 8080.",
            AFTER,
            AtLeast(1.25),
        ),
        (
            "Please never run git push --force on shared branches.",
            AFTER,
            AtLeast(1.5),
        ),
        (
            "I don't like it when you rewrite whole files for a one-line fix.",
            AFTER,
            AtLeast(2.0),
        ),
        (
            "Going forward, please use the logging crate, not println.",
            AFTER,
            AtLeast(2.0),
        ),
        (
            "Always write tests first in this project.",
            FIRST,
            AtLeast(1.0),
        ),
        // A preference stated while answering a question still counts.
        ("pnpm. From now on, always use pnpm.", ANSWER, AtLeast(2.0)),
        // ---- explicit memory requests ----
        (
            "Remember this: the staging server is at 10.0.0.5.",
            AFTER,
            Explicit,
        ),
        (
            "Please remember that I use PowerShell, not bash.",
            AFTER,
            Explicit,
        ),
        (
            "Save this to your memory: deploys happen on Fridays.",
            AFTER,
            Explicit,
        ),
        ("Can you remember this for next time?", AFTER, Explicit),
        ("Remember that our CI runs on Windows.", FIRST, Explicit),
        ("ok, memorize this.", AFTER, Explicit),
        // ---- ordinary messages that must not score (tricky negatives) ----
        ("No problem, thanks!", AFTER, Zero),
        ("No worries, take your time.", AFTER, Zero),
        ("No, that's fine, go ahead.", AFTER, Zero),
        ("No need to run the tests.", AFTER, Zero),
        (
            "I don't know why the build fails, can you look?",
            AFTER,
            Zero,
        ),
        ("Always happy to help — what's next?", AFTER, Zero),
        (
            "Can you fix the bug where it never terminates?",
            AFTER,
            Zero,
        ),
        ("What's wrong with this function?", AFTER, Zero),
        ("Nothing is wrong, it works now. Thanks!", AFTER, Zero),
        (
            "Something went wrong when I ran it, here's the output.",
            AFTER,
            Zero,
        ),
        ("Never mind, I figured it out.", AFTER, Zero),
        (
            "Don't worry about the tests for now, just get it compiling.",
            AFTER,
            Zero,
        ),
        (
            "I never use Windows Terminal, is there an alternative?",
            AFTER,
            Zero,
        ),
        ("Should I always run this as admin?", AFTER, Zero),
        ("Do you remember what we did yesterday?", AFTER, Zero),
        (
            "Write a function that always returns a sorted list.",
            FIRST,
            Zero,
        ),
        ("Thanks, that's perfect.", AFTER, Zero),
        ("If you don't mind, can you also add a test?", AFTER, Zero),
        ("It's always been like this.", AFTER, Zero),
        ("What would you do instead?", AFTER, Zero),
        ("Is there a way to never show this warning?", AFTER, Zero),
        ("Great job! Always good to see green tests.", AFTER, Zero),
        ("Stop the server please.", AFTER, Zero),
        ("Please don't hesitate to ask questions.", AFTER, Zero),
        ("Can you add a 'never' option to the enum?", AFTER, Zero),
        ("remember.txt is missing, can you recreate it?", AFTER, Zero),
        (
            "I used the wrong branch, can you switch to main?",
            AFTER,
            Zero,
        ),
        ("Next time I'll try the other approach myself.", AFTER, Zero),
        ("It crashes every time I use it.", AFTER, Zero),
        ("I don't think that's needed.", AFTER, Zero),
        (
            "The user said 'no, never again' in the ticket; can you find that ticket?",
            AFTER,
            Zero,
        ),
        // Rejections in the first message have nothing to correct.
        (
            "That's wrong in the docs, can you fix the README?",
            FIRST,
            Zero,
        ),
        // Answering the assistant's yes/no question is a choice, not a correction.
        ("No.", ANSWER, Zero),
        ("No, just the code.", ANSWER, Zero),
        // Code, quotes, logs, pasted e-mails.
        (
            "```rust\n// don't use unwrap here, always handle errors\nfn main() {}\n```\nWhat does this do?",
            AFTER,
            Zero,
        ),
        (
            "Here's the log:\n2026-11-12 10:00:01 ERROR never connected: wrong password\n2026-11-12 10:00:02 WARN retrying",
            AFTER,
            Zero,
        ),
        (
            "> No, that's wrong, remember to always call me.\n\nThis is what my colleague wrote, can you reply politely?",
            AFTER,
            Zero,
        ),
        (
            "Can you summarize this email?\n\nFrom: Bob <bob@example.com>\nSubject: Re: release\n\nNo, don't ship it yet. From now on always ping me before releases.",
            AFTER,
            Zero,
        ),
        (
            "Summarize this thread.\nOn Mon, Nov 10, 2026 at 3:14 PM Bob wrote:\nNo, that's wrong. Never deploy on Fridays.",
            AFTER,
            Zero,
        ),
        (
            "The README says: \"Never commit secrets. Always use environment variables.\" Is that enforced anywhere?",
            AFTER,
            Zero,
        ),
        (
            "Why does `git commit --no-verify` never run the hooks?",
            AFTER,
            Zero,
        ),
        (
            "    fn never_returns() -> ! { loop {} }\n\nwhy doesn't this compile?",
            AFTER,
            Zero,
        ),
        (
            "Traceback (most recent call last):\n  File \"x.py\", line 3, in <module>\nValueError: wrong value\nwhat is this?",
            AFTER,
            Zero,
        ),
        // Not English: no textual signals.
        (
            "Όχι, χρησιμοποίησε pnpm από τώρα και στο εξής.",
            AFTER,
            Zero,
        ),
        ("No sé por qué falla, ¿puedes revisarlo?", AFTER, Zero),
        ("No, no funciona así, usa pnpm.", AFTER, Zero),
        ("Ja, das ist gut so. Immer weiter.", AFTER, Zero),
        // Commands' generated prompts are not the user's words.
        ("/plan add a cache, never touch the db", AFTER, Zero),
    ]
}

#[test]
fn the_corpus_scores_as_labelled() {
    let corpus = corpus();
    assert!(corpus.len() >= 40, "the corpus has {} cases", corpus.len());
    let mut failures = Vec::new();
    for (text, context, expect) in &corpus {
        let score = score_text(text, *context);
        let ok = match expect {
            Zero => score.weight == 0.0 && !score.explicit,
            AtLeast(min) => {
                !score.explicit && score.weight >= *min && score.weight < REVIEW_THRESHOLD
            }
            Explicit => score.explicit && score.weight >= REVIEW_THRESHOLD,
        };
        if !ok {
            failures.push(format!(
                "{text:?} ({context:?}): expected {expect:?}, got {score:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn one_message_never_crosses_the_threshold_but_two_corrections_do() {
    let loud = "No! That's wrong. Don't do that. Never do that again. From now on, I prefer you ask \
                first, you forgot it again, use the other one instead.";
    let score = score_text(loud, AFTER);
    assert!(
        score.weight <= MESSAGE_CAP && score.weight < REVIEW_THRESHOLD,
        "{score:?}"
    );

    let first = score_text("No, that's wrong.", AFTER).weight;
    let second = score_text("No, use pnpm instead.", AFTER).weight;
    assert!(first < REVIEW_THRESHOLD && second < REVIEW_THRESHOLD);
    assert!(first + second >= REVIEW_THRESHOLD, "{first} + {second}");

    // A lone weak phrase stays far below.
    assert!(score_text("Let's do it the other way round instead.", AFTER).weight <= 0.75);
}

#[test]
fn the_start_of_a_message_weighs_more() {
    let head = score_text("Don't add new dependencies.", AFTER).weight;
    let body = score_text("Looks good overall. Don't add new dependencies.", AFTER).weight;
    assert!(head > body && body > 0.0, "{head} vs {body}");
}

#[test]
fn a_long_paste_is_read_only_at_its_start() {
    let filler = "Some text from the design document about caching layers. ".repeat(30);
    let pasted = format!("Here is the doc, what do you think?\n{filler}\nNever deploy on Fridays.");
    assert_eq!(score_text(&pasted, AFTER).weight, 0.0);
}

#[test]
fn every_pattern_compiles() {
    for regex in [
        &*NO_START,
        &*COURTESY,
        &*WRONG_IDIOMS,
        &*WRONG,
        &*WRONG_START,
        &*NOT_WHAT,
        &*INSTEAD,
        &*WH_QUESTION,
        &*DONT,
        &*ALWAYS_NEVER,
        &*YOU_SHOULD_ALWAYS,
        &*DURABLE,
        &*DURABLE_CUE,
        &*SELF_FUTURE,
        &*PREFER,
        &*REMEMBER_THIS,
        &*REMEMBER_THAT,
        &*SAVE_TO_MEMORY,
        &*REMEMBER_TO,
        &*INLINE_CODE,
        &*DOUBLE_QUOTED,
        &*SINGLE_QUOTED,
        &*EMAIL_START,
        &*LOG_LINE,
    ] {
        let _ = regex.is_match("x");
    }
}

#[test]
fn the_denial_marker_matches_hermes_s_denial_message() {
    assert!(is_user_denial(&hermes_tools::approval::denied_message()));
    assert!(!is_user_denial(&hermes_tools::approval::timeout_message()));
    assert!(!is_user_denial(
        &hermes_tools::approval::smart_denied_message("recursive delete")
    ));
}

// ---- turns -----------------------------------------------------------------------------------

fn user(text: &str) -> Message {
    Message::user()
        .with_text(text)
        .with_generated_id_if_missing()
}

fn assistant(text: &str) -> Message {
    Message::assistant()
        .with_text(text)
        .with_generated_id_if_missing()
}

fn call(id: &str, name: &str, args: Value) -> Message {
    let params = CallToolRequestParams::new(name.to_string())
        .with_arguments(args.as_object().cloned().unwrap_or_default());
    Message::assistant().with_tool_request(id, Ok(params))
}

fn result(id: &str, text: &str) -> Message {
    Message::user().with_tool_response(
        id,
        Ok(CallToolResult::success(vec![ContentBlock::text(
            text.to_string(),
        )])),
    )
}

fn base(history: &[Message], typed: &str) -> TurnBase {
    TurnBase::before(history, Some(typed))
}

fn outcome(calls: Vec<CallRecord>) -> TurnOutcome {
    TurnOutcome {
        message_id: Some("m1".into()),
        call_count: calls.len(),
        calls,
        completed: true,
        answer: Some("Done.".into()),
        ..Default::default()
    }
}

fn terminal(command: &str, failed: bool) -> CallRecord {
    let output = if failed {
        json!({"output": "not found", "exit_code": 1}).to_string()
    } else {
        json!({"output": "ok", "exit_code": 0}).to_string()
    };
    CallRecord::new("terminal", &json!({"command": command}), &output, failed)
}

#[test]
fn a_correction_turn_keeps_its_context_as_evidence() {
    let history = vec![user("Install the deps."), assistant("I ran npm install.")];
    let analysis = analyze(
        &base(&history, "No, use pnpm instead."),
        &outcome(Vec::new()),
    );
    assert_eq!(analysis.signals.len(), 1);
    let signal = &analysis.signals[0];
    assert!(matches!(signal.kind, SignalKind::Correction { .. }));
    assert_eq!(signal.turn, 2);
    assert_eq!(signal.message_id.as_deref(), Some("m1"));
    let texts: Vec<&str> = signal.evidence.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Install the deps.",
            "I ran npm install.",
            "No, use pnpm instead.",
            "Done."
        ]
    );
}

#[test]
fn an_ordinary_turn_gives_nothing() {
    let history = vec![user("Install the deps."), assistant("Done, all installed.")];
    let analysis = analyze(
        &base(&history, "Thanks! Now run the tests."),
        &outcome(Vec::new()),
    );
    assert!(analysis.is_empty(), "{analysis:?}");
}

#[test]
fn the_assistant_s_own_words_and_tool_output_never_count() {
    let history = vec![
        user("Tidy the module."),
        assistant("No, that's wrong of me. From now on I will always ask. Remember this!"),
    ];
    let calls = vec![CallRecord::new(
        "read_file",
        &json!({"path": "notes.md"}),
        "Never commit to main. Remember this: always use pnpm.",
        false,
    )];
    assert!(analyze(&base(&history, "Looks good."), &outcome(calls)).is_empty());
}

#[test]
fn learn_is_an_explicit_signal() {
    let history = vec![user("Build it."), assistant("Built.")];
    let message = hermes_prompts::review::learn_message("the build steps");
    let analysis = analyze(&base(&history, &message), &outcome(Vec::new()));
    assert_eq!(analysis.signals.len(), 1);
    assert_eq!(
        analysis.signals[0].kind,
        SignalKind::Explicit {
            via: "learn".into()
        }
    );
    assert!(
        analysis.signals[0]
            .evidence
            .iter()
            .any(|e| e.text == "/learn the build steps")
    );
    // The generated prompt itself is not read for phrases (it says "don't" and "never").
    assert!(
        !analysis
            .signals
            .iter()
            .any(|s| matches!(s.kind, SignalKind::Correction { .. }))
    );
}

#[test]
fn a_denied_command_is_a_signal_and_the_next_message_is_its_reason() {
    let denied = CallRecord::new(
        "terminal",
        &json!({"command": "rm -rf build"}),
        &json!({"success": false, "error": hermes_tools::approval::denied_message()}).to_string(),
        true,
    );
    assert!(denied.denied && !denied.failed);
    let history = vec![user("Clean up."), assistant("Cleaning.")];
    let analysis = analyze(
        &base(&history, "Clean up the build."),
        &outcome(vec![denied]),
    );
    let signal = &analysis.signals[0];
    assert_eq!(signal.weight, DENIAL_WEIGHT);
    assert_eq!(
        signal.kind,
        SignalKind::Denial {
            what: "terminal: rm -rf build".into(),
            reason: None
        }
    );

    // The next turn: what the user writes is attached to that denial.
    let history = vec![
        user("Clean up the build."),
        call("c1", "terminal", json!({"command": "rm -rf build"})),
        result("c1", &hermes_tools::approval::denied_message()),
        assistant("I stopped."),
    ];
    let next = base(&history, "build/ holds my release notes, keep it.");
    assert!(next.previous_turn_denied);
    let analysis = analyze(&next, &outcome(Vec::new()));
    assert_eq!(
        analysis.denial_reason.as_deref(),
        Some("build/ holds my release notes, keep it.")
    );
    let mut stored = SessionSignals::default();
    stored.add(signal.clone(), 1);
    stored.attach_denial_reason(analysis.denial_reason.as_deref().unwrap());
    assert!(matches!(
        &stored.signals[0].kind,
        SignalKind::Denial { reason: Some(r), .. } if r.contains("release notes")
    ));
}

#[test]
fn failing_twice_then_working_differently_is_a_workaround() {
    let calls = vec![
        terminal("npm test", true),
        CallRecord::new("read_file", &json!({"path": "package.json"}), "{}", false),
        terminal("npx jest", true),
        terminal("pnpm test", false),
    ];
    let analysis = analyze(&base(&[], "run the tests"), &outcome(calls));
    let signal = analysis
        .signals
        .iter()
        .find(|s| matches!(s.kind, SignalKind::Workaround { .. }))
        .expect("a workaround");
    assert_eq!(
        signal.kind,
        SignalKind::Workaround {
            tool: "terminal".into(),
            failures: 2
        }
    );
    assert!(
        signal
            .evidence
            .iter()
            .any(|e| e.text.contains("WORKED terminal: pnpm test"))
    );
    assert!(
        signal
            .evidence
            .iter()
            .any(|e| e.text.contains("FAILED terminal: npx jest"))
    );
}

#[test]
fn not_every_failure_and_success_is_a_workaround() {
    let none = |calls: Vec<CallRecord>| {
        !analyze(&base(&[], "go"), &outcome(calls))
            .signals
            .iter()
            .any(|s| matches!(s.kind, SignalKind::Workaround { .. }))
    };
    // One failure only.
    assert!(none(vec![
        terminal("npm test", true),
        terminal("pnpm test", false)
    ]));
    // The same command again (a code fix in between, not a workaround).
    assert!(none(vec![
        terminal("cargo test", true),
        terminal("cargo test", true),
        terminal("cargo test", false),
    ]));
    // A success of the tool in between breaks the streak.
    assert!(none(vec![
        terminal("a", true),
        terminal("b", false),
        terminal("c", true),
        terminal("d", false),
    ]));
    // Exploration tools fail as part of normal work.
    let read =
        |path: &str, failed| CallRecord::new("read_file", &json!({"path": path}), "x", failed);
    assert!(none(vec![
        read("a", true),
        read("b", true),
        read("c", false)
    ]));
}

#[test]
fn a_long_successful_turn_is_a_signal() {
    let calls: Vec<CallRecord> = (0..LONG_TURN_CALLS)
        .map(|i| CallRecord::new("read_file", &json!({"path": format!("f{i}")}), "x", false))
        .collect();
    let analysis = analyze(&base(&[], "port the module"), &outcome(calls.clone()));
    let signal = analysis
        .signals
        .iter()
        .find(|s| matches!(s.kind, SignalKind::LongTurn { .. }))
        .expect("a long turn");
    assert_eq!(signal.weight, LONG_TURN_WEIGHT);
    assert!(
        matches!(&signal.kind, SignalKind::LongTurn { calls: 15, tools } if tools == "read_file ×15")
    );

    // One call short, cancelled, or ended without an answer: nothing.
    let mut short = outcome(calls[1..].to_vec());
    short.call_count = LONG_TURN_CALLS - 1;
    assert!(analyze(&base(&[], "x"), &short).is_empty());
    let mut cancelled = outcome(calls.clone());
    cancelled.completed = false;
    assert!(analyze(&base(&[], "x"), &cancelled).is_empty());
    let mut unanswered = outcome(calls);
    unanswered.answer = None;
    assert!(analyze(&base(&[], "x"), &unanswered).is_empty());

    // Very long: enough on its own.
    let mut very = outcome(Vec::new());
    very.call_count = VERY_LONG_TURN_CALLS;
    assert_eq!(
        analyze(&base(&[], "x"), &very).signals[0].weight,
        VERY_LONG_TURN_WEIGHT
    );
}

#[test]
fn steers_are_read_too() {
    let history = vec![user("Refactor the parser."), assistant("On it.")];
    let mut done = outcome(Vec::new());
    done.steers = vec!["wait, don't touch the lexer".into()];
    let analysis = analyze(&base(&history, "Refactor the parser."), &done);
    assert_eq!(analysis.signals.len(), 1);
    assert!(matches!(
        analysis.signals[0].kind,
        SignalKind::Correction { .. }
    ));
    assert_eq!(analysis.signals[0].message_id, None);
}

#[test]
fn rewinding_past_an_answer_is_a_strong_signal() {
    let messages = vec![
        user("first"),
        assistant("answer one"),
        user("second"),
        call("c1", "terminal", json!({"command": "ls"})),
        result("c1", "x"),
        assistant("answer two"),
    ];
    let signal = rewind_signal(&messages, 2, "undo", |_| false).unwrap();
    assert_eq!(signal.weight, REWIND_WEIGHT);
    assert_eq!(
        signal.kind,
        SignalKind::Rewind {
            via: "undo".into(),
            turns: 1
        }
    );
    assert_eq!(signal.turn, 2);
    let texts: Vec<&str> = signal.evidence.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, vec!["answer one", "second", "answer two"]);
    // Alone it crosses the threshold.
    let mut stored = SessionSignals::default();
    stored.add(signal, 1);
    assert!(stored.triggered());

    // Nothing the agent did is dropped: no signal.
    assert!(rewind_signal(&messages, 6, "rewind", |_| false).is_none());
    assert!(rewind_signal(&[user("only a question")], 0, "rewind", |_| false).is_none());
    let report = assistant("Undid 1 turn.");
    assert!(
        rewind_signal(&[user("x"), report.clone()], 0, "rewind", |m| m.id
            == report.id)
        .is_none()
    );
}

// ---- the session's file ----------------------------------------------------------------------

fn correction(weight: f32) -> Signal {
    Signal::new(
        SignalKind::Correction {
            phrases: vec!["no,".into()],
        },
        weight,
        1,
        None,
    )
    .with_evidence([Excerpt::user(&"x".repeat(5000))])
}

#[test]
fn excerpts_and_signals_are_capped() {
    let mut stored = SessionSignals::default();
    for i in 0..(MAX_SIGNALS + 10) {
        stored.add(correction(if i == 0 { 0.5 } else { 1.0 }), i as i64);
    }
    assert_eq!(stored.signals.len(), MAX_SIGNALS);
    assert_eq!(stored.dropped, 10);
    // The lightest went first; the dropped weight still counts.
    assert!(stored.signals.iter().all(|s| s.weight == 1.0));
    assert_eq!(stored.score(), 0.5 + 9.0 + MAX_SIGNALS as f32);
    // Excerpts are cut (plus the ellipsis).
    assert_eq!(
        stored.signals[0].evidence[0].text.chars().count(),
        MAX_EXCERPT + 1
    );

    // Reviewed signals are pushed out first, and stop counting.
    let last = stored.next_seq;
    stored.mark_reviewed(last);
    assert_eq!(stored.score(), 0.0);
    assert!(!stored.triggered());
    stored.add(correction(2.0), 99);
    assert_eq!(stored.signals.len(), MAX_SIGNALS);
    assert_eq!(stored.score(), 2.0);
    assert_eq!(stored.evidence().len(), 1);
}

#[test]
fn explicit_signals_trigger_and_survive_the_cap() {
    let mut stored = SessionSignals::default();
    stored.add(
        Signal::new(
            SignalKind::Explicit {
                via: "remember".into(),
            },
            EXPLICIT_WEIGHT,
            1,
            None,
        ),
        1,
    );
    for _ in 0..MAX_SIGNALS {
        stored.add(correction(0.75), 2);
    }
    assert!(stored.signals.iter().any(Signal::is_explicit));
    assert!(stored.triggered());
}

#[test]
fn the_file_round_trips_and_a_damaged_one_starts_afresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut stored = SessionSignals::default();
    stored.add(correction(1.5), 7);
    stored.add(
        Signal::new(
            SignalKind::Denial {
                what: "terminal: rm".into(),
                reason: Some("keep it".into()),
            },
            DENIAL_WEIGHT,
            2,
            Some("m".into()),
        ),
        8,
    );
    save(dir.path(), "abc-123", &stored).unwrap();
    let loaded = load(dir.path(), "abc-123");
    assert_eq!(loaded.signals, stored.signals);
    assert_eq!(loaded.v, FILE_VERSION);
    let raw: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("abc-123.json")).unwrap()).unwrap();
    assert_eq!(raw["signals"][1]["kind"], "denial");
    assert_eq!(raw["signals"][1]["reason"], "keep it");

    std::fs::write(dir.path().join("bad.json"), "{ nope").unwrap();
    assert_eq!(load(dir.path(), "bad"), SessionSignals::default());
    // An id that isn't a plain name never becomes a path.
    assert!(file(dir.path(), "../evil").is_none());
    assert_eq!(load(dir.path(), "../evil"), SessionSignals::default());
}

#[tokio::test]
async fn the_store_applies_changes_in_order_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = SignalStore::open(dir.path().to_path_buf());
    let history = vec![user("x"), assistant("y")];
    store.record(
        "s1",
        analyze(&base(&history, "No, that's wrong."), &outcome(Vec::new())),
    );
    store.record("s1", Analysis::default());
    store.record(
        "s1",
        analyze(
            &base(&history, "No, use pnpm instead."),
            &outcome(Vec::new()),
        ),
    );
    let now = store.snapshot("s1").await;
    assert_eq!(now.signals.len(), 2);
    assert_eq!(
        now.signals.iter().map(|s| s.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(now.signals.iter().all(|s| s.at > 0));
    assert!(now.triggered(), "two corrections: {}", now.score());
    store.mark_reviewed("s1", 2);
    assert_eq!(store.snapshot("s1").await.score(), 0.0);
    drop(store);

    // A new process reads the same file.
    let store = SignalStore::open(dir.path().to_path_buf());
    let again = store.snapshot("s1").await;
    assert_eq!(again.signals.len(), 2);
    assert_eq!(again.reviewed_seq, 2);
    store.forget("s1");
    assert_eq!(store.snapshot("s1").await, SessionSignals::default());
    assert!(!dir.path().join("s1.json").exists());
}

/// The reviewer's adversarial corpus (CP7.1 review): ordinary messages that must not score, and
/// the corrections next to them that still must.
fn reviewer_corpus() -> Vec<(&'static str, TextContext, Expect)> {
    vec![
        // ---- courtesy, idioms, status reports ----
        ("no problem", AFTER, Zero),
        ("No problem at all, next task: add logging.", AFTER, Zero),
        ("no worries", AFTER, Zero),
        ("I don't know", AFTER, Zero),
        ("never mind", AFTER, Zero),
        ("always a pleasure", AFTER, Zero),
        ("don't worry about it", AFTER, Zero),
        ("Don't forget to close the file handle.", AFTER, Zero),
        ("No idea why it fails.", AFTER, Zero),
        ("No luck with the new version either.", AFTER, Zero),
        ("No, I haven't tried that yet.", AFTER, Zero),
        ("No rush.", AFTER, Zero),
        ("Nope, still the same error.", AFTER, Zero),
        ("No, it still fails with the same error.", AFTER, Zero),
        ("No. Still broken.", AFTER, Zero),
        ("I never said it was easy, take your time.", AFTER, Zero),
        ("Why did you choose tokio here? Just curious.", AFTER, Zero),
        ("You missed nothing, all good.", AFTER, Zero),
        ("Stop the dev server and restart it.", AFTER, Zero),
        ("ok", AFTER, Zero),
        ("thanks", AFTER, Zero),
        ("Always.", ANSWER, Zero),
        ("No, never.", ANSWER, Zero),
        // ---- "instead" as narration, "wrong" as a topic, forecasts ----
        (
            "Instead of reqwest I used ureq in the other project, it was fine.",
            AFTER,
            Zero,
        ),
        (
            "I went with sqlite instead of postgres for the prototype.",
            AFTER,
            Zero,
        ),
        ("We ended up using a HashMap instead.", AFTER, Zero),
        ("It'd be wrong to say it's done.", AFTER, Zero),
        ("The test checks the wrong-password path.", AFTER, Zero),
        ("Add a test for the wrong path case.", AFTER, Zero),
        ("Handle the case when the key is wrong.", AFTER, Zero),
        ("In the future this module will be split.", AFTER, Zero),
        ("Next time the job runs, it should log more.", AFTER, Zero),
        (
            "Whenever you get a chance, look at the flaky test.",
            AFTER,
            Zero,
        ),
        // ---- "remember" that isn't a request to remember ----
        (
            "Remember that function we wrote yesterday? Can you extend it?",
            AFTER,
            Zero,
        ),
        (
            "Remember that function we wrote yesterday, extend it.",
            AFTER,
            Zero,
        ),
        (
            "Remember when we fixed the parser? Same issue here.",
            AFTER,
            Zero,
        ),
        ("remember the config file from before, open it", AFTER, Zero),
        // ---- pasted things: code only, diffs, compiler output, tables, chats, URLs, paths ----
        (
            "```\nNo, that's wrong. Never do this. From now on use X.\n```",
            AFTER,
            Zero,
        ),
        ("```\nfn main() {}\n```", AFTER, Zero),
        (
            "```diff\n- // never call this\n+ // always call this\n```\nlooks good?",
            AFTER,
            Zero,
        ),
        (
            "error[E0308]: mismatched types\n --> src/main.rs:3:5\n  |\n3 |     wrong\n  |     ^^^^^ expected `i32`\nhelp?",
            AFTER,
            Zero,
        ),
        (
            "thread 'main' panicked at 'no, wrong state', src/lib.rs:10:5\nwhat now",
            AFTER,
            Zero,
        ),
        (
            "| option | meaning |\n|---|---|\n| never | don't retry |\n| always | retry |\nadd this table to the docs",
            AFTER,
            Zero,
        ),
        (
            "User: no, that's wrong\nAssistant: sorry\nUser: never do that again\n\ncan you summarize this chat?",
            AFTER,
            Zero,
        ),
        (
            "Alice: Don't merge it yet\nBob: Always rebase first\nsummarize",
            AFTER,
            Zero,
        ),
        (
            "check https://example.com/never/wrong/instead?always=1",
            AFTER,
            Zero,
        ),
        (r"open C:\Users\me\never\wrong\instead.txt", AFTER, Zero),
        (
            "Here's the stack:\n  at Foo.bar (foo.js:1:2)\n  at never (x.js:3:4)\nfix?",
            AFTER,
            Zero,
        ),
        // ---- other languages in Latin script, shouting, typos ----
        ("Nein, das ist falsch, nimm immer pnpm.", AFTER, Zero),
        ("Non, c'est faux, utilise toujours pnpm.", AFTER, Zero),
        ("No, è sbagliato, usa sempre pnpm.", AFTER, Zero),
        (
            "WHY IS THE BUILD SO SLOW TODAY? CAN YOU CHECK THE CACHE",
            AFTER,
            Zero,
        ),
        ("teh build is borken, cant you fix it", AFTER, Zero),
        ("dont know whats wrong with it, check pls", AFTER, Zero),
        // ---- still corrections ----
        ("NO THATS WRONG", AFTER, AtLeast(1.25)),
        ("I asked you to use pnpm instead.", AFTER, AtLeast(1.0)),
        ("Why didn't you run the tests first?", AFTER, AtLeast(1.0)),
        ("I never said to delete it.", AFTER, AtLeast(1.0)),
        ("I didn't ask for a rewrite.", AFTER, AtLeast(1.0)),
        ("You forgot the migration again.", AFTER, AtLeast(1.0)),
        (
            "From now on you will ask before pushing.",
            AFTER,
            AtLeast(2.0),
        ),
        ("No, use the other config instead.", AFTER, AtLeast(1.75)),
        ("Note: never push to main.", AFTER, AtLeast(1.0)),
        (
            "Remember that the deploy key lives in the vault.",
            AFTER,
            Explicit,
        ),
        ("Remember: deploys happen on Fridays.", AFTER, Explicit),
        ("ok, remember that I use PowerShell.", AFTER, Explicit),
    ]
}

#[test]
fn the_reviewer_s_corpus_scores_as_labelled() {
    let corpus = reviewer_corpus();
    assert!(corpus.len() >= 40, "the corpus has {} cases", corpus.len());
    let mut failures = Vec::new();
    for (text, context, expect) in &corpus {
        let score = score_text(text, *context);
        let ok = match expect {
            Zero => score.weight == 0.0 && !score.explicit,
            AtLeast(min) => {
                !score.explicit && score.weight >= *min && score.weight < REVIEW_THRESHOLD
            }
            Explicit => score.explicit,
        };
        if !ok {
            failures.push(format!(
                "{text:?} ({context:?}): expected {expect:?}, got {score:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_huge_message_is_read_quickly() {
    // A pasted multi-megabyte log on the turn's task: only its start is read.
    let huge = format!("No, that's wrong.\n{}", "\"a\" 'b' x/y\n".repeat(400_000));
    let started = std::time::Instant::now();
    let score = score_text(&huge, AFTER);
    assert!(score.weight >= 1.0, "{score:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn the_same_correction_sent_again_counts_once() {
    // `/undo` and resend: the same words to the same answer.
    let history = vec![user("Install the deps."), assistant("I ran npm install.")];
    let mut signals = SessionSignals::default();
    for _ in 0..3 {
        for signal in analyze(&base(&history, "No, use pnpm."), &outcome(Vec::new())).signals {
            signals.add(signal, 1);
        }
    }
    assert_eq!(signals.signals.len(), 1);
    // The same words to a later answer are another correction.
    let later = vec![
        user("Install the deps."),
        assistant("I ran npm install."),
        user("Now the dev deps."),
        assistant("I ran npm install -D."),
    ];
    for signal in analyze(&base(&later, "No, use pnpm."), &outcome(Vec::new())).signals {
        signals.add(signal, 2);
    }
    assert_eq!(signals.signals.len(), 2);
    // After a review, the same words count again.
    signals.mark_reviewed(signals.next_seq);
    for signal in analyze(&base(&later, "No, use pnpm."), &outcome(Vec::new())).signals {
        signals.add(signal, 3);
    }
    assert_eq!(signals.evidence().len(), 1);
}

#[test]
fn secrets_pasted_by_the_user_are_masked_in_the_evidence() {
    let key = "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef";
    let typed = format!("Remember this: my OpenRouter key is {key}");
    let history = vec![user("hi"), assistant("Hello!")];
    let denied = CallRecord::new(
        "terminal",
        &json!({"command": format!("curl -H 'Authorization: Bearer {key}' https://x")}),
        "BLOCKED: User denied this command.",
        true,
    );
    let analysis = analyze(&base(&history, &typed), &outcome(vec![denied]));
    let stored = serde_json::to_string(&analysis.signals).unwrap();
    assert!(!analysis.signals.is_empty());
    assert!(!stored.contains(key), "{stored}");
    assert!(
        !stored.contains("0123456789abcdef0123456789abcdef"),
        "{stored}"
    );
}

#[test]
fn probing_and_unrelated_commands_are_not_workarounds() {
    let none = |calls: Vec<CallRecord>| {
        !analyze(&base(&[], "go"), &outcome(calls))
            .signals
            .iter()
            .any(|s| matches!(s.kind, SignalKind::Workaround { .. }))
    };
    // Searching: `rg` exits 1 on no match.
    assert!(none(vec![
        terminal("rg foo src", true),
        terminal("rg Foo src", true),
        terminal("rg fooBar src", false),
    ]));
    assert!(none(vec![
        terminal("git grep foo", true),
        terminal("test -f a.txt", true),
        terminal("Test-Path b.txt", true),
        terminal("ls src", false),
    ]));
    // The build failing while the agent fixes the code, then the next step of the work.
    assert!(none(vec![
        terminal("cargo build", true),
        terminal("cargo build", true),
        terminal("git add -A", false),
    ]));
    assert!(none(vec![
        terminal("cargo test", true),
        terminal("cd crates/x && cargo test", true),
        terminal("cargo clippy", false),
    ]));
    // The same command failing, then a release build: a fix, not a workaround.
    assert!(none(vec![
        terminal("cargo build", true),
        terminal("cargo build", true),
        terminal("cargo build --release", false),
    ]));
    // Still a workaround: other tries at the same thing.
    assert!(!none(vec![
        terminal("pip install foo", true),
        terminal("pip3 install foo", true),
        terminal("pip install --user foo", false),
    ]));
}

#[test]
fn a_file_from_a_newer_version_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let newer = r#"{"v": 99, "signals": [{"kind": "something_new", "weight": 1.0}]}"#;
    std::fs::write(dir.path().join("s1.json"), newer).unwrap();
    let history = vec![user("x"), assistant("y")];
    apply(
        dir.path(),
        Op::Record {
            session: "s1".into(),
            analysis: analyze(&base(&history, "No, that's wrong."), &outcome(Vec::new())),
        },
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("s1.json")).unwrap(),
        newer
    );
}

#[test]
fn remember_this_the_agent_already_saved_is_only_a_weak_signal() {
    let history = vec![user("Install the deps."), assistant("I ran npm install.")];
    let typed = "Remember this: I always use pnpm, never npm.";
    let saved = CallRecord::new(
        "memory",
        &json!({"target": "user", "operations": [{"action": "add", "content": "Uses pnpm."}]}),
        r#"{"success":true}"#,
        false,
    );
    let analysis = analyze(&base(&history, typed), &outcome(vec![saved]));
    assert_eq!(analysis.signals.len(), 1, "{analysis:?}");
    assert!(!analysis.signals[0].is_explicit());
    assert_eq!(analysis.signals[0].weight, 1.0);
    // Its words are still the evidence.
    assert!(analysis.signals[0].evidence.iter().any(|e| e.text == typed));
    // A refused save, or none, leaves it explicit.
    let refused = CallRecord::new(
        "memory",
        &json!({"target": "user", "operations": [{"action": "add", "content": "x"}]}),
        r#"{"success":false,"error":"threat"}"#,
        true,
    );
    for calls in [vec![refused], Vec::new()] {
        let analysis = analyze(&base(&history, typed), &outcome(calls));
        assert!(analysis.signals[0].is_explicit(), "{analysis:?}");
    }
}

#[test]
fn unfinished_review_tries_wait_from_the_second_on_and_a_review_resets_them() {
    let hour = 3_600_000_i64;
    let mut signals = SessionSignals::default();
    signals.add(correction(3.0), 1);
    assert_eq!(signals.retry_wait(10 * hour), None);
    // The first try cut off (an exit): redone freely.
    signals.review_started(10 * hour);
    assert_eq!(signals.retry_wait(10 * hour), None);
    // The second: an hour from its start, then two, ... at most a day.
    signals.review_started(10 * hour);
    let wait = signals.retry_wait(10 * hour + 1000).unwrap();
    assert_eq!(wait, std::time::Duration::from_millis(3_600_000 - 1000));
    assert_eq!(signals.retry_wait(11 * hour), None);
    signals.review_started(20 * hour);
    assert!(signals.retry_wait(21 * hour + 1).is_some());
    assert_eq!(signals.retry_wait(22 * hour), None);
    for _ in 0..20 {
        signals.review_started(30 * hour);
    }
    assert!(signals.retry_wait(30 * hour + 23 * hour).is_some());
    assert_eq!(signals.retry_wait(30 * hour + 24 * hour), None);
    // A clock set back: no wait.
    assert_eq!(signals.retry_wait(29 * hour), None);
    // Survives the file; a reply read resets it.
    let back: SessionSignals =
        serde_json::from_slice(&serde_json::to_vec(&signals).unwrap()).unwrap();
    assert_eq!(back.review_tries, 23);
    signals.mark_reviewed(signals.next_seq);
    assert_eq!(signals.review_tries, 0);
    assert_eq!(signals.retry_wait(30 * hour), None);
    // A file without tries doesn't mention them.
    let text = serde_json::to_string(&signals).unwrap();
    assert!(!text.contains("review_tries"), "{text}");
}
