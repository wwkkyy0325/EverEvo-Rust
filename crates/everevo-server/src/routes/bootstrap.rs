//! Bootstrap routes — status check + download with SSE progress.

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Json, Router,
};
use futures::Stream;
use std::convert::Infallible;
use std::sync::Arc;
use crate::app_state::AppState;

// ── Routes ──────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/bootstrap/status", get(status_handler))
        .route("/api/bootstrap/download", get(download_handler))
}

// ── Status ──────────────────────────────────────────────────────────────

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let status = state.bootstrap.check().await.unwrap_or_else(|e| {
        tracing::error!("Bootstrap check failed: {e}");
        everevo_bootstrap::BootstrapResult {
            ready: vec![],
            missing: vec![],
            corrupt: vec![],
            download_size_bytes: 0,
        }
    });
    Json(build_status_json(&status))
}

// ── Download (SSE) ──────────────────────────────────────────────────────
//
// This is a thin wrapper: it subscribes to InitPipeline events, spawns
// pipeline.run() in the background, and converts every InitEvent into an
// SSE event that the frontend already understands.

async fn download_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let pipeline = state.init_pipeline.clone();

    let stream = async_stream::stream! {
        // Subscribe BEFORE spawning so we catch AllDone from the marker fast-path.
        let mut events = pipeline.events();

        let _handle = tokio::spawn(async move {
            if let Err(e) = pipeline.run().await {
                tracing::error!(%e, "InitPipeline failed");
            }
        });

        loop {
            match events.recv().await {
                Ok(everevo_bootstrap::pipeline::InitEvent::Checking) => {
                    yield sse("checking", serde_json::json!({}));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::FoundMissing { total, total_bytes }) => {
                    yield sse("start", serde_json::json!({
                        "total": total,
                        "total_mb": total_bytes / 1_048_576,
                    }));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::DownloadProgress { key, percentage, speed_mb }) => {
                    yield sse("progress", serde_json::json!({
                        "key": key, "percentage": percentage, "speed_mb": speed_mb,
                    }));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::LayerStart { key, layer, .. }) => {
                    // layer 2 = extraction → emit "extracting" for frontend
                    if layer == 2 {
                        yield sse("extracting", serde_json::json!({"key": key}));
                    }
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::LayerDone { key, layer: _, total_layers: _, is_asset_done: _ }) => {
                    yield sse("extracted", serde_json::json!({"key": key, "status": "ok"}));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::AssetDone { key, completed, total }) => {
                    yield sse("asset_done", serde_json::json!({
                        "key": key, "completed": completed, "total": total,
                    }));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::AssetFailed { key, error, .. }) => {
                    yield sse("asset_failed", serde_json::json!({"key": key, "error": error}));
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::AllDone) => {
                    yield sse("done", serde_json::json!({"message": "all ready"}));
                    break;
                }
                Ok(everevo_bootstrap::pipeline::InitEvent::FatalError(e)) => {
                    yield sse("error", serde_json::json!({"error": e}));
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "SSE event lag");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Build an SSE Event from an event name and a JSON value.
fn sse(name: &str, data: serde_json::Value) -> Result<Event, Infallible> {
    Ok(Event::default().event(name).data(data.to_string()))
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn build_status_json(
    status: &everevo_bootstrap::BootstrapResult,
) -> serde_json::Value {
    let assets: Vec<serde_json::Value> = status
        .ready
        .iter()
        .map(|r| asset_json(&r.key, &r.version, "ready", 0, ""))
        .chain(status.missing.iter().map(|m| {
            asset_json(&m.key, &m.version, "missing", m.size_bytes / 1_048_576, &m.description)
        }))
        .chain(status.corrupt.iter().map(|c| {
            asset_json(&c.key, &c.version, "corrupt", 0, "checksum mismatch")
        }))
        .collect();

    serde_json::json!({
        "all_ready": status.missing.is_empty() && status.corrupt.is_empty(),
        "ready_count": status.ready.len(),
        "missing_count": status.missing.len(),
        "corrupt_count": status.corrupt.len(),
        "total_download_mb": status.download_size_bytes / 1_048_576,
        "assets": assets,
    })
}

fn asset_json(key: &str, version: &str, status: &str, size_mb: u64, desc: &str) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "name": asset_display_name(key),
        "version": version,
        "category": asset_category(key),
        "status": status,
        "size_mb": size_mb,
        "description": desc,
    })
}

fn asset_display_name(key: &str) -> String {
    match key {
        "python" => "Python 3.12 (Portable)".into(),
        "node" => "Node.js 22 (Portable)".into(),
        "git" => "Git (MinGit)".into(),
        "onnxruntime" => "ONNX Runtime".into(),
        "bge-small-zh" => "BGE-small-zh (中文句向量)".into(),
        "all-MiniLM-L6-v2" => "all-MiniLM-L6-v2 (英文句向量)".into(),
        _ => key.into(),
    }
}

fn asset_category(key: &str) -> String {
    match key {
        "python" | "node" | "git" | "onnxruntime" => "runtime".into(),
        _ => "model".into(),
    }
}
