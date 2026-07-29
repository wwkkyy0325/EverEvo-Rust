//! Tools discovery endpoint — lists all available tools (built-in + MCP).

use crate::app_state::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/tools", get(list_tools))
}

async fn list_tools(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut tools: Vec<serde_json::Value> = vec![
        tool("shell", "Execute a shell command in a sandboxed environment"),
        tool("download", "Download files from URLs with mirror failover"),
        tool("bootstrap_check", "Check status of portable runtimes and embedding models"),
        tool("memory", "Search and manage persistent long-term memory (search/save/delete)"),
        tool("TodoWrite", "Create and manage a structured task list for the current session"),
        tool("EnterPlanMode", "Signal intent to plan before implementing non-trivial tasks"),
        tool("ExitPlanMode", "Submit a plan for user approval before implementation"),
        tool("Workflow", "Execute multiple tasks in parallel using sub-agents"),
        tool("Skill", "Invoke specialized skills by name (use action=list to discover)"),
        tool("Verify", "Verify the output of a previous task for correctness"),
        tool("Task", "Spawn a sub-agent to execute a task independently"),
        tool("web_fetch", "Fetch content from a URL (HTML stripped, 16K limit)"),
        tool("web_search", "Search the web and return result blocks with titles and URLs"),
        tool("compact", "Manually trigger context compaction to free up space"),
        tool("team", "Dispatch a team of role-specialized sub-agents (reviewer/researcher/coder/tester)"),
        tool("code_search", "Search the codebase for symbols using FTS5 index (query, kind, limit)"),
        tool("code_map", "Return a Markdown directory overview of the codebase"),
        tool("list_dir", "List files and directories in the workspace with sizes and timestamps"),
        tool("read_file", "Read a file from the workspace with line numbers"),
        tool("write_file", "Create or overwrite a file in the workspace"),
        tool("cluster", "Orchestrate parallel sub-agents: fan_out, map_reduce, verify"),
        tool("workflow_run", "Execute a multi-step automation workflow from JSON definition"),
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
        "builtin_count": 22,
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
