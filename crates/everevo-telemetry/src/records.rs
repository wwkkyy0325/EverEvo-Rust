//! Public record types for telemetry data.

use uuid::Uuid;

/// A retrieval-quality metric to be persisted.
#[derive(Debug, Clone)]
pub struct RetrievalRecord {
    pub trace_id: Uuid,
    pub query: String,
    pub source: String,
    pub recall_k: i32,
    pub precision_at_5: Option<f64>,
    pub mrr: Option<f64>,
    pub latency_ms: i64,
    pub experiment_id: Option<String>,
    pub variant: Option<String>,
}

/// Per-turn agent telemetry to be persisted.
#[derive(Debug, Clone)]
pub struct AgentTurnRecord {
    pub trace_id: Uuid,
    pub turn_number: i32,
    pub tool_calls_total: i32,
    pub tool_calls_success: i32,
    pub task_completed: bool,
    pub latency_ms: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub experiment_id: Option<String>,
    pub variant: Option<String>,
}
