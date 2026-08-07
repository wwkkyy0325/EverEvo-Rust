//! Bootstrap ReadFile — kernel-built file reading tool.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

pub struct BootstrapReadFile;

#[async_trait]
impl Tool for BootstrapReadFile {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str {
        "Read a file from the filesystem. Kernel-built — always available."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to read"}
            },
            "required": ["path"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let path = params["path"].as_str().ok_or_else(|| EverEvoError::Tool {
            tool: "read_file".into(),
            message: "Missing 'path' parameter".into(),
        })?;
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(ToolOutput::text(content)),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to read '{path}': {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
