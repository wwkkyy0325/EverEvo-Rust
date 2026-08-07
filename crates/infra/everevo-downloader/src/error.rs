//! Download error types.

use std::path::PathBuf;
use thiserror::Error;

/// All download-related errors.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Task cancelled: {task_id}")]
    Cancelled { task_id: String },

    #[error("All mirrors exhausted for URL: {url} (tried: {tried:?})")]
    AllMirrorsExhausted { url: String, tried: Vec<String> },

    #[error("Server does not support range requests: {url}")]
    RangeNotSupported { url: String },

    #[error("Invalid URL: {url}")]
    InvalidUrl { url: String },

    #[error("File size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("Task not found: {task_id}")]
    TaskNotFound { task_id: String },

    #[error("Timeout after {duration_ms}ms: {url}")]
    Timeout { url: String, duration_ms: u64 },

    #[error("Max retries exceeded ({max}): {url}")]
    MaxRetriesExceeded { url: String, max: u32 },

    #[error("Chunk assembly failed: {0}")]
    ChunkAssembly(String),

    #[error("{0}")]
    Other(String),
}

impl DownloadError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<DownloadError> for everevo_core::EverEvoError {
    fn from(e: DownloadError) -> Self {
        everevo_core::EverEvoError::Download(e.to_string())
    }
}
