//! The composer's data, with no GPUI in it: what a send is ([`SendRequest`], [`Sent`]), the queue
//! behind a running turn ([`QueuedItem`]), the controls' state ([`Controls`], [`Mode`], the
//! reasoning efforts, the model list and its filter) and the drafts kept in `native-ui.json`
//! ([`Drafts`]: one per thread, bounded, an empty one removed).
//!
//! t3code: `apps/web/src/components/chat/ComposerFooterControls.tsx` (`ComposerPermissionsSection`:
//! the permission modes, their names and descriptions), `ComposerModelEffort.tsx` (the effort
//! names), `QueuedRunsControl.tsx` (a queue shows its first messages and "Show all N queued"),
//! `lib/contextWindow.ts` (`formatContextWindowTokens`: `18k`, `1.2M`). Our modes are the runtime's
//! `ApprovalMode` (the editor menu of `crates/desktop/src/acp/controls.rs`: Auto, Manual, Accept
//! edits, Plan, YOLO); the efforts are Hermes's own list as that file has it.
//!
//! Hermes: `acp_adapter/modes.py` (the modes), the frontend's `REASONING_EFFORTS` (the efforts).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// --- sending -------------------------------------------------------------------------------------

/// What the composer sends: the text, and how to treat it when a turn is already running.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendRequest {
    /// What the user typed.
    pub text: String,
    /// The id this send goes by (the core's admission id): the thread's row for the prompt is
    /// shown when the turn with this id starts, so a queued prompt appears when it runs.
    pub admission_id: String,
    /// While a turn runs: put this into the running turn now (Ctrl+Enter, t3code's "steer")
    /// instead of queueing it behind the turn (Enter).
    pub steer: bool,
}

/// What the core did with a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// The turn started.
    Started,
    /// A turn was running: the prompt waits behind it.
    Queued,
    /// The text went into the running turn.
    Steered,
}

/// One prompt waiting behind the running turn, as the core's queue lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedItem {
    /// The core's admission id.
    pub id: String,
    /// What was typed.
    pub text: String,
    /// Already on screen as a message (a redirect, a steer the turn missed): the queue lists
    /// no card for it.
    pub shown: bool,
}

/// A change to the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueOp {
    /// Take a prompt out.
    Remove(String),
    /// Run it now, inside the running turn ("Send now").
    Steer(String),
    /// Replace its text: the edit goes to the back of the queue as a new admission, `new_id`
    /// (the composer names it, so the thread shows its row when its turn starts).
    Replace {
        id: String,
        text: String,
        new_id: String,
    },
}

/// Picks made in the footer that the next send applies (a new thread has no session to apply
/// them to until it is created; a stored thread applies them at once and this stays empty).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picks {
    /// `Some(None)`: back to Settings' effort.
    pub effort: Option<Option<String>>,
    pub mode: Option<Mode>,
}

impl Picks {
    /// Whether nothing was picked.
    pub fn is_empty(&self) -> bool {
        self.effort.is_none() && self.mode.is_none()
    }
}

/// One change made in the footer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The model of the next turn (`provider`, `model`).
    Model {
        provider: String,
        model: String,
    },
    /// The reasoning effort (`None`: Settings').
    Effort(Option<String>),
    Mode(Mode),
}

/// t3code lists the first two of a longer queue and "Show all N queued" (`QUEUE_VISIBLE_ROWS`).
pub const QUEUE_VISIBLE_ROWS: usize = 3;

/// The queue items shown: all of them, or the first `QUEUE_VISIBLE_ROWS - 1` of a long queue that
/// is not expanded.
pub fn visible_queue(queue: &[QueuedItem], expanded: bool) -> &[QueuedItem] {
    if queue.len() > QUEUE_VISIBLE_ROWS && !expanded {
        &queue[..QUEUE_VISIBLE_ROWS - 1]
    } else {
        queue
    }
}

/// Whether a text can be sent: not empty, not only white space.
pub fn sendable(text: &str) -> bool {
    !text.trim().is_empty()
}

// --- the permission modes ---------------------------------------------------------------------

/// How the session handles permission decisions (`hermes_tools::ApprovalMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Every file edit and every command asks first.
    Manual,
    /// File edits go through; flagged commands ask (the default).
    AcceptEdits,
    /// A reviewer model judges risky commands; only an uncertain verdict asks.
    Auto,
    /// Edits are refused; the model presents a plan.
    Plan,
    /// Nothing asks.
    Yolo,
}

impl Mode {
    /// The modes in the order the picker lists them: t3code's from the most to the least careful
    /// (supervised, auto-accept edits, auto, full access), with Plan after Manual.
    pub const ALL: [Mode; 5] = [
        Mode::Manual,
        Mode::Plan,
        Mode::AcceptEdits,
        Mode::Auto,
        Mode::Yolo,
    ];

    /// The name the chip and the menu show.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Manual => "Manual",
            Mode::AcceptEdits => "Accept edits",
            Mode::Auto => "Auto",
            Mode::Plan => "Plan",
            Mode::Yolo => "YOLO",
        }
    }

    /// What it does (the wording of the editor menu).
    pub fn description(self) -> &'static str {
        match self {
            Mode::Manual => "Always ask before making changes.",
            Mode::AcceptEdits => "Auto-approve edits, ask before risky commands.",
            Mode::Auto => "A reviewer model judges risky commands.",
            Mode::Plan => "Create a plan before making changes.",
            Mode::Yolo => "Never ask (commands that must never run still don't).",
        }
    }

    /// A stable name for ids and tests.
    pub fn id(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::AcceptEdits => "accept_edits",
            Mode::Auto => "auto",
            Mode::Plan => "plan",
            Mode::Yolo => "yolo",
        }
    }
}

// --- the reasoning effort ------------------------------------------------------------------------

/// Hermes's efforts, with "Thinking off" as `off`: the same list for every model, as in its own
/// frontend. A level a model does not know is the provider's to clamp or refuse.
pub const EFFORTS: [(&str, &str); 8] = [
    ("off", "Off"),
    ("minimal", "Minimal"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra High"),
    ("max", "Max"),
    ("ultra", "Ultra"),
];

/// What the picker shows when nothing was picked and Settings has none (Hermes's default).
pub const DEFAULT_EFFORT: &str = "medium";

/// The name of an effort id (the id itself for one the list does not have).
pub fn effort_label(id: &str) -> String {
    EFFORTS
        .iter()
        .find(|(effort, _)| *effort == id)
        .map_or_else(|| id.to_string(), |(_, label)| (*label).to_string())
}

// --- the controls' state -----------------------------------------------------------------------

/// What the composer's footer shows about a thread (or about a new one), read from the core.
#[derive(Clone, Debug, PartialEq)]
pub struct Controls {
    /// A turn is running (or its queue is not empty yet).
    pub running: bool,
    /// The prompts waiting behind it, in the order they will run.
    pub queue: Vec<QueuedItem>,
    /// The provider's id and the model the next turn runs on (a pick made mid-turn already shows).
    pub provider: String,
    pub model: String,
    /// The provider's display name.
    pub provider_name: String,
    /// The effort picked for the session or set in Settings; empty when neither.
    pub effort: String,
    pub mode: Mode,
    /// How full the context is: tokens used and the window.
    pub context: Option<(usize, usize)>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            running: false,
            queue: Vec::new(),
            provider: String::new(),
            model: String::new(),
            provider_name: String::new(),
            effort: String::new(),
            mode: Mode::AcceptEdits,
            context: None,
        }
    }
}

impl Controls {
    /// The model chip's label: the model, or a hint when no model is chosen.
    pub fn model_label(&self) -> String {
        if self.model.is_empty() {
            "Choose a model".to_string()
        } else {
            self.model.clone()
        }
    }

    /// The effort chip's text (what is picked, else Hermes's default).
    pub fn effort_text(&self) -> String {
        effort_label(if self.effort.is_empty() {
            DEFAULT_EFFORT
        } else {
            &self.effort
        })
    }
}

/// `18k`, `200k`, `1.2M`: a token count as t3code's context meter writes it.
pub fn format_tokens(tokens: usize) -> String {
    let compact = |value: f64, unit: &str| {
        let text = format!("{value:.1}");
        format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
    };
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => compact(tokens as f64 / 1_000., "k"),
        _ => compact(tokens as f64 / 1_000_000., "M"),
    }
}

/// `18k / 200k` and the percent, for the context chip's tooltip.
pub fn context_label(used: usize, max: usize) -> String {
    let percent = (used * 100).checked_div(max).map_or(0, |p| p.min(100));
    format!(
        "Context {} / {} ({percent}%)",
        format_tokens(used),
        format_tokens(max)
    )
}

// --- the model list ----------------------------------------------------------------------------

/// One model of one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
}

/// The models the configured endpoints list (read-only: adding endpoints is Settings, Phase 7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelChoices {
    pub entries: Vec<ModelEntry>,
}

impl ModelChoices {
    /// The entries whose model or provider contains every word of `query` (any case), the current
    /// model first. All of them for an empty query.
    pub fn filter(&self, query: &str, current: Option<(&str, &str)>) -> Vec<&ModelEntry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut found: Vec<&ModelEntry> = self
            .entries
            .iter()
            .filter(|entry| {
                let haystack = format!("{} {}", entry.provider_name, entry.model).to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect();
        if let Some((provider, model)) = current
            && let Some(at) = found
                .iter()
                .position(|e| e.provider == provider && e.model == model)
        {
            let entry = found.remove(at);
            found.insert(0, entry);
        }
        found
    }
}

/// How many models the picker draws at once (a search narrows the rest).
pub const MODELS_SHOWN: usize = 200;

// --- drafts ---------------------------------------------------------------------------------------

/// One draft: the text, and when it was last typed (unix milliseconds).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftEntry {
    pub text: String,
    pub at_ms: i64,
}

/// Unsent text kept per thread (key: the thread's id) and per new thread (key: `new:<project>`),
/// in `native-ui.json`. Bounded: at most [`Drafts::MAX_DRAFTS`] of at most [`Drafts::MAX_CHARS`]
/// characters, an empty one is removed, and one whose thread is gone and untouched for
/// [`Drafts::STALE_MS`] is dropped by [`Drafts::prune`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Drafts {
    entries: BTreeMap<String, DraftEntry>,
}

impl Drafts {
    /// Most drafts kept; the oldest go first.
    pub const MAX_DRAFTS: usize = 100;
    /// Longest draft kept, in characters (a paste of a whole log is cut).
    pub const MAX_CHARS: usize = 100_000;
    /// All drafts together, in bytes of UTF-8 (what `native-ui.json` grows by at most, and what
    /// each debounced save copies and writes); the oldest go first.
    pub const MAX_TOTAL_BYTES: usize = 2_000_000;
    /// A draft of a thread the list no longer has is dropped after this long (a week: a thread
    /// archived and brought back keeps its draft meanwhile).
    pub const STALE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

    /// The draft of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|d| d.text.as_str())
    }

    /// How many drafts are kept.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether none is kept.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Keeps `text` as the draft of `key` (an empty or white-space one removes it). Returns
    /// whether anything changed.
    pub fn set(&mut self, key: &str, text: &str, now_ms: i64) -> bool {
        if !sendable(text) {
            return self.entries.remove(key).is_some();
        }
        let text: String = text.chars().take(Self::MAX_CHARS).collect();
        if self.entries.get(key).is_some_and(|d| d.text == text) {
            return false;
        }
        self.entries.insert(
            key.to_string(),
            DraftEntry {
                text,
                at_ms: now_ms,
            },
        );
        self.cap();
        true
    }

    /// Forgets the draft of `key`.
    pub fn remove(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    /// Drops the drafts of threads and projects the list does not have (`listed(key)` false) that
    /// were last typed more than [`Self::STALE_MS`] ago. Returns whether anything went.
    pub fn prune(&mut self, listed: impl Fn(&str) -> bool, now_ms: i64) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|key, d| listed(key) || now_ms - d.at_ms <= Self::STALE_MS);
        self.entries.len() != before
    }

    /// Reads leniently: an entry of the wrong shape is skipped, the rest is kept.
    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut drafts = Drafts::default();
        if let Some(object) = value.as_object() {
            for (key, entry) in object {
                if let Ok(mut entry) = serde_json::from_value::<DraftEntry>(entry.clone())
                    && sendable(&entry.text)
                {
                    // A file edited by hand is held to the same bounds.
                    if entry.text.len() > Self::MAX_CHARS {
                        entry.text = entry.text.chars().take(Self::MAX_CHARS).collect();
                    }
                    drafts.entries.insert(key.clone(), entry);
                }
            }
        }
        drafts.cap();
        drafts
    }

    /// Keeps the newest [`Self::MAX_DRAFTS`], within [`Self::MAX_TOTAL_BYTES`].
    fn cap(&mut self) {
        let total = |entries: &BTreeMap<String, DraftEntry>| -> usize {
            entries.values().map(|d| d.text.len()).sum()
        };
        while self.entries.len() > Self::MAX_DRAFTS
            || (self.entries.len() > 1 && total(&self.entries) > Self::MAX_TOTAL_BYTES)
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, d)| d.at_ms)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// The draft key of a new thread in `project`.
pub fn new_thread_key(project: &str) -> String {
    format!("new:{project}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> QueuedItem {
        QueuedItem {
            id: id.into(),
            text: id.into(),
            shown: false,
        }
    }

    #[test]
    fn a_long_queue_shows_two_and_all_when_expanded() {
        let queue: Vec<_> = ["a", "b", "c", "d"].map(item).into();
        assert_eq!(visible_queue(&queue, false).len(), 2);
        assert_eq!(visible_queue(&queue, true).len(), 4);
        assert_eq!(visible_queue(&queue[..3], false).len(), 3);
        assert!(visible_queue(&[], false).is_empty());
    }

    #[test]
    fn only_text_with_something_in_it_is_sendable() {
        assert!(sendable("hi"));
        assert!(!sendable(""));
        assert!(!sendable(" \n\t  "));
    }

    #[test]
    fn tokens_read_like_t3codes_meter() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(18_300), "18.3k");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
        assert_eq!(context_label(18_000, 200_000), "Context 18k / 200k (9%)");
        assert_eq!(context_label(5, 0), "Context 5 / 0 (0%)");
    }

    #[test]
    fn every_mode_has_a_name_a_description_and_its_own_id() {
        let ids: std::collections::HashSet<_> = Mode::ALL.iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), Mode::ALL.len());
        assert!(
            Mode::ALL
                .iter()
                .all(|m| !m.label().is_empty() && !m.description().is_empty())
        );
    }

    #[test]
    fn efforts_have_names_and_an_unknown_one_keeps_its_id() {
        assert_eq!(effort_label("xhigh"), "Extra High");
        assert_eq!(effort_label("weird"), "weird");
        let controls = Controls::default();
        assert_eq!(controls.effort_text(), "Medium");
        let controls = Controls {
            effort: "minimal".into(),
            ..Controls::default()
        };
        assert_eq!(controls.effort_text(), "Minimal");
        assert_eq!(Controls::default().model_label(), "Choose a model");
    }

    fn entry(provider: &str, model: &str) -> ModelEntry {
        ModelEntry {
            provider: provider.into(),
            provider_name: provider.to_uppercase(),
            model: model.into(),
        }
    }

    #[test]
    fn the_model_filter_matches_words_in_any_order_and_puts_the_current_one_first() {
        let choices = ModelChoices {
            entries: vec![
                entry("or", "aion-labs/aion-2.0"),
                entry("or", "amazon/nova-lite-v1"),
                entry("local", "qwen3-coder"),
            ],
        };
        assert_eq!(choices.filter("", None).len(), 3);
        assert_eq!(choices.filter("NOVA lite", None).len(), 1);
        assert_eq!(choices.filter("local coder", None)[0].model, "qwen3-coder");
        assert!(choices.filter("zzz", None).is_empty());
        let shown = choices.filter("", Some(("local", "qwen3-coder")));
        assert_eq!(shown[0].model, "qwen3-coder");
        assert_eq!(shown.len(), 3);
    }

    #[test]
    fn drafts_are_kept_per_key_and_an_empty_one_is_removed() {
        let mut drafts = Drafts::default();
        assert!(drafts.set("a", "hello", 10));
        assert!(
            !drafts.set("a", "hello", 20),
            "the same text changes nothing"
        );
        assert_eq!(drafts.get("a"), Some("hello"));
        assert!(drafts.set("a", "hello again", 30));
        assert!(drafts.set("a", "   \n", 40), "white space removes it");
        assert_eq!(drafts.get("a"), None);
        assert!(!drafts.set("a", "", 50), "nothing to remove");
        assert!(drafts.is_empty());
    }

    #[test]
    fn drafts_are_bounded_in_number_and_length() {
        let mut drafts = Drafts::default();
        for n in 0..Drafts::MAX_DRAFTS + 20 {
            drafts.set(&format!("t{n:03}"), "x", n as i64);
        }
        assert_eq!(drafts.len(), Drafts::MAX_DRAFTS);
        assert_eq!(drafts.get("t000"), None, "the oldest went first");
        assert!(
            drafts
                .get(&format!("t{:03}", Drafts::MAX_DRAFTS + 19))
                .is_some()
        );
        drafts.set("long", &"y".repeat(Drafts::MAX_CHARS + 5000), 1_000_000);
        assert_eq!(
            drafts.get("long").unwrap().chars().count(),
            Drafts::MAX_CHARS
        );
    }

    #[test]
    fn all_drafts_together_stay_under_the_total_and_the_newest_survive() {
        let mut drafts = Drafts::default();
        // Twenty-five drafts of 100,000 three-byte characters: 7.5 MB if all were kept.
        let big = "\u{20ac}".repeat(Drafts::MAX_CHARS);
        for n in 0..25 {
            assert!(drafts.set(&format!("t{n:02}"), &big, n));
        }
        let total: usize = drafts.keys().map(|k| drafts.get(k).unwrap().len()).sum();
        assert!(total <= Drafts::MAX_TOTAL_BYTES, "{total}");
        assert!(drafts.get("t24").is_some(), "the one being typed is kept");
        assert!(drafts.get("t00").is_none(), "the oldest went");
        // A file edited by hand is held to the same bounds.
        let mut map = serde_json::Map::new();
        map.insert(
            "huge".into(),
            serde_json::json!({"text": "z".repeat(Drafts::MAX_CHARS * 3), "at_ms": 1}),
        );
        let read = Drafts::from_json(&serde_json::Value::Object(map));
        assert_eq!(read.get("huge").unwrap().len(), Drafts::MAX_CHARS);
    }

    #[test]
    fn drafts_keep_unicode_emoji_and_new_lines_through_the_file() {
        let text = "Grüße 日本語 \u{1f600}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\r\nline two\n\ttabbed";
        let mut drafts = Drafts::default();
        drafts.set("t1", text, 5);
        let json = serde_json::to_value(&drafts).unwrap();
        let back = Drafts::from_json(&serde_json::from_str(&json.to_string()).unwrap());
        assert_eq!(back.get("t1"), Some(text));
        // A cut long draft never splits a character.
        let long = "\u{1f600}".repeat(Drafts::MAX_CHARS + 3);
        drafts.set("t2", &long, 6);
        assert_eq!(drafts.get("t2").unwrap().chars().count(), Drafts::MAX_CHARS);
    }

    #[test]
    fn drafts_of_gone_threads_are_dropped_after_a_week_but_not_before() {
        let mut drafts = Drafts::default();
        drafts.set("gone-old", "a", 0);
        drafts.set("gone-new", "b", Drafts::STALE_MS);
        drafts.set("listed-old", "c", 0);
        let now = Drafts::STALE_MS + 1000;
        let listed = |key: &str| key == "listed-old";
        assert!(drafts.prune(listed, now));
        assert_eq!(drafts.get("gone-old"), None);
        assert_eq!(drafts.get("gone-new"), Some("b"), "a recent one stays");
        assert_eq!(
            drafts.get("listed-old"),
            Some("c"),
            "a listed thread keeps its draft"
        );
        assert!(!drafts.prune(listed, now), "nothing more to drop");
    }

    #[test]
    fn drafts_read_leniently() {
        let value = serde_json::json!({
            "ok": {"text": "hi", "at_ms": 5},
            "empty": {"text": "  ", "at_ms": 5},
            "bad": 7,
            "odd": {"text": 3},
        });
        let drafts = Drafts::from_json(&value);
        assert_eq!(drafts.keys().collect::<Vec<_>>(), ["ok"]);
        assert!(Drafts::from_json(&serde_json::json!([1, 2])).is_empty());
        let round: Drafts = serde_json::from_str(&serde_json::to_string(&drafts).unwrap()).unwrap();
        assert_eq!(round, drafts);
    }
}
