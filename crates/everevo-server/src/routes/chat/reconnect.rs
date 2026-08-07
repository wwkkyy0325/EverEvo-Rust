//! Reconnection handler — replays messages from DB as SSE events.

use std::convert::Infallible;
use std::sync::Arc;

use axum::response::sse::Event;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use everevo_core::types::ChatRequest;

/// Replay all messages from DB as SSE events — for reconnecting to
/// background/daemon sessions. Also notifies if the session is still running.
pub(super) async fn handle_reconnect(
    state: &Arc<AppState>,
    req: ChatRequest,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), String> {
    let session_id = req.session_id.ok_or("session_id required for reconnect")?;

    // Verify session exists
    let session = state
        .db
        .get_session(session_id)
        .await
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| "Session not found".to_string())?;

    // Parse metadata
    let meta: everevo_core::types::SessionMeta =
        serde_json::from_str(&session.metadata).unwrap_or_default();

    // Send session info event
    let _ = tx
        .send(Ok(Event::default().event("session_info").data(
            serde_json::json!({
                "session_id": session_id,
                "mode": meta.mode.as_str(),
                "state": meta.state.as_str(),
            })
            .to_string(),
        )))
        .await;

    // Load all messages
    let messages = state
        .db
        .get_messages(session_id, None)
        .await
        .map_err(|e| format!("Load messages: {e}"))?;

    // Replay messages as SSE events
    for msg in &messages {
        let event_type = match msg.role.as_str() {
            "user" => "user_message",
            "assistant" => "assistant_message",
            "tool" => "tool_message",
            _ => "message",
        };
        let _ = tx
            .send(Ok(Event::default().event(event_type).data(
                serde_json::json!({
                    "id": msg.id,
                    "role": msg.role,
                    "content": msg.content,
                    "created_at": msg.created_at,
                })
                .to_string(),
            )))
            .await;
    }

    // Check if session is still running (has a bg worker)
    let is_running = state.bg_sessions.read().await.contains_key(&session_id);

    if is_running {
        // Session is still active — hold connection open and poll for new messages
        let mut last_count = messages.len();
        // Poll every 500ms for new messages, up to 5 minutes
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Check if still running
            if !state.bg_sessions.read().await.contains_key(&session_id) {
                break; // bg worker finished
            }

            // Check for new messages
            let current = state
                .db
                .get_messages(session_id, None)
                .await
                .map_err(|e| format!("Poll messages: {e}"))?;

            // Send any new messages
            for msg in &current[last_count..] {
                let _ = tx
                    .send(Ok(Event::default().event("new_message").data(
                        serde_json::json!({
                            "id": msg.id,
                            "role": msg.role,
                            "content": msg.content,
                            "created_at": msg.created_at,
                        })
                        .to_string(),
                    )))
                    .await;
            }
            last_count = current.len();
        }
    }

    // Done
    let _ = tx
        .send(Ok(Event::default().event("reconnect_done").data(
            serde_json::json!({
                "session_id": session_id,
                "message_count": messages.len(),
            })
            .to_string(),
        )))
        .await;

    Ok(())
}
