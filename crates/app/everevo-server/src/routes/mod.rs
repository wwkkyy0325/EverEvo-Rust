//! Route modules — each file defines a `pub fn router() -> Router<Arc<AppState>>`.
//!
//! To add a new route module:
//! 1. Add `pub mod <name>;` below
//! 2. Add `.merge(<name>::router())` in `all_routes()`

pub mod bootstrap;
pub mod character_routes;
pub mod chat;
pub mod command_routes;
pub mod config;
pub mod context_routes;
pub mod diary_routes;
pub mod domain_routes;
pub mod health;
pub mod kg_routes;
pub mod mcp_routes;
pub mod memory_routes;
pub mod model_routes;
pub mod sandbox_routes;
pub mod session_routes;
pub mod skills_routes;
pub mod tools_routes;
pub mod workspace_routes;

use crate::app_state::AppState;
use axum::Router;
use std::sync::Arc;

/// Assemble all API routes in one place.
///
/// Every route module is registered here. When adding a new route module,
/// add it to both the `pub mod` list above and the merge chain below.
pub fn all_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(health::router())
        .merge(config::router())
        .merge(character_routes::router())
        .merge(bootstrap::router())
        .merge(chat::router())
        .merge(session_routes::router())
        .merge(sandbox_routes::router())
        .merge(skills_routes::router())
        .merge(domain_routes::router())
        .merge(kg_routes::router())
        .merge(mcp_routes::router())
        .merge(tools_routes::router())
        .merge(workspace_routes::router())
        .merge(diary_routes::router())
        .merge(memory_routes::router())
        .merge(context_routes::router())
        .merge(command_routes::router())
        .merge(model_routes::router())
}
