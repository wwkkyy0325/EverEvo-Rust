//! WorkflowRunner tool — lets the LLM execute predefined or ad-hoc workflows.
//!
//! Accepts a JSON workflow definition and executes it via the workflow engine.
//! Supports real callbacks (shell/fetch/memory/agent) when wired in the server
//! orchestration layer; falls back to noop for testing.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use everevo_workflow::WorkflowCallbacks;

pub struct WorkflowRunnerTool {
    callbacks: Option<Arc<dyn WorkflowCallbacks>>,
    /// Directory of saved workflow definitions (`data/workflows/`). When set,
    /// the LLM can run a workflow by `name` instead of hand-authoring JSON.
    workflows_dir: Option<PathBuf>,
}

impl WorkflowRunnerTool {
    pub fn new() -> Self {
        Self {
            callbacks: None,
            workflows_dir: None,
        }
    }

    /// Wire real callbacks for production use (shell, fetch, memory, agent).
    pub fn with_callbacks(mut self, cb: Arc<dyn WorkflowCallbacks>) -> Self {
        self.callbacks = Some(cb);
        self
    }

    /// Point at a library directory so workflows can be run by name.
    pub fn with_workflows_dir(mut self, dir: PathBuf) -> Self {
        self.workflows_dir = Some(dir);
        self
    }

    /// Load a workflow definition by name from `workflows_dir`.
    /// Name is sanitized to alphanumerics + `-_.` to prevent path traversal.
    pub fn load_named(
        &self,
        name: &str,
    ) -> Result<everevo_workflow::WorkflowDefinition, EverEvoError> {
        let dir = self
            .workflows_dir
            .as_ref()
            .ok_or_else(|| EverEvoError::InvalidInput("no workflows_dir configured".into()))?;
        let safe: String = name
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe.is_empty() {
            return Err(EverEvoError::InvalidInput(format!(
                "invalid workflow name: {name}"
            )));
        }
        let path = dir.join(format!("{safe}.json"));
        let content = std::fs::read_to_string(&path)
            .map_err(|_| EverEvoError::NotFound(format!("workflow '{name}' not found")))?;
        serde_json::from_str(&content)
            .map_err(|e| EverEvoError::InvalidInput(format!("workflow '{name}' invalid JSON: {e}")))
    }

    /// Save a workflow definition to the library as `<name>.json` (the write
    /// counterpart of `load_named`). Name is sanitized to alphanumerics + `-_.`.
    pub fn save_workflow(
        &self,
        name: &str,
        def: &everevo_workflow::WorkflowDefinition,
    ) -> Result<std::path::PathBuf, EverEvoError> {
        let dir = self
            .workflows_dir
            .as_ref()
            .ok_or_else(|| EverEvoError::InvalidInput("no workflows_dir configured".into()))?;
        let safe: String = name
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe.is_empty() {
            return Err(EverEvoError::InvalidInput(format!(
                "invalid workflow name: {name}"
            )));
        }
        std::fs::create_dir_all(dir).ok();
        let path = dir.join(format!("{safe}.json"));
        let json = serde_json::to_string_pretty(def)
            .map_err(|e| EverEvoError::Internal(format!("serialize workflow: {e}")))?;
        std::fs::write(&path, json)
            .map_err(|e| EverEvoError::Internal(format!("write workflow: {e}")))?;
        tracing::info!(name = %safe, path = %path.display(), "Workflow saved to library");
        Ok(path)
    }

    /// List saved workflows in the library dir: (name, description) pairs.
    pub fn list_saved(&self) -> Result<Vec<(String, String)>, EverEvoError> {
        Ok(self
            .workflows_dir
            .as_deref()
            .map(scan_workflow_library)
            .unwrap_or_default())
    }
}

/// Scan a workflow library directory for `(name, description)` pairs.
pub fn scan_workflow_library(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let desc = std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|v| {
                v.get("description")
                    .and_then(|d| d.as_str())
                    .map(String::from)
            })
            .unwrap_or_default();
        if !name.is_empty() {
            out.push((name, desc));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Tool to list saved workflows in the library, so the LLM can discover
/// reusable procedures by name and invoke them via `workflow_run(name=...)`.
pub struct ListWorkflowsTool {
    workflows_dir: Option<PathBuf>,
}

impl ListWorkflowsTool {
    pub fn new(workflows_dir: PathBuf) -> Self {
        Self {
            workflows_dir: Some(workflows_dir),
        }
    }
}

#[async_trait]
impl Tool for ListWorkflowsTool {
    fn name(&self) -> &str {
        "list_workflows"
    }
    fn description(&self) -> &str {
        "List saved reusable workflows in the library (data/workflows/). \
         Returns name + description for each. Use this to discover workflows \
         you can run by name via the workflow_run tool — prefer reusing a saved \
         workflow over hand-writing the JSON when one fits."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let entries = self
            .workflows_dir
            .as_deref()
            .map(scan_workflow_library)
            .unwrap_or_default();
        if entries.is_empty() {
            return Ok(ToolOutput {
                content: "No saved workflows in the library yet. You can author one inline via workflow_run, or drop a <name>.json into data/workflows/.".into(),
                is_error: false,
                ..Default::default()
            });
        }
        let lines = entries
            .iter()
            .map(|(name, desc)| {
                if desc.is_empty() {
                    format!("- {name}")
                } else {
                    format!("- {name} — {desc}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content: format!("Saved workflows (run with workflow_run name=<name>):\n{lines}"),
            is_error: false,
            ..Default::default()
        })
    }
}

/// Tool for the LLM to save a workflow to the library, sedimenting a
/// repeatable multi-step procedure so it can be reused by name later.
pub struct SaveWorkflowTool {
    workflows_dir: PathBuf,
}

impl SaveWorkflowTool {
    pub fn new(workflows_dir: PathBuf) -> Self {
        Self { workflows_dir }
    }
}

#[async_trait]
impl Tool for SaveWorkflowTool {
    fn name(&self) -> &str {
        "save_workflow"
    }
    fn description(&self) -> &str {
        "Save a reusable workflow to the library (data/workflows/<name>.json) so it \
         can be run later by name via workflow_run. Use to sediment a repeatable \
         multi-step procedure you've worked out, so future sessions skip the \
         discovery. Parameters: name (kebab-case), workflow (JSON definition)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "kebab-case workflow name"},
                "workflow": {"type": "object", "description": "JSON workflow definition (name, steps[])"}
            },
            "required": ["name", "workflow"]
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
        let name = params["name"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("name is required".into()))?;
        let def: everevo_workflow::WorkflowDefinition =
            serde_json::from_value(params["workflow"].clone())
                .map_err(|e| EverEvoError::InvalidInput(format!("Invalid workflow JSON: {e}")))?;
        // Reuse WorkflowRunnerTool's save logic (path-traversal-safe write).
        let safe: String = name
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe.is_empty() {
            return Err(EverEvoError::InvalidInput(format!(
                "invalid workflow name: {name}"
            )));
        }
        let is_reused = self.workflows_dir.join(format!("{safe}.json")).exists();
        let runner = WorkflowRunnerTool::new().with_workflows_dir(self.workflows_dir.clone());
        let path = runner.save_workflow(name, &def)?;
        let reuse_hint = if is_reused {
            " — overwritten an existing workflow (reused! if used 3+ times, recommend promote_to_skill)"
        } else {
            ""
        };
        Ok(ToolOutput {
            content: format!(
                "Saved workflow '{name}' → {}. Run it later with workflow_run name={name}.{reuse_hint}",
                path.display()
            ),
            is_error: false,
            ..Default::default()
        })
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
        "Execute a multi-step automation workflow. Pass `name` to run a SAVED \
         workflow from the library (discover names with list_workflows), or \
         pass `workflow` to run an inline JSON definition. Steps can include: \
         shell, fetch, memory_save/memory_search, agent, delay, log, set_variable, \
         condition (branching). Step outputs are reusable via ${{step_id.key}}. \
         Prefer saved workflows (by name) for repeatable multi-step procedures."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of a saved workflow in data/workflows/ (without .json). Use list_workflows to discover."
                },
                "workflow": {
                    "type": "object",
                    "description": "Inline JSON workflow definition (use instead of `name` for ad-hoc workflows)",
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
            }
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
        // Resolve the workflow: by name (from the library dir) or inline JSON.
        let def: everevo_workflow::WorkflowDefinition = if let Some(name) = params["name"].as_str()
        {
            self.load_named(name)?
        } else {
            serde_json::from_value(params["workflow"].clone())
                .map_err(|e| EverEvoError::InvalidInput(format!("Invalid workflow JSON: {e}")))?
        };

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
            result
                .step_results
                .iter()
                .map(|r| {
                    let status = if r.success { "OK" } else { "FAIL" };
                    format!(
                        "  [{}] {} — {}",
                        status,
                        r.step_id,
                        truncate(&r.output, 200)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );

        Ok(ToolOutput {
            content: summary,
            is_error: !result.success,
            ..Default::default()
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Char-safe: `&s[..max]` panics on multi-byte UTF-8 straddling the
        // boundary (server-crash risk). Take chars instead.
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
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

    #[test]
    fn test_scan_workflow_library() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("alpha.json"),
            r#"{"name":"alpha","description":"first","steps":[]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("beta.json"),
            r#"{"name":"beta","description":"second","steps":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("ignore.txt"), "x").unwrap();
        let list = scan_workflow_library(dir.path());
        assert_eq!(list.len(), 2, "only .json files");
        assert_eq!(list[0], ("alpha".into(), "first".into()));
        assert_eq!(list[1], ("beta".into(), "second".into()));
    }

    #[test]
    fn test_load_named_succeeds() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("demo.json"),
            r#"{"name":"demo","steps":[{"id":"s1","type":"log","params":{"msg":"hi"}}]}"#,
        )
        .unwrap();
        let tool = WorkflowRunnerTool::new().with_workflows_dir(dir.path().to_path_buf());
        let def = tool.load_named("demo").unwrap();
        assert_eq!(def.name, "demo");
    }

    #[test]
    fn test_load_named_rejects_traversal() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = WorkflowRunnerTool::new().with_workflows_dir(dir.path().to_path_buf());
        // All path separators / dots are stripped -> empty safe name -> error.
        assert!(tool.load_named("../../../etc/passwd").is_err());
    }

    #[test]
    fn test_list_workflows_tool_empty_message() {
        let dir = tempfile::TempDir::new().unwrap();
        let _tool = ListWorkflowsTool::new(dir.path().to_path_buf());
        // execute is async — just verify list logic via scan directly.
        assert!(scan_workflow_library(dir.path()).is_empty());
    }

    #[test]
    fn test_save_workflow_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let runner = WorkflowRunnerTool::new().with_workflows_dir(dir.path().to_path_buf());
        let def: everevo_workflow::WorkflowDefinition = serde_json::from_str(
            r#"{"name":"my-flow","description":"test","steps":[{"id":"s1","type":"log","params":{"msg":"hi"}}]}"#,
        )
        .unwrap();
        let path = runner.save_workflow("my-flow", &def).unwrap();
        assert!(path.exists(), "workflow file should exist");
        // discover + reload
        let listed = scan_workflow_library(dir.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "my-flow");
        let loaded = runner.load_named("my-flow").unwrap();
        assert_eq!(loaded.name, "my-flow");
        assert_eq!(loaded.steps.len(), 1);
    }

    #[test]
    fn test_save_workflow_sanitizes_traversal() {
        let dir = tempfile::TempDir::new().unwrap();
        let runner = WorkflowRunnerTool::new().with_workflows_dir(dir.path().to_path_buf());
        let def: everevo_workflow::WorkflowDefinition =
            serde_json::from_str(r#"{"name":"x","description":"","steps":[]}"#).unwrap();
        // Path separators are stripped -> sanitized to a flat file directly in dir.
        let path = runner.save_workflow("../../etc/pwned", &def).unwrap();
        assert_eq!(
            path.parent(),
            Some(dir.path()),
            "must be a flat file in the library dir (no traversal)"
        );
    }
}
