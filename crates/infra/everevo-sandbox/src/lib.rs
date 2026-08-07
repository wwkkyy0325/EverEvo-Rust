//! EverEvo Sandbox — multi-tier process isolation.
//!
//! ## Architecture (designed as independent service boundary)
//!
//! ```text

// Internal implementation uses tokio::process::Command — the disallowed_methods
// lint guards external callers, not the sandbox's own process spawning.
#![allow(clippy::disallowed_methods)]
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

pub mod audit;
mod config;
mod error;
#[cfg(windows)]
mod job_object;
mod limits;
pub mod permission;
mod provider;
mod resolved;
pub mod session;
#[cfg(not(windows))]
mod unix_limits;

pub use audit::AuditRecord;
pub use config::SandboxConfig;
pub use error::SandboxError;
pub use limits::ResourceLimits;
pub use permission::{
    check_permission, command_is_denied, NetworkPolicy, PermissionDecision, PermissionLevel,
    PermissionRules,
};
pub use provider::TieredSandbox;
pub use resolved::{Shell, ShellKind, ShellResolver};
pub use session::SessionSandbox;

pub use everevo_core::sandbox::SandboxProvider;
