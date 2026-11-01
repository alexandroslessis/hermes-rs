//! `session_search`: recall over past conversations, ported from Hermes's
//! `tools/session_search_tool.py` (MIT). The shape follows the arguments: `query` = discovery
//! (FTS5, one session per hit, the top result hydrated), `session_id` + `around_message_id` =
//! scroll, `session_id` alone = read, nothing = browse recent sessions. No LLM calls: every
//! shape returns stored messages. (Hermes's profiles, lineage and links don't exist here.)

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use hermes_store::{SearchQuery, SearchRow, SearchSort, Store};
use serde_json::{Value, json};

use crate::{SessionTools, ToolOutput};

/// Per-message content cap for the read shape.
const READ_MAX_CONTENT: usize = 2000;
/// FTS rows scanned before deduplicating by session.
const DISCOVER_SCAN_LIMIT: usize = 300;
const EXCLUDE_SESSION_IDS_CAP: usize = 20;

fn clamp(value: Option<&Value>, default: i64, lo: i64, hi: i64) -> i64 {
    value
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
        .unwrap_or(default)
        .clamp(lo, hi)
}

fn when(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%B %d, %Y at %I:%M %p").to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// A message row slimmed for the model; `max` caps long content.
fn shape(row: &SearchRow, anchor: Option<i64>, max: Option<usize>) -> Value {
    let mut entry = json!({
        "id": row.id,
        "role": row.role,
        "content": row.content,
        "timestamp": row.created,
    });
    if let Some(calls) = &row.tool_calls {
        entry["tool_calls"] = calls.clone();
    }
    if anchor == Some(row.id) {
        entry["anchor"] = json!(true);
    }
    let chars = row.content.chars().count();
    if let Some(max) = max.filter(|&max| chars > max) {
        let cut: String = row.content.chars().take(max).collect();
        entry["content"] = json!(format!("{cut}…"));
        entry["content_truncated"] = json!(true);
        entry["original_content_chars"] = json!(chars);
    }
    entry
}

/// `after`/`before` bounds: an ISO date or datetime, or `7d` / `24h` / `2w` back from now.
fn parse_bound(value: Option<&Value>) -> Result<Option<i64>, String> {
    let Some(text) = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let lower = text.to_lowercase();
    if let Some(unit) = ["h", "d", "w"].iter().find(|u| lower.ends_with(*u)) {
        if let Ok(n) = lower.trim_end_matches(unit).trim().parse::<i64>() {
            let hours = match *unit {
                "h" => n,
                "d" => n * 24,
                _ => n * 24 * 7,
            };
            return Ok(Some(Utc::now().timestamp_millis() - hours * 3_600_000));
        }
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Ok(Some(t.timestamp_millis()));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(Some(t.and_utc().timestamp_millis()));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(date
            .and_hms_opt(0, 0, 0)
            .map(|t| t.and_utc().timestamp_millis()));
    }
    Err(format!(
        "Invalid time bound '{text}'. Use an ISO date/datetime (2026-06-01) or a relative duration (7d, 24h, 2w)."
    ))
}

fn error(message: impl Into<String>) -> ToolOutput {
    ToolOutput::error(message)
}

/// The anchored window of `rows` around `anchor_index`, plus bookends and counts.
struct View<'a> {
    window: Vec<&'a SearchRow>,
    bookend_start: Vec<&'a SearchRow>,
    bookend_end: Vec<&'a SearchRow>,
    before: usize,
    after: usize,
}

fn anchored_view(
    rows: &[SearchRow],
    anchor_index: usize,
    window: usize,
    bookend: usize,
) -> View<'_> {
    let start = anchor_index.saturating_sub(window);
    let end = (anchor_index + window + 1).min(rows.len());
    let conversational = |row: &&SearchRow| row.role == "user" || row.role == "assistant";
    let anchor_id = rows[anchor_index].id;
    View {
        window: rows[start..end]
            .iter()
            .filter(|row| row.id == anchor_id || conversational(row))
            .collect(),
        bookend_start: rows[..start]
            .iter()
            .filter(conversational)
            .filter(|row| !row.content.is_empty())
            .take(bookend)
            .collect(),
        bookend_end: {
            let mut tail: Vec<&SearchRow> = rows[end..]
                .iter()
                .rev()
                .filter(conversational)
                .filter(|row| !row.content.is_empty())
                .take(bookend)
                .collect();
            tail.reverse();
            tail
        },
        before: start,
        after: rows.len() - end,
    }
}

impl SessionTools {
    pub(crate) fn session_search(&self, args: &Value) -> ToolOutput {
        let Some(store) = self.history.clone() else {
            return error("Session search is not available: the session database is not open.");
        };
        let session_id = args
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(session_id) = session_id {
            return match args.get("around_message_id").filter(|v| !v.is_null()) {
                Some(anchor) => self.scroll(&store, session_id, anchor, args.get("window")),
                None => self.read_session(&store, session_id),
            };
        }
        let limit = clamp(args.get("limit"), 3, 1, 10) as usize;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if query.is_empty() {
            return self.browse(&store, limit);
        }
        let (after, before) = match (
            parse_bound(args.get("after")),
            parse_bound(args.get("before")),
        ) {
            (Ok(after), Ok(before)) => (after, before),
            (Err(e), _) | (_, Err(e)) => return error(e),
        };
        let sort = match args
            .get("sort")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("newest") => SearchSort::Newest,
            Some("oldest") => SearchSort::Oldest,
            _ => SearchSort::Relevance,
        };
        let roles: Vec<String> = args
            .get("role_filter")
            .and_then(Value::as_str)
            .map(|roles| {
                roles
                    .split(',')
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .collect()
            })
            .filter(|roles: &Vec<String>| !roles.is_empty())
            .unwrap_or_else(|| vec!["user".into(), "assistant".into()]);
        let full = args.get("detail").and_then(Value::as_str) == Some("full");
        let mut excluded: Vec<String> = args
            .get("exclude_session_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .take(EXCLUDE_SESSION_IDS_CAP)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // The current session is already in context.
        excluded.push(self.session_id.clone());
        self.discover(
            &store, query, &roles, limit, sort, full, &excluded, after, before,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn discover(
        &self,
        store: &Store,
        query: &str,
        roles: &[String],
        limit: usize,
        sort: SearchSort,
        full: bool,
        excluded: &[String],
        after: Option<i64>,
        before: Option<i64>,
    ) -> ToolOutput {
        let in_window = |ms: i64| after.is_none_or(|a| ms >= a) && before.is_none_or(|b| ms < b);
        let mut results = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        // A query naming a session's title finds that session first.
        let title = query.trim_matches(|c| c == '`' || c == '\'' || c == '"');
        if let Ok(Some(id)) = store.session_by_title(title) {
            let record = store.session(&id).ok().flatten();
            let rows = store.search_rows(&id).unwrap_or_default();
            if let Some(record) =
                record.filter(|r| !excluded.contains(&r.id) && in_window(r.created_at))
                && !rows.is_empty()
            {
                let view = anchored_view(&rows, 0, 5, 3);
                results.push(json!({
                    "session_id": id,
                    "when": when(record.created_at),
                    "model": record.model,
                    "title": record.title,
                    "matched_role": "session_title",
                    "match_message_id": rows[0].id,
                    "snippet": format!("Session title matched: {}", record.title.clone().unwrap_or_default()),
                    "bookend_start": [],
                    "messages": view.window.iter().map(|r| shape(r, Some(rows[0].id), Some(4000))).collect::<Vec<_>>(),
                    "bookend_end": view.bookend_end.iter().map(|r| shape(r, None, Some(1200))).collect::<Vec<_>>(),
                    "messages_before": 0,
                    "messages_after": view.after,
                    "detail": "full",
                }));
                seen.push(id);
            }
        }

        let hits = match store.search(&SearchQuery {
            query,
            roles,
            exclude_sessions: excluded,
            after_ms: after,
            before_ms: before,
            sort,
            limit: DISCOVER_SCAN_LIMIT,
        }) {
            Ok(hits) => hits,
            Err(e) => return error(format!("Search failed: {e:#}")),
        };
        if hits.is_empty() && results.is_empty() {
            return ToolOutput::ok(json!({
                "success": true, "mode": "discover", "query": query,
                "detail": if full { "full" } else { "adaptive" }, "results": [], "count": 0,
                "message": "No matching sessions found. FTS5 ANDs all terms by default — broaden with OR (`alpha OR beta`), exact-match with quoted phrases, exclude with NOT, or prefix-match with `deploy*`.",
            }));
        }
        let mut chosen = Vec::new();
        for hit in hits {
            if seen.len() >= limit {
                break;
            }
            if !seen.contains(&hit.session_id) {
                seen.push(hit.session_id.clone());
                chosen.push(hit);
            }
        }
        for hit in chosen {
            let Ok(rows) = store.search_rows(&hit.session_id) else {
                continue;
            };
            let Some(anchor_index) = rows.iter().position(|row| row.id == hit.id) else {
                continue;
            };
            let record = store.session(&hit.session_id).ok().flatten();
            // Adaptive: only the top-ranked result is hydrated in full.
            let hydrate = full || results.is_empty();
            let view = anchored_view(&rows, anchor_index, 5, 3);
            let messages: Vec<Value> = view
                .window
                .iter()
                .filter(|row| hydrate || row.id == hit.id)
                .map(|row| shape(row, Some(hit.id), Some(4000)))
                .collect();
            let bookend = |rows: &[&SearchRow]| -> Vec<Value> {
                if hydrate {
                    rows.iter().map(|r| shape(r, None, Some(1200))).collect()
                } else {
                    Vec::new()
                }
            };
            results.push(json!({
                "session_id": hit.session_id,
                "when": record.as_ref().map_or("unknown".into(), |r| when(r.created_at)),
                "model": record.as_ref().map_or("unknown".into(), |r| r.model.clone()),
                "title": record.as_ref().and_then(|r| r.title.clone()),
                "matched_role": hit.role,
                "match_message_id": hit.id,
                "snippet": hit.snippet,
                "bookend_start": bookend(&view.bookend_start),
                "messages": messages,
                "bookend_end": bookend(&view.bookend_end),
                "messages_before": view.before,
                "messages_after": view.after,
                "detail": if hydrate { "full" } else { "compact" },
            }));
        }
        let count = results.len();
        ToolOutput::ok(json!({
            "success": true,
            "mode": "discover",
            "query": query,
            "detail": if full { "full" } else { "adaptive" },
            "results": results,
            "count": count,
            "sessions_searched": seen.len(),
            "hint": "To read more around a compact result, scroll: session_search(session_id=..., around_message_id=match_message_id).",
        }))
    }

    fn read_session(&self, store: &Store, session_id: &str) -> ToolOutput {
        let Ok(Some(record)) = store.session(session_id) else {
            return error(format!("session_id not found: {session_id}"));
        };
        let rows = match store.search_rows(session_id) {
            Ok(rows) => rows,
            Err(e) => return error(format!("failed to load session: {e:#}")),
        };
        let (head, tail) = (20, 10);
        let total = rows.len();
        let truncated = total > head + tail;
        let shaped: Vec<Value> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| !truncated || *i < head || *i >= total - tail)
            .map(|(_, row)| shape(row, None, Some(READ_MAX_CONTENT)))
            .collect();
        let mut out = json!({
            "success": true,
            "mode": "read",
            "session_id": session_id,
            "session_meta": {"title": record.title, "when": when(record.created_at), "model": record.model},
            "message_count": total,
            "truncated": truncated,
            "messages": shaped,
        });
        if truncated {
            out["message"] = json!(format!(
                "Session has {total} messages; showing first {head} + last {tail}. Pass around_message_id (any id above) to scroll the middle."
            ));
        }
        ToolOutput::ok(out)
    }

    fn scroll(
        &self,
        store: &Store,
        session_id: &str,
        anchor: &Value,
        window: Option<&Value>,
    ) -> ToolOutput {
        let Some(anchor) = anchor
            .as_i64()
            .or_else(|| anchor.as_str()?.trim().parse().ok())
        else {
            return error("scroll requires integer around_message_id");
        };
        let window = clamp(window, 5, 1, 20) as usize;
        let owner = store.search_row_session(anchor).ok().flatten();
        if owner.as_deref() == Some(self.session_id.as_str()) {
            return error(
                "scroll rejected: anchor lives in the current session (already in your active context)",
            );
        }
        let Ok(Some(record)) = store.session(session_id) else {
            return error(format!("session_id not found: {session_id}"));
        };
        let rows = store.search_rows(session_id).unwrap_or_default();
        let Some(index) = rows.iter().position(|row| row.id == anchor) else {
            return error(format!(
                "around_message_id {anchor} not in session_id {session_id}"
            ));
        };
        let start = index.saturating_sub(window);
        let end = (index + window + 1).min(rows.len());
        ToolOutput::ok(json!({
            "success": true,
            "mode": "scroll",
            "session_id": session_id,
            "around_message_id": anchor,
            "session_meta": {"title": record.title, "when": when(record.created_at), "model": record.model},
            "window": window,
            "messages": rows[start..end].iter().map(|row| shape(row, Some(anchor), None)).collect::<Vec<_>>(),
            "messages_before": start,
            "messages_after": rows.len() - end,
            "hint": "Scroll forward: re-call with around_message_id = the LAST message's id; backward: the FIRST message's id (the boundary message repeats as an orientation marker). messages_before/messages_after < window means you've hit that end of the session.",
        }))
    }

    fn browse(&self, store: &Store, limit: usize) -> ToolOutput {
        let sessions = match store.list_sessions(limit + 1) {
            Ok(sessions) => sessions,
            Err(e) => return error(format!("Failed to list recent sessions: {e:#}")),
        };
        let results: Vec<Value> = sessions
            .into_iter()
            .filter(|s| s.id != self.session_id)
            .take(limit)
            .map(|s| {
                let rows = store.search_rows(&s.id).unwrap_or_default();
                let preview: String = rows
                    .iter()
                    .find(|row| row.role == "user")
                    .map(|row| row.content.chars().take(120).collect())
                    .unwrap_or_default();
                json!({
                    "session_id": s.id,
                    "title": s.title,
                    "started_at": when(s.created_at),
                    "last_active": when(s.updated_at),
                    "message_count": rows.len(),
                    "preview": preview,
                })
            })
            .collect();
        let count = results.len();
        ToolOutput::ok(json!({
            "success": true,
            "mode": "browse",
            "results": results,
            "count": count,
            "message": format!("Showing {count} most recent sessions. Pass a query= to search, or session_id+around_message_id to scroll."),
        }))
    }
}
