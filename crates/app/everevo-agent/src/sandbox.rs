//! Sandbox re-exports — thin wrapper around everevo-sandbox.
//!
//! The sandbox implementation lives in the `everevo-sandbox` crate
//! (designed as an independent service boundary). This module provides
//! convenience re-exports so agent code doesn't need to know the crate name.

pub use everevo_core::sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
pub use everevo_sandbox::{SandboxConfig, Shell, ShellKind, ShellResolver, TieredSandbox};
