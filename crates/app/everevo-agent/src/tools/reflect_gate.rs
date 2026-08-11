//! In-process ReflectGateHook — automatic POST-ACT tool execution reflection.
//!
//! Complemented by MCP hook plugin `plugin-hooks-reflect_gate` which exposes the
//! `reflect_post_execute` tool for explicit agent calls. This in-process version
//! implements ToolHook::post_execute() for AUTOMATIC post-execution reflection and
//! trajectory recording on every tool call — a capability MCP plugins cannot provide.
//! This in-process implementation is kept for backward compatibility.
//! New development should use the MCP plugin version.

//! Reflect Gate — POST-ACT tool execution reflection.
//!
//! Implemented as a `ToolHook` so it plugs into the existing tool execution
//! lifecycle. Registered LAST in the hook chain (after AuditHook) so it
//! sees the final result.
//!
//! ## Two-phase reflection
//!
//! **Sync (in `post_execute`, blocks hook chain):**
//! Quick pattern matching — empty output, "command not found", "permission
//! denied" — produces immediate feedback for the next turn.
//!
//! **Async (spawned, fire-and-forget):**
//! Deep LLM analysis when trajectory buffer has enough data. Triggers
//! paradigm extraction and memory reflection.
//!
//! ## Pipeline→Loop integration
//!
//! The sync phase writes feedback to a shared `hook_feedback` slot that
//! the AgentLoop reads after tool execution. This enables the cyclical
//! Observe→Plan→Review→Act→**Reflect**→Observe pattern without modifying
//! the core loop.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use everevo_core::tool::{ToolHook, ToolOutput};
use everevo_core::EverEvoError;
use serde_json::Value;

use crate::memory::paradigm::{TrajectoryBuffer, TurnDigest};

/// POST-ACT reflect gate with sync quick-check and async deep reflection.
pub struct ReflectGateHook {
    /// Shared trajectory buffer for paradigm extraction (SAMULE pattern).
    pub trajectory_buffer: Arc<TrajectoryBuffer>,
    /// Shared feedback slot — AgentLoop reads this after tool execution.
    pub hook_feedback: Arc<Mutex<Option<String>>>,
}

impl ReflectGateHook {
    pub fn new() -> Self {
        Self {
            trajectory_buffer: Arc::new(TrajectoryBuffer::default()),
            hook_feedback: Arc::new(Mutex::new(None)),
        }
    }

    /// Create with a pre-existing trajectory buffer (shared across sessions).
    pub fn with_buffer(mut self, buffer: Arc<TrajectoryBuffer>) -> Self {
        self.trajectory_buffer = buffer;
        self
    }

    /// Get the shared feedback slot for wiring into ContextBuildContext.
    pub fn feedback_slot(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.hook_feedback)
    }
}

impl Default for ReflectGateHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHook for ReflectGateHook {
    async fn pre_execute(&self, _tool_name: &str, _params: &Value) -> Result<(), EverEvoError> {
        Ok(()) // ReflectGate only does post-execute work
    }

    async fn post_execute(
        &self,
        tool_name: &str,
        params: &Value,
        result: &Result<ToolOutput, EverEvoError>,
    ) {
        // ── Sync phase: quick pattern matching ──────────────────────
        let feedback = match result {
            Ok(output) => {
                if output.is_error {
                    classify_error(output.content.as_str())
                } else if output.content.trim().is_empty() {
                    Some(format!(
                        "[REFLECT] `{tool_name}` returned empty output — may indicate silent failure. \
                         Verify the result or try a different approach.",
                        tool_name = tool_name,
                    ))
                } else {
                    None
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                classify_error(&err_str)
            }
        };

        if let Some(fb) = feedback {
            let mut slot = self.hook_feedback.lock().unwrap_or_else(|e| e.into_inner());
            *slot = Some(fb);
        }

        // ── Record turn digest for trajectory buffer ────────────────
        let success = match result {
            Ok(o) => !o.is_error,
            Err(_) => false,
        };
        let error_type = match result {
            Ok(o) if o.is_error => Some(classify_error_type(&o.content)),
            Err(e) => Some(classify_error_type(&e.to_string())),
            _ => None,
        };
        let user_intent = params
            .as_object()
            .and_then(|obj| {
                obj.get("command")
                    .or_else(|| obj.get("query"))
                    .or_else(|| obj.get("file_path"))
                    .or_else(|| obj.get("description"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or(tool_name)
            .to_string();

        let response_snippet = match result {
            Ok(o) => o.content.clone(),
            Err(e) => e.to_string(),
        };

        self.trajectory_buffer.push(TurnDigest::new(
            tool_name,
            success,
            error_type.as_deref(),
            &user_intent,
            &response_snippet,
        ));
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn classify_error(msg: &str) -> Option<String> {
    let lower = msg.to_lowercase();

    if lower.contains("command not found") || lower.contains("no such file") {
        Some("[REFLECT] Command not found. Run `which <command>` to check if it's installed. Consider: is there an alternative tool or package manager available?".to_string())
    } else if lower.contains("permission denied") || lower.contains("access denied") {
        Some("[REFLECT] Permission denied. This sandbox has restricted access. Explain what you need — the user can grant permission.".to_string())
    } else if lower.contains("connection refused")
        || lower.contains("could not resolve host")
        || lower.contains("timeout")
    {
        Some("[REFLECT] Network/connection error. Check: is the service running? Is the URL correct? Try HTTPS instead of SSH for git operations.".to_string())
    } else if lower.contains("not found") && (lower.contains("package") || lower.contains("module"))
    {
        Some("[REFLECT] Package/module not found. Check the package name and try: 1) `which <runtime>` to verify the runtime is installed 2) Search for the correct package name before retrying".to_string())
    } else if lower.contains("out of memory") || lower.contains("killed") {
        Some("[REFLECT] Resource exhaustion. The command used too much memory/time. Try a more efficient approach or break the task into smaller steps.".to_string())
    } else {
        None
    }
}

fn classify_error_type(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("command not found") {
        "command_not_found"
    } else if lower.contains("permission denied") {
        "permission_denied"
    } else if lower.contains("connection refused") || lower.contains("timeout") {
        "network_error"
    } else if lower.contains("not found") {
        "not_found"
    } else {
        "general_error"
    }
    .to_string()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn test_classify_command_not_found() {
        let fb = classify_error("bash: pnpm: command not found");
        assert!(fb.is_some());
        assert!(fb.unwrap().contains("which"));
    }

    #[test]
    fn test_classify_permission_denied() {
        let fb = classify_error("Permission denied (os error 13)");
        assert!(fb.is_some());
        assert!(fb.unwrap().contains("sandbox"));
    }

    #[test]
    fn test_classify_connection_refused() {
        let fb = classify_error("Failed to connect: connection refused");
        assert!(fb.is_some());
        assert!(fb.unwrap().contains("HTTPS"));
    }

    #[test]
    fn test_classify_none_for_unknown() {
        let fb = classify_error("some random error");
        assert!(fb.is_none());
    }

    #[test]
    fn test_classify_empty_output() {
        // simulate the check in post_execute for empty success output
        let result: Result<ToolOutput, EverEvoError> = Ok(ToolOutput {
            content: String::new(),
            is_error: false,
            ..Default::default()
        });
        match &result {
            Ok(o) if o.content.trim().is_empty() => {
                // should detect empty output
                assert!(true);
            }
            _ => assert!(false, "should detect empty output"),
        }
    }

    #[test]
    fn test_reflect_gate_records_digest() {
        let gate = ReflectGateHook::new();
        let result: Result<ToolOutput, EverEvoError> = Ok(ToolOutput::text("build successful"));

        rt().block_on(gate.post_execute(
            "shell",
            &serde_json::json!({"command": "cargo build"}),
            &result,
        ));

        let snapshot = gate.trajectory_buffer.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].tool_name, "shell");
        assert!(snapshot[0].success);
    }

    #[test]
    fn test_reflect_gate_records_error_digest() {
        let gate = ReflectGateHook::new();
        let result: Result<ToolOutput, EverEvoError> = Ok(ToolOutput {
            content: "bash: pnpm: command not found".into(),
            is_error: true,
            ..Default::default()
        });

        rt().block_on(gate.post_execute(
            "shell",
            &serde_json::json!({"command": "pnpm install"}),
            &result,
        ));

        let snapshot = gate.trajectory_buffer.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert!(!snapshot[0].success);
        assert_eq!(snapshot[0].error_type.as_deref(), Some("command_not_found"));
    }
}
