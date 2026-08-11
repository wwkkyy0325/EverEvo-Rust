//! Plugin Registry — central coordinator for plugin lifecycle.
//!
//! Owns the VersionStore, ProcessPool, and CanaryRouter. Provides the
//! primary API for the kernel to discover, spawn, and manage plugins.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use everevo_core::tool::ToolRegistry;
use everevo_mcp::adapter::McpTool;
use uuid::Uuid;

use super::canary::{spawn_canary_safety_loop, CanaryRouter};
use super::pool::ProcessPool;
use super::version::VersionStore;

// ── Registry ────────────────────────────────────────────────────────────

/// Central plugin registry coordinating version management, process pooling,
/// and canary routing.
pub struct PluginRegistry {
    store: Arc<VersionStore>,
    pool: Arc<ProcessPool>,
    router: Arc<CanaryRouter>,
    plugins_dir: PathBuf,
}

impl PluginRegistry {
    /// Open the plugin registry at the given directory.
    pub async fn open(
        plugins_dir: impl Into<PathBuf>,
    ) -> Result<Self, super::version::VersionError> {
        let plugins_dir: PathBuf = plugins_dir.into();
        std::fs::create_dir_all(&plugins_dir)?;

        let store = Arc::new(VersionStore::open(&plugins_dir)?);
        let pool = Arc::new(ProcessPool::default_settings());
        let router = Arc::new(CanaryRouter::new(Arc::clone(&store)));

        Ok(Self {
            store,
            pool,
            router,
            plugins_dir,
        })
    }

    /// Register a plugin's tools into the given ToolRegistry.
    ///
    /// Spawns the plugin binary, performs MCP handshake, discovers tools,
    /// and wraps them as Tool trait objects via McpTool::from_defs().
    pub async fn register_plugin_tools(
        &self,
        plugin_id: &str,
        session_id: Uuid,
        registry: &mut ToolRegistry,
    ) -> Result<usize, String> {
        let config = self
            .store
            .load_config(plugin_id)
            .map_err(|e| format!("load config for '{plugin_id}': {e}"))?;

        let version = self.store.resolve(&config, session_id);
        let exe_path = self.store.exe_path(plugin_id, &version);

        if !exe_path.exists() {
            return Err(format!(
                "Plugin '{plugin_id}' version '{version}' binary not found at {}",
                exe_path.display()
            ));
        }

        // Verify checksum before spawning
        self.store
            .verify_checksum(plugin_id, &version)
            .map_err(|e| {
                format!("checksum verification failed for '{plugin_id}@{version}': {e}")
            })?;

        // Get or spawn MCP client
        let client = self.pool.acquire(plugin_id, &version, &exe_path).await?;

        // Discover tools and wrap as Tool trait objects
        let tools = {
            let c = client.lock().await;
            McpTool::from_defs(Arc::clone(&client), &c.tools)
        };

        // Register all discovered tools
        let count = tools.len();
        for tool in tools {
            registry.register(tool);
        }

        tracing::info!(
            %plugin_id,
            %version,
            tool_count = count,
            "Plugin tools registered"
        );

        Ok(count)
    }

    /// Record a tool call result for metrics.
    pub fn record_call(&self, plugin_id: &str, version: &str, success: bool, latency_ms: u64) {
        if let Err(e) = self
            .store
            .record_call(plugin_id, version, success, latency_ms)
        {
            tracing::warn!(%plugin_id, %version, error = %e, "Failed to record plugin metrics");
        }
    }

    /// Start the canary safety loop as a background task.
    pub fn start_safety_loop(&self, plugin_ids: Vec<String>) {
        let router = Arc::clone(&self.router);
        tokio::spawn(async move {
            spawn_canary_safety_loop(router, plugin_ids, std::time::Duration::from_secs(60)).await;
        });
    }

    /// Promote a canary to stable.
    pub fn promote(&self, plugin_id: &str) -> Result<(), super::version::VersionError> {
        let config = self.store.load_config(plugin_id)?;
        if let Some(ref canary_ver) = config.canary {
            // Drain idle processes for the canary version
            let pool = Arc::clone(&self.pool);
            let pid = plugin_id.to_string();
            let ver = canary_ver.clone();
            tokio::spawn(async move {
                pool.drain_version(&pid, &ver).await;
            });
        }
        self.store.promote(plugin_id)
    }

    /// Emergency rollback: remove canary, keep stable.
    pub fn rollback(&self, plugin_id: &str) -> Result<(), super::version::VersionError> {
        let config = self.store.load_config(plugin_id)?;
        if let Some(ref canary_ver) = config.canary {
            let pool = Arc::clone(&self.pool);
            let pid = plugin_id.to_string();
            let ver = canary_ver.clone();
            tokio::spawn(async move {
                pool.drain_version(&pid, &ver).await;
            });
        }
        self.store.rollback(plugin_id)
    }

    // ── Accessors ──────────────────────────────────────────────────────

    pub fn store(&self) -> &VersionStore {
        &self.store
    }

    pub fn router(&self) -> &CanaryRouter {
        &self.router
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}
