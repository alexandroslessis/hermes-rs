//! What the editor lets the user change in a thread: the model (its picker), the reasoning effort
//! and the mode. ACP calls the first two "config options" (`category: "model"` and
//! `"thought_level"` are the ones Zed shows as pickers) and the last a "mode". Pure: builds the
//! JSON the editor shows from what the gateway answers, and reads what it picks.
//!
//! Hermes: `acp_adapter/model_catalog.py` (`build_model_state`, `encode_model_choice`),
//! `acp_adapter/modes.py` and `gateway_server.py` (`set_config_option`, `set_session_mode`). Unlike
//! Hermes's modes (edit-approval policies), ours are "ask before dangerous commands" and "don't
//! ask" (`/yolo`), since hermes-rs has no per-edit approvals.

use serde_json::{Value, json};

pub const DEFAULT_MODE: &str = "default";
pub const YOLO_MODE: &str = "yolo";

/// The efforts the editor offers: Hermes's own list (`REASONING_EFFORTS` in its frontend, with
/// "Thinking" off as `off`), the same for every model as there, plus "Default" (nothing picked:
/// Settings' effort, else the provider's own). A level a model doesn't know is the provider's
/// to clamp or refuse.
const EFFORTS: [(&str, &str); 9] = [
    ("default", "Default"),
    ("off", "Off"),
    ("minimal", "Minimal"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra High"),
    ("max", "Max"),
    ("ultra", "Ultra"),
];

/// The commands the gateway runs for `slash.exec` that its `commands.catalog` doesn't list (the
/// page types them itself): offered to the editor, which refuses a command that isn't listed.
pub const GATEWAY_COMMANDS: [(&str, &str); 8] = [
    (
        "status",
        "Show the session, its model and folder, tokens so far and the context in use",
    ),
    ("usage", "Show the token usage of this session"),
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

pub fn modes(yolo: bool) -> Value {
    json!({
        "currentModeId": if yolo { YOLO_MODE } else { DEFAULT_MODE },
        "availableModes": [
            {"id": DEFAULT_MODE, "name": "Default",
             "description": "Ask before running dangerous commands."},
            {"id": YOLO_MODE, "name": "YOLO",
             "description": "Run flagged commands without asking (the ones that must never run still don't)."},
        ],
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
pub fn mode_option(yolo: bool) -> Value {
    let modes = modes(yolo);
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

/// The effort picker; `current` is the gateway's effort for the session (`""`: nothing picked,
/// `none`: thinking off).
pub fn effort_option(current: &str) -> Value {
    let current = match current {
        "" => "default",
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
            "default" => "",
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
        assert_eq!(effort_option("")["currentValue"], "default");
        assert_eq!(effort_option("none")["currentValue"], "off");
        assert_eq!(effort_option("high")["currentValue"], "high");
        assert_eq!(effort_option("low")["category"], "thought_level");
    }

    #[test]
    fn the_editors_efforts_map_to_the_gateways() {
        assert_eq!(gateway_effort("default"), Some(""));
        assert_eq!(gateway_effort("off"), Some("none"));
        assert_eq!(gateway_effort("xhigh"), Some("xhigh"));
        assert_eq!(gateway_effort("medium"), Some("medium"));
        assert_eq!(gateway_effort("extreme"), None);
    }

    #[test]
    fn the_mode_is_also_a_config_option() {
        let option = mode_option(true);
        assert_eq!(option["category"], "mode");
        assert_eq!(option["currentValue"], "yolo");
        assert_eq!(option["options"][0]["value"], "default");
    }

    #[test]
    fn the_mode_follows_yolo() {
        assert_eq!(modes(false)["currentModeId"], "default");
        assert_eq!(modes(true)["currentModeId"], "yolo");
        assert_eq!(modes(false)["availableModes"].as_array().unwrap().len(), 2);
    }
}
