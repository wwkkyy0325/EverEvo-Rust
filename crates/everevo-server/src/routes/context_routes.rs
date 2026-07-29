//! Context injection observability endpoint — returns the latest context
//! snapshot for a session, showing what each stage contributed.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::app_state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/sessions/{id}/context", get(get_latest_snapshot))
}

async fn get_latest_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let snapshots = state.context_snapshots.read().await;
    let latest = snapshots.get(&id).and_then(|v| v.last()).cloned();

    match latest {
        Some(snapshot) => Json(serde_json::to_value(&snapshot).unwrap_or_else(|_| {
            serde_json::json!({"error": "serialization failed"})
        })),
        None => Json(serde_json::json!({
            "error": "No context snapshot available for this session",
            "hint": "Send a message first to trigger context assembly"
        })),
    }
}
