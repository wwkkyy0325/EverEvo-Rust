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
    pub content_hash: String,
    pub tool_calls: Option<String>, // JSON string
    pub tool_call_id: Option<String>,
    pub thinking: String, // chain-of-thought, empty if none
    /// Serialized ContentBlock array for interleaved rendering.
    pub blocks_json: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl MessageRow {
    /// Create a new message row with auto-computed content hash.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: Uuid,
        role: impl Into<String>,
        content: impl Into<String>,
        tool_calls: Option<String>,
        tool_call_id: Option<String>,
        thinking: Option<String>,
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
            thinking: thinking.unwrap_or_default(),
            blocks_json: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// Set the blocks_json field (builder pattern).
    pub fn with_blocks(mut self, blocks: Option<String>) -> Self {
        self.blocks_json = blocks;
        self
    }

    /// Verify the content hasn't been modified since insertion.
    pub fn verify_integrity(&self) -> bool {
        let actual = sha256_hash(&self.content);
        actual == self.content_hash
    }
}

pub use everevo_core::memory::sha256_hash;

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MessageRow::new ───────────────────────────────────────────

    #[test]
    fn test_message_row_new_basic() {
        let session_id = Uuid::new_v4();
        let row = MessageRow::new(session_id, "user", "Hello, world!", None, None, None);

        assert_eq!(row.session_id, session_id);
        assert_eq!(row.role, "user");
        assert_eq!(row.content, "Hello, world!");
        assert!(row.thinking.is_empty());
        assert!(row.tool_calls.is_none());
        assert!(row.tool_call_id.is_none());
        assert!(row.blocks_json.is_none());
    }

    #[test]
    fn test_message_row_new_with_tool_call() {
        let session_id = Uuid::new_v4();
        let tool_calls = Some(r#"[{"name":"shell","args":{"cmd":"ls"}}]"#.to_string());
        let row = MessageRow::new(
            session_id,
            "assistant",
            "",
            tool_calls.clone(),
            Some("tool_abc123".into()),
            None,
        );

        assert_eq!(row.role, "assistant");
        assert_eq!(row.tool_calls, tool_calls);
        assert_eq!(row.tool_call_id, Some("tool_abc123".into()));
    }

    #[test]
    fn test_message_row_new_with_thinking() {
        let session_id = Uuid::new_v4();
        let row = MessageRow::new(
            session_id,
            "assistant",
            "Result: 42",
            None,
            None,
            Some("Let me think about this...".into()),
        );

        assert_eq!(row.thinking, "Let me think about this...");
    }

    #[test]
    fn test_message_row_ids_are_unique() {
        let sid = Uuid::new_v4();
        let a = MessageRow::new(sid, "user", "a", None, None, None);
        let b = MessageRow::new(sid, "user", "b", None, None, None);
        assert_ne!(a.id, b.id, "each message gets a unique UUID");
    }

    // ── Content hash ──────────────────────────────────────────────

    #[test]
    fn test_message_row_content_hash_consistent() {
        let sid = Uuid::new_v4();
        let a = MessageRow::new(sid, "user", "same content", None, None, None);
        let expected = sha256_hash("same content");
        assert_eq!(a.content_hash, expected);
    }

    #[test]
    fn test_message_row_different_content_different_hash() {
        let sid = Uuid::new_v4();
        let a = MessageRow::new(sid, "user", "content A", None, None, None);
        let b = MessageRow::new(sid, "user", "content B", None, None, None);
        assert_ne!(a.content_hash, b.content_hash);
    }

    // ── Integrity ─────────────────────────────────────────────────

    #[test]
    fn test_verify_integrity_valid() {
        let sid = Uuid::new_v4();
        let row = MessageRow::new(sid, "user", "trusted content", None, None, None);
        assert!(row.verify_integrity());
    }

    #[test]
    fn test_verify_integrity_tampered() {
        let sid = Uuid::new_v4();
        let mut row = MessageRow::new(sid, "user", "original", None, None, None);
        // Simulate tampering: change content without updating hash
        row.content = "tampered!".into();
        assert!(!row.verify_integrity());
    }

    #[test]
    fn test_verify_integrity_empty_content() {
        let sid = Uuid::new_v4();
        let row = MessageRow::new(sid, "user", "", None, None, None);
        assert!(row.verify_integrity());
    }

    // ── with_blocks builder ────────────────────────────────────────

    #[test]
    fn test_with_blocks() {
        let sid = Uuid::new_v4();
        let row = MessageRow::new(sid, "assistant", "hello", None, None, None)
            .with_blocks(Some(r#"[{"type":"text"}]"#.into()));
        assert_eq!(row.blocks_json, Some(r#"[{"type":"text"}]"#.into()));
    }

    #[test]
    fn test_with_blocks_none() {
        let sid = Uuid::new_v4();
        let row = MessageRow::new(sid, "assistant", "hello", None, None, None).with_blocks(None);
        assert!(row.blocks_json.is_none());
    }
}
