//! Full-text search over past conversations (what Hermes's `session_search` reads). Every message
//! the user saw gets a row in `search_rows` with a stable integer id, its role (`user`,
//! `assistant` or `tool`) and its text; an FTS5 index over that text is kept in sync by triggers.
//! Runtime scaffolding hidden from the user (nudges, steer markers) is not indexed.

use anyhow::Result;
use hermes_llm::conversation::message::{Message, MessageContent};
use rmcp::model::Role;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) const SEARCH_SCHEMA: &str = "
CREATE TABLE search_rows (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_calls TEXT,
    created INTEGER NOT NULL,
    UNIQUE (session_id, seq)
);
CREATE VIRTUAL TABLE search_fts USING fts5(content, content='search_rows', content_rowid='id');
CREATE TRIGGER search_rows_ai AFTER INSERT ON search_rows BEGIN
    INSERT INTO search_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER search_rows_ad AFTER DELETE ON search_rows BEGIN
    INSERT INTO search_fts(search_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER search_rows_au AFTER UPDATE ON search_rows BEGIN
    INSERT INTO search_fts(search_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO search_fts(rowid, content) VALUES (new.id, new.content);
END;
";

/// One stored message as session search shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    /// `[{name, arguments}]` for an assistant message that called tools.
    pub tool_calls: Option<Value>,
    /// Unix seconds.
    pub created: i64,
}

/// One FTS match.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Relevance,
    Newest,
    Oldest,
}

pub struct SearchQuery<'a> {
    pub query: &'a str,
    pub roles: &'a [String],
    /// The asking session: its own messages are already in context.
    pub exclude_sessions: &'a [String],
    /// Session creation bounds, in milliseconds.
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub sort: SearchSort,
    pub limit: usize,
}

/// `(role, text, tool calls)` for a message the user saw, `None` for hidden scaffolding.
fn row_for(message: &Message) -> Option<(&'static str, String, Option<Value>)> {
    if !message.is_user_visible() {
        return None;
    }
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    let mut tool_output = false;
    for content in &message.content {
        match content {
            MessageContent::Text(text) => texts.push(text.text.clone()),
            MessageContent::ToolRequest(request) => {
                if let Ok(call) = &request.tool_call {
                    calls.push(json!({"name": call.name, "arguments": call.arguments}));
                }
            }
            MessageContent::ToolResponse(_) => {
                tool_output = true;
                texts.extend(content.as_tool_response_text());
            }
            _ => {}
        }
    }
    let role = match (message.role.clone(), tool_output) {
        (_, true) => "tool",
        (Role::User, false) => "user",
        (Role::Assistant, false) => "assistant",
    };
    let calls = (!calls.is_empty()).then(|| Value::Array(calls));
    Some((role, texts.join("\n"), calls))
}

/// Keeps the search row of the message at `seq` in step with the message.
pub(crate) fn index_message(
    conn: &Connection,
    session_id: &str,
    seq: usize,
    message: &Message,
) -> Result<()> {
    match row_for(message) {
        Some((role, content, calls)) => {
            conn.execute(
                "INSERT INTO search_rows (session_id, seq, role, content, tool_calls, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id, seq) DO UPDATE SET role = excluded.role,
                    content = excluded.content, tool_calls = excluded.tool_calls",
                params![
                    session_id,
                    seq as i64,
                    role,
                    content,
                    calls.map(|c| c.to_string()),
                    message.created
                ],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM search_rows WHERE session_id = ?1 AND seq = ?2",
                params![session_id, seq as i64],
            )?;
        }
    }
    Ok(())
}

/// Schema v1 → v2: create the index and fill it from the stored messages.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SEARCH_SCHEMA)?;
    let rows: Vec<(String, i64, String)> = {
        let mut statement = conn.prepare("SELECT session_id, seq, json FROM messages")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (session_id, seq, json) in rows {
        if let Ok(message) = serde_json::from_str::<Message>(&json) {
            index_message(conn, &session_id, seq as usize, &message)?;
        }
    }
    Ok(())
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchRow> {
    let calls: Option<String> = row.get(4)?;
    Ok(SearchRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        tool_calls: calls.and_then(|c| serde_json::from_str(&c).ok()),
        created: row.get(5)?,
    })
}

/// Quotes every term, so a query FTS5 can't parse still searches its words.
fn quoted_terms(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

impl crate::Store {
    /// FTS5 search (BM25 ranking, optionally biased by session age). Hermes's query syntax
    /// works: `alpha OR beta`, quoted phrases, `NOT`, `deploy*`.
    pub fn search(&self, query: &SearchQuery<'_>) -> Result<Vec<SearchHit>> {
        let conn = self.conn.lock();
        let run = |fts_query: &str| -> rusqlite::Result<Vec<SearchHit>> {
            let roles = query
                .roles
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            let excluded = query
                .exclude_sessions
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            let order = match query.sort {
                SearchSort::Relevance => "rank",
                SearchSort::Newest => "s.created_at DESC, rank",
                SearchSort::Oldest => "s.created_at ASC, rank",
            };
            let sql = format!(
                "SELECT r.id, r.session_id, r.role,
                        snippet(search_fts, 0, '>>>', '<<<', '...', 40)
                 FROM search_fts JOIN search_rows r ON r.id = search_fts.rowid
                 JOIN sessions s ON s.id = r.session_id
                 WHERE search_fts MATCH ?
                   AND r.role IN ({roles})
                   AND r.session_id NOT IN ({excluded})
                   AND s.created_at >= ? AND s.created_at < ?
                 ORDER BY {order} LIMIT ?"
            );
            let mut values: Vec<rusqlite::types::Value> = vec![fts_query.to_string().into()];
            values.extend(query.roles.iter().map(|r| r.clone().into()));
            values.extend(query.exclude_sessions.iter().map(|s| s.clone().into()));
            values.push(query.after_ms.unwrap_or(i64::MIN).into());
            values.push(query.before_ms.unwrap_or(i64::MAX).into());
            values.push((query.limit as i64).into());
            let mut statement = conn.prepare(&sql)?;
            statement
                .query_map(rusqlite::params_from_iter(values), |row| {
                    Ok(SearchHit {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        role: row.get(2)?,
                        snippet: row.get(3)?,
                    })
                })?
                .collect()
        };
        match run(query.query) {
            Ok(hits) => Ok(hits),
            // Syntax FTS5 rejects (a stray quote or colon): search the words instead.
            Err(_) => Ok(run(&quoted_terms(query.query))?),
        }
    }

    /// Every searchable message of a session, in order.
    pub fn search_rows(&self, session_id: &str) -> Result<Vec<SearchRow>> {
        let conn = self.conn.lock();
        let mut statement = conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, created FROM search_rows
             WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([session_id], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The session holding search row `id`.
    pub fn search_row_session(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT session_id FROM search_rows WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// A session whose title equals `title` (case-insensitive), newest first.
    pub fn session_by_title(&self, title: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT id FROM sessions WHERE lower(title) = lower(?1) ORDER BY updated_at DESC LIMIT 1",
                [title],
                |row| row.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewSession, Store};

    fn session(store: &Store, title: &str) -> String {
        let id = store
            .create_session(NewSession {
                cwd: "C:/work".into(),
                provider: "p".into(),
                model: "m".into(),
                system_prompt: "s".into(),
            })
            .unwrap()
            .id;
        store.set_title(&id, title).unwrap();
        id
    }

    fn query<'a>(text: &'a str, roles: &'a [String], exclude: &'a [String]) -> SearchQuery<'a> {
        SearchQuery {
            query: text,
            roles,
            exclude_sessions: exclude,
            after_ms: None,
            before_ms: None,
            sort: SearchSort::Relevance,
            limit: 10,
        }
    }

    #[test]
    fn visible_messages_are_searchable_and_scaffolding_is_not() {
        let store = Store::open_in_memory().unwrap();
        let a = session(&store, "Deploy notes");
        let messages = [
            Message::user().with_text("How do we deploy the gateway to staging?"),
            Message::assistant().with_text("Run the deploy script with --staging."),
            Message::user()
                .with_text("[System: continue the deploy]")
                .with_visibility(false, true),
        ];
        for (seq, message) in messages.iter().enumerate() {
            store
                .upsert_message(&a, seq, &message.clone().with_generated_id_if_missing())
                .unwrap();
        }
        let roles = vec!["user".to_string(), "assistant".to_string()];
        let hits = store.search(&query("deploy", &roles, &[])).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.session_id == a));
        assert!(hits.iter().any(|hit| hit.snippet.contains(">>>deploy<<<")));
        // The asking session can exclude itself; FTS5 syntax errors fall back to plain terms.
        assert!(
            store
                .search(&query("deploy", &roles, &[a.clone()]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .search(&query("staging\"", &roles, &[]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.search_rows(&a).unwrap().len(), 2);
        assert_eq!(
            store.session_by_title("deploy NOTES").unwrap(),
            Some(a.clone())
        );
        // Hiding a message later removes it from the index.
        let hidden = store.load_messages(&a).unwrap()[1]
            .clone()
            .with_visibility(false, true);
        store.upsert_message(&a, 1, &hidden).unwrap();
        assert_eq!(
            store.search(&query("script", &roles, &[])).unwrap().len(),
            0
        );
        store.delete_session(&a).unwrap();
        assert!(
            store
                .search(&query("deploy", &roles, &[]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_version_1_database_is_indexed_on_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(crate::SCHEMA).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('s1', 't', 'c', 'p', 'm', 'sp', 1, 1)",
                [],
            )
            .unwrap();
            let message = Message::user()
                .with_text("remember the purple elephant")
                .with_generated_id_if_missing();
            conn.execute(
                "INSERT INTO messages VALUES ('s1', ?1, 0, 'user', ?2)",
                params![message.id, serde_json::to_string(&message).unwrap()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let roles = vec!["user".to_string()];
        let hits = store.search(&query("elephant", &roles, &[])).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
    }
}

impl crate::Store {
    /// Sessions whose title contains `text` or whose messages contain its words (as prefixes),
    /// for filtering the session list.
    pub fn sessions_matching(&self, text: &str) -> Result<std::collections::HashSet<String>> {
        let text = text.trim();
        let mut found = std::collections::HashSet::new();
        if text.is_empty() {
            return Ok(found);
        }
        let conn = self.conn.lock();
        let mut titles =
            conn.prepare("SELECT id FROM sessions WHERE title LIKE '%' || ?1 || '%'")?;
        for id in titles.query_map([text], |row| row.get::<_, String>(0))? {
            found.insert(id?);
        }
        let terms = text
            .split_whitespace()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut messages = conn.prepare(
            "SELECT DISTINCT r.session_id FROM search_fts JOIN search_rows r ON r.id = search_fts.rowid
             WHERE search_fts MATCH ?1 LIMIT 1000",
        )?;
        if let Ok(rows) = messages.query_map([terms], |row| row.get::<_, String>(0)) {
            for id in rows.flatten() {
                found.insert(id);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod sidebar_tests {
    use crate::{NewSession, Store};
    use hermes_llm::conversation::message::Message;

    #[test]
    fn sessions_match_by_title_and_text_and_pins_persist() {
        let store = Store::open_in_memory().unwrap();
        let new = || NewSession {
            cwd: "C:/w".into(),
            provider: "p".into(),
            model: "m".into(),
            system_prompt: String::new(),
        };
        let a = store.create_session(new()).unwrap();
        let b = store.create_session(new()).unwrap();
        store.set_title(&a.id, "Fix the parser").unwrap();
        store
            .upsert_message(
                &b.id,
                0,
                &Message::user()
                    .with_text("deploying to staging")
                    .with_generated_id_if_missing(),
            )
            .unwrap();
        assert!(store.sessions_matching("parser").unwrap().contains(&a.id));
        let hits = store.sessions_matching("deploy").unwrap();
        assert!(hits.contains(&b.id) && !hits.contains(&a.id));
        store.set_pinned(&b.id, true).unwrap();
        assert!(store.session(&b.id).unwrap().unwrap().pinned);
        assert!(
            !store
                .list_sessions(10)
                .unwrap()
                .iter()
                .find(|s| s.id == a.id)
                .unwrap()
                .pinned
        );
    }
}
