//! Bootstrap Shell — kernel-built shell tool for self-repair.
//!
//! Always available. Executes commands on the host system for
//! plugin compilation, git operations, and filesystem manipulation.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

pub struct BootstrapShell;

#[async_trait]
#[allow(clippy::disallowed_methods)] // kernel privilege: direct process execution for self-repair
impl Tool for BootstrapShell {
    fn name(&self) -> &str { "shell" }

    fn description(&self) -> &str {
        "Execute a shell command on the host system. Used for plugin compilation \
         (cargo build), git operations, and filesystem tasks. This is a kernel-built \
         tool that cannot be removed or overridden by plugins."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory (optional)"
                }
            },
            "required": ["command"]
        })
    }

    fn risk_level(&self) -> RiskLevel { RiskLevel::High }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let command = params["command"]
            .as_str()
            .ok_or_else(|| EverEvoError::Tool {
                tool: "shell".into(),
                message: "Missing 'command' parameter".into(),
            })?;
        let working_dir = params["working_dir"].as_str();

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", command]);
            c
        };

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let content = if stderr.is_empty() {
                    stdout.to_string()
                } else {
                    format!("{stdout}\n{stderr}")
                };
                Ok(ToolOutput {
                    content,
                    is_error: !output.status.success(),
                    ..Default::default()
                })
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to execute command: {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
