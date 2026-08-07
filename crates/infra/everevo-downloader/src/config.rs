//! Downloader configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::task::Region;

/// Global configuration for the download engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloaderConfig {
    /// Maximum concurrent tasks across the entire engine.
    pub max_concurrent_tasks: usize,

    /// Maximum concurrent chunks per task (when using chunked strategy).
    pub max_chunks_per_task: usize,

    /// Default chunk size in bytes (4 MiB).
    pub default_chunk_size: u64,

    /// Default maximum retry attempts per chunk.
    pub max_retries: u32,

    /// Default request timeout in seconds.
    pub timeout_secs: u64,

    /// File size threshold above which chunked download is used.
    /// 0 = never auto-chunk. Default: 10 MiB.
    pub chunk_threshold: u64,

    /// Temporary directory for partial downloads.
    /// If not set, uses the destination file's directory.
    pub temp_dir: Option<PathBuf>,

    /// User-Agent header sent with all requests.
    pub user_agent: String,

    /// Connection pool idle timeout in seconds.
    pub pool_idle_timeout_secs: u64,

    /// Whether to use the mirror resolution system.
    pub mirror_enabled: bool,

    /// Region hint for mirror selection (can be overridden per-task).
    pub default_region: Region,

    /// Whether to verify SHA-256 checksums when available.
    pub verify_checksums: bool,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: 4,
            max_chunks_per_task: 8,
            default_chunk_size: 4 * 1024 * 1024, // 4 MiB
            max_retries: 3,
            timeout_secs: 30,
            chunk_threshold: 10 * 1024 * 1024, // 10 MiB
            temp_dir: None,
            user_agent: format!("EverEvo-Downloader/{} (Rust)", env!("CARGO_PKG_VERSION")),
            pool_idle_timeout_secs: 90,
            mirror_enabled: true,
            default_region: Region::Auto,
            verify_checksums: true,
        }
    }
}

impl DownloaderConfig {
    /// Effective chunk size for a task (task override or engine default).
    pub fn effective_chunk_size(&self, task_chunk_size: u64) -> u64 {
        if task_chunk_size > 0 {
            task_chunk_size
        } else {
            self.default_chunk_size
        }
    }

    /// Effective max chunks for a task.
    pub fn effective_max_chunks(&self, task_max_chunks: usize) -> usize {
        if task_max_chunks > 0 {
            task_max_chunks.min(self.max_chunks_per_task)
        } else {
            self.max_chunks_per_task
        }
    }

    /// Effective retries for a task.
    pub fn effective_retries(&self, task_retries: u32) -> u32 {
        if task_retries > 0 {
            task_retries
        } else {
            self.max_retries
        }
    }

    /// Effective timeout for a task.
    pub fn effective_timeout_secs(&self, task_timeout: u64) -> u64 {
        if task_timeout > 0 {
            task_timeout
        } else {
            self.timeout_secs
        }
    }

    /// Whether to use chunked download for a given file size.
    pub fn should_chunk(&self, file_size: u64, task_chunks: usize) -> bool {
        task_chunks > 0 || (self.chunk_threshold > 0 && file_size > self.chunk_threshold)
    }
}
