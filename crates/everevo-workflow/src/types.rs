//! Workflow definition types — JSON-serializable workflow DSL.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    /// Human-readable name.
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub description: String,
    /// Execution steps (ordered).
    pub steps: Vec<Step>,
    /// Shared variables accessible across steps via `${{step_name.output_key}}`.
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Stop on first error (default: true).
    #[serde(default = "default_true")]
    pub stop_on_error: bool,
    /// Max total execution time in seconds (default: 300).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    300
}

/// A single workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Unique step identifier (used for variable references).
    pub id: String,
    /// Step type.
    #[serde(rename = "type")]
    pub step_type: StepType,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Type-specific parameters.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Condition for conditional execution (JS-like expression evaluated against variables).
    #[serde(default)]
    pub condition: Option<String>,
    /// Max retries on failure (default: 0).
    #[serde(default)]
    pub retry: u32,
    /// Timeout for this step in seconds (default: 60).
    #[serde(default = "default_step_timeout")]
    pub timeout_secs: u64,
}

fn default_step_timeout() -> u64 {
    60
}

/// Step type determines what the engine executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    /// Execute a shell command. Params: { command: string, working_dir?: string }
    Shell,
    /// Fetch a URL. Params: { url: string }
    Fetch,
    /// Save to persistent memory. Params: { key: string, content: string }
    MemorySave,
    /// Search persistent memory. Params: { query: string }
    MemorySearch,
    /// Run a sub-agent with a prompt. Params: { prompt: string, max_turns?: number }
    Agent,
    /// Conditional branch. Params: { if: { ...steps }, else?: { ...steps } }
    Condition,
    /// Wait N seconds. Params: { seconds: number }
    Delay,
    /// Emit a log message. Params: { message: string, level?: "info"|"warn"|"error" }
    Log,
    /// Set a variable for later steps. Params: { key: string, value: string }
    SetVariable,
}

/// Result of executing a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub success: bool,
    pub output: String,
    /// Variables exported by this step (accessible as `${{step_id.key}}`).
    #[serde(default)]
    pub exports: HashMap<String, String>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Error message if failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Number of retries used.
    #[serde(default)]
    pub retries: u32,
}

/// Result of executing an entire workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub workflow_name: String,
    pub success: bool,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub step_results: Vec<StepResult>,
    pub total_duration_ms: u64,
    pub final_variables: HashMap<String, String>,
}
