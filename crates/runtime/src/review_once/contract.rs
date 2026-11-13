//! The evidence review's output contract: the JSON schema sent as `response_format`, reading a
//! reply (with or without the schema: a code fence or a sentence around the object is tolerated,
//! anything else is malformed and earns the one repair request), and strict per-operation
//! validation. The model's output is untrusted (the evidence it read may carry a prompt
//! injection), so every field is checked here, before anything touches a store; the stores then
//! apply their own limits and threat scans on top (`apply.rs`).
//!
//! Malformed (the reply as a whole): not one JSON object, or `memory`/`skills` present but not
//! a list. Rejected (one operation, the others still apply): not an object, an unknown field, an
//! unknown `op`/`target`, a wrong type, a missing or superfluous text, an oversize text, a skill
//! name the skill rules refuse, a second operation on the same skill, more operations than
//! [`MAX_MEMORY_OPS`] / [`MAX_SKILL_OPS`].

use hermes_memory::Target;
use serde_json::{Value, json};

/// Memory operations one review may make (the rest are rejected).
pub const MAX_MEMORY_OPS: usize = 8;
/// Skill operations one review may make.
pub const MAX_SKILL_OPS: usize = 4;
/// One memory entry's text (and `old`), in characters. The stores hold 2,200 and 1,375 characters
/// in all, so a longer entry is a paste, not a fact.
pub const MAX_MEMORY_TEXT: usize = 1_000;
/// A `why`, in characters.
pub const MAX_WHY: usize = 500;
/// A skill `patch` (a diff or a whole SKILL.md), in characters: SKILL.md's own limit.
pub const MAX_PATCH: usize = hermes_tools::skill_manage::MAX_SKILL_CONTENT_CHARS;
/// A reply longer than this (bytes) is not read at all.
pub const MAX_REPLY_BYTES: usize = 512 * 1024;

const MEMORY_FIELDS: [&str; 5] = ["op", "target", "old", "text", "why"];
const SKILL_FIELDS: [&str; 5] = ["op", "name", "patch", "scope", "why"];

/// A memory operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAction {
    Add,
    Replace,
    Remove,
}

impl MemoryAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Replace => "replace",
            Self::Remove => "remove",
        }
    }
}

/// A validated memory operation.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryChange {
    /// Its place in the reply's `memory` list (0-based), for the log.
    pub index: usize,
    pub action: MemoryAction,
    pub target: Target,
    pub old: Option<String>,
    pub text: Option<String>,
    pub why: String,
}

/// A skill operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillAction {
    Create,
    Patch,
}

impl SkillAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Patch => "patch",
        }
    }
}

/// A validated skill operation (`none` is not kept).
#[derive(Clone, Debug, PartialEq)]
pub struct SkillChange {
    pub index: usize,
    pub action: SkillAction,
    pub name: String,
    /// A unified diff or a whole SKILL.md.
    pub patch: String,
    /// Where a `create` puts the skill (the reply's `scope`); `None`: the reply didn't say, and
    /// the review picks by the evidence (`review_once::evidence_scope`). Always `None` for a patch.
    pub scope: Option<hermes_tools::skill_manage::SkillScope>,
    pub why: String,
}

/// An operation refused by validation, with why.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejection {
    /// `memory`, `skills`, or `reply` (an unknown top-level field).
    pub section: &'static str,
    pub index: usize,
    pub reason: String,
}

/// A reply read and validated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub memory: Vec<MemoryChange>,
    pub skills: Vec<SkillChange>,
    pub rejected: Vec<Rejection>,
    /// `{"op": "none"}` skill entries.
    pub nones: usize,
}

impl Parsed {
    /// Whether the reply asks for no change at all (a normal answer).
    pub fn is_empty(&self) -> bool {
        self.memory.is_empty() && self.skills.is_empty()
    }
}

/// The schema of a reply, strict in OpenAI's sense (every field required, nothing else
/// allowed; a field an operation doesn't use is `null`).
pub fn schema() -> Value {
    let nullable = json!({"type": ["string", "null"]});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["memory", "skills"],
        "properties": {
            "memory": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": MEMORY_FIELDS,
                    "properties": {
                        "op": {"type": "string", "enum": ["add", "replace", "remove"]},
                        "target": {"type": "string", "enum": ["memory", "user"]},
                        "old": nullable,
                        "text": nullable,
                        "why": {"type": "string"}
                    }
                }
            },
            "skills": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": SKILL_FIELDS,
                    "properties": {
                        "op": {"type": "string", "enum": ["create", "patch", "none"]},
                        "name": nullable,
                        "patch": nullable,
                        "scope": {"type": ["string", "null"], "enum": ["global", "project", null]},
                        "why": {"type": "string"}
                    }
                }
            }
        }
    })
}

/// The request field that asks for [`schema`] (OpenAI's structured outputs; OpenRouter, vLLM,
/// LM Studio, llama.cpp's server and Ollama take the same field).
pub fn response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {"name": "review_ops", "strict": true, "schema": schema()}
    })
}

/// The JSON object in a reply: the whole text, or inside one code fence, or from its first `{`
/// to its last `}` (a sentence around it). `Err` says why there is none.
fn extract(text: &str) -> Result<Value, String> {
    if text.len() > MAX_REPLY_BYTES {
        return Err(format!(
            "the reply is {} KB, more than the {} KB read",
            text.len() / 1024,
            MAX_REPLY_BYTES / 1024
        ));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("the reply was empty".into());
    }
    let mut candidates: Vec<&str> = vec![trimmed];
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let body_start = after.find('\n').map_or(after.len(), |i| i + 1);
        if let Some(end) = after[body_start..].find("```") {
            candidates.push(after[body_start..body_start + end].trim());
        }
    }
    if let (Some(open), Some(close)) = (trimmed.find('{'), trimmed.rfind('}'))
        && open < close
    {
        candidates.push(&trimmed[open..=close]);
    }
    let mut last_error = String::new();
    for candidate in candidates {
        match serde_json::from_str::<Value>(candidate) {
            Ok(value) if value.is_object() => return Ok(value),
            Ok(_) => last_error = "the reply is JSON but not an object".into(),
            Err(e) => last_error = format!("the reply is not valid JSON ({e})"),
        }
    }
    Err(last_error)
}

/// Reads a reply. `Err`: malformed as a whole (worth the one repair request). `Ok`: the valid
/// operations, and each invalid one with its reason.
pub fn parse(text: &str) -> Result<Parsed, String> {
    let value = extract(text)?;
    let object = value.as_object().ok_or("the reply is not an object")?;
    let mut parsed = Parsed::default();
    for (key, _) in object {
        if key != "memory" && key != "skills" {
            parsed.rejected.push(Rejection {
                section: "reply",
                index: 0,
                reason: format!("unknown field '{}' ignored", cut(key, 40)),
            });
        }
    }
    let list = |key: &str| -> Result<Vec<Value>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => Ok(items.clone()),
            Some(_) => Err(format!("'{key}' must be a list")),
        }
    };
    let memory = list("memory")?;
    let skills = list("skills")?;
    for (index, item) in memory.iter().enumerate() {
        if index >= MAX_MEMORY_OPS {
            parsed.rejected.push(reject(
                "memory",
                index,
                format!("more than {MAX_MEMORY_OPS} memory operations in one review"),
            ));
            continue;
        }
        match memory_change(index, item) {
            Ok(change) => parsed.memory.push(change),
            Err(reason) => parsed.rejected.push(reject("memory", index, reason)),
        }
    }
    let mut kept = 0usize;
    for (index, item) in skills.iter().enumerate() {
        match skill_change(index, item) {
            Ok(None) => parsed.nones += 1,
            Ok(Some(_)) if kept >= MAX_SKILL_OPS => parsed.rejected.push(reject(
                "skills",
                index,
                format!("more than {MAX_SKILL_OPS} skill operations in one review"),
            )),
            Ok(Some(change)) if parsed.skills.iter().any(|c| c.name == change.name) => {
                parsed.rejected.push(reject(
                    "skills",
                    index,
                    format!("a second operation on skill '{}'", change.name),
                ));
            }
            Ok(Some(change)) => {
                kept += 1;
                parsed.skills.push(change);
            }
            Err(reason) => parsed.rejected.push(reject("skills", index, reason)),
        }
    }
    Ok(parsed)
}

fn reject(section: &'static str, index: usize, reason: String) -> Rejection {
    Rejection {
        section,
        index,
        reason,
    }
}

/// The first `max` characters of `text`, for messages.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// An object with only `allowed` fields.
fn fields<'a>(
    item: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = item.as_object().ok_or("not an object")?;
    if let Some(unknown) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unknown field '{}'", cut(unknown, 40)));
    }
    Ok(object)
}

/// A string field: `None` when absent, null or blank; an error for any other type or when it is
/// longer than `max` characters.
fn text_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let len = s.chars().count();
            if len > max {
                return Err(format!("'{key}' is {len} characters (limit {max})"));
            }
            Ok(Some(s.clone()))
        }
        Some(_) => Err(format!("'{key}' must be a string")),
    }
}

fn memory_change(index: usize, item: &Value) -> Result<MemoryChange, String> {
    let object = fields(item, &MEMORY_FIELDS)?;
    let op = text_field(object, "op", 20)?.ok_or("'op' is missing")?;
    let action = match op.as_str() {
        "add" => MemoryAction::Add,
        "replace" => MemoryAction::Replace,
        "remove" => MemoryAction::Remove,
        other => return Err(format!("unknown op '{}'", cut(other, 20))),
    };
    let target_name = text_field(object, "target", 20)?.ok_or("'target' is missing")?;
    let target = match target_name.as_str() {
        "memory" => Target::Memory,
        "user" => Target::User,
        other => return Err(format!("unknown target '{}'", cut(other, 20))),
    };
    let old = text_field(object, "old", MAX_MEMORY_TEXT)?;
    let text = text_field(object, "text", MAX_MEMORY_TEXT)?;
    let why = text_field(object, "why", MAX_WHY)?.unwrap_or_default();
    match action {
        MemoryAction::Add if text.is_none() => return Err("add needs 'text'".into()),
        MemoryAction::Add if old.is_some() => return Err("add takes no 'old'".into()),
        MemoryAction::Replace if old.is_none() || text.is_none() => {
            return Err("replace needs 'old' and 'text'".into());
        }
        MemoryAction::Remove if old.is_none() => return Err("remove needs 'old'".into()),
        MemoryAction::Remove if text.is_some() => return Err("remove takes no 'text'".into()),
        _ => {}
    }
    Ok(MemoryChange {
        index,
        action,
        target,
        old,
        text,
        why,
    })
}

/// `Ok(None)` for `{"op": "none"}`.
fn skill_change(index: usize, item: &Value) -> Result<Option<SkillChange>, String> {
    let object = fields(item, &SKILL_FIELDS)?;
    let op = text_field(object, "op", 20)?.ok_or("'op' is missing")?;
    let action = match op.as_str() {
        "none" => return Ok(None),
        "create" => SkillAction::Create,
        "patch" => SkillAction::Patch,
        other => return Err(format!("unknown op '{}'", cut(other, 20))),
    };
    let name = text_field(object, "name", 200)?.ok_or("'name' is missing")?;
    if let Some(error) = hermes_tools::skill_manage::validate_skill_name(&name) {
        return Err(error);
    }
    let patch = text_field(object, "patch", MAX_PATCH)?.ok_or("'patch' is missing")?;
    let why = text_field(object, "why", MAX_WHY)?.unwrap_or_default();
    // A reply from before `scope` existed has none (null is "decide for me"). Only a create has
    // a place to choose: on a patch the field is ignored.
    let scope = match (text_field(object, "scope", 20)?.as_deref(), action) {
        (_, SkillAction::Patch) | (None, _) => None,
        (Some("global"), _) => Some(hermes_tools::skill_manage::SkillScope::Global),
        (Some("project"), _) => Some(hermes_tools::skill_manage::SkillScope::Project),
        (Some(other), _) => return Err(format!("unknown scope '{}'", cut(other, 20))),
    };
    Ok(Some(SkillChange {
        index,
        action,
        name,
        patch,
        scope,
        why,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_found_in_a_fence_or_a_sentence_and_nothing_else() {
        let body = r#"{"memory": [], "skills": []}"#;
        assert!(parse(body).unwrap().is_empty());
        assert!(parse(&format!("```json\n{body}\n```")).is_ok());
        assert!(parse(&format!("Here you go:\n{body}\nThanks.")).is_ok());
        assert!(parse("Nothing to save.").is_err());
        assert!(parse("").is_err());
        assert!(parse("[1, 2]").is_err());
        assert!(parse(r#"{"memory": {"op": "add"}}"#).is_err(), "not a list");
        assert!(parse(r#"{"memory": [], "skills": [}"#).is_err());
        assert!(parse(&"x".repeat(MAX_REPLY_BYTES + 1)).is_err());
        // Absent lists are empty lists.
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn each_bad_operation_is_rejected_alone() {
        let reply = json!({
            "memory": [
                {"op": "add", "target": "user", "old": null, "text": "Prefers pnpm.", "why": "signal 2"},
                {"op": "delete", "target": "user", "old": "x", "text": null, "why": ""},
                {"op": "add", "target": "notes", "old": null, "text": "x", "why": ""},
                {"op": "add", "target": "memory", "old": null, "text": 42, "why": ""},
                {"op": "add", "target": "memory", "old": null, "text": "x", "why": "", "tier": "core"},
                {"op": "replace", "target": "memory", "old": "x", "text": null, "why": ""},
                {"op": "remove", "target": "memory", "old": "x", "text": "y", "why": ""},
                "add this"
            ],
            "skills": [
                {"op": "create", "name": "Bad Name", "patch": "---\n", "why": ""},
                {"op": "create", "name": "../evil", "patch": "---\n", "why": ""},
                {"op": "patch", "name": "deploy", "patch": "@@ -1 +1 @@\n-a\n+b\n", "why": ""},
                {"op": "patch", "name": "deploy", "patch": "@@ -1 +1 @@\n-a\n+c\n", "why": ""},
                {"op": "none", "name": null, "patch": null, "why": ""},
                {"op": "delete", "name": "deploy", "patch": null, "why": ""}
            ],
            "extra": true
        });
        let parsed = parse(&reply.to_string()).unwrap();
        assert_eq!(parsed.memory.len(), 1);
        assert_eq!(parsed.memory[0].text.as_deref(), Some("Prefers pnpm."));
        assert_eq!(parsed.memory[0].target, Target::User);
        assert_eq!(parsed.skills.len(), 1);
        assert_eq!(parsed.skills[0].name, "deploy");
        assert_eq!(parsed.nones, 1);
        let reasons: Vec<String> = parsed
            .rejected
            .iter()
            .map(|r| format!("{}[{}]: {}", r.section, r.index, r.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![
                "reply[0]: unknown field 'extra' ignored",
                "memory[1]: unknown op 'delete'",
                "memory[2]: unknown target 'notes'",
                "memory[3]: 'text' must be a string",
                "memory[4]: unknown field 'tier'",
                "memory[5]: replace needs 'old' and 'text'",
                "memory[6]: remove takes no 'text'",
                "memory[7]: not an object",
                "skills[0]: Invalid skill name 'Bad Name'. Use lowercase letters, numbers, hyphens, dots, and underscores. Must start with a letter or digit.",
                "skills[1]: Invalid skill name '../evil'. Use lowercase letters, numbers, hyphens, dots, and underscores. Must start with a letter or digit.",
                "skills[3]: a second operation on skill 'deploy'",
                "skills[5]: unknown op 'delete'",
            ]
        );
    }

    #[test]
    fn oversize_texts_and_too_many_operations_are_rejected() {
        let long = "x".repeat(MAX_MEMORY_TEXT + 1);
        let mut memory: Vec<Value> =
            vec![json!({"op": "add", "target": "memory", "old": null, "text": long, "why": ""})];
        for i in 0..MAX_MEMORY_OPS {
            memory.push(json!({"op": "add", "target": "memory", "old": null, "text": format!("fact {i}"), "why": ""}));
        }
        let skills: Vec<Value> = (0..MAX_SKILL_OPS + 1)
            .map(|i| json!({"op": "create", "name": format!("s{i}"), "patch": "body", "why": ""}))
            .collect();
        let parsed = parse(&json!({"memory": memory, "skills": skills}).to_string()).unwrap();
        assert_eq!(parsed.memory.len(), MAX_MEMORY_OPS - 1);
        assert_eq!(parsed.skills.len(), MAX_SKILL_OPS);
        assert!(parsed.rejected[0].reason.contains("limit 1000"));
        assert_eq!(parsed.rejected.len(), 3);
        let why = "w".repeat(MAX_WHY + 1);
        let parsed = parse(&json!({"memory": [{"op": "add", "target": "memory", "old": null, "text": "x", "why": why}]}).to_string()).unwrap();
        assert!(parsed.memory.is_empty() && parsed.rejected.len() == 1);
    }

    #[test]
    fn a_skills_scope_is_global_project_or_left_to_the_review() {
        use hermes_tools::skill_manage::SkillScope;
        let item = |op: &str, scope: Value| {
            json!({"op": op, "name": "deploy", "patch": "body", "scope": scope, "why": ""})
        };
        let reply = |items: Vec<Value>| json!({"memory": [], "skills": items}).to_string();
        let one = |op: &str, scope: Value| {
            let parsed = parse(&reply(vec![item(op, scope)])).unwrap();
            assert!(parsed.rejected.is_empty(), "{:?}", parsed.rejected);
            parsed.skills[0].scope
        };
        assert_eq!(one("create", json!("project")), Some(SkillScope::Project));
        assert_eq!(one("create", json!("global")), Some(SkillScope::Global));
        assert_eq!(one("create", Value::Null), None);
        // A patch has no place to choose: the field is ignored.
        assert_eq!(one("patch", json!("project")), None);
        // Replies from before the field existed still read.
        let old = json!({"memory": [], "skills": [{"op": "create", "name": "deploy", "patch": "b", "why": ""}]});
        assert_eq!(parse(&old.to_string()).unwrap().skills[0].scope, None);
        // Anything else is refused alone.
        for bad in [json!("everywhere"), json!(3), json!("PROJECT")] {
            let parsed = parse(&reply(vec![item("create", bad)])).unwrap();
            assert!(parsed.skills.is_empty() && parsed.rejected.len() == 1, "{parsed:?}");
        }
    }

    #[test]
    fn the_schema_is_strict() {
        let format = response_format();
        assert_eq!(format["json_schema"]["strict"], true);
        let schema = &format["json_schema"]["schema"];
        assert_eq!(schema["additionalProperties"], false);
        for list in ["memory", "skills"] {
            let item = &schema["properties"][list]["items"];
            assert_eq!(item["additionalProperties"], false);
            let required: Vec<&str> = item["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let properties: Vec<&str> = item["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            let mut sorted = required.clone();
            sorted.sort();
            let mut props = properties.clone();
            props.sort();
            assert_eq!(sorted, props, "strict mode: every property is required");
        }
    }
}
