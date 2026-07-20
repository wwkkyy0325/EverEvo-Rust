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
pub mod runtime;

use std::path::PathBuf;
use tokio::sync::RwLock;

use everevo_core::EverEvoError;
use manifest::Manifest;

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
        Self { data_dir, cached_result: RwLock::new(None) }
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

        let runtime_result = check_manifest(&runtime_dir, &RUNTIMES).await;
        let model_result = check_manifest(&models_dir, &MODELS).await;

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

    /// Invalidate the cached check result — forces re-scan on next `check()`.
    pub async fn invalidate(&self) {
        let mut cached = self.cached_result.write().await;
        *cached = None;
    }
}

// ── Asset Definitions ───────────────────────────────────────────────────

/// A file to download alongside the primary asset.
#[derive(Debug, Clone)]
pub struct AssetFile {
    pub filename: String,
    pub url: String,
    pub mirror_url: Option<String>,
}

/// Something that can be provisioned.
#[derive(Debug, Clone)]
pub struct Asset {
    pub key: String,
    pub kind: AssetKind,
    pub version: String,
    pub primary_url: String,
    /// Mirror URLs for domestic access.
    pub mirror_urls: Vec<String>,
    /// Additional files needed (e.g., tokenizer.json for models).
    pub extra_files: Vec<AssetFile>,
    /// Expected SHA-256 (hex) for integrity check.
    pub sha256: Option<String>,
    /// Compressed size in bytes.
    pub size_bytes: u64,
    /// Target install directory relative to `{category_dir}/{key}/`.
    pub description: String,
}

impl Asset {
    pub fn is_model(&self) -> bool { matches!(self.kind, AssetKind::Model) }
    pub fn is_runtime(&self) -> bool { matches!(self.kind, AssetKind::Runtime) }

    /// All URLs to try for this asset (primary first, then mirrors).
    pub fn all_urls(&self) -> Vec<&str> {
        std::iter::once(self.primary_url.as_str())
            .chain(self.mirror_urls.iter().map(|s| s.as_str()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Runtime,
    Model,
}

// ── Manifest Check Logic ────────────────────────────────────────────────

struct CheckOutcome {
    ready: Vec<Provisioned>,
    missing: Vec<Asset>,
    corrupt: Vec<Provisioned>,
}

/// Check all defined assets against a manifest file.
async fn check_manifest(dir: &PathBuf, assets: &[Asset]) -> CheckOutcome {
    let manifest = Manifest::load(&dir.join(".manifest.json")).await;

    let mut ready = Vec::new();
    let mut missing = Vec::new();
    let mut corrupt = Vec::new();

    for asset in assets {
        let install_dir = dir.join(&asset.key);
        let entry = manifest.as_ref().ok().and_then(|m| m.get(&asset.key));

        let version_match = entry.map(|e| e.version == asset.version).unwrap_or(false);
        let dir_exists = install_dir.exists();

        // Fallback: if manifest is missing/empty but .extracted sentinel
        // matches, the asset was manually placed or pre-seeded. Treat as ready.
        let sentinel_match = !version_match
            && dir_exists
            && read_sentinel_version(&install_dir).as_deref() == Some(&asset.version);

        // Verify all declared files exist AND have reasonable sizes.
        // An interrupted download may leave a 0-byte or truncated file
        // that passes the exist() check but is corrupt.
        let files_intact = verify_files_intact(asset, &install_dir);

        if version_match && dir_exists {
            if !files_intact {
                corrupt.push(Provisioned {
                    key: asset.key.clone(),
                    version: asset.version.clone(),
                    path: install_dir,
                });
                continue;
            }
            // Verify checksum if available
            let verified = if let Some(ref expected_sha) = asset.sha256 {
                verify_dir_checksum(&install_dir, expected_sha).await
            } else {
                true // No checksum = skip verification
            };

            if verified {
                ready.push(Provisioned {
                    key: asset.key.clone(),
                    version: asset.version.clone(),
                    path: install_dir,
                });
            } else {
                corrupt.push(Provisioned {
                    key: asset.key.clone(),
                    version: asset.version.clone(),
                    path: install_dir,
                });
            }
        } else if sentinel_match {
            if !files_intact {
                corrupt.push(Provisioned {
                    key: asset.key.clone(),
                    version: asset.version.clone(),
                    path: install_dir,
                });
                continue;
            }
            ready.push(Provisioned {
                key: asset.key.clone(),
                version: asset.version.clone(),
                path: install_dir,
            });
        } else {
            missing.push(asset.clone());
        }
    }

    CheckOutcome {
        ready,
        missing,
        corrupt,
    }
}

/// Verify all declared files exist with reasonable minimum sizes.
/// An interrupted download may leave a 0-byte or truncated file that
/// passes the `exists()` check but is corrupt.
fn verify_files_intact(asset: &Asset, install_dir: &std::path::Path) -> bool {
    // Model ONNX files must be at least 1 MB (real models are 20–280 MB)
    let onnx_path = install_dir.join("model_quantized.onnx");
    let onnx_ok = onnx_path.exists()
        && onnx_path.metadata().map(|m| m.len() > 1_048_576).unwrap_or(false);

    // Extra files (json configs, tokenizers) must be at least 10 bytes
    let extras_ok = asset.extra_files.iter().all(|ef| {
        let p = install_dir.join(&ef.filename);
        p.exists() && p.metadata().map(|m| m.len() > 10).unwrap_or(false)
    });

    // Runtimes don't have model_quantized.onnx — only check extras
    if asset.is_runtime() {
        return extras_ok;
    }

    onnx_ok && extras_ok
}

/// Read the `.extracted` sentinel version string, if present.
fn read_sentinel_version(dir: &std::path::Path) -> Option<String> {
    let sentinel = dir.join(".extracted");
    if !sentinel.exists() { return None; }
    std::fs::read_to_string(&sentinel).ok().map(|s| s.trim().to_string())
}

/// Verify directory integrity via a marker file checksum.
/// Since runtimes/models are extracted archives, we check a sentinel file.
async fn verify_dir_checksum(dir: &std::path::Path, expected_sha: &str) -> bool {
    // Look for a .checksum file we wrote after successful extraction
    let checksum_path = dir.join(".checksum");
    if let Ok(content) = tokio::fs::read_to_string(&checksum_path).await {
        return content.trim() == expected_sha;
    }
    // Fallback: check if key executables exist
    let sentinels = dir.join("sentinels.txt");
    if sentinels.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&sentinels).await {
            return content.trim() == expected_sha;
        }
    }
    true // Can't verify → assume OK if version match
}

// ── Runtimes ────────────────────────────────────────────────────────────

const PYTHON_VERSION: &str = "3.12.8";
const NODE_VERSION: &str = "22.12.0";
const GIT_VERSION: &str = "2.47.1";
const ONNX_VERSION: &str = "1.24.2";

use std::sync::LazyLock;

static RUNTIMES: LazyLock<Vec<Asset>> = LazyLock::new(|| {
    vec![
        Asset {
            key: "python".into(),
            kind: AssetKind::Runtime,
            version: PYTHON_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/python/{0}/python-{0}-embed-amd64.zip",
                PYTHON_VERSION
            ),
            mirror_urls: vec![
                format!(
                    "https://registry.npmmirror.com/-/binary/python/{0}/python-{0}-embed-amd64.zip",
                    PYTHON_VERSION
                ),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 10_000_000, // ~10 MB
            description: "Python 3.12 embeddable runtime (portable, no install)".into(),
        },
        Asset {
            key: "node".into(),
            kind: AssetKind::Runtime,
            version: NODE_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/node/v{0}/node-v{0}-win-x64.zip",
                NODE_VERSION
            ),
            mirror_urls: vec![
                format!(
                    "https://npmmirror.com/mirrors/node/v{0}/node-v{0}-win-x64.zip",
                    NODE_VERSION
                ),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 30_000_000, // ~30 MB
            description: "Node.js portable runtime".into(),
        },
        Asset {
            key: "git".into(),
            kind: AssetKind::Runtime,
            version: GIT_VERSION.into(),
            primary_url: format!(
                "https://cdn.npmmirror.com/binaries/git-for-windows/v{0}.windows.1/MinGit-{0}-64-bit.zip",
                GIT_VERSION
            ),
            mirror_urls: vec![
                format!(
                    "https://npmmirror.com/mirrors/git-for-windows/v{0}.windows.1/MinGit-{0}-64-bit.zip",
                    GIT_VERSION
                ),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 50_000_000, // ~50 MB
            description: "MinGit portable (minimal Git for Windows)".into(),
        },
        Asset {
            key: "onnxruntime".into(),
            kind: AssetKind::Runtime,
            version: ONNX_VERSION.into(),
            primary_url: format!(
                "https://github.com/microsoft/onnxruntime/releases/download/v{0}/onnxruntime-win-x64-{0}.zip",
                ONNX_VERSION
            ),
            mirror_urls: vec![
                // npmmirror CDN — confirmed working in China (direct, no redirect)
                format!(
                    "https://cdn.npmmirror.com/binaries/onnxruntime/v{0}/onnxruntime-win-x64-{0}.zip",
                    ONNX_VERSION
                ),
                // npmmirror registry (302 redirect to CDN)
                format!(
                    "https://registry.npmmirror.com/-/binary/onnxruntime/v{0}/onnxruntime-win-x64-{0}.zip",
                    ONNX_VERSION
                ),
            ],
            extra_files: vec![],
            sha256: None,
            size_bytes: 71_000_000, // ~71 MB (v1.24.2)
            description: "ONNX Runtime for model inference".into(),
        },
    ]
});

static MODELS: LazyLock<Vec<Asset>> = LazyLock::new(|| {
    vec![
        Asset {
            key: "bge-small-zh".into(),
            kind: AssetKind::Model,
            version: "v1.5".into(),
            primary_url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 35_500_000, // ~35 MB ONNX INT8
            description: "BGE-small-zh — Chinese sentence embedding, 384 dims".into(),
        },
        Asset {
            key: "all-MiniLM-L6-v2".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 22_500_000,
            description: "all-MiniLM-L6-v2 — English sentence embedding, 384 dims".into(),
        },
        // ── EN Reranker (cross-encoder, lightweight) ────────────────
        Asset {
            key: "reranker-en".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 90_000_000,
            description: "EN cross-encoder reranker — re-rank retrieved docs".into(),
        },
        // ── CN Reranker (cross-encoder, bilingual) ──────────────────
        Asset {
            key: "reranker-cn".into(),
            kind: AssetKind::Model,
            version: "v1".into(),
            primary_url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/onnx/model_quantized.onnx".into(),
            mirror_urls: vec![],
            extra_files: vec![
                AssetFile { filename: "tokenizer.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/tokenizer.json".into(), mirror_url: None },
                AssetFile { filename: "config.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/config.json".into(), mirror_url: None },
                AssetFile { filename: "special_tokens_map.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/special_tokens_map.json".into(), mirror_url: None },
                AssetFile { filename: "tokenizer_config.json".into(), url: "https://hf-mirror.com/Xenova/bge-reranker-base/resolve/main/tokenizer_config.json".into(), mirror_url: None },
            ],
            sha256: None,
            size_bytes: 280_000_000,
            description: "BGE cross-encoder reranker — bilingual CN+EN re-ranking".into(),
        },
    ]
});

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Download error: {0}")]
    Download(String),

    #[error("Extraction error: {0}")]
    Extract(String),

    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("Unsupported archive format: {0}")]
    UnsupportedArchive(String),
}

impl From<BootstrapError> for EverEvoError {
    fn from(e: BootstrapError) -> Self {
        EverEvoError::Bootstrap(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtimes_defined() {
        assert_eq!(RUNTIMES.len(), 4); // python, node, git, onnxruntime
        assert_eq!(MODELS.len(), 4); // 2 embeddings + 2 rerankers
    }

    #[test]
    fn test_asset_urls_valid() {
        for asset in RUNTIMES.iter().chain(MODELS.iter()) {
            assert!(
                asset.primary_url.starts_with("https://"),
                "Invalid URL for {}: {}",
                asset.key,
                asset.primary_url
            );
        }
    }

    #[test]
    fn test_python_is_embeddable() {
        let python = RUNTIMES.iter().find(|a| a.key == "python").unwrap();
        assert!(python.primary_url.contains("embed"), "Python must be embeddable version");
    }

    #[test]
    fn test_git_is_mingit() {
        let git = RUNTIMES.iter().find(|a| a.key == "git").unwrap();
        assert!(git.primary_url.contains("MinGit"), "Git must be MinGit portable");
    }

    #[test]
    fn test_models_have_primary_url() {
        for model in MODELS.iter() {
            assert!(!model.primary_url.is_empty(), "Model {} lacks primary URL", model.key);
        }
    }

    #[test]
    fn test_total_download_size() {
        let runtime_size: u64 = RUNTIMES.iter().map(|a| a.size_bytes).sum();
        let model_size: u64 = MODELS.iter().map(|a| a.size_bytes).sum();
        // ~90MB runtimes + ~57MB models = ~147MB
        assert!(runtime_size > 50_000_000, "Runtime estimate too low: {runtime_size}");
        assert!(model_size > 30_000_000, "Model estimate too low: {model_size}");
    }

    #[test]
    fn test_bootstrap_new() {
        let bs = Bootstrap::new(PathBuf::from("./data"));
        assert!(bs.data_dir.ends_with("data"));
    }
}
