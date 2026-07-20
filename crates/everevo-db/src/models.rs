//! Database row types — map directly to SQLite tables.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A session row in the `sessions` table.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: String, // JSON string
}

/// A message row in the `messages` table.
///
/// ## Immutability
///
/// Once inserted, a message row MUST NOT be modified. The `content_hash`
/// (SHA-256) acts as an integrity check — if content is tampered with,
/// the hash won't match. All downstream projections (chunks, entities,
/// wiki pages) reference messages via (session_id, message_id, content_hash).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    /// SHA-256 hash of `content` at insert time.
    /// Computed automatically by the database layer.
    pub content_hash: String,
    pub tool_calls: Option<String>, // JSON string
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl MessageRow {
    /// Create a new message row with auto-computed content hash.
    pub fn new(
        session_id: Uuid,
        role: impl Into<String>,
        content: impl Into<String>,
        tool_calls: Option<String>,
        tool_call_id: Option<String>,
    ) -> Self {
        let content: String = content.into();
        let content_hash = sha256_hash(&content);
        Self {
            id: Uuid::new_v4(),
            session_id,
            role: role.into(),
            content,
            content_hash,
            tool_calls,
            tool_call_id,
            created_at: chrono::Utc::now(),
        }
    }

    /// Verify the content hasn't been modified since insertion.
    pub fn verify_integrity(&self) -> bool {
        let actual = sha256_hash(&self.content);
        actual == self.content_hash
    }
}

pub fn sha256_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}
