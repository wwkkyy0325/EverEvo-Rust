//! Route modules — each file defines a `pub fn router() -> Router<Arc<AppState>>`.
//!
//! To add a new route module:
//! 1. Add `pub mod <name>;` below
//! 2. Add `.merge(<name>::router())` in `all_routes()`
//!
//! Micro-route modules are grouped by domain (2026-08-13 restructure):
//! `system_routes` (health/model/mcp/tools), `knowledge_routes` (kg/diary),
//! `utility_routes` (command/context/character/workspace).

pub mod bootstrap;
pub mod chat;
pub mod config;
pub mod domain_routes;
pub mod knowledge_routes;
pub mod memory_routes;
pub mod sandbox_routes;
pub mod session_routes;
pub mod skills_routes;
pub mod system_routes;
pub mod utility_routes;

use crate::app_state::AppState;
use axum::Router;
use std::sync::Arc;

/// Assemble all API routes in one place.
///
/// Every route module is registered here. When adding a new route module,
/// add it to both the `pub mod` list above and the merge chain below.
pub fn all_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(system_routes::router())
        .merge(config::router())
        .merge(bootstrap::router())
        .merge(chat::router())
        .merge(session_routes::router())
        .merge(sandbox_routes::router())
        .merge(skills_routes::router())
        .merge(domain_routes::router())
        .merge(knowledge_routes::router())
        .merge(memory_routes::router())
        .merge(utility_routes::router())
}
