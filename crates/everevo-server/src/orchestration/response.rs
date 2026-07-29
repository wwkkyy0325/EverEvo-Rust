//! Response finalization — extracted from chat.rs §8-10.
//! Persists the assistant message and sends closing SSE events.

use crate::app_state::AppState;
use axum::response::sse::Event;
use chrono::Utc;
use everevo_db::models::MessageRow;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Persist the assistant response and send closing SSE events
/// (content_block_stop → message_delta → message_stop → done).
#[allow(clippy::too_many_arguments)]
pub async fn persist_and_send(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    state: &Arc<AppState>,
    session_id: Uuid,
    assistant_id: Uuid,
    full_response: &str,
    thinking: &str,
    persisted_blocks: &[serde_json::Value],
    thinking_open: bool,
    text_block_idx: Option<usize>,
    block_index: usize,
) -> Result<(), String> {
    // Persist assistant message
    if !full_response.is_empty() {
        let mut blocks = persisted_blocks.to_vec();
        if thinking_open {
            blocks.push(
                serde_json::json!({"index": block_index, "type": "thinking", "thinking": thinking}),
            );
        }
        if let Some(tb) = text_block_idx {
            blocks.push(serde_json::json!({"index": tb, "type": "text", "text": full_response}));
        }
        let blocks_json = if blocks.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&blocks).unwrap_or_default())
        };
        let content_hash = everevo_db::models::sha256_hash(full_response);
        let msg = MessageRow {
            id: assistant_id,
            session_id,
            role: "assistant".into(),
            content: full_response.to_string(),
            content_hash,
            tool_calls: None,
            tool_call_id: None,
            thinking: thinking.to_string(),
            blocks_json,
            created_at: Utc::now(),
        };
        state
            .db
            .add_message(&msg)
            .await
            .map_err(|e| e.to_string())?;
        state
            .dreaming_engine
            .push_message("assistant", full_response, &assistant_id.to_string(), &session_id.to_string());
    }

    // Close open blocks
    if thinking_open {
        let _ = tx.send(super::stream::stop_event(block_index)).await;
    }
    if let Some(tb) = text_block_idx {
        let _ = tx.send(super::stream::stop_event(tb)).await;
    }

    // message_delta
    let _ = tx.send(Ok(Event::default().event("message_delta").data(
        serde_json::json!({"delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"input_tokens": 0, "output_tokens": 0}}).to_string(),
    ))).await;

    // message_stop
    let _ = tx
        .send(Ok(Event::default().event("message_stop").data("{}")))
        .await;

    // Legacy done event
    let done = serde_json::json!({"session_id": session_id, "message_id": assistant_id});
    let _ = tx
        .send(Ok(Event::default().event("done").data(done.to_string())))
        .await;

    // Cleanup
    state.session_actors.write().await.remove(&session_id);
    if let Some(sb) = state.sandboxes.read().await.get(&session_id) {
        sb.flush_audit();
    }

    Ok(())
}
