//! InitPipeline — event-driven bootstrap orchestration with layer-aware depth tracking.
//!
//! ## Pipeline depth
//!
//! Not every asset needs extraction. The pipeline models each asset's required
//! layers explicitly so that completion checks are gated on the correct layer count:
//!
//! ```text
//! Runtime (depth=2, Deep):
//!   Layer 1 [download] → ZIP on disk
//!   Layer 2 [extract]  → unzipped to data/runtime/{key}/
//!   ✅ layer 2 done = asset done
//!
//! Model (depth=1, Shallow):
//!   Layer 1 [download] → model_quantized.onnx + tokenizer.json + config.json
//!   ✅ layer 1 done = asset done
//! ```
//!
//! ## Architecture
//!
//! ```text
//! InitPipeline::run()
//!   ├─ Phase 1: marker check (fast path — skip if .everevo_init exists)
//!   ├─ Phase 2: Bootstrap::check() — discover missing assets
//!   ├─ Phase 3: cache pre-check — already-downloaded ZIPs / model files
//!   ├─ Phase 4: submit DownloadTasks to Downloader
//!   ├─ Phase 5: event loop — tokio::select! over downloader events + extraction results
//!   └─ Phase 6: finalize — await extract handles, sweep, write marker
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, Mutex};

use everevo_downloader::observer::DownloadEvent;
use everevo_downloader::task::{DownloadTask, Priority};
use everevo_downloader::Downloader;

use crate::runtime::RuntimeManager;
use crate::{Asset, Bootstrap};

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

// ── AssetDepth ───────────────────────────────────────────────────────────

/// How many pipeline layers an asset requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetDepth {
    /// Download only — models (1 layer).
    Shallow,
    /// Download + Extract — runtimes (2 layers).
    Deep,
}

impl AssetDepth {
    fn layer_count(self) -> u8 {
        match self {
            Self::Shallow => 1,
            Self::Deep => 2,
        }
    }

    fn from_asset(asset: &Asset) -> Self {
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
struct LayerTracker {
    _key: String,
    depth: AssetDepth,
    /// 1-indexed; starts at 1.
    current_layer: u8,
    /// Work-units completed in the current layer.
    layer_units_done: usize,
    /// Total work-units in the current layer (0 = not yet assigned).
    layer_units_total: usize,
}

impl LayerTracker {
    fn new(key: String, depth: AssetDepth) -> Self {
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
    fn is_current_layer_done(&self) -> bool {
        self.layer_units_total > 0 && self.layer_units_done >= self.layer_units_total
    }

    /// True when all layers are complete.
    fn is_asset_done(&self) -> bool {
        self.current_layer == self.depth.layer_count() && self.is_current_layer_done()
    }

    /// Move to the next layer, resetting unit counters.
    /// Returns false if already at max depth.
    fn advance_layer(&mut self, total_units: usize) -> bool {
        if self.current_layer >= self.depth.layer_count() {
            return false;
        }
        self.current_layer += 1;
        self.layer_units_done = 0;
        self.layer_units_total = total_units;
        true
    }

    fn increment_unit(&mut self) {
        self.layer_units_done += 1;
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

struct ExtractResult {
    key: String,
    success: bool,
    error: Option<String>,
}

fn truncate_error(s: &str) -> String {
    if s.len() <= 200 {
        s.to_string()
    } else {
        format!("{}...", &s[..197])
    }
}

// ── InitError ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Bootstrap check failed: {0}")]
    Bootstrap(#[from] crate::BootstrapError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

// ── InitPipeline ─────────────────────────────────────────────────────────

/// The bootstrap pipeline orchestrator.
///
/// Spawned via `tokio::spawn`; consumers observe progress through the
/// broadcast channel returned by [`events()`](InitPipeline::events).
pub struct InitPipeline {
    data_dir: PathBuf,
    bootstrap: Arc<Bootstrap>,
    downloader: Arc<Downloader>,
    runtime_mgr: RuntimeManager,
    event_tx: broadcast::Sender<InitEvent>,
    marker_path: PathBuf,
    /// Ensures only one pipeline runs at a time (startup + SSE route).
    run_lock: Mutex<()>,
    /// Directory containing bundled `.tar.zst` assets (Tauri resources).
    /// Empty = no bundled assets; fall back to download path.
    resource_dir: PathBuf,
}

impl InitPipeline {
    /// Create a new pipeline.
    ///
    /// The broadcast channel uses 256 capacity, matching the downloader's
    /// own event channel.
    pub fn new(
        data_dir: PathBuf,
        bootstrap: Arc<Bootstrap>,
        downloader: Arc<Downloader>,
        resource_dir: PathBuf,
    ) -> Self {
        let runtime_mgr = RuntimeManager::new(&data_dir);
        let marker_path = data_dir.join(".everevo_init");
        let (event_tx, _) = broadcast::channel(256);
        Self {
            data_dir,
            bootstrap,
            downloader,
            runtime_mgr,
            event_tx,
            marker_path,
            run_lock: Mutex::new(()),
            resource_dir,
        }
    }

    /// Subscribe to pipeline events.  Call **before** spawning `run()` so
    /// no event (including `AllDone` from the marker fast-path) is missed.
    pub fn events(&self) -> broadcast::Receiver<InitEvent> {
        self.event_tx.subscribe()
    }

    /// Synchronous marker check.
    pub fn is_initialized(&self) -> bool {
        self.marker_path.exists()
    }

    /// Path to `data/.everevo_init`.
    pub fn marker_path(&self) -> &Path {
        &self.marker_path
    }

    // ── emit helper ────────────────────────────────────────────────

    fn emit(&self, event: InitEvent) {
        let _ = self.event_tx.send(event);
    }

    // ── run ────────────────────────────────────────────────────────

    /// Execute the full provisioning pipeline.
    ///
    /// Safe to call even when already initialized — Phase 1 returns
    /// immediately with `AllDone`.
    pub async fn run(&self) -> Result<(), InitError> {
        // Prevent concurrent runs (startup background + SSE route).
        let _guard = self.run_lock.lock().await;

        // ── Phase 1: Verify completeness (marker + actual files) ──
        self.bootstrap.invalidate().await;
        let check = self.bootstrap.check().await?;

        if check.missing.is_empty() && check.corrupt.is_empty() {
            if !self.marker_path.exists() {
                self.write_marker().await?;
            }
            self.emit(InitEvent::AllDone);
            return Ok(());
        }

        // Marker exists but assets are incomplete (e.g. new extra_files added).
        // Remove the marker so we re-provision what's missing.
        if self.marker_path.exists() {
            tracing::info!(
                missing = check.missing.len(),
                "Init marker found but assets incomplete — re-provisioning"
            );
            let _ = tokio::fs::remove_file(&self.marker_path).await;
        }

        // ── Phase 1.5: Try bundled assets first (local extraction, seconds) ──
        if !self.resource_dir.as_os_str().is_empty() {
            let extractor =
                crate::resource_extractor::ResourceExtractor::new(&self.resource_dir, &self.data_dir);
            if extractor.has_bundled_assets() {
                tracing::info!(
                    dir = %self.resource_dir.display(),
                    "Bundled assets detected — extracting from bundle"
                );
                self.emit(InitEvent::Checking);
                match extractor.extract_all(&self.event_tx).await {
                    Ok(_) => {
                        self.write_marker().await?;
                        self.emit(InitEvent::AllDone);
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Bundle extraction failed — falling back to downloader");
                        // Fall through to Phase 2 (download path)
                    }
                }
            } else {
                tracing::info!(
                    dir = %self.resource_dir.display(),
                    "No bundled assets found in resource dir — will download"
                );
            }
        }

        // ── Phase 2: Bootstrap check done, assets missing ───────

        let total_assets = check.missing.len();
        self.emit(InitEvent::FoundMissing {
            total: total_assets,
            total_bytes: check.download_size_bytes,
        });

        // Build asset lookup map once.
        let asset_map: HashMap<String, &Asset> =
            check.missing.iter().map(|a| (a.key.clone(), a)).collect();

        let mut trackers: HashMap<String, LayerTracker> = check
            .missing
            .iter()
            .map(|a| {
                let depth = AssetDepth::from_asset(a);
                (a.key.clone(), LayerTracker::new(a.key.clone(), depth))
            })
            .collect();

        let temp_dir = self.data_dir.join("downloads");
        let _ = tokio::fs::create_dir_all(&temp_dir).await;

        // ── Phase 3: Cache pre-check ──────────────────────────
        let mut to_download: Vec<&Asset> = Vec::new();

        for asset in &check.missing {
            let Some(tracker) = trackers.get_mut(&asset.key) else {
                tracing::warn!(key = %asset.key, "Asset in missing list but not in trackers — skipping");
                continue;
            };

            if asset.is_model() {
                let target = self.runtime_mgr.models_dir().join(&asset.key);
                let model_ok = target.join("model_quantized.onnx").exists();
                let extra_ok = asset
                    .extra_files
                    .iter()
                    .all(|ef| target.join(&ef.filename).exists());
                if model_ok && extra_ok {
                    let sentinel = target.join(".extracted");
                    let _ = tokio::fs::write(&sentinel, &asset.version).await;
                    let _ = self.runtime_mgr.update_manifest(asset).await;
                    self.bootstrap.invalidate().await;

                    tracker.layer_units_total = 1;
                    tracker.layer_units_done = 1;
                    let tl = tracker.depth.layer_count();
                    self.emit(InitEvent::LayerStart {
                        key: asset.key.clone(),
                        layer: 1,
                        layer_name: "download".into(),
                    });
                    self.emit(InitEvent::LayerDone {
                        key: asset.key.clone(),
                        layer: 1,
                        total_layers: tl,
                        is_asset_done: true,
                    });
                } else {
                    to_download.push(asset);
                }
            } else {
                // Runtime — check for cached ZIP
                if let Some(cached_zip) = self.runtime_mgr.find_cached_zip(asset) {
                    tracing::info!(key = %asset.key, "Found cached ZIP, extracting…");
                    match self.runtime_mgr.install(&cached_zip, asset).await {
                        Ok(_) => {
                            let _ = self.runtime_mgr.update_manifest(asset).await;
                            self.bootstrap.invalidate().await;

                            tracker.layer_units_total = 1;
                            tracker.layer_units_done = 1;
                            tracker.advance_layer(1);
                            tracker.layer_units_done = 1;
                            let tl = tracker.depth.layer_count();
                            self.emit(InitEvent::LayerStart {
                                key: asset.key.clone(),
                                layer: 1,
                                layer_name: "download".into(),
                            });
                            self.emit(InitEvent::LayerDone {
                                key: asset.key.clone(),
                                layer: 1,
                                total_layers: tl,
                                is_asset_done: false,
                            });
                            self.emit(InitEvent::LayerStart {
                                key: asset.key.clone(),
                                layer: 2,
                                layer_name: "extract".into(),
                            });
                            self.emit(InitEvent::LayerDone {
                                key: asset.key.clone(),
                                layer: 2,
                                total_layers: tl,
                                is_asset_done: true,
                            });
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(key = %asset.key, error = %e, "Cached ZIP failed, re-downloading");
                            let _ = tokio::fs::remove_file(&cached_zip).await;
                        }
                    }
                }
                to_download.push(asset);
            }
        }

        // Emit AssetDone for every asset completed from cache.
        {
            let _done_count = trackers.values().filter(|t| t.is_asset_done()).count();
            let mut emitted = 0usize;
            for (key, t) in &trackers {
                if t.is_asset_done() {
                    emitted += 1;
                    self.emit(InitEvent::AssetDone {
                        key: key.clone(),
                        completed: emitted,
                        total: total_assets,
                    });
                }
            }
            if emitted == total_assets {
                self.write_marker().await?;
                self.emit(InitEvent::AllDone);
                return Ok(());
            }
        }

        // Subscribe BEFORE submitting — if we wait until Phase 5, small
        // files (tokenizer.json ~1 MB) may complete before we're listening.
        let mut dl_events = self.downloader.events();

        // ── Phase 4: Submit downloads ─────────────────────────
        let mut task_map: HashMap<String, String> = HashMap::new(); // task_id → asset_key
                                                                    // URL fallback state — when a mirror fails, try the next URL from the Asset.
        let mut url_lists: HashMap<String, Vec<String>> = HashMap::new(); // asset_key → [urls]
        let mut url_index: HashMap<String, usize> = HashMap::new(); // asset_key → current index

        for asset in &to_download {
            let Some(tracker) = trackers.get_mut(&asset.key) else {
                tracing::warn!(key = %asset.key, "Asset in download list but not in trackers — skipping");
                continue;
            };
            let urls = asset.all_urls();

            self.emit(InitEvent::LayerStart {
                key: asset.key.clone(),
                layer: 1,
                layer_name: "download".into(),
            });

            if asset.is_model() {
                let target = self.runtime_mgr.models_dir().join(&asset.key);
                let _ = tokio::fs::create_dir_all(&target).await;
                let total_files = 1 + asset.extra_files.len();
                tracker.layer_units_total = total_files;

                // Main model file
                let model_dest = target.join("model_quantized.onnx");
                if model_dest.exists()
                    && model_dest.metadata().ok().map(|m| m.len()).unwrap_or(0) > 0
                {
                    tracker.increment_unit();
                } else if let Some(url) = urls.first().copied() {
                    let task = DownloadTask::new(url, &model_dest).with_priority(Priority::High);
                    task_map.insert(task.id.clone(), asset.key.clone());
                    let _ = self.downloader.submit(task).await;
                }

                // Extra files
                for extra in &asset.extra_files {
                    let extra_dest = target.join(&extra.filename);
                    if extra_dest.exists()
                        && extra_dest.metadata().ok().map(|m| m.len()).unwrap_or(0) > 0
                    {
                        tracker.increment_unit();
                    } else {
                        let task = DownloadTask::new(extra.url.as_str(), &extra_dest)
                            .with_priority(Priority::High);
                        task_map.insert(task.id.clone(), asset.key.clone());
                        let _ = self.downloader.submit(task).await;
                    }
                }

                // All files already on disk → done immediately.
                if tracker.is_current_layer_done() {
                    let sentinel = target.join(".extracted");
                    let _ = tokio::fs::write(&sentinel, &asset.version).await;
                    let _ = self.runtime_mgr.update_manifest(asset).await;
                    self.bootstrap.invalidate().await;

                    let tl = tracker.depth.layer_count();
                    self.emit(InitEvent::LayerDone {
                        key: asset.key.clone(),
                        layer: 1,
                        total_layers: tl,
                        is_asset_done: true,
                    });
                }
            } else {
                // Runtime: single ZIP — try primary URL first, fallback to mirrors.
                let zip_dest = temp_dir.join(format!("{}.zip", asset.key));
                tracker.layer_units_total = 1;

                let url_vec: Vec<String> = urls.iter().map(|s| s.to_string()).collect();
                if !url_vec.is_empty() {
                    url_lists.insert(asset.key.clone(), url_vec);
                    url_index.insert(asset.key.clone(), 0);
                    let task = DownloadTask::new(urls[0], &zip_dest).with_priority(Priority::High);
                    task_map.insert(task.id.clone(), asset.key.clone());
                    let _ = self.downloader.submit(task).await;
                }
            }
        }

        // Count how many assets still need work (not done yet).
        let mut need_count = trackers.values().filter(|t| !t.is_asset_done()).count();

        // Emit AssetDone for any model that was fully resolved in Phase 4.
        emit_pending_asset_dones(&self.event_tx, &trackers, total_assets);

        // ── Phase 5: Event loop ────────────────────────────────
        let (extract_tx, mut extract_rx) = mpsc::unbounded_channel::<ExtractResult>();
        let mut extract_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        let loop_start = std::time::Instant::now();
        let max_duration = std::time::Duration::from_secs(900); // 15 min

        while need_count > 0 && loop_start.elapsed() < max_duration {
            tokio::select! {
                // ── Downloader events ──────────────────────────
                dl_event = dl_events.recv() => {
                    match dl_event {
                        Ok(DownloadEvent::Progress { task_id, progress }) => {
                            if let Some(key) = task_map.get(&task_id) {
                                self.emit(InitEvent::DownloadProgress {
                                    key: key.clone(),
                                    percentage: progress.percentage,
                                    speed_mb: (progress.speed_bytes / 1_048_576.0 * 10.0).round() / 10.0,
                                });
                            }
                        }
                        Ok(DownloadEvent::Completed { task_id, path, .. }) => {
                            let Some(key) = task_map.get(&task_id).cloned() else {
                                tracing::warn!(%task_id, "Completed event: task_id not in task_map");
                                continue;
                            };
                            let Some(asset) = asset_map.get(&key).copied() else { continue };
                            let Some(tracker) = trackers.get_mut(&key) else { continue };

                            tracker.increment_unit();
                            tracing::info!(
                                %key,
                                file = %path,
                                units = %format!("{}/{}", tracker.layer_units_done, tracker.layer_units_total),
                                layer = tracker.current_layer,
                                depth = ?tracker.depth,
                                "Download unit completed"
                            );

                            if tracker.is_current_layer_done() {
                                let tl = tracker.depth.layer_count();
                                let layer = tracker.current_layer;

                                if asset.is_runtime() && layer == 1 {
                                    // Layer 1 done → advance to layer 2 (extract)
                                    self.emit(InitEvent::LayerDone {
                                        key: key.clone(), layer: 1, total_layers: tl,
                                        is_asset_done: false,
                                    });
                                    self.emit(InitEvent::LayerStart {
                                        key: key.clone(), layer: 2,
                                        layer_name: "extract".into(),
                                    });

                                    let zip_path = PathBuf::from(&path);
                                    let asset_c = asset.clone();
                                    let mgr = self.runtime_mgr.clone();
                                    let bs = self.bootstrap.clone();
                                    let tx = extract_tx.clone();
                                    let key_c = key.clone();

                                    let handle = tokio::spawn(async move {
                                        let result = mgr.install(&zip_path, &asset_c).await;
                                        match result {
                                            Ok(_) => {
                                                let _ = mgr.update_manifest(&asset_c).await;
                                                bs.invalidate().await;
                                                let _ = tx.send(ExtractResult {
                                                    key: key_c, success: true, error: None,
                                                });
                                            }
                                            Err(e) => {
                                                let _ = tx.send(ExtractResult {
                                                    key: key_c, success: false,
                                                    error: Some(e.to_string()),
                                                });
                                            }
                                        }
                                    });
                                    extract_handles.push(handle);
                                } else if asset.is_model() {
                                    // Model: layer 1 = final layer
                                    let target = self.runtime_mgr.models_dir().join(&asset.key);
                                    let _ = tokio::fs::write(
                                        target.join(".extracted"), &asset.version,
                                    ).await;
                                    let _ = self.runtime_mgr.update_manifest(asset).await;
                                    self.bootstrap.invalidate().await;

                                    self.emit(InitEvent::LayerDone {
                                        key: key.clone(), layer: 1, total_layers: tl,
                                        is_asset_done: true,
                                    });
                                    need_count = need_count.saturating_sub(1);
                                    emit_pending_asset_dones(
                                        &self.event_tx, &trackers, total_assets,
                                    );
                                }
                            }
                        }
                        Ok(DownloadEvent::Failed { task_id, error, .. }) => {
                            let key = task_map.get(&task_id).cloned().unwrap_or_default();
                            if key.is_empty() { continue; }
                            // Try the next mirror URL from the Asset, if any.
                            if let Some(idx) = url_index.get_mut(&key) {
                                *idx += 1;
                                if let Some(urls) = url_lists.get(&key) {
                                    if *idx < urls.len() {
                                        tracing::info!(%key, url = %urls[*idx], attempt = *idx + 1, "Retrying with next mirror");
                                        let zip_dest = temp_dir.join(format!("{}.zip", key));
                                        let task = DownloadTask::new(urls[*idx].as_str(), &zip_dest)
                                            .with_priority(Priority::High);
                                        task_map.insert(task.id.clone(), key.clone());
                                        let _ = self.downloader.submit(task).await;
                                        continue;
                                    }
                                }
                            }
                            // All URLs exhausted.
                            self.emit(InitEvent::AssetFailed {
                                key, layer: 1, error: truncate_error(&error),
                            });
                        }
                        Ok(DownloadEvent::MirrorSwitched { task_id, from_mirror, to_mirror, .. }) => {
                            let key = task_map.get(&task_id).cloned().unwrap_or_default();
                            tracing::info!(%key, %from_mirror, %to_mirror, "Mirror switched");
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "Pipeline event lag");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        _ => {} // TaskQueued, ResolvingMirror, ChunkDone, etc.
                    }
                }

                // ── Extraction completions ──────────────────────
                Some(extract) = extract_rx.recv() => {
                    if extract.success {
                        if let Some(tracker) = trackers.get_mut(&extract.key) {
                            tracker.advance_layer(1);
                            tracker.layer_units_done = 1;
                            let tl = tracker.depth.layer_count();
                            need_count = need_count.saturating_sub(1);

                            self.emit(InitEvent::LayerDone {
                                key: extract.key.clone(), layer: 2, total_layers: tl,
                                is_asset_done: true,
                            });
                            emit_pending_asset_dones(
                                &self.event_tx, &trackers, total_assets,
                            );
                        }
                    } else {
                        // Extraction failed (likely corrupt ZIP). Try next mirror URL.
                        let key = &extract.key;
                        let mut retried = false;
                        if let Some(idx) = url_index.get_mut(key) {
                            *idx += 1;
                            if let Some(urls) = url_lists.get(key) {
                                if *idx < urls.len() {
                                    tracing::info!(%key, url = %urls[*idx], attempt = *idx + 1, "Extract failed, retrying with next mirror");
                                    // Delete the corrupt zip so the new download writes fresh data.
                                    let zip_dest = temp_dir.join(format!("{}.zip", key));
                                    let _ = tokio::fs::remove_file(&zip_dest).await;
                                    let task = DownloadTask::new(urls[*idx].as_str(), &zip_dest)
                                        .with_priority(Priority::High);
                                    task_map.insert(task.id.clone(), key.clone());
                                    let _ = self.downloader.submit(task).await;
                                    retried = true;
                                }
                            }
                        }
                        if !retried {
                            self.emit(InitEvent::AssetFailed {
                                key: extract.key.clone(),
                                layer: 2,
                                error: extract.error.unwrap_or_else(|| "unknown".into()),
                            });
                            need_count = need_count.saturating_sub(1);
                        }
                    }
                }

                // ── 30-second timeout safety net ────────────────
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                    let recheck = match self.bootstrap.check().await {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    if recheck.missing.is_empty() && recheck.corrupt.is_empty() {
                        tracing::info!("All assets ready (recheck) — breaking event loop");
                        break;
                    }
                }
            }
        }

        // ── Phase 6: Finalize ──────────────────────────────────
        for handle in extract_handles {
            let _ = handle.await;
        }

        // Drain remaining extraction results
        while let Ok(extract) = extract_rx.try_recv() {
            if extract.success {
                if let Some(tracker) = trackers.get_mut(&extract.key) {
                    if !tracker.is_asset_done() {
                        tracker.advance_layer(1);
                        tracker.layer_units_done = 1;
                        let tl = tracker.depth.layer_count();
                        self.emit(InitEvent::LayerDone {
                            key: extract.key.clone(),
                            layer: 2,
                            total_layers: tl,
                            is_asset_done: true,
                        });
                    }
                }
            }
        }
        emit_pending_asset_dones(&self.event_tx, &trackers, total_assets);

        let final_check = self.bootstrap.check().await?;
        if final_check.missing.is_empty() && final_check.corrupt.is_empty() {
            self.write_marker().await?;
            self.emit(InitEvent::AllDone);
        } else {
            let failed: Vec<String> = final_check.missing.iter().map(|a| a.key.clone()).collect();
            self.emit(InitEvent::FatalError {
                error: format!(
                    "{} assets could not be provisioned: {}",
                    failed.len(),
                    failed.join(", ")
                ),
            });
        }

        Ok(())
    }

    async fn write_marker(&self) -> Result<(), InitError> {
        if let Some(parent) = self.marker_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.marker_path, b"everevo_init\n").await?;
        tracing::info!(path = %self.marker_path.display(), "Init marker written");
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Emit `AssetDone` for every tracker that is complete but hasn't emitted yet.
///
/// Call after batch operations (cache pre-check, Phase 4 submission, event-loop
/// completions) to push accurate `completed` / `total` counts to the frontend.
fn emit_pending_asset_dones(
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

    // ── truncate_error ──────────────────────────────────────────────

    #[test]
    fn test_truncate_error_short() {
        assert_eq!(truncate_error("hi"), "hi");
    }

    #[test]
    fn test_truncate_error_boundary() {
        let exact = "a".repeat(200);
        assert_eq!(truncate_error(&exact), exact);
    }

    #[test]
    fn test_truncate_error_long() {
        let long = "a".repeat(300);
        let truncated = truncate_error(&long);
        assert_eq!(truncated.len(), 200); // "..." takes 3 chars, so 197 + "..." = 200
        assert!(truncated.ends_with("..."));
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
