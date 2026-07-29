//! EverEvo asset bundler — downloads and compresses runtimes + models
//! into `.tar.zst` archives for offline distribution via Tauri resources.
//!
//! ## Usage
//!
//! ```bash
//! cargo run --bin everevo-bundler --release
//! cargo run --bin everevo-bundler --release -- --target aarch64-apple-darwin
//! cargo run --bin everevo-bundler --release -- --skip-git --skip-reranker-cn
//! ```

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use everevo_bootstrap::Asset;

#[derive(Parser)]
#[command(name = "everevo-bundler")]
struct Cli {
    #[arg(long)]
    target: Option<String>,
    #[arg(long, default_value = "resources/bundled")]
    output: PathBuf,
    #[arg(long)]
    skip_git: bool,
    #[arg(long)]
    skip_reranker_cn: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "everevo_bundler=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let target = cli
        .target
        .unwrap_or_else(everevo_bootstrap::detect_target);

    tracing::info!(%target, output = %cli.output.display(), "EverEvo bundler starting");

    let assets = everevo_bootstrap::assets_for_target(&target);
    let filtered: Vec<&Asset> = assets
        .iter()
        .filter(|a| {
            if cli.skip_git && a.key == "git" { return false; }
            if cli.skip_reranker_cn && a.key == "reranker-cn" { return false; }
            !a.is_system_provided()
        })
        .collect();

    tracing::info!(total = filtered.len(), "Assets to bundle");

    std::fs::create_dir_all(&cli.output)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("everevo-bundler/0.1")
        .build()?;

    // Load existing manifest for resume support
    let manifest_path = cli.output.join("manifest.json");
    let mut manifest: serde_json::Map<String, serde_json::Value> =
        if manifest_path.exists() {
            let data = std::fs::read_to_string(&manifest_path).unwrap_or_default();
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            serde_json::Map::new()
        };

    let mut success = 0usize;
    let mut skipped = 0usize;

    for asset in &filtered {
        let output_path = cli.output.join(format!("{}.tar.zst", asset.key));

        // Skip if already bundled (non-zero file + manifest entry matches version)
        if output_path.exists() {
            if let Ok(meta) = std::fs::metadata(&output_path) {
                if meta.len() > 1024 {
                    let version_ok = manifest
                        .get(&asset.key)
                        .and_then(|v| v.get("version"))
                        .and_then(|v| v.as_str())
                        .map(|v| v == asset.version)
                        .unwrap_or(false);
                    if version_ok {
                        skipped += 1;
                        tracing::info!(key = %asset.key, "⏭ Already bundled — skipping");
                        continue;
                    } else {
                        tracing::info!(key = %asset.key, "Version mismatch — re-bundling");
                        let _ = std::fs::remove_file(&output_path);
                    }
                } else {
                    tracing::info!(key = %asset.key, "Corrupt file — re-bundling");
                    let _ = std::fs::remove_file(&output_path);
                }
            }
        }

        tracing::info!(key = %asset.key, kind = ?asset.kind, desc = %asset.description, "Bundling…");

        let result = if asset.is_runtime() {
            bundle_runtime(asset, &cli.output, &client).await
        } else {
            bundle_model(asset, &cli.output, &client).await
        };

        match result {
            Ok(size) => {
                success += 1;
                manifest.insert(
                    asset.key.clone(),
                    serde_json::json!({
                        "version": asset.version,
                        "size_bytes": size,
                    }),
                );
                // Write manifest after each success (crash-safe)
                if let Err(e) = std::fs::write(
                    &manifest_path,
                    serde_json::to_string_pretty(&manifest).unwrap_or_default(),
                ) {
                    tracing::warn!(error = %e, "Failed to write manifest");
                }
                tracing::info!(key = %asset.key, size_mb = size / 1_048_576, "✓ Bundled");
            }
            Err(e) => {
                tracing::error!(key = %asset.key, error = %e, "✗ FAILED");
                // Exit immediately — re-run retries only the failed one.
                return Err(format!(
                    "Asset '{}' failed: {e}\nRe-run the script to retry (already-bundled assets are skipped).",
                    asset.key
                )
                .into());
            }
        }
    }

    tracing::info!(
        path = %manifest_path.display(),
        "{success} bundled, {skipped} skipped"
    );

    Ok(())
}

// ── Download helpers ────────────────────────────────────────────────────

/// Try downloading a URL to a file, with one retry.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<u64, String> {
    for attempt in 0..2 {
        match try_download(client, url, dest).await {
            Ok(size) => return Ok(size),
            Err(e) if attempt == 0 => {
                tracing::warn!(url, error = %e, "Download failed, retrying…");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

async fn try_download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<u64, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP GET failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("read body: {e}"))?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("write: {e}"))?;

    Ok(bytes.len() as u64)
}

/// Download a URL using mirror fallback.
async fn download_with_mirrors(
    client: &reqwest::Client,
    urls: &[&str],
    dest: &Path,
) -> Result<u64, String> {
    for (i, url) in urls.iter().enumerate() {
        if i > 0 {
            tracing::info!(url, "Trying mirror…");
        }
        match download_file(client, url, dest).await {
            Ok(size) => return Ok(size),
            Err(e) => {
                tracing::warn!(url, error = %e, "Download failed");
            }
        }
    }
    Err("All mirrors exhausted".into())
}

// ── Runtime bundling (ZIP download → extract → tar.zst) ──────────────

async fn bundle_runtime(
    asset: &Asset,
    output_dir: &Path,
    client: &reqwest::Client,
) -> Result<u64, String> {
    let temp = tempfile::TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let zip_path = temp.path().join(format!("{}.zip", asset.key));

    // 1. Download ZIP
    let urls: Vec<&str> = asset.all_urls();
    tracing::info!(
        key = %asset.key,
        url = urls.first().copied().unwrap_or("none"),
        "Downloading runtime…"
    );
    let _size = download_with_mirrors(client, &urls, &zip_path).await?;
    tracing::info!(key = %asset.key, size_mb = _size / 1_048_576, "Downloaded");

    // 2. Extract ZIP to temp dir
    let extract_dir = temp.path().join("extracted");
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("mkdir: {e}"))?;
    extract_zip(&zip_path, &extract_dir)?;
    tracing::info!(key = %asset.key, "Extracted");

    // 3. Create tar.zst
    let output_path = output_dir.join(format!("{}.tar.zst", asset.key));
    create_tar_zst(&extract_dir, &output_path)
}

// ── Model bundling (download files → tar.zst) ────────────────────────

async fn bundle_model(
    asset: &Asset,
    output_dir: &Path,
    client: &reqwest::Client,
) -> Result<u64, String> {
    let temp = tempfile::TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let model_dir = temp.path().join(&asset.key);
    std::fs::create_dir_all(&model_dir).map_err(|e| format!("mkdir: {e}"))?;

    // 1. Download model_quantized.onnx
    let onnx_path = model_dir.join("model_quantized.onnx");
    tracing::info!(key = %asset.key, url = %asset.primary_url, "Downloading model…");
    download_file(client, &asset.primary_url, &onnx_path).await?;

    // 2. Download extra files
    for extra in &asset.extra_files {
        let extra_path = model_dir.join(&extra.filename);
        tracing::info!(key = %asset.key, file = %extra.filename, "Downloading extra…");
        download_file(client, &extra.url, &extra_path).await?;
    }

    // 3. Create tar.zst
    let output_path = output_dir.join(format!("{}.tar.zst", asset.key));
    create_tar_zst(&model_dir, &output_path)
}

// ── ZIP extraction ──────────────────────────────────────────────────────

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let reader = BufReader::new(file);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("read zip: {e}"))?;

    let canonical_dest = dest
        .canonicalize()
        .unwrap_or_else(|_| dest.to_path_buf());

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        let name = entry.name().to_string();

        // Strip top-level directory (most ZIPs have a single root dir)
        let relative: String = name
            .split('/')
            .skip(1) // skip root dir
            .collect::<Vec<_>>()
            .join("/");

        if relative.is_empty() {
            continue;
        }

        let out_path = dest.join(&relative);

        // Path traversal defense
        let resolved = out_path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.join(out_path.file_name().unwrap_or_default()))
            .unwrap_or_else(|| out_path.clone());

        if !resolved.starts_with(&canonical_dest) {
            tracing::warn!(entry = name, "Zip Slip blocked");
            continue;
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("mkdir {}: {e}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let mut outfile = std::fs::File::create(&out_path)
                .map_err(|e| format!("create {}: {e}", out_path.display()))?;
            std::io::copy(&mut entry, &mut outfile)
                .map_err(|e| format!("write {}: {e}", out_path.display()))?;
        }
    }

    Ok(())
}

// ── tar.zst creation ────────────────────────────────────────────────────

fn create_tar_zst(input_dir: &Path, output_path: &Path) -> Result<u64, String> {
    let out_file =
        std::fs::File::create(output_path).map_err(|e| format!("create: {e}"))?;

    // zstd compression level 3 — good balance of speed/size
    let encoder = zstd::stream::Encoder::new(out_file, 3)
        .map_err(|e| format!("zstd encoder: {e}"))?;

    let mut tar_builder = tar::Builder::new(encoder);
    add_dir_to_tar(&mut tar_builder, input_dir, "")?;

    // Finish tar → finish zstd
    let encoder = tar_builder
        .into_inner()
        .map_err(|e| format!("tar finish: {e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("zstd finish: {e}"))?;

    // Read back to get final compressed size
    let metadata = std::fs::metadata(output_path)
        .map_err(|e| format!("metadata: {e}"))?;
    Ok(metadata.len())
}

fn add_dir_to_tar<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let path = entry.path();
        let name = if prefix.is_empty() {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        } else {
            format!("{}/{}", prefix, path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"))
        };

        if path.is_dir() {
            add_dir_to_tar(builder, &path, &name)?;
        } else {
            let mut file = std::fs::File::open(&path).map_err(|e| format!("open: {e}"))?;
            builder
                .append_file(&name, &mut file)
                .map_err(|e| format!("tar append {name}: {e}"))?;
        }
    }
    Ok(())
}
