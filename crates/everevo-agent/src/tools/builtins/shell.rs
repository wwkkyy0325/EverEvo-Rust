//! Shell execution tool — runs commands via the sandbox.
//!
//! Uses `Arc<dyn SandboxProvider>` for dependency injection — testable with mock sandbox.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::sandbox::{ExecutionConfig, SandboxProvider};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

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
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in a sandboxed environment. \
         On Windows, uses Git Bash → PowerShell → CMD (WSL is opt-in). \
         Commands have a 30-second default timeout (max 300s) and run in an isolated workspace."
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

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        // Check cancellation before spawning
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Ok(ToolOutput {
                content: "cancelled".into(),
                is_error: true,
            });
        }

        let command = params["command"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("command is required".into()))?;

        let timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30).min(300);
        let working_dir = params["working_dir"].as_str().map(std::path::PathBuf::from);

        let mut config = ExecutionConfig::new(command).with_timeout(timeout_secs);
        if let Some(dir) = working_dir {
            config = config.with_working_dir(dir);
        }

        // Permission gate — check before execution
        let result = self.sandbox.execute(&config).await?;

        // ── Confirmation gate ──────────────────────────────────────
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

        // Merge stdout and stderr — the LLM needs to see BOTH.
        // Many tools (cargo, npm, git) write diagnostics to stderr while
        // producing normal output on stdout; dropping stderr hides warnings.
        let mut content = String::new();
        if !result.stdout.is_empty() {
            content.push_str(&result.stdout);
        }
        if !result.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("--- stderr ---\n");
            content.push_str(&result.stderr);
        }

        // ── Exit code classification ──────────────────────────────
        if result.killed_by_timeout {
            return Ok(ToolOutput {
                content: format!("Timeout after {timeout_secs}s\n\n{content}"),
                is_error: true,
            });
        }
        match result.exit_code {
            0 => Ok(ToolOutput {
                content,
                is_error: false,
            }),
            126 => Ok(ToolOutput {
                // Permission denied by sandbox
                content: format!("Permission denied (exit 126)\n\n{content}"),
                is_error: true,
            }),
            127 => Ok(ToolOutput {
                // Command not found
                content: format!("Command not found (exit 127)\n\n{content}"),
                is_error: true,
            }),
            _ => Ok(ToolOutput {
                content: format!("Exit code {}\n\n{content}", result.exit_code),
                is_error: true,
            }),
        }
    }
}
