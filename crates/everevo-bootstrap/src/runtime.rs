//! Runtime Manager — extraction, retry guard, PATH injection.
//!
//! ## Robustness guarantees
//!
//! - **Marker AFTER success**: `.extracted` written to tmp, then rename, then verify
//! - **Retry limit**: 3 attempts per asset, then permanent error (no infinite loop)
//! - **ZIP reuse**: if ZIP exists on disk, skip download, go straight to extraction
//! - **Atomic rename with backup**: old target → .backup_ → rename tmp → verify → cleanup backup
//! - **Crash recovery**: tmp dirs cleaned on restart; backup restored if rename fails

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::Asset;

// ── RuntimeEnv ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RuntimeEnv {
    pub paths: Vec<PathBuf>,
    pub env_vars: HashMap<String, String>,
    pub installed: Vec<String>,
}

impl RuntimeEnv {
    pub fn empty() -> Self {
        Self { paths: Vec::new(), env_vars: HashMap::new(), installed: Vec::new() }
    }
}

// ── RuntimeManager ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RuntimeManager {
    runtime_dir: PathBuf,
    models_dir: PathBuf,
}

impl RuntimeManager {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            runtime_dir: data_dir.join("runtime"),
            models_dir: data_dir.join("models"),
        }
    }

    pub fn runtime_dir(&self) -> &PathBuf { &self.runtime_dir }
    pub fn models_dir(&self) -> &PathBuf { &self.models_dir }

    /// Check if a cached ZIP exists for this asset (no re-download needed).
    pub fn find_cached_zip(&self, asset: &Asset) -> Option<PathBuf> {
        let cache = self.runtime_dir.parent()?
            .join("downloads")
            .join(format!("{}.zip", asset.key));
        if cache.exists() && cache.metadata().ok()?.len() > 1024 {
            Some(cache)
        } else {
            None
        }
    }

    /// Extract a ZIP to the runtime/models directory.
    ///
    /// **Atomic**: extract to `.tmp_{name}/` first, then rename.
    /// **Idempotent**: skips if `.extracted` sentinel exists with matching version.
    /// **Retry guard**: 3 attempts max, then `PermanentFailure`.
    pub async fn install(&self, zip_path: &Path, asset: &Asset) -> Result<PathBuf, ExtractError> {
        let target = self.target_dir(asset);
        let sentinel = target.join(".extracted");
        let attempts_file = self.runtime_dir.join(format!(".attempts_{}", asset.key));

        // ── Already extracted? ────────────────────────────────────
        if sentinel.exists() {
            if let Ok(ver) = tokio::fs::read_to_string(&sentinel).await {
                if ver.trim() == asset.version { return Ok(target); }
            }
        }

        // ── Retry guard ───────────────────────────────────────────
        let attempts = read_attempts(&attempts_file).await;
        if attempts >= 3 {
            return Err(ExtractError::PermanentFailure(format!(
                "{} failed {} times. Delete {} to retry.",
                asset.key, attempts, attempts_file.display()
            )));
        }
        let _ = tokio::fs::write(&attempts_file, (attempts + 1).to_string()).await;

        tracing::info!(key = %asset.key, attempt = attempts + 1, "Extracting...");

        // ── Extract to tmp ────────────────────────────────────────
        let tmp_dir = target.with_file_name(format!(".tmp_{}", asset.key));
        if tmp_dir.exists() { let _ = tokio::fs::remove_dir_all(&tmp_dir).await; }
        tokio::fs::create_dir_all(&tmp_dir).await.map_err(|e| ExtractError::Io(e, tmp_dir.clone()))?;

        let zip_path_c = zip_path.to_path_buf();
        let tmp_c = tmp_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            let r = extract_zip_sync(&zip_path_c, &tmp_c);
            // Flatten versioned subdirectory (onnxruntime-win-x64-1.21.0/ → .)
            if r.is_ok() { flatten_tmp_dir(&tmp_c); }
            r
        })
        .await
        .map_err(|e| ExtractError::Internal(format!("Join: {e}")))?;

        if let Err(e) = result {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            return Err(e);
        }

        // Write sentinel into tmp BEFORE rename
        let tmp_sentinel = tmp_dir.join(".extracted");
        tokio::fs::write(&tmp_sentinel, &asset.version).await
            .map_err(|e| ExtractError::Io(e, tmp_sentinel))?;

        // ── Atomic rename with backup ─────────────────────────────
        let backup = target.with_file_name(format!(".backup_{}", asset.key));
        if target.exists() {
            if backup.exists() { let _ = tokio::fs::remove_dir_all(&backup).await; }
            tokio::fs::rename(&target, &backup).await
                .map_err(|e| ExtractError::Io(e, target.clone()))?;
        }
        if let Err(e) = tokio::fs::rename(&tmp_dir, &target).await {
            if backup.exists() { let _ = tokio::fs::rename(&backup, &target).await; }
            return Err(ExtractError::Io(e, target));
        }

        // ── Verify sentinel survived ──────────────────────────────
        match tokio::fs::read_to_string(&sentinel).await {
            Ok(ver) if ver.trim() == asset.version => {}
            _ => {
                let _ = tokio::fs::remove_dir_all(&target).await;
                if backup.exists() { let _ = tokio::fs::rename(&backup, &target).await; }
                return Err(ExtractError::Internal("Sentinel verify failed after rename".into()));
            }
        }

        // Success — cleanup
        if backup.exists() { let _ = tokio::fs::remove_dir_all(&backup).await; }
        let _ = tokio::fs::remove_file(&attempts_file).await;
        tracing::info!(key = %asset.key, path = %target.display(), "Extracted OK");
        Ok(target)
    }

    // ── PATH injection ────────────────────────────────────────────

    pub async fn build_env(&self) -> Result<RuntimeEnv, ExtractError> {
        let mut env = RuntimeEnv::empty();
        self.add_if_extracted(&mut env, "python", &[&[], &["Scripts"]]);
        self.add_if_extracted(&mut env, "node", &[&[]]);
        self.add_if_extracted(&mut env, "git", &[&["bin"], &["cmd"], &["mingw64", "bin"]]);
        self.add_if_extracted(&mut env, "onnxruntime", &[&["lib"]]);
        Ok(env)
    }

    fn add_if_extracted(&self, env: &mut RuntimeEnv, key: &str, subdirs: &[&[&str]]) {
        let dir = self.runtime_dir.join(key);
        // Primary check: the .extracted sentinel written by install()
        let extracted = dir.join(".extracted").exists();
        // Fallback: check if any expected subdirectories actually have files
        // (handles manually placed assets or pre-flattened directories)
        let has_files = subdirs.iter().any(|sub| {
            let path: PathBuf = if sub.is_empty() { dir.clone() } else { sub.iter().fold(dir.clone(), |p, s| p.join(s)) };
            path.exists() && std::fs::read_dir(&path).map_or(false, |mut rd| rd.next().is_some())
        });
        if extracted || has_files {
            for sub in subdirs {
                env.paths.push(if sub.is_empty() { dir.clone() } else { sub.iter().fold(dir.clone(), |p, s| p.join(s)) });
            }
            env.installed.push(key.into());
        }
    }

    // ── Manifest ──────────────────────────────────────────────────

    pub async fn update_manifest(&self, asset: &Asset) -> Result<(), ExtractError> {
        let manifest_path = if asset.is_model() {
            self.models_dir.join(".manifest.json")
        } else {
            self.runtime_dir.join(".manifest.json")
        };
        let mut manifest = Manifest::load(&manifest_path).await.unwrap_or_else(|_| Manifest { entries: HashMap::new() });
        manifest.upsert(&asset.key, &asset.version, None);
        manifest.save(&manifest_path).await.map_err(|e| ExtractError::Io(e, manifest_path))?;
        Ok(())
    }

    fn target_dir(&self, asset: &Asset) -> PathBuf {
        if asset.is_model() {
            self.models_dir.join(&asset.key)
        } else {
            self.runtime_dir.join(&asset.key)
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Sync helper: strip the versioned wrapper directory from extracted archives.
fn flatten_tmp_dir(dir: &Path) {
    let mut dirs: Vec<std::ffi::OsString> = Vec::new();
    let mut files = 0usize;

    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            dirs.push(entry.file_name());
        } else {
            files += 1;
        }
    }

    // Only flatten when there's exactly one subdirectory and no loose files
    if dirs.len() != 1 || files > 0 {
        return;
    }

    let wrapper = dir.join(&dirs[0]);
    let Ok(rd) = std::fs::read_dir(&wrapper) else { return };

    let mut count = 0u32;
    for entry in rd.flatten() {
        let name = entry.file_name();
        if std::fs::rename(wrapper.join(&name), dir.join(&name)).is_ok() {
            count += 1;
        }
    }

    let _ = std::fs::remove_dir(&wrapper);
    tracing::info!(wrapper = %dirs[0].to_string_lossy(), count, "Flattened versioned directory");
}

async fn read_attempts(path: &Path) -> u32 {
    if !path.exists() { return 0; }
    tokio::fs::read_to_string(path).await
        .ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn extract_zip_sync(zip_path: &Path, dest: &Path) -> Result<(), ExtractError> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| ExtractError::Io(e, zip_path.to_path_buf()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| ExtractError::Zip(e.to_string()))?;

    // Canonical dest once (the tmp dir; guaranteed to exist).
    let canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)
            .map_err(|e| ExtractError::Zip(e.to_string()))?;
        let out_path = dest.join(entry.name());

        // Create directories FIRST so that canonicalize() succeeds below.
        // On Windows, canonicalize() on a non-existent path fails, and the
        // non-canonical fallback may not match the canonical-dest prefix
        // (e.g. `F:\...` vs `\\?\F:\...`), causing false-positive Zip Slip
        // rejections for legitimate entries like `usr/share/licenses/...`.
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| ExtractError::Io(e, out_path.clone()))?;
        } else if let Some(parent) = out_path.parent() {
            let parent = parent.to_path_buf();
            std::fs::create_dir_all(&parent)
                .map_err(|e| ExtractError::Io(e, parent))?;
        }

        // Zip Slip defense — resolve via existing parent dir so
        // canonicalize() works on Windows for files that don't exist yet.
        let resolved = resolve_safe(&out_path, entry.is_dir());
        if !resolved.starts_with(&canonical_dest) {
            tracing::warn!(entry = entry.name(), "Zip Slip blocked: path escapes dest");
            continue;
        }

        // Write file contents (dirs already created above).
        if !entry.is_dir() {
            let mut outfile = std::fs::File::create(&out_path)
                .map_err(|e| ExtractError::Io(e, out_path.clone()))?;
            std::io::copy(&mut entry, &mut outfile)
                .map_err(|e| ExtractError::Io(e, out_path.clone()))?;
        }
    }
    Ok(())
}

/// Resolve a safe canonical path for Zip Slip checking.
///
/// Files don't exist yet → canonicalize the parent dir and join the filename.
/// Directories already exist → canonicalize directly.
fn resolve_safe(path: &Path, is_dir: bool) -> PathBuf {
    if is_dir {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    }
    match path.parent() {
        Some(parent) if parent.exists() => {
            let canon_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            canon_parent.join(path.file_name().unwrap_or_default())
        }
        _ => path.to_path_buf(),
    }
}

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("I/O at {1}: {0}")]
    Io(std::io::Error, PathBuf),
    #[error("ZIP: {0}")]
    Zip(String),
    #[error("Internal: {0}")]
    Internal(String),
    #[error("Permanent: {0}")]
    PermanentFailure(String),
}

impl From<ExtractError> for everevo_core::EverEvoError {
    fn from(e: ExtractError) -> Self {
        everevo_core::EverEvoError::Bootstrap(format!("Extraction: {e}"))
    }
}
