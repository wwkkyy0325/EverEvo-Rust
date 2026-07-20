//! Provider traits — download, bootstrap, and shell abstractions.
//!
//! Following the same pattern as `Tool`, `LlmProvider`, `SandboxProvider`,
//! and `Agent`: traits live in `everevo-core` so downstream crates can
//! implement mocks for testing without depending on heavy implementations.

use async_trait::async_trait;

use crate::EverEvoError;

// ── Download Provider ──────────────────────────────────────────────────

/// Result of a download operation.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub output_path: String,
    pub bytes_downloaded: u64,
    pub duration_ms: u64,
    pub mirror_used: Option<String>,
}

/// Abstract download provider — implement for testing or multi-backend.
#[async_trait]
pub trait DownloadProvider: Send + Sync {
    /// Download a file from a URL to a local path.
    async fn download(&self, url: &str, dest: &std::path::Path) -> Result<DownloadResult, EverEvoError>;
}

// ── Bootstrap Provider ─────────────────────────────────────────────────

/// Result of a bootstrap check.
#[derive(Debug, Clone)]
pub struct BootstrapStatus {
    pub ready: Vec<String>,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
    pub download_size_bytes: u64,
}

/// Abstract bootstrap provider — implement for testing or mocked provisioning.
#[async_trait]
pub trait BootstrapProvider: Send + Sync {
    /// Check which assets are ready, missing, or corrupt.
    async fn check(&self) -> Result<BootstrapStatus, EverEvoError>;

    /// Provision (download + extract) missing assets.
    async fn provision(&self) -> Result<Vec<String>, EverEvoError>;
}

