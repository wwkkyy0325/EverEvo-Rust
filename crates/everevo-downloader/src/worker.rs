//! Download workers — execute the actual HTTP requests.
//!
//! Two strategies:
//! - **Simple**: single GET, `Range` header for resume.
//! - **Chunked**: split file, N concurrent workers, assemble at end.

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

use futures::StreamExt;

use crate::config::DownloaderConfig;
use crate::error::DownloadError;
use crate::mirror::MirrorRegistry;
use crate::observer::{DownloadEvent, EventBroadcaster, ObserverSet};
use crate::resume::ResumeState;
use crate::state::TaskMeta;
use crate::strategy::DownloadStrategy;
use crate::task::DownloadTask;

/// A shared HTTP client — created once per engine, cloned cheaply.
pub(crate) type HttpClient = reqwest::Client;

/// Build a reqwest Client from config.
pub(crate) fn build_client(config: &DownloaderConfig) -> Result<HttpClient, DownloadError> {
    reqwest::Client::builder()
        .user_agent(&config.user_agent)
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(300))
        .pool_idle_timeout(std::time::Duration::from_secs(config.pool_idle_timeout_secs))
        .tcp_nodelay(true)
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .map_err(DownloadError::Http)
}

// ── Entry Point ─────────────────────────────────────────────────────────

/// Execute a single download task. This is the main worker function.
pub(crate) async fn execute_task(
    task: &DownloadTask,
    client: &HttpClient,
    config: &DownloaderConfig,
    mirrors: &MirrorRegistry,
    events: &EventBroadcaster,
    observers: &ObserverSet,
    meta: &tokio::sync::Mutex<TaskMeta>,
) -> Result<(PathBuf, u64, String), DownloadError> {
    let effective_timeout = config.effective_timeout_secs(task.timeout_secs);
    let effective_retries = config.effective_retries(task.max_retries);
    let effective_chunks = config.effective_max_chunks(task.max_chunks);
    let chunk_size = config.effective_chunk_size(task.chunk_size);
    let _timeout_dur = std::time::Duration::from_secs(effective_timeout);

    // ── Mirror Resolution ──────────────────────────────────────────
    let urls_to_try = build_url_list(task, mirrors, config);

    let mut last_error: Option<DownloadError> = None;

    for (attempt, (url, mirror_name)) in urls_to_try.iter().enumerate() {
        if attempt > 0 {
            events.send(DownloadEvent::MirrorSwitched {
                task_id: task.id.clone(),
                from_mirror: urls_to_try[attempt - 1].1.clone(),
                to_mirror: mirror_name.clone(),
                reason: last_error
                    .as_ref()
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            });
            notify_observers(
                observers,
                DownloadEvent::MirrorSwitched {
                    task_id: task.id.clone(),
                    from_mirror: urls_to_try[attempt - 1].1.clone(),
                    to_mirror: mirror_name.clone(),
                    reason: last_error
                        .as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or_default(),
                },
            )
            .await;
        }

        // ── Probe: HEAD request to get Content-Length ──────────────
        // Try ureq first (sync Winsock works in Tauri), reqwest as fallback.
        let (total_size, _supports_range) =
            match probe_url_ureq(url).await {
                Ok(v) => v,
                Err(_) => {
                    match probe_url(url, client, effective_timeout).await {
                        Ok(v) => v,
                        Err(e) => {
                            let detail = format_reqwest_error(&e);
                            tracing::warn!(url, mirror = mirror_name, error = %e, %detail, "Probe failed");
                            last_error = Some(e);
                            continue;
                        }
                    }
                }
            };

        // ── Resume State ────────────────────────────────────────────
        let resume_path = task.resume_path();
        let mut resume = ResumeState::load(&resume_path)
            .await
            .ok()
            .flatten()
            .filter(|r| r.url == *url && r.total_size == total_size)
            .unwrap_or_else(|| ResumeState::new(&task.id, url, total_size, chunk_size));

        // ── Choose Strategy ─────────────────────────────────────────
        let strategy = if task.max_chunks > 0 {
            DownloadStrategy::Chunked {
                concurrency: effective_chunks,
            }
        } else {
            DownloadStrategy::choose(
                total_size,
                task.max_chunks,
                effective_chunks,
                config.chunk_threshold,
            )
        };

        // ── Execute ─────────────────────────────────────────────────
        // ureq (sync, blocking Winsock) works reliably in Tauri's process
        // model where Tokio's IOCP-based async I/O silently drops outbound
        // TCP SYNs. Use ureq FIRST for simple downloads; only fall back to
        // reqwest for chunked/concurrent downloads (>10 MB threshold).
        let result = match strategy {
            DownloadStrategy::Simple => {
                // Try ureq first (sync, reliable), reqwest as fallback
                match tokio::task::spawn_blocking({
                    let task = task.clone();
                    let url = url.clone();
                    move || download_ureq_fallback(&task, &url, total_size)
                })
                .await
                {
                    Ok(Ok(path)) => {
                        tracing::info!(url, mirror = mirror_name, "ureq download OK");
                        Ok(path)
                    }
                    Ok(Err(ue)) => {
                        tracing::warn!(url, mirror = mirror_name, error = %ue, "ureq failed, trying reqwest");
                        download_simple(
                            task, url, client, total_size, &mut resume, effective_retries,
                            effective_timeout, events, observers, meta,
                        )
                        .await
                    }
                    Err(join_err) => {
                        tracing::warn!(url, mirror = mirror_name, error = %join_err, "ureq spawn failed, trying reqwest");
                        download_simple(
                            task, url, client, total_size, &mut resume, effective_retries,
                            effective_timeout, events, observers, meta,
                        )
                        .await
                    }
                }
            }
            DownloadStrategy::Chunked { concurrency } => {
                download_chunked(
                    task, url, client, total_size, chunk_size, concurrency,
                    &mut resume, effective_retries, effective_timeout, events, observers, meta,
                )
                .await
            }
        };

        match result {
            Ok(path) => {
                resume.cleanup(&resume_path).await;
                let size = total_size;
                events.send(DownloadEvent::Completed {
                    task_id: task.id.clone(),
                    path: path.display().to_string(),
                    size_bytes: size,
                    duration_ms: meta.lock().await.duration_ms(),
                    mirror_used: mirror_name.clone(),
                });
                notify_observers(
                    observers,
                    DownloadEvent::Completed {
                        task_id: task.id.clone(),
                        path: path.display().to_string(),
                        size_bytes: size,
                        duration_ms: meta.lock().await.duration_ms(),
                        mirror_used: mirror_name.clone(),
                    },
                )
                .await;
                return Ok((path, size, mirror_name.clone()));
            }
            Err(e) => {
                let detail = format_reqwest_error(&e);
                tracing::warn!(url, mirror = mirror_name, error = %e, %detail, "Download attempt failed");

                // Ureq fallback for chunked downloads (reqwest timed out)
                let is_timeout = matches!(&e, DownloadError::Http(re) if re.is_timeout());
                if is_timeout && total_size > 0 {
                    match tokio::task::spawn_blocking({
                        let task = task.clone();
                        let url = url.clone();
                        move || download_ureq_fallback(&task, &url, total_size)
                    })
                    .await
                    {
                        Ok(Ok(path)) => {
                            tracing::info!(url, mirror = mirror_name, "ureq fallback succeeded");
                            events.send(DownloadEvent::Completed {
                                task_id: task.id.clone(),
                                path: path.display().to_string(),
                                size_bytes: total_size,
                                duration_ms: meta.lock().await.duration_ms(),
                                mirror_used: mirror_name.clone(),
                            });
                            notify_observers(
                                observers,
                                DownloadEvent::Completed {
                                    task_id: task.id.clone(),
                                    path: path.display().to_string(),
                                    size_bytes: total_size,
                                    duration_ms: meta.lock().await.duration_ms(),
                                    mirror_used: mirror_name.clone(),
                                },
                            )
                            .await;
                            return Ok((path, total_size, mirror_name.clone()));
                        }
                        Ok(Err(ue)) => {
                            tracing::warn!(url, mirror = mirror_name, error = %ue, "ureq fallback also failed");
                        }
                        Err(join_err) => {
                            tracing::warn!(url, mirror = mirror_name, error = %join_err, "ureq spawn also failed");
                        }
                    }
                }

                last_error = Some(e);
                // Save resume state for next attempt / mirror
                let _ = resume.save(&resume_path).await;
            }
        }
    }

    // All mirrors exhausted
    Err(last_error.unwrap_or_else(|| DownloadError::AllMirrorsExhausted {
        url: task.url.clone(),
        tried: urls_to_try.iter().map(|(_, n)| n.clone()).collect(),
    }))
}

// ── URL List Builder ────────────────────────────────────────────────────

fn build_url_list(
    task: &DownloadTask,
    mirrors: &MirrorRegistry,
    config: &DownloaderConfig,
) -> Vec<(String, String)> {
    let mut urls: Vec<(String, String)> = Vec::new();

    // Always try the original URL first
    urls.push((task.url.clone(), "original".into()));

    // Add mirror candidates
    if config.mirror_enabled {
        let region = if matches!(task.region, crate::task::Region::Auto) {
            config.default_region
        } else {
            task.region
        };
        let candidates = mirrors.resolve(&task.url, region);
        for (mirror_url, mirror_name) in candidates {
            if mirror_url != task.url {
                urls.push((mirror_url, mirror_name));
            }
        }
    }

    urls
}

// ── URL Probe ───────────────────────────────────────────────────────────

/// HEAD request to get file metadata — always uses 8s timeout.
async fn probe_url(
    url: &str,
    client: &HttpClient,
    _download_timeout: u64,
) -> Result<(u64, bool), DownloadError> {
    let resp = client
        .head(url)
        .timeout(std::time::Duration::from_secs(8)) // probe is always fast
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(DownloadError::Http(
            resp.error_for_status().unwrap_err(),
        ));
    }

    let total_size = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let supports_range = resp
        .headers()
        .get("accept-ranges")
        .map(|v| v.to_str().unwrap_or("") == "bytes")
        .unwrap_or(false);

    Ok((total_size, supports_range))
}

// ── Simple Download ─────────────────────────────────────────────────────

async fn download_simple(
    task: &DownloadTask,
    url: &str,
    client: &HttpClient,
    total_size: u64,
    resume: &mut ResumeState,
    max_retries: u32,
    timeout_secs: u64,
    events: &EventBroadcaster,
    observers: &ObserverSet,
    meta: &tokio::sync::Mutex<TaskMeta>,
) -> Result<PathBuf, DownloadError> {
    let mut attempts = 0;
    // For simple downloads, check actual on-disk file size for resume offset
    let mut downloaded: u64 = if task.dest_path.exists() {
        tokio::fs::metadata(&task.dest_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    loop {
        let mut req = client
            .get(url)
            .timeout(std::time::Duration::from_secs(timeout_secs));
        if downloaded > 0 {
            req = req.header("Range", format!("bytes={downloaded}-"));
        }

        let resp = req.send().await?;
        if !resp.status().is_success() && resp.status().as_u16() != 206 {
            return Err(DownloadError::Http(resp.error_for_status().unwrap_err()));
        }

        // Ensure parent dir exists
        if let Some(parent) = task.dest_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| DownloadError::io(parent, e))?;
        }

        // Open file for append (or create)
        let mut file = if downloaded > 0 {
            File::options()
                .append(true)
                .open(&task.dest_path)
                .await
                .map_err(|e| DownloadError::io(&task.dest_path, e))?
        } else {
            File::create(&task.dest_path)
                .await
                .map_err(|e| DownloadError::io(&task.dest_path, e))?
        };

        // Stream body to file
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk)
                .await
                .map_err(|e| DownloadError::io(&task.dest_path, e))?;
            downloaded += chunk.len() as u64;

            // Update progress
            {
                let mut m = meta.lock().await;
                m.update_progress(downloaded, total_size.max(downloaded));
                if let Some(p) = m.progress() {
                    events.send(DownloadEvent::Progress {
                        task_id: task.id.clone(),
                        progress: p.clone(),
                    });
                }
            }
        }

        file.flush()
            .await
            .map_err(|e| DownloadError::io(&task.dest_path, e))?;

        // Verify completion
        if total_size == 0 || downloaded >= total_size {
            // Mark the single chunk as done in resume
            resume.mark_chunk_done(0);
            return Ok(task.dest_path.clone());
        }

        // Partial — retry
        attempts += 1;
        if attempts > max_retries {
            return Err(DownloadError::MaxRetriesExceeded {
                url: url.to_string(),
                max: max_retries,
            });
        }

        events.send(DownloadEvent::Retrying {
            task_id: task.id.clone(),
            attempt: attempts,
            max_attempts: max_retries,
            reason: format!("Download incomplete: {downloaded}/{total_size}"),
        });
        notify_observers(
            observers,
            DownloadEvent::Retrying {
                task_id: task.id.clone(),
                attempt: attempts,
                max_attempts: max_retries,
                reason: format!("Download incomplete: {downloaded}/{total_size}"),
            },
        )
        .await;
    }
}

// ── Chunked Download ────────────────────────────────────────────────────

async fn download_chunked(
    task: &DownloadTask,
    url: &str,
    client: &HttpClient,
    total_size: u64,
    chunk_size: u64,
    concurrency: usize,
    resume: &mut ResumeState,
    max_retries: u32,
    timeout_secs: u64,
    events: &EventBroadcaster,
    _observers: &ObserverSet,
    meta: &tokio::sync::Mutex<TaskMeta>,
) -> Result<PathBuf, DownloadError> {
    let total_chunks = resume.total_chunks;
    let _timeout_dur = std::time::Duration::from_secs(timeout_secs);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));

    // Collect pending chunks
    let pending: Vec<usize> = (0..total_chunks)
        .filter(|i| !resume.completed_chunks.contains(i))
        .collect();

    if pending.is_empty() {
        // All chunks done — just assemble
        return assemble_file(task, total_chunks).await;
    }

    // Naive approach: spawn all pending, semaphore limits concurrency.
    // For huge files with 1000+ chunks, use a worker-pool instead (TODO).
    let handles: Vec<tokio::task::JoinHandle<Result<usize, DownloadError>>> = pending
        .iter()
        .map(|&idx| {
            let client = client.clone();
            let url = url.to_string();
            let task_id = task.id.clone();
            let task_owned = task.clone();
            let sem = semaphore.clone();
            let events = EventBroadcaster::clone(events);
            let retries = max_retries;

            tokio::spawn(async move {
                let _permit = sem.acquire().await;
                download_chunk(&task_id, &url, &client, total_size, chunk_size, idx, &task_owned, retries, timeout_secs, &events).await?;
                // Permit dropped here
                Ok(idx)
            })
        })
        .collect();

    // Wait for all chunks, tracking progress
    let total_downloaded = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let _td = total_downloaded.clone();

    for handle in handles {
        match handle.await.unwrap_or_else(|e| Err(DownloadError::Other(e.to_string()))) {
            Ok(idx) => {
                resume.mark_chunk_done(idx);
                // Emit chunk done
                events.send(DownloadEvent::ChunkDone {
                    task_id: task.id.clone(),
                    chunk_index: idx,
                    total_chunks,
                });
                // Update overall progress
                let done_bytes = resume.completed_chunks.len() as u64 * chunk_size;
                {
                    let mut m = meta.lock().await;
                    m.update_progress(done_bytes.min(total_size), total_size);
                    if let Some(p) = m.progress() {
                        events.send(DownloadEvent::Progress {
                            task_id: task.id.clone(),
                            progress: p.clone(),
                        });
                    }
                }
            }
            Err(e) => {
                // Cancel remaining? For now, propagate first error
                return Err(e);
            }
        }
    }

    // Assemble
    assemble_file(task, total_chunks).await
}

/// Download a single chunk with retry.
async fn download_chunk(
    task_id: &str,
    url: &str,
    client: &HttpClient,
    total_size: u64,
    chunk_size: u64,
    index: usize,
    task: &DownloadTask,
    max_retries: u32,
    timeout_secs: u64,
    events: &EventBroadcaster,
) -> Result<(), DownloadError> {
    let (start, end) = {
        let s = index as u64 * chunk_size;
        let e = if index as u64 == (total_size - 1) / chunk_size {
            total_size.saturating_sub(1)
        } else {
            (s + chunk_size).saturating_sub(1)
        };
        (s, e)
    };

    let chunk_path = task.chunk_path(index);

    // Check if chunk already exists (from prior partial run)
    if chunk_path.exists() {
        let meta = tokio::fs::metadata(&chunk_path)
            .await
            .map_err(|e| DownloadError::io(&chunk_path, e))?;
        let expected = end - start + 1;
        if meta.len() == expected {
            tracing::debug!(chunk = index, "Chunk already complete, skipping");
            return Ok(());
        }
    }

    let range_header = format!("bytes={start}-{end}");
    let mut attempts = 0;

    loop {
        let resp = client
            .get(url)
            .header("Range", &range_header)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 206 {
            if attempts < max_retries {
                attempts += 1;
                events.send(DownloadEvent::Retrying {
                    task_id: task_id.to_string(),
                    attempt: attempts,
                    max_attempts: max_retries,
                    reason: format!("Chunk {index} HTTP {status}"),
                });
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempts as u64)).await;
                continue;
            }
            return Err(DownloadError::Http(resp.error_for_status().unwrap_err()));
        }

        let bytes = resp.bytes().await?;
        fs::write(&chunk_path, &bytes)
            .await
            .map_err(|e| DownloadError::io(&chunk_path, e))?;

        return Ok(());
    }
}

/// Assemble all chunks into the final destination file in order.
async fn assemble_file(
    task: &DownloadTask,
    total_chunks: usize,
) -> Result<PathBuf, DownloadError> {
    if let Some(parent) = task.dest_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DownloadError::io(parent, e))?;
    }

    let mut dest = File::create(&task.dest_path)
        .await
        .map_err(|e| DownloadError::io(&task.dest_path, e))?;

    for i in 0..total_chunks {
        let chunk_path = task.chunk_path(i);
        let data = fs::read(&chunk_path)
            .await
            .map_err(|e| DownloadError::io(&chunk_path, e))?;
        dest.write_all(&data)
            .await
            .map_err(|e| DownloadError::io(&task.dest_path, e))?;
        // Clean up chunk file
        let _ = fs::remove_file(&chunk_path).await;
    }

    dest.flush()
        .await
        .map_err(|e| DownloadError::io(&task.dest_path, e))?;

    Ok(task.dest_path.clone())
}

// ── Error Diagnostics ────────────────────────────────────────────────────

/// Unwrap the reqwest error chain into a single string for diagnosis.
fn format_reqwest_error(e: &DownloadError) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let DownloadError::Http(req_err) = e {
        parts.push(format!("reqwest: {req_err}"));
        if req_err.is_timeout() { parts.push("(timeout)".into()); }
        if req_err.is_connect() { parts.push("(connect)".into()); }
        if req_err.is_redirect() { parts.push("(redirect)".into()); }
        if req_err.is_body() { parts.push("(body)".into()); }
        if req_err.is_decode() { parts.push("(decode)".into()); }
        // Walk source chain
        let mut src = req_err.source();
        while let Some(inner) = src {
            parts.push(format!("← {inner}"));
            src = inner.source();
        }
    }
    parts.join(" ")
}

// ── Ureq Fallback (synchronous, native-tls) ─────────────────────────────

/// Probe via ureq HEAD request (sync, spawn_blocking).
async fn probe_url_ureq(url: &str) -> Result<(u64, bool), DownloadError> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let resp = ureq::head(&url)
            .call()
            .map_err(|e| DownloadError::Other(format!("ureq probe: {e}")))?;
        let len: u64 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let range = resp
            .headers()
            .get("accept-ranges")
            .map(|v| v.to_str().unwrap_or("") == "bytes")
            .unwrap_or(false);
        Ok((len, range))
    })
    .await
    .map_err(|e| DownloadError::Other(format!("ureq probe join: {e}")))?
}

/// Synchronous download via `ureq` with `native-tls`.
/// Used as primary path for simple downloads and fallback for chunked.
fn download_ureq_fallback(task: &DownloadTask, url: &str, total_size: u64) -> Result<PathBuf, String> {
    use std::io::{Read, Write};

    let on_disk: u64 = if task.dest_path.exists() {
        std::fs::metadata(&task.dest_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Already-complete check (handles duplicate task submissions for same
    // file via multiple mirror URLs):
    // - If total_size is known and file size matches → skip download
    // - If file is large enough (±10% tolerance) → skip download
    // - If file exists with > 1KB but total_size is unknown → assume done
    if on_disk > 1024 {
        if total_size > 0 && on_disk >= total_size.saturating_sub(total_size / 10) {
            tracing::debug!(path = %task.dest_path.display(), size = on_disk, total = total_size, "ureq: file already complete (size match)");
            return Ok(task.dest_path.clone());
        }
        if total_size == 0 {
            tracing::debug!(path = %task.dest_path.display(), size = on_disk, "ureq: file exists, skipping (no probe total)");
            return Ok(task.dest_path.clone());
        }
    }

    // If another task is currently downloading the same file and we have
    // a partial file, wait briefly and re-check (up to 30s).
    if on_disk > 0 && on_disk < 1024 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Ok(m) = std::fs::metadata(&task.dest_path) {
            if m.len() > 1024 {
                tracing::debug!(path = %task.dest_path.display(), "ureq: file grew, assuming another task finished");
                return Ok(task.dest_path.clone());
            }
        }
    }

    let mut downloaded = on_disk;

    if let Some(parent) = task.dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }

    let resp = if downloaded > 0 && downloaded < total_size {
        ureq::get(url)
            .header("Range", &format!("bytes={downloaded}-"))
            .call()
            .map_err(|e| format!("ureq range: {e}"))?
    } else {
        ureq::get(url)
            .call()
            .map_err(|e| format!("ureq get: {e}"))?
    };

    let mut file = if downloaded > 0 {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&task.dest_path)
            .map_err(|e| format!("open append: {e}"))?
    } else {
        std::fs::File::create(&task.dest_path)
            .map_err(|e| format!("create dest: {e}"))?
    };

    let mut reader = resp.into_body().into_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut last_log = std::time::Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 { break; }
        file.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
        downloaded += n as u64;

        // Log progress every 5 seconds
        if last_log.elapsed().as_secs() >= 5 {
            if total_size > 0 {
                let pct = (downloaded as f64 / total_size as f64) * 100.0;
                tracing::info!(pct = %format!("{pct:.0}%"), size_mb = downloaded / 1_048_576, "ureq downloading");
            }
            last_log = std::time::Instant::now();
        }
    }
    file.flush().map_err(|e| format!("flush: {e}"))?;

    if downloaded == 0 {
        return Err("empty response (0 bytes)".into());
    }
    if total_size > 0 && downloaded < total_size {
        return Err(format!("incomplete: {downloaded}/{total_size}"));
    }

    tracing::info!(path = %task.dest_path.display(), size_mb = downloaded / 1_048_576, "ureq download complete");
    Ok(task.dest_path.clone())
}

// ── Observer Helper ─────────────────────────────────────────────────────

async fn notify_observers(observers: &ObserverSet, event: DownloadEvent) {
    observers.notify(event).await;
}
