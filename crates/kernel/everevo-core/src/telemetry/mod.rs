//! EverEvo Telemetry — observability and metrics for the agent system.
//!
//! Provides fire-and-forget span tracing, retrieval-metrics recording, and
//! agent-turn telemetry backed by SQLite. All writes are dispatched to a
//! dedicated background thread so the hot path (including [`SpanGuard::drop`])
//! never blocks on I/O.

mod config;
mod records;
mod trace;
mod writer;

use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use uuid::Uuid;

use crate::EverEvoError;
use config::WriteCmd;
use trace::should_sample;
use writer::run_writer;

// Re-exports
pub use config::TelemetryConfig;
pub use records::{AgentTurnRecord, RetrievalRecord};
pub use trace::{SpanGuard, Trace};

// ── Telemetry ──────────────────────────────────────────────────────────────

/// Central telemetry handle.
///
/// Cheap to clone — the internal sender is an `mpsc::Sender` and the config is
/// `Arc<RwLock<…>>`. Only the original instance (returned by [`Telemetry::new`])
/// owns the writer thread handle; clones do not.
pub struct Telemetry {
    pub(crate) config: Arc<RwLock<TelemetryConfig>>,
    pub(crate) tx: Option<mpsc::Sender<WriteCmd>>,
    /// Only `Some` on the original; clones get `None`.
    _writer: Option<Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>>,
}

impl Clone for Telemetry {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            tx: self.tx.clone(),
            _writer: None,
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        // Drop the sender so the writer thread sees a closed channel.
        self.tx = None;
        // If this is the original, wait for the writer to finish closing the
        // database so that file-based tests can safely re-open it.
        if let Some(writer) = &self._writer {
            if let Ok(mut guard) = writer.lock() {
                if let Some(handle) = guard.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl Telemetry {
    /// Create a new telemetry subsystem.
    ///
    /// When `config.enabled` is `true` this spawns a background writer thread,
    /// creates the SQLite database (if it does not already exist), and runs
    /// `CREATE TABLE IF NOT EXISTS` for all telemetry tables.
    pub fn new(config: TelemetryConfig) -> Result<Self, EverEvoError> {
        if !config.enabled {
            return Ok(Self {
                config: Arc::new(RwLock::new(config)),
                tx: None,
                _writer: None,
            });
        }

        let (tx, rx) = mpsc::channel::<WriteCmd>();
        let db_path = config.db_path.clone();

        let handle = std::thread::Builder::new()
            .name("everevo-telemetry-writer".into())
            .spawn(move || {
                run_writer(rx, &db_path);
            })
            .map_err(|e| {
                EverEvoError::Internal(format!("failed to spawn telemetry writer thread: {e}"))
            })?;

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            tx: Some(tx),
            _writer: Some(Arc::new(std::sync::Mutex::new(Some(handle)))),
        })
    }

    /// Start a new trace for the given session.
    ///
    /// Returns `None` when telemetry is disabled or the trace is discarded by
    /// the configured sample rate.
    pub fn start_trace(&self, session_id: Uuid) -> Option<Trace> {
        let config = self.config.read().ok()?;
        if !config.enabled {
            return None;
        }
        if !should_sample(config.sample_rate) {
            return None;
        }
        drop(config);

        Some(Trace {
            trace_id: Uuid::new_v4(),
            session_id,
            experiment_id: None,
            variant: None,
            started_at: Utc::now(),
            telemetry: Arc::new(self.clone()),
        })
    }

    /// Record a retrieval-quality metric (fire-and-forget).
    pub fn record_retrieval(&self, record: RetrievalRecord) {
        let Some(tx) = &self.tx else { return };
        let _ = tx.send(WriteCmd::Retrieval {
            id: Uuid::new_v4().to_string(),
            trace_id: record.trace_id.to_string(),
            query: record.query,
            source: record.source,
            recall_k: record.recall_k,
            precision_at_5: record.precision_at_5,
            mrr: record.mrr,
            latency_ms: record.latency_ms,
            experiment_id: record.experiment_id,
            variant: record.variant,
        });
    }

    /// Record per-turn agent telemetry (fire-and-forget).
    pub fn record_agent_turn(&self, record: AgentTurnRecord) {
        let Some(tx) = &self.tx else { return };
        let _ = tx.send(WriteCmd::AgentTurn {
            id: Uuid::new_v4().to_string(),
            trace_id: record.trace_id.to_string(),
            turn_number: record.turn_number,
            tool_calls_total: record.tool_calls_total,
            tool_calls_success: record.tool_calls_success,
            task_completed: i32::from(record.task_completed),
            latency_ms: record.latency_ms,
            tokens_input: record.tokens_input,
            tokens_output: record.tokens_output,
            experiment_id: record.experiment_id,
            variant: record.variant,
        });
    }

    /// Block until all previously submitted writes have been flushed to the
    /// database. Useful in tests and before shutdown.
    pub fn flush(&self) {
        let Some(tx) = &self.tx else { return };
        let (done_tx, done_rx) = mpsc::channel();
        let _ = tx.send(WriteCmd::Flush(done_tx));
        let _ = done_rx.recv();
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn disabled_telemetry_is_noop() {
        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: false,
            sample_rate: 1.0,
            db_path: PathBuf::from(":memory:"),
        })
        .expect("should succeed");

        let session_id = Uuid::new_v4();
        assert!(telemetry.start_trace(session_id).is_none());

        // These should not panic when telemetry is disabled.
        telemetry.record_retrieval(RetrievalRecord {
            trace_id: Uuid::new_v4(),
            query: "q".into(),
            source: "s".into(),
            recall_k: 1,
            precision_at_5: None,
            mrr: None,
            latency_ms: 1,
            experiment_id: None,
            variant: None,
        });
        telemetry.record_agent_turn(AgentTurnRecord {
            trace_id: Uuid::new_v4(),
            turn_number: 1,
            tool_calls_total: 0,
            tool_calls_success: 0,
            task_completed: false,
            latency_ms: 1,
            tokens_input: 0,
            tokens_output: 0,
            experiment_id: None,
            variant: None,
        });
        telemetry.flush();
    }

    #[test]
    fn sample_rate_zero_discards_all() {
        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 0.0,
            db_path: PathBuf::from(":memory:"),
        })
        .expect("should succeed");

        let session_id = Uuid::new_v4();
        for _ in 0..100 {
            assert!(telemetry.start_trace(session_id).is_none());
        }
    }

    #[test]
    fn sample_rate_one_captures_all() {
        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 1.0,
            db_path: PathBuf::from(":memory:"),
        })
        .expect("should succeed");

        let session_id = Uuid::new_v4();
        for _ in 0..20 {
            assert!(telemetry.start_trace(session_id).is_some());
        }
    }

    #[test]
    fn span_drop_writes_to_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test-span.db");

        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 1.0,
            db_path: db_path.clone(),
        })
        .expect("failed to create telemetry");

        let session_id = Uuid::new_v4();
        let trace_id;
        {
            let mut trace = telemetry.start_trace(session_id).unwrap();
            trace_id = trace.trace_id;
            {
                let _span = trace
                    .span("llm.call")
                    .with("model", "test-model")
                    .metric("latency_ms", 123.0)
                    .status("ok");
            }
        }

        telemetry.flush();
        drop(telemetry);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&db_path)
                .await
                .unwrap();

            let row: (String, String, String, i64) =
                sqlx::query_as("SELECT trace_id, name, status, duration_ms FROM telemetry_spans")
                    .fetch_one(&pool)
                    .await
                    .unwrap();

            assert_eq!(row.0, trace_id.to_string());
            assert_eq!(row.1, "llm.call");
            assert_eq!(row.2, "ok");
            assert!(row.3 >= 0);
        });
    }

    #[test]
    fn retrieval_record_writes_to_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test-retrieval.db");

        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 1.0,
            db_path: db_path.clone(),
        })
        .expect("failed to create telemetry");

        let trace_id = Uuid::new_v4();
        telemetry.record_retrieval(RetrievalRecord {
            trace_id,
            query: "test query".into(),
            source: "vector".into(),
            recall_k: 10,
            precision_at_5: Some(0.8),
            mrr: Some(0.75),
            latency_ms: 55,
            experiment_id: Some("exp-1".into()),
            variant: Some("v1".into()),
        });

        telemetry.flush();
        drop(telemetry);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&db_path)
                .await
                .unwrap();

            let row: (
                String,
                String,
                i32,
                Option<f64>,
                Option<f64>,
                i64,
                Option<String>,
            ) = sqlx::query_as(
                "SELECT trace_id, query, recall_k, precision_at_5, mrr, latency_ms, experiment_id \
                 FROM telemetry_retrievals",
            )
            .fetch_one(&pool)
            .await
            .unwrap();

            assert_eq!(row.0, trace_id.to_string());
            assert_eq!(row.1, "test query");
            assert_eq!(row.2, 10);
            assert!((row.3.unwrap() - 0.8).abs() < f64::EPSILON);
            assert!((row.4.unwrap() - 0.75).abs() < f64::EPSILON);
            assert_eq!(row.5, 55);
            assert_eq!(row.6.as_deref(), Some("exp-1"));
        });
    }

    #[test]
    fn agent_turn_writes_to_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test-agent-turn.db");

        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 1.0,
            db_path: db_path.clone(),
        })
        .expect("failed to create telemetry");

        let trace_id = Uuid::new_v4();
        telemetry.record_agent_turn(AgentTurnRecord {
            trace_id,
            turn_number: 3,
            tool_calls_total: 5,
            tool_calls_success: 4,
            task_completed: true,
            latency_ms: 1200,
            tokens_input: 1500,
            tokens_output: 800,
            experiment_id: Some("exp-2".into()),
            variant: Some("baseline".into()),
        });

        telemetry.flush();
        drop(telemetry);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&db_path)
                .await
                .unwrap();

            let row: (String, i32, i32, i32, i32, i64, i64, Option<String>) = sqlx::query_as(
                "SELECT trace_id, turn_number, tool_calls_total, tool_calls_success, \
                        task_completed, tokens_input, tokens_output, variant \
                 FROM telemetry_agent_turns",
            )
            .fetch_one(&pool)
            .await
            .unwrap();

            assert_eq!(row.0, trace_id.to_string());
            assert_eq!(row.1, 3);
            assert_eq!(row.2, 5);
            assert_eq!(row.3, 4);
            assert_eq!(row.4, 1);
            assert_eq!(row.5, 1500);
            assert_eq!(row.6, 800);
            assert_eq!(row.7.as_deref(), Some("baseline"));
        });
    }

    #[test]
    fn span_metadata_and_metrics_are_json_serialized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test-meta.db");

        let telemetry = Telemetry::new(TelemetryConfig {
            enabled: true,
            sample_rate: 1.0,
            db_path: db_path.clone(),
        })
        .expect("failed to create telemetry");

        #[derive(serde::Serialize)]
        struct Nested {
            foo: i32,
        }

        {
            let mut trace = telemetry.start_trace(Uuid::new_v4()).unwrap();
            {
                let _span = trace
                    .span("complex.span")
                    .with("string_val", "hello")
                    .with("nested", &Nested { foo: 42 })
                    .metric("score", 0.95)
                    .metric("count", 3.0)
                    .status("ok");
            }
        }

        telemetry.flush();
        drop(telemetry);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&db_path)
                .await
                .unwrap();

            let row: (String, String) =
                sqlx::query_as("SELECT metadata, metrics FROM telemetry_spans")
                    .fetch_one(&pool)
                    .await
                    .unwrap();

            let metadata: serde_json::Value =
                serde_json::from_str(&row.0).expect("metadata should be valid JSON");
            let metrics: serde_json::Value =
                serde_json::from_str(&row.1).expect("metrics should be valid JSON");

            assert_eq!(metadata["string_val"], "hello");
            assert_eq!(metadata["nested"]["foo"], 42);
            assert_eq!(metrics["score"], 0.95);
            assert_eq!(metrics["count"], 3.0);
        });
    }
}
