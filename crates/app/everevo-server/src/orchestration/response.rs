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
    input_tokens: u64,
    output_tokens: u64,
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
        state.dreaming_engine.push_message(
            "assistant",
            full_response,
            &assistant_id.to_string(),
            &session_id.to_string(),
        );
    }

    // Close open blocks
    if thinking_open {
        let _ = tx.send(super::stream::stop_event(block_index)).await;
    }
    if let Some(tb) = text_block_idx {
        let _ = tx.send(super::stream::stop_event(tb)).await;
    }

    // message_delta — real token counts from LLM API
    let _ = tx.send(Ok(Event::default().event("message_delta").data(
        serde_json::json!({"delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}}).to_string(),
    ))).await;

    // message_stop
    let _ = tx
        .send(Ok(Event::default().event("message_stop").data("{}")))
        .await;

    // Done event — includes real token usage from LLM API
    let done = serde_json::json!({"session_id": session_id, "message_id": assistant_id, "input_tokens": input_tokens, "output_tokens": output_tokens});
    let _ = tx
        .send(Ok(Event::default().event("done").data(done.to_string())))
        .await;

    // Cleanup
    state.session_actors.write().await.remove(&session_id);
    if let Some(sb) = state.sandboxes.read().await.get(&session_id) {
        sb.flush_audit();
    }

    // ── Summarizer: fire-and-forget session summary on close ──
    // Only runs when there's substantive conversation to summarize.
    // Benchmark mode (EVEREVO_BENCHMARK=1) skips it — the handoff summary is
    // written to the GLOBAL tier and would leak one GAIA question's content
    // into later questions' context.
    if !full_response.is_empty()
        && full_response.len() > 50
        && std::env::var("EVEREVO_BENCHMARK").is_err()
    {
        let llm = {
            let guard = state.llm.read().await;
            guard.values().find_map(|v| v.clone())
        };
        if let Some(client) = llm {
            let fm = state.fact_manager.clone();
            let sid = session_id;
            let summary_text = full_response.to_string();
            tokio::spawn(async move {
                let buffer = everevo_agent::memory::TrajectoryBuffer::default();
                let summary = everevo_agent::memory::summarize_session(
                    &client,
                    &fm,
                    &buffer,
                    sid,
                    &summary_text,
                    &summary_text,
                )
                .await;
                tracing::info!(
                    session_id = %sid,
                    achieved = summary.goals_achieved.len(),
                    paradigms = summary.paradigms_extracted,
                    "Summarizer: session summary saved"
                );
            });
        }
    }

    Ok(())
}
