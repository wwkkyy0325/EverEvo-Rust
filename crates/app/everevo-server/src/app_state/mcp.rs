use std::{collections::HashMap, sync::Arc};

use super::AppState;

impl AppState {
    /// Spawn a background task that periodically checks MCP server health
    /// and attempts reconnection for dead servers.
    ///
    /// Claude Code alignment: MCP servers that crash are automatically
    /// reconnected within 60 seconds without user intervention.
    pub(crate) fn spawn_mcp_health_checker(state: &Arc<Self>) {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            // Skip first tick (it fires immediately)
            interval.tick().await;
            loop {
                interval.tick().await;
                let dead: Vec<String> = {
                    let clients = state.mcp_clients.read().await;
                    clients
                        .iter()
                        .filter_map(|(name, client)| {
                            match client.try_lock() {
                                Ok(mut guard) => {
                                    if !guard.is_alive() {
                                        Some(name.clone())
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None, // busy — skip health check
                            }
                        })
                        .collect()
                };

                for name in &dead {
                    // Find config for this server
                    let srv = state
                        .config
                        .mcp_servers
                        .iter()
                        .find(|s| &s.name == name)
                        .cloned();

                    if let Some(srv) = srv {
                        if !srv.enabled {
                            continue;
                        }
                        // Drop dead client
                        state.mcp_clients.write().await.remove(name);
                        tracing::warn!(%name, "MCP server dead — attempting reconnect");

                        let result = match srv.transport.as_str() {
                            "http" | "sse" => {
                                everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await
                            }
                            _ => {
                                let args: Vec<&str> = srv.args.iter().map(String::as_str).collect();
                                let env = state.inject_runtime_path(&srv.env);
                                everevo_mcp::discover_mcp_tools(&srv.command, &args, &env).await
                            }
                        };

                        match result {
                            Ok((client, tools)) => {
                                tracing::info!(
                                    %name,
                                    tool_count = tools.len(),
                                    "MCP server auto-reconnected"
                                );
                                state.mcp_clients.write().await.insert(name.clone(), client);
                            }
                            Err(e) => {
                                tracing::error!(
                                    %name,
                                    error = %e,
                                    "MCP auto-reconnect failed — will retry in 60s"
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    pub(crate) async fn connect_mcp_servers(state: &Arc<Self>) {
        for srv in &state.config.mcp_servers {
            if !srv.enabled {
                continue;
            }
            let result = match srv.transport.as_str() {
                "http" | "sse" => {
                    everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await
                }
                _ => {
                    // stdio (default)
                    let args: Vec<&str> = srv.args.iter().map(String::as_str).collect();
                    // Ensure bootstrapped runtimes (node/npx) are on the child's PATH
                    // so `npx @playwright/mcp` resolves even on a clean machine.
                    let env = state.inject_runtime_path(&srv.env);
                    everevo_mcp::discover_mcp_tools(&srv.command, &args, &env).await
                }
            };

            match result {
                Ok((client, tools)) => {
                    tracing::info!(
                        name = %srv.name,
                        transport = %srv.transport,
                        tool_count = tools.len(),
                        "MCP server connected"
                    );
                    state
                        .mcp_clients
                        .write()
                        .await
                        .insert(srv.name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!(name = %srv.name, transport = %srv.transport, error = %e, "MCP server connection failed");
                }
            }
        }
    }

    /// Start the built-in `everevo-webagent` as an MCP stdio child process.
    ///
    /// The webagent provides `web_search`, `web_fetch`, and `web_browse` tools
    /// with anti-detection browser automation. It runs as a separate process
    /// so search/browser failures never crash the main server.
    ///
    /// Binary discovery order:
    /// 1. `everevo-webagent` / `everevo-webagent.exe` next to the server binary
    /// 2. Same name in PATH
    /// 3. `target/debug/everevo-webagent` (dev mode)
    pub(crate) async fn start_webagent(state: &Arc<Self>) {
        let binary = Self::find_webagent_binary();
        tracing::info!(?binary, "Starting built-in webagent");

        let args: &[&str] = &[];
        let env = state.inject_runtime_path(&HashMap::new());
        match everevo_mcp::discover_mcp_tools(&binary, args, &env).await {
            Ok((client, tools)) => {
                tracing::info!(tool_count = tools.len(), "Built-in webagent connected");
                state
                    .mcp_clients
                    .write()
                    .await
                    .insert("everevo-webagent".into(), client);
            }
            Err(e) => {
                tracing::warn!(
                    binary = %binary,
                    error = %e,
                    "Built-in webagent unavailable — web search will use fallback tools. \
                     Build with: cargo build -p everevo-webagent"
                );
            }
        }
    }

    /// Find the webagent binary. Checks common locations.
    fn find_webagent_binary() -> String {
        let exe_name = if cfg!(windows) {
            "everevo-webagent.exe"
        } else {
            "everevo-webagent"
        };

        // 1. Next to the server binary (production layout)
        if let Ok(server_exe) = std::env::current_exe() {
            if let Some(dir) = server_exe.parent() {
                let candidate = dir.join(exe_name);
                if candidate.exists() {
                    return candidate.display().to_string();
                }
            }
        }

        // 2. Dev mode: target/debug/everevo-webagent
        let dev_path = std::path::Path::new("target/debug").join(exe_name);
        if dev_path.exists() {
            return dev_path.display().to_string();
        }

        // 3. Dev mode: target/release/everevo-webagent
        let rel_path = std::path::Path::new("target/release").join(exe_name);
        if rel_path.exists() {
            return rel_path.display().to_string();
        }

        // 5. Fallback: just the name, let the OS try PATH
        exe_name.to_string()
    }

    /// Build the env map for a stdio MCP child process, prepending bootstrapped
    /// runtime dirs (node/npx) to PATH so `npx @playwright/mcp` resolves on a
    /// clean machine that has no system Node installed.
    fn inject_runtime_path(&self, base: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env = base.clone();
        let sep = if cfg!(windows) { ";" } else { ":" };
        // Windows env lookup is case-insensitive; reuse an existing-key spelling
        // (e.g. "Path") if the caller already set one, else use canonical "PATH".
        let key = env
            .keys()
            .find(|k| k.eq_ignore_ascii_case("PATH"))
            .cloned()
            .unwrap_or_else(|| "PATH".to_string());
        let host_path = std::env::var("PATH").unwrap_or_default();
        let runtime_paths: Vec<String> = self
            .runtime_env
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let combined = if runtime_paths.is_empty() {
            host_path
        } else {
            format!("{}{}{}", runtime_paths.join(sep), sep, host_path)
        };
        env.insert(key, combined);
        env
    }

    /// Gracefully shut down all MCP client connections on server shutdown.
    /// Sends MCP `shutdown` request for stdio clients, then drops connections.
    pub async fn destroy_all_mcp_clients(&self) {
        let mut mcp = self.mcp_clients.write().await;
        let count = mcp.len();
        for (name, client) in mcp.drain() {
            if let Ok(mut guard) = client.try_lock() {
                if let Err(e) = guard.shutdown().await {
                    tracing::warn!(server = %name, error = %e, "MCP shutdown failed");
                }
            }
            tracing::debug!(server = %name, "MCP client disconnected");
        }
        tracing::info!(count, "All MCP clients shut down");
    }
}
