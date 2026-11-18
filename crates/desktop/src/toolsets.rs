//! Capabilities ▸ Tools: Hermes's JSON for the toolsets (`/api/tools/toolsets*`,
//! `/api/tools/terminal/backends`). The logic (which toolsets exist, switching them, the tools
//! they hide) is `hermes_core::toolsets`.
//!
//! Hermes: `hermes_cli/web_routers/tools.py` (`/api/tools/toolsets*`).

use serde_json::{Value, json};

pub use hermes_core::toolsets::{CONFIG_KEY, disabled_tools, disabled_toolsets, switch};

/// `GET /api/tools/toolsets` (`ToolsetInfo[]`).
pub fn list(disabled_toolsets: &[String]) -> Value {
    json!(
        hermes_core::toolsets::list(disabled_toolsets)
            .iter()
            .map(|set| {
                json!({
                    "name": set.name, "label": set.label, "description": set.description,
                    "tools": set.tools, "enabled": set.enabled, "available": set.enabled,
                    "configured": true,
                })
            })
            .collect::<Vec<_>>()
    )
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
    fn the_page_sees_each_toolset_with_its_switch() {
        let off = switch(&[], "terminal", false).unwrap();
        let listed = list(&off);
        assert_eq!(listed[1]["name"], "terminal");
        assert_eq!(listed[1]["enabled"], false);
        assert_eq!(listed[1]["available"], false);
        assert_eq!(listed[0]["enabled"], true);
        assert_eq!(listed[0]["configured"], true);
        assert_eq!(listed[0]["tools"], json!(["web_search", "web_extract"]));
    }
}
