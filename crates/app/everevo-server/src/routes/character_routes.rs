//! Character API — read/write the agent's own voice profile (`character.json`).
//!
//! Mirrors [`config`](crate::routes::config). The editor UI (Settings → Character)
//! round-trips the `AgentCharacter` through these endpoints.

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};

use crate::app_state::AppState;
use everevo_agent::AgentCharacter;
use everevo_core::ApiError;

pub fn router() -> Router<Arc<AppState>> {
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
