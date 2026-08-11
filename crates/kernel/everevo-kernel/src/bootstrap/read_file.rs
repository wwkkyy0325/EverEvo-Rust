//! Bootstrap ReadFile — kernel-built file reading tool.
//!
//! Resolves relative paths against a configurable `work_dir` (the sandbox
//! session directory) so that `./test.txt` reads from the correct location
//! regardless of the server process's own cwd.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct BootstrapReadFile {
    work_dir: Option<PathBuf>,
}

impl BootstrapReadFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base directory for resolving relative paths.
    /// When set, `read_file("./test.txt")` resolves to `{work_dir}/test.txt`.
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
impl Tool for BootstrapReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem. Kernel-built — always available. \
         Relative paths resolve against the sandbox working directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to read (relative paths resolve to workspace)"}
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
        let path = params["path"].as_str().ok_or_else(|| EverEvoError::Tool {
            tool: "read_file".into(),
            message: "Missing 'path' parameter".into(),
        })?;
        let resolved = self.resolve_path(path);
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let msg = if resolved != path {
                    format!("[read_file resolved '{path}' → '{resolved}']\n{content}")
                } else {
                    content
                };
                Ok(ToolOutput::text(msg))
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to read '{resolved}': {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
