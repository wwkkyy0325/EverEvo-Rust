//! Download task definition.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Unique task identifier.
pub type TaskId = String;

/// Task priority — higher values execute first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Priority {
    #[default]
    Normal = 1,
    Low = 0,
    High = 2,
    Critical = 3,
}

/// The network region hint — helps the mirror resolver prefer domestic/international mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Region {
    #[default]
    Auto,
    /// Prefer domestic (CN) mirrors
    Domestic,
    /// Prefer international mirrors
    International,
}

/// A download task submitted to the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    /// Unique ID — auto-generated if not set.
    pub id: TaskId,

    /// The primary URL to download from.
    pub url: String,

    /// Local destination path (directory + filename).
    pub dest_path: PathBuf,

    /// Task priority.
    #[serde(default)]
    pub priority: Priority,

    /// Region hint for mirror selection.
    #[serde(default)]
    pub region: Region,

    /// Maximum concurrent chunk workers for this task (0 = use engine default).
    #[serde(default)]
    pub max_chunks: usize,

    /// Chunk size in bytes (0 = use engine default, 4 MiB).
    #[serde(default)]
    pub chunk_size: u64,

    /// Maximum retry attempts per chunk (0 = use engine default).
    #[serde(default)]
    pub max_retries: u32,

    /// Request timeout in seconds (0 = use engine default).
    #[serde(default)]
    pub timeout_secs: u64,

    /// Extra HTTP headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// If set, checks SHA-256 of the downloaded file.
    #[serde(default)]
    pub expected_sha256: Option<String>,

    /// Arbitrary metadata attached by the caller.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl DownloadTask {
    /// Create a new download task with defaults.
    pub fn new(url: impl Into<String>, dest_path: impl Into<PathBuf>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            url: url.into(),
            dest_path: dest_path.into(),
            priority: Priority::Normal,
            region: Region::Auto,
            max_chunks: 0,
            chunk_size: 0,
            max_retries: 0,
            timeout_secs: 0,
            headers: HashMap::new(),
            expected_sha256: None,
            metadata: serde_json::Value::Null,
        }
    }

    /// Set priority.
    pub fn with_priority(mut self, p: Priority) -> Self {
        self.priority = p;
        self
    }

    /// Set region hint.
    pub fn with_region(mut self, r: Region) -> Self {
        self.region = r;
        self
    }

    /// Set concurrency.
    pub fn with_chunks(mut self, n: usize, size: u64) -> Self {
        self.max_chunks = n;
        self.chunk_size = size;
        self
    }

    /// Set retry count.
    pub fn with_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Set expected checksum.
    pub fn with_sha256(mut self, hash: impl Into<String>) -> Self {
        self.expected_sha256 = Some(hash.into());
        self
    }

    /// Set arbitrary metadata.
    pub fn with_metadata(mut self, meta: serde_json::Value) -> Self {
        self.metadata = meta;
        self
    }

    /// Add a custom header.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Derive the temp directory for chunk files.
    pub fn temp_dir(&self) -> PathBuf {
        self.dest_path.with_extension("")
    }

    /// Derive a chunk file path.
    pub fn chunk_path(&self, index: usize) -> PathBuf {
        let mut base = self.dest_path.clone();
        let ext = base.extension().and_then(|e| e.to_str()).unwrap_or("");
        base.set_extension(format!("{ext}.part.{index}"));
        base
    }

    /// Derive the resume state file path.
    pub fn resume_path(&self) -> PathBuf {
        let mut base = self.dest_path.clone();
        let stem = base
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("download");
        base.set_file_name(format!("{stem}.resume.json"));
        base
    }
}
