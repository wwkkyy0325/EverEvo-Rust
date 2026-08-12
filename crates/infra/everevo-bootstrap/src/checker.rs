// ── Manifest Check Logic ────────────────────────────────────────────────

use crate::manifest::Manifest;
use crate::{Asset, Provisioned};
use std::path::{Path, PathBuf};

pub(crate) struct CheckOutcome {
    pub(crate) ready: Vec<Provisioned>,
    pub(crate) missing: Vec<Asset>,
    pub(crate) corrupt: Vec<Provisioned>,
}

/// Check all defined assets against a manifest file.
pub(crate) async fn check_manifest(dir: &Path, assets: &[Asset]) -> CheckOutcome {
    let manifest = Manifest::load(&dir.join(".manifest.json")).await;

    let mut ready = Vec::new();
    let mut missing = Vec::new();
    let mut corrupt = Vec::new();

    for asset in assets {
        // SystemProvided assets are not extracted — skip filesystem checks.
        if asset.is_system_provided() {
            ready.push(Provisioned {
                key: asset.key.clone(),
                version: asset.version.clone(),
                path: PathBuf::new(),
            });
            continue;
        }

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
        && onnx_path
            .metadata()
            .map(|m| m.len() > 1_048_576)
            .unwrap_or(false);

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
    if !sentinel.exists() {
        return None;
    }
    std::fs::read_to_string(&sentinel)
        .ok()
        .map(|s| s.trim().to_string())
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
