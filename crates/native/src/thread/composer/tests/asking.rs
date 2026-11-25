//! CP5.3 in a headless window over a [`FixtureBackend`]: the approval panel and the question panel
//! above the composer. Behaviour (each of the four choices reaches the core's answer, the keys,
//! Escape, numbered shortcuts, a free-text answer, a multi-select, a request settled elsewhere
//! closes the panel at once, two requests queue, a second answer is nothing, a refused answer
//! keeps the request, the composer stays usable) and layout (both panels at 640, 1100 and 1920 px
//! with a very long command, path and reason, many choices, the key hints inside their buttons,
//! the card under the panel).

use gpui_kit::{Entity, Focusable as _, component::input::TextareaState};
use hermes_core::{CoreEvent, QuestionKind};

use super::*;
use crate::model::Backend as _;
use crate::model::backend::ComposerOp;
use crate::thread::composer::ask::{Answer, Choice};
use crate::thread::composer::panel::{INSET, KEY_GUARD, NO_DIFF, OVERLAP};

// --- helpers ------------------------------------------------------------------------------------

const ALL: [&str; 4] = ["once", "session", "always", "deny"];

impl Rig {
    /// The core asks for an approval in `t1`, and the user looks at it for a moment (past the
    /// panel's [`KEY_GUARD`]).
    fn ask_approval(&mut self, id: &str, command: &str, reason: &str, choices: &[&'static str]) {
        self.ask_approval_now(id, command, reason, choices);
        self.wait_past_guard();
    }

    /// The same with no time for the user to look: a key now was meant for something else.
    fn ask_approval_now(&mut self, id: &str, command: &str, reason: &str, choices: &[&'static str]) {
        let event = CoreEvent::ApprovalRequested {
            session_id: "t1".into(),
            prompt_id: id.into(),
            command: command.into(),
            description: reason.into(),
            choices: choices.to_vec(),
        };
        self.backend.ask(event.clone());
        self.emit(event);
        self.settle();
    }

    fn wait_past_guard(&mut self) {
        self.cx.advance_clock(KEY_GUARD + std::time::Duration::from_millis(10));
        self.settle();
    }

    /// A key as the keyboard sends it while it is held down (auto-repeat).
    fn press_held(&mut self, key: &'static str) {
        let keystroke = gpui_kit::Keystroke::parse(key).ok().expect("a key");
        self.with_window(move |window, cx| {
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke,
                    is_held: true,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
    }

    /// The agent asks a question in `t1`.
    fn ask_question(&mut self, id: &str, question: &str, choices: &[&str], multi: bool) {
        let event = CoreEvent::ClarifyRequested {
            session_id: "t1".into(),
            prompt_id: id.into(),
            question: question.into(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            multi_select: multi,
        };
        self.backend.ask(event.clone());
        self.emit(event);
        self.wait_past_guard();
    }

    /// The core settles a request (answered elsewhere, the turn stopped, the time ran out).
    fn settle_ask(&mut self, id: &str, kind: QuestionKind) {
        self.emit(CoreEvent::Settled {
            session_id: "t1".into(),
            prompt_id: id.into(),
            kind,
        });
    }

    fn answers(&self) -> Vec<(String, Answer)> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                ComposerOp::Answer { prompt_id, answer } => Some((prompt_id, answer)),
                _ => None,
            })
            .collect()
    }

    fn panel_open(&mut self) -> bool {
        self.present("ask-panel")
    }

    /// The panel has the keyboard (it takes it when it appears and nothing is typed).
    fn panel_focused(&mut self) -> bool {
        let view = self.view.clone();
        self.cx
            .update_window(self.window, |_, window, cx| {
                view.read(cx).ask_focus.is_focused(window)
            })
            .unwrap()
    }

    fn field(&mut self) -> Entity<TextareaState> {
        let view = self.view.clone();
        self.read(|cx| view.read(cx).answer_input.clone())
    }

    fn type_in_field(&mut self, text: &str) {
        let field = self.field();
        self.with_window(move |window, cx| {
            field.update(cx, |input, cx| input.focus(window, cx));
            window.input(text, cx);
        });
    }

    fn input_placeholder(&mut self) -> String {
        let view = self.view.clone();
        self.read(|cx| view.read(cx).input_hint().to_string())
    }
}

fn request_ids(rig: &mut Rig) -> Vec<String> {
    let model = rig.model.clone();
    rig.read(|cx| model.read(cx).asks().iter().map(|r| r.prompt_id.clone()).collect())
}

// --- the approval panel ---------------------------------------------------------------------------

#[core::prelude::v1::test]
fn each_of_the_four_buttons_gives_the_core_its_answer() {
    for (button, choice) in [
        ("approval-once", Choice::Once),
        ("approval-session", Choice::Session),
        ("approval-always", Choice::Always),
        ("approval-deny", Choice::Deny),
    ] {
        let mut rig = running_rig(Vec::new());
        rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
        assert!(rig.panel_open(), "{button}: the panel is open");
        rig.click(button);
        if choice == Choice::Always {
            // It asks first; the confirmation's button gives the answer.
            assert!(rig.answers().is_empty(), "always: nothing before the confirmation");
            assert!(rig.present("approval-confirm"));
            rig.click("approval-confirm-always");
        }
        assert_eq!(
            rig.answers(),
            [("p1".to_string(), Answer::Approval(choice))],
            "{button}"
        );
        assert!(!rig.panel_open(), "{button}: the panel closes when the core took the answer");
    }
}

#[core::prelude::v1::test]
fn the_panel_says_the_command_and_why_it_was_flagged() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    let layout = rig.layout();
    assert!(layout.get("approval-command").is_ok());
    assert!(layout.get("approval-reason").is_ok());
    assert!(layout.get("approval-diff").is_err(), "a command has no diff");
    // All four are offered, in the order Deny, session, always, once (the primary last).
    let xs: Vec<f32> = ["approval-deny", "approval-session", "approval-always", "approval-once"]
        .iter()
        .map(|n| layout.bounds(n).left().as_f32())
        .collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
}

#[core::prelude::v1::test]
fn a_mode_that_asks_every_time_offers_once_and_deny_only() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "chmod 777 x", "chmod 777", &["once", "deny"]);
    assert!(rig.present("approval-once") && rig.present("approval-deny"));
    assert!(!rig.present("approval-session") && !rig.present("approval-always"));
}

#[core::prelude::v1::test]
fn the_panel_takes_the_keyboard_and_enter_allows_once_escape_denies() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    assert!(rig.panel_focused(), "nothing is typed: the panel has the keys");
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Once))]);
    assert!(!rig.panel_open());

    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    rig.with_window(|window, cx| window.press("escape", cx));
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Deny))]);
}

#[core::prelude::v1::test]
fn digits_pick_the_nth_choice_and_arrows_move_the_highlight() {
    for (key, choice) in [
        ("1", Choice::Once),
        ("2", Choice::Session),
        ("3", Choice::Always),
        ("4", Choice::Deny),
    ] {
        let mut rig = running_rig(Vec::new());
        rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
        rig.with_window(move |window, cx| window.press(key, cx));
        if choice == Choice::Always {
            assert!(rig.answers().is_empty(), "3 asks for the confirmation first");
            rig.with_window(|window, cx| window.press("enter", cx));
        }
        assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(choice))], "key {key}");
    }
    // Down twice from "once" is "always"; Enter asks to confirm it, Escape goes back (no
    // denial), Enter again and Enter confirms.
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    rig.with_window(|window, cx| {
        window.press("down", cx);
        window.press("down", cx);
    });
    assert!(rig.answers().is_empty(), "moving answers nothing");
    rig.with_window(|window, cx| window.press("enter", cx));
    assert!(rig.answers().is_empty() && rig.present("approval-confirm"));
    rig.with_window(|window, cx| window.press("escape", cx));
    assert!(rig.answers().is_empty(), "Escape leaves the confirmation, it does not deny");
    assert!(!rig.present("approval-confirm") && rig.present("approval-actions"));
    rig.with_window(|window, cx| window.press("enter", cx));
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Always))]);
    // A key that is not the panel's is not taken: a digit past the choices does nothing.
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "chmod 777 x", "chmod 777", &["once", "deny"]);
    rig.with_window(|window, cx| window.press("3", cx));
    assert!(rig.answers().is_empty());
    assert!(rig.panel_open());
}

#[core::prelude::v1::test]
fn escape_in_an_empty_composer_denies_but_enter_there_allows_nothing() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    rig.focus_input();
    rig.with_window(|window, cx| window.press("enter", cx));
    assert!(rig.answers().is_empty(), "an Enter in the input is not an approval");
    assert!(rig.sends().is_empty(), "and sends nothing");
    rig.with_window(|window, cx| window.press("escape", cx));
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Deny))]);
}

#[core::prelude::v1::test]
fn a_composer_with_text_keeps_it_and_the_panel_does_not_take_the_keyboard() {
    let mut rig = running_rig(Vec::new());
    rig.type_text("also check the tests");
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    assert!(!rig.panel_focused(), "the user is typing: the panel waits for a click");
    // Escape in a composer with text is not a denial.
    rig.with_window(|window, cx| window.press("escape", cx));
    assert!(rig.answers().is_empty());
    assert_eq!(rig.text(), "also check the tests");
    // Enter queues the message behind the waiting turn, as always, and answers nothing.
    rig.with_window(|window, cx| window.press("enter", cx));
    assert!(rig.answers().is_empty());
    assert_eq!(rig.sends(), [("also check the tests".to_string(), false)]);
    assert!(rig.panel_open(), "the request still waits");
}

#[core::prelude::v1::test]
fn the_input_says_a_message_is_queued_while_a_request_waits() {
    let mut rig = running_rig(Vec::new());
    let normal = rig.input_placeholder();
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    let waiting = rig.input_placeholder();
    assert_ne!(normal, waiting);
    assert!(waiting.contains("queue"), "{waiting}");
    rig.click("approval-once");
    assert_eq!(rig.input_placeholder(), normal, "back to the usual words");
}

#[core::prelude::v1::test]
fn a_request_settled_elsewhere_closes_the_panel_without_an_answer() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    assert!(rig.panel_open());
    rig.settle_ask("p1", QuestionKind::Approval);
    assert!(!rig.panel_open());
    assert!(rig.answers().is_empty(), "nobody answered from here");
    // Nor does a stopped or finished turn leave one open.
    rig.ask_approval("p2", "rm -rf build", "recursive delete", &ALL);
    assert!(rig.panel_open());
    rig.emit(CoreEvent::Idle {
        session_id: "t1".into(),
    });
    assert!(!rig.panel_open());
}

#[core::prelude::v1::test]
fn two_requests_queue_in_order_and_the_second_shows_when_the_first_is_answered() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "first command", "run a command", &ALL);
    rig.ask_approval("p2", "second command", "run a command", &ALL);
    assert!(rig.present("ask-count"), "1 of 2 is said");
    let first = rig.layout().bounds("approval-command");
    rig.click("approval-once");
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Once))]);
    assert!(rig.panel_open(), "the second request is on screen now");
    assert!(!rig.present("ask-count"), "alone now");
    let second = rig.layout().bounds("approval-command");
    assert_eq!(first.size.width, second.size.width);
    rig.click("approval-deny");
    assert_eq!(
        rig.answers(),
        [
            ("p1".into(), Answer::Approval(Choice::Once)),
            ("p2".into(), Answer::Approval(Choice::Deny))
        ]
    );
    assert!(!rig.panel_open());
}

#[core::prelude::v1::test]
fn a_second_answer_to_the_same_request_is_nothing() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    // Two clicks and a key before the panel could close: one answer reaches the core.
    let model = rig.model.clone();
    rig.cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.answer("p1", Answer::Approval(Choice::Once), cx);
            m.answer("p1", Answer::Approval(Choice::Deny), cx);
        })
    });
    rig.settle();
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Once))]);
    // And once it is closed, an answer to it reaches nobody.
    let model = rig.model.clone();
    rig.cx.update(|cx| {
        model.update(cx, |m, cx| m.answer("p1", Answer::Approval(Choice::Deny), cx))
    });
    rig.settle();
    assert_eq!(rig.answers().len(), 1);
}

#[core::prelude::v1::test]
fn a_request_the_core_no_longer_has_still_closes_the_panel() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    // Answered from elsewhere an instant before: the backend says it is closed.
    rig.backend.answer("p1", &Answer::Approval(Choice::Deny)).unwrap();
    rig.click("approval-once");
    assert!(!rig.panel_open(), "no longer open is not an error");
    assert!(rig.notices().is_empty());
}

#[core::prelude::v1::test]
fn a_refused_answer_keeps_the_request_and_says_so() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    rig.backend.fail_with(Some("the core is busy"));
    rig.click("approval-once");
    assert!(rig.panel_open(), "the user can try again");
    assert_eq!(
        rig.notices(),
        [(Kind::Error, "Could not send the answer".to_string())],
        "the user is told"
    );
    rig.backend.fail_with(None);
    rig.click("approval-once");
    assert!(!rig.panel_open());
}

#[core::prelude::v1::test]
fn an_edit_shows_the_file_and_the_diff_of_the_running_call() {
    let mut rig = running_rig(Vec::new());
    rig.emit(CoreEvent::TurnStarted {
        session_id: "t1".into(),
        admission_id: "a".into(),
    });
    rig.emit(CoreEvent::ToolStarted {
        session_id: "t1".into(),
        tool_id: "c1".into(),
        name: "write_file".into(),
        args: serde_json::json!({"path": "src/lib.rs", "content": "fn main() {}\nfn other() {}\n"}),
    });
    rig.ask_approval("p1", "write_file src/lib.rs", "file edit", &["once", "deny"]);
    let layout = rig.layout();
    assert!(layout.get("approval-edit").is_ok());
    assert!(layout.get("approval-path").is_ok(), "the path");
    assert!(layout.get("approval-diff").is_ok(), "the diff");
    assert!(layout.get("approval-command").is_err());
    assert!(!layout.get("approval-no-diff").is_ok());
    // Another file's approval has no card to take a diff from: the path, and the panel says the
    // diff is not available (never another call's diff, never a silent gap).
    rig.settle_ask("p1", QuestionKind::Approval);
    rig.ask_approval("p2", "write_file other.rs", "file edit", &["once", "deny"]);
    assert!(rig.present("approval-path") && !rig.present("approval-diff"));
    assert!(rig.present("approval-no-diff"));
    assert!(!NO_DIFF.is_empty());
}

// --- review (CP5.3): keys that were not meant as answers, what is shown is what runs ---------------

#[core::prelude::v1::test]
fn a_key_in_the_first_moments_of_a_request_answers_nothing() {
    for key in ["enter", "2", "1", "escape"] {
        let mut rig = running_rig(Vec::new());
        rig.ask_approval_now("p1", "rm -rf build", "recursive delete", &ALL);
        assert!(rig.panel_focused());
        rig.with_window(move |window, cx| window.press(key, cx));
        assert!(rig.answers().is_empty(), "{key}: too soon to be an answer");
        assert!(rig.panel_open());
        // Looked at, the same key answers.
        rig.wait_past_guard();
        rig.with_window(move |window, cx| window.press(key, cx));
        assert_eq!(rig.answers().len(), 1, "{key}");
    }
}

#[core::prelude::v1::test]
fn a_held_enter_does_not_approve_the_next_request() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "first", "run a command", &ALL);
    rig.ask_approval("p2", "second", "run a command", &ALL);
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers(), [("p1".into(), Answer::Approval(Choice::Once))]);
    // The key stays down: the repeats reach the second request, long after it appeared.
    rig.wait_past_guard();
    for _ in 0..5 {
        rig.press_held("enter");
    }
    assert_eq!(rig.answers().len(), 1, "a held Enter is not an answer");
    assert!(rig.panel_open());
    // A fresh press is.
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers().len(), 2);
}

#[core::prelude::v1::test]
fn the_panel_does_not_take_the_keyboard_from_outside_the_composer() {
    let mut rig = running_rig(Vec::new());
    // Something else in the window has the keyboard (the sidebar's search, a dialog).
    let elsewhere = rig.cx.update(|cx| cx.focus_handle());
    let other = elsewhere.clone();
    rig.with_window(move |window, cx| window.focus(&other, cx));
    rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    assert!(!rig.panel_focused(), "the panel waits for a click");
    rig.with_window(|window, cx| window.press("enter", cx));
    assert!(rig.answers().is_empty());
    drop(elsewhere);
}

#[core::prelude::v1::test]
fn hidden_and_control_characters_in_a_command_are_shown_escaped() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval(
        "p1",
        "echo ok\u{202e}txt.exe\nrm -rf \x1b[8mx",
        "run a command",
        &ALL,
    );
    // The text drawn is `ask::visible` of the command (unit-tested there); two lines are taller
    // than one, and the panel says what was escaped.
    assert!(rig.layout().bounds("approval-command").size.height.as_f32() > 40.);
    assert!(rig.present("approval-warning"), "the panel says so");
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "c\u{0430}t notes.txt", "run a command", &ALL);
    assert!(rig.present("approval-warning"), "a lookalike letter is pointed out");
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "cat notes.txt", "run a command", &ALL);
    assert!(!rig.present("approval-warning"));
}

#[core::prelude::v1::test]
fn always_allow_says_what_it_persists_and_cancel_goes_back() {
    let mut rig = running_rig(Vec::new());
    rig.ask_approval("p1", "chmod 777 x", "chmod 777", &ALL);
    rig.click("approval-always");
    assert!(rig.present("approval-confirm-text") && !rig.present("approval-actions"));
    let words = crate::thread::composer::panel::always_words("chmod 777");
    assert!(words.contains("\u{201c}chmod 777\u{201d}") && words.contains("permanent"), "{words}");
    rig.click("approval-confirm-cancel");
    assert!(rig.answers().is_empty());
    assert!(rig.present("approval-actions") && rig.panel_open());
}

#[core::prelude::v1::test]
fn on_a_narrow_card_the_buttons_wrap_as_two_right_aligned_pairs() {
    let mut rig = ask_rig(352. + 2. * 16., |rig| {
        rig.ask_approval("p1", "rm -rf build", "recursive delete", &ALL);
    });
    let layout = rig.layout();
    let b = |n: &str| layout.bounds(&format!("approval-{n}"));
    let (deny, session, always, once) = (b("deny"), b("session"), b("always"), b("once"));
    // Deny beside "for this session"; "Always" beside the primary; the primary last.
    assert!((deny.top().as_f32() - session.top().as_f32()).abs() < 0.5);
    assert!((always.top().as_f32() - once.top().as_f32()).abs() < 0.5);
    assert!(deny.left() < session.left() && always.left() < once.left());
    assert!(once.top() >= session.top());
    if once.top() > session.top() {
        assert!((once.right().as_f32() - session.right().as_f32()).abs() < 0.5, "right-aligned");
    }
}

// --- questions ------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn clicking_a_choice_answers_a_single_select_question() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    assert!(rig.present("question-panel") && rig.present("ask-choice-0") && rig.present("ask-choice-1"));
    rig.click("ask-choice-1");
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("green".into()))]);
    assert!(!rig.panel_open());
}

#[core::prelude::v1::test]
fn digits_enter_and_escape_work_on_a_question() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    assert!(rig.panel_focused());
    rig.with_window(|window, cx| window.press("1", cx));
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("blue".into()))]);

    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    rig.with_window(|window, cx| {
        window.press("down", cx);
        window.press("down", cx);
    });
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("green".into()))]);

    // Escape skips: the empty answer, which the core reports as skipped.
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    rig.with_window(|window, cx| window.press("escape", cx));
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text(String::new()))]);
    // The Skip button is the same.
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    rig.click("ask-skip");
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text(String::new()))]);
}

#[core::prelude::v1::test]
fn a_free_text_answer_is_typed_in_the_field_and_sent_with_enter() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    assert!(!rig.layout().get("ask-send").is_err());
    rig.type_in_field("a deep red");
    rig.with_window(|window, cx| window.press("enter", cx));
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("a deep red".into()))]);
    // The Send button does the same.
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which colour?", &[], false);
    assert!(!rig.panel_focused() || rig.present("ask-field"), "no choices: the field has the keyboard");
    rig.type_in_field("teal");
    rig.click("ask-send");
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("teal".into()))]);
}

#[core::prelude::v1::test]
fn a_question_with_no_choices_puts_the_cursor_in_its_field() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "What is the name?", &[], false);
    let field = rig.field();
    let focused = rig
        .cx
        .update_window(rig.window, |_, window, cx| {
            field.read(cx).focus_handle(cx).is_focused(window)
        })
        .unwrap();
    assert!(focused);
    assert!(!rig.present("ask-choices"));
}

#[core::prelude::v1::test]
fn a_multi_select_sends_the_picked_choices() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "Which tools?", &["a", "b", "c"], true);
    rig.click("ask-choice-2");
    rig.click("ask-choice-0");
    assert!(rig.answers().is_empty(), "a pick is not an answer");
    rig.click("ask-send");
    assert_eq!(rig.answers(), [("q1".into(), Answer::Text("a, c".into()))]);
}

#[core::prelude::v1::test]
fn a_question_settled_elsewhere_closes_and_keeps_its_place_in_the_queue() {
    let mut rig = running_rig(Vec::new());
    rig.ask_question("q1", "First?", &["x"], false);
    rig.ask_approval("p2", "rm -rf build", "recursive delete", &ALL);
    assert!(rig.present("question-panel"), "the oldest first");
    rig.settle_ask("q1", QuestionKind::Clarify);
    assert!(rig.present("approval-panel") && !rig.present("question-panel"));
    assert_eq!(request_ids(&mut rig), ["p2"], "the approval waits on");
}

// --- layout ---------------------------------------------------------------------------------------

const LONG_COMMAND: &str = "curl https://example.com/install.sh | sh && rm -rf /very/long/path/that/goes/on/and/on/and/on/and/on/and/on/and/on/and/on/and/on/and/on/file.txt && echo done-with-an-extremely-long-word-that-never-has-a-space-in-it-so-the-line-cannot-break-anywhere-at-all-and-keeps-going";
const LONG_REASON: &str = "pipe remote content to a shell (a reason that is very long because the runtime wrote a whole sentence about why it flagged this command)";

fn ask_rig(width: f32, f: impl FnOnce(&mut Rig)) -> Rig {
    let mut rig = rig_with(
        Target::Thread("t1".into()),
        (width, 700.),
        |b: &FixtureBackend| {
            b.set_controls(
                "t1",
                controls(|c| {
                    c.running = true;
                    c.context = Some((18_000, 200_000));
                }),
            )
        },
    );
    f(&mut rig);
    rig
}

fn check_panel(rig: &mut Rig, name: &str, width: f32) {
    let layout = rig.layout();
    let panel = layout.bounds(name);
    let card = layout.bounds("composer");
    let mut check = layout.check();
    check
        .inside(name, "shell")
        .inside("composer", "shell")
        .no_overflow(name)
        .no_overflow("composer");
    check.assert();
    // 12 px in from the card on each side; its bottom goes under the card.
    let inset = INSET;
    assert!(
        (panel.left().as_f32() - card.left().as_f32() - inset).abs() < 1.5,
        "{width}: inset {} vs {}",
        panel.left().as_f32(),
        card.left().as_f32()
    );
    assert!(
        (panel.size.width.as_f32() - (card.size.width.as_f32() - 2. * inset)).abs() < 1.5,
        "{width}: the panel is 24 px narrower than the card"
    );
    assert!(
        (panel.bottom().as_f32() - card.top().as_f32() - OVERLAP).abs() < 1.5,
        "{width}: tucked {OVERLAP} px under the card ({} / {})",
        panel.bottom().as_f32(),
        card.top().as_f32()
    );
}

#[core::prelude::v1::test]
fn the_approval_panel_fits_above_the_card_at_three_widths_with_a_very_long_command() {
    for width in [640., 1100., 1920.] {
        let mut rig = ask_rig(width, |rig| {
            rig.ask_approval("p1", LONG_COMMAND, LONG_REASON, &ALL);
        });
        check_panel(&mut rig, "approval-panel", width);
        let layout = rig.layout();
        let panel = layout.bounds("approval-panel");
        // The block is capped and scrolls; every button is inside the panel, hints inside buttons.
        assert!(layout.bounds("approval-command").size.height.as_f32() <= 160.5, "{width}");
        let mut check = layout.check();
        for id in ["deny", "session", "always", "once"] {
            check.inside(&format!("approval-{id}"), "approval-panel");
            check.inside(&format!("approval-{id}/hint"), &format!("approval-{id}"));
            check.inside(&format!("approval-{id}/label"), &format!("approval-{id}"));
        }
        check
            .inside("approval-command", "approval-panel")
            .inside("approval-reason", "approval-panel")
            .inside("approval-header", "approval-panel");
        check.assert();
        // The panel is not taller than the window's half; the buttons wrap on a narrow card.
        assert!(panel.size.height.as_f32() < 350., "{width}");
        if width >= 1100. {
            let tops: Vec<f32> = ["deny", "session", "always", "once"]
                .iter()
                .map(|n| layout.bounds(&format!("approval-{n}")).top().as_f32())
                .collect();
            assert!(tops.iter().all(|t| (t - tops[0]).abs() < 0.5), "{width}: one row {tops:?}");
        }
    }
}

#[core::prelude::v1::test]
fn the_edit_panel_fits_with_a_very_long_path() {
    for width in [640., 1100., 1920.] {
        let path = format!("{}/a_file_with_a_long_name.rs", "nested_folder/".repeat(14));
        let mut rig = ask_rig(width, |rig| {
            rig.emit(CoreEvent::ToolStarted {
                session_id: "t1".into(),
                tool_id: "c1".into(),
                name: "patch".into(),
                args: serde_json::json!({"path": path, "old_string": "a\nb\nc", "new_string": "a\nB\nc\nd"}),
            });
            rig.ask_approval("p1", &format!("patch {path}"), "file edit", &["once", "deny"]);
        });
        check_panel(&mut rig, "approval-panel", width);
        let layout = rig.layout();
        let mut check = layout.check();
        check
            .inside("approval-edit", "approval-panel")
            .inside("approval-path", "approval-edit")
            .inside("approval-diff", "approval-edit");
        check.assert();
    }
}

#[core::prelude::v1::test]
fn the_question_panel_fits_with_many_choices_a_long_question_and_the_field() {
    for width in [640., 1100., 1920.] {
        let many: Vec<String> = (1..=12)
            .map(|n| format!("choice number {n} with a rather long label to see how it wraps {}", "word ".repeat(n)))
            .collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let question = format!("A long question. {}", "It keeps going and going. ".repeat(30));
        let mut rig = ask_rig(width, |rig| {
            rig.ask_question("q1", &question, &many, true);
        });
        check_panel(&mut rig, "question-panel", width);
        let layout = rig.layout();
        let body = layout.bounds("ask-body");
        assert!(body.size.height.as_f32() <= 385., "{width}: the body scrolls");
        let mut check = layout.check();
        check
            .inside("ask-body", "question-panel")
            .inside("ask-actions", "question-panel")
            .inside("ask-send", "ask-actions")
            .inside("ask-skip", "ask-actions")
            .inside("ask-send/hint", "ask-send");
        check.assert();
        // The panel never swallows the window.
        assert!(layout.bounds("question-panel").size.height.as_f32() < 520., "{width}");
    }
}

#[core::prelude::v1::test]
fn a_short_question_is_a_short_panel_with_its_choices_and_field_in_order() {
    let mut rig = ask_rig(1100., |rig| {
        rig.ask_question("q1", "Which colour?", &["blue", "green"], false);
    });
    let layout = rig.layout();
    let tops: Vec<f32> = ["ask-question", "ask-choice-0", "ask-choice-1", "ask-field", "ask-actions"]
        .iter()
        .map(|n| layout.bounds(n).top().as_f32())
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");
    // The numbered hints sit at the right of each choice row, aligned.
    let rights: Vec<f32> = (0..2)
        .map(|i| layout.bounds(&format!("ask-choice-{i}/kbd")).right().as_f32())
        .collect();
    assert!((rights[0] - rights[1]).abs() < 0.5, "{rights:?}");
    check_panel(&mut rig, "question-panel", 1100.);
}
