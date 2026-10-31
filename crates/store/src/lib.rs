//! SQLite persistence for sessions, their messages and token usage.
//!
//! Messages are stored as the JSON of `hermes_llm`'s `Message`, keyed by message id and ordered
//! by their position in the conversation. The caller owns merging streamed chunks (through
//! `Conversation::push`) and upserts the merged message here.

use std::path::Path;

use anyhow::{Context as _, Result};
use hermes_llm::conversation::message::Message;
use hermes_llm::conversation::token_usage::ProviderUsage;
use parking_lot::Mutex;
use rmcp::model::Role;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = "
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    title TEXT,
    cwd TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    system_prompt TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE messages (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);
CREATE TABLE usage (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    at INTEGER NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    cost REAL
);
CREATE INDEX usage_by_session ON usage(session_id);
";

/// A stored session. `system_prompt` is frozen at creation and only replaced by compaction.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewSession {
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost: Option<f64>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening session database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version != SCHEMA_VERSION {
            anyhow::bail!(
                "session database has schema version {version}, expected {SCHEMA_VERSION}"
            );
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_session(&self, new: NewSession) -> Result<SessionRecord> {
        let now = now_ms();
        let record = SessionRecord {
            id: format!("ses_{}", uuid::Uuid::new_v4().simple()),
            title: None,
            cwd: new.cwd,
            provider: new.provider,
            model: new.model,
            system_prompt: new.system_prompt,
            created_at: now,
            updated_at: now,
        };
        self.conn.lock().execute(
            "INSERT INTO sessions (id, title, cwd, provider, model, system_prompt, created_at, updated_at)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![record.id, record.cwd, record.provider, record.model, record.system_prompt, now],
        )?;
        Ok(record)
    }

    pub fn session(&self, id: &str) -> Result<Option<SessionRecord>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT id, title, cwd, provider, model, system_prompt, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok(SessionRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        cwd: row.get(2)?,
                        provider: row.get(3)?,
                        model: row.get(4)?,
                        system_prompt: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    /// Most recently updated first.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock();
        let mut statement = conn.prepare(
            "SELECT id, title, cwd, provider, model, system_prompt, created_at, updated_at
             FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                cwd: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                system_prompt: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now_ms()],
        )?;
        Ok(())
    }

    /// Switching models rebuilds the frozen prompt too; see `Runtime::set_session_model`.
    pub fn set_model(&self, id: &str, model: &str, system_prompt: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE sessions SET model = ?2, system_prompt = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, model, system_prompt, now_ms()],
        )?;
        Ok(())
    }

    /// Only compaction may rewrite the frozen system prompt (the prompt-cache invariant).
    pub fn set_system_prompt(&self, id: &str, system_prompt: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE sessions SET system_prompt = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, system_prompt, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let conn = self.conn.lock();
        let mut statement =
            conn.prepare("SELECT json FROM messages WHERE session_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map([session_id], |row| row.get::<_, String>(0))?;
        rows.map(|json| Ok(serde_json::from_str(&json?)?)).collect()
    }

    /// Inserts or replaces the message at position `seq` in the conversation. Rows are keyed by
    /// position, not message id: providers may reuse completion ids across responses.
    pub fn upsert_message(&self, session_id: &str, seq: usize, message: &Message) -> Result<()> {
        let id = message
            .id
            .as_deref()
            .context("messages must have an id before they are stored")?;
        let json = serde_json::to_string(message)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO messages (session_id, id, seq, role, json) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, seq) DO UPDATE SET id = excluded.id, role = excluded.role, json = excluded.json",
            params![session_id, id, seq as i64, role_name(message), json],
        )?;
        conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, now_ms()],
        )?;
        Ok(())
    }

    /// Replaces the whole stored conversation (compaction, undo).
    pub fn replace_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;
        for (seq, message) in messages.iter().enumerate() {
            let id = message
                .id
                .as_deref()
                .context("messages must have an id before they are stored")?;
            tx.execute(
                "INSERT INTO messages (session_id, id, seq, role, json) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, id, seq as i64, role_name(message), serde_json::to_string(message)?],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_usage(&self, session_id: &str, usage: &ProviderUsage) -> Result<()> {
        let tokens = |value: Option<i32>| i64::from(value.unwrap_or(0));
        self.conn.lock().execute(
            "INSERT INTO usage (session_id, at, model, input_tokens, output_tokens,
                                cache_read_tokens, cache_write_tokens, cost)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_id,
                now_ms(),
                usage.model,
                tokens(usage.usage.input_tokens),
                tokens(usage.usage.output_tokens),
                tokens(usage.usage.cache_read_input_tokens),
                tokens(usage.usage.cache_write_input_tokens),
                usage.cost,
            ],
        )?;
        Ok(())
    }

    pub fn usage_totals(&self, session_id: &str) -> Result<UsageTotals> {
        Ok(self.conn.lock().query_row(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0), COALESCE(SUM(cache_write_tokens), 0),
                    SUM(cost)
             FROM usage WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok(UsageTotals {
                    calls: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cache_read_tokens: row.get(3)?,
                    cache_write_tokens: row.get(4)?,
                    cost: row.get(5)?,
                })
            },
        )?)
    }
}

fn role_name(message: &Message) -> &'static str {
    match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_llm::conversation::token_usage::Usage;

    fn store_with_session() -> (Store, SessionRecord) {
        let store = Store::open_in_memory().unwrap();
        let session = store
            .create_session(NewSession {
                cwd: "C:/work".into(),
                provider: "openrouter".into(),
                model: "test/model".into(),
                system_prompt: "frozen".into(),
            })
            .unwrap();
        (store, session)
    }

    #[test]
    fn upsert_replaces_the_row_at_its_position() {
        let (store, session) = store_with_session();
        let first = Message::user().with_text("hi").with_id("m1");
        let mut reply = Message::assistant().with_text("he").with_id("m1");
        store.upsert_message(&session.id, 0, &first).unwrap();
        store.upsert_message(&session.id, 1, &reply).unwrap();
        reply = Message::assistant().with_text("hello").with_id("m1");
        store.upsert_message(&session.id, 1, &reply).unwrap();

        let loaded = store.load_messages(&session.id).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].as_concat_text(), "hi");
        assert_eq!(loaded[1].as_concat_text(), "hello");
    }

    #[test]
    fn sessions_list_newest_first_and_cascade_on_delete() {
        let (store, older) = store_with_session();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let newer = store
            .create_session(NewSession {
                cwd: "C:/other".into(),
                provider: "openrouter".into(),
                model: "test/model".into(),
                system_prompt: "frozen".into(),
            })
            .unwrap();
        store
            .upsert_message(&older.id, 0, &Message::user().with_text("x").with_id("m1"))
            .unwrap();
        let ids: Vec<_> = store
            .list_sessions(10)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![older.id.clone(), newer.id.clone()]);

        store.delete_session(&older.id).unwrap();
        assert!(store.session(&older.id).unwrap().is_none());
        assert!(store.load_messages(&older.id).unwrap().is_empty());
    }

    #[test]
    fn usage_totals_sum_calls() {
        let (store, session) = store_with_session();
        for _ in 0..2 {
            let usage = ProviderUsage::new(
                "test/model".into(),
                Usage::new(Some(100), Some(10), Some(110)),
            );
            store.record_usage(&session.id, &usage).unwrap();
        }
        let totals = store.usage_totals(&session.id).unwrap();
        assert_eq!(totals.calls, 2);
        assert_eq!(totals.input_tokens, 200);
        assert_eq!(totals.output_tokens, 20);
    }

    #[test]
    fn file_database_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let id = {
            let store = Store::open(&path).unwrap();
            let session = store
                .create_session(NewSession {
                    cwd: "C:/work".into(),
                    provider: "p".into(),
                    model: "m".into(),
                    system_prompt: "frozen".into(),
                })
                .unwrap();
            store
                .upsert_message(
                    &session.id,
                    0,
                    &Message::user().with_text("persist me").with_id("m1"),
                )
                .unwrap();
            session.id
        };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.session(&id).unwrap().unwrap().system_prompt, "frozen");
        assert_eq!(
            store.load_messages(&id).unwrap()[0].as_concat_text(),
            "persist me"
        );
    }
}
