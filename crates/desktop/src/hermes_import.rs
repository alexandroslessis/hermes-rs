//! Sessions from the original Hermes, imported: its `state.db` (and each profile's) is read
//! without being changed, and a session becomes one of ours with its title, folder, times and
//! conversation (user and assistant text, tool calls and their results).
//!
//! The page's Import screen (Hermes's `session.foreign.list/preview/import`, built for Claude Code
//! and Codex sessions) lists them, and `/import-hermes` imports them all. A session imported once
//! is remembered (`hermes-imports.json`) and not imported again. Imported sessions talk to the
//! current default endpoint and model: Hermes's providers may not exist here.
//!
//! Hermes: `hermes_state_common.py` (the schema), `hermes_constants.py` (`get_hermes_home`:
//! `HERMES_HOME`, else `%LOCALAPPDATA%\hermes` on Windows, else `~/.hermes`),
//! `tui_gateway/methods_session_foreign.py` (the RPCs).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use hermes_llm::conversation::message::{Message, MessageContent};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, Role};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};

/// Where imports are remembered: Hermes session key → our session id.
const IMPORTS: &str = "hermes-imports.json";
/// Sessions per page of the Import screen.
const PAGE: usize = 25;
/// Messages a preview shows.
const PREVIEW: usize = 40;

/// One Hermes database: the default home's or a profile's.
#[derive(Clone, Debug)]
pub struct Source {
    pub profile: String,
    pub db: PathBuf,
}

impl Source {
    fn label(&self) -> String {
        if self.profile == "default" {
            "Hermes".into()
        } else {
            format!("Hermes ({})", self.profile)
        }
    }
}

/// Hermes's home: `HERMES_HOME`, else the platform default, else `~/.hermes` (older installs).
fn homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|h| !h.is_empty()) {
        homes.push(PathBuf::from(home));
    }
    if let Some(local) = dirs::data_local_dir() {
        homes.push(local.join("hermes"));
    }
    if let Some(user) = dirs::home_dir() {
        homes.push(user.join(".hermes"));
    }
    homes
}

/// Every Hermes database here: each home's `state.db` and its profiles'.
pub fn sources() -> Vec<Source> {
    sources_in(&homes())
}

fn sources_in(homes: &[PathBuf]) -> Vec<Source> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for home in homes {
        let db = home.join("state.db");
        if db.is_file() && seen.insert(db.clone()) {
            out.push(Source { profile: "default".into(), db });
        }
        let Ok(profiles) = std::fs::read_dir(home.join("profiles")) else {
            continue;
        };
        let mut profiles: Vec<PathBuf> = profiles.flatten().map(|e| e.path()).collect();
        profiles.sort();
        for profile in profiles {
            let db = profile.join("state.db");
            let name = profile.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if db.is_file() && !name.starts_with('.') && seen.insert(db.clone()) {
                out.push(Source { profile: name, db });
            }
        }
    }
    out
}

/// The database, read-only (Hermes may be running; nothing of it is changed).
fn open(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
}

fn columns(conn: &Connection, table: &str) -> HashSet<String> {
    let mut statement = match conn.prepare(&format!("PRAGMA table_info({table})")) {
        Ok(statement) => statement,
        Err(_) => return HashSet::new(),
    };
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The SQL condition for a message row in the conversation's full history: current rows and
/// those compression folded away, not compression summaries (older schemas have none of these).
fn history_rows(message_columns: &HashSet<String>) -> String {
    let mut conditions = vec!["role IN ('user', 'assistant', 'tool')".to_string()];
    if message_columns.contains("active") && message_columns.contains("compacted") {
        conditions.push("(active = 1 OR compacted = 1)".into());
    }
    if message_columns.contains("_compressed_summary") {
        conditions.push("_compressed_summary = 0".into());
    }
    conditions.join(" AND ")
}

/// A content column's text: plain, or the text parts of a JSON parts list (images noted).
fn content_text(content: Option<String>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let trimmed = content.trim_start();
    if trimmed.starts_with('[') {
        if let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(trimmed) {
            if parts.iter().all(|p| p.get("type").is_some()) {
                return parts
                    .iter()
                    .map(|part| match part["type"].as_str() {
                        Some("text" | "input_text" | "output_text") => part["text"].as_str().unwrap_or_default().to_string(),
                        Some(kind) if kind.contains("image") => "[image]".into(),
                        Some(kind) => format!("[{kind}]"),
                        None => String::new(),
                    })
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
    }
    content
}

/// One Hermes session in the Import screen's list (`ForeignSession`).
fn summary(conn: &Connection, source: &Source, ours: &HashMap<String, String>) -> rusqlite::Result<Vec<(f64, Value)>> {
    let session_columns = columns(conn, "sessions");
    let message_columns = columns(conn, "messages");
    let has = |c: &str| session_columns.contains(c);
    let title = if has("title") { "title" } else { "NULL" };
    let cwd = if has("cwd") { "cwd" } else { "NULL" };
    let last = if has("last_activity_at") { "COALESCE(last_activity_at, ended_at, started_at)" } else { "COALESCE(ended_at, started_at)" };
    let rows = history_rows(&message_columns);
    let sql = format!(
        "SELECT id, {title}, {cwd}, {last},
                (SELECT count(*) FROM messages m WHERE m.session_id = s.id AND m.role = 'user' AND {rows}),
                (SELECT content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' AND {rows} ORDER BY id LIMIT 1)
         FROM sessions s"
    );
    let mut statement = conn.prepare(&sql)?;
    let found = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in found.flatten() {
        let (id, title, cwd, last, turns, first) = row;
        if turns == 0 {
            continue;
        }
        let first = content_text(first);
        let key = key(source, &id);
        let excerpt: String = first.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect();
        let title = title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| excerpt.chars().take(60).collect());
        out.push((
            last,
            json!({
                "id": key, "source": "hermes", "label": source.label(), "title": title, "cwd": cwd,
                "mtime": last, "turn_count": turns, "excerpt": excerpt,
                "already_imported": ours.contains_key(&key),
            }),
        ));
    }
    Ok(out)
}

fn key(source: &Source, session: &str) -> String {
    format!("hermes:{}:{session}", source.profile)
}

fn parse_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix("hermes:")?;
    let (profile, session) = rest.split_once(':')?;
    Some((profile.to_string(), session.to_string()))
}

fn find(key: &str) -> Result<(Source, String), String> {
    let (profile, session) = parse_key(key).ok_or_else(|| format!("Not a Hermes session: {key}"))?;
    let source = sources()
        .into_iter()
        .find(|s| s.profile == profile)
        .ok_or_else(|| format!("Hermes's {profile} profile wasn't found on this computer."))?;
    Ok((source, session))
}

fn load_imports(data_dir: &Path) -> HashMap<String, String> {
    std::fs::read(data_dir.join(IMPORTS))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_imports(data_dir: &Path, imports: &HashMap<String, String>) {
    if let Ok(bytes) = serde_json::to_vec_pretty(imports) {
        let _ = std::fs::write(data_dir.join(IMPORTS), bytes);
    }
}

/// `session.foreign.list` `{source, offset}` (`ForeignPage`): the Hermes sessions here, newest
/// first. Claude Code and Codex sessions aren't read (none listed).
pub fn list(data_dir: &Path, source: Option<&str>, offset: usize) -> Value {
    let host = whoami();
    if matches!(source, Some("claude" | "codex")) {
        return json!({"sessions": [], "next_offset": null, "host": host, "unreadable": 0});
    }
    let ours = load_imports(data_dir);
    let mut all = Vec::new();
    let mut unreadable = 0;
    for source in sources() {
        match open(&source.db).and_then(|conn| summary(&conn, &source, &ours)) {
            Ok(rows) => all.extend(rows),
            Err(error) => {
                tracing::warn!("reading {}: {error}", source.db.display());
                unreadable += 1;
            }
        }
    }
    all.sort_by(|a, b| b.0.total_cmp(&a.0));
    let page: Vec<Value> = all.iter().skip(offset).take(PAGE).map(|(_, row)| row.clone()).collect();
    let next = (offset + PAGE < all.len()).then_some(offset + PAGE);
    json!({"sessions": page, "next_offset": next, "host": host, "unreadable": unreadable})
}

fn whoami() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this computer".into())
}

/// A Hermes message row.
struct Row {
    role: String,
    content: String,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
    timestamp: f64,
}

fn rows(conn: &Connection, session: &str) -> rusqlite::Result<Vec<Row>> {
    let filter = history_rows(&columns(conn, "messages"));
    let mut statement = conn.prepare(&format!(
        "SELECT role, content, tool_call_id, tool_calls, timestamp FROM messages
         WHERE session_id = ?1 AND {filter} ORDER BY id"
    ))?;
    let found = statement.query_map([session], |row| {
        Ok(Row {
            role: row.get(0)?,
            content: content_text(row.get(1)?),
            tool_call_id: row.get(2)?,
            tool_calls: row.get(3)?,
            timestamp: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
        })
    })?;
    Ok(found.flatten().collect())
}

/// Hermes's rows as our conversation. Every tool call ends up with a result (a placeholder for
/// one the session never recorded, an interrupted turn), and results without their call are
/// dropped, so the history is valid for the next request.
fn conversation(rows: &[Row]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut open_calls: Vec<String> = Vec::new();
    let close = |out: &mut Vec<Message>, open_calls: &mut Vec<String>, results: Option<Message>| {
        let mut message = results.unwrap_or_else(Message::user);
        let answered: HashSet<String> = message.get_tool_response_ids().into_iter().map(str::to_string).collect();
        for id in open_calls.drain(..) {
            if !answered.contains(&id) {
                message = message.with_tool_response(
                    id,
                    Ok(CallToolResult::error(vec![ContentBlock::text(
                        "No result was recorded for this call (imported from Hermes).",
                    )])),
                );
            }
        }
        if !message.content.is_empty() {
            out.push(message.with_generated_id_if_missing());
        }
    };
    let mut index = 0;
    while index < rows.len() {
        let row = &rows[index];
        let created = row.timestamp as i64;
        match row.role.as_str() {
            "tool" => {
                // A run of results, answering the open calls.
                let mut results = Message::user();
                results.created = created;
                while index < rows.len() && rows[index].role == "tool" {
                    let result = &rows[index];
                    if let Some(id) = result.tool_call_id.as_deref().filter(|id| open_calls.iter().any(|c| c == id)) {
                        results = results.with_tool_response(
                            id.to_string(),
                            Ok(CallToolResult::success(vec![ContentBlock::text(result.content.clone())])),
                        );
                    }
                    index += 1;
                }
                close(&mut out, &mut open_calls, Some(results));
                continue;
            }
            "user" => {
                if !open_calls.is_empty() {
                    close(&mut out, &mut open_calls, None);
                }
                if !row.content.trim().is_empty() {
                    let mut message = Message::user().with_text(row.content.clone());
                    message.created = created;
                    out.push(message.with_generated_id_if_missing());
                }
            }
            "assistant" => {
                if !open_calls.is_empty() {
                    close(&mut out, &mut open_calls, None);
                }
                let mut message = Message::assistant();
                message.created = created;
                if !row.content.trim().is_empty() {
                    message = message.with_text(row.content.clone());
                }
                let calls: Vec<Value> = row
                    .tool_calls
                    .as_deref()
                    .and_then(|calls| serde_json::from_str(calls).ok())
                    .unwrap_or_default();
                for call in calls {
                    let (Some(id), Some(name)) = (call["id"].as_str(), call["function"]["name"].as_str()) else {
                        continue;
                    };
                    let arguments = match &call["function"]["arguments"] {
                        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
                        other => Some(other.clone()),
                    }
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                    message = message.with_tool_request(
                        id.to_string(),
                        Ok(CallToolRequestParams::new(name.to_string()).with_arguments(arguments)),
                    );
                    open_calls.push(id.to_string());
                }
                if !message.content.is_empty() {
                    out.push(message.with_generated_id_if_missing());
                }
            }
            _ => {}
        }
        index += 1;
    }
    if !open_calls.is_empty() {
        close(&mut out, &mut open_calls, None);
    }
    out
}

/// `session.foreign.preview` `{id}` (`ForeignPreview`): the conversation's text.
pub fn preview(data_dir: &Path, id: &str) -> Result<Value, String> {
    let (source, session) = find(id)?;
    let conn = open(&source.db).map_err(|e| e.to_string())?;
    let rows = rows(&conn, &session).map_err(|e| e.to_string())?;
    let texts: Vec<Value> = rows
        .iter()
        .filter(|r| matches!(r.role.as_str(), "user" | "assistant") && !r.content.trim().is_empty())
        .map(|r| json!({"role": r.role, "content": r.content}))
        .collect();
    let cwd: Option<String> = conn
        .query_row("SELECT cwd FROM sessions WHERE id = ?1", [&session], |row| row.get(0))
        .optional()
        .ok()
        .flatten()
        .flatten();
    Ok(json!({
        "truncated": texts.len() > PREVIEW,
        "total": texts.len(),
        "messages": texts.into_iter().take(PREVIEW).collect::<Vec<_>>(),
        "already_imported": load_imports(data_dir).get(id),
        "cwd": cwd,
    }))
}

/// What an import needs of the app: making a session, and where it may work.
pub struct Target<'a> {
    pub runtime: &'a hermes_runtime::Runtime,
    pub provider: hermes_runtime::ProviderConfig,
    pub model: String,
    pub data_dir: &'a Path,
}

/// `session.foreign.import` `{id}`: the session, made one of ours (or the one made before).
pub fn import(target: &Target, id: &str) -> Result<Value, String> {
    let mut imports = load_imports(target.data_dir);
    if let Some(ours) = imports.get(id) {
        if target.runtime.store().session(ours).ok().flatten().is_some() {
            return Ok(json!({"session_id": ours, "already_imported": true}));
        }
    }
    let (source, session) = find(id)?;
    let conn = open(&source.db).map_err(|e| e.to_string())?;
    let session_columns = columns(&conn, "sessions");
    let has = |c: &str| session_columns.contains(c);
    let sql = format!(
        "SELECT {}, {}, started_at, {} FROM sessions WHERE id = ?1",
        if has("title") { "title" } else { "NULL" },
        if has("cwd") { "cwd" } else { "NULL" },
        if has("last_activity_at") { "COALESCE(last_activity_at, ended_at, started_at)" } else { "COALESCE(ended_at, started_at)" },
    );
    let (title, cwd, started, last): (Option<String>, Option<String>, f64, Option<f64>) = conn
        .query_row(&sql, [&session], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|e| format!("Hermes session {session}: {e}"))?;
    let messages = conversation(&rows(&conn, &session).map_err(|e| e.to_string())?);
    if messages.is_empty() {
        return Err("That Hermes session has no messages to import.".into());
    }
    let folder = cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("C:\\"));
    let made = target
        .runtime
        .create_session(&target.provider, &target.model, None, &folder)
        .map_err(|e| format!("{e:#}"))?;
    let store = target.runtime.store();
    store.replace_messages(&made.id, &messages).map_err(|e| format!("{e:#}"))?;
    let first_user = messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|c| match c {
                    MessageContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    let title = title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| first_user.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect());
    if !title.is_empty() {
        let _ = store.set_title(&made.id, &title);
    }
    let started_ms = (started * 1000.0) as i64;
    let last_ms = (last.unwrap_or(started) * 1000.0) as i64;
    let _ = store.set_times(&made.id, started_ms, last_ms.max(started_ms));
    imports.insert(id.to_string(), made.id.clone());
    save_imports(target.data_dir, &imports);
    Ok(json!({"session_id": made.id, "already_imported": false}))
}

/// `/import-hermes`: every Hermes session not imported yet. `(imported, already, failed)`.
pub fn import_all(target: &Target) -> (usize, usize, Vec<String>) {
    let ours = load_imports(target.data_dir);
    let (mut imported, mut already, mut failed) = (0, 0, Vec::new());
    for source in sources() {
        let Ok(conn) = open(&source.db) else {
            failed.push(format!("{} (unreadable)", source.db.display()));
            continue;
        };
        let Ok(rows) = summary(&conn, &source, &ours) else {
            failed.push(format!("{} (unreadable)", source.db.display()));
            continue;
        };
        drop(conn);
        for (_, row) in rows {
            let id = row["id"].as_str().unwrap_or_default();
            match import(target, id) {
                Ok(result) if result["already_imported"] == true => already += 1,
                Ok(_) => imported += 1,
                Err(error) => failed.push(format!("{}: {error}", row["title"].as_str().unwrap_or(id))),
            }
        }
    }
    (imported, already, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, content: &str, call: Option<&str>, calls: Option<&str>) -> Row {
        Row {
            role: role.into(),
            content: content.into(),
            tool_call_id: call.map(str::to_string),
            tool_calls: calls.map(str::to_string),
            timestamp: 1_700_000_000.0,
        }
    }

    #[test]
    fn rows_become_a_valid_conversation() {
        let calls = r#"[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}},
                        {"id":"c2","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"ls\"}"}}]"#;
        let messages = conversation(&[
            row("user", "look at a.txt", None, None),
            row("assistant", "", None, Some(calls)),
            row("tool", "hello", Some("c1"), None),
            row("tool", "orphan", Some("zz"), None),
            row("assistant", "It says hello.", None, None),
            row("user", "and now?", None, None),
            row("assistant", "Running.", None, Some(r#"[{"id":"c3","function":{"name":"terminal","arguments":"{}"}}]"#)),
        ]);
        let roles: Vec<Role> = messages.iter().map(|m| m.role.clone()).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::User, Role::Assistant, Role::User, Role::Assistant, Role::User]);
        // c1 answered, c2 given a placeholder, the orphan dropped.
        let ids = messages[2].get_tool_response_ids();
        assert_eq!(ids, HashSet::from(["c1", "c2"]));
        assert!(format!("{:?}", messages[2]).contains("No result was recorded"));
        // The interrupted last call gets a placeholder too.
        assert_eq!(messages[6].get_tool_response_ids(), HashSet::from(["c3"]));
        assert_eq!(messages[0].created, 1_700_000_000);
    }

    #[test]
    fn multimodal_content_reads_as_text() {
        assert_eq!(
            content_text(Some(r#"[{"type":"text","text":"what is this"},{"type":"image_url","image_url":{"url":"x"}}]"#.into())),
            "what is this\n[image]"
        );
        assert_eq!(content_text(Some("[not json".into())), "[not json");
        assert_eq!(content_text(None), "");
    }

    #[test]
    fn homes_and_profiles_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("hermes");
        std::fs::create_dir_all(home.join("profiles/work")).unwrap();
        std::fs::create_dir_all(home.join("profiles/.staging")).unwrap();
        std::fs::write(home.join("state.db"), b"").unwrap();
        std::fs::write(home.join("profiles/work/state.db"), b"").unwrap();
        std::fs::write(home.join("profiles/.staging/state.db"), b"").unwrap();
        let found = sources_in(&[home.clone(), home.clone()]);
        let profiles: Vec<&str> = found.iter().map(|s| s.profile.as_str()).collect();
        assert_eq!(profiles, ["default", "work"]);
        assert_eq!(found[1].label(), "Hermes (work)");
        assert_eq!(parse_key("hermes:work:abc:d"), Some(("work".into(), "abc:d".into())));
    }
}
