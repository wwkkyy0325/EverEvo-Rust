//! EverEvo Axum server — app builder, shared state, and route wiring.
//!
//! ## Architecture docs (llmwiki)
//!
//! - [00-overview.md](../../../../docs/llmwiki/architecture/00-overview.md) — §4 入口管线 / §5 决策 1/2/4
//! - [01-entry-pipelines.md](../../../../docs/llmwiki/architecture/01-entry-pipelines.md) — §1 一次用户消息的完整旅程
//!   (SSE 内容块、回放、特殊 provider 路由)
//! - [05-orchestration.md](../../../../docs/llmwiki/architecture/05-orchestration.md) — SessionCoordinator、ContentBlockStreamer、auto-continue
//! - [06-tool-system.md](../../../../docs/llmwiki/architecture/06-tool-system.md) — assemble 装配、权限分层
//! - [07-bootstrap.md](../../../../docs/llmwiki/architecture/07-bootstrap.md) — 首启供给、MCP、A2A、workflow
//! - [08-config-and-state.md](../../../../docs/llmwiki/architecture/08-config-and-state.md) — AppState、多 Provider 路由

pub mod app_state;
pub mod main_impl;
pub mod middleware;
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

// ── Embedded frontend (compiled into binary for single-exe distribution) ──

use rust_embed::RustEmbed;

/// Frontend build output embedded at compile time.
/// Path relative to this crate's Cargo.toml (crates/everevo-server/).
#[derive(RustEmbed)]
#[folder = "../../../frontend/dist/"]
struct FrontendAssets;

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

    let api_routes = routes::all_routes();

    // Serve frontend: disk first (dev mode), fallback to embedded (single-exe)
    let dist = std::path::Path::new("frontend/dist");
    let router = if dist.join("index.html").exists() {
        tracing::info!("Serving frontend from frontend/dist/ (dev mode)");
        api_routes.fallback_service(
            ServeDir::new("frontend/dist").fallback(ServeFile::new("frontend/dist/index.html")),
        )
    } else if FrontendAssets::get("index.html").is_some() {
        tracing::info!("Serving frontend from embedded assets (single-exe mode)");
        api_routes.fallback(serve_embedded)
    } else {
        tracing::info!("No frontend found — API-only mode");
        api_routes
    };

    let router = router
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(axum::middleware::from_fn(middleware::panic_recovery))
        .with_state(Arc::clone(&state));

    Ok((router, state))
}

// ── Embedded asset serving ──────────────────────────────────────────────

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;

async fn serve_embedded(req: Request<Body>) -> Response<Body> {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // Try exact path, then SPA fallback to index.html
    let asset = FrontendAssets::get(path).or_else(|| FrontendAssets::get("index.html"));

    match asset {
        Some(file) => {
            let mime = mime_type(path);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "public, max-age=3600")
                .body(Body::from(file.data))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap(),
    }
}

fn mime_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
