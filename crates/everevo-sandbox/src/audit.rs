//! Audit trail — append-only JSONL writer, one record per line.
//!
//! Each session gets its own `audit.jsonl` in its sandbox directory.
//! Records are flushed after every write so crashes don't lose data.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::provider::AuditRecord;

/// Thread-safe append-only JSONL audit writer.
pub struct AuditWriter {
    inner: Mutex<BufWriter<File>>,
    path: PathBuf,
}

impl AuditWriter {
    /// Create (or append to) an audit file at `dir/audit.jsonl`.
    pub fn open(dir: &PathBuf) -> Result<Self, String> {
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
