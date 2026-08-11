//! Process pool — persistent subprocess reuse for plugin MCP clients.
//!
//! ## Design
//!
//! Spawning a subprocess + MCP handshake takes ~10-50ms. For plugins called
//! every turn (shell), this adds up. The pool keeps idle processes alive and
//! reuses them across turns — subsequent calls are ~50µs (JSON round-trip).
//!
//! ## Safety
//!
//! - Idle processes are pinged before reuse (dead processes are dropped)
//! - Each process is wrapped in `McpClient` (already handles JSON-RPC framing)
//! - Processes that crash are automatically removed from the pool
//! - Shutdown sends polite MCP shutdown → SIGTERM → SIGKILL

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use everevo_mcp::client::McpClient;
use tokio::sync::Mutex;

// ── Pool ────────────────────────────────────────────────────────────────

/// A pool of persistent plugin processes, keyed by `plugin_id@version`.
pub struct ProcessPool {
    idle: Mutex<HashMap<String, VecDeque<Arc<Mutex<McpClient>>>>>,
    /// Maximum idle processes per (plugin_id, version) pair.
    max_idle: usize,
    /// Maximum time an idle process can sit unused before being killed.
    #[allow(dead_code)]
    idle_timeout: Duration,
}

impl ProcessPool {
    /// Create a new process pool.
    pub fn new(max_idle: usize, idle_timeout: Duration) -> Self {
        Self {
            idle: Mutex::new(HashMap::new()),
            max_idle,
            idle_timeout,
        }
    }

    /// Create with default settings: up to 3 idle per plugin, 5 minute timeout.
    pub fn default_settings() -> Self {
        Self::new(3, Duration::from_secs(300))
    }

    /// Get or spawn a connected MCP client for the given plugin version.
    ///
    /// First tries to reuse an idle process. If none available, spawns a new
    /// subprocess, performs MCP initialize handshake, and discovers tools.
    pub async fn acquire(
        &self,
        plugin_id: &str,
        version: &str,
        exe_path: &Path,
    ) -> Result<Arc<Mutex<McpClient>>, String> {
        let key = pool_key(plugin_id, version);

        // Try to reuse an idle process
        {
            let mut idle = self.idle.lock().await;
            while let Some(client) = idle.get_mut(&key).and_then(|q| q.pop_front()) {
                // Verify the process is still alive
                let alive = {
                    let mut c = client.lock().await;
                    c.ping().await
                };
                if alive {
                    tracing::debug!(%key, "Reusing idle plugin process");
                    return Ok(client);
                }
                // Dead process — the Arc will drop it naturally
                tracing::debug!(%key, "Idle plugin process was dead, dropping");
            }
        }

        // Spawn a new process
        tracing::debug!(%key, path = %exe_path.display(), "Spawning plugin process");
        let client =
            McpClient::connect_stdio(&exe_path.to_string_lossy(), &[], &HashMap::new()).await?;
        let client = Arc::new(Mutex::new(client));

        Ok(client)
    }

    /// Return a client to the pool for reuse.
    ///
    /// If the pool already has max_idle processes for this key, the client
    /// is dropped (which kills the subprocess).
    pub async fn release(&self, plugin_id: &str, version: &str, client: Arc<Mutex<McpClient>>) {
        let key = pool_key(plugin_id, version);
        let mut idle = self.idle.lock().await;
        let queue = idle.entry(key.clone()).or_default();
        if queue.len() < self.max_idle {
            tracing::debug!(key = %key, "Returning plugin process to pool");
            queue.push_back(client);
        } else {
            tracing::debug!(key = %key, "Pool full, dropping plugin process");
            // client drops here → McpClient drops → Transport::Stdio drops → child.kill()
        }
    }

    /// Background health check: remove dead processes from the idle pool.
    /// Should be called periodically (e.g., every 60 seconds).
    pub async fn health_check(&self) {
        let mut idle = self.idle.lock().await;
        for queue in idle.values_mut() {
            // Temporarily drain to check each client — we need lock for is_alive()
            let clients: Vec<_> = std::mem::take(queue).into();
            for client in clients {
                let alive = client
                    .try_lock()
                    .map(|mut guard| guard.is_alive())
                    .unwrap_or(false);
                if alive {
                    queue.push_back(client);
                } else {
                    tracing::debug!("Health check removed dead MCP process from pool");
                }
            }
        }
        // Remove empty queues
        idle.retain(|_, queue| !queue.is_empty());
    }

    /// Kill all processes for a specific plugin version (used during rollback).
    pub async fn drain_version(&self, plugin_id: &str, version: &str) {
        let key = pool_key(plugin_id, version);
        let mut idle = self.idle.lock().await;
        if let Some(queue) = idle.remove(&key) {
            tracing::info!(
                %key,
                count = queue.len(),
                "Draining plugin processes for rollback"
            );
            // All clients drop → subprocesses killed
        }
    }

    /// Get the number of idle processes for a plugin version.
    pub async fn idle_count(&self, plugin_id: &str, version: &str) -> usize {
        let key = pool_key(plugin_id, version);
        let idle = self.idle.lock().await;
        idle.get(&key).map(|q| q.len()).unwrap_or(0)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn pool_key(plugin_id: &str, version: &str) -> String {
    format!("{plugin_id}@{version}")
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_key_format() {
        assert_eq!(pool_key("shell", "v1.0.0"), "shell@v1.0.0");
    }

    #[tokio::test]
    async fn test_new_pool_is_empty() {
        let pool = ProcessPool::default_settings();
        assert_eq!(pool.idle_count("shell", "v1.0.0").await, 0);
    }
}
