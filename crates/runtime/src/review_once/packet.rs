//! The evidence packet: what the review model reads in place of the conversation.
//!
//! Four sections, in this order of priority: the **signals** with their excerpts (the reason the
//! review runs at all), the **skills used** in the session (loaded with `skill_view`, attached by
//! pre-injection, or invoked as `/skill-name`), the top [`RELATED_SKILLS`] **related skills** from
//! the local index, and the current **memory** files. Everything is rendered from plain data in a
//! fixed order with no clock and no randomness, so the same input gives the same bytes (a golden
//! test pins them).
//!
//! **Budget.** The packet never exceeds its token budget ([`PACKET_MAX_TOKENS`] by default; see
//! [`budget_for`] for how the review model's window lowers it), counted by [`estimate_tokens`]: a
//! deliberately pessimistic estimate (an ASCII character is a third of a token, any other
//! character half a token per UTF-8 byte, so CJK text counts 1.5 tokens a character), not
//! `compress.rs`'s bytes / 4, which undercounts code, JSON and non-Latin text by up to 2-3x and
//! would let a packet overflow a small local model's window. Every character goes through one
//! writer that refuses what doesn't fit, so the bound holds by construction. When the evidence is
//! larger than the budget, the higher sections keep their place and the lower ones are cut
//! first: signals may take up to [`SIGNALS_SHARE`]% of the budget, used skills up to
//! [`LOADED_SHARE`]%, related skills up to [`RELATED_SHARE`]%, and memory gets the rest (at least
//! a tenth, more than the default memory limits). Within a section:
//! - signals are chosen strongest first (explicit requests, then weight, then age) and shown
//!   oldest first; the ones left out are counted in a line;
//! - a skill body is cut at [`MAX_SKILL_CHARS`] or at the room left (marked "cut"); a skill with
//!   no room left is named without its body;
//! - memory entries are kept whole, in file order, until the room runs out.
//!
//! **Secrets.** Everything is passed through `hermes_tools::redact` (excerpts already are, by
//! `signals.rs`). A skill whose shown text differs from the file in any way (cut, masked, or a
//! closing tag neutralised) is not "shown whole", and may then only be patched with a diff.
//!
//! Not from Hermes (its review replays the whole conversation to a forked agent).

use std::collections::BTreeMap;

use hermes_memory::Target;
use hermes_tools::redact::{Source, redact};

use crate::signals::{Signal, SignalKind};

/// The packet's token budget (a knob: `ReviewOnceSettings::packet_tokens`). The user's provider
/// counts requests, not tokens, so the packet may be generous; it still has to fit small local
/// models' windows (see [`budget_for`]).
pub const PACKET_MAX_TOKENS: usize = 24_000;
/// The estimator counts in sixths of a token: an ASCII character costs [`ASCII_UNITS`], every
/// UTF-8 byte of another character [`OTHER_BYTE_UNITS`].
pub const UNITS_PER_TOKEN: usize = 6;
const ASCII_UNITS: usize = 2;
const OTHER_BYTE_UNITS: usize = 3;
/// The share of the budget the signals may take (percent).
pub const SIGNALS_SHARE: usize = 40;
/// The share the skills used in the session may take.
pub const LOADED_SHARE: usize = 30;
/// The share the related skills may take.
pub const RELATED_SHARE: usize = 20;
/// One skill's body, in characters (preinject's cut: about 3k tokens).
pub const MAX_SKILL_CHARS: usize = 12_000;
/// Related skills shown, beside the ones used.
pub const RELATED_SKILLS: usize = 3;
/// A skill body is not shown at all when less room than this (characters) is left for it.
const MIN_SKILL_ROOM: usize = 400;

/// A skill as read from disk for the packet.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillText {
    pub name: String,
    /// SKILL.md as it is on disk.
    pub text: String,
    /// `skill_manage::fingerprint` of the file's bytes when read: a patch is applied only while
    /// the file is still what the model saw.
    pub fingerprint: String,
    /// A skill of the session's repository (not the user's own library).
    pub project: bool,
}

/// One memory file as the packet shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryFile {
    pub target: Target,
    pub enabled: bool,
    pub entries: Vec<String>,
    /// `MemoryStore::usage`: "used/limit" characters.
    pub usage: String,
}

/// What a packet is built from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PacketInput {
    /// The repository the session works in (its folder's name), when it is in one: the review may
    /// then keep a new skill in that repository.
    pub project: Option<String>,
    /// The unreviewed signals (`SessionSignals::evidence`).
    pub signals: Vec<Signal>,
    /// Skills used in the session, in the order first used.
    pub loaded: Vec<SkillText>,
    /// Related skills from the index, best first (none of them also used).
    pub related: Vec<SkillText>,
    /// MEMORY.md, then USER.md.
    pub memory: Vec<MemoryFile>,
}

/// A skill the packet showed: the review may patch only these.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownSkill {
    /// The model saw the file exactly as it is (so a whole-body replacement loses nothing).
    pub whole: bool,
    pub fingerprint: String,
}

/// A built packet.
#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub text: String,
    /// Skills shown with (some of) their body, by name.
    pub skills: BTreeMap<String, ShownSkill>,
    /// Signals shown, and left out to fit.
    pub signals_shown: usize,
    pub signals_left_out: usize,
    /// The highest `seq` among the input signals: what a successful review marks reviewed.
    pub max_seq: u64,
    /// The budget it was built for, in tokens.
    pub budget_tokens: usize,
}

fn char_units(c: char) -> usize {
    if c.is_ascii() {
        ASCII_UNITS
    } else {
        c.len_utf8() * OTHER_BYTE_UNITS
    }
}

/// `text`'s cost in sixths of a token (see [`estimate_tokens`]); additive over concatenation.
fn units(text: &str) -> usize {
    text.chars().map(char_units).sum()
}

/// A pessimistic token count of `text`, rounded up: an ASCII character a third of a token, other
/// characters half a token per UTF-8 byte (Latin accents and Cyrillic 1, CJK 1.5, emoji 2). Real
/// tokenizers do better on prose; the point is that the packet, the prompt and the reply never
/// overflow the review model's window.
pub fn estimate_tokens(text: &str) -> usize {
    units(text).div_ceil(UNITS_PER_TOKEN)
}

/// The longest prefix of `text` estimated at no more than `max_tokens`.
pub fn cut_to_tokens(text: &str, max_tokens: usize) -> &str {
    let limit = max_tokens.saturating_mul(UNITS_PER_TOKEN);
    let mut used = 0usize;
    for (i, c) in text.char_indices() {
        used += char_units(c);
        if used > limit {
            return &text[..i];
        }
    }
    text
}

/// The packet budget for a review model with `context_length` (when known): the knob, but no more
/// than half the window, and no more than what the window leaves once `reserved` tokens (the
/// system prompt, the response schema, the repair turn and the reply's cap) are set aside. A
/// window too small for that gives 0, and the review doesn't run. An unknown window keeps the
/// knob (nothing better is known; the provider's own context error then ends the review).
pub fn budget_for(knob: usize, context_length: Option<usize>, reserved: usize) -> usize {
    match context_length {
        Some(window) if window > 0 => knob.min(window / 2).min(window.saturating_sub(reserved)),
        _ => knob,
    }
}

/// Appends text only while the whole stays within `limit` units.
struct Writer {
    out: String,
    used: usize,
    limit: usize,
}

impl Writer {
    fn room(&self) -> usize {
        self.limit.saturating_sub(self.used)
    }

    fn push(&mut self, text: &str) -> bool {
        let cost = units(text);
        if cost > self.room() {
            return false;
        }
        self.out.push_str(text);
        self.used += cost;
        true
    }
}

/// Builds the packet for `input` within `budget_tokens`.
pub fn build(input: &PacketInput, budget_tokens: usize) -> Packet {
    let limit = budget_tokens.saturating_mul(UNITS_PER_TOKEN);
    let share = |percent: usize| limit / 100 * percent;
    let mut w = Writer {
        out: String::new(),
        used: 0,
        limit,
    };
    let score: f32 = input.signals.iter().map(|s| s.weight).sum();
    w.push(&format!(
        "# Evidence packet\n\n{} signal(s), total weight {score:.2}.\n\n## Signals\n\n",
        input.signals.len()
    ));
    let (signals_shown, signals_left_out) =
        write_signals(&mut w, &input.signals, share(SIGNALS_SHARE));
    if let Some(project) = &input.project {
        w.push(&format!(
            "## Project\n\nThis session works in the repository \"{}\": a new skill may be kept there (scope \"project\"). Skills marked \"project skill\" below are that repository's.\n\n",
            one_line(project, 80)
        ));
    }
    let mut skills = BTreeMap::new();
    if !input.loaded.is_empty() {
        w.push("## Skills used in this session\n\n");
        write_skills(&mut w, &input.loaded, share(LOADED_SHARE), &mut skills);
    }
    if !input.related.is_empty() {
        w.push("## Related skills (from the skill index)\n\n");
        write_skills(&mut w, &input.related, share(RELATED_SHARE), &mut skills);
    }
    w.push("## Memory\n\n");
    for file in &input.memory {
        write_memory(&mut w, file);
    }
    Packet {
        text: w.out,
        skills,
        signals_shown,
        signals_left_out,
        max_seq: input.signals.iter().map(|s| s.seq).max().unwrap_or(0),
        budget_tokens,
    }
}

/// Signals strongest first into at most `cap` bytes, then shown oldest first.
fn write_signals(w: &mut Writer, signals: &[Signal], cap: usize) -> (usize, usize) {
    let mut order: Vec<&Signal> = signals.iter().collect();
    order.sort_by(|a, b| {
        b.is_explicit()
            .cmp(&a.is_explicit())
            .then(b.weight.total_cmp(&a.weight))
            .then(a.seq.cmp(&b.seq))
    });
    let room = cap.min(w.room());
    let mut used = 0usize;
    let mut chosen: Vec<(u64, String)> = Vec::new();
    for signal in order {
        let block = render_signal(signal);
        let cost = units(&block);
        if used + cost <= room {
            used += cost;
            chosen.push((signal.seq, block));
        }
    }
    chosen.sort_by_key(|(seq, _)| *seq);
    let shown = chosen.len();
    for (_, block) in &chosen {
        w.push(block);
    }
    let left_out = signals.len() - shown;
    if left_out > 0 {
        w.push(&format!(
            "({left_out} weaker signal(s) left out to fit the packet.)\n\n"
        ));
    }
    (shown, left_out)
}

fn render_signal(signal: &Signal) -> String {
    let mut block = format!(
        "### Signal {}: {} (weight {:.2})\n",
        signal.seq,
        describe(&signal.kind),
        signal.weight
    );
    for excerpt in &signal.evidence {
        let role = match &excerpt.tool {
            Some(tool) => format!("tool {}", one_line(tool, 60)),
            None => one_line(&excerpt.role, 20),
        };
        let text = neutralise(&redact(&excerpt.text, Source::Output));
        block.push_str(&format!("<excerpt from=\"{role}\">\n{text}\n</excerpt>\n"));
    }
    block.push('\n');
    block
}

/// What a signal is, in a few words for the model.
fn describe(kind: &SignalKind) -> String {
    match kind {
        SignalKind::Correction { phrases } => {
            let phrases: Vec<String> = phrases
                .iter()
                .map(|p| format!("'{}'", one_line(p, 40)))
                .collect();
            format!(
                "the user corrected the agent or stated a rule ({})",
                phrases.join(", ")
            )
        }
        SignalKind::Explicit { via } if via == "learn" => {
            "the user asked to learn this (/learn)".into()
        }
        SignalKind::Explicit { .. } => "the user asked the agent to remember something".into(),
        SignalKind::Rewind { via, turns } => {
            format!(
                "the user undid {turns} turn(s) of the agent's work ({})",
                one_line(via, 20)
            )
        }
        SignalKind::Denial { what, reason } => {
            let mut text = format!(
                "the user denied: {}",
                one_line(&redact(what, Source::Output), 200)
            );
            if let Some(reason) = reason {
                text.push_str(&format!(
                    "; then said: {}",
                    one_line(&redact(reason, Source::Output), 300)
                ));
            }
            text
        }
        SignalKind::Workaround { tool, failures } => format!(
            "{} failed {failures} time(s), then worked another way (a workaround)",
            one_line(tool, 60)
        ),
        SignalKind::LongTurn { calls, tools } => format!(
            "a long procedure: {calls} tool calls ({}) ending in an answer",
            one_line(tools, 200)
        ),
    }
}

/// Skills into at most `cap` bytes (and what is left overall).
fn write_skills(
    w: &mut Writer,
    list: &[SkillText],
    cap: usize,
    shown: &mut BTreeMap<String, ShownSkill>,
) {
    let end = w.used + cap.min(w.room());
    for skill in list {
        if shown.contains_key(&skill.name) {
            continue;
        }
        let name = one_line(&skill.name, 80);
        let masked = neutralise(&redact(&skill.text, Source::Output));
        let total_chars = masked.chars().count();
        let open = format!("<skill name=\"{name}\">\n");
        let close = "\n</skill>\n\n";
        // Room for the body once the heading, tags and a "cut" heading are written.
        let heading_room = 120 * ASCII_UNITS + units(&name);
        let room = end
            .saturating_sub(w.used)
            .saturating_sub(heading_room + units(&open) + units(close));
        let body: String = if room < MIN_SKILL_ROOM * ASCII_UNITS {
            String::new()
        } else {
            cut_to(&masked, MAX_SKILL_CHARS, room)
        };
        if body.is_empty() {
            w.push(&format!("### Skill {name} (not shown: no room left)\n\n"));
            continue;
        }
        let shown_chars = body.chars().count();
        let whole = shown_chars == total_chars && masked == skill.text;
        let kind = if skill.project { "project skill, " } else { "" };
        let heading = if shown_chars == total_chars {
            if whole {
                format!("### Skill {name} ({kind}shown whole)\n")
            } else {
                format!("### Skill {name} ({kind}secrets masked: patch it with a diff only)\n")
            }
        } else {
            format!(
                "### Skill {name} ({kind}cut: first {shown_chars} of {total_chars} characters; patch it with a diff only)\n"
            )
        };
        if w.push(&format!("{heading}{open}{body}{close}")) {
            shown.insert(
                skill.name.clone(),
                ShownSkill {
                    whole,
                    fingerprint: skill.fingerprint.clone(),
                },
            );
        }
    }
}

fn write_memory(w: &mut Writer, file: &MemoryFile) {
    let name = file.target.file_name();
    let target = file.target.name();
    if !file.enabled {
        w.push(&format!(
            "### {name} (target \"{target}\"): off, takes no writes\n\n"
        ));
        return;
    }
    if !w.push(&format!(
        "### {name} (target \"{target}\", {} characters)\n",
        one_line(&file.usage, 40)
    )) {
        return;
    }
    if file.entries.is_empty() {
        w.push("(empty)\n\n");
        return;
    }
    // Room is kept for the closing line so a cut is always said.
    const LEFT_OUT_LINE: usize = 64;
    let mut left_out = 0usize;
    for entry in &file.entries {
        let block = format!(
            "<entry>{}</entry>\n",
            neutralise(&redact(entry, Source::Output))
        );
        if left_out > 0 || units(&block) + LEFT_OUT_LINE * ASCII_UNITS > w.room() {
            left_out += 1;
            continue;
        }
        w.push(&block);
    }
    if left_out > 0 {
        w.push(&format!("({left_out} more entries not shown)\n"));
    }
    w.push("\n");
}

/// The longest prefix of `text` with at most `max_chars` characters costing at most `max_units`.
fn cut_to(text: &str, max_chars: usize, max_units: usize) -> String {
    let (mut end, mut used) = (0usize, 0usize);
    for (count, (i, c)) in text.char_indices().enumerate() {
        used += char_units(c);
        if count >= max_chars || used > max_units {
            break;
        }
        end = i + c.len_utf8();
    }
    text[..end].to_string()
}

/// `text` on one line, at most `max` characters (for headings and attributes).
fn one_line(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() || c == '"' { ' ' } else { c })
        .take(max)
        .collect()
}

/// Untrusted text can't close the tag it sits in, or open one, and fake a section of its own:
/// the `<` of the packet's own tags becomes `&lt;` (visible, unlike a zero-width break, which the
/// threat scan would flag if the model copied it into a write).
fn neutralise(text: &str) -> String {
    let mut out = text.to_string();
    for tag in [
        "</excerpt",
        "</skill",
        "</entry",
        "<excerpt",
        "<skill",
        "<entry",
    ] {
        if out.contains(tag) {
            out = out.replace(tag, &tag.replacen('<', "&lt;", 1));
        }
    }
    out
}
