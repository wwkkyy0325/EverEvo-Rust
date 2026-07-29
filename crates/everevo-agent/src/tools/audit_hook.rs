//! Audit hook — logs every tool call with timing and outcome.
//!
//! Implements the `ToolHook` trait from everevo-core. Register via
//! `registry.add_hook(Arc::new(AuditHook))` to enable full tool audit trail.

use async_trait::async_trait;
use everevo_core::tool::{ToolHook, ToolOutput};
use everevo_core::EverEvoError;
use std::sync::atomic::{AtomicU64, Ordering};

/// Logs every tool execution with timing and result.
pub struct AuditHook {
    call_count: AtomicU64,
}

impl AuditHook {
    pub fn new() -> Self {
        Self {
            call_count: AtomicU64::new(0),
        }
    }

    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl Default for AuditHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHook for AuditHook {
    async fn pre_execute(
        &self,
        tool_name: &str,
        _params: &serde_json::Value,
    ) -> Result<(), EverEvoError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(%tool_name, call = self.call_count(), "Tool execution started");
        Ok(())
    }

    async fn post_execute(
        &self,
        tool_name: &str,
        _params: &serde_json::Value,
        result: &Result<ToolOutput, EverEvoError>,
    ) {
        match result {
            Ok(output) => {
                let status = if output.is_error { "error" } else { "ok" };
                tracing::info!(%tool_name, status, content_len = output.content.len(), "Tool execution completed");
            }
            Err(e) => {
                tracing::error!(%tool_name, error = %e, "Tool execution failed");
            }
        }
    }
}

// Future: timing hook with Duration tracking.
// Use std::time::Instant in pre_execute, compute elapsed in post_execute.
