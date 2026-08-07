//! Audit trail — structured record + append-only JSONL writer.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Structured audit record — one per sandbox execution.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditRecord {
    pub timestamp: String,
    pub shell: String,
    pub command: String,
    pub working_dir: String,
    pub timeout_secs: u64,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub killed_by_timeout: bool,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub permission_level: String,
    pub was_confirmed: bool,
    pub requires_admin: bool,
    pub network_allowed: bool,
    pub memory_limit_mb: Option<u64>,
    pub job_object_applied: bool,
    pub external_paths: Vec<String>,
    pub decision: String,
}

/// Thread-safe append-only JSONL audit writer.
pub struct AuditWriter {
    inner: Mutex<BufWriter<File>>,
    path: PathBuf,
}

impl AuditWriter {
    /// Create (or append to) an audit file at `dir/audit.jsonl`.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let path = dir.join("audit.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("audit open {}: {e}", path.display()))?;
        Ok(Self {
            inner: Mutex::new(BufWriter::new(file)),
            path,
        })
    }

    /// Append one audit record. Flushed immediately — crash-safe.
    pub fn write(&self, record: &AuditRecord) {
        if let Ok(mut w) = self.inner.lock() {
            if let Ok(json) = serde_json::to_string(record) {
                let _ = writeln!(w, "{json}");
                let _ = w.flush();
            }
        }
    }

    /// Number of records written so far (for stats / health checks).
    pub fn count(&self) -> usize {
        // We track via file line count on read, but for now return 0.
        // This is a placeholder for future dashboard use.
        0
    }

    /// Path to the audit file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for AuditWriter {
    fn drop(&mut self) {
        if let Ok(mut w) = self.inner.lock() {
            let _ = w.flush();
        }
    }
}
