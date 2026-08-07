//! In-process download tool with workspace-scoped output directory.
//!
//! Complemented by MCP plugin `plugin-download` which provides basic URL-to-file download.
//! This in-process version integrates with the session sandbox work_dir and downloader
//! engine — features the MCP plugin cannot provide.
//! This in-process implementation is kept for backward compatibility.
//! New development should use the MCP plugin version.

//! Download tool — wraps everevo-downloader.
//!
//! The agent uses this to download files with multi-mirror, resume, and progress support.
//! Calls `Downloader::submit()` directly — **no CLI overhead**.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use everevo_downloader::task::{DownloadTask, Priority, Region};
use everevo_downloader::Downloader;
use tokio_util::sync::CancellationToken;

use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;

pub struct DownloadTool {
    downloader: Arc<Downloader>,
    /// Sandbox working directory. Relative `dest_path`s are resolved against
    /// this directory so downloads (and their `.resume.json` sidecar files)
    /// stay inside the sandbox instead of leaking into the process CWD.
    work_dir: Option<PathBuf>,
}

impl DownloadTool {
    pub fn new(downloader: Arc<Downloader>) -> Self {
        Self {
            downloader,
            work_dir: None,
        }
    }

    /// Set the sandbox working directory for relative path resolution.
    pub fn with_work_dir(mut self, dir: PathBuf) -> Self {
        self.work_dir = Some(dir);
        self
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
        _cancel: Option<&CancellationToken>,
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

        // Resolve relative paths against the sandbox working directory so
        // downloads don't leak into the process CWD (which is src-tauri/
        // in Tauri dev mode, triggering unwanted rebuilds).
        let dest = resolve_dest_path(dest_path, self.work_dir.as_deref());

        let task = DownloadTask::new(url, dest)
            .with_region(region)
            .with_priority(priority);

        let handle = match self.downloader.submit(task).await {
            Ok(h) => h,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Download failed: {e}. The sandbox may not have network access. Try using the shell tool with curl/wget if network is available, or download the file manually."),
                    is_error: true,
                 ..Default::default() });
            }
        };

        let result = match handle.await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!(
                        "Download failed: {e}. Network may be unavailable in sandbox environment."
                    ),
                    is_error: true,
                    ..Default::default()
                });
            }
        };

        if result.is_success() {
            // When the server omits Content-Length, the downloader reports 0
            // bytes even though the file was saved correctly.  Read the real
            // file size from disk so the LLM sees accurate numbers.
            let size = if result.size_bytes == 0 {
                result
                    .path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .unwrap_or(0)
            } else {
                result.size_bytes
            };
            Ok(ToolOutput {
                content: format!(
                    "Downloaded {} bytes to {} in {}ms",
                    size,
                    result
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    result.duration_ms
                ),
                is_error: false,
                ..Default::default()
            })
        } else {
            Ok(ToolOutput {
                content: format!(
                    "Download failed: {}",
                    result.error_message().unwrap_or("unknown error")
                ),
                is_error: true,
                ..Default::default()
            })
        }
    }
}

/// Resolve a user-supplied `dest_path` to an absolute path.
///
/// - Absolute paths are returned as-is.
/// - Relative paths are joined against `work_dir` (sandbox working directory).
/// - If no `work_dir` is set, the returned path will still be absolute
///   (resolved against the process CWD as last resort).
fn resolve_dest_path(raw: &str, work_dir: Option<&Path>) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match work_dir {
        Some(dir) => dir.join(p),
        None => {
            // Fallback: resolve against process CWD so we still get an absolute
            // path (avoiding ambiguity), even though this will be src-tauri/
            // in dev mode.  This branch should only be hit in tests.
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        }
    }
}
