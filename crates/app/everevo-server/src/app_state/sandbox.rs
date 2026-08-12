use everevo_core::EverEvoError;
use everevo_sandbox::{PermissionLevel, SandboxConfig, SessionSandbox};

use super::AppState;

impl AppState {
    /// Create a sandbox for a session. If `session_workspace` is provided, it
    /// takes precedence over the global workspace_dir for this session only.
    /// Default (None/null) uses the isolated sandbox directory.
    pub async fn create_sandbox(
        &self,
        session_id: uuid::Uuid,
        level: PermissionLevel,
        session_workspace: Option<String>,
    ) -> Result<(), EverEvoError> {
        let sandbox_root = self.config.data_dir.join("sandbox");
        // Only use per-session workspace if explicitly set.
        // New sessions default to sandbox isolation — NO fallback to global.
        let effective_ws: Option<std::path::PathBuf> = session_workspace
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);
        // Add effective workspace to injected_paths for auto-approval
        let mut injected_paths = self.runtime_env.paths.clone();
        if let Some(ref ws) = effective_ws {
            if ws.is_dir() {
                injected_paths.push(ws.clone());
            }
        }
        // Sandbox inherits host env vars directly (no credential injection needed)
        let injected_env: Vec<(String, String)> =
            self.runtime_env.env_vars.clone().into_iter().collect();
        let base_config = SandboxConfig {
            sandbox_root,
            injected_paths,
            injected_env,
            ..Default::default()
        };
        let mut sandbox = SessionSandbox::create(&session_id.to_string(), &base_config)?
            .with_workspace(effective_ws);
        sandbox.set_permission_level(level);
        self.sandboxes.write().await.insert(session_id, sandbox);
        Ok(())
    }

    /// Kill all active sandbox processes on server shutdown.
    /// Sessions can be resumed after restart — sandboxes are recreated lazily.
    pub async fn destroy_all_sandboxes(&self) {
        let mut sandboxes = self.sandboxes.write().await;
        let count = sandboxes.len();
        for (id, sandbox) in sandboxes.drain() {
            if let Err(e) = sandbox.destroy() {
                tracing::warn!(%id, error = %e, "Sandbox cleanup failed");
            }
        }
        tracing::info!(count, "All sandbox processes terminated");
    }

    /// Destroy a session's sandbox and audit trail. Called when a session is deleted.
    pub async fn destroy_sandbox(&self, session_id: uuid::Uuid) {
        if let Some(sandbox) = self.sandboxes.write().await.remove(&session_id) {
            if let Err(e) = sandbox.destroy() {
                tracing::warn!(%session_id, error = %e, "Failed to clean up sandbox");
            }
        }
    }
}
