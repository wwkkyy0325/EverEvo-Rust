//! Pipeline event types (`InitEvent`) and their tag-based JSON serialization.

use serde::Serialize;

// ── InitEvent ────────────────────────────────────────────────────────────
//
// All events use `#[serde(tag = "type")]` so the JSON carries a `"type"` field
// that the frontend switches on.  Event names are backward-compatible with the
// existing `BootstrapView.tsx` SSE listener.

/// Events emitted by the init pipeline on its broadcast channel.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InitEvent {
    /// Pipeline started checking asset state.
    Checking,

    /// Bootstrap check complete; some assets are missing.
    FoundMissing { total: usize, total_bytes: u64 },

    /// Per-file download progress for an asset.
    DownloadProgress {
        key: String,
        /// 0.0–100.0
        percentage: f32,
        /// Transfer speed in MB/s (one decimal).
        speed_mb: f64,
    },

    /// An asset is entering a new pipeline layer.
    LayerStart {
        key: String,
        /// 1 = download, 2 = extract.
        layer: u8,
        /// Human-readable: "download" | "extract".
        layer_name: String,
    },

    /// An asset's current layer is complete.
    /// `is_asset_done` is true when this was the final layer.
    LayerDone {
        key: String,
        layer: u8,
        /// 1 (Shallow) or 2 (Deep).
        total_layers: u8,
        is_asset_done: bool,
    },

    /// An asset is fully provisioned (all layers).
    AssetDone {
        key: String,
        completed: usize,
        total: usize,
    },

    /// An asset failed at the given layer.
    AssetFailed {
        key: String,
        layer: u8,
        error: String,
    },

    /// All assets ready; `.everevo_init` marker written.
    AllDone,

    /// Unrecoverable pipeline error.
    FatalError { error: String },
}

// ── InitError ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Bootstrap check failed: {0}")]
    Bootstrap(#[from] crate::BootstrapError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── InitEvent serialization (tag-based JSON) ────────────────────

    #[test]
    fn test_init_event_json_tag() {
        let event = InitEvent::Checking;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "Checking");

        let event = InitEvent::AllDone;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "AllDone");
    }

    #[test]
    fn test_init_event_json_found_missing() {
        let event = InitEvent::FoundMissing {
            total: 3,
            total_bytes: 150_000_000,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "FoundMissing");
        assert_eq!(json["total"], 3);
        assert_eq!(json["total_bytes"], 150_000_000);
    }

    #[test]
    fn test_init_event_json_download_progress() {
        let event = InitEvent::DownloadProgress {
            key: "python".into(),
            percentage: 45.5,
            speed_mb: 2.3,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "DownloadProgress");
        assert_eq!(json["key"], "python");
        assert_eq!(json["percentage"], 45.5);
    }

    #[test]
    fn test_init_event_json_fatal_error() {
        let event = InitEvent::FatalError {
            error: "disk full".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "FatalError");
        assert_eq!(json["error"], "disk full");
    }
}
