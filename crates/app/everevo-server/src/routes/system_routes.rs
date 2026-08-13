//! System runtime/capability routes — grouped micro-route modules (2026-08-13 restructure).
use crate::app_state::AppState;
use axum::extract::Path;
use axum::extract::State;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use everevo_core::ApiError;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;

fn health_router() -> Router<Arc<AppState>> {
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

type HandlerResult<T> = std::result::Result<T, ApiError>;

fn err<E: ToString>(e: E) -> ApiError {
    ApiError::internal(e.to_string())
}

#[derive(Serialize)]
struct ModelInfo {
    name: String,
    display_name: String,
    dim: usize,
    active: bool,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
    active: String,
    active_dim: usize,
}

/// GET /api/models
async fn list_models(State(state): State<Arc<AppState>>) -> Json<ModelsResponse> {
    let reg = state
        .model_registry
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let active = reg.active();
    Json(ModelsResponse {
        models: reg
            .list()
            .iter()
            .map(|m| ModelInfo {
                name: m.name.clone(),
                display_name: m.display_name.clone(),
                dim: m.dim,
                active: m.active,
            })
            .collect(),
        active: active.map(|a| a.name.clone()).unwrap_or_default(),
        active_dim: active.map(|a| a.dim).unwrap_or(0),
    })
}

#[derive(Deserialize)]
struct ActivateRequest {
    model: String,
}

/// POST /api/models/activate
async fn activate_model(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ActivateRequest>,
) -> HandlerResult<Json<ModelInfo>> {
    let mut reg = state.model_registry.write().map_err(err)?;
    let meta = reg.activate(&req.model).map_err(err)?;
    Ok(Json(ModelInfo {
        name: meta.name,
        display_name: meta.display_name,
        dim: meta.dim,
        active: true,
    }))
}

#[derive(Serialize)]
struct ReindexResponse {
    collection: String,
    processed: usize,
    duration_ms: u64,
}

/// POST /api/vector/reindex
async fn reindex_collection(
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> HandlerResult<Json<ReindexResponse>> {
    let collection = req
        .get("collection")
        .and_then(|v| v.as_str())
        .unwrap_or("memory");
    let rag = state
        .rag_pipeline
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("RAG pipeline not initialized"))?;

    let start = std::time::Instant::now();
    let count = state.fact_manager.index_into_rag(rag).map_err(err)?;
    Ok(Json(ReindexResponse {
        collection: collection.to_string(),
        processed: count,
        duration_ms: start.elapsed().as_millis() as u64,
    }))
}

fn model_routes_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/models", axum::routing::get(list_models))
        .route("/api/models/activate", axum::routing::post(activate_model))
        .route(
            "/api/vector/reindex",
            axum::routing::post(reindex_collection),
        )
}

fn mcp_routes_router() -> Router<Arc<AppState>> {
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
) -> Result<Json<serde_json::Value>, ApiError> {
    // Find the server config
    let srv = state
        .config
        .mcp_servers
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("MCP server '{name}' not found in config")))?;

    if !srv.enabled {
        return Err(ApiError::bad_request(format!(
            "MCP server '{name}' is disabled in config"
        )));
    }

    // Drop old connection (kills old process for stdio)
    state.mcp_clients.write().await.remove(&name);

    // Re-discover tools with appropriate transport
    let result = match srv.transport.as_str() {
        "http" | "sse" => everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await,
        _ => {
            let args: Vec<&str> = srv.args.iter().map(String::as_str).collect();
            everevo_mcp::discover_mcp_tools(&srv.command, &args, &srv.env).await
        }
    };

    match result {
        Ok((client, tools)) => {
            tracing::info!(%name, tool_count = tools.len(), "MCP server reconnected");
            state.mcp_clients.write().await.insert(name.clone(), client);
            Ok(Json(serde_json::json!({
                "success": true,
                "name": name,
                "tool_count": tools.len(),
                "message": format!("Reconnected — {} tools available", tools.len()),
            })))
        }
        Err(e) => {
            tracing::warn!(%name, error = %e, "MCP server reconnect failed");
            Err(ApiError::internal(format!(
                "MCP server '{name}' reconnect failed: {e}"
            )))
        }
    }
}

fn tools_routes_router() -> Router<Arc<AppState>> {
    Router::new().route("/api/tools", get(list_tools))
}

async fn list_tools(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut tools: Vec<serde_json::Value> = vec![
        tool(
            "shell",
            "Execute a shell command in a sandboxed environment",
        ),
        tool("download", "Download files from URLs with mirror failover"),
        tool(
            "bootstrap_check",
            "Check status of portable runtimes and embedding models",
        ),
        tool(
            "memory",
            "Search and manage persistent long-term memory (search/save/delete)",
        ),
        tool(
            "TodoWrite",
            "Create and manage a structured task list for the current session",
        ),
        tool(
            "EnterPlanMode",
            "Signal intent to plan before implementing non-trivial tasks",
        ),
        tool(
            "ExitPlanMode",
            "Submit a plan for user approval before implementation",
        ),
        tool(
            "Workflow",
            "Execute multiple tasks in parallel using sub-agents",
        ),
        tool(
            "Skill",
            "Invoke specialized skills by name (use action=list to discover)",
        ),
        tool(
            "Verify",
            "Verify the output of a previous task for correctness",
        ),
        tool("Task", "Spawn a sub-agent to execute a task independently"),
        tool(
            "web_fetch",
            "Fetch content from a URL (HTML stripped, 16K limit)",
        ),
        tool(
            "web_search",
            "Search the web and return result blocks with titles and URLs",
        ),
        tool(
            "compact",
            "Manually trigger context compaction to free up space",
        ),
        tool(
            "team",
            "Dispatch a team of role-specialized sub-agents (reviewer/researcher/coder/tester)",
        ),
        tool(
            "code_search",
            "Search the codebase for symbols using FTS5 index (query, kind, limit)",
        ),
        tool(
            "code_map",
            "Return a Markdown directory overview of the codebase",
        ),
        tool(
            "list_dir",
            "List files and directories in the workspace with sizes and timestamps",
        ),
        tool(
            "read_file",
            "Read a file from the workspace with line numbers",
        ),
        tool("write_file", "Create or overwrite a file in the workspace"),
        tool(
            "cluster",
            "Orchestrate parallel sub-agents: fan_out, map_reduce, verify",
        ),
        tool(
            "workflow_run",
            "Execute a multi-step automation workflow from JSON definition",
        ),
    ];

    // Add MCP tools
    for (name, client) in state.mcp_clients.read().await.iter() {
        if let Ok(guard) = client.try_lock() {
            for t in &guard.tools {
                tools.push(serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "source": format!("mcp:{}", name),
                }));
            }
        }
    }

    Json(serde_json::json!({
        "tools": tools,
        "count": tools.len(),
        "builtin_count": tools.len().saturating_sub(
            state.mcp_clients.read().await.values().filter_map(|c| c.try_lock().ok().map(|g| g.tools.len())).sum::<usize>()
        ),
        "mcp_server_count": state.mcp_clients.read().await.len(),
    }))
}

fn tool(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "source": "builtin",
    })
}

pub fn router() -> axum::Router<Arc<crate::app_state::AppState>> {
    axum::Router::new()
        .merge(health_router())
        .merge(model_routes_router())
        .merge(mcp_routes_router())
        .merge(tools_routes_router())
}
