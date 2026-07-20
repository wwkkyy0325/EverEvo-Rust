//! EverEvo Sandbox — multi-tier process isolation.
//!
//! ## Architecture (designed as independent service boundary)
//!
//! ```text
//!                    ┌─────────────────────────┐
//!                    │   SandboxProvider trait │  ← everevo-core
//!                    └───────────┬─────────────┘
//!                                │
//!         ┌──────────────────────┼──────────────────────┐
//!         │                      │                      │
//!   ┌─────┴──────┐    ┌─────────┴─────────┐    ┌───────┴──────┐
//!   │ WSL Sandbox│    │Job Objects Sandbox │    │FS-Only Sandbox│
//!   │ (strongest) │    │ (OS-level limits)  │    │ (path confine)│
//!   └────────────┘    └───────────────────┘    └──────────────┘
//!         ↑                      ↑                      ↑
//!         └──────────────────────┴──────────────────────┘
//!                    TieredSandbox::resolve() → first available
//! ```
//!
//! ## Security Layers (Windows)
//!
//! 1. **WSL**: full Linux kernel isolation (if available)
//! 2. **Job Objects**: process-tree containment, memory/CPU limits, KILL_ON_JOB_CLOSE
//! 3. **Filesystem**: per-session `data/sandbox/{session_id}/` tmp dir
//! 4. **Always applied**: timeout, PATH injection, env var allowlist
//!
//! ## References
//!
//! - Arapuca (cross-platform sandbox): AppContainer + Job Objects + restricted tokens
//! - rappct (Windows AppContainer): Rust API for LPAC process launch
//! - wasmtime: fuel metering + capability-based security
//! - Docker: cgroups v2 + seccomp + user namespaces

mod config;
mod error;
mod limits;
pub mod permission;
mod provider;
mod resolved;
pub mod audit;
pub mod session;
#[cfg(windows)] mod job_object;
#[cfg(not(windows))] mod unix_limits;

pub use config::SandboxConfig;
pub use error::SandboxError;
pub use limits::ResourceLimits;
pub use permission::{
    PermissionLevel, PermissionRules, PermissionDecision, NetworkPolicy,
    command_is_denied, check_permission, extract_paths, is_path_allowed, glob_match,
};
pub use provider::{TieredSandbox, AuditRecord};
pub use resolved::{ShellResolver, Shell, ShellKind};
pub use session::SessionSandbox;

pub use everevo_core::sandbox::SandboxProvider;
