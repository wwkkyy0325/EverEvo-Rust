// ── Errors ──────────────────────────────────────────────────────────────

use everevo_core::EverEvoError;

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
