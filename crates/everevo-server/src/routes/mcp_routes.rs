//! MCP server management endpoints — list, status, and reconnect.

use crate::app_state::AppState;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/mcp/servers", get(list_servers))
        .route("/api/mcp/servers/{name}/reconnect", post(reconnect_server))
}

async fn list_servers(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let servers: Vec<serde_json::Value> = state
        .mcp_clients
        .read()
        .await
        .iter()
        .map(|(name, client)| {
            let entry = match client.try_lock() {
                Ok(guard) => serde_json::json!({
                    "name": name,
                    "status": "connected",
                    "server": guard.server_info.name,
                    "server_version": guard.server_info.version,
                    "tool_count": guard.tools.len(),
                    "tools": guard.tools.iter().map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                    })).collect::<Vec<_>>(),
                }),
                Err(_) => serde_json::json!({
                    "name": name,
                    "status": "busy",
                    "note": "Tool executing — lock held"
                }),
            };
            entry
        })
        .collect();

    Json(serde_json::json!({
        "servers": servers,
        "count": servers.len(),
    }))
}

/// Reconnect to an MCP server by name. Drops the old connection and re-discovers tools.
async fn reconnect_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Json<serde_json::Value> {
    // Find the server config
    let srv = match state.config.mcp_servers.iter().find(|s| s.name == name) {
        Some(s) => s.clone(),
        None => {
            return Json(serde_json::json!({
                "error": format!("MCP server '{name}' not found in config"),
                "success": false,
            }));
        }
    };

    if !srv.enabled {
        return Json(serde_json::json!({
            "error": format!("MCP server '{name}' is disabled in config"),
            "success": false,
        }));
    }

    // Drop old connection (kills old process for stdio)
    state.mcp_clients.write().await.remove(&name);

    // Re-discover tools with appropriate transport
    let result = match srv.transport.as_str() {
        "http" | "sse" => {
            everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await
        }
        _ => {
            let args: Vec<&str> = srv.args.iter().map(String::as_str).collect();
            everevo_mcp::discover_mcp_tools(&srv.command, &args, &srv.env).await
        }
    };

    match result {
        Ok((client, tools)) => {
            tracing::info!(%name, tool_count = tools.len(), "MCP server reconnected");
            state.mcp_clients.write().await.insert(name.clone(), client);
            Json(serde_json::json!({
                "success": true,
                "name": name,
                "tool_count": tools.len(),
                "message": format!("Reconnected — {} tools available", tools.len()),
            }))
        }
        Err(e) => {
            tracing::warn!(%name, error = %e, "MCP server reconnect failed");
            Json(serde_json::json!({
                "success": false,
                "name": name,
                "error": e,
            }))
        }
    }
}
