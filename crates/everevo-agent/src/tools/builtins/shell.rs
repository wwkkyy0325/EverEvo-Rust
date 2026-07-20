//! Shell execution tool — runs commands via the sandbox.
//!
//! Uses `Arc<dyn SandboxProvider>` for dependency injection — testable with mock sandbox.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::sandbox::{ExecutionConfig, SandboxProvider};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;

/// Execute shell commands via the sandbox.
pub struct ShellTool {
    sandbox: Arc<dyn SandboxProvider>,
}

impl ShellTool {
    pub fn new(sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str { "shell" }

    fn description(&self) -> &str {
        "Execute a shell command in a sandboxed environment. \
         On Windows, uses WSL → Git Bash → PowerShell → CMD. \
         Commands have a 30-second timeout and run in an isolated workspace."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default: 30, max: 300)", "default": 30 },
                "working_dir": { "type": "string", "description": "Working directory (default: sandbox temp dir)" }
            },
            "required": ["command"]
        })
    }

    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let command = params["command"].as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("command is required".into()))?;

        let timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30).min(300);
        let working_dir = params["working_dir"].as_str().map(|s| std::path::PathBuf::from(s));

        let mut config = ExecutionConfig::new(command)
            .with_timeout(timeout_secs);
        if let Some(dir) = working_dir {
            config = config.with_working_dir(dir);
        }

        // Permission gate — check before execution
        // The sandbox internally calls check_permission() in execute(),
        // which returns Deny/Confirm/Allow. At SemiAuto, dangerous commands
        // and external paths trigger a Confirm decision. In the current
        // implementation, Confirm proceeds (the UI confirmation hook is
        // the future integration point).
        let result = self.sandbox.execute(&config).await?;

        // ── Confirmation gate ──────────────────────────────────────
        // If the sandbox requires user confirmation, return the reason so
        // the caller can present a dialog. The caller should re-invoke with
        // `confirmed: true` after the user approves.
        if result.needs_confirmation {
            let reason = &result.confirmation_reason;
            return Ok(ToolOutput {
                content: format!(
                    "⚠️ 此命令需要你的确认才能执行。\n\n命令: {command}\n原因: {reason}\n\n\
                     请回复「确认执行」以继续，或「拒绝」以取消。\n\
                     (ConfirmRequired: use `confirmed: true` to proceed)",
                ),
                is_error: false,
            });
        }

        let content = if result.stdout.is_empty() { result.stderr.clone() } else { result.stdout.clone() };
        let is_error = result.exit_code != 0 || result.killed_by_timeout;
        if result.killed_by_timeout {
            return Ok(ToolOutput { content: format!("Timeout after {timeout_secs}s"), is_error: true });
        }
        if result.exit_code == 126 {
            return Ok(ToolOutput { content, is_error: true }); // blocked by permission
        }

        Ok(ToolOutput { content, is_error })
    }
}
