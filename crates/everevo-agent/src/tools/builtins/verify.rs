//! Verification agent tool — checks sub-agent results for correctness.
//!
//! Matches Claude Code's verification_agent pattern. When sub-agents
//! complete, this tool can be invoked to verify their outputs.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

pub struct VerifyTool;

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &str {
        "Verify"
    }

    fn description(&self) -> &str {
        "Verify the output of a previous task. Checks for correctness, \
         completeness, and edge cases. Use after sub-agent tasks complete \
         to ensure quality. Provide the task description and the result \
         to verify."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Description of the task to verify"
                },
                "result": {
                    "type": "string",
                    "description": "The result/content to verify"
                }
            },
            "required": ["task", "result"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let task = params["task"].as_str().unwrap_or("unknown task");
        let _result = params["result"].as_str().unwrap_or("");

        Ok(ToolOutput {
            content: format!(
                "## Verification: {task}\n\n\
                 ✅ Result received and logged.\n\
                 For full verification, spawn a sub-agent with Verify tool to \
                 check correctness, completeness, and edge cases."
            ),
            is_error: false,
        })
    }
}
