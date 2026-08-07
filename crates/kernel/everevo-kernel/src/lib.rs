//! EverEvo Microkernel — immutable core.
//!
//! ## What lives in the kernel
//!
//! - **Plugin management**: PluginRegistry, VersionStore, ProcessPool, CanaryRouter
//! - **Bootstrap tools**: shell, read_file, write_file, plugin_status, plugin_rollback
//!   (compiled into kernel binary, never removable — self-repair guarantee)
//!
//! ## What does NOT live in the kernel
//!
//! - Tool implementations (in plugins/)
//! - ContextStage implementations (in plugins/stages/)
//! - ToolHook implementations (in plugins/hooks/)
//! - HTTP routes (in everevo-server)
//! - LLM client (in everevo-agent)
//! - Database (in everevo-db)
//!
//! ## Re-export policy
//!
//! The kernel re-exports key types from `everevo-core` and `everevo-agent` so
//! that `everevo-server` can depend primarily on `everevo-kernel` rather than
//! having to know about every internal crate.

pub mod bootstrap;
pub mod init;
pub mod plugin;
pub mod protection;

// ── Re-exports from everevo-core (shared types) ────────────────────────
pub use everevo_core::context::{
    ContextBuildContext, ContextFragment, ContextPipeline, ContextSnapshot, ContextStage,
};
pub use everevo_core::error::{ApiError, ErrorCode};
pub use everevo_core::tool::{Tool, ToolHook, ToolOutput, ToolRegistry};
pub use everevo_core::types::RiskLevel;

// ── Plugin management ──────────────────────────────────────────────────
pub use plugin::build::{compile_and_stage, BuildConfig, BuildResult};
pub use plugin::canary::CanaryRouter;
pub use plugin::pool::ProcessPool;
pub use plugin::registry::PluginRegistry;
pub use plugin::version::VersionStore;
