//! Slash command listing endpoint — frontend calls this for autocomplete.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::app_state::AppState;

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

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/commands", axum::routing::get(list_commands))
}
