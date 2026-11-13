//! Skill provenance: where a skill came from, kept in its own SKILL.md frontmatter as
//!
//! ```yaml
//! sources:
//!   - session: "abc123"
//!     date: "2026-11-13"
//!     why: "the user corrected the deploy order twice"
//! ```
//!
//! Not in Hermes (Phase 8 of `docs/backend/PHASES.md`; `PORTING.md` marks `skill_manage` as
//! `diverged`). Every write a review makes, and every write the agent makes in a session, appends
//! one entry; later edits keep the list.
//!
//! ## Text edit, not a YAML round trip
//!
//! A skill is often written by the user, with their comments, key order and quoting. Parsing the
//! frontmatter and writing it back would reformat all of it, so the frontmatter is edited as
//! **text**: the only lines this module adds, removes or changes are those of the `sources:`
//! block. Everything else (the body, the other keys, comments, the byte-order mark, the line
//! endings) stays byte for byte. The new item copies the indentation of the existing ones and the
//! file's line ending.
//!
//! After the edit the result is parsed and checked: every other key must be equal to what it was,
//! and `sources` must be the list it should be. A file this module can't edit safely (no
//! frontmatter, a `sources` written as a flow list or a scalar, YAML that does not parse) is left
//! as it is: the entry is skipped, the write itself still goes through.
//!
//! ## Untrusted text
//!
//! `why` is written by a model from a conversation that may hold anything. It is cut to one line
//! of at most [`MAX_WHY`] characters with no control or invisible characters, and it goes through
//! the same threat scan as the rest of the skill (a hit replaces it by a fixed phrase, so a
//! poisoned `why` can neither block the write nor ride along). The list keeps the newest
//! [`MAX_SOURCES`] entries (the oldest items go).

use serde::{Deserialize, Serialize};

/// Entries a skill keeps (the oldest go first).
pub const MAX_SOURCES: usize = 20;
/// Characters of `why`.
pub const MAX_WHY: usize = 160;
const MAX_SESSION: usize = 64;
const WITHHELD: &str = "(text withheld: it failed the security scan)";

/// One provenance entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNote {
    pub session: String,
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub why: String,
}

impl SourceNote {
    /// A note for `session`, dated now, with `why` cleaned ([`clean_why`]).
    pub fn new(session: &str, why: &str) -> Self {
        Self::dated(
            session,
            &chrono::Utc::now().format("%Y-%m-%d").to_string(),
            why,
        )
    }

    /// [`Self::new`] with the date given.
    pub fn dated(session: &str, date: &str, why: &str) -> Self {
        let session: String = session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(MAX_SESSION)
            .collect();
        let date: String = date
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .take(10)
            .collect();
        Self {
            session: if session.is_empty() {
                "unknown".into()
            } else {
                session
            },
            date,
            why: clean_why(why),
        }
    }
}

/// `why` as it may be written: one line, at most [`MAX_WHY`] characters, no control or invisible
/// characters, and nothing the threat scan blocks.
pub fn clean_why(raw: &str) -> String {
    let spaced: String = raw
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut text = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MAX_WHY {
        text = text.chars().take(MAX_WHY - 3).collect::<String>();
        text.push_str("...");
    }
    let scope = hermes_memory::threats::Scope::All;
    if hermes_memory::threats::first_threat_message(&text, scope).is_some() {
        return WITHHELD.into();
    }
    text
}

/// Zero-width, bidirectional-control and similar characters that hide text.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
}

/// The entries a SKILL.md lists (none when it has no frontmatter or no `sources`).
pub fn sources_of(text: &str) -> Vec<SourceNote> {
    let Some(lines) = Frontmatter::split(text) else {
        return Vec::new();
    };
    let Some(map) = lines.mapping() else {
        return Vec::new();
    };
    map.get("sources")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| serde_yaml::from_value::<SourceNote>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `text` with `note` appended to its frontmatter's `sources`. `None` when nothing is to change:
/// the entry is there already (same session and reason), or the file can't be edited safely (see
/// the module doc).
pub fn append_source(text: &str, note: &SourceNote) -> Option<String> {
    let fm = Frontmatter::split(text)?;
    let before = fm.mapping()?;
    let old_len = match before.get("sources") {
        None | Some(serde_yaml::Value::Null) => 0,
        Some(serde_yaml::Value::Sequence(seq)) => seq.len(),
        Some(_) => return None,
    };
    if sources_of(text)
        .iter()
        .any(|s| s.session == note.session && s.why == note.why)
    {
        return None;
    }
    let eol = fm.eol();
    let item = |indent: &str| {
        [
            format!("{indent}- session: {}", quote(&note.session)),
            format!("{indent}  date: {}", quote(&note.date)),
            format!("{indent}  why: {}", quote(&note.why)),
        ]
        .map(|line| line + eol)
        .concat()
    };
    let mut lines: Vec<String> = fm.lines.iter().map(|l| l.to_string()).collect();
    let mut dropped = 0;
    match fm.block() {
        Block::Missing => {
            // Before the closing `---`.
            lines.insert(fm.close, format!("sources:{eol}{}", item("  ")));
        }
        Block::Found(b) => {
            let at = if b.inline_empty {
                // `sources: []` becomes the block key (a trailing comment stays).
                lines[b.key] = b.key_without_value(&fm, eol);
                b.key + 1
            } else {
                b.end + 1
            };
            // Over the cap: the oldest items go.
            let excess = (b.items.len() + 1).saturating_sub(MAX_SOURCES);
            let mut insert_at = at;
            if excess > 0 && excess <= b.items.len() {
                let from = b.items[0];
                let to = b.items.get(excess).copied().unwrap_or(b.end + 1);
                lines.drain(from..to);
                insert_at = at - (to - from);
                dropped = excess;
            }
            lines.insert(insert_at, item(&b.indent));
        }
        Block::Unsupported => return None,
    }
    let result = rebuild(text, &fm, &lines);
    // The check: other keys equal, `sources` the list it should be.
    let after = Frontmatter::split(&result)?;
    let after_map = after.mapping()?;
    let mut expected = before.clone();
    expected.remove("sources");
    let mut got = after_map.clone();
    let list = got.remove("sources");
    let list = list.as_ref().and_then(|v| v.as_sequence())?;
    let last = list.last()?;
    (expected == got
        && list.len() == old_len + 1 - dropped
        && serde_yaml::from_value::<SourceNote>(last.clone())
            .ok()
            .as_ref()
            == Some(note))
    .then_some(result)
}

/// Why a write that would change `sources` is refused (see [`keep_sources`]).
pub const SOURCES_REFUSAL: &str = "SKILL.md's `sources:` list (where the skill came from) is \
    kept by the app, and this write changes it in a way that can't be undone safely. Leave the \
    `sources:` lines exactly as they are.";

/// A write of SKILL.md (`new`) that isn't the user's own can't decide `sources`: the result
/// keeps the list of the file it replaces (`before`; none for a new skill), whatever the writer
/// put there (dropped the block, edited an entry, forged one in any YAML form).
///
/// `Ok(None)`: nothing to change (the same list, or a `new`/`before` this module can't read, which
/// the frontmatter validation deals with). `Err`: `new` changes `sources` and putting the old
/// list back can't be done safely (the write must be refused).
pub fn keep_sources(before: Option<&str>, new: &str) -> Result<Option<String>, String> {
    let refuse = || Err(SOURCES_REFUSAL.to_string());
    // What `sources` must be, and the lines that say so.
    let (want, old_text) = match before {
        None => (None, None),
        Some(before) => {
            let Some(old_fm) = Frontmatter::split(before) else {
                return Ok(None);
            };
            let Some(old_map) = old_fm.mapping() else {
                return Ok(None);
            };
            match old_map.get("sources") {
                None => (None, None),
                Some(value) => {
                    let Some((key, end)) = old_fm.key_extent("sources") else {
                        return refuse();
                    };
                    (Some(value.clone()), Some(old_fm.lines[key..=end].to_vec()))
                }
            }
        }
    };
    let Some(new_fm) = Frontmatter::split(new) else {
        return Ok(None);
    };
    let Some(new_map) = new_fm.mapping() else {
        return Ok(None);
    };
    if new_map.get("sources") == want.as_ref() {
        return Ok(None);
    }
    let eol = new_fm.eol();
    let block: String = old_text
        .iter()
        .flatten()
        .map(|line| format!("{}{eol}", line.trim_end_matches(['\r', '\n'])))
        .collect();
    let mut lines: Vec<String> = new_fm.lines.iter().map(|l| l.to_string()).collect();
    if new_map.contains_key("sources") {
        let Some((key, end)) = new_fm.key_extent("sources") else {
            return refuse();
        };
        lines.splice(key..=end, [block]);
    } else {
        lines.insert(new_fm.close, block);
    }
    let result = rebuild(new, &new_fm, &lines);
    let Some(after) = Frontmatter::split(&result).and_then(|fm| fm.mapping()) else {
        return refuse();
    };
    let mut expected = new_map.clone();
    expected.remove("sources");
    let mut got = after.clone();
    let list = got.remove("sources");
    if expected == got && list == want {
        Ok(Some(result))
    } else {
        refuse()
    }
}

/// A YAML double-quoted scalar (JSON strings are valid ones).
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// The file with its frontmatter lines replaced by `lines`.
fn rebuild(text: &str, fm: &Frontmatter, lines: &[String]) -> String {
    let mut out = String::with_capacity(text.len() + 160);
    out.push_str(&text[..fm.start]);
    for line in lines {
        out.push_str(line);
    }
    out.push_str(&text[fm.end..]);
    out
}

// ---- reading the frontmatter as lines ------------------------------------------------------

/// The frontmatter's lines (opening `---` to closing `---`, each with its line ending).
struct Frontmatter<'a> {
    /// Byte offsets of the lines in the file.
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
    /// Index of the closing `---` line in `lines`.
    close: usize,
}

enum Block {
    /// No top-level `sources` key.
    Missing,
    Found(SourcesBlock),
    /// A `sources` this module won't edit (flow list, scalar, a mapping...).
    Unsupported,
}

struct SourcesBlock {
    /// Line of the `sources:` key, and of the last line of its block (the key's own line for an
    /// empty one).
    key: usize,
    end: usize,
    /// The indentation of the items (the existing ones', else two spaces).
    indent: String,
    /// Line index of each item's first line.
    items: Vec<usize>,
    /// `sources: []`.
    inline_empty: bool,
}

impl SourcesBlock {
    /// The key line with `[]` taken off (keeping a trailing comment).
    fn key_without_value(&self, fm: &Frontmatter, eol: &str) -> String {
        let line = fm.lines[self.key].trim_end_matches(['\r', '\n']);
        let after_colon = line.split_once(':').map(|(_, rest)| rest).unwrap_or("");
        let comment = after_colon
            .find('#')
            .map(|i| format!(" {}", &after_colon[i..]))
            .unwrap_or_default();
        format!("sources:{comment}{eol}")
    }
}

impl<'a> Frontmatter<'a> {
    fn split(text: &'a str) -> Option<Self> {
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut offset = bom;
        let mut lines: Vec<&str> = Vec::new();
        let mut close = None;
        for (i, line) in text[bom..].split_inclusive('\n').enumerate() {
            if i == 0 && line.trim_end() != "---" {
                return None;
            }
            lines.push(line);
            offset += line.len();
            if i > 0 && line.trim_end() == "---" {
                close = Some(i);
                break;
            }
        }
        Some(Self {
            start: bom,
            end: offset,
            lines,
            close: close?,
        })
    }

    /// The line ending of the file's first line.
    fn eol(&self) -> &'static str {
        if self.lines[0].ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// The frontmatter parsed; `None` when it doesn't parse or isn't a mapping.
    fn mapping(&self) -> Option<serde_yaml::Mapping> {
        let yaml: String = self.lines[1..self.close].concat();
        serde_yaml::from_str::<serde_yaml::Value>(&yaml)
            .ok()?
            .as_mapping()
            .cloned()
    }

    /// Whether line `i` is the top-level key `key` written plainly (`key:` at column 0).
    fn is_key_line(&self, i: usize, key: &str) -> bool {
        self.lines[i]
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start_matches([' ', '\t']).starts_with(':'))
    }

    /// The lines of the top-level `key`: its own and what follows that belongs to its value
    /// (indented lines, `-` items at column 0). Blank lines and column-0 comments after it belong
    /// to whatever comes next. `None`: not written plainly at column 0, or written twice.
    fn key_extent(&self, key: &str) -> Option<(usize, usize)> {
        let mut found = (1..self.close).filter(|&i| self.is_key_line(i, key));
        let start = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let mut end = start;
        for j in start + 1..self.close {
            let text = self.lines[j].trim_end_matches(['\r', '\n']);
            if text.trim().is_empty() {
                continue;
            }
            if !text.starts_with([' ', '\t', '-']) {
                break;
            }
            end = j;
        }
        Some((start, end))
    }

    fn block(&self) -> Block {
        let key_line = (1..self.close).find(|&i| self.is_key_line(i, "sources"));
        let Some(key) = key_line else {
            return Block::Missing;
        };
        let line = self.lines[key].trim_end_matches(['\r', '\n']);
        let value = line
            .split_once(':')
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .split('#')
            .next()
            .unwrap_or("")
            .trim();
        let inline_empty = value == "[]";
        if !value.is_empty() && !inline_empty {
            return Block::Unsupported;
        }
        // The block: what follows that is indented or a `-` item; trailing blanks and column-0
        // comments belong to whatever comes next.
        let mut end = key;
        let mut indent: Option<String> = None;
        let mut items = Vec::new();
        for j in key + 1..self.close {
            let text = self.lines[j].trim_end_matches(['\r', '\n']);
            if text.trim().is_empty() {
                continue;
            }
            let lead: String = text
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let rest = &text[lead.len()..];
            if lead.is_empty() && !rest.starts_with('-') {
                break;
            }
            if rest.starts_with('#') {
                if !lead.is_empty() {
                    end = end.max(j);
                }
                continue;
            }
            let is_dash = rest == "-" || rest.starts_with("- ") || rest.starts_with("-\t");
            if is_dash {
                let want = indent.get_or_insert_with(|| lead.clone());
                if *want == lead {
                    items.push(j);
                }
            } else if items.is_empty() {
                // Something other than a sequence under the key.
                return Block::Unsupported;
            }
            end = j;
        }
        if inline_empty && end != key {
            return Block::Unsupported;
        }
        Block::Found(SourcesBlock {
            key,
            end,
            indent: indent.unwrap_or_else(|| "  ".into()),
            items,
            inline_empty,
        })
    }
}

#[cfg(test)]
mod tests;
