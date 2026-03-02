//! Persistence layer for chat sessions and message history.

use std::future::Future;

use crate::chat::ChatMessage;
use crate::error::Result;

/// A chat session.
#[derive(Debug, Clone)]
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Name of the model used in this session.
    pub model_name: String,
    /// Unix timestamp (seconds) when the session was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the last activity.
    pub updated_at: i64,
}

/// Persistence layer for chat sessions and message history.
///
/// Implementations must be `Send + Sync` for use across async tasks.
pub trait SessionStore: Send + Sync {
    /// Create a new session and return it.
    fn create_session(&self, model_name: &str) -> impl Future<Output = Result<Session>> + Send;
    /// Load a session by ID. Returns `None` if not found.
    fn get_session(&self, id: &str) -> impl Future<Output = Result<Option<Session>>> + Send;
    /// List all sessions, most recent first.
    fn list_sessions(&self) -> impl Future<Output = Result<Vec<Session>>> + Send;
    /// Append a message to a session.
    fn append_message(
        &self,
        session_id: &str,
        msg: &ChatMessage,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Load all messages for a session in chronological order.
    fn load_messages(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<Vec<ChatMessage>>> + Send;
}
