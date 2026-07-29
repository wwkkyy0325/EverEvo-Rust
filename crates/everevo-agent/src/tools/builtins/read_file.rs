//! ReadFile built-in tool — reads file content from the workspace.
//!
//! Claude Code equivalent: `Read` tool. Reads a file with optional offset/limit,
//! returns content with line numbers. Scoped to workspace — refuses absolute
//! paths outside workspace.

use std::path::PathBuf;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

const MAX_LINES: usize = 2000;

/// Reads file content from the workspace.
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file from the workspace. Returns content with line numbers. \
         Use for inspecting source code, config files, or any text file. \
         For binary files, use the shell tool instead. \
         Parameters: path (required — relative to workspace), \
         offset (optional — start line, 1-based), \
         limit (optional — max lines, default: 2000, max: 2000)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace root"
                },
                "offset": {
                    "type": "integer",
                    "description": "Start line number (1-based, default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to read (default: 2000, max: 2000)"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low // read-only, only within workspace
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

        if !target.exists() {
            return Ok(ToolOutput {
                content: format!("File not found: {}", target.display()),
                is_error: true,
            });
        }
        if !target.is_file() {
            return Ok(ToolOutput {
                content: format!("Not a file: {}", target.display()),
                is_error: true,
            });
        }

        let content = match std::fs::read_to_string(&target) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Failed to read {}: {e}", target.display()),
                    is_error: true,
                });
            }
        };

        let offset = params["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = params["limit"]
            .as_u64()
            .unwrap_or(MAX_LINES as u64)
            .min(MAX_LINES as u64) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = (offset - 1).min(total);
        let end = (start + limit).min(total);
        let selected = &lines[start..end];

        if selected.is_empty() {
            return Ok(ToolOutput {
                content: format!(
                    "{}\n(empty selection — file has {total} lines, offset={offset})",
                    target.display()
                ),
                is_error: false,
            });
        }

        let numbered: Vec<String> = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{line}", start + i + 1))
            .collect();

        let header = format!(
            "{} (lines {}-{}/{total}):\n",
            target.display(),
            start + 1,
            start + selected.len(),
        );

        Ok(ToolOutput {
            content: header + &numbered.join("\n"),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_and_schema() {
        let tool = ReadFileTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "read_file");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "path");
    }
}
