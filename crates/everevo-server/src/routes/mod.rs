//! Route modules — each file defines a `pub fn router() -> Router<Arc<AppState>>`.

pub mod bootstrap;
pub mod chat;
pub mod config;
pub mod domain_routes;
pub mod health;
pub mod kg_routes;
pub mod sandbox_routes;
pub mod session_routes;
