//! Telemetry injection pipeline — a registered, priority-ordered set of
//! telemetry emitters, mirroring the `ContextStage`/`ContextPipeline` pattern.
//!
//! Instead of scattered `telemetry.record_*()` call sites, record producers are
//! implemented as [`TelemetryStage`]s and registered once via
//! [`TelemetryPipeline::with_stage`]. [`TelemetryPipeline::emit`] runs every
//! stage against a shared [`TelemetryEmitContext`], collects an observability
//! [`TelemetrySnapshot`], and dispatches the produced records to the underlying
//! [`Telemetry`] sink (SQLite background writer).
//!
//! ```ignore
//! let pipeline = default_telemetry_pipeline(Arc::new(Telemetry::new(config)?));
//! let snapshot = pipeline.emit(&TelemetryEmitContext {
//!     trace_id: Some(trace_id),
//!     turn_number: Some(1),
//!     turn_latency_ms: Some(120),
//!     ..Default::default()
//! });
//! ```

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use super::records::{AgentTurnRecord, RetrievalRecord};
use super::trace::Trace;
use super::Telemetry;

// ── Emit context ────────────────────────────────────────────────────────────

/// One emit cycle's input. Each field is optional — a stage only acts on the
/// slice(s) it owns, and stages for missing slices contribute nothing (mirrors
/// `ContextStage::build` returning `None`).
#[derive(Debug, Clone, Default)]
pub struct TelemetryEmitContext {
    pub trace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    // ── Agent turn slice ──
    pub turn_number: Option<i32>,
    pub tool_calls_total: Option<i32>,
    pub tool_calls_success: Option<i32>,
    pub task_completed: Option<bool>,
    pub turn_latency_ms: Option<i64>,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    // ── Retrieval slice ──
    pub retrieval_query: Option<String>,
    pub retrieval_source: Option<String>,
    pub retrieval_recall_k: Option<i32>,
    pub retrieval_precision_at_5: Option<f64>,
    pub retrieval_mrr: Option<f64>,
    pub retrieval_latency_ms: Option<i64>,
    // ── Experiment ──
    pub experiment_id: Option<String>,
    pub variant: Option<String>,
}

// ── Record type ─────────────────────────────────────────────────────────────

/// A record produced by a stage and dispatched to the [`Telemetry`] sink.
#[derive(Debug, Clone)]
pub enum TelemetryRecord {
    AgentTurn(AgentTurnRecord),
    Retrieval(RetrievalRecord),
}

impl TelemetryRecord {
    /// Human-readable kind, used in the snapshot ("agent_turn", "retrieval").
    pub fn kind(&self) -> &'static str {
        match self {
            TelemetryRecord::AgentTurn(_) => "agent_turn",
            TelemetryRecord::Retrieval(_) => "retrieval",
        }
    }
}

// ── Stage trait ─────────────────────────────────────────────────────────────

/// A single telemetry emitter — mirrors `ContextStage::priority/name/build`.
///
/// [`TelemetryStage::emit`] inspects the shared [`TelemetryEmitContext`] and
/// returns the records it owns. Returning an empty `Vec` means "no
/// contribution" for this cycle.
pub trait TelemetryStage: Send + Sync {
    /// Execution order — lower runs first (matches `ContextStage::priority`).
    fn priority(&self) -> i32;

    /// Short name for logs and snapshots (`"agent_turn"`, `"retrieval"`, …).
    fn name(&self) -> &str;

    /// Produce records for this emit cycle. Empty `Vec` = skip.
    fn emit(&self, ctx: &TelemetryEmitContext) -> Vec<TelemetryRecord>;
}

// ── Pipeline ────────────────────────────────────────────────────────────────

/// Registered, priority-ordered collection of telemetry stages backed by a
/// [`Telemetry`] sink. Cheap to clone (`Arc`s + `Option`-typed stages are
/// boxed), mirroring `ContextPipeline`.
pub struct TelemetryPipeline {
    stages: Vec<Box<dyn TelemetryStage>>,
    sink: Arc<Telemetry>,
}

impl TelemetryPipeline {
    pub fn new(sink: Arc<Telemetry>) -> Self {
        Self {
            stages: Vec::new(),
            sink,
        }
    }

    /// Register a stage. Stages are sorted by priority after insertion.
    pub fn with_stage(mut self, stage: impl TelemetryStage + 'static) -> Self {
        self.stages.push(Box::new(stage));
        self.stages.sort_by_key(|s| s.priority());
        self
    }

    /// Run all stages against `ctx` and dispatch produced records to the sink.
    /// Returns an observability snapshot of which stage contributed what.
    pub fn emit(&self, ctx: &TelemetryEmitContext) -> TelemetrySnapshot {
        let mut stage_snapshots = Vec::new();
        for stage in &self.stages {
            let records = stage.emit(ctx);
            let contributed = !records.is_empty();
            let record_types: Vec<String> = records.iter().map(|r| r.kind().to_string()).collect();
            let record_count = records.len();
            for record in records {
                match record {
                    TelemetryRecord::AgentTurn(r) => self.sink.record_agent_turn(r),
                    TelemetryRecord::Retrieval(r) => self.sink.record_retrieval(r),
                }
            }
            tracing::debug!(
                stage = stage.name(),
                contributed,
                record_count,
                "Telemetry stage emitted"
            );
            stage_snapshots.push(StageEmitSnapshot {
                stage_name: stage.name().to_string(),
                priority: stage.priority(),
                contributed,
                record_types,
                record_count,
            });
        }
        TelemetrySnapshot {
            trace_id: ctx.trace_id,
            emitted_at: Utc::now().to_rfc3339(),
            stages: stage_snapshots,
        }
    }

    /// Start a new trace for the session — delegates to the underlying sink.
    /// Returns `None` when telemetry is disabled or dropped by sampling.
    pub fn start_trace(&self, session_id: Uuid) -> Option<Trace> {
        self.sink.start_trace(session_id)
    }
}

// ── Snapshot ────────────────────────────────────────────────────────────────

/// Observability snapshot of one emit cycle — mirrors `ContextSnapshot`.
#[derive(Debug, Clone)]
pub struct TelemetrySnapshot {
    pub trace_id: Option<Uuid>,
    pub emitted_at: String,
    pub stages: Vec<StageEmitSnapshot>,
}

/// Per-stage contribution summary — mirrors `StageSnapshot`.
#[derive(Debug, Clone)]
pub struct StageEmitSnapshot {
    pub stage_name: String,
    pub priority: i32,
    /// Whether the stage produced records this cycle (false = skipped).
    pub contributed: bool,
    /// Record kinds produced (e.g. `["agent_turn"]`).
    pub record_types: Vec<String>,
    pub record_count: usize,
}

// ── Built-in stages ─────────────────────────────────────────────────────────

/// Emits a [`RetrievalRecord`] when the retrieval slice is present.
#[derive(Debug, Default)]
pub struct RetrievalTelemetryStage;

impl TelemetryStage for RetrievalTelemetryStage {
    fn priority(&self) -> i32 {
        10
    }

    fn name(&self) -> &str {
        "retrieval"
    }

    fn emit(&self, ctx: &TelemetryEmitContext) -> Vec<TelemetryRecord> {
        let (Some(trace_id), Some(query)) = (ctx.trace_id, ctx.retrieval_query.clone()) else {
            return Vec::new();
        };
        vec![TelemetryRecord::Retrieval(RetrievalRecord {
            trace_id,
            query,
            source: ctx.retrieval_source.clone().unwrap_or_default(),
            recall_k: ctx.retrieval_recall_k.unwrap_or(0),
            precision_at_5: ctx.retrieval_precision_at_5,
            mrr: ctx.retrieval_mrr,
            latency_ms: ctx.retrieval_latency_ms.unwrap_or(0),
            experiment_id: ctx.experiment_id.clone(),
            variant: ctx.variant.clone(),
        })]
    }
}

/// Emits an [`AgentTurnRecord`] when the agent-turn slice is present.
#[derive(Debug, Default)]
pub struct TurnTelemetryStage;

impl TelemetryStage for TurnTelemetryStage {
    fn priority(&self) -> i32 {
        20
    }

    fn name(&self) -> &str {
        "agent_turn"
    }

    fn emit(&self, ctx: &TelemetryEmitContext) -> Vec<TelemetryRecord> {
        let (Some(trace_id), Some(turn_number)) = (ctx.trace_id, ctx.turn_number) else {
            return Vec::new();
        };
        vec![TelemetryRecord::AgentTurn(AgentTurnRecord {
            trace_id,
            turn_number,
            tool_calls_total: ctx.tool_calls_total.unwrap_or(0),
            tool_calls_success: ctx.tool_calls_success.unwrap_or(0),
            task_completed: ctx.task_completed.unwrap_or(false),
            latency_ms: ctx.turn_latency_ms.unwrap_or(0),
            tokens_input: ctx.tokens_input.unwrap_or(0),
            tokens_output: ctx.tokens_output.unwrap_or(0),
            error_type: ctx.error_type.clone(),
            error_message: ctx.error_message.clone(),
            experiment_id: ctx.experiment_id.clone(),
            variant: ctx.variant.clone(),
        })]
    }
}

/// The default pipeline — mirrors `context::default_pipeline()`.
///
/// Registered stages (priority order): retrieval (10), agent_turn (20).
/// Future instrumentation (llm.rs spans, sandbox, domain/vector/kg, …) is
/// added here as new [`TelemetryStage`] implementations.
pub fn default_telemetry_pipeline(sink: Arc<Telemetry>) -> TelemetryPipeline {
    TelemetryPipeline::new(sink)
        .with_stage(RetrievalTelemetryStage)
        .with_stage(TurnTelemetryStage)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TelemetryConfig;
    use std::path::PathBuf;

    fn telemetry(enabled: bool) -> Telemetry {
        Telemetry::new(TelemetryConfig {
            enabled,
            sample_rate: 1.0,
            db_path: PathBuf::from(":memory:"),
        })
        .expect("telemetry should init")
    }

    #[test]
    fn stages_are_sorted_by_priority() {
        let pipeline = TelemetryPipeline::new(Arc::new(telemetry(false)))
            .with_stage(TurnTelemetryStage)
            .with_stage(RetrievalTelemetryStage);
        let names: Vec<String> = pipeline
            .stages
            .iter()
            .map(|s| s.name().to_string())
            .collect();
        assert_eq!(names, vec!["retrieval", "agent_turn"]);
    }

    #[test]
    fn empty_context_contributes_nothing() {
        let pipeline = default_telemetry_pipeline(Arc::new(telemetry(false)));
        let snapshot = pipeline.emit(&TelemetryEmitContext::default());
        assert_eq!(snapshot.stages.len(), 2);
        assert!(snapshot.stages.iter().all(|s| !s.contributed));
        assert!(snapshot.stages.iter().all(|s| s.record_types.is_empty()));
    }

    #[test]
    fn turn_slice_writes_agent_turn_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = Arc::new(
            Telemetry::new(TelemetryConfig {
                enabled: true,
                sample_rate: 1.0,
                db_path: dir.path().join("turn.db"),
            })
            .expect("telemetry"),
        );
        let pipeline = default_telemetry_pipeline(Arc::clone(&sink));
        let trace_id = Uuid::new_v4();

        let snapshot = pipeline.emit(&TelemetryEmitContext {
            trace_id: Some(trace_id),
            turn_number: Some(2),
            tool_calls_total: Some(5),
            tool_calls_success: Some(4),
            task_completed: Some(true),
            turn_latency_ms: Some(1234),
            tokens_input: Some(1500),
            tokens_output: Some(800),
            experiment_id: Some("exp-1".into()),
            variant: Some("v1".into()),
            ..Default::default()
        });

        let turn = snapshot
            .stages
            .iter()
            .find(|s| s.stage_name == "agent_turn")
            .expect("agent_turn stage");
        assert!(turn.contributed);
        assert_eq!(turn.record_count, 1);

        sink.flush();
        drop(sink);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&dir.path().join("turn.db"))
                .await
                .unwrap();
            let row: (String, i32, i32, i32, i32, i64, i64, i64, Option<String>) = sqlx::query_as(
                "SELECT trace_id, turn_number, tool_calls_total, tool_calls_success, \
                        task_completed, latency_ms, tokens_input, tokens_output, experiment_id \
                 FROM telemetry_agent_turns",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(row.0, trace_id.to_string());
            assert_eq!(row.1, 2);
            assert_eq!(row.2, 5);
            assert_eq!(row.3, 4);
            assert_eq!(row.4, 1);
            assert_eq!(row.5, 1234);
            assert_eq!(row.6, 1500);
            assert_eq!(row.7, 800);
            assert_eq!(row.8.as_deref(), Some("exp-1"));
        });
    }

    #[test]
    fn retrieval_slice_writes_retrieval_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = Arc::new(
            Telemetry::new(TelemetryConfig {
                enabled: true,
                sample_rate: 1.0,
                db_path: dir.path().join("retrieval.db"),
            })
            .expect("telemetry"),
        );
        let pipeline = default_telemetry_pipeline(Arc::clone(&sink));
        let trace_id = Uuid::new_v4();

        let snapshot = pipeline.emit(&TelemetryEmitContext {
            trace_id: Some(trace_id),
            retrieval_query: Some("best widget".into()),
            retrieval_source: Some("memory-rrf".into()),
            retrieval_recall_k: Some(7),
            retrieval_precision_at_5: Some(0.8),
            retrieval_mrr: Some(0.75),
            retrieval_latency_ms: Some(55),
            experiment_id: Some("exp-2".into()),
            variant: Some("baseline".into()),
            ..Default::default()
        });

        let retrieval = snapshot
            .stages
            .iter()
            .find(|s| s.stage_name == "retrieval")
            .expect("retrieval stage");
        assert!(retrieval.contributed);

        sink.flush();
        drop(sink);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = crate::telemetry::writer::create_pool(&dir.path().join("retrieval.db"))
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
                Option<String>,
            ) = sqlx::query_as(
                "SELECT trace_id, query, recall_k, precision_at_5, mrr, latency_ms, \
                            experiment_id, variant \
                     FROM telemetry_retrievals",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(row.0, trace_id.to_string());
            assert_eq!(row.1, "best widget");
            assert_eq!(row.2, 7);
            assert!((row.3.unwrap() - 0.8).abs() < f64::EPSILON);
            assert!((row.4.unwrap() - 0.75).abs() < f64::EPSILON);
            assert_eq!(row.5, 55);
            assert_eq!(row.6.as_deref(), Some("exp-2"));
            assert_eq!(row.7.as_deref(), Some("baseline"));
        });
    }

    #[test]
    fn both_slices_emit_two_records_in_one_cycle() {
        let pipeline = default_telemetry_pipeline(Arc::new(telemetry(false)));
        let trace_id = Uuid::new_v4();

        let snapshot = pipeline.emit(&TelemetryEmitContext {
            trace_id: Some(trace_id),
            turn_number: Some(1),
            retrieval_query: Some("q".into()),
            ..Default::default()
        });

        assert_eq!(snapshot.stages.len(), 2);
        assert!(snapshot.stages.iter().all(|s| s.contributed));
        assert_eq!(
            snapshot
                .stages
                .iter()
                .map(|s| s.record_count)
                .sum::<usize>(),
            2
        );
    }

    #[test]
    fn disabled_telemetry_emit_does_not_panic() {
        let pipeline = default_telemetry_pipeline(Arc::new(telemetry(false)));
        let snapshot = pipeline.emit(&TelemetryEmitContext {
            trace_id: Some(Uuid::new_v4()),
            turn_number: Some(1),
            retrieval_query: Some("q".into()),
            ..Default::default()
        });
        // Stages still produce records; dispatch is a no-op on a disabled sink.
        assert!(snapshot.stages.iter().all(|s| s.contributed));
        // start_trace delegates to the disabled sink → None.
        assert!(pipeline.start_trace(Uuid::new_v4()).is_none());
    }
}
