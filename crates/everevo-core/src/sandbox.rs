//! SandboxProvider trait — the contract for process isolation.
//!
//! Lives in `everevo-core` so any crate can use it without depending on the implementation.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::EverEvoError;

/// Result of a sandboxed execution.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub killed_by_timeout: bool,
    /// When true, the caller MUST present a confirmation dialog to the user
    /// before re-invoking with `confirmed: true`. The sandbox did NOT execute
    /// the command — it requires explicit user approval first.
    pub needs_confirmation: bool,
    /// Human-readable reason why confirmation is required (e.g. "命令匹配危险模式: rm -rf /").
    /// Empty when `needs_confirmation` is false.
    pub confirmation_reason: String,
}

/// Configuration for a single sandboxed execution.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env_vars: HashMap<String, String>,
    pub timeout_secs: u64,
    pub memory_limit_mb: Option<u64>,
    pub network_allowed: bool,
    /// Has the user explicitly confirmed this command? (default: true for
    /// backward compat — set to false to require confirmation for dangerous commands)
    pub confirmed: bool,
}

impl ExecutionConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: vec![],
            working_dir: None,
            env_vars: HashMap::new(),
            timeout_secs: 30,
            memory_limit_mb: None,
            network_allowed: true,
            confirmed: false,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }
    pub fn with_env(mut self, key: &str, val: &str) -> Self {
        self.env_vars.insert(key.into(), val.into());
        self
    }
    pub fn with_network(mut self, allowed: bool) -> Self {
        self.network_allowed = allowed;
        self
    }
    pub fn with_memory_limit(mut self, mb: u64) -> Self {
        self.memory_limit_mb = Some(mb);
        self
    }
    /// Mark this command as user-confirmed — bypasses the SemiAuto confirmation gate.
    pub fn with_confirmed(mut self, confirmed: bool) -> Self {
        self.confirmed = confirmed;
        self
    }
}

/// Abstract sandbox — every isolation tier implements this.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Execute a command in an isolated environment.
    async fn execute(&self, config: &ExecutionConfig) -> Result<ExecutionResult, EverEvoError>;
}
