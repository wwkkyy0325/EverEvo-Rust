//! plugin-web-search — MCP server providing web search capability.
//!
//! Communicates via JSON-RPC 2.0 over stdin/stdout (MCP stdio transport).
//! This is a standalone binary — the kernel spawns it as a subprocess.
//!
//! ## Protocol
//!
//! Each stdin line is a JSON-RPC request. Each stdout line is a JSON-RPC response.
//! stderr is used for diagnostics only (never protocol data).
//!
//! ## Supported methods
//!
//! - `initialize`  → MCP handshake (required)
//! - `tools/list`  → discover available tools
//! - `tools/call`  → execute a tool
//! - `ping`        → liveness check

mod http;
mod probe;
mod quality;
mod research;
mod web;

use std::io::{BufRead, BufReader, Write};

// Crate-internal re-exports so cross-module call sites keep working.
pub(crate) use http::Hit;
pub(crate) use research::research_search_tool;
pub(crate) use web::execute_search;

// ── Tool: web_search ───────────────────────────────────────────────────

const SEARCH_TOOL_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "Search query"
        },
        "max_results": {
            "type": "integer",
            "description": "Maximum number of results (default: 5)",
            "default": 5
        }
    },
    "required": ["query"]
}"#;

/// Parse query from tool call arguments.
fn parse_search_args(args: &serde_json::Value) -> Result<(String, usize), String> {
    let query = args["query"]
        .as_str()
        .ok_or("Missing 'query' parameter")?
        .to_string();
    let max_results = args["max_results"].as_u64().unwrap_or(5) as usize;
    // Clamp to reasonable bounds
    let max_results = max_results.clamp(1, 20);
    Ok((query, max_results))
}

const RESEARCH_TOOL_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "Search query"
        },
        "max_results": {
            "type": "integer",
            "description": "Maximum number of merged results (default: 8)",
            "default": 8
        },
        "kind": {
            "type": "string",
            "enum": ["auto", "papers", "news"],
            "description": "Source priority hint (default: auto — routed by query keywords)"
        }
    },
    "required": ["query"]
}"#;

/// Parse research_search arguments: (query, max_results, kind).
fn parse_research_args(args: &serde_json::Value) -> Result<(String, usize, String), String> {
    let query = args["query"]
        .as_str()
        .ok_or("Missing 'query' parameter")?
        .to_string();
    let max_results = args["max_results"].as_u64().unwrap_or(8) as usize;
    let max_results = max_results.clamp(1, 15);
    let kind = args["kind"].as_str().unwrap_or("auto").to_string();
    Ok((query, max_results, kind))
}

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();

    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed → exit
        };

        if line.trim().is_empty() {
            continue;
        }

        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {e}")},
                    "id": null
                });
                let _ = writeln!(stdout, "{}", serde_json::to_string(&err).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        let method = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();

        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "serverInfo": {
                        "name": "web_search",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "tools": {}
                    }
                }
            }),

            "notifications/initialized" => {
                // No response needed for notifications
                continue;
            }

            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "web_search_local",
                            "description": "Local web search fallback (Sogou → Bing → DuckDuckGo/Google). Use ONLY when web_search (the server-side search executed by the API) failed, returned nothing useful, or you need raw engine results to verify. IMPORTANT: use SHORT KEYWORD queries, not full sentences — e.g. 'Mercedes Sosa studio albums list' instead of 'How many studio albums did Mercedes Sosa publish?'. Returns numbered results with title, URL, and snippet.",
                            "inputSchema": serde_json::from_str::<serde_json::Value>(SEARCH_TOOL_SCHEMA).unwrap()
                        },
                        {
                            "name": "research_search",
                            "description": "Merged academic + news search (arXiv, OpenAlex, Crossref, Semantic Scholar, PubMed, news feeds). Use for scientific papers, biomedical/clinical facts, statistics with citable sources, or recent news events. NOT for general web lookups — use web_search for that. Auto-routes source priority by query; pass 'kind' ('papers' | 'news') to bias routing. Returns deduplicated results tagged with their source.",
                            "inputSchema": serde_json::from_str::<serde_json::Value>(RESEARCH_TOOL_SCHEMA).unwrap()
                        }
                    ]
                }
            }),

            "tools/call" => {
                let params = &req["params"];
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];

                let result = match tool_name {
                    "web_search_local" => match parse_search_args(arguments) {
                        Ok((query, max_results)) => execute_search(&query, max_results),
                        Err(e) => Err(e),
                    },
                    "research_search" => match parse_research_args(arguments) {
                        Ok((query, max_results, kind)) => {
                            research_search_tool(&query, max_results, &kind)
                        }
                        Err(e) => Err(e),
                    },
                    other => Err(format!("Unknown tool: {other}")),
                };

                match result {
                    Ok(content) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": content}]
                        }
                    }),
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": e}],
                            "isError": true
                        }
                    }),
                }
            }

            "ping" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),

            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        };

        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}
