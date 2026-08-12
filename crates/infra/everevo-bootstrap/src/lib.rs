//! EverEvo bootstrap — first-run provisioning of portable runtimes and embedding models.
//!
//! ## Architecture
//!
//! ```text
//! App startup → Bootstrap::new(data_dir)
//!   → reads data/runtime/.manifest.json + data/models/.manifest.json
//!   → checks each item (file existence, version match)
//!   → returns BootstrapResult { ready, missing }
//!
//! If missing: download via everevo-downloader, extract, write manifest.
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! let bs = Bootstrap::new(config.data_dir.clone());
//! let status = bs.check().await?;
//! if !status.missing.is_empty() {
//!     bs.provision(&status.missing).await?;
//! }
//! ```

pub mod manifest;
pub mod pipeline;
pub mod resource_extractor;
pub mod runtime;

mod assets;
mod checker;
mod error;
mod registry;

pub use assets::{Asset, AssetFile, AssetKind};
pub use error::BootstrapError;
pub use registry::{assets_for_target, detect_target};

use std::path::PathBuf;
use tokio::sync::RwLock;

use async_trait::async_trait;
use checker::check_manifest;
use everevo_core::provider::{BootstrapProvider, BootstrapStatus};
use everevo_core::EverEvoError;
use manifest::Manifest;
use runtime::{RuntimeEnv, RuntimeManager};

// ── Bootstrap ───────────────────────────────────────────────────────────

/// The bootstrap engine.
pub struct Bootstrap {
    data_dir: PathBuf,
    /// Cached check result — avoids re-scanning files.
    cached_result: RwLock<Option<BootstrapResult>>,
}

/// Result of checking what's installed vs what's needed.
#[derive(Debug, Clone)]
pub struct BootstrapResult {
    /// Items that are correctly installed.
    pub ready: Vec<Provisioned>,
    /// Items that need to be downloaded.
    pub missing: Vec<Asset>,
    /// Items that failed checksum verification (may be corrupt).
    pub corrupt: Vec<Provisioned>,
    /// Total download size estimate in bytes (for missing items).
    pub download_size_bytes: u64,
}

/// A successfully provisioned asset.
#[derive(Debug, Clone)]
pub struct Provisioned {
    pub key: String,
    pub version: String,
    pub path: PathBuf,
}

impl Bootstrap {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cached_result: RwLock::new(None),
        }
    }

    /// The root data directory for all runtime + model assets.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Scan the filesystem and determine what's installed vs missing.
    /// Results are cached; call `invalidate()` to force re-scan.
    pub async fn check(&self) -> Result<BootstrapResult, BootstrapError> {
        // Return cached result if available
        {
            let cached = self.cached_result.read().await;
            if let Some(ref result) = *cached {
                return Ok(result.clone());
            }
        }

        let runtime_dir = self.data_dir.join("runtime");
        let models_dir = self.data_dir.join("models");

        let target = detect_target();
        let all_assets = assets_for_target(&target);
        // Split into runtimes (Runtime + SystemProvided) and models.
        let runtimes: Vec<Asset> = all_assets
            .iter()
            .filter(|a| !a.is_model())
            .cloned()
            .collect();
        let models: Vec<Asset> = all_assets
            .iter()
            .filter(|a| a.is_model())
            .cloned()
            .collect();

        let runtime_result = check_manifest(&runtime_dir, &runtimes).await;
        let model_result = check_manifest(&models_dir, &models).await;

        let mut ready = runtime_result.ready;
        ready.extend(model_result.ready);

        let mut missing = runtime_result.missing;
        missing.extend(model_result.missing);

        let mut corrupt = runtime_result.corrupt;
        corrupt.extend(model_result.corrupt);

        let download_size = missing.iter().map(|a| a.size_bytes).sum();

        let result = BootstrapResult {
            ready,
            missing,
            corrupt,
            download_size_bytes: download_size,
        };

        // Cache
        {
            let mut cached = self.cached_result.write().await;
            *cached = Some(result.clone());
        }

        Ok(result)
    }

    /// Build a runtime environment with PATH entries for all installed runtimes.
    ///
    /// Callers (e.g., `AppState::create_sandbox`) use this to inject portable
    /// Python, Node, Git, and ONNX Runtime into sandboxed process PATHs.
    pub async fn build_runtime_env(&self) -> RuntimeEnv {
        let mgr = RuntimeManager::new(&self.data_dir);
        mgr.build_env().await.unwrap_or_default()
    }

    /// Invalidate the cached check result — forces re-scan on next `check()`.
    pub async fn invalidate(&self) {
        let mut cached = self.cached_result.write().await;
        *cached = None;
    }
}

#[async_trait]
impl BootstrapProvider for Bootstrap {
    async fn check(&self) -> Result<BootstrapStatus, EverEvoError> {
        // UFCS: call the inherent check(), not the trait method
        let result = Bootstrap::check(self).await?;
        Ok(BootstrapStatus {
            ready: result.ready.into_iter().map(|r| r.key).collect(),
            missing: result.missing.into_iter().map(|m| m.key).collect(),
            corrupt: result.corrupt.into_iter().map(|c| c.key).collect(),
            download_size_bytes: result.download_size_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootstrap_new() {
        let bs = Bootstrap::new(PathBuf::from("./data"));
        assert!(bs.data_dir.ends_with("data"));
    }
}
