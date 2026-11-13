//! What the curator's one-request jobs may answer, and what becomes of it.
//!
//! The reply is read the way the evidence review's is (`review_once/contract.rs`): the JSON object
//! may sit in a code fence or a sentence, anything else is **malformed** and earns the one repair
//! request, and after that the job gives up. A reply that reads is then **validated against the
//! job**: the model may only name skills the job listed, and each part is judged on its own
//! (an archive entry that fails is dropped with its reason while the merge it belongs to stands;
//! a merge whose text fails is dropped whole, with every archive that rested on it).
//!
//! The model's output is untrusted (the skills it read may carry an injection): nothing it says is
//! executed. The most it can do is propose the two operations [`Proposal`] holds, on skills the job
//! showed it, and those still go through the skill store's own validation and threat scan, the
//! staging area's diff for the user, and (on approval) the same checks again.

use hermes_tools::provenance::{self, SourceNote};
use hermes_tools::skill_manage::{ARCHIVE_ACTION, SkillOp};
use serde_json::Value;

use crate::review_once::contract::{MAX_PATCH, MAX_WHY, extract};

/// A reply's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Merge,
    Repair,
    None,
}

/// A reply, read but not yet judged.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub action: Action,
    pub keep: Option<String>,
    pub content: Option<String>,
    pub archive: Vec<String>,
    pub why: String,
}

const FIELDS: [&str; 5] = ["action", "keep", "content", "archive", "why"];

/// Reads a reply. `Err`: malformed (the one repair request is worth it).
pub fn parse(text: &str) -> Result<Reply, String> {
    let value = extract(text)?;
    let object = value.as_object().ok_or("the reply is not an object")?;
    if let Some(unknown) = object.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(format!(
            "unknown field '{}'",
            unknown.chars().take(40).collect::<String>()
        ));
    }
    let string = |key: &str, max: usize| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) if s.chars().count() > max => Err(format!(
                "'{key}' is {} characters (limit {max})",
                s.chars().count()
            )),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("'{key}' must be a string")),
        }
    };
    let action = match string("action", 20)?.as_deref() {
        Some("merge") => Action::Merge,
        Some("repair") => Action::Repair,
        Some("none") => Action::None,
        Some(other) => {
            return Err(format!(
                "unknown action '{}'",
                other.chars().take(20).collect::<String>()
            ));
        }
        None => return Err("'action' is missing".into()),
    };
    let archive = match object.get("archive") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(name) => Ok(name.clone()),
                _ => Err("'archive' must be a list of names".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("'archive' must be a list of names".into()),
    };
    Ok(Reply {
        action,
        keep: string("keep", 200)?,
        content: string("content", MAX_PATCH)?,
        archive,
        why: string("why", MAX_WHY)?.unwrap_or_default(),
    })
}

/// What a job asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum JobKind {
    /// Merge these skills (sorted names, at least two).
    Merge(Vec<String>),
    /// Repair this skill.
    Repair(String),
}

/// A reply judged against its job: the operations to propose, and what was refused and why.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proposal {
    /// `edit` of the kept skill, then `archive` of each absorbed one; empty when nothing is
    /// proposed.
    pub ops: Vec<SkillOp>,
    /// The kept (or repaired) skill.
    pub keep: Option<String>,
    /// The skills that move to the archive.
    pub archived: Vec<String>,
    pub why: String,
    /// One line per refused part.
    pub rejected: Vec<String>,
}

impl Proposal {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// What a skill's current text says about where it came from (the `sources` of its front matter).
pub struct Known<'a> {
    pub name: &'a str,
    pub skill_md: &'a str,
    /// The folder holds more than SKILL.md: the skill may be kept, never archived.
    pub own_files: bool,
}

/// Judges `reply` against `kind`. `known` are the job's skills with their current text (the
/// merged skill's `sources` is the union of theirs, plus this run's entry).
pub fn judge(reply: &Reply, kind: &JobKind, known: &[Known<'_>]) -> Proposal {
    let mut out = Proposal {
        why: reply.why.clone(),
        ..Default::default()
    };
    let names: Vec<&str> = match kind {
        JobKind::Merge(names) => names.iter().map(String::as_str).collect(),
        JobKind::Repair(name) => vec![name.as_str()],
    };
    match (reply.action, kind) {
        (Action::None, _) => return out,
        (Action::Merge, JobKind::Repair(_)) => {
            out.rejected
                .push("a merge answer to a repair job: refused".into());
            return out;
        }
        (Action::Repair, JobKind::Merge(_)) => {
            out.rejected
                .push("a repair answer to a merge job: refused".into());
            return out;
        }
        _ => {}
    }
    let Some(keep) = reply.keep.as_deref().map(str::trim) else {
        out.rejected.push("'keep' is missing".into());
        return out;
    };
    if !names.contains(&keep) {
        out.rejected.push(format!(
            "'{}' is not a skill of this job (only {} may be kept)",
            show(keep),
            names.join(", ")
        ));
        return out;
    }
    let Some(content) = reply.content.as_deref() else {
        out.rejected.push("'content' is missing".into());
        return out;
    };
    // The text must be a SKILL.md for `keep`: front matter naming it.
    let (frontmatter, _) = hermes_tools::skills::parse_frontmatter(content);
    let named = frontmatter
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim);
    if named != Some(keep) {
        out.rejected.push(format!(
            "the text's front matter must say `name: {keep}`"
        ));
        return out;
    }
    // Archives: each on its own.
    let mut archive: Vec<String> = Vec::new();
    for entry in &reply.archive {
        let name = entry.trim();
        if reply.action == Action::Repair {
            out.rejected
                .push(format!("a repair archives nothing: '{}' kept", show(name)));
        } else if name == keep {
            out.rejected
                .push(format!("'{}' is the skill kept and can't be archived", show(name)));
        } else if !names.contains(&name) {
            out.rejected.push(format!(
                "'{}' is not a skill of this job: not archived",
                show(name)
            ));
        } else if known.iter().any(|k| k.name == name && k.own_files) {
            out.rejected.push(format!(
                "'{}' has files of its own besides SKILL.md and can't be archived by a merge",
                show(name)
            ));
        } else if archive.iter().any(|a| a == name) {
            // A repeat: nothing to say.
        } else {
            archive.push(name.to_string());
        }
    }
    if reply.action == Action::Merge && archive.is_empty() {
        out.rejected
            .push("the merge archives no skill of the job: nothing merged".into());
        return out;
    }
    // Provenance: the union of every skill's `sources`, then this run's own.
    let run = SourceNote::new("curator", &run_reason(reply, kind, keep, &archive));
    let content = match kind {
        JobKind::Merge(_) => match merged_sources(content, known) {
            Ok(text) => text,
            Err(error) => {
                out.rejected.push(error);
                return out;
            }
        },
        // A repair keeps the sources the file has (the store does that for a write that isn't the
        // user's own) and adds this run's.
        JobKind::Repair(_) => content.to_string(),
    };
    out.ops.push(SkillOp {
        action: "edit".into(),
        name: keep.to_string(),
        content: Some(content),
        source: Some(run),
        // Our own code set `sources` (the union above): the store must not put the old list back.
        own_hand: matches!(kind, JobKind::Merge(_)),
        ..Default::default()
    });
    for name in &archive {
        out.ops.push(SkillOp {
            action: ARCHIVE_ACTION.into(),
            name: name.clone(),
            absorbed_into: Some(keep.to_string()),
            ..Default::default()
        });
    }
    out.keep = Some(keep.to_string());
    out.archived = archive;
    out
}

fn show(name: &str) -> String {
    name.chars().take(60).collect()
}

fn run_reason(reply: &Reply, kind: &JobKind, keep: &str, archive: &[String]) -> String {
    let what = match kind {
        JobKind::Merge(_) => format!("merged {} into {keep}", archive.join(", ")),
        JobKind::Repair(_) => format!("repaired after corrections: {keep}"),
    };
    if reply.why.trim().is_empty() {
        what
    } else {
        format!("{what}: {}", reply.why.trim())
    }
}

/// `content` with `sources` replaced by the union of the job's skills' (oldest first, no repeats,
/// the newest [`provenance::MAX_SOURCES`] kept by `append_source`). `Err`: the text can't be given
/// a `sources` list safely.
fn merged_sources(content: &str, known: &[Known<'_>]) -> Result<String, String> {
    // Whatever the model wrote for `sources` goes: the text as a new skill has none.
    let mut text = match provenance::keep_sources(None, content) {
        Ok(Some(stripped)) => stripped,
        Ok(None) => content.to_string(),
        Err(error) => return Err(error),
    };
    let mut seen: Vec<SourceNote> = Vec::new();
    for skill in known {
        for note in provenance::sources_of(skill.skill_md) {
            if seen
                .iter()
                .any(|s| s.session == note.session && s.why == note.why)
            {
                continue;
            }
            if let Some(with) = provenance::append_source(&text, &note) {
                text = with;
            }
            seen.push(note);
        }
    }
    Ok(text)
}
