//! Telemetry configuration and internal command types.

use std::path::PathBuf;
use std::sync::mpsc;

/// Configuration for the telemetry subsystem.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled. When `false` every public method becomes
    /// a no-op and no database file is created.
    pub enabled: bool,
    /// Fraction of traces to capture, 0.0 – 1.0. 1.0 captures everything.
    pub sample_rate: f32,
    /// Path to the SQLite database file. `":memory:"` is supported.
    pub db_path: PathBuf,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_rate: 1.0,
            db_path: PathBuf::from("telemetry.db"),
        }
    }
}

// ── Internal write-command types ───────────────────────────────────────────

pub(crate) enum WriteCmd {
    Span {
        id: String,
        trace_id: String,
        parent_id: Option<String>,
        name: String,
        started_at: String,
        duration_ms: i64,
        status: String,
        metadata: String,
        metrics: String,
    },
    Retrieval {
        id: String,
        trace_id: String,
        query: String,
        source: String,
        recall_k: i32,
        precision_at_5: Option<f64>,
        mrr: Option<f64>,
        latency_ms: i64,
        experiment_id: Option<String>,
        variant: Option<String>,
    },
    AgentTurn {
        id: String,
        trace_id: String,
        turn_number: i32,
        tool_calls_total: i32,
        tool_calls_success: i32,
        task_completed: i32,
        latency_ms: i64,
        tokens_input: i64,
        tokens_output: i64,
        experiment_id: Option<String>,
        variant: Option<String>,
    },
    /// Flush signal; the writer sends back `()` on the oneshot channel once
    /// it has processed all preceding commands.
    Flush(mpsc::Sender<()>),
}
