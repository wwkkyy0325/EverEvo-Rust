//! WriteFile built-in tool — creates or overwrites a file in the workspace.
//!
//! Claude Code equivalent: `Write` tool. Creates parent directories automatically.
//! Scoped to workspace — refuses absolute paths outside workspace.

use std::path::PathBuf;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Creates or overwrites a file in the workspace.
pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file in the workspace. Creates parent directories \
         automatically. Use for writing code, config files, or any text content. \
         Parameters: path (required — relative to workspace), \
         content (required — the text to write)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace root"
                },
                "content": {
                    "type": "string",
                    "description": "Text content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium // writes files to disk
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let rel_path = params["path"].as_str().unwrap_or("");
        if rel_path.is_empty() {
            return Ok(ToolOutput { content: "path is required".into(), is_error: true });
        }
        let content = params["content"].as_str().unwrap_or("");
        if content.is_empty() && !params["content"].is_string() {
            return Ok(ToolOutput { content: "content is required".into(), is_error: true });
        }

        // Resolve path relative to workspace; reject absolute paths
        let target = if std::path::Path::new(rel_path).is_absolute() {
            return Ok(ToolOutput {
                content: format!(
                    "Absolute paths are not allowed. Use a path relative to workspace: {}",
                    self.workspace_root.display()
                ),
                is_error: true,
            });
        } else {
            self.workspace_root.join(rel_path.trim_start_matches('/').trim_start_matches('\\'))
        };

        // Create parent directories
        if let Some(parent) = target.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EverEvoError::Sandbox(format!("Failed to create parent dirs: {e}"))
                })?;
            }
        }

        std::fs::write(&target, content).map_err(|e| {
            EverEvoError::Sandbox(format!("Failed to write {}: {e}", target.display()))
        })?;

        let size = content.len();
        let lines = content.lines().count();
        Ok(ToolOutput {
            content: format!(
                "Wrote {} ({} bytes, {lines} lines).",
                target.display(),
                size,
            ),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_and_schema() {
        let tool = WriteFileTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "write_file");
        assert_eq!(tool.risk_level(), RiskLevel::Medium);
        let schema = tool.parameters_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"path"));
        assert!(required.contains(&"content"));
    }
}
