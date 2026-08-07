//! Plugin management — versioning, process pool, canary routing.
//!
//! ## Architecture
//!
//! ```text
//! PluginRegistry (coordinator)
//!   ├── VersionStore     → filesystem-based version management
//!   ├── ProcessPool      → persistent subprocess reuse
//!   └── CanaryRouter     → canary routing + auto promote/rollback
//! ```

pub mod build;
pub mod canary;
pub mod pool;
pub mod registry;
pub mod version;
