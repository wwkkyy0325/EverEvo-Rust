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

pub use tools::AssembledTools;

/// Build the per-session tool registry with all dependencies injected.
pub async fn build_registry(
    state: &Arc<AppState>,
    session_id: Uuid,
    client: &Arc<everevo_agent::llm::HttpClient>,
    notif_tx: &mpsc::UnboundedSender<crate::app_state::ConfirmationNotification>,
    permission_level: &str,
    sub_ctx: &everevo_agent::subagent_context::SubAgentContext,
) -> AssembledTools {
    tools::assemble(
        state,
        session_id,
        client,
        notif_tx,
        permission_level,
        sub_ctx,
    )
    .await
}

/// Persist the assistant message and send closing SSE events with a 5s timeout.
#[allow(clippy::too_many_arguments)]
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
        ),
    )
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(OrchestrationError::new("response", e)),
        Err(_elapsed) => {
            // Timeout: send a minimal done event so the frontend doesn't hang
            let done = serde_json::json!({"session_id": session_id, "message_id": assistant_id});
            let _ = tx
                .send(Ok(Event::default().event("done").data(done.to_string())))
                .await;
            tracing::warn!(%session_id, "Response finalization timed out — sent minimal done");
            Ok(())
        }
    }
}
