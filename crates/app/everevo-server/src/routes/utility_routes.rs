//! Utility routes — misc small endpoints (2026-08-13 restructure).
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app_state::AppState;
use everevo_agent::AgentCharacter;
use everevo_core::ApiError;
use uuid::Uuid;

#[derive(Serialize)]
struct CommandsResponse {
    commands: Vec<CommandEntry>,
}

#[derive(Serialize)]
struct CommandEntry {
    name: String,
    description: String,
    display: String,
}

/// GET /api/commands — list all registered slash commands.
async fn list_commands(State(state): State<Arc<AppState>>) -> Json<CommandsResponse> {
    let commands = state
        .commands
        .list()
        .iter()
        .map(|c| CommandEntry {
            name: c.name.clone(),
            description: c.description.clone(),
            display: c.display(),
        })
        .collect();
    Json(CommandsResponse { commands })
}

fn command_routes_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/commands", axum::routing::get(list_commands))
}

fn context_routes_router() -> Router<Arc<AppState>> {
    Router::new().route("/api/sessions/{id}/context", get(get_latest_snapshot))
}

async fn get_latest_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let snapshots = state.context_snapshots.read().await;
    let latest = snapshots.get(&id).and_then(|v| v.last()).cloned();

    match latest {
        Some(snapshot) => {
            Ok(Json(serde_json::to_value(&snapshot).map_err(|e| {
                ApiError::internal(format!("serialization failed: {e}"))
            })?))
        }
        None => Err(ApiError::not_found(
            "No context snapshot available for this session",
        )),
    }
}

fn character_routes_router() -> Router<Arc<AppState>> {
    Router::new().route("/api/character", get(get_character).put(put_character))
}

fn character_path(state: &AppState) -> std::path::PathBuf {
    state
        .config
        .data_dir
        .join("memory")
        .join("agent")
        .join("character.json")
}

/// Return the current character profile (auto-creating the default on first read).
async fn get_character(State(state): State<Arc<AppState>>) -> Json<AgentCharacter> {
    let path = character_path(&state);
    let profile = everevo_agent::load_character(&path).unwrap_or_default();
    Json(profile)
}

/// Persist an edited character profile. Validates that `name` is non-empty.
async fn put_character(
    State(state): State<Arc<AppState>>,
    Json(profile): Json<AgentCharacter>,
) -> Result<Json<AgentCharacter>, ApiError> {
    if profile.name.trim().is_empty() {
        return Err(ApiError::bad_request("`name` must not be empty"));
    }
    let path = character_path(&state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::internal(format!("failed to create dir: {e}")))?;
    }
    let json = serde_json::to_string_pretty(&profile)
        .map_err(|e| ApiError::internal(format!("failed to serialize character: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| ApiError::internal(format!("failed to write character: {e}")))?;
    tracing::info!(path = %path.display(), "Agent character profile updated via API");
    Ok(Json(profile))
}

/// Path to the persisted workspace config file.
fn workspace_config_path(state: &AppState) -> PathBuf {
    state.config.config_dir.join("workspace.json")
}

fn workspace_routes_router() -> Router<Arc<AppState>> {
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

pub fn router() -> axum::Router<Arc<crate::app_state::AppState>> {
    axum::Router::new()
        .merge(command_routes_router())
        .merge(context_routes_router())
        .merge(character_routes_router())
        .merge(workspace_routes_router())
}
