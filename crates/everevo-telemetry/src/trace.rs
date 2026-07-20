//! Trace and SpanGuard — active tracing with auto-persist on drop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::config::WriteCmd;
use crate::Telemetry;

/// An active trace that can spawn child spans.
pub struct Trace {
    pub trace_id: Uuid,
    pub session_id: Uuid,
    pub experiment_id: Option<String>,
    pub variant: Option<String>,
    pub started_at: DateTime<Utc>,
    pub(crate) telemetry: Arc<Telemetry>,
}

impl Trace {
    /// Open a new span. The span is automatically written to the database when
    /// it is dropped.
    pub fn span(&mut self, name: impl Into<String>) -> SpanGuard {
        SpanGuard {
            id: Uuid::new_v4(),
            trace_id: self.trace_id,
            parent_id: None,
            name: name.into(),
            started_at: Utc::now(),
            start: Instant::now(),
            status: "ok".into(),
            metadata: HashMap::new(),
            metrics: HashMap::new(),
            telemetry: Arc::clone(&self.telemetry),
        }
    }
}

// ── SpanGuard ──────────────────────────────────────────────────────────────

/// A builder-style guard that persists a span row on drop.
///
/// ```ignore
/// let mut trace = telemetry.start_trace(session_id).unwrap();
/// {
///     let _span = trace.span("llm.call")
///         .with("model", "claude-3")
///         .metric("tokens", 512.0)
///         .status("ok");
///     // … work happens here …
/// } // ← INSERT INTO telemetry_spans fires here
/// ```
pub struct SpanGuard {
    id: Uuid,
    trace_id: Uuid,
    parent_id: Option<String>,
    name: String,
    started_at: DateTime<Utc>,
    start: Instant,
    status: String,
    metadata: HashMap<String, serde_json::Value>,
    metrics: HashMap<String, f64>,
    telemetry: Arc<Telemetry>,
}

impl SpanGuard {
    /// Attach a metadata key/value pair. The value must be `Serialize`.
    pub fn with(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(value) {
            self.metadata.insert(key.into(), v);
        }
        self
    }

    /// Attach a numeric metric.
    pub fn metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(key.into(), value);
        self
    }

    /// Override the span status (default: `"ok"`).
    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        let Some(tx) = &self.telemetry.tx else {
            return;
        };

        let duration_ms = self.start.elapsed().as_millis() as i64;
        let metadata = serde_json::to_string(&self.metadata).unwrap_or_default();
        let metrics = serde_json::to_string(&self.metrics).unwrap_or_default();

        let _ = tx.send(WriteCmd::Span {
            id: self.id.to_string(),
            trace_id: self.trace_id.to_string(),
            parent_id: self.parent_id.clone(),
            name: self.name.clone(),
            started_at: self.started_at.to_rfc3339(),
            duration_ms,
            status: self.status.clone(),
            metadata,
            metrics,
        });
    }
}

// ── Sampling helper ────────────────────────────────────────────────────────

/// Determine whether a trace should be sampled based on the configured rate.
pub fn should_sample(rate: f32) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 || rate.is_nan() {
        return false;
    }
    // Use high 64 bits of a fresh v4 UUID as a pseudo-random float in [0, 1).
    let r = (Uuid::new_v4().as_u128() >> 64) as u64;
    (r as f64) / (u64::MAX as f64) < rate as f64
}
