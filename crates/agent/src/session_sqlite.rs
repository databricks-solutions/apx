//! SQLite-backed session store.
//!
//! Stores chat sessions and messages at `~/.apx/agent/db`,
//! following the same pattern as [`apx_db::LogsDb`].

use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use tracing::debug;

use crate::chat::{ChatMessage, Role};
use crate::error::{AgentError, Result};
use crate::session::{Session, SessionStore};

/// SQLite-backed [`SessionStore`].
#[derive(Debug, Clone)]
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

impl SqliteSessionStore {
    /// Open or create the session database at the default location (`~/.apx/agent/db`).
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined, the
    /// database directory cannot be created, or the database cannot be opened.
    pub async fn open() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| AgentError::Session("cannot determine home directory".into()))?;
        let path = home.join(".apx").join("agent").join("db");
        Self::open_at(&path).await
    }

    /// Open or create the session database at a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or schema initialization fails.
    pub async fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Session(format!("create directory: {e}")))?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .map_err(|e| AgentError::Session(format!("open database: {e}")))?;

        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    /// Initialize the database schema.
    async fn init_schema(&self) -> Result<()> {
        for sql in [
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model_name TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id)",
        ] {
            sqlx::query(sql)
                .execute(&self.pool)
                .await
                .map_err(|e| AgentError::Session(format!("schema init: {e}")))?;
        }
        debug!("Agent session schema initialized");
        Ok(())
    }
}

use crate::chat::now_secs;

impl SessionStore for SqliteSessionStore {
    async fn create_session(&self, model_name: &str) -> Result<Session> {
        let id = nanoid::nanoid!();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO sessions (id, model_name, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(model_name)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| AgentError::Session(format!("create session: {e}")))?;

        Ok(Session {
            id,
            model_name: model_name.into(),
            created_at: now,
            updated_at: now,
        })
    }

    async fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let row =
            sqlx::query("SELECT id, model_name, created_at, updated_at FROM sessions WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| AgentError::Session(format!("get session: {e}")))?;

        Ok(row.map(|r| Session {
            id: r.get("id"),
            model_name: r.get("model_name"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn list_sessions(&self) -> Result<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT id, model_name, created_at, updated_at FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AgentError::Session(format!("list sessions: {e}")))?;

        Ok(rows
            .iter()
            .map(|r| Session {
                id: r.get("id"),
                model_name: r.get("model_name"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    async fn append_message(&self, session_id: &str, msg: &ChatMessage) -> Result<()> {
        let role_str = msg.role.to_string();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(&role_str)
        .bind(&msg.content)
        .bind(msg.timestamp)
        .execute(&self.pool)
        .await
        .map_err(|e| AgentError::Session(format!("append message: {e}")))?;

        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(msg.timestamp)
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AgentError::Session(format!("update session: {e}")))?;

        Ok(())
    }

    async fn load_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let rows = sqlx::query(
            "SELECT role, content, created_at FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AgentError::Session(format!("load messages: {e}")))?;

        rows.iter()
            .map(|r| {
                let role_str: String = r.get("role");
                let role: Role = role_str
                    .parse()
                    .map_err(|e: String| AgentError::Session(e))?;
                Ok(ChatMessage {
                    role,
                    content: r.get("content"),
                    timestamp: r.get("created_at"),
                })
            })
            .collect()
    }

    async fn update_model(&self, session_id: &str, model_name: &str) -> Result<()> {
        let now = now_secs();
        sqlx::query("UPDATE sessions SET model_name = ?, updated_at = ? WHERE id = ?")
            .bind(model_name)
            .bind(now)
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AgentError::Session(format!("update model: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn temp_store() -> SqliteSessionStore {
        let dir = std::env::temp_dir().join(format!(
            "apx-agent-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        SqliteSessionStore::open_at(&dir.join("test.db"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn create_session_generates_unique_ids() {
        let store = temp_store().await;
        let s1 = store.create_session("model-a").await.unwrap();
        let s2 = store.create_session("model-a").await.unwrap();
        assert_ne!(s1.id, s2.id);
        assert_eq!(s1.model_name, "model-a");
    }

    #[tokio::test]
    async fn get_session_returns_none_for_missing() {
        let store = temp_store().await;
        let result = store.get_session("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn append_and_load_messages_roundtrip() {
        let store = temp_store().await;
        let session = store.create_session("model-b").await.unwrap();

        let user_msg = ChatMessage {
            role: Role::User,
            content: "hello".into(),
            timestamp: 1000,
        };
        store.append_message(&session.id, &user_msg).await.unwrap();

        let asst_msg = ChatMessage {
            role: Role::Assistant,
            content: "hi there".into(),
            timestamp: 1001,
        };
        store.append_message(&session.id, &asst_msg).await.unwrap();

        let messages = store.load_messages(&session.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].content, "hi there");
    }

    #[tokio::test]
    async fn list_sessions_ordered_by_recent() {
        let store = temp_store().await;
        let s1 = store.create_session("model-a").await.unwrap();
        let s2 = store.create_session("model-b").await.unwrap();

        // Bump s1's updated_at to a far-future timestamp so it sorts first
        let msg = ChatMessage {
            role: Role::User,
            content: "bump".into(),
            timestamp: i64::MAX / 2,
        };
        store.append_message(&s1.id, &msg).await.unwrap();

        let sessions = store.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        // s1 was bumped to far-future, so it comes first
        assert_eq!(sessions[0].id, s1.id);
        assert_eq!(sessions[1].id, s2.id);
    }

    #[tokio::test]
    async fn get_session_returns_some_for_existing() {
        let store = temp_store().await;
        let created = store.create_session("model-x").await.unwrap();
        let fetched = store.get_session(&created.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.model_name, "model-x");
    }

    #[tokio::test]
    async fn append_message_updates_session_timestamp() {
        let store = temp_store().await;
        let session = store.create_session("model-y").await.unwrap();
        let original_updated = session.updated_at;

        let msg = ChatMessage {
            role: Role::User,
            content: "later".into(),
            timestamp: original_updated + 100,
        };
        store.append_message(&session.id, &msg).await.unwrap();

        let refreshed = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(refreshed.updated_at, original_updated + 100);
    }

    #[tokio::test]
    async fn load_messages_empty_for_new_session() {
        let store = temp_store().await;
        let session = store.create_session("model-z").await.unwrap();
        let messages = store.load_messages(&session.id).await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn update_model_changes_model_name() {
        let store = temp_store().await;
        let session = store.create_session("old-model").await.unwrap();
        assert_eq!(session.model_name, "old-model");

        store.update_model(&session.id, "new-model").await.unwrap();

        let updated = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(updated.model_name, "new-model");
        assert!(updated.updated_at >= session.updated_at);
    }

    #[tokio::test]
    async fn load_messages_preserves_insertion_order() {
        let store = temp_store().await;
        let session = store.create_session("model-order").await.unwrap();

        // Insert messages with out-of-order timestamps
        let msgs = [
            ChatMessage {
                role: Role::User,
                content: "third-ts".into(),
                timestamp: 3000,
            },
            ChatMessage {
                role: Role::Assistant,
                content: "first-ts".into(),
                timestamp: 1000,
            },
            ChatMessage {
                role: Role::User,
                content: "second-ts".into(),
                timestamp: 2000,
            },
        ];
        for msg in &msgs {
            store.append_message(&session.id, msg).await.unwrap();
        }

        let loaded = store.load_messages(&session.id).await.unwrap();
        assert_eq!(loaded.len(), 3);
        // Order is by insertion (id ASC), not by timestamp
        assert_eq!(loaded[0].content, "third-ts");
        assert_eq!(loaded[1].content, "first-ts");
        assert_eq!(loaded[2].content, "second-ts");
    }
}
