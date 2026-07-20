//! Bootstrap tool — wraps everevo-bootstrap.
//!
//! The agent uses this to check and provision runtimes (Python, Node, Git, ONNX)
//! and embedding models (BGE-small-zh, all-MiniLM-L6-v2).

use std::sync::Arc;

use async_trait::async_trait;
use everevo_bootstrap::Bootstrap;

use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;

pub struct BootstrapTool {
    bootstrap: Arc<Bootstrap>,
}

impl BootstrapTool {
    pub fn new(bootstrap: Arc<Bootstrap>) -> Self {
        Self { bootstrap }
    }
}

#[async_trait]
impl Tool for BootstrapTool {
    fn name(&self) -> &str {
        "bootstrap_check"
    }

    fn description(&self) -> &str {
        "Check the status of portable runtimes (Python, Node.js, Git, ONNX Runtime) \
         and embedding models (Chinese: BGE-small-zh, English: all-MiniLM-L6-v2). \
         Returns which assets are ready, missing, or corrupt."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        let status = self.bootstrap.check().await.map_err(|e| {
            EverEvoError::Tool(format!("Bootstrap check failed: {e}"))
        })?;

        let mut lines = Vec::new();

        if !status.ready.is_empty() {
            lines.push(format!("Ready ({}):", status.ready.len()));
            for r in &status.ready {
                lines.push(format!("  ✅ {} v{}", r.key, r.version));
            }
        }
        if !status.corrupt.is_empty() {
            lines.push(format!("Corrupt ({}):", status.corrupt.len()));
            for c in &status.corrupt {
                lines.push(format!("  ⚠️  {} v{} — re-download needed", c.key, c.version));
            }
        }
        if !status.missing.is_empty() {
            lines.push(format!(
                "Missing ({}), ~{} MB to download:",
                status.missing.len(),
                status.download_size_bytes / 1_048_576
            ));
            for m in &status.missing {
                lines.push(format!(
                    "  ❌ {} v{} — {}",
                    m.key, m.version, m.description
                ));
            }
        }
        if status.ready.len() == 8 {
            lines.push("All 8 assets ready. ✅".into());
        }

        Ok(ToolOutput {
            content: lines.join("\n"),
            is_error: false,
        })
    }
}
