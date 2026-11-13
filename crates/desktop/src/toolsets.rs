//! Capabilities ▸ Tools: the agent's tools in Hermes's toolsets, each switchable. A toolset switched
//! off is kept in `desktop-config.json` (`disabled_toolsets`) and its tools are neither offered to the model
//! nor run, from the next turn on (subagents included).
//!
//! Hermes: `hermes_cli/web_routers/tools.py` (`/api/tools/toolsets*`), `hermes_cli/tools_config.py`
//! (`CONFIGURABLE_TOOLSETS`: names, labels, descriptions); Hermes keeps the enabled set per
//! platform (`platform_toolsets`), this keeps the disabled ones.

use serde_json::{Value, json};

/// The config key the switched-off toolsets are kept under.
pub const CONFIG_KEY: &str = "disabled_toolsets";

/// `(name, label, description, tools)`: Hermes's toolsets that this runtime has tools for.
pub const TOOLSETS: [(&str, &str, &str, &[&str]); 10] = [
    (
        "web",
        "Web Search & Scraping",
        "web_search, web_extract",
        &["web_search", "web_extract"],
    ),
    (
        "terminal",
        "Terminal & Processes",
        "terminal, run_script, process",
        &["terminal", "run_script", "process_manage"],
    ),
    (
        "file",
        "File Operations",
        "read, write, patch, search",
        &["read_file", "write_file", "patch", "search_files"],
    ),
    (
        "vision",
        "Vision / Image Analysis",
        "vision_analyze",
        &["vision_analyze"],
    ),
    (
        "skills",
        "Skills",
        "list, view, manage",
        &["skills_list", "skill_view", "skill_manage"],
    ),
    ("todo", "Task Planning", "todo_list", &["todo_list"]),
    (
        "memory",
        "Memory",
        "persistent memory across sessions",
        &["memory"],
    ),
    (
        "session_search",
        "Session Search",
        "search past conversations",
        &["session_search"],
    ),
    ("clarify", "Clarifying Questions", "clarify", &["clarify"]),
    (
        "delegation",
        "Task Delegation",
        "delegate_task",
        &["delegate_task"],
    ),
];

/// The switched-off toolsets in `config` (`disabled_toolsets`).
pub fn disabled_toolsets(config: &Value) -> Vec<String> {
    config
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The tools of the switched-off toolsets.
pub fn disabled_tools(disabled_toolsets: &[String]) -> Vec<String> {
    TOOLSETS
        .iter()
        .filter(|(name, ..)| disabled_toolsets.iter().any(|d| d == name))
        .flat_map(|(_, _, _, tools)| tools.iter().map(|t| t.to_string()))
        .collect()
}

/// `GET /api/tools/toolsets` (`ToolsetInfo[]`).
pub fn list(disabled_toolsets: &[String]) -> Value {
    json!(
        TOOLSETS
            .iter()
            .map(|(name, label, description, tools)| {
                let enabled = !disabled_toolsets.iter().any(|d| d == name);
                json!({
                    "name": name, "label": label, "description": description, "tools": tools,
                    "enabled": enabled, "available": enabled, "configured": true,
                })
            })
            .collect::<Vec<_>>()
    )
}

/// The switched-off toolsets after switching `name`; `None` for a toolset this app doesn't have.
pub fn switch(disabled_toolsets: &[String], name: &str, enabled: bool) -> Option<Vec<String>> {
    if !TOOLSETS.iter().any(|(known, ..)| *known == name) {
        return None;
    }
    let mut next: Vec<String> = disabled_toolsets
        .iter()
        .filter(|d| *d != name)
        .cloned()
        .collect();
    if !enabled {
        next.push(name.to_string());
    }
    Some(next)
}

/// `GET /api/tools/toolsets/{name}/config`: no providers to set up (the tools here need no keys).
pub fn config(name: &str) -> Value {
    json!({"name": name, "has_category": false, "providers": [], "active_provider": null})
}

/// `GET /api/tools/terminal/backends`: commands run here, in PowerShell.
pub fn terminal_backends() -> Value {
    json!({
        "active": "local",
        "backends": [{
            "name": "local",
            "label": "Local",
            "description": "Commands run on this computer, in PowerShell, in the session's folder.",
            "active": true,
            "status": "ready",
            "detail": "",
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_belongs_to_one_toolset() {
        let mut seen: Vec<&str> = TOOLSETS
            .iter()
            .flat_map(|(_, _, _, tools)| tools.iter().copied())
            .collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "a tool in two toolsets");
        let mut runtime: Vec<&str> = hermes_tools::SessionTools::tool_names();
        runtime.sort_unstable();
        assert_eq!(seen, runtime, "the runtime's tools and the toolsets differ");
    }

    #[test]
    fn switching_off_and_on() {
        let off = switch(&[], "terminal", false).unwrap();
        assert_eq!(off, ["terminal"]);
        assert_eq!(
            disabled_tools(&off),
            ["terminal", "run_script", "process_manage"]
        );
        assert_eq!(list(&off)[1]["enabled"], false);
        assert_eq!(list(&off)[0]["enabled"], true);
        assert!(switch(&off, "terminal", true).unwrap().is_empty());
        assert!(
            switch(&off, "browser", false).is_none(),
            "not a toolset here"
        );
        assert_eq!(
            disabled_toolsets(&json!(["web", 3, "file"])),
            ["web", "file"]
        );
        assert_eq!(disabled_toolsets(&Value::Null), Vec::<String>::new());
    }
}
