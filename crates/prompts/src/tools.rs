//! Tool schemas sent to the model, embedded from `prompts/tools/` (see its README for how they
//! differ from Hermes's).

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

const SCHEMAS: &[(&str, &str)] = &[
    (
        "terminal",
        include_str!("../../../prompts/tools/terminal.json"),
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

    #[test]
    fn no_schema_names_a_tool_we_do_not_have() {
        for name in tool_names() {
            let description = tool_schema(name).unwrap().description;
            for absent in [
                "vision_analyze",
                "execute_code",
                "delegate_task",
                "browser_",
            ] {
                assert!(
                    !description.contains(absent),
                    "{name} description mentions unavailable tool {absent}"
                );
            }
        }
    }
}
