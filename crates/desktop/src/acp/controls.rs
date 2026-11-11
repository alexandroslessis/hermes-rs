//! What the editor lets the user change in a thread: the model (its picker), the reasoning effort
//! and the mode. ACP calls the first two "config options" (`category: "model"` and
//! `"thought_level"` are the ones Zed shows as pickers) and the last a "mode". Pure: builds the
//! JSON the editor shows from what the gateway answers, and reads what it picks.
//!
//! Hermes: `acp_adapter/model_catalog.py` (`build_model_state`, `encode_model_choice`),
//! `acp_adapter/modes.py` and `gateway_server.py` (`set_config_option`, `set_session_mode`). Our
//! modes are the session's [`ApprovalMode`]: Auto, Manual, Accept edits, Plan and YOLO (the
//! first four as Claude Code names them, the last Hermes's `/yolo`).

use hermes_tools::ApprovalMode;
use serde_json::{Value, json};

/// The modes, in the order the editor lists them: id, name, description.
const MODES: [(ApprovalMode, &str, &str, &str); 5] = [
    (
        ApprovalMode::Auto,
        "auto",
        "Auto",
        "Hermes handles permission decisions: a reviewer model judges risky commands",
    ),
    (
        ApprovalMode::Manual,
        "manual",
        "Manual",
        "Always ask before making changes",
    ),
    (
        ApprovalMode::AcceptEdits,
        "accept_edits",
        "Accept edits",
        "Automatically accept file edits; ask before risky commands",
    ),
    (
        ApprovalMode::Plan,
        "plan",
        "Plan",
        "Create a plan before making changes",
    ),
    (
        ApprovalMode::Yolo,
        "yolo",
        "YOLO",
        "Never ask (the commands that must never run still don't)",
    ),
];

pub fn mode_id(mode: ApprovalMode) -> &'static str {
    MODES
        .iter()
        .find(|(m, ..)| *m == mode)
        .map_or("accept_edits", |(_, id, ..)| id)
}

/// The mode an editor picked; `None` for one we never offered.
pub fn parse_mode(id: &str) -> Option<ApprovalMode> {
    MODES
        .iter()
        .find(|(_, mode_id, ..)| *mode_id == id)
        .map(|(mode, ..)| *mode)
}

/// The efforts the editor offers: Hermes's own list (`REASONING_EFFORTS` in its frontend, with
/// "Thinking" off as `off`), the same for every model as there. A level a model doesn't know is
/// the provider's to clamp or refuse.
const EFFORTS: [(&str, &str); 8] = [
    ("off", "Off"),
    ("minimal", "Minimal"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra High"),
    ("max", "Max"),
    ("ultra", "Ultra"),
];

/// What the effort picker shows when nothing was picked and Settings has none: Hermes's default.
pub const DEFAULT_EFFORT: &str = "medium";

/// The commands the gateway runs for `slash.exec` that its `commands.catalog` doesn't list (the
/// page types them itself): offered to the editor, which refuses a command that isn't listed.
pub const GATEWAY_COMMANDS: [(&str, &str); 10] = [
    (
        "status",
        "Show the session, its model and folder, tokens so far and the context in use",
    ),
    ("usage", "Show the token usage of this session"),
    (
        "undo",
        "Drop the last turn (or the last N: /undo 2) from the conversation",
    ),
    ("retry", "Drop the last turn and send its prompt again"),
    ("context", "Show what fills the context window"),
    ("insights", "Show usage insights over the last days"),
    ("diff", "Show what changed in the working folder"),
    ("tools", "List the tools the agent has"),
    ("memory", "Show the memory writes waiting for approval"),
    (
        "refine",
        "Review this conversation for lessons to save to memory and skills",
    ),
];

pub fn modes(current: ApprovalMode) -> Value {
    json!({
        "currentModeId": mode_id(current),
        "availableModes": MODES
            .iter()
            .map(|(_, id, name, description)| json!({"id": id, "name": name, "description": description}))
            .collect::<Vec<_>>(),
    })
}

/// `"<provider>/<model>"`, the option value of one model: a model id can hold slashes
/// (OpenRouter's `vendor/name`), a provider id never does.
pub fn model_value(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

pub fn split_model(value: &str) -> Option<(&str, &str)> {
    let (provider, model) = value.split_once('/')?;
    (!provider.is_empty() && !model.is_empty()).then_some((provider, model))
}

/// The model picker, from the gateway's `model.options` answer. The current model is always in
/// the list, whether or not its provider listed it.
pub fn model_option(options: &Value) -> Value {
    let current_provider = options["provider"].as_str().unwrap_or_default();
    let current_model = options["model"].as_str().unwrap_or_default();
    let current = model_value(current_provider, current_model);
    let mut choices: Vec<Value> = Vec::new();
    for provider in options["providers"].as_array().into_iter().flatten() {
        let slug = provider["slug"].as_str().unwrap_or_default();
        let name = provider["name"].as_str().unwrap_or(slug);
        for model in provider["models"].as_array().into_iter().flatten() {
            if let Some(model) = model.as_str() {
                choices.push(json!({
                    "value": model_value(slug, model),
                    "name": model,
                    "description": name,
                }));
            }
        }
    }
    if !current_model.is_empty() && !choices.iter().any(|c| c["value"] == current.as_str()) {
        choices.insert(
            0,
            json!({"value": current, "name": current_model, "description": current_provider}),
        );
    }
    json!({
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": current,
        "options": choices,
    })
}

/// The mode as a config option too: an editor that gets config options (Zed) shows those and
/// ignores `modes`.
pub fn mode_option(current: ApprovalMode) -> Value {
    let modes = modes(current);
    json!({
        "id": "mode",
        "name": "Mode",
        "category": "mode",
        "type": "select",
        "currentValue": modes["currentModeId"],
        "options": modes["availableModes"].as_array().into_iter().flatten()
            .map(|m| json!({"value": m["id"], "name": m["name"], "description": m["description"]}))
            .collect::<Vec<_>>(),
    })
}

/// The effort picker; `current` is the gateway's effort for the session (`""`: nothing picked
/// and none in Settings, shown as Hermes's default; `none`: thinking off).
pub fn effort_option(current: &str) -> Value {
    let current = match current {
        "" => DEFAULT_EFFORT,
        "none" => "off",
        other => other,
    };
    json!({
        "id": "effort",
        "name": "Reasoning effort",
        "category": "thought_level",
        "type": "select",
        "currentValue": current,
        "options": EFFORTS
            .iter()
            .map(|(value, name)| json!({"value": value, "name": name}))
            .collect::<Vec<_>>(),
    })
}

/// An effort the editor picked as the gateway's value; `None` for one we never offered.
pub fn gateway_effort(picked: &str) -> Option<&'static str> {
    EFFORTS
        .iter()
        .find(|(value, _)| *value == picked)
        .map(|(value, _)| match *value {
            "off" => "none",
            other => other,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Value {
        json!({
            "provider": "or", "model": "vendor/big",
            "providers": [
                {"slug": "local", "name": "Local", "models": ["small", "other"]},
                {"slug": "or", "name": "OpenRouter", "models": ["vendor/big", "vendor/tiny"]},
            ],
        })
    }

    #[test]
    fn the_picker_lists_every_providers_models() {
        let option = model_option(&options());
        let values: Vec<&str> = option["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(
            values,
            [
                "local/small",
                "local/other",
                "or/vendor/big",
                "or/vendor/tiny"
            ]
        );
        assert_eq!(option["currentValue"], "or/vendor/big");
        assert_eq!(option["category"], "model");
        assert_eq!(option["options"][2]["description"], "OpenRouter");
    }

    #[test]
    fn a_current_model_the_provider_did_not_list_is_still_offered() {
        let mut unlisted = options();
        unlisted["model"] = json!("hidden");
        unlisted["provider"] = json!("local");
        let option = model_option(&unlisted);
        assert_eq!(option["options"][0]["value"], "local/hidden");
        assert_eq!(option["currentValue"], "local/hidden");
    }

    #[test]
    fn a_value_splits_at_the_first_slash_only() {
        assert_eq!(split_model("or/vendor/big"), Some(("or", "vendor/big")));
        assert_eq!(split_model("local/small"), Some(("local", "small")));
        assert_eq!(split_model("nothing"), None);
        assert_eq!(split_model("/model"), None);
        assert_eq!(split_model("provider/"), None);
    }

    #[test]
    fn an_unset_effort_reads_as_default() {
        assert_eq!(effort_option("")["currentValue"], "medium");
        assert_eq!(effort_option("none")["currentValue"], "off");
        assert_eq!(effort_option("high")["currentValue"], "high");
        assert_eq!(effort_option("low")["category"], "thought_level");
    }

    #[test]
    fn the_editors_efforts_map_to_the_gateways() {
        assert_eq!(gateway_effort("default"), None);
        assert_eq!(gateway_effort("off"), Some("none"));
        assert_eq!(gateway_effort("xhigh"), Some("xhigh"));
        assert_eq!(gateway_effort("medium"), Some("medium"));
        assert_eq!(gateway_effort("extreme"), None);
    }

    #[test]
    fn the_mode_is_also_a_config_option() {
        let option = mode_option(ApprovalMode::Yolo);
        assert_eq!(option["category"], "mode");
        assert_eq!(option["currentValue"], "yolo");
        assert_eq!(option["options"][0]["value"], "auto");
    }

    #[test]
    fn the_modes_are_claude_codes_four_and_yolo() {
        let all = modes(ApprovalMode::Plan);
        assert_eq!(all["currentModeId"], "plan");
        let ids: Vec<&str> = all["availableModes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["auto", "manual", "accept_edits", "plan", "yolo"]);
        for id in ids {
            assert_eq!(mode_id(parse_mode(id).unwrap()), id);
        }
        assert_eq!(parse_mode("default"), None);
    }
}
