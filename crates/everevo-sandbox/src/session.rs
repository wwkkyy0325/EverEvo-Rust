//! Per-session sandbox — isolated working directory + audit trail.
//!
//! Each conversation session gets its own `SessionSandbox`:
//! ```text
//! data/sandbox/{session_id}/
//!   ├── audit.jsonl        ← append-only execution audit trail
//!   ├── work/              ← working directory for tool commands
//!   └── ...                ← tool-generated files
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use everevo_core::sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
use everevo_core::EverEvoError;

use crate::audit::AuditWriter;
use crate::config::SandboxConfig;
use crate::permission::{PermissionDecision, PermissionLevel};
use crate::provider::TieredSandbox;

/// Per-session sandbox with isolated filesystem and persistent audit trail.
pub struct SessionSandbox {
    session_id: String,
    sandbox_dir: PathBuf,
    work_dir: PathBuf,
    /// When set, paths inside the workspace are auto-approved at SemiAuto level.
    /// Claude Code alignment: inside workspace = free, outside = confirm.
    workspace_path: Option<PathBuf>,
    engine: Arc<TieredSandbox>,
    audit: Arc<AuditWriter>,
    permission_level: PermissionLevel,
}

impl SessionSandbox {
    /// Initialize a sandbox for a session.
    ///
    /// Creates `data/sandbox/{session_id}/` and `work/` subdirectory,
    /// then opens the audit writer.
    pub fn create(session_id: &str, base_config: &SandboxConfig) -> Result<Self, EverEvoError> {
        let sandbox_dir = base_config.sandbox_root.join(session_id);
        let work_dir = sandbox_dir.join("work");

        std::fs::create_dir_all(&work_dir).map_err(|e| {
            EverEvoError::Sandbox(format!(
                "Failed to create sandbox dir {}: {e}",
                sandbox_dir.display()
            ))
        })?;

        let audit = Arc::new(AuditWriter::open(&sandbox_dir).map_err(EverEvoError::Sandbox)?);

        // Sandbox inherits host HOME + git config directly — no isolation.
        // This eliminates ambiguity: git, ssh, and other tools behave exactly
        // as they do in the host terminal.
        let sess_config = SandboxConfig {
            sandbox_root: sandbox_dir.clone(),
            ..base_config.clone()
        };
        let engine = Arc::new(TieredSandbox::new(sess_config)?);

        Ok(Self {
            session_id: session_id.to_string(),
            sandbox_dir,
            work_dir,
            workspace_path: None,
            engine,
            audit,
            permission_level: PermissionLevel::SemiAuto,
        })
    }

    /// Override the auto-created work_dir with a user-specified workspace.
    /// When `path` is Some and exists, the sandbox uses it as the working directory.
    /// Also sets workspace_path for permission auto-approval (Claude Code alignment).
    /// Fallback: when None or invalid, keep the original sandbox work_dir.
    pub fn with_workspace(mut self, path: Option<std::path::PathBuf>) -> Self {
        if let Some(ref p) = path {
            if p.is_dir() {
                tracing::info!(
                    workspace = %p.display(),
                    session = %self.session_id,
                    "Session sandbox bound to workspace"
                );
                self.work_dir = p.clone();
                self.workspace_path = Some(p.clone());
            } else {
                tracing::warn!(
                    path = %p.display(),
                    session = %self.session_id,
                    "Workspace path does not exist or is not a directory — using sandbox fallback"
                );
            }
        }
        self
    }

    /// Run a command through the sandbox, writing an audit record on completion.
    pub async fn execute(&self, ec: &ExecutionConfig) -> Result<ExecutionResult, EverEvoError> {
        // Force working directory into session sandbox
        let mut ec = ec.clone();
        ec.working_dir = Some(self.work_dir.clone());

        let result = self.engine.execute(&ec).await?;

        // Write audit record
        let records = self.engine.audit_log();
        for r in &records {
            self.audit.write(r);
        }

        Ok(result)
    }

    /// Pre-check a command against current permission rules.
    /// Returns Allow / Deny / Confirm — callers should handle Confirm
    /// by presenting a confirmation UI before calling execute().
    pub fn check_command(&self, command: &str) -> PermissionDecision {
        self.engine.check(command)
    }

    /// Upgrade permission level for this session (e.g., user clicked "allow").
    pub fn set_permission_level(&mut self, level: PermissionLevel) {
        self.permission_level = level;
        self.engine.set_permission_level(level); // &self via Mutex
    }

    /// Current permission level.
    pub fn permission_level(&self) -> PermissionLevel {
        self.permission_level
    }

    /// Add a path to the trusted paths list (user-approved).
    /// Trusted paths bypass SemiAuto external-path denial.
    pub fn trust_path(&self, pattern: &str) {
        self.engine
            .rules_mut()
            .trusted_paths
            .push(pattern.to_string());
        tracing::info!(session = %self.session_id, %pattern, "Path trusted by user");
    }

    /// Get current trusted paths.
    pub fn trusted_paths(&self) -> Vec<String> {
        self.engine.rules().trusted_paths.clone()
    }

    /// Add paths to the write allowlist (e.g., for environment setup).
    pub fn allow_write_path(&mut self, pattern: &str) {
        self.engine
            .rules_mut()
            .filesystem_write_allowlist
            .push(pattern.to_string());
        tracing::info!(session = %self.session_id, %pattern, "Write allowlist expanded");
    }

    /// Session identifier.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Sandbox root path.
    pub fn sandbox_dir(&self) -> &PathBuf {
        &self.sandbox_dir
    }

    /// Working directory for tool commands.
    pub fn work_dir(&self) -> &PathBuf {
        &self.work_dir
    }

    /// Audit trail path.
    pub fn audit_path(&self) -> &PathBuf {
        self.audit.path()
    }

    /// Number of audit records written.
    pub fn audit_count(&self) -> usize {
        self.audit.count()
    }

    /// Clean up the sandbox directory entirely.
    pub fn destroy(self) -> Result<(), EverEvoError> {
        // Drop audit writer first (flushes any buffered data)
        drop(self.audit);
        std::fs::remove_dir_all(&self.sandbox_dir)
            .map_err(|e| EverEvoError::Sandbox(format!("cleanup: {e}")))
    }

    /// Get a reference to the underlying sandbox engine.
    pub fn engine(&self) -> &TieredSandbox {
        &self.engine
    }

    /// Get the sandbox as a provider trait object (for tool injection).
    pub fn provider(&self) -> Arc<dyn SandboxProvider> {
        self.engine.clone() as Arc<dyn SandboxProvider>
    }

    /// Flush in-memory audit records from the engine to the JSONL file.
    /// Call this after tool execution to persist the audit trail.
    pub fn flush_audit(&self) {
        let records = self.engine.audit_log();
        for r in &records {
            self.audit.write(r);
        }
        if !records.is_empty() {
            tracing::debug!(count = records.len(), session = %self.session_id, "Audit flushed");
        }
    }
}
