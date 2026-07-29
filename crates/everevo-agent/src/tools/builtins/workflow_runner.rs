//! WorkflowRunner tool — lets the LLM execute predefined or ad-hoc workflows.
//!
//! Accepts a JSON workflow definition and executes it via the workflow engine.
//! Supports real callbacks (shell/fetch/memory/agent) when wired in the server
//! orchestration layer; falls back to noop for testing.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use everevo_workflow::WorkflowCallbacks;

pub struct WorkflowRunnerTool {
    callbacks: Option<Arc<dyn WorkflowCallbacks>>,
}

impl WorkflowRunnerTool {
    pub fn new() -> Self {
        Self { callbacks: None }
    }

    /// Wire real callbacks for production use (shell, fetch, memory, agent).
    pub fn with_callbacks(mut self, cb: Arc<dyn WorkflowCallbacks>) -> Self {
        self.callbacks = Some(cb);
        self
    }
}

impl Default for WorkflowRunnerTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WorkflowRunnerTool {
    fn name(&self) -> &str {
        "workflow_run"
    }

    fn description(&self) -> &str {
        "Execute a multi-step automation workflow defined as JSON. \
         Steps can include: shell (run commands), fetch (get URLs), \
         memory_save/memory_search (persistent memory), agent (sub-agent), \
         delay (wait), log (emit messages), set_variable, and condition (branching). \
         Each step's output is available to later steps via ${{step_id.key}} references. \
         Parameters: workflow (the JSON workflow definition)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workflow": {
                    "type": "object",
                    "description": "JSON workflow definition with name, steps array, and optional variables",
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "type": {
                                        "type": "string",
                                        "enum": ["shell", "fetch", "memory_save", "memory_search",
                                                 "agent", "delay", "log", "set_variable", "condition"]
                                    },
                                    "description": { "type": "string" },
                                    "params": { "type": "object" }
                                },
                                "required": ["id", "type"]
                            }
                        }
                    },
                    "required": ["name", "steps"]
                }
            },
            "required": ["workflow"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let workflow_json = &params["workflow"];
        let def: everevo_workflow::WorkflowDefinition =
            serde_json::from_value(workflow_json.clone())
                .map_err(|e| EverEvoError::InvalidInput(format!("Invalid workflow JSON: {e}")))?;

        let result = if let Some(ref cb) = self.callbacks {
            let engine = everevo_workflow::WorkflowEngine::with_callbacks(Arc::clone(cb));
            engine.execute(&def).await
        } else {
            let engine = everevo_workflow::WorkflowEngine::new_noop();
            engine.execute(&def).await
        };

        let summary = format!(
            "Workflow '{}': {} steps completed, {} failed ({})\n\n{}",
            result.workflow_name,
            result.steps_completed,
            result.steps_failed,
            if result.success { "success" } else { "FAILED" },
            result.step_results.iter().map(|r| {
                let status = if r.success { "OK" } else { "FAIL" };
                format!("  [{}] {} — {}", status, r.step_id, truncate(&r.output, 200))
            }).collect::<Vec<_>>().join("\n"),
        );

        Ok(ToolOutput {
            content: summary,
            is_error: !result.success,
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_and_schema() {
        let tool = WorkflowRunnerTool::new();
        assert_eq!(tool.name(), "workflow_run");
        assert_eq!(tool.risk_level(), RiskLevel::Medium);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["workflow"].is_object());
    }

    #[test]
    fn test_with_callbacks() {
        use everevo_workflow::NoopCallbacks;
        let cb = Arc::new(NoopCallbacks);
        let tool = WorkflowRunnerTool::new().with_callbacks(cb);
        assert!(tool.callbacks.is_some());
    }
}
