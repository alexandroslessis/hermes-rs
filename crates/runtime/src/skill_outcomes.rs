//! How the turns a skill was used in went: Phase 8 of `docs/backend/PHASES.md` (CP8.1). Each use
//! of a skill gets an outcome (`clean`, `corrected`, `undone`, `errored`) in the skill's record
//! in `.usage.json` (`hermes_tools::curator`), so that a skill that keeps being corrected shows
//! up (CP8.3's curator repairs low performers) and the Skills screen can show a rate.
//!
//! Not in Hermes (`PORTING.md`: diverged). **Zero requests**: everything comes from what the
//! turn already has and from Phase 7's learning signals ([`crate::signals`]).
//!
//! ## What is a use
//!
//! - a skill the local index **attached** to the turn's message (Phase 5, no `skill_view` call), or
//! - a whole-skill `skill_view` call of the turn that went through (a reference file is a detail
//!   of the skill, not a use; `/skill-name` invocations are not counted yet).
//!
//! One use per skill and turn (`session`, `turn`: the user turn's number, as the signals number
//! it). A skill already attached in an earlier turn is not a new use.
//!
//! ## How the outcome becomes known
//!
//! The use is written when its turn ends, with what that turn shows by itself, and may get
//! worse when a later event refers back to the turn (an outcome never gets better):
//!
//! | outcome | when |
//! |---|---|
//! | `clean` | the turn ended without any of the below (this is also what an unjudged use starts as) |
//! | `errored` | the turn failed (provider error, a crash), or a tool failed twice and then worked with other arguments (a `Workaround` signal) in it |
//! | `corrected` | the user denied a command in the turn (a `Denial` signal), **or the next user message corrects the agent** (a `Correction` signal from the message right after the turn) |
//! | `undone` | a rewind (`/undo`, `/retry`, edit and resend, regenerate, checkpoint restore) dropped the turn |
//!
//! The rule for a correction that arrives in a later turn: a `Correction` signal found in turn
//! `T` marks the uses of turn `T - 1` only, the turn whose answer the user is reacting to. A
//! skill used earlier than that, with an unrelated turn in between, is not blamed (the model's
//! own answer in `T - 1` was the last thing the user saw). A correction in a first message
//! marks nothing. A rewind of turns `T..T+n` marks the uses of those turns. A turn the user
//! cancelled is not judged at all (no entry): a stop says nothing about the skill.
//!
//! The signals' `turn` numbers are renumbered by rewinds, as are these: after a rewind the old
//! entries of the dropped turns are `undone`, and new turns reuse their numbers.

use hermes_tools::SkillStore;
use hermes_tools::curator::Outcome;

use crate::signals::{Analysis, CallRecord, Signal, SignalKind, TurnBase};

/// How the turn ended, for the outcome ([`TurnStatus`](crate::TurnStatus) without its text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Completed,
    Cancelled,
    Failed,
}

/// A skill used in a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Used {
    pub name: String,
    /// The local index attached it (counted in the record's `attached`); a `skill_view` load is
    /// counted in `uses` by the view itself.
    pub attached: bool,
}

/// An earlier turn's outcome made worse.
#[derive(Clone, Debug, PartialEq)]
pub struct Worsen {
    pub turns: std::ops::RangeInclusive<u64>,
    pub to: Outcome,
}

/// What one finished turn changes in the skills' records.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub session_turn: u64,
    pub outcome: Outcome,
    pub uses: Vec<Used>,
    pub worsen: Vec<Worsen>,
}

/// The skills a turn used: the ones attached (`attached`) and the whole-skill `skill_view` calls
/// that went through, each once.
pub fn used_skills(attached: &[String], calls: &[CallRecord]) -> Vec<Used> {
    let mut used: Vec<Used> = Vec::new();
    for call in calls {
        if call.name != "skill_view" || call.failed || call.denied {
            continue;
        }
        let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.args) else {
            continue;
        };
        if args.get("file_path").is_some_and(|f| !f.is_null()) {
            continue;
        }
        if let Some(name) = args.get("name").and_then(|n| n.as_str())
            && !used.iter().any(|u| u.name == name)
        {
            used.push(Used {
                name: name.to_string(),
                attached: false,
            });
        }
    }
    for name in attached {
        if !used.iter().any(|u| &u.name == name) {
            used.push(Used {
                name: name.clone(),
                attached: true,
            });
        }
    }
    used
}

/// What a finished turn does to the records. Pure.
pub fn plan(
    base: &TurnBase,
    analysis: &Analysis,
    calls: &[CallRecord],
    attached: &[String],
    ended: Ended,
) -> Plan {
    let turn = base.turn as u64;
    let own = |s: &&Signal| s.turn as u64 == turn;
    let denied = analysis
        .signals
        .iter()
        .filter(own)
        .any(|s| matches!(s.kind, SignalKind::Denial { .. }));
    let workaround = analysis
        .signals
        .iter()
        .filter(own)
        .any(|s| matches!(s.kind, SignalKind::Workaround { .. }));
    let outcome = if denied {
        Outcome::Corrected
    } else if ended == Ended::Failed || workaround {
        Outcome::Errored
    } else {
        Outcome::Clean
    };
    // The user's reaction to the turn before: a correction in this turn's message.
    let mut worsen = Vec::new();
    let corrects_previous = analysis.signals.iter().filter(own).any(|s| match &s.kind {
        SignalKind::Correction { phrases } => !phrases
            .iter()
            .all(|p| p == crate::signals::SAVED_REMEMBER_PHRASE),
        _ => false,
    });
    if turn > 1 && corrects_previous {
        worsen.push(Worsen {
            turns: turn - 1..=turn - 1,
            to: Outcome::Corrected,
        });
    }
    Plan {
        session_turn: turn,
        outcome,
        uses: if ended == Ended::Cancelled {
            Vec::new()
        } else {
            used_skills(attached, calls)
        },
        worsen,
    }
}

/// The turns a rewind signal dropped.
pub fn rewound_turns(signal: &Signal) -> Option<std::ops::RangeInclusive<u64>> {
    match &signal.kind {
        SignalKind::Rewind { turns, .. } if signal.turn > 0 => {
            let first = signal.turn as u64;
            Some(first..=first + (*turns as u64).saturating_sub(1))
        }
        _ => None,
    }
}

/// Writes a turn's plan into the skill records (file work: call from a blocking thread).
pub fn apply(skills: &SkillStore, session: &str, plan: &Plan) {
    for worse in &plan.worsen {
        skills.worsen_outcomes(session, worse.turns.clone(), &worse.to);
    }
    for used in &plan.uses {
        skills.record_skill_use(
            &used.name,
            session,
            plan.session_turn,
            plan.outcome.clone(),
            used.attached,
        );
    }
}

/// A rewind makes the dropped turns' uses `undone` (file work).
pub fn apply_rewind(skills: &SkillStore, session: &str, signal: &Signal) {
    if let Some(turns) = rewound_turns(signal) {
        skills.worsen_outcomes(session, turns, &Outcome::Undone);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{CallRecord, Excerpt};

    fn base(turn: usize) -> TurnBase {
        TurnBase {
            turn,
            ..Default::default()
        }
    }

    fn signal(kind: SignalKind, turn: usize) -> Signal {
        Signal {
            seq: 0,
            kind,
            weight: 1.0,
            turn,
            message_id: None,
            at: 0,
            evidence: Vec::<Excerpt>::new(),
            key: None,
        }
    }

    fn analysis(signals: Vec<Signal>) -> Analysis {
        Analysis {
            signals,
            denial_reason: None,
        }
    }

    fn view(name: &str, file: Option<&str>, failed: bool) -> CallRecord {
        let args = match file {
            Some(f) => serde_json::json!({"name": name, "file_path": f}),
            None => serde_json::json!({"name": name}),
        };
        CallRecord::new("skill_view", &args, "ok", failed)
    }

    fn correction(turn: usize) -> Signal {
        signal(
            SignalKind::Correction {
                phrases: vec!["no".into()],
            },
            turn,
        )
    }

    #[test]
    fn uses_are_attached_skills_and_whole_skill_views_once_each() {
        let calls = [
            view("a", None, false),
            view("a", None, false),
            view("b", Some("references/x.md"), false),
            view("c", None, true),
            CallRecord::new("terminal", &serde_json::json!({"command": "ls"}), "", false),
        ];
        let used = used_skills(&["d".into(), "a".into()], &calls);
        assert_eq!(
            used,
            [
                Used {
                    name: "a".into(),
                    attached: false
                },
                Used {
                    name: "d".into(),
                    attached: true
                }
            ]
        );
    }

    #[test]
    fn a_turn_with_nothing_wrong_is_clean() {
        let p = plan(
            &base(3),
            &analysis(vec![]),
            &[],
            &["a".into()],
            Ended::Completed,
        );
        assert_eq!((p.outcome, p.session_turn), (Outcome::Clean, 3));
        assert!(p.worsen.is_empty());
        assert_eq!(p.uses.len(), 1);
    }

    #[test]
    fn a_denial_or_a_failed_turn_or_a_workaround_set_the_turns_own_outcome() {
        let denial = signal(
            SignalKind::Denial {
                what: "rm".into(),
                reason: None,
            },
            2,
        );
        let workaround = signal(
            SignalKind::Workaround {
                tool: "terminal".into(),
                failures: 2,
            },
            2,
        );
        let ended = |signals, ended| plan(&base(2), &analysis(signals), &[], &["a".into()], ended);
        assert_eq!(
            ended(vec![denial.clone()], Ended::Completed).outcome,
            Outcome::Corrected
        );
        assert_eq!(ended(vec![], Ended::Failed).outcome, Outcome::Errored);
        assert_eq!(
            ended(vec![workaround], Ended::Completed).outcome,
            Outcome::Errored
        );
        // A denial wins over a failure; another turn's signal isn't this turn's.
        assert_eq!(
            ended(vec![denial], Ended::Failed).outcome,
            Outcome::Corrected
        );
        assert_eq!(
            ended(
                vec![signal(
                    SignalKind::Workaround {
                        tool: "t".into(),
                        failures: 2
                    },
                    9
                )],
                Ended::Completed
            )
            .outcome,
            Outcome::Clean
        );
    }

    #[test]
    fn a_cancelled_turn_is_not_judged() {
        let p = plan(
            &base(2),
            &analysis(vec![]),
            &[],
            &["a".into()],
            Ended::Cancelled,
        );
        assert!(p.uses.is_empty());
    }

    #[test]
    fn a_correction_marks_the_turn_before_it_not_itself_and_not_the_first() {
        let p = plan(
            &base(4),
            &analysis(vec![correction(4)]),
            &[],
            &["a".into()],
            Ended::Completed,
        );
        assert_eq!(
            p.worsen,
            [Worsen {
                turns: 3..=3,
                to: Outcome::Corrected
            }]
        );
        // This turn's own use is judged by this turn.
        assert_eq!(p.outcome, Outcome::Clean);
        let first = plan(
            &base(1),
            &analysis(vec![correction(1)]),
            &[],
            &[],
            Ended::Completed,
        );
        assert!(first.worsen.is_empty());
    }

    #[test]
    fn a_rewind_covers_the_dropped_turns() {
        let rewind = signal(
            SignalKind::Rewind {
                via: "undo".into(),
                turns: 3,
            },
            5,
        );
        assert_eq!(rewound_turns(&rewind), Some(5..=7));
        assert_eq!(rewound_turns(&correction(2)), None);
    }
}
