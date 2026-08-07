//! Session resolution — extracted from chat.rs §1-2.

use crate::app_state::AppState;
use crate::orchestration::OrchestrationError;
use everevo_core::llm::LlmMessage;
use std::sync::Arc;
use uuid::Uuid;

/// Resolve (or create) a session, load conversation history.
pub async fn resolve_and_load(
    state: &Arc<AppState>,
    session_id_opt: Option<Uuid>,
    message: &str,
) -> Result<(Uuid, Vec<LlmMessage>), OrchestrationError> {
    let session_id = match session_id_opt {
        Some(id) => {
            state
                .db
                .get_session(id)
                .await
                .map_err(|e| OrchestrationError::new("session", format!("DB lookup: {e}")))?
                .ok_or_else(|| OrchestrationError::new("session", "Session not found"))?;
            if !state.sandboxes.read().await.contains_key(&id) {
                let level = super::super::routes::chat::helpers::resolve_permission(
                    &state.config.default_permission_level,
                );
                let _ = state.create_sandbox(id, level, None).await;
            }
            id
        }
        None => {
            let title = super::super::routes::chat::helpers::truncate_for_title(message);
            let row =
                state.db.create_session(&title).await.map_err(|e| {
                    OrchestrationError::new("session", format!("Create session: {e}"))
                })?;
            let level = super::super::routes::chat::helpers::resolve_permission(
                &state.config.default_permission_level,
            );
            let _ = state.create_sandbox(row.id, level, None).await;
            row.id
        }
    };

    let db_messages = state
        .db
        .get_messages(session_id, Some(50))
        .await
        .map_err(|e| OrchestrationError::new("session", format!("Load history: {e}")))?;

    let history: Vec<LlmMessage> = db_messages
        .iter()
        .filter(|m| m.role != "tool") // keep assistant tool-use messages, skip raw tool results
        .map(|m| {
            let mut msg = super::super::routes::chat::helpers::db_message_to_llm(m);
            // Strip stale tool_calls from history — past turns' tool_use blocks
            // without paired tool_results would fail API validation.
            // Text content is preserved so the LLM can see its own reasoning.
            msg.tool_calls = None;
            msg
        })
        .collect();

    Ok((session_id, history))
}
