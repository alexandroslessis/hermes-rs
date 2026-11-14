//! Deferred tool schemas (Phase 11, CP11.2): in a `tokens` session the request's tool list is the
//! core tools plus `tool_search`; the rarely used ones are named in the frozen system prompt
//! (`hermes_prompts::deferred`) and join the list when the model loads them.
//!
//! Not in Hermes (it sends every schema every request). `docs/backend/PHASES.md`, Phase 11;
//! `PORTING.md` row `diverged`.
//!
//! **What is deferred.** The built-in tools in `hermes_prompts::deferred::BUILTIN` (the sizes and
//! the choice are in `docs/backend/progress/phase-11.md`) and every MCP tool but the ones the
//! session's block recorded as core (the first `deferred_mcp_keep` when it was made, server-name
//! order; a server that connects later is deferred whole, see [`split`]). `skill_manage` is not
//! deferred but absent: the background review (which has its own tool set) keeps it, the main
//! agent never has it.
//!
//! **Which sessions.** A session is deferred exactly when its frozen system prompt holds the
//! sealed deferred block (`is_deferred_prompt`; a copy of the block in a context file does not
//! count). The profile decides that when the session's first prompt is built; a compaction or a
//! model switch rebuilds the prompt but keeps its kind (`PromptTools::of_session` in `lib.rs`), so
//! a session's list never flips between the full one and the deferred one, and a resumed session
//! knows without a side file.
//!
//! **The loaded set is a function of the conversation** ([`loaded_in`]): every executed
//! `tool_search` call loads the deferred tools it names, and so does a direct call to a deferred
//! tool (answered with its schema instead of being run, see below), in the order they happened.
//! Nothing is stored: a resumed session, a branch and a rewind (which drops the later calls)
//! recompute the list the session had, and a compaction (which hides messages from the model but
//! keeps them in the conversation) keeps what was loaded. [`DeferredTools::tools`] puts the core
//! tools first, then `tool_search`, then the loaded ones in load order, so the cache prefix breaks
//! only where a load adds tools.
//!
//! **A deferred tool called before it was loaded** is not run: the model has not seen its schema,
//! and a guessed call (a `patch`, a `delegate_task`) can do harm. The call is answered with an
//! error that carries the schema and says the tool is in the list from the next step, and it
//! counts as a load, so the retry costs one round trip, the same as `tool_search` would have.
//! Auto-running the call was the alternative; it saves that round trip but runs arguments formed
//! without the schema, which a small local model gets wrong often enough to matter.
//!
//! **Gating.** The wrapper sits inside `GuardedTools`, which filters the tools the user switched
//! off (Capabilities > Tools) from the list and refuses calls to them, so a loaded tool of a
//! disabled toolset (or of a disconnected MCP server: it is not in the inner list) is never
//! offered; `tool_search` says "turned off" and does not return its schema.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use hermes_agent::operation::Emitter;
use hermes_agent::tool::ToolProvider;
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::MessageContent;
use hermes_prompts::deferred::{
    self, BUILTIN, MAX_MATCHES, MAX_NAMES, REVIEW_ONLY, TOOL_SEARCH, is_deferred_prompt,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use serde_json::{Value, json};

use crate::session::TurnSession;

/// The tools of one request, split. `deferred` are the ones not in the list until loaded.
struct Split {
    core: Vec<Tool>,
    deferred: Vec<Tool>,
}

/// The names a `tool_search` call asks for: `names` (an array, or a single string), trimmed,
/// without repeats, in the order given.
pub fn requested_names(args: &JsonObject) -> Vec<String> {
    let raw: Vec<&str> = match args.get("names") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(one)) => vec![one.as_str()],
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    raw.into_iter()
        .map(|name| name.trim().trim_matches(['"', '\'', '`']).trim())
        .filter(|name| !name.is_empty() && seen.insert(name.to_string()))
        .map(str::to_string)
        .collect()
}

/// The deferred tools `requested` loads: those named exactly; nothing at all when the call names
/// more than [`MAX_NAMES`] (it is refused).
fn loads(requested: &[String], deferred: &[String]) -> Vec<String> {
    if requested.len() > MAX_NAMES {
        return Vec::new();
    }
    requested
        .iter()
        .filter(|name| deferred.contains(name))
        .cloned()
        .collect()
}

/// The deferred tools a conversation has loaded, in load order. A call counts only when it was
/// answered (a call withdrawn or never run loaded nothing); messages hidden by a compaction count.
pub fn loaded_in(conversation: &Conversation, deferred: &[String]) -> Vec<String> {
    let answered: HashSet<&str> = conversation
        .messages()
        .iter()
        .flat_map(|message| message.get_tool_response_ids())
        .collect();
    let mut loaded: Vec<String> = Vec::new();
    let mut load = |name: &str| {
        if !loaded.iter().any(|have| have == name) {
            loaded.push(name.to_string());
        }
    };
    for request in conversation
        .messages()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(MessageContent::as_tool_request)
    {
        let Ok(call) = &request.tool_call else {
            continue;
        };
        if !answered.contains(request.id.as_str()) {
            continue;
        }
        if call.name == TOOL_SEARCH {
            let args = call.arguments.clone().unwrap_or_default();
            for name in loads(&requested_names(&args), deferred) {
                load(&name);
            }
        } else if deferred
            .iter()
            .any(|name| name.as_str() == call.name.as_ref())
        {
            load(call.name.as_ref());
        }
    }
    loaded
}

/// Whether a frozen system prompt makes its session deferred.
pub fn is_deferred_session(system_prompt: &str) -> bool {
    is_deferred_prompt(system_prompt)
}

/// A session's tools behind `tool_search` (see the module doc). In a session that is not deferred
/// it is transparent: the list and the calls are the inner provider's, byte for byte.
pub struct DeferredTools {
    pub inner: Arc<dyn ToolProvider<TurnSession>>,
    /// The tools the user switched off: `tool_search` doesn't hand out their schemas.
    pub disabled: Arc<HashSet<String>>,
}

/// The MCP tools a new deferred session keeps in its list: the first `keep` connected now (server
/// name order), recorded in the frozen block.
pub fn prompt_core_mcp(mcp: &[Tool], keep: usize) -> Vec<String> {
    mcp.iter()
        .take(keep)
        .map(|tool| tool.name.to_string())
        .collect()
}

/// The deferred entries for a prompt: built-in ones, then the MCP tools after the first `keep`
/// (the lines the frozen block lists). `mcp` is every MCP tool in the order the list has them.
pub fn prompt_entries(mcp: &[Tool], keep: usize) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = BUILTIN
        .iter()
        .map(|(name, line)| (name.to_string(), line.to_string()))
        .collect();
    lines.extend(mcp.iter().skip(keep).map(|tool| {
        (
            tool.name.to_string(),
            deferred::short_line(tool.description.as_deref().unwrap_or(""), 70),
        )
    }));
    lines
}

impl DeferredTools {
    pub fn new(inner: Arc<dyn ToolProvider<TurnSession>>, disabled: Arc<HashSet<String>>) -> Self {
        Self { inner, disabled }
    }
}

/// Splits a deferred session's tools into the core and the deferred ones. Which MCP tools are core
/// is what the session's frozen block recorded (`hermes_prompts::deferred::core_mcp`), not the
/// knob or the servers connected now: a server that connects later (its tools are deferred, found
/// by `tool_search`'s `query`), a reconnect in another order or a changed `deferred_mcp_keep`
/// never moves a tool out of the list mid-session.
fn split(all: Vec<Tool>, system_prompt: &str) -> Split {
    let core_mcp = deferred::core_mcp(system_prompt);
    let mut core = Vec::new();
    let mut deferred_tools = Vec::new();
    for mut tool in all {
        let name = tool.name.to_string();
        if name == REVIEW_ONLY {
            continue;
        }
        if BUILTIN.iter().any(|(deferred, _)| *deferred == name) {
            deferred_tools.push(tool);
        } else if name.starts_with("mcp__") {
            if core_mcp.contains(&name) {
                core.push(tool);
            } else {
                deferred_tools.push(tool);
            }
        } else {
            if name == "memory"
                && let Some(text) = tool.description.as_deref()
            {
                tool.description =
                    Some(deferred::memory_description_without_skill_manage(text).into());
            }
            core.push(tool);
        }
    }
    Split {
        core,
        deferred: deferred_tools,
    }
}

/// The tools a deferred session's next request lists: the core ones, `tool_search`, then the
/// loaded ones in load order. `None` for a session that is not deferred (it lists `all`).
pub fn listed(
    all: Vec<Tool>,
    system_prompt: &str,
    conversation: &Conversation,
) -> Option<Vec<Tool>> {
    if !is_deferred_session(system_prompt) {
        return None;
    }
    let Split { mut core, deferred } = split(all, system_prompt);
    let names: Vec<String> = deferred.iter().map(|tool| tool.name.to_string()).collect();
    core.push(DeferredTools::search_tool().ok()?);
    for name in loaded_in(conversation, &names) {
        if let Some(tool) = deferred.iter().find(|tool| tool.name == name.as_str()) {
            core.push(tool.clone());
        }
    }
    Some(core)
}

impl DeferredTools {
    fn search_tool() -> Result<Tool> {
        let schema = deferred::tool_search_schema()?;
        Ok(Tool::new(
            schema.name,
            schema.description,
            Arc::new(schema.parameters),
        ))
    }

    /// The error that answers a call to a tool the model cannot use yet, for
    /// `UnhandledToolCalls`: `None` when the session is not deferred or the name is not one of
    /// ours (an invented name gets the usual answer).
    pub async fn unavailable_reply(&self, session: &TurnSession, name: &str) -> Option<String> {
        if !is_deferred_session(&session.system_prompt) {
            return None;
        }
        if name == REVIEW_ONLY {
            return Some(
                json!({"success": false, "error": format!(
                    "{REVIEW_ONLY} is not available to you in this mode: a background review of the \
                     conversation saves skills. Say plainly in your answer what the task taught you.")})
                .to_string(),
            );
        }
        let all = self.inner.tools(session).await.ok()?;
        let split = split(all, &session.system_prompt);
        let tool = split.deferred.iter().find(|tool| tool.name == name)?;
        Some(
            json!({
                "success": false,
                "error": format!(
                    "{name} is a deferred tool: its schema was not loaded, so this call was not run. \
                     It is in your tool list from your next step: call it again with the parameters below."),
                "schema": schema_value(tool),
            })
            .to_string(),
        )
    }

    async fn search(&self, session: &TurnSession, args: &JsonObject) -> CallToolResult {
        let all = match self.inner.tools(session).await {
            Ok(all) => all,
            Err(error) => return failed(format!("tool_search could not list the tools: {error}")),
        };
        let split = split(all, &session.system_prompt);
        let deferred_names: Vec<String> = split
            .deferred
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let already = loaded_in(&session.conversation, &deferred_names);
        let requested = requested_names(args);
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if requested.is_empty() && query.is_empty() {
            return failed(format!(
                "Pass `names` (exact tool names, at most {MAX_NAMES}) to load tools, or `query` to \
                 look for them. Deferred tools: {}.",
                deferred_names.join(", ")
            ));
        }
        if requested.len() > MAX_NAMES {
            return failed(format!(
                "{} names in one call; at most {MAX_NAMES}, and nothing was loaded. Load what you \
                 need now and the rest later.",
                requested.len()
            ));
        }
        let mut result = serde_json::Map::new();
        let mut loaded_now = Vec::new();
        let mut schemas = Vec::new();
        let mut already_loaded = Vec::new();
        let mut in_list = Vec::new();
        let mut turned_off = Vec::new();
        let mut unavailable = Vec::new();
        let mut unknown = Vec::new();
        for name in &requested {
            if let Some(tool) = split
                .deferred
                .iter()
                .find(|tool| tool.name == name.as_str())
            {
                if self.disabled.contains(name) {
                    turned_off.push(name.clone());
                } else if already.contains(name) {
                    already_loaded.push(name.clone());
                } else {
                    loaded_now.push(name.clone());
                    schemas.push(schema_value(tool));
                }
            } else if name == REVIEW_ONLY {
                unavailable.push(name.clone());
            } else if name == TOOL_SEARCH
                || split.core.iter().any(|tool| tool.name == name.as_str())
            {
                in_list.push(name.clone());
            } else {
                unknown.push(name.clone());
            }
        }
        let mut matches = Vec::new();
        if !query.is_empty() {
            let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
            for tool in &split.deferred {
                let text = format!(
                    "{} {}",
                    tool.name,
                    tool.description.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if self.disabled.contains(tool.name.as_ref())
                    || !terms.iter().all(|term| text.contains(term))
                {
                    continue;
                }
                if matches.len() < MAX_MATCHES {
                    let loaded = already
                        .iter()
                        .any(|name| name.as_str() == tool.name.as_ref());
                    matches.push(json!({
                        "name": tool.name,
                        "description": deferred::short_line(tool.description.as_deref().unwrap_or(""), 100),
                        "loaded": loaded,
                    }));
                }
            }
        }
        let helped = !loaded_now.is_empty()
            || !already_loaded.is_empty()
            || !in_list.is_empty()
            || !matches.is_empty();
        for (key, list) in [
            ("loaded", &loaded_now),
            ("already_loaded", &already_loaded),
            ("already_in_your_tool_list", &in_list),
            ("turned_off_by_the_user", &turned_off),
            ("not_available_to_you", &unavailable),
            ("unknown", &unknown),
        ] {
            if !list.is_empty() {
                result.insert(key.to_string(), json!(list));
            }
        }
        if !schemas.is_empty() {
            result.insert("schemas".into(), Value::Array(schemas));
            result.insert(
                "note".into(),
                json!("The loaded tools are in your tool list from your next step; call them with exactly the parameters above."),
            );
        }
        if !query.is_empty() {
            result.insert("matches".into(), Value::Array(matches));
        }
        if !unknown.is_empty() {
            result.insert("available_deferred".into(), json!(deferred_names));
        }
        result.insert("success".into(), json!(helped || !turned_off.is_empty()));
        let text = Value::Object(result).to_string();
        if helped || !turned_off.is_empty() {
            CallToolResult::success(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::error(vec![ContentBlock::text(text)])
        }
    }
}

fn schema_value(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

fn failed(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        json!({"success": false, "error": message}).to_string(),
    )])
}

#[async_trait]
impl ToolProvider<TurnSession> for DeferredTools {
    async fn tools(&self, session: &TurnSession) -> Result<Vec<Tool>> {
        let all = self.inner.tools(session).await?;
        if !is_deferred_session(&session.system_prompt) {
            return Ok(all);
        }
        listed(all, &session.system_prompt, &session.conversation)
            .context("the tool_search schema does not parse")
    }

    async fn call(
        &self,
        session: &TurnSession,
        request_id: &str,
        call: CallToolRequestParams,
        emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        if call.name == TOOL_SEARCH && is_deferred_session(&session.system_prompt) {
            let args = call.arguments.unwrap_or_default();
            return Ok(self.search(session, &args).await);
        }
        self.inner.call(session, request_id, call, emit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::conversation::message::Message;

    fn call(name: &str, args: Value) -> CallToolRequestParams {
        let Value::Object(args) = args else { panic!() };
        CallToolRequestParams::new(name.to_string()).with_arguments(args)
    }

    fn round(id: &str, name: &str, args: Value, answered: bool) -> Vec<Message> {
        let mut messages = vec![Message::assistant().with_tool_request(id, Ok(call(name, args)))];
        if answered {
            messages.push(Message::user().with_tool_response(
                id,
                Ok(CallToolResult::success(vec![ContentBlock::text("ok")])),
            ));
        }
        messages
    }

    fn deferred() -> Vec<String> {
        ["session_search", "web_search", "web_extract", "mcp__s__a"]
            .map(String::from)
            .to_vec()
    }

    fn conversation(messages: Vec<Message>) -> Conversation {
        Conversation::new_unvalidated(messages)
    }

    #[test]
    fn names_are_trimmed_deduplicated_and_a_string_is_a_list() {
        let args = json!({"names": [" web_search ", "web_search", "`session_search`", ""]});
        let Value::Object(args) = args else { panic!() };
        assert_eq!(requested_names(&args), ["web_search", "session_search"]);
        let Value::Object(one) = json!({"names": "web_search"}) else {
            panic!()
        };
        assert_eq!(requested_names(&one), ["web_search"]);
        let Value::Object(none) = json!({"query": "x"}) else {
            panic!()
        };
        assert!(requested_names(&none).is_empty());
    }

    #[test]
    fn the_loaded_set_follows_the_calls_in_order() {
        let mut messages = round(
            "a",
            "tool_search",
            json!({"names": ["web_search", "nope"]}),
            true,
        );
        messages.extend(round(
            "b",
            "tool_search",
            json!({"names": ["session_search", "web_search"]}),
            true,
        ));
        let loaded = loaded_in(&conversation(messages), &deferred());
        assert_eq!(loaded, ["web_search", "session_search"]);
    }

    #[test]
    fn a_direct_call_to_a_deferred_tool_loads_it_and_a_call_nobody_answered_loads_nothing() {
        let mut messages = round("a", "web_extract", json!({"urls": ["x"]}), true);
        messages.extend(round(
            "b",
            "tool_search",
            json!({"names": ["web_search"]}),
            false,
        ));
        let loaded = loaded_in(&conversation(messages), &deferred());
        assert_eq!(loaded, ["web_extract"]);
    }

    #[test]
    fn too_many_names_load_nothing() {
        let many: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        let mut names = deferred();
        names.extend(many.clone());
        let mut asked = many;
        asked.push("web_search".into());
        let messages = round("a", "tool_search", json!({"names": asked}), true);
        assert!(loaded_in(&conversation(messages), &names).is_empty());
    }

    #[test]
    fn truncating_the_conversation_drops_the_later_loads() {
        let mut messages = round("a", "tool_search", json!({"names": ["web_search"]}), true);
        let cut = messages.len();
        messages.extend(round(
            "b",
            "tool_search",
            json!({"names": ["session_search"]}),
            true,
        ));
        assert_eq!(
            loaded_in(&conversation(messages.clone()), &deferred()).len(),
            2
        );
        messages.truncate(cut);
        assert_eq!(
            loaded_in(&conversation(messages), &deferred()),
            ["web_search"]
        );
    }

    #[test]
    fn the_prompt_lists_the_builtins_then_the_mcp_tools_past_the_first_few() {
        let tool = |name: &str| {
            Tool::new(
                name.to_string(),
                "Does a thing. More.",
                Arc::new(JsonObject::new()),
            )
        };
        let mcp = vec![tool("mcp__s__a"), tool("mcp__s__b"), tool("mcp__s__c")];
        let lines = prompt_entries(&mcp, 1);
        assert_eq!(lines.len(), BUILTIN.len() + 2);
        assert_eq!(
            lines[BUILTIN.len()],
            ("mcp__s__b".to_string(), "Does a thing".to_string())
        );
        assert_eq!(prompt_entries(&mcp, 3).len(), BUILTIN.len());
    }
}
