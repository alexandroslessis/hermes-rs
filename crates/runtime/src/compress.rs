//! Hermes's context compression (`agent/context_compressor.py`, MIT), lean mode: when the context
//! nears the window, old tool results become one-line stubs, the first messages and a token-budget
//! tail are kept, and everything between is replaced by a structured summary an LLM writes. A
//! later compaction updates that summary instead of starting over.
//!
//! Hermes rewrites the transcript; here nothing the user sees is lost. Compacted messages are
//! hidden from the model but stay in the chat, a stub replacing a tool result is an agent-only
//! copy next to the user-only original, and the handoff the model reads is an agent-only message.
//! A user-only notice marks where it happened and holds the summary.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use hermes_agent::events::AgentEvent;
use hermes_agent::operation::{Emitter, Operation, OperationResult, applied, not_applicable};
use hermes_llm::base::Provider;
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use hermes_llm::model::ModelConfig;
use hermes_prompts::compress as prompts;
use hermes_tools::redact::{Source, redact};
use rmcp::model::{CallToolResult, ContentBlock, Role};
use serde_json::Value;

use crate::ops::SharedTurnState;
use crate::session::{HermesEffect, TurnSession};

// ---- budgets ------------------------------------------------------------------------------------

/// `CHARS_PER_TOKEN` of Hermes's rough estimator.
const CHARS_PER_TOKEN: usize = 4;
/// Windows below `_SMALL_CTX_WINDOW_LIMIT` compress at 75% (`_SMALL_CTX_THRESHOLD_PERCENT`),
/// larger ones at the 50% default.
const SMALL_WINDOW: usize = 512_000;
const LEAN_TAIL_FLOOR_TOKENS: usize = 10_000;
const LEAN_TAIL_CAP_TOKENS: usize = 25_000;
const TAIL_MAX_CONTEXT_FRACTION: f64 = 0.20;
const TAIL_MAX_THRESHOLD_FRACTION: f64 = 0.50;
const MIN_SUMMARY_TOKENS: usize = 2_000;
const SUMMARY_TOKENS_CEILING: usize = 10_000;
const SUMMARY_RATIO: f64 = 0.20;
/// `protect_first_n`: the opening messages stay verbatim until the first compaction.
const PROTECT_FIRST_N: usize = 3;
/// `_PRUNE_MIN_CHARS`: shorter tool results aren't worth a stub.
const PRUNE_MIN_CHARS: usize = 200;
/// Lean mode demotes tail tool results older than the newest rounds when they're this long.
const TAIL_KEEP_TOOL_ROUNDS: usize = 6;
const TAIL_DEMOTE_MIN_CHARS: usize = 1_500;
/// Summarizer input limits (`_CONTENT_MAX`, `_CONTENT_HEAD`, `_CONTENT_TAIL`, `_TOOL_ARGS_*`,
/// `_SUMMARY_INPUT_MAX_CHARS`).
const CONTENT_MAX: usize = 6_000;
const CONTENT_HEAD: usize = 4_000;
const CONTENT_TAIL: usize = 1_500;
const TOOL_ARGS_MAX: usize = 1_500;
const TOOL_ARGS_HEAD: usize = 1_200;
const SUMMARY_INPUT_MAX_CHARS: usize = 160_000;
/// Window assumed when the model's is unknown (Hermes's `DEFAULT_FALLBACK_CONTEXT`).
pub const DEFAULT_CONTEXT_LENGTH: usize = 128_000;

/// The operation name the compaction's messages are noted under.
const NOTE: &str = "compaction";

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub context_length: usize,
    /// Compress once the request reaches this many tokens.
    pub threshold: usize,
    /// Tokens of recent messages kept verbatim.
    pub tail: usize,
    max_summary: usize,
}

impl Budget {
    pub fn new(context_length: Option<usize>) -> Self {
        let context_length = context_length
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_CONTEXT_LENGTH);
        let percent = if context_length < SMALL_WINDOW {
            0.75
        } else {
            0.50
        };
        let threshold = (context_length as f64 * percent) as usize;
        // `lean_tail_budget`: 2.5% of the window within the floor and cap, at most half the trigger.
        let share = (threshold as f64 * TAIL_MAX_THRESHOLD_FRACTION) as usize;
        let mut floor = LEAN_TAIL_FLOOR_TOKENS.min(share);
        let mut window_term = LEAN_TAIL_CAP_TOKENS.min(context_length / 40);
        if threshold > MIN_SUMMARY_TOKENS {
            window_term = window_term.min(share);
        } else {
            floor = LEAN_TAIL_FLOOR_TOKENS.min(share);
        }
        let tail = floor
            .max(window_term)
            .min((context_length as f64 * TAIL_MAX_CONTEXT_FRACTION) as usize)
            .max(1);
        Self {
            context_length,
            threshold,
            tail,
            max_summary: (context_length / 20).min(SUMMARY_TOKENS_CEILING),
        }
    }

    /// `_tail_soft_ceiling`: optional tail rows may overrun the budget by half.
    fn soft_ceiling(&self) -> usize {
        let ceiling = (self.tail as f64 * 1.5) as usize;
        ceiling
            .min((self.context_length as f64 * TAIL_MAX_CONTEXT_FRACTION) as usize)
            .max(self.tail)
    }

    /// `_compute_summary_budget`: a fifth of what is summarized, within the floor and ceiling.
    fn summary(&self, content_tokens: usize) -> usize {
        let budget = (content_tokens as f64 * SUMMARY_RATIO) as usize;
        MIN_SUMMARY_TOKENS.max(budget.min(self.max_summary))
    }
}

// ---- token estimates ----------------------------------------------------------------------------

pub fn estimate_text(text: &str) -> usize {
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Rough tokens of one message as the model sees it (text, tool calls, tool results).
pub fn estimate_message(message: &Message) -> usize {
    let chars: usize = message
        .content
        .iter()
        .map(|content| match content {
            MessageContent::Text(text) => text.text.len(),
            MessageContent::Thinking(thinking) => thinking.thinking.len(),
            MessageContent::ToolRequest(request) => request.tool_call.as_ref().map_or(0, |call| {
                call.name.len()
                    + call
                        .arguments
                        .as_ref()
                        .map_or(0, |args| Value::Object(args.clone()).to_string().len())
            }),
            MessageContent::ToolResponse(response) => {
                let images = response.tool_result.as_ref().map_or(0, |result| {
                    result
                        .content
                        .iter()
                        .filter(|block| matches!(block, ContentBlock::Image(_)))
                        .count()
                });
                response_text(content).len() + images * 1_600 * CHARS_PER_TOKEN
            }
            // Roughly what providers bill for an image of the embed size.
            MessageContent::Image(_) => 1_600 * CHARS_PER_TOKEN,
            _ => 0,
        })
        .sum();
    // A few tokens of per-message framing.
    chars.div_ceil(CHARS_PER_TOKEN) + 4
}

/// Tokens of the messages the model sees.
pub fn estimate_messages<'a>(messages: impl IntoIterator<Item = &'a Message>) -> usize {
    messages
        .into_iter()
        .filter(|message| message.is_agent_visible())
        .map(estimate_message)
        .sum()
}

/// Tokens of the tool definitions sent with every request.
pub fn tool_definition_tokens() -> usize {
    hermes_prompts::tools::tool_names()
        .filter_map(|name| hermes_prompts::tools::tool_schema(name).ok())
        .map(|schema| {
            schema.name.len()
                + schema.description.len()
                + Value::Object(schema.parameters).to_string().len()
        })
        .sum::<usize>()
        .div_ceil(CHARS_PER_TOKEN)
}

fn response_text(content: &MessageContent) -> String {
    match content.as_tool_response().map(|r| &r.tool_result) {
        Some(Ok(_)) => content.as_tool_response_text().unwrap_or_default(),
        Some(Err(error)) => error.message.to_string(),
        None => String::new(),
    }
}

// ---- message roles ------------------------------------------------------------------------------

/// The handoff the model reads in place of the compacted turns.
pub fn is_handoff(message: &Message) -> bool {
    message.metadata.operation_note(NOTE, "handoff").is_some()
}

/// The chat's marker of a compaction, carrying the summary.
pub fn notice(message: &Message) -> Option<CompactionNotice> {
    let value = message.metadata.operation_note(NOTE, "notice")?;
    serde_json::from_value(value.clone()).ok()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactionNotice {
    /// Messages the summary replaced.
    pub summarized: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub focus: Option<String>,
}

fn is_tool_results(message: &Message) -> bool {
    message.role == Role::User && message.is_tool_response()
}

/// A message the user typed (not tool results, a runtime nudge or a handoff).
fn is_real_user_turn(message: &Message) -> bool {
    message.role == Role::User
        && !message.is_tool_response()
        && !is_handoff(message)
        && message
            .content
            .iter()
            .any(|c| matches!(c, MessageContent::Text(t) if !t.text.trim().is_empty()))
}

fn has_tool_calls(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|c| matches!(c, MessageContent::ToolRequest(_)))
}

// ---- tool-result stubs --------------------------------------------------------------------------

fn json_object(text: &str) -> serde_json::Map<String, Value> {
    match serde_json::from_str(text) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

fn str_arg<'a>(args: &'a serde_json::Map<String, Value>, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn shown_arg(args: &serde_json::Map<String, Value>, key: &str, default: &str) -> String {
    match args.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => default.to_string(),
        Some(other) => other.to_string(),
    }
}

fn truncate_chars(text: &str, max: usize, keep: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(keep).collect::<String>())
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `_result_failure_suffix`: a failed call must not compress into the line a success would.
fn failure_suffix(content: &str, is_error: bool) -> String {
    let payload = json_object(content);
    let error = payload.get("error").filter(|e| !e.is_null());
    let failed = error.is_some() || payload.get("success") == Some(&Value::Bool(false)) || is_error;
    if !failed {
        return String::new();
    }
    let reason = match error {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None if is_error && payload.is_empty() => content.to_string(),
        None => String::new(),
    };
    let preview: String = reason
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    if preview.is_empty() {
        " FAILED".into()
    } else {
        format!(" FAILED: {preview}")
    }
}

/// `_summarize_tool_result`: the one line an old tool result becomes.
pub fn summarize_tool_result(
    name: &str,
    args: &serde_json::Map<String, Value>,
    content: &str,
    is_error: bool,
) -> String {
    let len = thousands(content.chars().count());
    let lines = if content.trim().is_empty() {
        0
    } else {
        content.matches('\n').count() + 1
    };
    let failed = failure_suffix(content, is_error);
    match name {
        "terminal" => {
            let command = truncate_chars(str_arg(args, "command"), 80, 77);
            let exit = json_object(content)
                .get("exit_code")
                .and_then(Value::as_i64)
                .map_or("?".to_string(), |code| code.to_string());
            format!("[terminal] ran `{command}` -> exit {exit}, {lines} lines output")
        }
        "read_file" => format!(
            "[read_file] read {} from line {} ({len} chars){failed}",
            shown_arg(args, "path", "?"),
            shown_arg(args, "offset", "1"),
        ),
        "write_file" if !failed.is_empty() => {
            format!("[write_file] {}{failed}", shown_arg(args, "path", "?"))
        }
        "write_file" => {
            let written = match args.get("content").and_then(Value::as_str) {
                Some(text) => (text.matches('\n').count() + 1).to_string(),
                None => "?".into(),
            };
            format!(
                "[write_file] wrote to {} ({written} lines)",
                shown_arg(args, "path", "?")
            )
        }
        "search_files" => {
            let count = json_object(content)
                .get("total_count")
                .and_then(Value::as_u64)
                .map_or("?".to_string(), |n| n.to_string());
            format!(
                "[search_files] {} search for '{}' in {} -> {count} matches",
                shown_arg(args, "target", "content"),
                shown_arg(args, "pattern", "?"),
                shown_arg(args, "path", "."),
            )
        }
        "patch" => format!(
            "[patch] {} in {} ({len} chars result){failed}",
            shown_arg(args, "mode", "replace"),
            shown_arg(args, "path", "?"),
        ),
        "skill_view" => format!(
            "[skill_view] name={}{failed} ({len} chars)",
            shown_arg(args, "name", "?")
        ),
        "skills_list" => {
            let category = str_arg(args, "category");
            let scope = if category.is_empty() {
                String::new()
            } else {
                format!(" category={category}")
            };
            let listed = json_object(content)
                .get("count")
                .and_then(Value::as_u64)
                .map(|n| format!(" {n} skills"))
                .unwrap_or_default();
            format!("[skills_list]{scope}{listed}{failed} ({len} chars)")
        }
        "memory" => format!(
            "[memory] {} on {}{failed}",
            shown_arg(args, "action", "?"),
            shown_arg(args, "target", "?")
        ),
        "todo_list" => "[todo] updated task list".into(),
        "process_manage" => format!(
            "[process] {} session={}{failed}",
            shown_arg(args, "action", "?"),
            shown_arg(args, "session_id", "?"),
        ),
        _ => {
            let first: String = args
                .iter()
                .take(2)
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!(" {key}={}", value.chars().take(40).collect::<String>())
                })
                .collect();
            format!("[{name}]{first} ({len} chars result){failed}")
        }
    }
}

/// `_lean_recovery_stub`, without Hermes's session_search hint.
fn demoted_stub(name: &str, chars: usize) -> String {
    format!(
        "[{name} output demoted at compaction — {} chars preserved in session history.]",
        thousands(chars)
    )
}

/// The tool name and arguments of every tool call, by call id.
fn tool_calls_by_id(
    messages: &[Message],
) -> std::collections::HashMap<String, (String, serde_json::Map<String, Value>)> {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(MessageContent::as_tool_request)
        .filter_map(|request| {
            let call = request.tool_call.as_ref().ok()?;
            Some((
                request.id.clone(),
                (
                    call.name.to_string(),
                    call.arguments.clone().unwrap_or_default(),
                ),
            ))
        })
        .collect()
}

/// A tool-results message with each long result replaced by `stub(name, args, text, is_error)`;
/// `None` when nothing changed.
fn stubbed(
    message: &Message,
    calls: &std::collections::HashMap<String, (String, serde_json::Map<String, Value>)>,
    min_chars: usize,
    stub: impl Fn(&str, &serde_json::Map<String, Value>, &str, bool) -> String,
) -> Option<Message> {
    let mut changed = false;
    let mut copy = message.clone();
    for content in &mut copy.content {
        let text = response_text(content);
        if text.chars().count() <= min_chars || text.starts_with('[') && text.len() < 400 {
            continue;
        }
        let MessageContent::ToolResponse(response) = content else {
            continue;
        };
        let (name, args) = calls
            .get(&response.id)
            .cloned()
            .unwrap_or_else(|| ("unknown".into(), serde_json::Map::new()));
        let is_error = match &response.tool_result {
            Ok(result) => result.is_error.unwrap_or(false),
            Err(_) => true,
        };
        let line = stub(&name, &args, &text, is_error);
        response.tool_result = Ok(if is_error {
            CallToolResult::error(vec![ContentBlock::text(line)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(line)])
        });
        changed = true;
    }
    changed.then_some(copy)
}

// ---- the summarizer's input ---------------------------------------------------------------------

/// `elide_middle` with Hermes's marker.
fn elide_middle(text: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= head + tail {
        return text.to_string();
    }
    let omitted = chars.len() - head - tail;
    format!(
        "{}⟪{} of {} chars omitted here by Hermes's context compressor.⟫{}",
        chars[..head].iter().collect::<String>(),
        thousands(omitted),
        thousands(chars.len()),
        chars[chars.len() - tail..].iter().collect::<String>(),
    )
}

fn strip_think_blocks(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn bounded(text: &str) -> String {
    let text = redact(text, Source::Output);
    if text.chars().count() > CONTENT_MAX {
        elide_middle(&text, CONTENT_HEAD, CONTENT_TAIL)
    } else {
        text
    }
}

/// `_serialize_records_for_summary`: one labeled, redacted record per message or tool result.
fn serialize(messages: &[Message]) -> Vec<String> {
    let mut records = Vec::new();
    for message in messages {
        if is_tool_results(message) {
            for content in &message.content {
                if let MessageContent::ToolResponse(response) = content {
                    records.push(format!(
                        "[TOOL RESULT {}]: {}",
                        response.id,
                        bounded(&response_text(content))
                    ));
                }
            }
            continue;
        }
        let text: String = message
            .content
            .iter()
            .filter_map(|content| match content {
                MessageContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut body = bounded(&strip_think_blocks(&text));
        let calls: Vec<String> = message
            .content
            .iter()
            .filter_map(MessageContent::as_tool_request)
            .map(|request| match &request.tool_call {
                Ok(call) => {
                    let args = redact(
                        &call
                            .arguments
                            .clone()
                            .map(|args| Value::Object(args).to_string())
                            .unwrap_or_default(),
                        Source::Output,
                    );
                    format!(
                        "  {}({})",
                        call.name,
                        truncate_chars(&args, TOOL_ARGS_MAX, TOOL_ARGS_HEAD)
                    )
                }
                Err(_) => "  ?(...)".into(),
            })
            .collect();
        if !calls.is_empty() {
            body.push_str(&format!("\n[Tool calls:\n{}\n]", calls.join("\n")));
        }
        let role = match message.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        records.push(format!("[{role}]: {body}"));
    }
    records
}

/// Joins the records within `SUMMARY_INPUT_MAX_CHARS`, sampling evenly when they don't fit
/// (lean mode's one bounded request).
fn summary_input(records: &[String]) -> String {
    let total: usize = records.iter().map(|r| r.len() + 2).sum();
    if total <= SUMMARY_INPUT_MAX_CHARS {
        return records.join("\n\n");
    }
    let keep = (records.len() * SUMMARY_INPUT_MAX_CHARS / total).max(1);
    let step = records.len() as f64 / keep as f64;
    let mut picked: Vec<usize> = (0..keep).map(|i| (i as f64 * step) as usize).collect();
    // The newest record always goes in.
    if picked.last() != Some(&(records.len() - 1)) {
        picked.push(records.len() - 1);
    }
    let mut out = Vec::new();
    let mut previous = None;
    let mut used = 0;
    for index in picked {
        if let Some(prev) = previous
            && index > prev + 1
        {
            out.push(format!("[... {} turns omitted ...]", index - prev - 1));
        }
        if used + records[index].len() > SUMMARY_INPUT_MAX_CHARS {
            break;
        }
        used += records[index].len() + 2;
        out.push(records[index].clone());
        previous = Some(index);
    }
    out.join("\n\n")
}

// ---- planning -----------------------------------------------------------------------------------

/// Where the compaction cuts, in indices of the full conversation.
struct Plan {
    /// Agent-visible messages summarized (head and tail excluded).
    middle: Vec<usize>,
    /// The first agent-visible message of the kept tail.
    tail_start: usize,
    /// Tool-results messages whose model copy gets stubs.
    stub_old: Vec<usize>,
    stub_tail: Vec<usize>,
    previous_summary: Option<String>,
}

fn plan(messages: &[Message], budget: &Budget) -> Option<Plan> {
    let visible: Vec<usize> = (0..messages.len())
        .filter(|&i| messages[i].is_agent_visible())
        .collect();
    let compacted_before = visible.iter().any(|&i| is_handoff(&messages[i]));
    // `_effective_protect_first_n`: the head decays to nothing after the first compaction so
    // early turns don't fossilize.
    let mut head = if compacted_before {
        0
    } else {
        PROTECT_FIRST_N.min(visible.len())
    };
    // Don't separate a tool call in the head from its results.
    while head < visible.len() && is_tool_results(&messages[visible[head]]) {
        head += 1;
    }
    if visible.len() <= head + 3 {
        return None;
    }
    // `_find_tail_cut_by_tokens`: walk back to the soft ceiling; the newest message always stays.
    let ceiling = budget.soft_ceiling();
    let mut cut = visible.len() - 1;
    let mut used = estimate_message(&messages[visible[cut]]);
    while cut > head {
        let cost = estimate_message(&messages[visible[cut - 1]]);
        if used + cost > ceiling {
            break;
        }
        used += cost;
        cut -= 1;
    }
    // Everything fits in the tail: cut with the raw budget so something can be summarized.
    if cut <= head {
        cut = visible.len() - 1;
        used = estimate_message(&messages[visible[cut]]);
        while cut > head + 1 {
            let cost = estimate_message(&messages[visible[cut - 1]]);
            if used + cost > budget.tail {
                break;
            }
            used += cost;
            cut -= 1;
        }
    }
    // `_align_boundary_backward`: tool results stay with their call.
    while cut > head && is_tool_results(&messages[visible[cut]]) {
        cut -= 1;
    }
    // Keep the latest user message (the active task) in the tail, unless the turn after it is
    // itself over the ceiling and has tool rounds to summarize (Hermes's oversized-turn split).
    if let Some(last_user) = (head..visible.len())
        .rev()
        .find(|&i| is_real_user_turn(&messages[visible[i]]))
        && last_user < cut
    {
        let anchored: usize = visible[last_user..]
            .iter()
            .map(|&i| estimate_message(&messages[i]))
            .sum();
        let has_rounds = visible[last_user..cut]
            .iter()
            .any(|&i| has_tool_calls(&messages[i]));
        if anchored <= ceiling || !has_rounds {
            cut = last_user;
        }
    }
    if cut <= head {
        return None;
    }
    let previous_summary = visible
        .iter()
        .rev()
        .find(|&&i| is_handoff(&messages[i]))
        .and_then(|&i| {
            prompts::summary_of_handoff(&messages[i].as_concat_text()).map(str::to_string)
        });
    let middle: Vec<usize> = visible[head..cut]
        .iter()
        .copied()
        .filter(|&i| !is_handoff(&messages[i]))
        .collect();
    if middle.is_empty() {
        return None;
    }
    // Old results feed the summarizer as stubs (Hermes prunes before it summarizes); the
    // head's become stubs for the model.
    let stub_old = visible[..cut]
        .iter()
        .copied()
        .filter(|&i| is_tool_results(&messages[i]))
        .collect();
    // Lean mode: tail results older than the newest rounds are demoted too.
    let tail_results: Vec<usize> = visible[cut..]
        .iter()
        .copied()
        .filter(|&i| is_tool_results(&messages[i]))
        .collect();
    let stub_tail = tail_results
        .len()
        .checked_sub(TAIL_KEEP_TOOL_ROUNDS)
        .map(|n| tail_results[..n].to_vec())
        .unwrap_or_default();
    Some(Plan {
        middle,
        tail_start: visible[cut],
        stub_old,
        stub_tail,
        previous_summary,
    })
}

// ---- compaction ---------------------------------------------------------------------------------

pub struct Summarizer {
    pub llm: Arc<dyn Provider>,
    pub model: String,
    pub context_length: Option<usize>,
}

pub struct Compacted {
    pub conversation: Conversation,
    pub notice: CompactionNotice,
}

/// Compacts `conversation`; `None` when there is nothing between the head and the tail.
/// `notice_at` is where the chat's marker goes (the end when `None`).
pub async fn compact(
    conversation: &Conversation,
    fixed_tokens: usize,
    summarizer: &Summarizer,
    focus: Option<&str>,
    notice_at: Option<usize>,
) -> Result<Option<Compacted>> {
    let budget = Budget::new(summarizer.context_length);
    let messages = conversation.messages();
    let Some(plan) = plan(messages, &budget) else {
        return Ok(None);
    };
    let tokens_before = fixed_tokens + estimate_messages(messages);
    let calls = tool_calls_by_id(messages);
    let prune = |index: usize| {
        stubbed(
            &messages[index],
            &calls,
            PRUNE_MIN_CHARS,
            |name, args, text, is_error| summarize_tool_result(name, args, text, is_error),
        )
    };
    // The summarizer reads the middle with old results already stubbed.
    let turns: Vec<Message> = plan
        .middle
        .iter()
        .map(|&i| {
            if plan.stub_old.contains(&i) {
                prune(i).unwrap_or_else(|| messages[i].clone())
            } else {
                messages[i].clone()
            }
        })
        .collect();
    // Whether the session has a user at all (the summarized slice may hold only replies).
    let has_user_turn = messages.iter().any(is_real_user_turn);
    let records = serialize(&turns);
    let summary_budget = budget.summary(estimate_messages(&turns));
    let focus = focus.map(|f| redact(f, Source::Output));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let prompt = prompts::summary_prompt(&prompts::SummaryRequest {
        turns: &summary_input(&records),
        previous_summary: plan.previous_summary.as_deref(),
        focus: focus.as_deref(),
        has_user_turn,
        summary_budget,
        today: &today,
    });
    let summary = summarize(summarizer, &prompt).await?;

    // Rebuild: the middle hidden from the model, stub copies beside the originals, the handoff
    // before the tail, the notice where the chat shows it.
    let middle: std::collections::HashSet<usize> = plan.middle.iter().copied().collect();
    let tail_stubs: std::collections::HashSet<usize> = plan.stub_tail.iter().copied().collect();
    let head_stubs: std::collections::HashSet<usize> = plan
        .stub_old
        .iter()
        .copied()
        .filter(|&i| i < plan.tail_start && !middle.contains(&i))
        .collect();
    let notice = CompactionNotice {
        summarized: plan.middle.len(),
        tokens_before,
        tokens_after: 0,
        focus: focus.clone(),
    };
    let mut handoff = Message::user()
        .with_text(prompts::handoff(&summary))
        .with_visibility(false, true)
        .with_generated_id_if_missing();
    handoff
        .metadata
        .set_operation_note(NOTE, "handoff", Value::Bool(true));
    let mut out: Vec<Message> = Vec::with_capacity(messages.len() + 4);
    for (index, message) in messages.iter().enumerate() {
        if index == plan.tail_start {
            out.push(handoff.clone());
        }
        // This compaction's handoff replaces the earlier ones, and summarized stub copies go.
        let is_copy = message.metadata.operation_note(NOTE, "copy").is_some();
        if is_handoff(message) || is_copy && middle.contains(&index) {
            continue;
        }
        let stub = if head_stubs.contains(&index) {
            prune(index)
        } else if tail_stubs.contains(&index) {
            stubbed(
                message,
                &calls,
                TAIL_DEMOTE_MIN_CHARS,
                |name, _, text, _| demoted_stub(name, text.chars().count()),
            )
        } else {
            None
        };
        match stub {
            Some(mut copy) => {
                out.push(hidden(message));
                copy.id = None;
                copy = copy
                    .with_visibility(false, true)
                    .with_generated_id_if_missing();
                copy.metadata
                    .set_operation_note(NOTE, "copy", Value::Bool(true));
                out.push(copy);
            }
            None if middle.contains(&index) => {
                out.push(hidden(message));
            }
            None => out.push(message.clone()),
        }
    }
    let tokens_after = fixed_tokens + estimate_messages(&out);
    // Hermes refuses a compaction that would grow the request.
    if tokens_after >= tokens_before {
        return Ok(None);
    }
    let notice = CompactionNotice {
        tokens_after,
        ..notice
    };
    let mut marker = Message::assistant()
        .with_text(summary)
        .with_visibility(true, false)
        .with_generated_id_if_missing();
    marker.metadata.set_operation_note(
        NOTE,
        "notice",
        serde_json::to_value(&notice).context("compaction notice")?,
    );
    // The original message the notice goes before, found again after the rebuild.
    let at = notice_at
        .and_then(|index| messages.get(index))
        .and_then(|message| message.id.as_deref())
        .and_then(|id| out.iter().position(|m| m.id.as_deref() == Some(id)))
        .unwrap_or(out.len());
    out.insert(at, marker);
    Ok(Some(Compacted {
        conversation: Conversation::new_unvalidated(out),
        notice,
    }))
}

/// A message hidden from the model by a compaction, noted so [`undo`] can bring it back.
fn hidden(message: &Message) -> Message {
    let mut hidden = message
        .clone()
        .with_visibility(message.is_user_visible(), false);
    hidden
        .metadata
        .set_operation_note(NOTE, "hidden", Value::Bool(true));
    hidden
}

/// After a rewind that cut off the handoff: the messages it summarized are the model's again,
/// its stub copies and notices go. `false` when the conversation still has its handoff (or
/// was never compacted).
pub fn undo(conversation: &mut Conversation) -> bool {
    let messages = conversation.messages_mut();
    let compacted = messages.iter().any(|m| {
        m.metadata.operation_note(NOTE, "hidden").is_some()
            || m.metadata.operation_note(NOTE, "copy").is_some()
    });
    if !compacted || messages.iter().any(is_handoff) {
        return false;
    }
    messages.retain(|m| m.metadata.operation_note(NOTE, "copy").is_none() && notice(m).is_none());
    for message in messages.iter_mut() {
        if message.metadata.operation_note(NOTE, "hidden").is_some() {
            message.metadata.agent_visible = true;
            if let Some(notes) = message.metadata.operations.as_mut()
                && let Some(note) = notes.get_mut(NOTE)
            {
                note.remove("hidden");
            }
        }
    }
    true
}

/// One summarizer call: the prompt as the only message, no tools; the reply's text without
/// reasoning, redacted (the summarizer may echo secrets).
async fn summarize(summarizer: &Summarizer, prompt: &str) -> Result<String> {
    let config = ModelConfig::new(&summarizer.model).with_context_limit(summarizer.context_length);
    let request = [Message::user().with_text(prompt)];
    let (reply, _) = summarizer
        .llm
        .complete(&config, "", &request, &[])
        .await
        .map_err(|error| anyhow::anyhow!("the summary request failed: {error}"))?;
    let text: String = reply
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let summary = redact(strip_think_blocks(&text).trim(), Source::Output);
    anyhow::ensure!(!summary.is_empty(), "the summary model returned no text");
    Ok(summary)
}

// ---- the turn step ------------------------------------------------------------------------------

/// Rebuilds the session's frozen system prompt (Hermes refreshes it at the compaction boundary).
pub type PromptRebuild = Arc<dyn Fn() -> Result<String> + Send + Sync>;

/// Compresses before a model call once the request would pass the threshold. A failed summary
/// leaves the context as it is for the rest of the turn (Hermes's cooldown), and a compaction
/// that didn't get under the threshold isn't repeated until the conversation grows.
pub struct AutoCompress {
    pub state: SharedTurnState,
    pub summarizer: Summarizer,
    pub rebuild_prompt: PromptRebuild,
}

#[async_trait]
impl Operation<TurnSession, HermesEffect> for AutoCompress {
    fn name(&self) -> &'static str {
        "compress"
    }

    async fn run(
        &self,
        session: &TurnSession,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<HermesEffect>> {
        // Only right before a model call: the last message the model sees is from the user side.
        let next_is_inference = conversation
            .messages()
            .iter()
            .rev()
            .find(|message| message.is_agent_visible())
            .is_some_and(|message| message.role == Role::User);
        {
            let state = self.state.lock();
            if !next_is_inference
                || state.summary_requested
                || state.compression_failed
                || state.compacted_at == Some(conversation.len())
            {
                return not_applicable();
            }
        }
        let budget = Budget::new(self.summarizer.context_length);
        let fixed = estimate_text(&session.system_prompt) + tool_definition_tokens();
        let tokens = session.context_tokens(conversation, fixed);
        if tokens < budget.threshold {
            return not_applicable();
        }
        tracing::info!(tokens, threshold = budget.threshold, "compressing context");
        emit.emit(AgentEvent::Compacting);
        // The notice goes above the message that started this turn.
        let kickoff = conversation
            .messages()
            .iter()
            .rposition(|message| is_real_user_turn(message) && message.is_user_visible());
        let result = compact(conversation, fixed, &self.summarizer, None, kickoff).await;
        let compacted = match result {
            Ok(Some(compacted)) => compacted,
            Ok(None) => {
                self.state.lock().compacted_at = Some(conversation.len());
                emit.emit(AgentEvent::CompactionFailed(None));
                return not_applicable();
            }
            Err(error) => {
                tracing::warn!("context compression failed: {error:#}");
                self.state.lock().compression_failed = true;
                emit.emit(AgentEvent::CompactionFailed(Some(format!("{error:#}"))));
                return not_applicable();
            }
        };
        self.state.lock().compacted_at = Some(compacted.conversation.len());
        let system_prompt = (self.rebuild_prompt)().ok();
        applied([HermesEffect::Compacted {
            conversation: compacted.conversation,
            system_prompt,
            notice: compacted.notice,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;

    fn tool_round(id: &str, command: &str, output: &str) -> [Message; 2] {
        let mut args = serde_json::Map::new();
        args.insert("command".into(), Value::String(command.into()));
        let call = CallToolRequestParams::new("terminal").with_arguments(args);
        [
            Message::assistant().with_tool_request(id, Ok(call)),
            Message::user().with_tool_response(
                id,
                Ok(CallToolResult::success(vec![ContentBlock::text(output)])),
            ),
        ]
    }

    #[test]
    fn budgets_follow_hermes() {
        let small = Budget::new(Some(200_000));
        assert_eq!(small.threshold, 150_000);
        assert_eq!(small.tail, 10_000);
        let large = Budget::new(Some(1_000_000));
        assert_eq!(large.threshold, 500_000);
        assert_eq!(large.tail, 25_000);
        assert_eq!(large.summary(1_000), MIN_SUMMARY_TOKENS);
        assert_eq!(large.summary(1_000_000), SUMMARY_TOKENS_CEILING);
    }

    #[test]
    fn tool_results_become_one_line() {
        let mut args = serde_json::Map::new();
        args.insert("command".into(), Value::String("cargo test".into()));
        let line = summarize_tool_result(
            "terminal",
            &args,
            "{\"output\":\"a\\nb\",\"exit_code\":0}",
            false,
        );
        assert_eq!(
            line,
            "[terminal] ran `cargo test` -> exit 0, 1 lines output"
        );
        let mut args = serde_json::Map::new();
        args.insert("path".into(), Value::String("a.rs".into()));
        let line = summarize_tool_result("write_file", &args, "{\"error\":\"denied\"}", true);
        assert_eq!(line, "[write_file] a.rs FAILED: denied");
    }

    #[test]
    fn plan_keeps_head_tail_and_the_last_user_message() {
        let big = "x".repeat(40_000);
        let mut messages = vec![
            Message::user().with_text("first"),
            Message::assistant().with_text("ok"),
        ];
        for n in 0..6 {
            messages.extend(tool_round(&format!("c{n}"), "dir", &big));
        }
        messages.push(Message::user().with_text("latest"));
        let plan = plan(&messages, &Budget::new(Some(100_000))).unwrap();
        // Head: the first two messages plus the first round's call needs its result.
        assert!(!plan.middle.contains(&0) && !plan.middle.contains(&1));
        // Tail: the newest round (10k tokens fits the 15k soft ceiling) and the question.
        assert_eq!(plan.tail_start, messages.len() - 3);
        assert!(plan.middle.iter().all(|&i| i < plan.tail_start));
        assert!(!is_tool_results(&messages[plan.tail_start]));
    }

    #[test]
    fn nothing_to_plan_for_a_short_conversation() {
        let messages = vec![
            Message::user().with_text("hi"),
            Message::assistant().with_text("hello"),
        ];
        assert!(plan(&messages, &Budget::new(Some(100_000))).is_none());
    }

    #[test]
    fn serialized_turns_are_labeled_and_bounded() {
        let [call, result] = tool_round("c1", "dir", &"y".repeat(10_000));
        let records = serialize(&[Message::user().with_text("do it"), call, result]);
        assert_eq!(records[0], "[USER]: do it");
        assert!(
            records[1]
                .starts_with("[ASSISTANT]: \n[Tool calls:\n  terminal({\"command\":\"dir\"})")
        );
        assert!(records[2].starts_with("[TOOL RESULT c1]: yyyy"));
        assert!(records[2].contains("chars omitted here"));
    }
}
