//! The questions the agent asks the editor: a flagged command's approval (`session/request_permission`)
//! and the clarify tool's questions (`elicitation/create`, or a permission card when the editor
//! has no forms). Pure: builds the requests and reads the answers; `server.rs` sends them.
//!
//! Hermes: `acp_adapter/permissions.py` (`_build_permission_options`, `_build_permission_tool_call`,
//! `_map_outcome_to_hermes`), `acp_adapter/elicitation.py`, `acp_adapter/gateway_server.py`
//! (`_answer_permission`, `_answer_clarify`).

use serde_json::{Value, json};

use super::rpc::RpcError;

/// The option id → the gateway's approval choice (`hub.rs` `APPROVAL_CHOICES`).
const APPROVAL_OPTIONS: [(&str, &str, &str, &str); 4] = [
    // (gateway choice, option id, label, ACP option kind)
    ("once", "allow_once", "Allow once", "allow_once"),
    // ACP has no session-scoped kind: the closest hint, our meaning in the id.
    (
        "session",
        "allow_session",
        "Allow for session",
        "allow_always",
    ),
    ("always", "allow_always", "Allow always", "allow_always"),
    ("deny", "deny", "Deny", "reject_once"),
];

fn text_content(text: &str) -> Value {
    json!({"type": "content", "content": {"type": "text", "text": text}})
}

/// The id of the permission card's own tool call.
pub fn card_id(prompt_id: &str) -> String {
    format!("perm-{prompt_id}")
}

/// `session/request_permission` for a flagged command, offering the choices the gateway lists.
pub fn approval_request(
    session_id: &str,
    prompt_id: &str,
    command: &str,
    description: &str,
    choices: &[String],
) -> Value {
    let title = if description.is_empty() {
        command.to_string()
    } else {
        format!("{description}: {command}")
    };
    let body = if description.is_empty() {
        format!("$ {command}")
    } else {
        format!("{description}\n$ {command}")
    };
    let mut options: Vec<Value> = APPROVAL_OPTIONS
        .iter()
        .filter(|(choice, ..)| choices.is_empty() || choices.iter().any(|c| c == choice))
        .map(|(_, id, label, kind)| json!({"optionId": id, "name": label, "kind": kind}))
        .collect();
    // Never a card with no way out.
    if !options.iter().any(|o| o["optionId"] == "deny") {
        options.push(json!({"optionId": "deny", "name": "Deny", "kind": "reject_once"}));
    }
    json!({
        "sessionId": session_id,
        "toolCall": {
            "toolCallId": card_id(prompt_id),
            "title": title,
            "kind": "execute",
            "status": "pending",
            "content": [text_content(&body)],
            "rawInput": {"command": command, "description": description},
        },
        "options": options,
    })
}

/// The editor's answer as the gateway's choice. Anything but a listed allow is a denial: a
/// cancelled card (Stop), an error, or an option we never offered.
pub fn approval_choice(answer: &Result<Value, RpcError>, request: &Value) -> &'static str {
    let Ok(answer) = answer else {
        return "deny";
    };
    let outcome = &answer["outcome"];
    if outcome["outcome"] != "selected" {
        return "deny";
    }
    let Some(picked) = outcome["optionId"].as_str() else {
        return "deny";
    };
    let offered = request["options"]
        .as_array()
        .is_some_and(|options| options.iter().any(|o| o["optionId"] == picked));
    if !offered {
        return "deny";
    }
    APPROVAL_OPTIONS
        .iter()
        .find(|(_, id, ..)| *id == picked)
        .map_or("deny", |(choice, ..)| choice)
}

/// Closes the permission card's tool call once it is answered, so it doesn't stay pending.
pub fn closing_update(prompt_id: &str, allowed: bool) -> Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": card_id(prompt_id),
        "status": if allowed { "completed" } else { "failed" },
    })
}

/// `elicitation/create` (a form with one `answer` field) for the clarify tool.
pub fn clarify_form(session_id: &str, question: &str, choices: &[String], multi: bool) -> Value {
    let mut field = json!({"type": "string"});
    if !choices.is_empty() {
        field = json!({"type": "string", "enum": choices});
        if multi {
            field = json!({"type": "array", "items": field});
        }
    }
    json!({
        "sessionId": session_id,
        "mode": "form",
        "message": question,
        "requestedSchema": {
            "type": "object",
            "properties": {"answer": field},
            "required": ["answer"],
        },
    })
}

/// The form's answer as the clarify tool's text: accepted → the answer (a multi-select list
/// joined with commas, which `hub.rs` splits again); declined or cancelled → `""`, the skip.
/// `None` when the editor didn't really answer.
pub fn clarify_form_answer(answer: &Result<Value, RpcError>) -> Option<String> {
    let answer = answer.as_ref().ok()?;
    match answer["action"].as_str()? {
        "accept" => match &answer["content"]["answer"] {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => None,
        },
        "decline" | "cancel" => Some(String::new()),
        _ => None,
    }
}

/// A clarify question as a permission card, for editors without forms: one option per choice
/// plus Skip. Returns the request and each option id's answer.
pub fn clarify_card(
    session_id: &str,
    prompt_id: &str,
    question: &str,
    choices: &[String],
) -> (Value, Vec<(String, String)>) {
    let mut options: Vec<Value> = Vec::new();
    let mut answers: Vec<(String, String)> = Vec::new();
    for (index, choice) in choices.iter().enumerate() {
        let id = format!("choice-{index}");
        options.push(json!({"optionId": id, "name": choice, "kind": "allow_once"}));
        answers.push((id, choice.clone()));
    }
    options.push(json!({"optionId": "skip", "name": "Skip", "kind": "reject_once"}));
    answers.push(("skip".to_string(), String::new()));
    let body = if choices.is_empty() {
        format!("{question}\n\nAnswer in your next message, or skip.")
    } else {
        question.to_string()
    };
    let request = json!({
        "sessionId": session_id,
        "toolCall": {
            "toolCallId": format!("clarify-{prompt_id}"),
            "title": question,
            "kind": "other",
            "status": "pending",
            "content": [text_content(&body)],
            "rawInput": {"question": question, "choices": choices},
        },
        "options": options,
    });
    (request, answers)
}

/// The card's answer as the clarify tool's text: the picked choice, or `""` (skip) for a
/// cancelled card.
pub fn clarify_card_answer(
    answer: &Result<Value, RpcError>,
    answers: &[(String, String)],
) -> Option<String> {
    let answer = answer.as_ref().ok()?;
    let outcome = &answer["outcome"];
    if outcome["outcome"] != "selected" {
        return Some(String::new());
    }
    let picked = outcome["optionId"].as_str()?;
    answers
        .iter()
        .find(|(id, _)| id == picked)
        .map(|(_, text)| text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_choices() -> Vec<String> {
        ["once", "session", "always", "deny"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn selected(option: &str) -> Result<Value, RpcError> {
        Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}))
    }

    #[test]
    fn an_approval_offers_what_the_gateway_lists() {
        let request = approval_request("s1", "p1", "rm -rf x", "recursive delete", &all_choices());
        let options = request["options"].as_array().unwrap();
        let ids: Vec<_> = options
            .iter()
            .map(|o| o["optionId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["allow_once", "allow_session", "allow_always", "deny"]);
        assert_eq!(request["toolCall"]["toolCallId"], "perm-p1");
        assert_eq!(request["toolCall"]["title"], "recursive delete: rm -rf x");
        assert_eq!(
            request["toolCall"]["content"][0]["content"]["text"],
            "recursive delete\n$ rm -rf x"
        );

        let once_only = approval_request("s1", "p1", "x", "", &["once".into(), "deny".into()]);
        assert_eq!(once_only["options"].as_array().unwrap().len(), 2);
        assert_eq!(once_only["toolCall"]["title"], "x");
    }

    #[test]
    fn every_allow_maps_to_its_choice_and_the_rest_deny() {
        let request = approval_request("s1", "p1", "x", "y", &all_choices());
        assert_eq!(approval_choice(&selected("allow_once"), &request), "once");
        assert_eq!(
            approval_choice(&selected("allow_session"), &request),
            "session"
        );
        assert_eq!(
            approval_choice(&selected("allow_always"), &request),
            "always"
        );
        assert_eq!(approval_choice(&selected("deny"), &request), "deny");
        // Stop in the editor cancels the card; an option we never offered; a broken client.
        assert_eq!(
            approval_choice(&Ok(json!({"outcome": {"outcome": "cancelled"}})), &request),
            "deny"
        );
        assert_eq!(
            approval_choice(&selected("allow_forever"), &request),
            "deny"
        );
        assert_eq!(
            approval_choice(&Err(RpcError::internal("gone")), &request),
            "deny"
        );
        // An allow we didn't offer (a once-only card) is not honored.
        let once_only = approval_request("s1", "p1", "x", "", &["once".into()]);
        assert_eq!(
            approval_choice(&selected("allow_always"), &once_only),
            "deny"
        );
    }

    #[test]
    fn the_card_is_closed_by_the_decision() {
        assert_eq!(closing_update("p1", true)["status"], "completed");
        assert_eq!(closing_update("p1", false)["status"], "failed");
        assert_eq!(closing_update("p1", true)["toolCallId"], "perm-p1");
    }

    #[test]
    fn a_form_asks_free_text_a_choice_or_several() {
        let free = clarify_form("s1", "Name?", &[], false);
        assert_eq!(
            free["requestedSchema"]["properties"]["answer"]["type"],
            "string"
        );
        let pick = clarify_form("s1", "Which?", &["a".into(), "b".into()], false);
        assert_eq!(
            pick["requestedSchema"]["properties"]["answer"]["enum"],
            json!(["a", "b"])
        );
        let many = clarify_form("s1", "Which?", &["a".into(), "b".into()], true);
        assert_eq!(
            many["requestedSchema"]["properties"]["answer"]["type"],
            "array"
        );
        assert_eq!(free["mode"], "form");
    }

    #[test]
    fn a_form_answer_is_the_clarify_text() {
        assert_eq!(
            clarify_form_answer(&Ok(
                json!({"action": "accept", "content": {"answer": "Rust"}})
            )),
            Some("Rust".to_string())
        );
        assert_eq!(
            clarify_form_answer(&Ok(
                json!({"action": "accept", "content": {"answer": ["a", "b"]}})
            )),
            Some("a, b".to_string())
        );
        assert_eq!(
            clarify_form_answer(&Ok(json!({"action": "decline"}))),
            Some(String::new())
        );
        assert_eq!(clarify_form_answer(&Ok(json!({"action": "weird"}))), None);
        assert_eq!(clarify_form_answer(&Err(RpcError::internal("x"))), None);
    }

    #[test]
    fn a_card_has_a_button_per_choice_and_a_skip() {
        let (request, answers) =
            clarify_card("s1", "p2", "Which?", &["Rust".into(), "Python".into()]);
        let options = request["options"].as_array().unwrap();
        assert_eq!(options.len(), 3);
        assert_eq!(options[0]["name"], "Rust");
        assert_eq!(options[2]["optionId"], "skip");
        assert_eq!(
            clarify_card_answer(&selected("choice-1"), &answers),
            Some("Python".to_string())
        );
        assert_eq!(
            clarify_card_answer(&selected("skip"), &answers),
            Some(String::new())
        );
        // A cancelled card is a skip; a broken client is no answer.
        assert_eq!(
            clarify_card_answer(&Ok(json!({"outcome": {"outcome": "cancelled"}})), &answers),
            Some(String::new())
        );
        assert_eq!(
            clarify_card_answer(&Err(RpcError::internal("x")), &answers),
            None
        );
    }

    #[test]
    fn a_free_question_on_a_card_says_how_to_answer() {
        let (request, _) = clarify_card("s1", "p2", "Name?", &[]);
        assert!(
            request["toolCall"]["content"][0]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("next message")
        );
    }
}
