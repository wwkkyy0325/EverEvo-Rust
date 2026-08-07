//! Provider traits — bootstrap, download, and shell abstractions.
//!
//! Following the same pattern as `Tool`, `LlmProvider`, `SandboxProvider`,
//! and `Agent`: traits live in `everevo-core` so downstream crates can
//! implement mocks for testing without depending on heavy implementations.

use async_trait::async_trait;

use crate::EverEvoError;

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
///
/// The canonical implementation is [`everevo_bootstrap::Bootstrap`].
/// Provisioning (download + extract) is handled separately by
/// [`everevo_bootstrap::pipeline::InitPipeline`] because it requires a
/// downloader backend and emits SSE progress events.
#[async_trait]
pub trait BootstrapProvider: Send + Sync {
    /// Check which assets are ready, missing, or corrupt.
    async fn check(&self) -> Result<BootstrapStatus, EverEvoError>;
}
