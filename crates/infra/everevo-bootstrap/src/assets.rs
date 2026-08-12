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
    pub fn is_model(&self) -> bool {
        matches!(self.kind, AssetKind::Model)
    }
    pub fn is_runtime(&self) -> bool {
        matches!(self.kind, AssetKind::Runtime)
    }
    pub fn is_system_provided(&self) -> bool {
        matches!(self.kind, AssetKind::SystemProvided)
    }

    /// All URLs to try for this asset (primary first, then mirrors).
    pub fn all_urls(&self) -> Vec<&str> {
        if self.is_system_provided() {
            return vec![];
        }
        std::iter::once(self.primary_url.as_str())
            .chain(self.mirror_urls.iter().map(|s| s.as_str()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// Downloaded + extracted from CDN (portable runtime).
    Runtime,
    /// ONNX model files (platform-independent, download only).
    Model,
    /// Expected to exist on system PATH (e.g., git on macOS/Linux).
    /// Not downloaded; checked at startup via `which`.
    SystemProvided,
}
