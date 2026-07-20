//! Health check endpoint.

use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::sync::Arc;

use crate::app_state::AppState;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/health", get(handler))
}

async fn handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
