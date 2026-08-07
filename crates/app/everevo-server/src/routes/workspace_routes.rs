//! Workspace management API — set and query the primary working directory.
//!
//! Claude Code alignment: the workspace is the root directory for all file
//! operations, shown prominently in the system prompt's Environment block.
//! Persists to data/config/workspace.json so it survives server restarts.

use crate::app_state::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use everevo_core::ApiError;
use std::path::PathBuf;
use std::sync::Arc;

/// Path to the persisted workspace config file.
fn workspace_config_path(state: &AppState) -> PathBuf {
    state.config.config_dir.join("workspace.json")
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/workspace", get(get_workspace).put(set_workspace))
}

async fn get_workspace(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let ws = state.workspace_dir.read().await;
    Json(match ws.as_ref() {
        Some(p) => serde_json::json!({
            "path": p.display().to_string(),
            "exists": p.is_dir(),
        }),
        None => serde_json::json!({
            "path": null,
            "exists": false,
        }),
    })
}

async fn set_workspace(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path_str = body["path"].as_str().unwrap_or("");
    if path_str.is_empty() {
        *state.workspace_dir.write().await = None;
        // Persist cleared state
        let config_path = workspace_config_path(&state);
        let _ = std::fs::remove_file(&config_path);
        return Ok(Json(serde_json::json!({ "ok": true, "path": null })));
    }
    let path = PathBuf::from(path_str);
    if !path.is_dir() {
        return Err(ApiError::bad_request(format!(
            "Path does not exist or is not a directory: {}",
            path.display()
        )));
    }
    *state.workspace_dir.write().await = Some(path.clone());

    // Persist to data/config/workspace.json
    let config_path = workspace_config_path(&state);
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let saved = serde_json::json!({
        "workspace_dir": path.display().to_string(),
    });
    let _ = std::fs::write(
        &config_path,
        serde_json::to_string_pretty(&saved).unwrap_or_default(),
    );

    Ok(Json(serde_json::json!({
        "ok": true,
        "path": path.display().to_string(),
        "exists": true,
    })))
}
