//! Chat orchestration — extracted from `chat.rs:handle_chat()`.
//!
//! Each function handles one phase of the chat request lifecycle.
//! State is passed explicitly between phases for testability.
//!
//! ## Reliability guarantees
//!
//! - `SessionGuard` auto-cleans sandbox on drop (even on panic)
//! - `OrchestrationError` tracks which phase failed
//! - All SSE channels are closed on error to prevent hangs

pub mod content_block;
pub mod response;
pub mod session;
pub mod session_coordinator;
pub mod stream;
pub mod tools;

pub use content_block::{ContentBlockStreamer, StreamerAction};

use axum::response::sse::Event;
use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::app_state::AppState;
use everevo_core::llm::LlmMessage;

// ── Error type ────────────────────────────────────────────────────────

/// Structured orchestration error — records which phase failed.
#[derive(Debug)]
pub struct OrchestrationError {
    pub phase: &'static str,
    pub message: String,
}

impl fmt::Display for OrchestrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.phase, self.message)
    }
}

impl OrchestrationError {
    pub fn new(phase: &'static str, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
        }
    }
}

impl From<String> for OrchestrationError {
    fn from(s: String) -> Self {
        Self {
            phase: "orchestration",
            message: s,
        }
    }
}

impl From<everevo_core::EverEvoError> for OrchestrationError {
    fn from(e: everevo_core::EverEvoError) -> Self {
        Self {
            phase: "core",
            message: e.to_string(),
        }
    }
}

/// RAII guard that ensures sandbox cleanup on drop (even after panic).
/// Registered for a session — destroys the sandbox when dropped.
pub struct SessionGuard {
    state: Arc<AppState>,
    session_id: Uuid,
    active: bool,
}

impl SessionGuard {
    pub fn new(state: &Arc<AppState>, session_id: Uuid) -> Self {
        Self {
            state: Arc::clone(state),
            session_id,
            active: true,
        }
    }

    /// Prevent cleanup — call when the session should persist.
    pub fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.active {
            let state = Arc::clone(&self.state);
            let sid = self.session_id;
            tokio::task::spawn(async move {
                state.destroy_sandbox(sid).await;
            });
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────

/// Resolve or create a session, returning (session_id, history_messages).
pub async fn resolve_session(
    state: &Arc<AppState>,
    session_id_opt: Option<Uuid>,
    message: &str,
) -> Result<(Uuid, Vec<LlmMessage>), OrchestrationError> {
    session::resolve_and_load(state, session_id_opt, message).await
}

pub use session_coordinator::{SessionCoordinator, SessionReceivers};
pub use tools::AssembledTools;

/// Build the per-session tool registry with all dependencies injected.
pub async fn build_registry(
    state: &Arc<AppState>,
    session_id: Uuid,
    client: &Arc<everevo_agent::llm::HttpClient>,
    coord: &mut SessionCoordinator,
    permission_level: &str,
    sub_ctx: &everevo_agent::subagent_context::SubAgentContext,
) -> AssembledTools {
    tools::assemble(
        state,
        session_id,
        client,
        &coord.confirm_tx,
        &coord.ask_user_tx,
        permission_level,
        sub_ctx,
    )
    .await
}

/// Persist the assistant message and send closing SSE events with a 5s timeout.
#[allow(clippy::too_many_arguments)]
/// Persist a session's lifecycle state (`SessionState`) into its DB metadata
/// JSON. Revives the previously-dead `SessionState` enum so the status endpoint
/// and reconnect path report real state instead of an implicit-`idle` default.
pub async fn set_session_state(
    db: &everevo_db::Database,
    session_id: Uuid,
    new_state: everevo_core::types::SessionState,
) {
    let Ok(Some(session)) = db.get_session(session_id).await else {
        return;
    };
    let mut meta: everevo_core::types::SessionMeta =
        serde_json::from_str(&session.metadata).unwrap_or_default();
    let state_str = new_state.as_str();
    meta.state = new_state;
    let serialized = serde_json::to_string(&meta).unwrap_or_default();
    if let Err(e) = db.update_session_metadata(session_id, &serialized).await {
        tracing::warn!(%session_id, state = %state_str, error = %e, "Failed to persist session state");
    }
}

#[allow(clippy::too_many_arguments)] // finalize bundles a full persisted response
pub async fn finalize_response(
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
) -> Result<(), OrchestrationError> {
    // Wrap in a 5-second timeout — if the DB is slow, we still send done
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        response::persist_and_send(
            tx,
            state,
            session_id,
            assistant_id,
            full_response,
            thinking,
            persisted_blocks,
            thinking_open,
            text_block_idx,
            block_index,
            input_tokens,
            output_tokens,
        ),
    )
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(OrchestrationError::new("response", e)),
        Err(_elapsed) => {
            // Timeout: send a minimal done event (token counts unknown = 0)
            let done = serde_json::json!({"session_id": session_id, "message_id": assistant_id, "input_tokens": 0, "output_tokens": 0});
            let _ = tx
                .send(Ok(Event::default().event("done").data(done.to_string())))
                .await;
            tracing::warn!(%session_id, "Response finalization timed out — sent minimal done");
            Ok(())
        }
    }
}

/// Send a structured error event through the SSE channel.
///
/// The frontend renders SSE `error` events as an inline error block.
/// This helper ensures consistent formatting with the `ErrorCode` enum.
#[allow(dead_code)]
pub(crate) fn send_sse_error(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    code: everevo_core::ErrorCode,
    msg: &str,
) {
    let _ = tx.try_send(Ok(Event::default()
        .event("error")
        .data(serde_json::json!({"code": code, "message": msg}).to_string())));
}
