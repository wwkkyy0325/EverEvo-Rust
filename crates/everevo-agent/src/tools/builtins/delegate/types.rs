//! Shared sub-agent types — handles for monitoring and cancellation.

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Handle to a running sub-agent — enables monitoring and cancellation.
#[derive(Clone)]
pub struct SubAgentHandle {
    pub id: Uuid,
    pub description: String,
    pub started_at: chrono::DateTime<Utc>,
    pub cancel: CancellationToken,
}

/// Snapshot of sub-agent status for API reporting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubAgentStatus {
    pub id: Uuid,
    pub description: String,
    pub started_at: String,
    pub status: String, // "running" | "completed" | "failed" | "timeout" | "cancelled"
    pub elapsed_ms: u64,
}
