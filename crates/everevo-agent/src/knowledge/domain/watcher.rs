//! Domain watcher — monitors inbox directory for new files.
//!
//! Phase 3a: poll-based (simple, cross-platform).
//! Phase 3b: inotify (Linux) / ReadDirectoryChangesW (Windows).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use everevo_core::EverEvoError;

/// Monitors a directory for new files and triggers the ingestion pipeline.
pub struct DomainWatcher {
    inbox_path: PathBuf,
    registry_path: PathBuf,
    /// Track already-processed files by (filename, modified_time).
    processed: HashMap<String, u64>,
}

impl DomainWatcher {
    pub fn new(
        inbox_path: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
    ) -> Result<Self, EverEvoError> {
        let inbox_path: PathBuf = inbox_path.into();
        std::fs::create_dir_all(&inbox_path)
            .map_err(|e| EverEvoError::Internal(format!("Create inbox: {e}")))?;
        Ok(Self {
            inbox_path,
            registry_path: registry_path.into(),
            processed: HashMap::new(),
        })
    }

    /// Scan inbox for new or modified files.
    /// Returns list of (filename, content_bytes) for new files.
    pub fn scan(&mut self) -> Result<Vec<(String, Vec<u8>)>, EverEvoError> {
        let mut new_files = Vec::new();
        let entries = std::fs::read_dir(&self.inbox_path)
            .map_err(|e| EverEvoError::Internal(format!("Read inbox: {e}")))?;

        for entry in entries {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Entry: {e}")))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().to_string();
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                })
                .unwrap_or(0);

            let key = format!("{filename}:{modified}");
            if self.processed.contains_key(&key) {
                continue;
            }

            let content = std::fs::read(&path)
                .map_err(|e| EverEvoError::Internal(format!("Read file {filename}: {e}")))?;

            self.processed.insert(key, modified);
            new_files.push((filename, content));
        }

        Ok(new_files)
    }

    /// Clear processed files cache (for re-scan).
    pub fn reset(&mut self) {
        self.processed.clear();
    }

    pub fn inbox_path(&self) -> &Path {
        &self.inbox_path
    }
    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_watcher_scan() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let reg_path = dir.path().join("registry.json");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("test.md"), b"# Test").unwrap();

        let mut watcher = DomainWatcher::new(&inbox, &reg_path).unwrap();
        let files = watcher.scan().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "test.md");

        // Second scan should be empty (already processed)
        let files2 = watcher.scan().unwrap();
        assert!(files2.is_empty());
    }
}
