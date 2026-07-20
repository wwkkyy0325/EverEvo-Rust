//! Download tool — wraps everevo-downloader.
//!
//! The agent uses this to download files with multi-mirror, resume, and progress support.
//! Calls `Downloader::submit()` directly — **no CLI overhead**.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use everevo_downloader::Downloader;
use everevo_downloader::task::{DownloadTask, Priority, Region};

use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;

pub struct DownloadTool {
    downloader: Arc<Downloader>,
}

impl DownloadTool {
    pub fn new(downloader: Arc<Downloader>) -> Self {
        Self { downloader }
    }
}

#[async_trait]
impl Tool for DownloadTool {
    fn name(&self) -> &str {
        "download"
    }

    fn description(&self) -> &str {
        "Download a file from a URL to a local path. \
         Supports multi-mirror failover (domestic + international), \
         resumable downloads, and large-file chunked transfer. \
         Progress events are streamed back to the agent."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to download from"
                },
                "dest_path": {
                    "type": "string",
                    "description": "Local destination path (absolute or relative to sandbox workspace)"
                },
                "region": {
                    "type": "string",
                    "enum": ["domestic", "international", "auto"],
                    "description": "Network region hint for mirror selection",
                    "default": "auto"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high", "critical"],
                    "description": "Task priority",
                    "default": "normal"
                }
            },
            "required": ["url", "dest_path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium // network access required
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("url is required".into()))?;

        let dest_path = params["dest_path"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("dest_path is required".into()))?;

        let region = match params.get("region").and_then(|v| v.as_str()) {
            Some("domestic") => Region::Domestic,
            Some("international") => Region::International,
            _ => Region::Auto,
        };

        let priority = match params.get("priority").and_then(|v| v.as_str()) {
            Some("high") => Priority::High,
            Some("critical") => Priority::Critical,
            Some("low") => Priority::Low,
            _ => Priority::Normal,
        };

        let task = DownloadTask::new(url, PathBuf::from(dest_path))
            .with_region(region)
            .with_priority(priority);

        let handle = match self.downloader.submit(task).await {
            Ok(h) => h,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Download failed: {e}. The sandbox may not have network access. Try using the shell tool with curl/wget if network is available, or download the file manually."),
                    is_error: true,
                });
            }
        };

        let result = match handle.await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Download failed: {e}. Network may be unavailable in sandbox environment."),
                    is_error: true,
                });
            }
        };

        if result.is_success() {
            Ok(ToolOutput {
                content: format!(
                    "Downloaded {} bytes to {} in {}ms",
                    result.size_bytes,
                    result.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                    result.duration_ms
                ),
                is_error: false,
            })
        } else {
            Ok(ToolOutput {
                content: format!("Download failed: {}", result.error_message().unwrap_or("unknown error")),
                is_error: true,
            })
        }
    }
}
