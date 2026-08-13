//! PipelineTool — tool-callable context pipeline ("选择性复用管线部分" +
//! "自主组装管线"). The DEFAULT pipeline still auto-injects stages by priority;
//! this tool lets the agent additionally:
//! - `list_stages` — see the module library (SELF-DISCOVER-style).
//! - `run_stage {name}` — apply ONE stage's guidance on demand.
//! - `run_pipeline {stages:[..]}` — apply a SELECTED subset (not the whole).
//! - `compose {task}` — get a recommended stage sequence for the task.
//!
//! Main-loop only (like `ask_user` / `problem_model`). Moved from the server
//! crate during the P1.1 tool-ownership refactor.

use crate::stages::{compose_stages, stage_catalog};

/// Tool-callable pipeline.
pub struct PipelineTool;

#[async_trait::async_trait]
impl everevo_core::tool::Tool for PipelineTool {
    fn name(&self) -> &str {
        "pipeline"
    }
    fn description(&self) -> &str {
        "Inspect and invoke the agent's context pipeline as a tool. `list_stages` shows the \
         available reasoning stages; `run_stage <name>` applies one stage's guidance on demand; \
         `run_pipeline <stages>` applies a selected subset; `compose <task>` recommends a stage \
         sequence for the task. The default pipeline still runs automatically — this is for \
         selective reuse / self-assembly."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_stages", "run_stage", "run_pipeline", "compose"],
                    "description": "Pipeline operation."
                },
                "name": {"type": "string", "description": "Stage name for run_stage."},
                "stages": {"type": "array", "items": {"type": "string"}, "description": "Stage names for run_pipeline."},
                "task": {"type": "string", "description": "Task / question description for compose."}
            },
            "required": ["action"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel {
        everevo_core::types::RiskLevel::Low
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        let action = params["action"].as_str().unwrap_or("");
        let catalog = stage_catalog();
        let find = |name: &str| catalog.iter().find(|e| e.name == name);

        let output = match action {
            "list_stages" => {
                if catalog.is_empty() {
                    "No tool-callable stages registered.".to_string()
                } else {
                    let mut s = String::from("## Pipeline stages\n");
                    for e in &catalog {
                        s.push_str(&format!("- **{}**: {}\n", e.name, e.description));
                    }
                    s
                }
            }
            "run_stage" => {
                let name = params["name"].as_str().unwrap_or("");
                match find(name) {
                    Some(e) => format!("## {}\n{}", e.name, e.prompt),
                    None => {
                        return Err(everevo_core::EverEvoError::InvalidInput(format!(
                            "unknown stage `{name}` — use `list_stages` first"
                        )))
                    }
                }
            }
            "run_pipeline" => {
                let names: Vec<&str> = params["stages"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if names.is_empty() {
                    return Err(everevo_core::EverEvoError::InvalidInput(
                        "run_pipeline requires `stages` (non-empty array)".into(),
                    ));
                }
                let mut s = String::from("## Pipeline (selected stages)\n");
                for name in &names {
                    match find(name) {
                        Some(e) => {
                            s.push_str(&format!("\n### {}\n{}\n", e.name, e.prompt));
                        }
                        None => {
                            return Err(everevo_core::EverEvoError::InvalidInput(format!(
                                "unknown stage `{name}` — use `list_stages` first"
                            )))
                        }
                    }
                }
                s
            }
            "compose" => {
                let task = params["task"].as_str().unwrap_or("");
                if task.trim().is_empty() {
                    return Err(everevo_core::EverEvoError::InvalidInput(
                        "compose requires `task`".into(),
                    ));
                }
                let seq = compose_stages(task);
                let mut s = format!(
                    "## Recommended pipeline for the task\nOrdered: {}\n",
                    seq.join(" → ")
                );
                for name in &seq {
                    if let Some(e) = find(name) {
                        s.push_str(&format!("\n### {}\n{}\n", e.name, e.prompt));
                    }
                }
                s
            }
            other => {
                return Err(everevo_core::EverEvoError::InvalidInput(format!(
                    "unknown action `{other}` — expected list_stages|run_stage|run_pipeline|compose"
                )))
            }
        };

        Ok(everevo_core::tool::ToolOutput::text(output))
    }
}
