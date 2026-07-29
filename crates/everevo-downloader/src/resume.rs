//! Resume/checkpoint state for interrupted downloads.
//!
//! When a download is interrupted, the engine writes a `.resume.json` file
//! next to the destination. On retry, it reads this file to determine which
//! chunks have already been downloaded.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent resume state for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeState {
    /// Original task ID.
    pub task_id: String,
    /// The URL being downloaded (to verify it hasn't changed).
    pub url: String,
    /// Total file size in bytes (from Content-Length header).
    pub total_size: u64,
    /// Chunk size used.
    pub chunk_size: u64,
    /// Total number of chunks.
    pub total_chunks: usize,
    /// Which chunks have been fully downloaded (index → byte range).
    #[serde(default)]
    pub completed_chunks: Vec<usize>,
    /// Which mirror was last used successfully.
    pub last_mirror: Option<String>,
    /// Timestamp of last update.
    pub updated_at: String,
}

impl ResumeState {
    /// Create a new resume state.
    pub fn new(task_id: &str, url: &str, total_size: u64, chunk_size: u64) -> Self {
        let total_chunks = if chunk_size > 0 && total_size > 0 {
            total_size.div_ceil(chunk_size) as usize
        } else {
            1
        };

        Self {
            task_id: task_id.to_string(),
            url: url.to_string(),
            total_size,
            chunk_size,
            total_chunks,
            completed_chunks: Vec::new(),
            last_mirror: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Mark a chunk as completed.
    pub fn mark_chunk_done(&mut self, index: usize) {
        if !self.completed_chunks.contains(&index) {
            self.completed_chunks.push(index);
            self.completed_chunks.sort_unstable();
        }
    }

    /// Get the byte range for a specific chunk.
    #[allow(dead_code)]
    pub fn chunk_range(&self, index: usize) -> (u64, u64) {
        let start = index as u64 * self.chunk_size;
        let end = if index == self.total_chunks - 1 {
            self.total_size.saturating_sub(1)
        } else {
            (start + self.chunk_size).saturating_sub(1)
        };
        (start, end)
    }

    /// Check if all chunks are done.
    #[allow(dead_code)]
    pub fn is_complete(&self) -> bool {
        self.completed_chunks.len() >= self.total_chunks
    }

    /// Bytes remaining to download.
    #[allow(dead_code)]
    pub fn remaining_bytes(&self) -> u64 {
        let done = self.completed_chunks.len() as u64 * self.chunk_size;
        self.total_size.saturating_sub(done)
    }

    /// Save to a file.
    pub async fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(path, json).await?;
        Ok(())
    }

    /// Load from a file.
    pub async fn load(path: &PathBuf) -> std::io::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let json = tokio::fs::read_to_string(path).await?;
        let state: Self = serde_json::from_str(&json)?;
        Ok(Some(state))
    }

    /// Delete the resume file.
    pub async fn cleanup(&self, path: &PathBuf) {
        let _ = tokio::fs::remove_file(path).await;
    }
}
