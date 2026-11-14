//! Tool schemas sent to the model, embedded from `prompts/tools/` (see its README for how they
//! differ from Hermes's).
//!
//! `skill_manage`, `session_search`, `memory` and `delegate_task` are **diverged** from Hermes (Phase 11
//! CP11.1): short descriptions ending in a pointer to the bundled skill `hermes-tools-guide`, which keeps
//! the long text. Their byte budgets and a snapshot of their shape are tests below.

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

const SCHEMAS: &[(&str, &str)] = &[
    (
        "terminal",
        include_str!("../../../prompts/tools/terminal.json"),
    ),
    (
        "run_script",
        include_str!("../../../prompts/tools/run_script.json"),
    ),
    (
        "process_manage",
        include_str!("../../../prompts/tools/process_manage.json"),
    ),
    (
        "read_file",
        include_str!("../../../prompts/tools/read_file.json"),
    ),
    (
        "write_file",
        include_str!("../../../prompts/tools/write_file.json"),
    ),
    ("patch", include_str!("../../../prompts/tools/patch.json")),
    (
        "search_files",
        include_str!("../../../prompts/tools/search_files.json"),
    ),
    (
        "todo_list",
        include_str!("../../../prompts/tools/todo_list.json"),
    ),
    (
        "clarify",
        include_str!("../../../prompts/tools/clarify.json"),
    ),
    ("memory", include_str!("../../../prompts/tools/memory.json")),
    (
        "session_search",
        include_str!("../../../prompts/tools/session_search.json"),
    ),
    (
        "skills_list",
        include_str!("../../../prompts/tools/skills_list.json"),
    ),
    (
        "skill_view",
        include_str!("../../../prompts/tools/skill_view.json"),
    ),
    (
        "skill_manage",
        include_str!("../../../prompts/tools/skill_manage.json"),
    ),
    (
        "web_search",
        include_str!("../../../prompts/tools/web_search.json"),
    ),
    (
        "web_extract",
        include_str!("../../../prompts/tools/web_extract.json"),
    ),
    (
        "vision_analyze",
        include_str!("../../../prompts/tools/vision_analyze.json"),
    ),
    (
        "delegate_task",
        include_str!("../../../prompts/tools/delegate_task.json"),
    ),
];

/// A tool definition as stored: name, description and JSON-schema parameters.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Map<String, Value>,
}

pub fn tool_schema(name: &str) -> Result<ToolSchema> {
    let (_, json) = SCHEMAS
        .iter()
        .find(|(schema_name, _)| *schema_name == name)
        .with_context(|| format!("no schema for tool {name}"))?;
    let value: Value = serde_json::from_str(json)?;
    let description = value["description"]
        .as_str()
        .context("schema description")?
        .to_string();
    let Value::Object(parameters) = value["parameters"].clone() else {
        anyhow::bail!("schema parameters for {name} must be an object");
    };
    Ok(ToolSchema {
        name: name.to_string(),
        description,
        parameters,
    })
}

pub fn tool_names() -> impl Iterator<Item = &'static str> {
    SCHEMAS.iter().map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_parses_and_matches_its_name() {
        for name in tool_names() {
            let schema = tool_schema(name).unwrap();
            let value: Value =
                serde_json::from_str(SCHEMAS.iter().find(|(n, _)| *n == name).unwrap().1).unwrap();
            assert_eq!(value["name"], name);
            assert_eq!(schema.parameters["type"], "object");
        }
    }

    /// What the model is sent for one tool: the whole function entry, compact (the OpenAI shape).
    fn serialised(name: &str) -> String {
        let schema = tool_schema(name).unwrap();
        serde_json::json!({
            "type": "function",
            "function": {"name": schema.name, "description": schema.description, "parameters": schema.parameters},
        })
        .to_string()
    }

    /// Every `description` string in a schema, nested ones included (parameters and their items).
    fn description_bytes(value: &Value) -> usize {
        match value {
            Value::Object(map) => map
                .iter()
                .map(|(key, inner)| match (key.as_str(), inner) {
                    ("description", Value::String(text)) => text.len(),
                    _ => description_bytes(inner),
                })
                .sum(),
            Value::Array(items) => items.iter().map(description_bytes).sum(),
            _ => 0,
        }
    }

    /// `value` without its `description` strings: the shape a call must have.
    fn shape(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(key, inner)| !(key.as_str() == "description" && inner.is_string()))
                    .map(|(key, inner)| (key.clone(), shape(inner)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
            other => other.clone(),
        }
    }

    #[test]
    fn no_schema_names_a_tool_we_do_not_have() {
        for name in tool_names() {
            let description = tool_schema(name).unwrap().description;
            for absent in ["execute_code", "cronjob", "browser_"] {
                assert!(
                    !description.contains(absent),
                    "{name} description mentions unavailable tool {absent}"
                );
            }
        }
    }

    /// The four schemas CP11.1 shortened, with the budget for all their `description` strings (the
    /// top-level one and every parameter's, in bytes). A bigger description is a decision: raise
    /// the number here and say why in `prompts/tools/README.md`; text that does not change how a
    /// call must be formed belongs in the `hermes-tools-guide` skill instead.
    const SHORTENED: &[(&str, usize)] = &[
        ("skill_manage", 1_500),
        ("session_search", 1_750),
        ("memory", 1_850),
        ("delegate_task", 1_400),
    ];

    /// The pointer every shortened schema ends with: a few words of frozen prompt text.
    const GUIDE_POINTER: &str = "Details: skill_view hermes-tools-guide";

    fn raw(name: &str) -> Value {
        serde_json::from_str(SCHEMAS.iter().find(|(n, _)| *n == name).unwrap().1).unwrap()
    }

    #[test]
    fn the_shortened_schemas_stay_short() {
        for (name, budget) in SHORTENED {
            let bytes = description_bytes(&raw(name));
            assert!(
                bytes <= *budget,
                "{name}: descriptions are {bytes} bytes, budget {budget} (see prompts/tools/README.md)"
            );
        }
    }

    #[test]
    fn the_whole_tool_block_stays_small() {
        // Bytes as serialised into a request, all 18 tools (CP11.1: 27.4 KB, down from 30.5 KB).
        let total: usize = tool_names().map(|name| serialised(name).len()).sum();
        assert!(total <= 27_700, "the tools block is {total} bytes");
    }

    #[test]
    fn a_shortened_schema_points_at_the_guide_and_only_those_do() {
        for name in tool_names() {
            let description = tool_schema(name).unwrap().description;
            let points = description.ends_with(GUIDE_POINTER);
            assert_eq!(
                points,
                SHORTENED.iter().any(|(n, _)| *n == name),
                "{name}: {description}"
            );
        }
    }

    /// Shortening only touched `description` strings: names, types, `required`, enums, defaults
    /// and the `anyOf` shapes are what they were before CP11.1 (`tests/fixtures/tool_shapes.json`
    /// is the old schemas with every description removed), so calls, staging records, ACP and the
    /// review's contracts keep working.
    #[test]
    fn the_shortened_schemas_keep_every_name_type_required_and_enum() {
        let golden: Value =
            serde_json::from_str(include_str!("../tests/fixtures/tool_shapes.json")).unwrap();
        for (name, _) in SHORTENED {
            assert_eq!(
                shape(&raw(name)),
                golden[*name],
                "{name}: the shape changed"
            );
        }
    }

    /// Descriptions are the one thing that shrank, so every property keeps a description where it
    /// had one and the constraints that decide how a call is formed are still said.
    #[test]
    fn the_constraints_a_call_depends_on_are_still_in_the_schema() {
        let must_say: &[(&str, &[&str])] = &[
            (
                "skill_manage",
                &[
                    "atomically",
                    "sole op",
                    "before that skill's other ops",
                    "REPLACES the whole file",
                    "max 60 chars",
                    "max 64 chars",
                    "RELATIVE",
                    "references/, templates/, scripts/ or assets/",
                    "must exist",
                    "no PR numbers/dates/incident narration",
                    "no leading slash",
                ],
            ),
            (
                "session_search",
                &[
                    "session_id",
                    "around_message_id",
                    "max 10",
                    "Cap 20",
                    "[1, 20]",
                    "ISO date",
                    "ONLY",
                    "ignored when session_id is set",
                ],
            ),
            (
                "memory",
                &[
                    "ONE `operations` call",
                    "FINAL result",
                    "reissue ONE batch",
                    "note_read must be a call by itself",
                    "REQUIRED for patch/remove",
                    "Up to 8",
                    "archive",
                    "don't repeat the call",
                    "Skip trivial or easily re-discovered facts",
                    "one-line entries",
                ],
            ),
            (
                "delegate_task",
                &[
                    "`context`",
                    "SELF-REPORTS",
                    "cannot call delegate_task, clarify or memory",
                    "the error states the limit",
                ],
            ),
        ];
        for (name, phrases) in must_say {
            let text = serialised(name);
            for phrase in *phrases {
                assert!(text.contains(phrase), "{name} no longer says {phrase:?}");
            }
        }
        // Every parameter that had a description still has one.
        let golden_old_described: &[(&str, usize)] = &[
            ("skill_manage", 12),
            ("session_search", 11),
            ("memory", 8),
            ("delegate_task", 3),
        ];
        for (name, count) in golden_old_described {
            let described = serialised(name).matches("\"description\":").count() - 1;
            assert_eq!(described, *count, "{name}: parameter descriptions");
        }
    }
}
