//! SandboxedShellTool — the per-session wrapper that routes commands into
//! the sandbox with confirmation-gate support.
//!
//! Extracted from `chat.rs` to keep the route handler focused on SSE streaming.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::app_state::{ConfirmationNotification, PendingConfirmation};

/// Wraps a sandbox to force all commands into the session work directory.
/// Also handles the confirmation flow: when the sandbox requires user
/// confirmation, this tool blocks on a oneshot channel until the user
/// responds via the `/api/sandbox/confirm` endpoint.
///
/// When `auto_confirm` is true (sub-agent inheriting FullyAuto parent):
/// commands execute with `confirmed: true` immediately, bypassing the
/// confirmation gate. Admin commands fail-fast instead of deadlocking.
pub struct SandboxedShellTool {
    pub inner: Arc<dyn everevo_core::sandbox::SandboxProvider>,
    pub work_dir: std::path::PathBuf,
    pub session_id: Uuid,
    /// Shared pending confirmations map — the confirm endpoint resolves these.
    pub confirmations: Arc<RwLock<HashMap<Uuid, PendingConfirmation>>>,
    /// Channel to notify the SSE stream about a pending confirmation.
    pub notif_tx: mpsc::UnboundedSender<ConfirmationNotification>,
    /// When true, bypass the confirmation gate entirely.
    pub auto_confirm: bool,
}

#[async_trait::async_trait]
impl everevo_core::tool::Tool for SandboxedShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Execute a shell command in an isolated sandbox. Use RELATIVE paths (e.g., ./file.txt)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command. Use relative paths." },
                "timeout_secs": { "type": "integer", "description": "Timeout (default: 30, max: 300)", "default": 30 }
            },
            "required": ["command"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel {
        everevo_core::types::RiskLevel::Medium
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        let command = params["command"].as_str().ok_or_else(|| {
            everevo_core::EverEvoError::InvalidInput("command is required".into())
        })?;
        let timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30).min(300);

        let confirmed = self.auto_confirm
            || params
                .get("confirmed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

        let config = everevo_core::sandbox::ExecutionConfig::new(command)
            .with_timeout(timeout_secs)
            .with_working_dir(self.work_dir.clone())
            .with_confirmed(confirmed);
        let mut result = self.inner.execute(&config).await?;

        // Confirmation gate (Claude Code style)
        if result.needs_confirmation {
            if self.auto_confirm {
                tracing::warn!(session_id = %self.session_id, command = %command, reason = %result.confirmation_reason, "Sub-agent admin command blocked (auto_confirm)");
                return Ok(everevo_core::tool::ToolOutput {
                    content: format!(
                        "Command requires admin privileges: {}. Use a non-admin alternative.",
                        result.confirmation_reason
                    ),
                    is_error: true,
                });
            }
            let reason = result.confirmation_reason.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.confirmations.write().await.insert(
                self.session_id,
                PendingConfirmation {
                    command: command.to_string(),
                    reason: reason.clone(),
                    response_tx: tx,
                },
            );
            let _ = self.notif_tx.send(ConfirmationNotification {
                session_id: self.session_id,
                command: command.to_string(),
                reason: reason.clone(),
            });
            tracing::info!(session_id = %self.session_id, command = %command, %reason, "Waiting for user confirmation...");
            let approved = rx.await.unwrap_or(false);
            self.confirmations.write().await.remove(&self.session_id);
            if !approved {
                return Ok(everevo_core::tool::ToolOutput {
                    content: format!("User denied execution: {reason}"),
                    is_error: true,
                });
            }
            let config = everevo_core::sandbox::ExecutionConfig::new(command)
                .with_timeout(timeout_secs)
                .with_working_dir(self.work_dir.clone())
                .with_confirmed(true);
            result = self.inner.execute(&config).await?;
        }

        let content = if result.stdout.is_empty() {
            result.stderr.clone()
        } else {
            result.stdout.clone()
        };
        let is_error = result.exit_code != 0 || result.killed_by_timeout;
        if result.killed_by_timeout {
            return Ok(everevo_core::tool::ToolOutput {
                content: format!("Timeout after {timeout_secs}s"),
                is_error: true,
            });
        }
        Ok(everevo_core::tool::ToolOutput {
            content,
            is_error: is_error || result.exit_code == 126,
        })
    }
}
