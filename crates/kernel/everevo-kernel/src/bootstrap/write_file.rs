//! Bootstrap WriteFile — kernel-built file writing tool.
//!
//! Enforces kernel self-protection: refuses to write to kernel source,
//! kernel binaries, workspace config, or migrations.
//! Resolves relative paths against a configurable `work_dir` (the sandbox
//! session directory).

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::protection;

#[derive(Default)]
pub struct BootstrapWriteFile {
    work_dir: Option<PathBuf>,
}

impl BootstrapWriteFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base directory for resolving relative paths.
    pub fn with_work_dir(mut self, dir: PathBuf) -> Self {
        self.work_dir = Some(dir);
        self
    }

    fn resolve_path(&self, path: &str) -> String {
        let p = Path::new(path);
        if p.is_absolute() {
            path.to_string()
        } else if let Some(ref wd) = self.work_dir {
            wd.join(path).to_string_lossy().to_string()
        } else {
            path.to_string()
        }
    }
}

#[async_trait]
impl Tool for BootstrapWriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write content to a file. Kernel-built — always available. \
         Blocked from writing to kernel source, binaries, or config. \
         Relative paths resolve against the sandbox working directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to write (kernel paths blocked, relative OK)"},
                "content": {"type": "string", "description": "Content to write"}
            },
            "required": ["path", "content"]
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
        let path = params["path"].as_str().ok_or_else(|| EverEvoError::Tool {
            tool: "write_file".into(),
            message: "Missing 'path' parameter".into(),
        })?;
        let content = params["content"]
            .as_str()
            .ok_or_else(|| EverEvoError::Tool {
                tool: "write_file".into(),
                message: "Missing 'content' parameter".into(),
            })?;

        let resolved = self.resolve_path(path);

        // ── Kernel self-protection chokepoint ─────────────────────────
        // Check both the original and resolved paths against kernel protection
        if protection::is_kernel_protected(path) || protection::is_kernel_protected(&resolved) {
            return Ok(ToolOutput {
                content: format!(
                    "BLOCKED: '{resolved}' is in a kernel-protected area.\n\
                     Kernel source, binaries, and workspace config are immutable.\n\
                     Use plugin_dev(action='edit', ...) to modify plugin code instead."
                ),
                is_error: true,
                ..Default::default()
            });
        }

        // Create parent dirs if needed
        if let Some(parent) = Path::new(&resolved).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match std::fs::write(&resolved, content) {
            Ok(()) => {
                let msg = if resolved != path {
                    format!(
                        "[write_file resolved '{path}' → '{resolved}']\nWrote {} bytes",
                        content.len()
                    )
                } else {
                    format!("Wrote {} bytes to '{resolved}'", content.len())
                };
                Ok(ToolOutput::text(msg))
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to write '{resolved}': {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
