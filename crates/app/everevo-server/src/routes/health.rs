//! Health check + agent status endpoint.

use crate::app_state::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/health", get(handler))
}

async fn handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let session_count = state.sandboxes.read().await.len();
    let llm_guard = state.llm.read().await;
    let llm_count = llm_guard.len();
    let primary_configured = llm_guard.contains_key("primary");
    let provider_ids: Vec<String> = llm_guard.keys().cloned().collect();
    drop(llm_guard);

    let mcp: Vec<serde_json::Value> = state
        .mcp_clients
        .read()
        .await
        .iter()
        .map(|(name, client)| match client.try_lock() {
            Ok(mut guard) => {
                let alive = guard.is_alive();
                serde_json::json!({
                    "name": name,
                    "tools": guard.tools.len(),
                    "tool_names": guard.tool_names(),
                    "server": guard.server_info.name,
                    "status": if alive { "connected" } else { "dead" },
                })
            }
            Err(_) => serde_json::json!({
                "name": name,
                "status": "busy",
                "note": "Tool executing — lock held"
            }),
        })
        .collect();

    let startup_check = state.startup_report.read().await;
    let startup_json = startup_check.as_ref().map(|r| {
        serde_json::json!({
            "pass": r.pass,
            "warn": r.warn,
            "fail": r.fail,
            "total_ms": r.total_ms,
            "actual_port": r.actual_port,
            "checks": r.items.iter().map(|i| {
                serde_json::json!({
                    "name": i.name,
                    "status": i.status,
                    "detail": i.detail,
                    "latency_ms": i.latency_ms,
                })
            }).collect::<Vec<_>>(),
        })
    });

    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "startup_check": startup_json,
        "llm": {
            "configured": llm_count,
            "primary_available": primary_configured,
            "any_available": llm_count > 0,
            "provider_ids": provider_ids,
        },
        "sessions": {
            "active": session_count,
        },
        "mcp_servers": mcp,
        "features": {
            "autocompact": true,
            "tool_hooks": true,
            "mcp": true,
            "mcp_reconnect": true,
            "web_fetch": true,
            "web_search": true,
            "compact": true,
            "context_snip": true,
            "context_overflow_recovery": true,
            "cancel_support": true,
            "docker_safety": true,
            "subagent_types": ["reviewer", "research", "code-explorer", "file"],
            "team_roles": ["reviewer", "researcher", "coder", "tester", "general"],
            "workflow_modes": ["parallel", "sequential"],
            "team_coordination": true,
            "workflow_engine": true,
            "code_search": true,
            "workspace": true,
            "list_dir": true,
            "read_file": true,
            "write_file": true,
            "cluster": true,
        },
    }))
}
