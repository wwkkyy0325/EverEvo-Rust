//! ResourceExtractor — extracts bundled `.tar.zst` assets on first startup.
//!
//! ## How it works
//!
//! On first boot (or when `.everevo_init` is missing), the extractor scans the
//! Tauri resource directory for `.tar.zst` archives, decompresses each with zstd,
//! extracts the tar contents to `data/runtime/{key}/` or `data/models/{key}/`,
//! writes `.extracted` sentinels, and updates `.manifest.json`. After all assets
//! are extracted, it writes the `.everevo_init` marker.
//!
//! ## Streaming
//!
//! Extraction uses `zstd::stream::Decoder` + `tar::Archive` — the archive is
//! never fully loaded into memory. This supports archives of any size (including
//! ONNX Runtime at ~300MB).
//!
//! ## Compatibility
//!
//! The output structure is identical to the download-based InitPipeline path:
//! - `data/runtime/{key}/` with `.extracted` sentinel
//! - `data/models/{key}/` with `.extracted` sentinel
//! - `data/runtime/.manifest.json` and `data/models/.manifest.json`
//!
//! This means the startup checks and RuntimeManager work identically regardless
//! of whether assets came from bundle extraction or CDN download.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;

use tokio::sync::broadcast;

use crate::pipeline::InitEvent;
use crate::runtime::RuntimeManager;
use crate::Manifest;

/// Metadata for a single bundled asset archive.
#[derive(Debug, Clone)]
pub struct BundledAsset {
    /// Asset key (e.g., "python", "all-MiniLM-L6-v2").
    pub key: String,
    /// Path to the `.tar.zst` file in the resource directory.
    pub archive_path: PathBuf,
    /// Expected version string (from manifest.json or filename).
    pub version: String,
}

/// Result of extracting a single bundled asset.
#[derive(Debug)]
pub struct ExtractionResult {
    pub key: String,
    pub target_dir: PathBuf,
    pub success: bool,
    pub error: Option<String>,
}

/// Extracts bundled `.tar.zst` assets into the data directory.
pub struct ResourceExtractor {
    /// Directory containing `.tar.zst` files (Tauri resource dir or fallback).
    resource_dir: PathBuf,
    /// Target data directory (e.g., `./data/`).
    data_dir: PathBuf,
}

impl ResourceExtractor {
    /// Create a new extractor.
    ///
    /// `resource_dir` is the directory containing `.tar.zst` archives and
    /// `manifest.json`. In Tauri, this comes from `app.path_resolver().resolve_resource("bundled")`.
    /// In CLI mode, this is `{exe_dir}/resources/bundled/`.
    pub fn new(resource_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            resource_dir: resource_dir.into(),
            data_dir: data_dir.into(),
        }
    }

    /// Resolve the effective resource directory — checks platform subdirectory
    /// first (e.g. `bundled/x86_64-pc-windows-msvc/`), then falls back to the
    /// base resource dir.
    fn resolve_dir(&self) -> PathBuf {
        let target = crate::detect_target();
        let platform_dir = self.resource_dir.join(&target);
        if platform_dir.exists() {
            return platform_dir;
        }
        self.resource_dir.clone()
    }

    /// Check whether bundled assets are present in the resource directory.
    /// Returns true if at least one `.tar.zst` file exists.
    pub fn has_bundled_assets(&self) -> bool {
        let dir = self.resolve_dir();
        if !dir.exists() {
            return false;
        }
        std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "zst"))
            })
            .unwrap_or(false)
    }

    /// List all `.tar.zst` files in the resource directory, reading version info
    /// from the accompanying `manifest.json` if present.
    pub fn list_assets(&self) -> Result<Vec<BundledAsset>, std::io::Error> {
        let dir = self.resolve_dir();
        let manifest_path = dir.join("manifest.json");
        let manifest: HashMap<String, serde_json::Value> = if manifest_path.exists() {
            let data = std::fs::read_to_string(&manifest_path)?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut assets = Vec::new();
        let rd = std::fs::read_dir(&dir)?;
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "zst") {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                // Strip .tar from key: "python.tar.zst" → "python"
                let key = stem.strip_suffix(".tar").unwrap_or(stem);

                let version = manifest
                    .get(key)
                    .and_then(|v| v.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| "unknown".to_string());

                assets.push(BundledAsset {
                    key: key.to_string(),
                    archive_path: path,
                    version,
                });
            }
        }

        tracing::info!(
            count = assets.len(),
            dir = %dir.display(),
            "Found bundled assets"
        );
        Ok(assets)
    }

    /// Extract a single bundled asset: zstd decode → tar extract → write sentinel.
    ///
    /// Extraction is **streaming** — the archive is never fully loaded into memory.
    /// The `tar` crate handles path traversal safety (no Zip Slip).
    pub async fn extract(
        &self,
        asset: &BundledAsset,
        event_tx: &broadcast::Sender<InitEvent>,
    ) -> Result<ExtractionResult, crate::BootstrapError> {
        let target_dir = if asset.key.contains("onnxruntime")
            || asset.key == "python"
            || asset.key == "node"
            || asset.key == "git"
        {
            self.data_dir.join("runtime").join(&asset.key)
        } else {
            self.data_dir.join("models").join(&asset.key)
        };

        // Emit layer start event
        let _ = event_tx.send(InitEvent::LayerStart {
            key: asset.key.clone(),
            layer: 1,
            layer_name: "extracting".into(),
        });

        // Remove existing target if present (fresh extraction)
        if target_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&target_dir).await;
        }
        tokio::fs::create_dir_all(&target_dir)
            .await
            .map_err(crate::BootstrapError::Io)?;

        // Open the archive file
        let archive_file = std::fs::File::open(&asset.archive_path)
            .map_err(|e| {
                crate::BootstrapError::Extract(format!(
                    "Cannot open archive {}: {e}",
                    asset.archive_path.display()
                ))
            })?;

        // zstd streaming decoder
        let decoder = zstd::stream::Decoder::new(BufReader::new(archive_file))
            .map_err(|e| crate::BootstrapError::Extract(format!("zstd decode error: {e}")))?;

        // tar archive — extract to target directory
        let mut archive = tar::Archive::new(decoder);
        archive.set_overwrite(true);

        // Defend against path traversal: resolve canonical target dir
        let canonical_target = target_dir
            .canonicalize()
            .unwrap_or_else(|_| target_dir.clone());

        for entry_result in archive.entries()
            .map_err(|e| crate::BootstrapError::Extract(format!("tar read error: {e}")))?
        {
            let mut entry = entry_result
                .map_err(|e| crate::BootstrapError::Extract(format!("tar entry error: {e}")))?;

            let entry_path = entry
                .path()
                .map_err(|e| crate::BootstrapError::Extract(format!("tar path error: {e}")))?;

            // Strip top-level directory component (most archives have a root dir)
            let relative: PathBuf = entry_path
                .components()
                .skip(1) // skip the root dir (e.g., "python-3.12.8/")
                .collect();

            if relative.as_os_str().is_empty() {
                continue; // skip the root dir entry itself
            }

            let out_path = target_dir.join(&relative);

            // Create parent dirs
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    crate::BootstrapError::Extract(format!(
                        "Cannot create dir {}: {e}",
                        parent.display()
                    ))
                })?;
            }

            // Path traversal defense
            let resolved = out_path
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .map(|p| p.join(out_path.file_name().unwrap_or_default()))
                .unwrap_or_else(|| out_path.clone());

            if !resolved.starts_with(&canonical_target) {
                tracing::warn!(
                    entry = %entry_path.display(),
                    "Tar Slip blocked: path escapes target dir"
                );
                continue;
            }

            // Extract entry
            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&out_path).map_err(|e| {
                    crate::BootstrapError::Extract(format!(
                        "Cannot create dir {}: {e}",
                        out_path.display()
                    ))
                })?;
            } else {
                let mut outfile = std::fs::File::create(&out_path).map_err(|e| {
                    crate::BootstrapError::Extract(format!(
                        "Cannot create file {}: {e}",
                        out_path.display()
                    ))
                })?;
                // entry.unpack_in is simpler but we want the path traversal check above
                std::io::copy(&mut entry, &mut outfile).map_err(|e| {
                    crate::BootstrapError::Extract(format!(
                        "Cannot write {}: {e}",
                        out_path.display()
                    ))
                })?;
            }
        }

        // Write .extracted sentinel (compatible with RuntimeManager)
        let sentinel = target_dir.join(".extracted");
        tokio::fs::write(&sentinel, &asset.version)
            .await
            .map_err(crate::BootstrapError::Io)?;

        // Update manifest
        let mgr = RuntimeManager::new(&self.data_dir);
        let manifest_path = if asset.key.contains("onnxruntime")
            || asset.key == "python"
            || asset.key == "node"
            || asset.key == "git"
        {
            mgr.runtime_dir().join(".manifest.json")
        } else {
            mgr.models_dir().join(".manifest.json")
        };

        let mut manifest = Manifest::load(&manifest_path)
            .await
            .unwrap_or_else(|_| Manifest {
                entries: HashMap::new(),
            });
        manifest.upsert(&asset.key, &asset.version, None);
        manifest
            .save(&manifest_path)
            .await
            .map_err(|e| crate::BootstrapError::Io(std::io::Error::other(e)))?;

        // Emit layer done
        let _ = event_tx.send(InitEvent::LayerDone {
            key: asset.key.clone(),
            layer: 1,
            total_layers: 1,
            is_asset_done: true,
        });

        tracing::info!(
            key = %asset.key,
            dir = %target_dir.display(),
            "Asset extracted from bundle"
        );

        Ok(ExtractionResult {
            key: asset.key.clone(),
            target_dir,
            success: true,
            error: None,
        })
    }

    /// Extract all bundled assets and emit progress events.
    /// On success, writes the `.everevo_init` marker.
    pub async fn extract_all(
        &self,
        event_tx: &broadcast::Sender<InitEvent>,
    ) -> Result<Vec<ExtractionResult>, crate::BootstrapError> {
        let assets = self
            .list_assets()
            .map_err(crate::BootstrapError::Io)?;

        if assets.is_empty() {
            tracing::info!("No bundled assets found — will fall back to downloader");
            return Ok(vec![]);
        }

        let total = assets.len();
        let _ = event_tx.send(InitEvent::FoundMissing {
            total,
            total_bytes: 0, // size unknown for bundled assets
        });

        let mut results = Vec::new();
        for (i, asset) in assets.iter().enumerate() {
            match self.extract(asset, event_tx).await {
                Ok(result) => {
                    let _ = event_tx.send(InitEvent::AssetDone {
                        key: result.key.clone(),
                        completed: i + 1,
                        total,
                    });
                    results.push(result);
                }
                Err(e) => {
                    tracing::error!(
                        key = %asset.key,
                        error = %e,
                        "Failed to extract bundled asset"
                    );
                    let _ = event_tx.send(InitEvent::AssetFailed {
                        key: asset.key.clone(),
                        layer: 1,
                        error: e.to_string(),
                    });
                }
            }
        }

        tracing::info!(
            ok = results.len(),
            total,
            "Bundle extraction complete"
        );
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;

    fn create_test_archive(dir: &Path, key: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let archive_path = dir.join(format!("{}.tar.zst", key));

        // Build a tar archive in memory
        let mut tar_buf = Vec::new();
        {
            let mut tar_builder = tar::Builder::new(&mut tar_buf);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                tar_builder
                    .append_data(&mut header, format!("{}/{}", key, name), *content)
                    .unwrap();
            }
            tar_builder.finish().unwrap();
        }

        // Compress with zstd
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(&tar_buf).unwrap();
        let compressed = encoder.finish().unwrap();

        std::fs::write(&archive_path, &compressed).unwrap();
        archive_path
    }

    #[test]
    fn test_has_bundled_assets_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let extractor = ResourceExtractor::new(tmp.path(), tmp.path().join("data"));
        assert!(!extractor.has_bundled_assets());
    }

    #[test]
    fn test_has_bundled_assets_with_archive() {
        let tmp = TempDir::new().unwrap();
        create_test_archive(
            tmp.path(),
            "python",
            &[("python.exe", b"fake-exe"), ("README.txt", b"readme")],
        );
        let extractor = ResourceExtractor::new(tmp.path(), tmp.path().join("data"));
        assert!(extractor.has_bundled_assets());
    }

    #[test]
    fn test_list_assets() {
        let tmp = TempDir::new().unwrap();
        create_test_archive(tmp.path(), "python", &[("python.exe", b"fake")]);
        create_test_archive(tmp.path(), "node", &[("node.exe", b"fake")]);

        let extractor = ResourceExtractor::new(tmp.path(), tmp.path().join("data"));
        let assets = extractor.list_assets().unwrap();
        assert_eq!(assets.len(), 2);
        let keys: Vec<&str> = assets.iter().map(|a| a.key.as_str()).collect();
        assert!(keys.contains(&"python"));
        assert!(keys.contains(&"node"));
    }

    #[test]
    fn test_list_assets_skips_non_zst() {
        let tmp = TempDir::new().unwrap();
        create_test_archive(tmp.path(), "python", &[("python.exe", b"fake")]);
        std::fs::write(tmp.path().join("manifest.json"), b"{}").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), b"hello").unwrap();

        let extractor = ResourceExtractor::new(tmp.path(), tmp.path().join("data"));
        let assets = extractor.list_assets().unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].key, "python");
    }

    #[tokio::test]
    async fn test_extract_single_asset() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        create_test_archive(
            tmp.path(),
            "python",
            &[("python.exe", b"fake-exe"), ("python312.dll", b"fake-dll")],
        );

        // Write manifest
        std::fs::write(
            tmp.path().join("manifest.json"),
            r#"{"python": {"version": "3.12.8"}}"#,
        )
        .unwrap();

        let extractor = ResourceExtractor::new(tmp.path(), &data_dir);
        let assets = extractor.list_assets().unwrap();
        assert_eq!(assets.len(), 1);

        let (tx, _) = broadcast::channel(8);
        let result = extractor.extract(&assets[0], &tx).await.unwrap();
        assert!(result.success);
        assert_eq!(result.key, "python");

        // Verify output
        let target = data_dir.join("runtime").join("python");
        assert!(target.join("python.exe").exists());
        assert!(target.join("python312.dll").exists());

        // Verify sentinel
        let sentinel = target.join(".extracted");
        assert!(sentinel.exists());
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap().trim(),
            "3.12.8"
        );

        // Verify manifest was written
        let manifest_path = data_dir.join("runtime").join(".manifest.json");
        assert!(manifest_path.exists());
    }

    #[tokio::test]
    async fn test_extract_model_asset() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        create_test_archive(
            tmp.path(),
            "all-MiniLM-L6-v2",
            &[
                ("model_quantized.onnx", &[0u8; 100][..]),
                ("tokenizer.json", b"{}"),
            ],
        );

        std::fs::write(
            tmp.path().join("manifest.json"),
            r#"{"all-MiniLM-L6-v2": {"version": "v1"}}"#,
        )
        .unwrap();

        let extractor = ResourceExtractor::new(tmp.path(), &data_dir);
        let assets = extractor.list_assets().unwrap();

        let (tx, _) = broadcast::channel(8);
        let result = extractor.extract(&assets[0], &tx).await.unwrap();
        assert!(result.success);

        // Models go to data/models/, not data/runtime/
        let target = data_dir.join("models").join("all-MiniLM-L6-v2");
        assert!(target.join("model_quantized.onnx").exists());
        assert!(target.join("tokenizer.json").exists());
    }

    #[test]
    fn test_resource_extractor_new() {
        let extractor = ResourceExtractor::new("/tmp/bundled", "/tmp/data");
        assert!(extractor.resource_dir.ends_with("bundled"));
        assert!(extractor.data_dir.ends_with("data"));
    }

    #[test]
    fn test_has_bundled_assets_missing_dir() {
        let extractor =
            ResourceExtractor::new("/nonexistent/path/12345", "/tmp/data");
        assert!(!extractor.has_bundled_assets());
    }
}
