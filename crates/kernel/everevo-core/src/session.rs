//! Session-scoped shared types — the notification/pending maps that bridge the
//! agent tools (`ask_user`, sandbox confirmation) and the server's SSE stream.
//!
//! Moved here from `everevo-server::app_state` during the P1.1 tool-ownership
//! refactor (architecture-restructure-plan.md): agent-layer tools need these
//! types, and the kernel is the shared home both crates already depend on.

use uuid::Uuid;

/// A pending command awaiting user confirmation. `POST /api/sandbox/confirm`
/// resolves the oneshot (`true` = approve, `false` = deny).
pub struct PendingConfirmation {
    pub command: String,
    pub reason: String,
    pub response_tx: tokio::sync::oneshot::Sender<bool>,
}

/// Notification sent to the SSE stream when a tool needs user confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmationNotification {
    pub session_id: Uuid,
    pub command: String,
    pub reason: String,
}

/// A pending free-text question blocking the `ask_user` tool. The reply
/// arrives at `POST /api/sessions/{id}/ask` and fires the oneshot.
pub struct PendingAsk {
    pub question: String,
    pub reply_tx: tokio::sync::oneshot::Sender<String>,
}

/// Notification sent to the SSE stream when the agent asks the user a question.
#[derive(Debug, Clone)]
pub struct AskNotification {
    pub session_id: Uuid,
    pub question: String,
}
