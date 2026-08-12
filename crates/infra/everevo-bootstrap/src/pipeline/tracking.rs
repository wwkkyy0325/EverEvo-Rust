//! Layer/depth tracking state machine for pipeline assets.

use std::collections::HashMap;

use tokio::sync::broadcast;

use crate::pipeline::InitEvent;
use crate::Asset;

// ── AssetDepth ───────────────────────────────────────────────────────────

/// How many pipeline layers an asset requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetDepth {
    /// Download only — models (1 layer).
    Shallow,
    /// Download + Extract — runtimes (2 layers).
    Deep,
}

impl AssetDepth {
    pub(crate) fn layer_count(self) -> u8 {
        match self {
            Self::Shallow => 1,
            Self::Deep => 2,
        }
    }

    pub(crate) fn from_asset(asset: &Asset) -> Self {
        if asset.is_runtime() {
            Self::Deep
        } else {
            Self::Shallow
        }
    }
}

// ── LayerTracker ─────────────────────────────────────────────────────────

/// Per-asset progress through the pipeline layers.
///
/// A freshly-created tracker has `layer_units_total = 0` and is **not** done
/// (the `> 0` guard in `is_current_layer_done` prevents `0 >= 0` from being
/// treated as complete).
pub(crate) struct LayerTracker {
    _key: String,
    pub(crate) depth: AssetDepth,
    /// 1-indexed; starts at 1.
    pub(crate) current_layer: u8,
    /// Work-units completed in the current layer.
    pub(crate) layer_units_done: usize,
    /// Total work-units in the current layer (0 = not yet assigned).
    pub(crate) layer_units_total: usize,
}

impl LayerTracker {
    pub(crate) fn new(key: String, depth: AssetDepth) -> Self {
        Self {
            _key: key,
            depth,
            current_layer: 1,
            layer_units_done: 0,
            layer_units_total: 0,
        }
    }

    /// True when every unit of the current layer is finished AND work was
    /// actually assigned (`> 0` guard).
    pub(crate) fn is_current_layer_done(&self) -> bool {
        self.layer_units_total > 0 && self.layer_units_done >= self.layer_units_total
    }

    /// True when all layers are complete.
    pub(crate) fn is_asset_done(&self) -> bool {
        self.current_layer == self.depth.layer_count() && self.is_current_layer_done()
    }

    /// Move to the next layer, resetting unit counters.
    /// Returns false if already at max depth.
    pub(crate) fn advance_layer(&mut self, total_units: usize) -> bool {
        if self.current_layer >= self.depth.layer_count() {
            return false;
        }
        self.current_layer += 1;
        self.layer_units_done = 0;
        self.layer_units_total = total_units;
        true
    }

    pub(crate) fn increment_unit(&mut self) {
        self.layer_units_done += 1;
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Emit `AssetDone` for every tracker that is complete but hasn't emitted yet.
///
/// Call after batch operations (cache pre-check, Phase 4 submission, event-loop
/// completions) to push accurate `completed` / `total` counts to the frontend.
pub(crate) fn emit_pending_asset_dones(
    tx: &broadcast::Sender<InitEvent>,
    trackers: &HashMap<String, LayerTracker>,
    total: usize,
) {
    let all_done: Vec<String> = trackers
        .iter()
        .filter(|(_, t)| t.is_asset_done())
        .map(|(k, _)| k.clone())
        .collect();

    for (i, key) in all_done.iter().enumerate() {
        let _ = tx.send(InitEvent::AssetDone {
            key: key.clone(),
            completed: i + 1,
            total,
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Asset, AssetKind};

    // ── AssetDepth ──────────────────────────────────────────────────

    fn make_asset(key: &str, kind: AssetKind) -> Asset {
        Asset {
            key: key.into(),
            kind,
            version: "v1".into(),
            primary_url: "https://example.com/test.zip".into(),
            mirror_urls: vec![],
            extra_files: vec![],
            sha256: None,
            size_bytes: 1000,
            description: "test asset".into(),
        }
    }

    #[test]
    fn test_asset_depth_from_asset() {
        let runtime = make_asset("python", AssetKind::Runtime);
        let model = make_asset("bge", AssetKind::Model);

        assert_eq!(AssetDepth::from_asset(&runtime), AssetDepth::Deep);
        assert_eq!(AssetDepth::from_asset(&model), AssetDepth::Shallow);
    }

    #[test]
    fn test_asset_depth_layer_count() {
        assert_eq!(AssetDepth::Deep.layer_count(), 2);
        assert_eq!(AssetDepth::Shallow.layer_count(), 1);
    }

    // ── LayerTracker ────────────────────────────────────────────────

    #[test]
    fn test_layer_tracker_new() {
        let t = LayerTracker::new("python".into(), AssetDepth::Deep);
        assert_eq!(t.current_layer, 1);
        assert_eq!(t.layer_units_done, 0);
        assert_eq!(t.layer_units_total, 0);
        assert!(!t.is_current_layer_done()); // 0 >= 0 guard prevents false positive
        assert!(!t.is_asset_done());
    }

    #[test]
    fn test_layer_tracker_shallow_lifecycle() {
        // Model: single layer (download only) with 5 files
        let mut t = LayerTracker::new("bge".into(), AssetDepth::Shallow);
        t.layer_units_total = 5;

        // Progress through units
        assert!(!t.is_current_layer_done());
        for _ in 0..4 {
            t.increment_unit();
        }
        assert!(!t.is_current_layer_done()); // 4/5 done

        t.increment_unit(); // 5/5
        assert!(t.is_current_layer_done());
        assert!(
            t.is_asset_done(),
            "Shallow asset done when layer 1 complete"
        );

        // advance_layer should return false (already at max depth)
        assert!(!t.advance_layer(10));
    }

    #[test]
    fn test_layer_tracker_deep_lifecycle() {
        // Runtime: two layers (download → extract)
        let mut t = LayerTracker::new("python".into(), AssetDepth::Deep);
        t.layer_units_total = 1;

        // Layer 1: download
        t.increment_unit();
        assert!(t.is_current_layer_done());
        assert!(!t.is_asset_done(), "Deep asset not done after layer 1");

        // Advance to layer 2: extract
        assert!(t.advance_layer(1));
        assert_eq!(t.current_layer, 2);
        assert_eq!(t.layer_units_done, 0);
        assert_eq!(t.layer_units_total, 1);
        assert!(!t.is_current_layer_done()); // layer 2 not yet done

        t.increment_unit();
        assert!(t.is_current_layer_done());
        assert!(t.is_asset_done(), "Deep asset done after layer 2");

        // advance_layer at max depth returns false
        assert!(!t.advance_layer(1));
    }

    #[test]
    fn test_layer_tracker_advance_resets_counters() {
        let mut t = LayerTracker::new("node".into(), AssetDepth::Deep);
        t.layer_units_total = 3;
        t.layer_units_done = 3;
        assert!(t.is_current_layer_done());

        assert!(t.advance_layer(1));
        assert_eq!(t.current_layer, 2);
        assert_eq!(t.layer_units_done, 0);
        assert_eq!(t.layer_units_total, 1);
    }

    #[test]
    fn test_layer_tracker_no_guard_bypass() {
        // layer_units_total == 0 → is_current_layer_done must return false
        // even though layer_units_done (0) >= layer_units_total (0)
        let t = LayerTracker::new("test".into(), AssetDepth::Shallow);
        assert!(
            !t.is_current_layer_done(),
            "Guard: 0 >= 0 must not trigger done when no work was assigned"
        );
    }

    // ── emit_pending_asset_dones ────────────────────────────────────

    #[test]
    fn test_emit_empty_trackers() {
        let (tx, mut rx) = broadcast::channel(8);
        let trackers: HashMap<String, LayerTracker> = HashMap::new();
        emit_pending_asset_dones(&tx, &trackers, 0);
        // No events emitted — receiver gets nothing
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_emit_single_done() {
        let (tx, mut rx) = broadcast::channel(8);
        let mut trackers = HashMap::new();
        let mut t = LayerTracker::new("bge".into(), AssetDepth::Shallow);
        t.layer_units_total = 1;
        t.increment_unit();
        assert!(t.is_asset_done());
        trackers.insert("bge".into(), t);

        emit_pending_asset_dones(&tx, &trackers, 3);

        let event = rx.try_recv().unwrap();
        match event {
            InitEvent::AssetDone {
                key,
                completed,
                total,
            } => {
                assert_eq!(key, "bge");
                assert_eq!(completed, 1);
                assert_eq!(total, 3);
            }
            other => panic!("expected AssetDone, got {other:?}"),
        }
    }

    #[test]
    fn test_emit_only_done_trackers() {
        let (tx, mut rx) = broadcast::channel(8);
        let mut trackers = HashMap::new();

        // Python (Deep): layer 1 done but not layer 2 → not asset-done
        let mut py = LayerTracker::new("python".into(), AssetDepth::Deep);
        py.layer_units_total = 1;
        py.increment_unit(); // layer 1 done
        trackers.insert("python".into(), py);

        // BGE (Shallow): all done
        let mut bge = LayerTracker::new("bge".into(), AssetDepth::Shallow);
        bge.layer_units_total = 1;
        bge.increment_unit();
        trackers.insert("bge".into(), bge);

        emit_pending_asset_dones(&tx, &trackers, 5);

        // Only bge should be emitted (1 event), python not yet done
        let event = rx.try_recv().unwrap();
        match event {
            InitEvent::AssetDone { key, .. } => assert_eq!(key, "bge"),
            other => panic!("expected AssetDone for bge, got {other:?}"),
        }
        // No second event
        assert!(rx.try_recv().is_err());
    }
}
