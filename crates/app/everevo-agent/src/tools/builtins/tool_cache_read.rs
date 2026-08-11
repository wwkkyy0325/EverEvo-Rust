//! `tool_cache_read` — re-read a paged tool output from disk.
//!
//! When a tool output is too large to keep in context (spec deliverable 6), the
//! loop writes the full text to `data/sessions/<id>/tool_cache/<call_id>.txt`
//! and keeps only a 2KB preview + absolute path. This tool lets the agent
//! retrieve the full text on demand — progressive disclosure in the other
//! direction: preview now, pull the whole result only when the task needs it.
//!
//! Reads are unrestricted (same policy as the sandbox read path), so no
//! allowlist is required here; the 4MB guard only prevents accidentally loading
//! a non-cache file that is far larger than anything paging would produce.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Guard against reading files far beyond what paging ever writes (~4MB).
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

pub struct ToolCacheReadTool;

impl ToolCacheReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolCacheReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolCacheReadTool {
    fn name(&self) -> &str {
        "tool_cache_read"
    }

    fn description(&self) -> &str {
        "Read the full text of a paged tool output that was saved to disk. \
         Use when a tool result was shortened to '[tool output saved: <path> ...]' \
         and you need the complete content. Parameters: path (required — the \
         absolute path shown in the placeholder)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the saved tool output (.txt), as shown in the '[tool output saved: <path>]' placeholder"
                }
            },
            "required": ["path"]
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
        let path_str = params["path"].as_str().unwrap_or("");
        if path_str.is_empty() {
            return Ok(ToolOutput {
                content: "path is required".into(),
                is_error: true,
                ..Default::default()
            });
        }
        let path = std::path::PathBuf::from(path_str);

        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("无法读取 {}: {e}", path.display()),
                    is_error: true,
                    ..Default::default()
                });
            }
        };
        if meta.len() > MAX_READ_BYTES {
            return Ok(ToolOutput {
                content: format!(
                    "{} 超过 {}MB 上限，拒绝读取",
                    path.display(),
                    MAX_READ_BYTES / 1024 / 1024
                ),
                is_error: true,
                ..Default::default()
            });
        }
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(e) => Ok(ToolOutput {
                content: format!("无法读取 {}: {e}", path.display()),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_full_text() {
        let tool = ToolCacheReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let body = "x".repeat(5000);
        let p = dir.path().join("out.txt");
        std::fs::write(&p, &body).unwrap();
        let out = tool
            .execute(serde_json::json!({ "path": p.display().to_string() }), None)
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content.len(), body.len());
        assert_eq!(out.content, body);
    }

    #[tokio::test]
    async fn missing_path_is_error() {
        let tool = ToolCacheReadTool::new();
        let out = tool.execute(serde_json::json!({}), None).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("path is required"));
    }

    #[tokio::test]
    async fn nonexistent_file_is_error() {
        let tool = ToolCacheReadTool::new();
        let out = tool
            .execute(serde_json::json!({ "path": "C:/no/such/file.txt" }), None)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("无法读取"));
    }

    #[tokio::test]
    async fn oversized_file_rejected() {
        let tool = ToolCacheReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("huge.txt");
        let big = vec![b'x'; 5 * 1024 * 1024];
        std::fs::write(&p, &big).unwrap();
        let out = tool
            .execute(serde_json::json!({ "path": p.display().to_string() }), None)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("上限"));
    }
}
