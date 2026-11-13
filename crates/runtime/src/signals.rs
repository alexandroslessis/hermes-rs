//! Learning signals: what in a session suggests there is something worth learning from it (a
//! correction, an undone turn, a denied command, a workaround the agent found, a long procedure,
//! an explicit "remember this"). Phase 7 of `docs/backend/PHASES.md` reviews a session only when
//! its signals add up, instead of Hermes's fixed every-N-turns nudges; this module is the
//! detection half (CP7.1). The review that reads them (the evidence packet, the one-shot request,
//! when it runs) is CP7.2/7.3.
//!
//! Not from Hermes (backend behaviour is ours, AGENTS.md rule 1); `PORTING.md` marks it
//! `diverged`. Hermes's `agent/background_review.py` has no signals: it replays the whole
//! conversation every 10 turns or 10 model calls.
//!
//! ## How it works
//!
//! - **Zero requests.** Everything is computed locally from what the turn already has: the text
//!   the user typed, the `/steer` messages delivered during it, and a record of every tool call
//!   the turn's `GuardedTools` ran (`TurnState::calls`, so a compaction mid-turn loses nothing).
//!   [`analyze`] is a pure function; `Runtime::begin_turn` calls it when the turn ends.
//! - **Only main turns.** Background jobs (the review, the curator, subagent children, `/bg`,
//!   `/btw`) never go through `begin_turn`, so they never add signals. The assistant's own text and
//!   tool output are never read for phrases; only what the user wrote.
//! - **Never the system prompt.** Nothing here reaches the model.
//! - **Persistence**: `data_dir/signals/<session id>.json`, one small file per session (the store
//!   schema is frozen, ground rule 6). One plain OS thread ([`SignalStore`]) applies every change
//!   in order (read, change, write the file whole through a temp file), so no async worker ever
//!   touches the disk (ground rule 7): callers only send it a message. Each change re-reads the
//!   file, so two processes on one data folder (the app and `hermes-acp`) lose at most a change
//!   made in the same instant (last writer wins, as for the other side files). A panic in one
//!   change is caught (the thread goes on); a file a newer version wrote is never rewritten.
//! - **Private**: excerpts go through `hermes_tools::redact` (vendor keys, auth headers, private
//!   keys, JWTs, credentials in URLs are masked) before they are kept: they live on disk and will
//!   be sent to the user's own review model (CP7.2), never anywhere else.
//! - **Bounded**: at most [`MAX_SIGNALS`] signals per session, each with at most
//!   [`MAX_EVIDENCE`] excerpts of at most [`MAX_EXCERPT`] characters (tool output
//!   [`MAX_TOOL_EXCERPT`]); a signal pushed out by the cap still counts toward the score
//!   (`dropped_weight`) until the next review.
//!
//! ## Weights and the threshold
//!
//! The score is the sum of the weights of the signals not reviewed yet. A review is due at
//! [`REVIEW_THRESHOLD`] (3.0), or at once after an explicit "remember this" or `/learn`. The
//! weights are set so that:
//! - one ordinary correction never crosses it: a whole message is capped at [`MESSAGE_CAP`]
//!   (2.5), and a lone weak phrase ("instead" in the middle of a message) weighs 0.75;
//! - two corrections cross it ("No, that's wrong" 2.25 + "No, use pnpm instead" 1.75);
//! - one strong signal crosses it alone: an undone, retried or rewound turn ([`REWIND_WEIGHT`]),
//!   a very long successful turn, an explicit request ([`EXPLICIT_WEIGHT`]).
//!
//! ## What counts as a correction (and what doesn't)
//!
//! The user's text is cleaned first: fenced and inline code, quoted lines (`>`), indented code,
//! lines that look like logs, stack traces or code, quoted speech (`"..."`, `'...'`), URLs and
//! paths, the speakers' lines of a pasted chat ("Alice: …", "User: …", two or more), and
//! everything from a pasted e-mail's headers (`From:`, `On ... wrote:`) on are removed; only the
//! first [`MAX_READ`] characters are read at all, and a long paste keeps only its first
//! [`HEAD_KEEP`] cleaned characters (the user's own words usually come first). A message that isn't English (non-Latin script, or no common English word) gives no
//! textual signal. Commands' generated prompts (`/plan`, `/init`, skills, `/review`) are not
//! read; `/learn` is an explicit signal.
//!
//! Phrases then count by context: imperatives ("don't X", "always X", "never X") only at the
//! start of a sentence or clause and, for always/never, only before a verb from a list (so
//! "always happy to help", "never mind", "it never terminates" don't count); "don't worry /
//! mind / know" never count; questions count only for "why did you delete/change/…" (not "why
//! did you choose X?", which is curiosity) and (half) "instead"; "instead" in the user's own
//! narration ("I went with sqlite instead") doesn't count; "wrong" as a topic ("handle the case
//! when the key is wrong", "a test for the wrong path") doesn't; "no," counts only at the very
//! start and not before "problem", "worries", "that's fine", nor before a status report ("No,
//! still failing", "No luck": a fix that didn't work yet is no lesson); "remember that …" is
//! explicit only when a fact follows (a verb), not "remember that function we wrote". Context
//! scales the weight: a session's first message has nothing to correct (rejections don't count,
//! rules count three quarters), and an answer to the assistant's question ("No." to "Shall I
//! also…?") is a choice, not a correction ("no" doesn't count, the rest counts half).
//!
//! ## Rewind, branch, resume
//!
//! Evidence is captured text, not pointers, so it survives what happens to the conversation
//! later. Undoing, retrying (the page's `/retry` is a rewind and resend), editing and resending
//! a message, or restoring a checkpoint adds a [`SignalKind::Rewind`] and never removes earlier
//! signals: what the user said before is still what they said, even if the turn it was in is
//! gone. Hermes's own `/retry` after a provider error (`Runtime::retry_turn`) is not a signal:
//! the provider failed, not the agent. A branch (`/branch`) starts with no signals (the parent
//! keeps them; copying would review the same evidence twice). Deleting a session deletes its
//! file. A resumed or reopened session reads its file again. The same words sent again to the
//! same answer (the undone message resent, as `/undo` invites) count once while unreviewed
//! ([`Signal::key`]); the rewind itself is the new signal.
//!
//! There is no decay: the score only grows until a review covers it. Weak phrases from ordinary
//! chat can still add up over a long session (four narrated "instead"s that slipped through
//! would make 3.0); the review then costs one request and returns no ops (CP7.2's prompt), and
//! CP7.3 runs it at most once per idle period.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, mpsc};

use hermes_llm::conversation::message::{Message, MessageContent};
use regex::Regex;
use rmcp::model::Role;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- knobs ----------------------------------------------------------------------------------

/// The score at which a session is worth a review (CP7.3 reads it; a placeholder for a
/// `review.threshold` setting).
pub const REVIEW_THRESHOLD: f32 = 3.0;
/// What one user message's phrases can add at most (below the threshold: a single message is
/// never enough unless it asks explicitly).
pub const MESSAGE_CAP: f32 = 2.5;
/// An explicit "remember this" or `/learn`: always enough on its own.
pub const EXPLICIT_WEIGHT: f32 = 10.0;
/// An undone, retried or rewound turn: a strong signal, enough on its own.
pub const REWIND_WEIGHT: f32 = 3.0;
/// A command or edit the user denied.
pub const DENIAL_WEIGHT: f32 = 2.0;
/// A tool that failed twice, then worked with different arguments.
pub const WORKAROUND_WEIGHT: f32 = 1.5;
/// Tool calls that make a turn long (a procedure that may be worth a skill).
pub const LONG_TURN_CALLS: usize = 15;
pub const LONG_TURN_WEIGHT: f32 = 2.0;
/// Tool calls that make a turn very long: enough on its own.
pub const VERY_LONG_TURN_CALLS: usize = 30;
pub const VERY_LONG_TURN_WEIGHT: f32 = 3.0;

/// Signals kept per session.
pub const MAX_SIGNALS: usize = 32;
/// Excerpts kept per signal.
pub const MAX_EVIDENCE: usize = 6;
/// Characters per message excerpt.
pub const MAX_EXCERPT: usize = 400;
/// Characters per tool output or argument excerpt.
pub const MAX_TOOL_EXCERPT: usize = 300;
/// A cleaned message longer than this is a paste: only its first [`HEAD_KEEP`] characters are
/// read.
pub const LONG_PASTE: usize = 1200;
pub const HEAD_KEEP: usize = 600;
/// Characters of a message read at all (the cleaning's cost stays bounded on a huge paste).
pub const MAX_READ: usize = 16 * 1024;
/// Tool calls a turn records in full (more are only counted).
pub const MAX_CALL_RECORDS: usize = 500;
/// Characters of a call's arguments kept to compare calls.
const MAX_ARGS_KEPT: usize = 2000;
const FILE_VERSION: u32 = 1;

// ---- types ----------------------------------------------------------------------------------

/// What happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalKind {
    /// The user corrected the agent or stated a rule; `phrases` are what matched.
    Correction { phrases: Vec<String> },
    /// "Remember this…" (`via: "remember"`) or `/learn` (`via: "learn"`): always triggers.
    Explicit { via: String },
    /// Turns were undone, retried, edited or restored away (`via`: `rewind`, `undo`,
    /// `rollback`).
    Rewind { via: String, turns: usize },
    /// A command or edit the user denied; `reason` is what they wrote next, if anything.
    Denial {
        what: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// `tool` failed `failures` times, then worked with different arguments.
    Workaround { tool: String, failures: usize },
    /// A turn of `calls` tool calls that ended with an answer; `tools` sums them up.
    LongTurn { calls: usize, tools: String },
}

/// A piece of the conversation kept as evidence (captured text, so it survives rewinds and
/// compaction).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Excerpt {
    /// `user`, `assistant` or `tool`.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub text: String,
}

impl Excerpt {
    fn user(text: &str) -> Self {
        Self {
            role: "user".into(),
            tool: None,
            text: excerpt(text, MAX_EXCERPT),
        }
    }

    fn assistant(text: &str) -> Self {
        Self {
            role: "assistant".into(),
            tool: None,
            text: excerpt(text, MAX_EXCERPT),
        }
    }

    fn tool(name: &str, text: &str) -> Self {
        Self {
            role: "tool".into(),
            tool: Some(name.to_string()),
            text: excerpt(text, MAX_TOOL_EXCERPT),
        }
    }
}

/// One signal, with its evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Order in the session (set when stored).
    #[serde(default)]
    pub seq: u64,
    #[serde(flatten)]
    pub kind: SignalKind,
    pub weight: f32,
    /// The user turn it came from (1 = the first message; informational, turns are renumbered
    /// by rewinds).
    #[serde(default)]
    pub turn: usize,
    /// The id of the user message it came from, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Unix milliseconds (set when stored).
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub evidence: Vec<Excerpt>,
    /// For a signal read from the user's words: a hash of those words (normalised), so the same
    /// message sent again (resent after `/undo`, the page's retry, a paste repeated) isn't
    /// counted twice while unreviewed. Not the text itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<u64>,
}

impl Signal {
    fn new(kind: SignalKind, weight: f32, turn: usize, message_id: Option<String>) -> Self {
        Self {
            seq: 0,
            kind,
            weight,
            turn,
            message_id,
            at: 0,
            evidence: Vec::new(),
            key: None,
        }
    }

    /// Keys it with the words it came from and what they answered.
    fn keyed(mut self, answered: &str, text: &str) -> Self {
        self.key = Some(text_key(answered, text));
        self
    }

    fn with_evidence(mut self, evidence: impl IntoIterator<Item = Excerpt>) -> Self {
        self.evidence = evidence
            .into_iter()
            .filter(|e| !e.text.is_empty())
            .take(MAX_EVIDENCE)
            .collect();
        self
    }

    /// Whether it always triggers a review.
    pub fn is_explicit(&self) -> bool {
        matches!(self.kind, SignalKind::Explicit { .. })
    }
}

/// A session's signals: what `signals/<id>.json` holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSignals {
    pub v: u32,
    pub next_seq: u64,
    /// Signals up to this `seq` were covered by a review (CP7.2/7.3 sets it).
    pub reviewed_seq: u64,
    /// Unreviewed signals pushed out by the cap, and their weight (still in the score).
    pub dropped: u64,
    pub dropped_weight: f32,
    pub signals: Vec<Signal>,
    /// Scheduled reviews started since the signals were last marked reviewed, and when the last
    /// one started (unix ms). On disk so that a review cut off by an exit (or a crash) or failing
    /// after its request isn't sent again by every later run of the app (CP7.3,
    /// [`Self::retry_wait`]).
    #[serde(skip_serializing_if = "is_zero")]
    pub review_tries: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub review_started_ms: i64,
}

fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// The first wait after two reviews in a row that didn't cover the signals, doubling per further
/// one up to [`RETRY_MAX`].
pub const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(3600);
pub const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

impl SessionSignals {
    /// How long a scheduled review must still wait because earlier ones started and never covered
    /// the signals (cut off by an exit, or failed after a request), in this run of the app or an
    /// earlier one. The first such review is redone freely (an app closed mid-review is the usual
    /// case); from the second on the wait is [`RETRY_FIRST`], doubling, at most [`RETRY_MAX`]. A
    /// clock set back counts as time passed.
    pub fn retry_wait(&self, now_ms: i64) -> Option<std::time::Duration> {
        if self.review_tries < 2 {
            return None;
        }
        let wait = RETRY_FIRST
            .saturating_mul(1 << (self.review_tries - 2).min(10))
            .min(RETRY_MAX);
        let elapsed = now_ms.saturating_sub(self.review_started_ms);
        if elapsed < 0 {
            return None;
        }
        wait.checked_sub(std::time::Duration::from_millis(elapsed as u64))
            .filter(|left| !left.is_zero())
    }

    /// Records that a review of the signals started (see [`Self::retry_wait`]).
    pub fn review_started(&mut self, now_ms: i64) {
        self.review_tries = self.review_tries.saturating_add(1);
        self.review_started_ms = now_ms;
    }

    /// The signals not reviewed yet, oldest first: the evidence for the next review.
    pub fn evidence(&self) -> Vec<&Signal> {
        self.signals
            .iter()
            .filter(|s| s.seq > self.reviewed_seq)
            .collect()
    }

    /// The weight of everything not reviewed yet.
    pub fn score(&self) -> f32 {
        self.evidence().iter().map(|s| s.weight).sum::<f32>() + self.dropped_weight
    }

    /// Whether a review is due: an explicit request, or the score at the threshold.
    pub fn triggered(&self) -> bool {
        self.triggered_at(REVIEW_THRESHOLD)
    }

    /// [`Self::triggered`] at another threshold (the `review_threshold` budget knob).
    pub fn triggered_at(&self, threshold: f32) -> bool {
        self.evidence().iter().any(|s| s.is_explicit()) || self.score() >= threshold
    }

    /// The newest signal's `seq` when any is unreviewed: what a review that starts now covers
    /// (signals that arrive while it runs get higher numbers and stay unreviewed).
    pub fn unreviewed_upto(&self) -> Option<u64> {
        // `next_seq` is the last number given out; pushed-out signals count too.
        (!self.evidence().is_empty() || self.dropped > 0).then_some(self.next_seq)
    }

    /// Records that a review covered every signal up to `seq`. They stay in the file (Phase 8
    /// derives skill outcomes from them) until the cap pushes them out, reviewed ones first.
    pub fn mark_reviewed(&mut self, seq: u64) {
        self.reviewed_seq = self.reviewed_seq.max(seq.min(self.next_seq));
        self.dropped = 0;
        self.dropped_weight = 0.0;
        self.review_tries = 0;
    }

    /// Adds a signal (numbered and dated here) and keeps the file within its cap. A signal from
    /// words already counted and not reviewed yet (the same message sent again) is skipped.
    pub fn add(&mut self, mut signal: Signal, now_ms: i64) {
        if signal.key.is_some()
            && self
                .signals
                .iter()
                .any(|s| s.seq > self.reviewed_seq && s.key == signal.key)
        {
            return;
        }
        self.next_seq += 1;
        signal.seq = self.next_seq;
        if signal.at == 0 {
            signal.at = now_ms;
        }
        signal.evidence.truncate(MAX_EVIDENCE);
        self.signals.push(signal);
        self.enforce_cap();
    }

    /// Attaches what the user wrote after a denial to the latest denial still without one.
    pub fn attach_denial_reason(&mut self, text: &str) {
        let reviewed = self.reviewed_seq;
        if let Some(signal) = self.signals.iter_mut().rev().find(|s| {
            s.seq > reviewed && matches!(&s.kind, SignalKind::Denial { reason: None, .. })
        }) {
            let reason = excerpt(text, MAX_EXCERPT);
            if let SignalKind::Denial { reason: slot, .. } = &mut signal.kind {
                *slot = Some(reason.clone());
            }
            if signal.evidence.len() < MAX_EVIDENCE {
                signal.evidence.push(Excerpt::user(&reason));
            }
        }
    }

    /// Drops signals over [`MAX_SIGNALS`]: reviewed ones first (oldest first), then the
    /// lightest unreviewed one that isn't explicit (oldest first), else the oldest.
    fn enforce_cap(&mut self) {
        while self.signals.len() > MAX_SIGNALS {
            let reviewed = self.reviewed_seq;
            let victim = self
                .signals
                .iter()
                .position(|s| s.seq <= reviewed)
                .or_else(|| {
                    self.signals
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| !s.is_explicit())
                        .min_by(|(i, a), (j, b)| a.weight.total_cmp(&b.weight).then(i.cmp(j)))
                        .map(|(i, _)| i)
                })
                .unwrap_or(0);
            let removed = self.signals.remove(victim);
            if removed.seq > reviewed {
                self.dropped += 1;
                self.dropped_weight += removed.weight;
            }
        }
    }
}

// ---- what a turn hands over ----------------------------------------------------------------

/// One tool call of a turn, as `GuardedTools` saw it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallRecord {
    pub name: String,
    /// The arguments as JSON (cut at [`MAX_ARGS_KEPT`] characters).
    pub args: String,
    pub failed: bool,
    /// The user denied it (a denial is not a failure of the tool).
    pub denied: bool,
    /// The start of its output (secrets not masked yet: that happens when it becomes evidence).
    pub output: String,
}

impl CallRecord {
    pub fn new(name: &str, args: &Value, output: &str, failed: bool) -> Self {
        let denied = is_user_denial(output);
        Self {
            name: name.to_string(),
            args: excerpt_plain(&args.to_string(), MAX_ARGS_KEPT),
            failed: failed && !denied,
            denied,
            // Cut only: secrets are masked when it becomes evidence (`excerpt`), and the slack
            // keeps a key at the cut whole enough to be recognised then.
            output: excerpt_plain(output.trim(), MAX_TOOL_EXCERPT + 512),
        }
    }
}

/// Appends a call to a turn's record: every call is counted, the first [`MAX_CALL_RECORDS`]
/// are kept.
pub fn push_call(calls: &mut Vec<CallRecord>, count: &mut usize, record: CallRecord) {
    *count += 1;
    if calls.len() < MAX_CALL_RECORDS {
        calls.push(record);
    }
}

/// Whether a tool result is the user's "Deny" (`hermes_tools::approval::denied_message`), not a
/// timeout or the approval judge.
pub fn is_user_denial(output: &str) -> bool {
    output.contains("BLOCKED: User denied this command.")
}

/// What is known about a turn when it starts, read from the conversation before the user's
/// message is added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnBase {
    /// The user turn's number (1 = the first message).
    pub turn: usize,
    /// What the user typed (`None` for Hermes's `/retry` after an error).
    pub typed: Option<String>,
    /// The user's previous message and the assistant's last answer (context for evidence).
    pub prior_user: Option<String>,
    pub prior_assistant: Option<String>,
    /// The previous turn ended with a command the user denied.
    pub previous_turn_denied: bool,
}

impl TurnBase {
    pub fn before(messages: &[Message], typed: Option<&str>) -> Self {
        let users: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| is_typed_user(m))
            .map(|(i, _)| i)
            .collect();
        let last_user = users.last().copied();
        let prior_assistant = messages
            .iter()
            .rev()
            .filter(|m| m.role == Role::Assistant && m.is_agent_visible())
            .map(Message::as_concat_text)
            .find(|text| !text.trim().is_empty());
        let previous_turn_denied = messages[last_user.map_or(0, |i| i + 1)..]
            .iter()
            .flat_map(|m| &m.content)
            .filter(|c| c.as_tool_response().is_some())
            .any(|c| is_user_denial(&response_text(c)));
        Self {
            turn: users.len() + usize::from(typed.is_some()),
            typed: typed.map(str::to_string),
            prior_user: last_user.map(|i| typed_text(&messages[i])),
            prior_assistant,
            previous_turn_denied,
        }
    }
}

/// What the turn did, gathered when it ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnOutcome {
    /// The id of the user's message.
    pub message_id: Option<String>,
    /// `/steer` messages delivered during the turn (the user's words too).
    pub steers: Vec<String>,
    pub calls: Vec<CallRecord>,
    /// Every call, including any beyond [`MAX_CALL_RECORDS`].
    pub call_count: usize,
    /// The turn completed (not cancelled, not failed).
    pub completed: bool,
    /// The answer it ended with, when it ended with one.
    pub answer: Option<String>,
}

/// What one turn adds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    pub signals: Vec<Signal>,
    /// What the user wrote after a denial in the previous turn (attached to that denial).
    pub denial_reason: Option<String>,
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty() && self.denial_reason.is_none()
    }
}

// ---- analysis -------------------------------------------------------------------------------

/// The signals one turn gives. Pure: no I/O, no model.
pub fn analyze(base: &TurnBase, outcome: &TurnOutcome) -> Analysis {
    let mut signals = Vec::new();
    let turn = base.turn;
    let id = outcome.message_id.clone();
    let typed = base
        .typed
        .as_deref()
        .filter(|t| !t.trim().is_empty() && !is_generated_prompt(t));
    let before = || {
        let mut excerpts = Vec::new();
        if let Some(text) = &base.prior_user {
            excerpts.push(Excerpt::user(text));
        }
        if let Some(text) = &base.prior_assistant {
            excerpts.push(Excerpt::assistant(text));
        }
        excerpts
    };
    let after: Vec<Excerpt> = outcome
        .answer
        .iter()
        .map(|a| Excerpt::assistant(a))
        .collect();

    // `/learn`: the agent writes a skill now; the review checks what came of it.
    if let Some(request) = base
        .typed
        .as_deref()
        .and_then(hermes_prompts::review::learn_request)
    {
        let asked = if request.is_empty() {
            "/learn (from this conversation)".to_string()
        } else {
            format!("/learn {request}")
        };
        signals.push(
            Signal::new(
                SignalKind::Explicit {
                    via: "learn".into(),
                },
                EXPLICIT_WEIGHT,
                turn,
                id.clone(),
            )
            .with_evidence(
                before()
                    .into_iter()
                    .chain([Excerpt::user(&asked)])
                    .chain(after.clone()),
            ),
        );
    }

    let context = TextContext {
        first: base.prior_assistant.is_none(),
        after_question: base
            .prior_assistant
            .as_deref()
            .is_some_and(ends_with_question),
    };
    if let Some(text) = typed
        && let Some(signal) = text_signal(text, context, turn, id.clone())
    {
        // "Remember this" that the agent already saved with its `memory` tool in the same turn
        // is done: it stays a weak signal (the review may still check the wording) instead of
        // forcing a review that would only find the entry there.
        let signal = if signal.is_explicit() && saved_to_memory(&outcome.calls) {
            Signal::new(
                SignalKind::Correction {
                    phrases: vec![SAVED_REMEMBER_PHRASE.into()],
                },
                SAVED_REMEMBER_WEIGHT,
                turn,
                id.clone(),
            )
        } else {
            signal
        };
        signals.push(
            // Keyed with what it answered: the same words sent again to the same answer (after
            // `/undo` or a retry) are one correction, the same words to a later answer another.
            signal
                .keyed(base.prior_assistant.as_deref().unwrap_or(""), text)
                .with_evidence(
                    before()
                        .into_iter()
                        .chain([Excerpt::user(text)])
                        .chain(after.clone()),
                ),
        );
    }
    // A steer interrupts work in progress: never a first message, never an answer.
    let steer_context = TextContext {
        first: false,
        after_question: false,
    };
    for steer in &outcome.steers {
        if let Some(signal) = text_signal(steer, steer_context, turn, None) {
            let asked = typed.map(Excerpt::user);
            signals.push(
                signal.keyed(typed.unwrap_or(""), steer).with_evidence(
                    asked
                        .into_iter()
                        .chain([Excerpt::user(steer)])
                        .chain(after.clone()),
                ),
            );
        }
    }

    let denied: Vec<&CallRecord> = outcome.calls.iter().filter(|c| c.denied).collect();
    if !denied.is_empty() {
        let what = denied
            .iter()
            .take(3)
            .map(|c| describe_call(&c.name, &c.args))
            .collect::<Vec<_>>()
            .join("; ");
        signals.push(
            Signal::new(
                SignalKind::Denial { what, reason: None },
                DENIAL_WEIGHT,
                turn,
                id.clone(),
            )
            .with_evidence(
                typed.map(Excerpt::user).into_iter().chain(
                    denied
                        .iter()
                        .take(3)
                        .map(|c| Excerpt::tool(&c.name, &describe_call(&c.name, &c.args))),
                ),
            ),
        );
    }

    if let Some((tool, failures, worked)) = workaround(&outcome.calls) {
        let failed: Vec<Excerpt> = failures
            .iter()
            .rev()
            .take(2)
            .rev()
            .map(|c| {
                Excerpt::tool(
                    &c.name,
                    &format!("FAILED {} -> {}", describe_call(&c.name, &c.args), c.output),
                )
            })
            .collect();
        signals.push(
            Signal::new(
                SignalKind::Workaround {
                    tool: tool.to_string(),
                    failures: failures.len(),
                },
                WORKAROUND_WEIGHT,
                turn,
                id.clone(),
            )
            .with_evidence(typed.map(Excerpt::user).into_iter().chain(failed).chain([
                Excerpt::tool(
                    &worked.name,
                    &format!(
                        "WORKED {} -> {}",
                        describe_call(&worked.name, &worked.args),
                        worked.output
                    ),
                ),
            ])),
        );
    }

    if outcome.completed && outcome.answer.is_some() && outcome.call_count >= LONG_TURN_CALLS {
        let weight = if outcome.call_count >= VERY_LONG_TURN_CALLS {
            VERY_LONG_TURN_WEIGHT
        } else {
            LONG_TURN_WEIGHT
        };
        let tools = tool_summary(&outcome.calls);
        let steps: Vec<String> = outcome
            .calls
            .iter()
            .take(20)
            .map(|c| describe_call(&c.name, &c.args))
            .collect();
        signals.push(
            Signal::new(
                SignalKind::LongTurn {
                    calls: outcome.call_count,
                    tools: tools.clone(),
                },
                weight,
                turn,
                id.clone(),
            )
            .with_evidence(
                typed
                    .map(Excerpt::user)
                    .into_iter()
                    .chain([Excerpt {
                        role: "tool".into(),
                        tool: None,
                        text: excerpt(&steps.join("\n"), MAX_EXCERPT * 2),
                    }])
                    .chain(after.clone()),
            ),
        );
    }

    Analysis {
        signals,
        denial_reason: typed
            .filter(|_| base.previous_turn_denied)
            .map(|t| excerpt(t, MAX_EXCERPT)),
    }
}

/// The signal a rewind gives: the turns from message `keep` on are about to be dropped. `None`
/// when nothing the agent did is among them (a failed message, or only command reports).
pub fn rewind_signal(
    messages: &[Message],
    keep: usize,
    via: &str,
    is_command_report: impl Fn(&Message) -> bool,
) -> Option<Signal> {
    let dropped = messages.get(keep..).filter(|d| !d.is_empty())?;
    let reply = dropped.iter().any(|m| {
        m.role == Role::Assistant
            && !is_command_report(m)
            && m.content.iter().any(|c| match c {
                MessageContent::Text(text) => !text.text.trim().is_empty(),
                other => other.as_tool_request().is_some(),
            })
    });
    if !reply {
        return None;
    }
    let first_user = dropped.iter().find(|m| is_typed_user(m));
    let turns = dropped.iter().filter(|m| is_typed_user(m)).count().max(1);
    let last_answer = dropped
        .iter()
        .rev()
        .filter(|m| m.role == Role::Assistant && !is_command_report(m))
        .map(Message::as_concat_text)
        .find(|t| !t.trim().is_empty());
    let base = TurnBase::before(&messages[..keep], None);
    let evidence = base
        .prior_assistant
        .as_deref()
        .map(Excerpt::assistant)
        .into_iter()
        .chain(first_user.map(|m| Excerpt::user(&typed_text(m))))
        .chain(last_answer.as_deref().map(Excerpt::assistant));
    Some(
        Signal::new(
            SignalKind::Rewind {
                via: via.to_string(),
                turns,
            },
            REWIND_WEIGHT,
            base.turn + 1,
            first_user.and_then(|m| m.id.clone()),
        )
        .with_evidence(evidence),
    )
}

/// The answer a turn ended with: its last message, when the model wrote text there.
pub fn final_answer(messages: &[Message]) -> Option<String> {
    let last = messages.last()?;
    (last.role == Role::Assistant && last.is_agent_visible() && last.error_kind().is_none())
        .then(|| last.as_concat_text())
        .filter(|text| !text.trim().is_empty())
}

/// The phrase of a "remember this" the agent already saved (a weak `Correction` that corrects
/// nothing: `skill_outcomes` ignores it).
pub const SAVED_REMEMBER_PHRASE: &str = "remember (saved by the agent)";

/// The weight of a "remember this" the agent saved itself in the same turn.
const SAVED_REMEMBER_WEIGHT: f32 = 1.0;

/// Whether the turn wrote to memory: a `memory` call that adds or patches and went through (an
/// entry staged for approval counts: the lesson already waits for the user).
fn saved_to_memory(calls: &[CallRecord]) -> bool {
    calls.iter().any(|c| {
        c.name == "memory"
            && !c.failed
            && !c.denied
            && (c.args.contains("\"add\"") || c.args.contains("\"patch\""))
    })
}

fn text_signal(
    text: &str,
    context: TextContext,
    turn: usize,
    id: Option<String>,
) -> Option<Signal> {
    let score = score_text(text, context);
    if score.explicit {
        return Some(Signal::new(
            SignalKind::Explicit {
                via: "remember".into(),
            },
            EXPLICIT_WEIGHT,
            turn,
            id,
        ));
    }
    (score.weight > 0.0).then(|| {
        Signal::new(
            SignalKind::Correction {
                phrases: score.phrases,
            },
            score.weight,
            turn,
            id,
        )
    })
}

/// Tools whose failures are part of normal work (a guessed path, a patch whose context moved,
/// a search with no hits), so failing and then working is no lesson.
const NOT_WORKAROUNDS: &[&str] = &[
    "clarify",
    "delegate_task",
    "memory",
    "patch",
    "read_file",
    "search_files",
    "session_search",
    "skill_manage",
    "skill_view",
    "skills_list",
    "todo_list",
    "vision_analyze",
    "web_extract",
    "web_search",
    "write_file",
];

/// Shell programs that "fail" as an answer (no match, no such file, not installed): probing with
/// them, failing and then finding something is exploration, not a workaround.
const PROBES: &[&str] = &[
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "findstr",
    "select-string",
    "sls",
    "find",
    "fd",
    "where",
    "which",
    "whereis",
    "type",
    "command",
    "test",
    "[",
    "[[",
    "ls",
    "dir",
    "gci",
    "get-childitem",
    "test-path",
    "get-command",
    "gcm",
    "cat",
    "gc",
    "get-content",
    "head",
    "tail",
    "stat",
    "file",
    "diff",
    "cmp",
    "fc",
    "echo",
    "printf",
    "true",
    "false",
    "exit",
    "tree",
];
/// `git` subcommands that only look.
const GIT_PROBES: &[&str] = &[
    "grep",
    "rev-parse",
    "show",
    "log",
    "ls-files",
    "cat-file",
    "diff",
    "status",
    "branch",
    "describe",
    "remote",
    "config",
    "blame",
    "show-ref",
    "ls-remote",
];

/// A terminal command's words, lower case, from its main step: the first `&&` / `;` / `|`
/// segment that isn't a `cd`, without leading `VAR=value`s; the program without its folder and
/// `.exe`. Empty when it isn't a terminal call.
fn command_words(call: &CallRecord) -> Vec<String> {
    if call.name != "terminal" {
        return Vec::new();
    }
    let command = serde_json::from_str::<Value>(&call.args)
        .ok()
        .and_then(|v| {
            v.get("command")
                .and_then(Value::as_str)
                .map(str::to_lowercase)
        })
        .unwrap_or_default();
    let words = |segment: &str| -> Vec<String> {
        let mut words: Vec<String> = segment
            .split_whitespace()
            .map(|w| w.trim_matches(['"', '\'', '(', ')']).to_string())
            .skip_while(|w| w.contains('=') && !w.starts_with('-'))
            .filter(|w| !w.is_empty())
            .collect();
        if let Some(program) = words.first_mut() {
            let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
            *program = base.strip_suffix(".exe").unwrap_or(base).to_string();
        }
        words
    };
    command
        .split(['&', ';', '|'])
        .map(words)
        .find(|w| {
            w.first().is_some_and(|p| {
                !matches!(
                    p.as_str(),
                    "cd" | "pushd" | "set-location" | "sl" | "chdir" | "export" | "set"
                )
            })
        })
        .unwrap_or_default()
}

/// A terminal call whose failure is an answer, not a problem ([`PROBES`]).
fn is_probe(words: &[String]) -> bool {
    match words.first().map(String::as_str) {
        Some("git") => words
            .iter()
            .skip(1)
            .find(|w| !w.starts_with('-'))
            .is_some_and(|sub| GIT_PROBES.contains(&sub.as_str())),
        Some(program) => PROBES.contains(&program),
        None => false,
    }
}

/// For the terminal: the success is another try at what failed, not the next step of the work.
/// It shares a word other than the program with a failure ("npm test" → "pnpm test", "pip
/// install x" → "pip install --user x"; not "cargo test" → "cargo clippy" or "git diff"), and the
/// failures were different attempts or the program changed (the same `cargo build` failing
/// while the agent fixes the code, then `cargo build --release`, is not a workaround).
fn retries_what_failed(worked: &CallRecord, failures: &[&CallRecord]) -> bool {
    if worked.name != "terminal" {
        return true;
    }
    let worked_words = command_words(worked);
    let words = |w: &[String]| -> Vec<String> {
        w.iter()
            .skip(1)
            .filter(|w| w.len() >= 2 && !w.starts_with('-'))
            .cloned()
            .collect()
    };
    let shared = failures.iter().any(|f| {
        let failed = words(&command_words(f));
        words(&worked_words).iter().any(|w| failed.contains(w))
    });
    let attempts_differ = failures.iter().any(|f| f.args != failures[0].args)
        || failures
            .iter()
            .any(|f| command_words(f).first() != worked_words.first());
    shared && attempts_differ
}

/// The first tool that failed at least twice in a row (other tools may run in between) and then
/// worked with arguments unlike every failed call's (for the terminal: another try at the same
/// thing, [`retries_what_failed`]; probing commands such as `rg` or `test -f` don't count).
fn workaround(calls: &[CallRecord]) -> Option<(&str, Vec<&CallRecord>, &CallRecord)> {
    let mut streaks: Vec<(&str, Vec<&CallRecord>)> = Vec::new();
    for call in calls {
        if call.denied
            || NOT_WORKAROUNDS.contains(&call.name.as_str())
            || is_probe(&command_words(call))
        {
            continue;
        }
        let at = streaks.iter().position(|(name, _)| *name == call.name);
        if call.failed {
            match at {
                Some(i) => streaks[i].1.push(call),
                None => streaks.push((&call.name, vec![call])),
            }
            continue;
        }
        if let Some(i) = at {
            let (name, failures) = streaks.remove(i);
            if failures.len() >= 2
                && failures.iter().all(|f| f.args != call.args)
                && retries_what_failed(call, &failures)
            {
                return Some((name, failures, call));
            }
        }
    }
    None
}

fn tool_summary(calls: &[CallRecord]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for call in calls {
        match counts.iter_mut().find(|(name, _)| *name == call.name) {
            Some(entry) => entry.1 += 1,
            None => counts.push((&call.name, 1)),
        }
    }
    counts
        .iter()
        .map(|(name, n)| format!("{name} ×{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A call in a line: its command, path or query when it has one, else its arguments.
fn describe_call(name: &str, args: &str) -> String {
    let field = serde_json::from_str::<Value>(args).ok().and_then(|value| {
        [
            "command",
            "path",
            "file_path",
            "name",
            "query",
            "url",
            "code",
        ]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
    });
    format!(
        "{name}: {}",
        excerpt(&field.unwrap_or_else(|| args.to_string()), 200)
    )
}

// ---- the user's text ------------------------------------------------------------------------

/// Where a message stands in the conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextContext {
    /// No answer from the assistant yet: there is nothing to correct.
    pub first: bool,
    /// The assistant's last answer ended with a question: this message answers it.
    pub after_question: bool,
}

/// What one message's phrases add up to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextScore {
    pub weight: f32,
    pub phrases: Vec<String>,
    /// An explicit "remember this": always triggers.
    pub explicit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// "No," / "Nope." at the very start.
    NoStart,
    /// "That's wrong", "wrong file".
    Wrong,
    /// "Not what I asked", "why did you…", "I told you…", "stop doing…".
    NotWhat,
    /// "…instead".
    Instead,
    /// "Don't X" as an imperative.
    Dont,
    /// "Always X" / "Never X" as an imperative, "you should always…".
    AlwaysNever,
    /// "From now on…", "next time…", "going forward…" with an instruction.
    Durable,
    /// "I prefer…", "I'd rather…", "I don't like it when you…".
    Prefer,
    /// "Remember to X".
    RememberTo,
}

impl Rule {
    /// The weight at the start of the message (its first sentence) and elsewhere.
    fn weights(self) -> (f32, f32) {
        match self {
            Rule::NoStart => (1.0, 0.0),
            Rule::Wrong | Rule::NotWhat => (1.25, 1.0),
            Rule::Instead => (0.75, 0.75),
            Rule::Dont => (1.0, 0.75),
            Rule::AlwaysNever => (1.5, 1.25),
            Rule::Durable | Rule::Prefer => (2.0, 1.5),
            Rule::RememberTo => (1.0, 0.75),
        }
    }

    /// How the context scales the weight (0 = doesn't count).
    fn factor(self, context: TextContext, question: bool) -> f32 {
        let mut factor = 1.0;
        if context.first {
            match self {
                Rule::NoStart | Rule::Wrong | Rule::NotWhat | Rule::Instead => return 0.0,
                Rule::Dont => factor *= 0.5,
                _ => factor *= 0.75,
            }
        }
        if context.after_question {
            match self {
                Rule::NoStart => return 0.0,
                Rule::Durable => {}
                _ => factor *= 0.5,
            }
        }
        if question {
            match self {
                Rule::NotWhat => {}
                Rule::Instead => factor *= 0.5,
                _ => return 0.0,
            }
        }
        factor
    }
}

/// Words that open an imperative without changing it ("Okay, but don't…", "Please never…").
const LEAD: &str = r"^(?:(?:ok|okay|no+|nope|nah|and|but|also|so|please|pls|just|actually|again|seriously|wait|hmm+|well|then|oh|ah|yes|yeah|yep|hey|sorry|look|listen|guys|dude)\b[\s,!.:—–-]*)*";

fn regex(pattern: &str) -> Regex {
    // The patterns are constants checked by the tests; a bad one is a bug caught there.
    Regex::new(pattern).unwrap_or_else(|e| panic!("bad signal pattern {pattern}: {e}"))
}

static NO_START: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(?:(?:no+|nope|nah)\s+)*(?:no+|nope|nah)(?:\s*[,.!:;—–-]+|\s*$)"));
/// What may follow "No," without it being a rejection: courtesy ("No problem") and status reports
/// on the agent's attempt ("No, still failing", "No luck"): a fix that didn't work yet is not a
/// lesson.
static COURTESY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"^\s*(?:no\s+)?(?:problem|problems|worries|worry|thanks|thank|need|rush|biggie|big deal|it'?s (?:fine|ok|okay|good|all right|alright)|that'?s (?:fine|ok|okay|great|perfect|good|all|it|right|correct)|all good|that works|it works|perfect|great|of course|sure|you'?re right|exactly|still|not yet|it(?:'s| is)? still|i haven'?t|i have not|haven'?t|same|luck|idea|clue|change|difference|errors?|output|response|i didn'?t (?:try|get|see|check|run))\b",
    )
});
/// "Instead" in the user's own narration ("I went with sqlite instead", "Instead of reqwest I
/// used ureq"): a first-person subject with a past-tense verb and no "you" in the sentence.
static NARRATION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:i|we)\s+(?:(?:just|then|also|actually|eventually|finally|already)\s+)?(?:[a-z]+ed|went|chose|took|got|had|did|made|wrote|built|ran|found|switched|ended|tried|picked|bought|kept|left|put|set|stuck|swapped|gave)\b",
    )
});
static YOU: LazyLock<Regex> = LazyLock::new(|| regex(r"\byou(?:'re|'ve|'d|'ll|r)?\b"));
/// "You missed nothing", "you broke nothing": reassurance.
static NOT_WHAT_IDIOMS: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\byou (?:forgot|missed|ignored|broke|deleted|removed|overwrote|skipped) (?:nothing|none|anything)\b",
    )
});
static WRONG_IDIOMS: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:nothing|something|anything|what|what'?s|whats|not|isn'?t|wasn'?t|aren'?t|can|could|might|may|will|would|went|go|goes|going|gone)\s+(?:\w+\s+)?wrong\b|\b(?:i|we)(?:'ve|'d)?\s+(?:\w+\s+){0,2}(?:the|a|my|our)\s+wrong\b|\b(?:if|when|whenever|unless|in case|once)\s+[^.;]*?\bwrong\b|\b(?:for|with|on|of|about|given|against|handles?|handling|rejects?|catch(?:es)?)\s+(?:the|a|an|any)\s+wrong\b|\bwrong-",
    )
});
static WRONG: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:that'?s|that is|this is|it'?s|it is|you'?re|you are|you got (?:it|that|this)|is|was|are|were|looks?|seems?|still|totally|completely|all|dead|plain)\s+(?:\w+\s+)?wrong\b|\bwrong\s+(?:file|folder|dir|directory|path|command|approach|way|one|branch|version|function|method|place|module|crate|package|repo|repository|tool|answer|language|syntax|order|name|variable|type|import|commit|server|port|model|endpoint|url|key|project|location|format|assumption)s?\b",
    )
});
static WRONG_START: LazyLock<Regex> = LazyLock::new(|| regex(&format!(r"{LEAD}wrong\b")));
static NOT_WHAT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\bnot what i (?:asked|wanted|meant|said|need|needed|expected)\b|\b(?:that'?s|that is|this is|it'?s|it is) (?:not (?:right|correct|it|what i|how|true|quite)|incorrect|a mistake)\b|\bthat isn'?t (?:right|correct|what|how)\b|\bi (?:didn'?t|did not|never) (?:ask|asked|want|wanted) (?:you|for|to|that|this|it)\b|\bi (?:didn'?t|did not|never) (?:tell|told|say|said) (?:you|to|that you|that we|anything)\b|\bwhy (?:did|would) you (?:\w+\s+)?(?:delete|remove|change|ignore|break|skip|overwrite|rewrite|revert|touch|drop|rename|move|disable|undo|reformat|not|stop)\b|\bwhy (?:didn'?t|did not|wouldn'?t) you\b|\byou (?:forgot|missed|ignored|broke|deleted|removed|overwrote|skipped|misread|misunderstood)\b|\byou keep \w+ing\b|\byou (?:shouldn'?t|should not) have\b|\byou were (?:supposed|told|asked)\b|\b(?:as|like) i (?:said|asked|told you|mentioned)\b|\bi (?:already )?(?:told|asked) you\b|\bi said (?:to|not|we|you|it|that)\b|\bstop (?:doing|using|adding|changing|asking|creating|writing|making|guessing|apologi[sz]ing|repeating|ignoring|editing|touching|deleting|removing|overwriting|reformatting)\b|\bplease stop\b",
    )
});
static INSTEAD: LazyLock<Regex> = LazyLock::new(|| regex(r"\binstead\b"));
static WH_QUESTION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(?:what|what'?s|whats|how|which|where|who|when)\b"));
static DONT: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"{LEAD}(?:don'?t|do not)\s+(?:ever\s+)?([a-z][a-z'-]*)"
    ))
});
/// "Don't X" that are idioms, not instructions.
const DONT_IDIOMS: &[&str] = &[
    "worry",
    "worries",
    "mind",
    "know",
    "bother",
    "hesitate",
    "forget",
    "care",
    "panic",
    "stress",
    "think",
    "sweat",
    "apologize",
    "apologise",
    "feel",
    "get",
    "mean",
    "see",
    "have",
    "need",
    "want",
];
static ALWAYS_NEVER: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"{LEAD}(?:you (?:should|must|need to|have to|ought to)\s+)?(?:always|never)\s+(?:ever\s+)?([a-z][a-z'-]*)"
    ))
});
static YOU_SHOULD_ALWAYS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\byou (?:should|must|need to|have to|ought to) (?:always|never)\b"));
/// Verbs an "always" / "never" rule starts with (so "always happy", "never mind" don't count).
const RULE_VERBS: &[&str] = &[
    "add",
    "answer",
    "apologize",
    "apologise",
    "ask",
    "assume",
    "be",
    "build",
    "bump",
    "call",
    "change",
    "check",
    "close",
    "comment",
    "commit",
    "confirm",
    "copy",
    "create",
    "default",
    "delete",
    "deploy",
    "do",
    "document",
    "double-check",
    "edit",
    "end",
    "explain",
    "follow",
    "force",
    "format",
    "generate",
    "give",
    "go",
    "guess",
    "hardcode",
    "import",
    "include",
    "inline",
    "install",
    "keep",
    "kill",
    "leave",
    "let",
    "lint",
    "log",
    "make",
    "mention",
    "merge",
    "mock",
    "modify",
    "move",
    "open",
    "overwrite",
    "pin",
    "prefer",
    "prefix",
    "print",
    "publish",
    "pull",
    "push",
    "put",
    "quote",
    "read",
    "rebase",
    "refactor",
    "reformat",
    "release",
    "rely",
    "remove",
    "rename",
    "reply",
    "respond",
    "restart",
    "return",
    "reuse",
    "rewrite",
    "run",
    "save",
    "send",
    "ship",
    "show",
    "sign",
    "skip",
    "sort",
    "split",
    "squash",
    "stage",
    "start",
    "stop",
    "store",
    "switch",
    "tag",
    "tell",
    "test",
    "touch",
    "translate",
    "trust",
    "try",
    "update",
    "upgrade",
    "use",
    "verify",
    "wrap",
    "write",
];
static DURABLE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:from now on|going forward|from here on(?: out)?|in (?:the )?future|next time|in future sessions|for future (?:sessions|reference)|whenever you)\b",
    )
});
static DURABLE_CUE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:please|always|never|don'?t|do not|use|make sure|you should|you must|you need to|i want|i'd like|ask|remember|keep|stop|prefer|avoid|check|run|put|write|call|try|be)\b",
    )
});
/// "Next time I'll…", "in the future we will…": the user's own plans, not a rule.
static SELF_FUTURE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:i|we)(?:'ll| will| might| may| can| could| should| would| plan to| want to| need to)\b",
    )
});
/// "In the future this module will be split", "next time it'll run faster": a forecast about
/// something other than the agent (any subject but "you" before will / 'll / might…).
static FUTURE_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b([a-z]+)(?:'ll|\s+(?:will|won't|wont|might|may|shall|is going to|are going to|gonna))\b",
    )
});
static PREFER: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\bi(?:'d| would)?\s+(?:really\s+|much\s+|strongly\s+|always\s+)?(?:prefer|rather)\b|\bmy preference is\b|\bi (?:like|love|want) (?:it )?when you\b|\bi(?:'d| would) like you to (?:always|never)\b|\bi (?:really )?(?:don'?t|do not) (?:want|like) (?:it )?(?:you to|when you|that you|how you)\b|\bi (?:hate|dislike) (?:it )?(?:when|that|how) you\b",
    )
});
static REMEMBER_THIS: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"{LEAD}(?:please\s+)?(?:remember|memori[sz]e|keep in mind|don'?t forget|never forget|make a note of|take note of)\s+(?:this|that|it)\s*(?:[:.!]*\s*$|:|for (?:next time|later|the future|future sessions|future reference|next sessions?)\b)"
    ))
});
static REMEMBER_THAT: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"{LEAD}(?:please\s+)?(?:remember|memori[sz]e)(?:\s+that\b|\s*:)"
    ))
});
/// A verb in what follows "remember that": a fact ("…that I use PowerShell", "…that our CI runs
/// on Windows"), not a noun phrase ("…that function we wrote").
static FACT_VERB: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:is|are|was|were|am|i'm|it's|we're|you're|they're|there's|isn't|aren't|use|uses|run|runs|has|have|live|lives|need|needs|must|should|can|can't|cannot|don't|doesn't|do|does|won't|will|always|never|prefer|prefers|like|likes|want|wants|work|works|deploy|deploys|go|goes|belongs?|means?|requires?|expects?|hate|hates)\b",
    )
});
static SAVE_TO_MEMORY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:save|add|put|store|write|keep|record|note)\s+(?:this|that|it|these|those)\s+(?:down\s+)?(?:in|to|into)\s+(?:your\s+|the\s+)?(?:memory|memories|long-term memory|user profile|notes)\b|\bcommit (?:this|that|it) to memory\b|^(?:can|could|would|will) you (?:please )?(?:remember|memori[sz]e) (?:this|that)\b",
    )
});
static REMEMBER_TO: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"{LEAD}(?:please\s+)?remember to\s+\w")));

/// Scores one message the user wrote. Zero for ordinary messages.
pub fn score_text(text: &str, context: TextContext) -> TextScore {
    if is_generated_prompt(text) {
        return TextScore::default();
    }
    let cleaned = clean(text);
    if cleaned.trim().is_empty() || !looks_english(&cleaned) {
        return TextScore::default();
    }
    let mut hits: Vec<(Rule, f32, String)> = Vec::new();
    let mut hit = |rule: Rule, head: bool, question: bool, phrase: &str| {
        let (start, elsewhere) = rule.weights();
        let weight = if head { start } else { elsewhere } * rule.factor(context, question);
        if weight <= 0.0 {
            return;
        }
        let phrase = excerpt(phrase.trim(), 60);
        match hits.iter_mut().find(|(r, _, _)| *r == rule) {
            Some(entry) if weight > entry.1 => *entry = (rule, weight, phrase),
            Some(_) => {}
            None => hits.push((rule, weight, phrase)),
        }
    };
    let mut explicit: Option<String> = None;

    if let Some(found) = NO_START.find(&cleaned)
        && !COURTESY.is_match(&cleaned[found.end()..])
    {
        hit(Rule::NoStart, true, false, found.as_str());
    }
    for (index, sentence) in sentences(&cleaned).iter().enumerate() {
        let head = index == 0;
        let (s, question) = (sentence.text.as_str(), sentence.question);
        let parts: Vec<&str> = std::iter::once(s).chain(clauses(s)).collect();
        if let Some(found) = SAVE_TO_MEMORY.find(s) {
            explicit.get_or_insert_with(|| found.as_str().to_string());
        }
        if !question
            && let Some(found) = parts
                .iter()
                .find_map(|p| REMEMBER_THIS.find(p))
                .or_else(|| {
                    // "Remember that <a fact>", not "remember that function we wrote?": the clause
                    // after "that" must say something (have a verb), within its own clause.
                    parts.iter().find_map(|p| {
                        let found = REMEMBER_THAT.find(p)?;
                        let rest = &p[found.end()..];
                        let fact = if found.as_str().ends_with(':') {
                            !rest.trim().is_empty()
                        } else {
                            let clause = rest.split([',', ';', ':', '—', '–']).next().unwrap_or("");
                            FACT_VERB.is_match(clause)
                        };
                        fact.then_some(found)
                    })
                })
        {
            explicit.get_or_insert_with(|| found.as_str().to_string());
        }
        if let Some(found) = NOT_WHAT.find(&NOT_WHAT_IDIOMS.replace_all(s, " ")) {
            hit(Rule::NotWhat, head, question, found.as_str());
        }
        if let Some(found) = INSTEAD.find(s)
            && !(question && WH_QUESTION.is_match(s))
            && !(NARRATION.is_match(s) && !YOU.is_match(s))
        {
            hit(Rule::Instead, head, question, found.as_str());
        }
        if question {
            continue;
        }
        let unidiomatic = WRONG_IDIOMS.replace_all(s, " ");
        if let Some(found) = WRONG.find(&unidiomatic) {
            hit(Rule::Wrong, head, false, found.as_str());
        } else if let Some(found) = parts.iter().find_map(|p| WRONG_START.find(p)) {
            hit(Rule::Wrong, head, false, found.as_str());
        }
        if let Some(phrase) = imperative(&DONT, &parts, |verb| !DONT_IDIOMS.contains(&verb)) {
            hit(Rule::Dont, head, false, &phrase);
        }
        if let Some(found) = YOU_SHOULD_ALWAYS.find(s) {
            hit(Rule::AlwaysNever, head, false, found.as_str());
        } else if let Some(phrase) =
            imperative(&ALWAYS_NEVER, &parts, |verb| RULE_VERBS.contains(&verb))
        {
            hit(Rule::AlwaysNever, head, false, &phrase);
        }
        if let Some(found) = DURABLE.find(s)
            && DURABLE_CUE.is_match(s)
            && !SELF_FUTURE.is_match(s)
            && !FUTURE_SUBJECT
                .captures_iter(s)
                .any(|c| c.get(1).is_some_and(|w| w.as_str() != "you"))
        {
            hit(Rule::Durable, head, false, found.as_str());
        }
        if let Some(found) = PREFER.find(s) {
            hit(Rule::Prefer, head, false, found.as_str());
        }
        if let Some(found) = parts.iter().find_map(|p| REMEMBER_TO.find(p)) {
            hit(Rule::RememberTo, head, false, found.as_str());
        }
    }

    if let Some(phrase) = explicit {
        let mut phrases = vec![excerpt(phrase.trim(), 60)];
        phrases.extend(hits.into_iter().map(|(_, _, p)| p));
        return TextScore {
            weight: EXPLICIT_WEIGHT,
            phrases,
            explicit: true,
        };
    }
    let weight = hits.iter().map(|(_, w, _)| w).sum::<f32>().min(MESSAGE_CAP);
    TextScore {
        weight,
        phrases: hits.into_iter().map(|(_, _, p)| p).collect(),
        explicit: false,
    }
}

/// The first clause where `pattern` matches with a verb `accept` takes: the matched phrase.
fn imperative(pattern: &Regex, parts: &[&str], accept: impl Fn(&str) -> bool) -> Option<String> {
    parts.iter().find_map(|part| {
        let captures = pattern.captures(part)?;
        let verb = captures.get(1)?.as_str().trim_end_matches(['\'', '-']);
        accept(verb).then(|| captures.get(0).map_or("", |m| m.as_str()).to_string())
    })
}

/// Messages a command wrote rather than the user (`/plan`, `/init`, `/learn`, a skill or bundle,
/// `/review`, anything typed as a slash command).
pub fn is_generated_prompt(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with('/')
        || text.starts_with("[/")
        || text.starts_with("[IMPORTANT:")
        || hermes_prompts::review::learn_request(text).is_some()
        || hermes_prompts::coding::plan_request(text).is_some()
        || hermes_prompts::coding::init_request(text).is_some()
        || hermes_tools::skills::describe_invocation(text).is_some()
        || hermes_tools::describe_bundle_invocation(text).is_some()
}

static INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| regex(r"`[^`\n]*`"));
static DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| regex(r#""[^"\n]{0,300}""#));
static SINGLE_QUOTED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(^|\s)'[^'\n]{1,80}'([\s.,!?;:)]|$)"));
static EMAIL_START: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^\s*(?:(?:from|sent|to|subject|cc|bcc|date|reply-to):\s+\S|on\s.{4,160}\swrote:\s*$|-{2,}\s*(?:original message|forwarded message))",
    )
});
static LOG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?i)^\s*(?:\[?\d{4}-\d{2}-\d{2}[ t]\d{2}:\d{2}|\[?\d{2}:\d{2}:\d{2}|[\w.]*(?:error|warn|warning|info|debug|trace|fatal|panic|exception|caused by)\b\s*[:\]\[(]|\[(?:error|warn|warning|info|debug|trace|fatal)\]|at\s+[\w.$<>]+\s*\(|traceback \(most recent call last\)|file "[^"]+", line \d+|error\[e\d+\]|-->\s|\d+\s*\|\s|\|\s|thread '.*' panicked|npm err!|stack backtrace|\$\s|>>>\s|ps [a-z]:\\|[a-z]:\\[^ ]*>)"#,
    )
});
/// URLs and paths (`https://…/never/wrong`, `C:\…\instead.txt`, `src/never.rs`): names, not words.
static PATH_OR_URL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"[^\s\x22']*(?:://|[\\/])[^\s\x22']*"));
/// A line that opens with a speaker's name, as in a pasted chat ("Alice: …", "User: …").
static SPEAKER: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"^\s*(?:\[[^\]]{1,30}\]\s*)?([A-Za-z][\w.'-]{0,20}(?: [A-Za-z][\w.'-]{0,20})?):\s+\S")
});
/// Labels that open a line of the user's own message ("Note: …", "Also: …"), not a speaker.
const NOT_SPEAKERS: &[&str] = &[
    "note",
    "also",
    "edit",
    "update",
    "ps",
    "p.s",
    "fix",
    "todo",
    "tip",
    "example",
    "e.g",
    "i.e",
    "context",
    "goal",
    "task",
    "problem",
    "issue",
    "question",
    "answer",
    "result",
    "expected",
    "actual",
    "steps",
    "reason",
    "why",
    "summary",
    "tl;dr",
    "tldr",
    "important",
    "nb",
    "n.b",
    "and",
    "but",
    "so",
    "remember",
    "rule",
    "rules",
    "bonus",
    "next",
    "then",
    "first",
    "second",
    "finally",
    "now",
    "later",
    "before",
    "after",
];

fn is_speaker_line(line: &str) -> bool {
    SPEAKER.captures(line).is_some_and(|c| {
        c.get(1)
            .is_some_and(|name| !NOT_SPEAKERS.contains(&name.as_str().to_lowercase().as_str()))
    })
}

const CODE_STARTS: &[&str] = &[
    "fn ",
    "pub ",
    "def ",
    "let ",
    "const ",
    "import ",
    "#include",
    "class ",
    "public ",
    "private ",
    "}",
    "//",
    "#!",
    "<?",
    "<div",
    "@@",
    "diff --git",
    "struct ",
    "impl ",
    "function ",
    "var ",
    "return ",
];

/// The user's own words: without code, quotes, logs, pasted e-mails and the tail of a long
/// paste; lower case, typographic apostrophes and quotes made plain.
fn clean(text: &str) -> String {
    // A huge paste (a log, a file) costs time on the turn's task for nothing: the user's own
    // words are at its start, and only the first `HEAD_KEEP` cleaned characters are read anyway.
    let text: String = text.chars().take(MAX_READ).collect();
    let text = text
        .replace('\r', "")
        .replace(['\u{2019}', '\u{2018}', '\u{2032}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"");
    // A pasted chat ("User: …", "Bob: …"): two or more lines that open with a speaker's name are
    // somebody else's words.
    let transcript = text
        .lines()
        .filter(|line| is_speaker_line(line))
        .take(2)
        .count()
        >= 2;
    let mut kept: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if EMAIL_START.is_match(line) {
            break;
        }
        let code = CODE_STARTS.iter().any(|s| trimmed.starts_with(s))
            || trimmed.ends_with([';', '{', '}']);
        if trimmed.starts_with('>')
            || line.starts_with("    ")
            || line.starts_with('\t')
            || code
            || LOG_LINE.is_match(line)
            || (transcript && is_speaker_line(line))
        {
            continue;
        }
        kept.push(line);
    }
    let joined = kept.join("\n");
    let joined = INLINE_CODE.replace_all(&joined, " ");
    let joined = PATH_OR_URL.replace_all(&joined, " ");
    let joined = DOUBLE_QUOTED.replace_all(&joined, " ");
    let joined = SINGLE_QUOTED.replace_all(&joined, "$1 $2");
    let joined = joined.replace(['*', '_', '#'], "");
    let mut lower = joined.to_lowercase();
    if lower.chars().count() > LONG_PASTE {
        lower = lower.chars().take(HEAD_KEEP).collect();
    }
    lower.trim().to_string()
}

/// Common English words that are not also common words of the other languages the user might
/// write in, so their absence says the message isn't English.
const ENGLISH_WORDS: &[&str] = &[
    "the", "to", "it", "you", "this", "that", "and", "of", "for", "not", "please", "with", "be",
    "are", "don't", "dont", "my", "your", "can", "should", "what", "why", "how", "i'm", "it's",
    "that's", "from", "now", "use", "have", "has", "was", "will", "just", "there", "here", "they",
    "but", "if", "or", "at", "is", "we", "would", "could", "did", "does", "want", "ask", "before",
    "after", "next", "time", "again", "first", "run", "make", "need", "when", "which", "about",
    "into", "than", "them", "these", "those", "only", "some", "any", "every", "should", "always",
    "never", "instead", "wrong", "keep", "stop", "remember", "prefer",
];

fn looks_english(text: &str) -> bool {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if letters == 0 {
        return false;
    }
    let foreign = text
        .chars()
        .filter(|c| c.is_alphabetic() && !c.is_ascii())
        .count();
    if foreign * 10 > letters * 3 {
        return false;
    }
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphabetic() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    words.len() < 4 || words.iter().any(|w| ENGLISH_WORDS.contains(w))
}

struct Sentence {
    text: String,
    question: bool,
}

fn sentences(text: &str) -> Vec<Sentence> {
    fn flush(current: &mut String, out: &mut Vec<Sentence>) {
        let text = current.trim();
        if !text.is_empty() {
            out.push(Sentence {
                question: text.ends_with('?'),
                text: text.to_string(),
            });
        }
        current.clear();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            flush(&mut current, &mut out);
            continue;
        }
        current.push(c);
        if matches!(c, '.' | '!' | '?' | ';') && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
        {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);
    out
}

fn clauses(sentence: &str) -> impl Iterator<Item = &str> {
    sentence
        .split([',', ';', ':', '—', '–'])
        .flat_map(|part| part.split(" - "))
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn ends_with_question(text: &str) -> bool {
    text.trim_end_matches(|c: char| c.is_whitespace() || "*_`)\"'".contains(c))
        .ends_with('?')
}

// ---- messages -------------------------------------------------------------------------------

/// A message the user typed (what `/undo` counts as a turn).
fn is_typed_user(message: &Message) -> bool {
    message.role == Role::User
        && message.is_user_visible()
        && !message.is_tool_response()
        && message
            .content
            .iter()
            .any(|c| matches!(c, MessageContent::Text(_)))
}

/// What the user typed in a stored message: without `@` expansions and attached context.
fn typed_text(message: &Message) -> String {
    hermes_tools::references::typed_text(message)
        .map(str::to_string)
        .unwrap_or_else(|| crate::preinject::user_text(message))
}

fn response_text(content: &MessageContent) -> String {
    match content.as_tool_response().map(|r| &r.tool_result) {
        Some(Ok(_)) => content.as_tool_response_text().unwrap_or_default(),
        Some(Err(error)) => error.message.to_string(),
        None => String::new(),
    }
}

/// `text` trimmed to `max` characters, with an ellipsis when cut.
/// `text` trimmed to `max` characters, with an ellipsis when cut, and with the secrets it holds
/// masked (`hermes_tools::redact`: vendor-prefixed keys, auth headers, private keys, JWTs,
/// credentials in URLs): excerpts are kept on disk and go to the review's model later, and the
/// user may have pasted a key. Masked before the cut (with some slack), so a key at the cut is
/// never kept half-unmasked.
pub fn excerpt(text: &str, max: usize) -> String {
    let text = text.trim();
    let head: String = text.chars().take(max + 512).collect();
    let text = hermes_tools::redact::redact(&head, hermes_tools::redact::Source::Output);
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// A stable hash of a message's words and the start of what they answered (FNV-1a over the
/// lower-cased words), for [`Signal::key`].
fn text_key(answered: &str, text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let answered: String = answered.chars().take(2000).collect();
    let text: String = text.chars().take(MAX_READ).collect();
    let words = answered
        .split_whitespace()
        .chain(["\u{0}"])
        .chain(text.split_whitespace());
    for word in words {
        for byte in word.to_lowercase().bytes().chain([b' ']) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

fn excerpt_plain(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ---- persistence ----------------------------------------------------------------------------

fn file(dir: &Path, session_id: &str) -> Option<PathBuf> {
    let safe = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    safe.then(|| dir.join(format!("{session_id}.json")))
}

/// A session's signals as stored (empty when there are none or the file is damaged). Blocking.
pub fn load(dir: &Path, session_id: &str) -> SessionSignals {
    let Some(path) = file(dir, session_id) else {
        return SessionSignals::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return SessionSignals::default();
    };
    match serde_json::from_slice::<SessionSignals>(&bytes) {
        Ok(mut signals) => {
            // A hand-edited file still respects the caps.
            signals.enforce_cap();
            for signal in &mut signals.signals {
                signal.evidence.truncate(MAX_EVIDENCE);
            }
            signals
        }
        Err(e) => {
            tracing::warn!(
                "signals: {} is damaged, starting afresh: {e}",
                path.display()
            );
            SessionSignals::default()
        }
    }
}

fn save(dir: &Path, session_id: &str, signals: &SessionSignals) -> std::io::Result<()> {
    let Some(path) = file(dir, session_id) else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let mut signals = signals.clone();
    signals.v = FILE_VERSION;
    let temp = path.with_extension("json.tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(&signals).unwrap_or_default(),
    )?;
    std::fs::rename(&temp, &path)
}

enum Op {
    Record {
        session: String,
        analysis: Analysis,
    },
    Reviewed {
        session: String,
        upto: u64,
    },
    ReviewStarted {
        session: String,
    },
    Forget {
        session: String,
    },
    Snapshot {
        session: String,
        reply: tokio::sync::oneshot::Sender<SessionSignals>,
    },
}

struct Inner {
    tx: Option<mpsc::Sender<Op>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The last handle is gone: the thread writes what is queued, then stops. Joining waits
        // for at most those writes. (The thread holds no handle, so this never runs on it; the
        // id check keeps a future change from turning that into a self-join.)
        drop(self.tx.take());
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// The sessions' signal files, written by one thread in the order changes arrive. Cheap to
/// clone; sending never blocks.
#[derive(Clone)]
pub struct SignalStore {
    inner: std::sync::Arc<Inner>,
}

impl SignalStore {
    /// The store over `dir` (`data_dir/signals`).
    pub fn open(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Op>();
        let thread = std::thread::Builder::new()
            .name("hermes-signals".into())
            .spawn(move || {
                for op in rx {
                    // A bug in one change must not stop the thread (every later change and
                    // snapshot would be lost silently).
                    let applied =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply(&dir, op)));
                    if applied.is_err() {
                        tracing::error!("signals: a change panicked; skipped");
                    }
                }
            })
            .map_err(|e| tracing::warn!("signals: no writer thread, signals are not kept: {e}"))
            .ok();
        Self {
            inner: std::sync::Arc::new(Inner {
                tx: Some(tx),
                thread,
            }),
        }
    }

    fn send(&self, op: Op) {
        if let Some(tx) = &self.inner.tx {
            let _ = tx.send(op);
        }
    }

    /// Adds a turn's signals (nothing is written for a turn without any).
    pub fn record(&self, session_id: &str, analysis: Analysis) {
        if !analysis.is_empty() {
            self.send(Op::Record {
                session: session_id.to_string(),
                analysis,
            });
        }
    }

    /// Records that a review covered the session's signals up to `seq`.
    pub fn mark_reviewed(&self, session_id: &str, seq: u64) {
        self.send(Op::Reviewed {
            session: session_id.to_string(),
            upto: seq,
        });
    }

    /// Records that a review of the session's signals is about to send its request
    /// ([`SessionSignals::retry_wait`]).
    pub fn review_started(&self, session_id: &str) {
        self.send(Op::ReviewStarted {
            session: session_id.to_string(),
        });
    }

    /// Deletes a session's file.
    pub fn forget(&self, session_id: &str) {
        self.send(Op::Forget {
            session: session_id.to_string(),
        });
    }

    /// The session's signals, after every change sent before this call.
    pub async fn snapshot(&self, session_id: &str) -> SessionSignals {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send(Op::Snapshot {
            session: session_id.to_string(),
            reply,
        });
        answer.await.unwrap_or_default()
    }
}

fn apply(dir: &Path, op: Op) {
    let now = chrono::Utc::now().timestamp_millis();
    let (session, change): (String, Box<dyn FnOnce(&mut SessionSignals)>) = match op {
        Op::Snapshot { session, reply } => {
            let _ = reply.send(load(dir, &session));
            return;
        }
        Op::Forget { session } => {
            if let Some(path) = file(dir, &session) {
                let _ = std::fs::remove_file(path);
            }
            return;
        }
        Op::Record { session, analysis } => (
            session,
            Box::new(move |signals| {
                if let Some(reason) = &analysis.denial_reason {
                    signals.attach_denial_reason(reason);
                }
                for signal in analysis.signals {
                    signals.add(signal, now);
                }
            }),
        ),
        Op::Reviewed { session, upto } => (
            session,
            Box::new(move |signals| signals.mark_reviewed(upto)),
        ),
        Op::ReviewStarted { session } => (
            session,
            Box::new(move |signals| signals.review_started(now)),
        ),
    };
    // A file a newer version wrote is left alone (rewriting it in this format would lose what
    // this version doesn't know).
    if let Some(path) = file(dir, &session)
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && value
            .get("v")
            .and_then(Value::as_u64)
            .is_some_and(|v| v > u64::from(FILE_VERSION))
    {
        tracing::warn!(
            "signals: {} is from a newer version, left as is",
            path.display()
        );
        return;
    }
    let mut signals = load(dir, &session);
    change(&mut signals);
    if let Err(e) = save(dir, &session, &signals) {
        tracing::warn!("signals: writing {session}'s file failed: {e}");
    }
}

#[cfg(test)]
mod tests;
