//! Session & message history — CRUD with cursor pagination.
//!
//! ## Design
//!
//! Session list uses **offset pagination** (small N, stable order).
//! Message history uses **cursor pagination** via `?before=<uuid>` —
//! the standard pattern for append-only feeds (OpenAI, Slack, Discord).
//!
//! Response envelope:
//! ```json
//! { "data": [...], "has_more": true, "next_cursor": "uuid", "total": 42 }
//! ```

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::app_state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // Session CRUD
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).put(update_title).delete(delete_session),
        )
        // Message history (separate endpoint for cursor pagination)
        .route("/api/sessions/{id}/messages", get(get_messages))
        // Session status (mode + state for daemon sessions)
        .route("/api/sessions/{id}/status", get(get_session_status))
}

// ── DTOs ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit_20")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

#[derive(Debug, Deserialize)]
struct MessagesQuery {
    /// Cursor — fetch messages older than this ID.
    #[serde(default)]
    before: Option<Uuid>,
    #[serde(default = "default_limit_50")]
    limit: i64,
}

#[derive(Debug, Deserialize)]
struct CreateSessionBody {
    #[serde(default = "default_title")]
    title: String,
}

#[derive(Debug, Deserialize)]
struct UpdateTitleBody {
    title: String,
}

fn default_limit_20() -> i64 {
    20
}
fn default_limit_50() -> i64 {
    50
}
fn default_title() -> String {
    "New Session".into()
}

// ── Envelope ────────────────────────────────────────────────────────────

/// Standard paginated response envelope.
#[derive(Debug, Serialize)]
struct Paginated<T: Serialize> {
    data: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<i64>,
}

// ── Session list ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SessionItem {
    id: Uuid,
    title: String,
    created_at: String,
    updated_at: String,
    message_count: i64,
    /// First ~120 chars of the most recent message, if any.
    last_message: Option<String>,
    /// Session mode: "interactive" or "background"
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    /// Session state: "idle", "running", "completed", "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<serde_json::Value> {
    let limit = q.limit.min(100);
    let offset = q.offset.max(0);

    let (rows, total) = match tokio::try_join!(
        state.db.list_sessions_enriched(limit, offset),
        state.db.count_sessions(),
    ) {
        Ok((rows, total)) => (rows, total),
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    let items: Vec<SessionItem> = rows
        .into_iter()
        .map(|r| {
            let (mode, state) = parse_metadata(&r.metadata);
            SessionItem {
                id: r.id,
                title: r.title,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                message_count: r.message_count,
                last_message: r.last_content.map(|c| truncate_preview(&c, 120)),
                mode,
                state,
            }
        })
        .collect();

    let has_more = offset + limit < total;
    Json(
        serde_json::to_value(Paginated {
            data: items,
            next_cursor: None,
            has_more,
            total: Some(total),
        })
        .unwrap_or_else(|_| serde_json::json!({ "error": "serialization failed" })),
    )
}

// ── Session detail ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SessionDetail {
    id: Uuid,
    title: String,
    created_at: String,
    updated_at: String,
    message_count: i64,
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let session = match state.db.get_session(id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Json(serde_json::json!({ "error": "Session not found" })),
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    // Count messages for this session
    let count = state
        .db
        .list_sessions_enriched(1, 0)
        .await
        .ok()
        .and_then(|rows| rows.into_iter().find(|r| r.id == id))
        .map(|r| r.message_count)
        .unwrap_or(0);

    Json(serde_json::json!({
        "data": SessionDetail {
            id: session.id,
            title: session.title,
            created_at: session.created_at.to_rfc3339(),
            updated_at: session.updated_at.to_rfc3339(),
            message_count: count,
        }
    }))
}

// ── Message history (cursor pagination) ─────────────────────────────────

#[derive(Debug, Serialize)]
struct MessageItem {
    id: Uuid,
    role: String,
    content: String,
    tool_calls: Option<serde_json::Value>,
    tool_call_id: Option<String>,
    thinking: String,
    blocks_json: Option<serde_json::Value>,
    created_at: String,
}

async fn get_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagesQuery>,
) -> Json<serde_json::Value> {
    let limit = q.limit.min(100);

    // Verify session exists
    if state.db.get_session(id).await.ok().flatten().is_none() {
        return Json(serde_json::json!({ "error": "Session not found" }));
    }

    let rows = match state.db.get_messages_before(id, q.before, limit).await {
        Ok(r) => r,
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    let has_more = if let Some(last) = rows.last() {
        state
            .db
            .has_more_messages(id, last.id)
            .await
            .unwrap_or(false)
    } else {
        false
    };

    let next_cursor = rows.last().map(|r| r.id.to_string());

    let messages: Vec<MessageItem> = rows
        .into_iter()
        .map(|m| MessageItem {
            id: m.id,
            role: m.role,
            content: m.content,
            tool_calls: m.tool_calls.and_then(|s| serde_json::from_str(&s).ok()),
            tool_call_id: m.tool_call_id,
            thinking: m.thinking,
            blocks_json: m.blocks_json.and_then(|s| serde_json::from_str(&s).ok()),
            created_at: m.created_at.to_rfc3339(),
        })
        .collect();

    Json(
        serde_json::to_value(Paginated {
            data: messages,
            next_cursor,
            has_more,
            total: None,
        })
        .unwrap_or_else(|_| serde_json::json!({ "error": "serialization failed" })),
    )
}

// ── Create / Update / Delete ────────────────────────────────────────────

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSessionBody>,
) -> Json<serde_json::Value> {
    match state.db.create_session(&body.title).await {
        Ok(row) => {
            // Flush any buffered messages from the previous session before starting new one.
            // Hermes `on_session_end` pattern: prevent memory loss at session boundaries.
            state.dreaming_engine.flush_on_session_end().await;
            // Initialize per-session sandbox + audit trail
            let _ = state
                .create_sandbox(
                    row.id,
                    resolve_permission(&state.config.default_permission_level),
                )
                .await;
            Json(serde_json::json!({
                "data": {
                    "id": row.id,
                    "title": row.title,
                    "created_at": row.created_at.to_rfc3339(),
                    "updated_at": row.updated_at.to_rfc3339(),
                }
            }))
        }
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn update_title(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTitleBody>,
) -> Json<serde_json::Value> {
    match state.db.update_session_title(id, &body.title).await {
        Ok(()) => Json(serde_json::json!({ "data": { "updated": true } })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    // Flush buffered messages before destroying the session
    state.dreaming_engine.flush_on_session_end().await;
    // Destroy sandbox + audit trail first
    state.destroy_sandbox(id).await;
    // Clean up context snapshots for this session
    state.context_snapshots.write().await.remove(&id);
    match state.db.delete_session(id).await {
        Ok(()) => Json(serde_json::json!({ "data": { "deleted": true } })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

// ── Session Status ────────────────────────────────────────────────────────

async fn get_session_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let session = match state.db.get_session(id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Json(serde_json::json!({ "error": "Session not found" })),
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    let meta: everevo_core::types::SessionMeta =
        serde_json::from_str(&session.metadata).unwrap_or_default();
    let has_bg = state.bg_sessions.read().await.contains_key(&id);

    Json(serde_json::json!({
        "id": session.id,
        "mode": meta.mode.as_str(),
        "state": if has_bg { "running" } else { meta.state.as_str() },
        "title": session.title,
        "updated_at": session.updated_at.to_rfc3339(),
    }))
}

// ── Helpers ────────────────────────────────────────────────────────────

fn parse_metadata(raw: &str) -> (Option<String>, Option<String>) {
    let meta: everevo_core::types::SessionMeta =
        serde_json::from_str(raw).unwrap_or_default();
    (Some(meta.mode.as_str().to_string()), Some(meta.state.as_str().to_string()))
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let single_line = text.lines().next().unwrap_or(text);
    if single_line.chars().count() > max_chars {
        single_line
            .chars()
            .take(max_chars.saturating_sub(3))
            .chain("...".chars())
            .collect()
    } else {
        single_line.to_string()
    }
}

fn resolve_permission(level: &str) -> everevo_sandbox::PermissionLevel {
    match level {
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        _ => everevo_sandbox::PermissionLevel::SemiAuto,
    }
}
