//! EverEvo Axum server — app builder, shared state, and route wiring.

pub mod app_state;
pub mod main_impl;
pub mod orchestration;
pub mod routes;
pub mod sandbox_tool;
pub mod startup_check;

use std::sync::Arc;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::app_state::AppState;
use everevo_core::{AppConfig, EverEvoError};
use everevo_db::Database;

/// Build the Axum application with all routes and middleware.
///
/// Returns the router and a shared handle to the application state so the
/// caller can start background tasks (e.g. the dreaming scheduler) and
/// perform graceful shutdown.
pub async fn build_app(
    config: AppConfig,
    db: Database,
) -> Result<(Router, Arc<AppState>), EverEvoError> {
    let state = AppState::new(config, db).await?;

    let cors = {
        let allowed_origins: Vec<axum::http::HeaderValue> = std::env::var("EVEREVO_CORS_ORIGINS")
            .ok()
            .map(|s| s.split(',').filter_map(|o| o.trim().parse().ok()).collect())
            .filter(|v: &Vec<_>| !v.is_empty())
            .unwrap_or_else(|| vec!["http://localhost:3000".parse().unwrap()]);
        CorsLayer::new()
            .allow_origin(allowed_origins)
            .allow_methods(Any)
            .allow_headers(Any)
    };

    let api_routes = Router::new()
        .merge(routes::health::router())
        .merge(routes::config::router())
        .merge(routes::bootstrap::router())
        .merge(routes::chat::router())
        .merge(routes::session_routes::router())
        .merge(routes::sandbox_routes::router())
        .merge(routes::domain_routes::router())
        .merge(routes::kg_routes::router())
        .merge(routes::mcp_routes::router())
        .merge(routes::tools_routes::router())
        .merge(routes::workspace_routes::router())
        .merge(routes::diary_routes::router())
        .merge(routes::memory_routes::router());

    // Serve frontend static files + SPA fallback if built
    let dist = std::path::Path::new("frontend/dist");
    let router = if dist.join("index.html").exists() {
        tracing::info!("Serving frontend from frontend/dist/");
        api_routes.fallback_service(
            ServeDir::new("frontend/dist").fallback(ServeFile::new("frontend/dist/index.html")),
        )
    } else {
        api_routes
    };

    let router = router
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)) // 1MB cap
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&state));

    Ok((router, state))
}
