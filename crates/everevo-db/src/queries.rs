//! Database query functions — session and message CRUD.

use crate::models::{MessageRow, SessionRow};
use crate::Database;
use chrono::Utc;
use everevo_core::EverEvoError;
use uuid::Uuid;

// ── Sessions ───────────────────────────────────────────────────────────

impl Database {
    pub async fn create_session(&self, title: &str) -> Result<SessionRow, EverEvoError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let metadata = "{}".to_string();

        sqlx::query_as::<_, SessionRow>(
            "INSERT INTO sessions (id, title, created_at, updated_at, metadata) VALUES (?, ?, ?, ?, ?)
             RETURNING id, title, created_at, updated_at, metadata",
        )
        .bind(id)
        .bind(title)
        .bind(now)
        .bind(now)
        .bind(&metadata)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Create session failed: {e}")))
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionRow>, EverEvoError> {
        sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, created_at, updated_at, metadata FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("List sessions failed: {e}")))
    }

    pub async fn get_session(&self, id: Uuid) -> Result<Option<SessionRow>, EverEvoError> {
        sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, created_at, updated_at, metadata FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Get session failed: {e}")))
    }

    pub async fn delete_session(&self, id: Uuid) -> Result<(), EverEvoError> {
        // Delete messages first (foreign key cascade would be better, but SQLite pragma)
        sqlx::query("DELETE FROM messages WHERE session_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Delete messages failed: {e}")))?;

        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Delete session failed: {e}")))?;

        Ok(())
    }

    pub async fn update_session_title(
        &self,
        id: Uuid,
        title: &str,
    ) -> Result<(), EverEvoError> {
        sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Update session title failed: {e}")))?;

        Ok(())
    }
}

// ── Messages ───────────────────────────────────────────────────────────

impl Database {
    /// Insert a message with automatically computed content_hash.
    /// Messages are append-only — content_hash provides integrity verification.
    pub async fn add_message(&self, row: &MessageRow) -> Result<MessageRow, EverEvoError> {
        let content_hash = crate::models::sha256_hash(&row.content);
        sqlx::query_as::<_, MessageRow>(
            "INSERT INTO messages (id, session_id, role, content, content_hash, tool_calls, tool_call_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id, session_id, role, content, content_hash, tool_calls, tool_call_id, created_at",
        )
        .bind(row.id)
        .bind(row.session_id)
        .bind(&row.role)
        .bind(&row.content)
        .bind(&content_hash)
        .bind(&row.tool_calls)
        .bind(&row.tool_call_id)
        .bind(row.created_at)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Add message failed: {e}")))
    }

    pub async fn get_messages(
        &self,
        session_id: Uuid,
        limit: Option<usize>,
    ) -> Result<Vec<MessageRow>, EverEvoError> {
        let limit = limit.unwrap_or(100);
        sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, content_hash, tool_calls, tool_call_id, created_at
             FROM messages WHERE session_id = ?
             ORDER BY created_at ASC
             LIMIT ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Get messages failed: {e}")))
    }

    pub async fn search_sessions(&self, query: &str) -> Result<Vec<SessionRow>, EverEvoError> {
        // Escape LIKE wildcards to prevent DoS via % and _ injection
        let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        // SQLite uses \ as default escape char
        sqlx::query_as::<_, SessionRow>(
            "SELECT DISTINCT s.id, s.title, s.created_at, s.updated_at, s.metadata
             FROM sessions s
             LEFT JOIN messages m ON s.id = m.session_id
             WHERE s.title LIKE ? OR m.content LIKE ?
             ORDER BY s.updated_at DESC
             LIMIT 20",
        )
        .bind(&pattern)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Search sessions failed: {e}")))
    }
}

// ── Pagination & Enrichment ─────────────────────────────────────────────

/// Lightweight row for session list enrichment — avoids joining full messages.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionWithMeta {
    pub id: Uuid,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub metadata: String,
    pub message_count: i64,
    pub last_content: Option<String>,
}

impl Database {
    /// List sessions with message count and last-message preview.
    pub async fn list_sessions_enriched(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SessionWithMeta>, EverEvoError> {
        sqlx::query_as::<_, SessionWithMeta>(
            r#"SELECT
                s.id, s.title, s.created_at, s.updated_at, s.metadata,
                COUNT(m.id) AS message_count,
                (SELECT m2.content FROM messages m2
                 WHERE m2.session_id = s.id
                 ORDER BY m2.created_at DESC LIMIT 1) AS last_content
               FROM sessions s
               LEFT JOIN messages m ON s.id = m.session_id
               GROUP BY s.id
               ORDER BY s.updated_at DESC
               LIMIT ? OFFSET ?"#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("List sessions enriched failed: {e}")))
    }

    /// Count total sessions (for pagination metadata).
    pub async fn count_sessions(&self) -> Result<i64, EverEvoError> {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Count sessions failed: {e}")))
    }

    /// Cursor-based message pagination: fetch messages older than `before` id.
    ///
    /// Returns messages in **reverse chronological order** (newest first),
    /// limited to `limit` rows. Pass `before = None` for the first page.
    pub async fn get_messages_before(
        &self,
        session_id: Uuid,
        before: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<MessageRow>, EverEvoError> {
        if let Some(cursor) = before {
            sqlx::query_as::<_, MessageRow>(
                "SELECT id, session_id, role, content, content_hash, tool_calls, tool_call_id, created_at
                 FROM messages
                 WHERE session_id = ? AND created_at < (SELECT created_at FROM messages WHERE id = ?)
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .bind(session_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Get messages before failed: {e}")))
        } else {
            sqlx::query_as::<_, MessageRow>(
                "SELECT id, session_id, role, content, content_hash, tool_calls, tool_call_id, created_at
                 FROM messages
                 WHERE session_id = ?
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .bind(session_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Get messages latest failed: {e}")))
        }
    }

    /// Check if there are more messages older than the given cursor.
    pub async fn has_more_messages(
        &self,
        session_id: Uuid,
        before: Uuid,
    ) -> Result<bool, EverEvoError> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM messages
             WHERE session_id = ? AND created_at < (SELECT created_at FROM messages WHERE id = ?)",
        )
        .bind(session_id)
        .bind(before)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Has more check failed: {e}")))?;
        Ok(count > 0)
    }
}

// ── Facts (Long-Term Memory Search) ──────────────────────────────────────

/// A fact row in the SQLite index.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct FactRow {
    pub id: String,
    pub description: String,
    pub content: String,
    pub fact_type: String,
    pub retrieval_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl Database {
    /// Upsert a fact into the SQLite index (INSERT OR REPLACE).
    pub async fn upsert_fact(&self, id: &str, description: &str, content: &str, fact_type: &str) -> Result<(), EverEvoError> {
        sqlx::query(
            "INSERT INTO facts (id, description, content, fact_type, retrieval_count, created_at, updated_at)
             VALUES (?, ?, ?, ?, 0, datetime('now'), datetime('now'))
             ON CONFLICT(id) DO UPDATE SET description=excluded.description, content=excluded.content,
             fact_type=excluded.fact_type, updated_at=datetime('now')",
        )
        .bind(id)
        .bind(description)
        .bind(content)
        .bind(fact_type)
        .execute(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Upsert fact failed: {e}")))?;
        Ok(())
    }

    /// Delete a fact from the SQLite index.
    pub async fn delete_fact(&self, id: &str) -> Result<(), EverEvoError> {
        sqlx::query("DELETE FROM facts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Delete fact failed: {e}")))?;
        Ok(())
    }

    /// Search facts via FTS5 full-text index.
    /// Returns results ranked by BM25 relevance. Sub-millisecond performance.
    pub async fn search_facts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FactRow>, EverEvoError> {
        sqlx::query_as::<_, FactRow>(
            "SELECT f.id, f.description, f.content, f.fact_type, f.retrieval_count, f.created_at, f.updated_at
             FROM facts f
             INNER JOIN facts_fts ft ON f.rowid = ft.rowid
             WHERE facts_fts MATCH ?
             ORDER BY rank
             LIMIT ?",
        )
        .bind(query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("Search facts failed: {e}")))
    }

    /// List all facts ordered by recency.
    pub async fn list_facts(&self, limit: usize) -> Result<Vec<FactRow>, EverEvoError> {
        sqlx::query_as::<_, FactRow>(
            "SELECT id, description, content, fact_type, retrieval_count, created_at, updated_at
             FROM facts ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| EverEvoError::Database(format!("List facts failed: {e}")))
    }

    /// Bump retrieval count for frequently accessed facts (ranking boost).
    pub async fn bump_fact_retrieval(&self, id: &str) -> Result<(), EverEvoError> {
        sqlx::query("UPDATE facts SET retrieval_count = retrieval_count + 1 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| EverEvoError::Database(format!("Bump fact retrieval failed: {e}")))?;
        Ok(())
    }
}
