//! Route modules — each file defines a `pub fn router() -> Router<Arc<AppState>>`.

pub mod bootstrap;
pub mod chat;
pub mod config;
pub mod diary_routes;
pub mod domain_routes;
pub mod memory_routes;
pub mod health;
pub mod kg_routes;
pub mod mcp_routes;
pub mod sandbox_routes;
pub mod session_routes;
pub mod tools_routes;
pub mod workspace_routes;
