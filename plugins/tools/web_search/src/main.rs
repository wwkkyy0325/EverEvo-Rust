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

use std::io::{BufRead, BufReader, Write};

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

/// Execute the search (stub — uses DuckDuckGo Lite HTML scraping).
///
/// In production, this would use the existing search engine logic from
/// `everevo-agent/src/tools/builtins/web_search/engine.rs`.
fn execute_search(query: &str, _max_results: usize) -> Result<String, String> {
    // Build a DuckDuckGo Lite search URL
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        urlencoding(query)
    );

    // Fetch the search results page
    let response = ureq::get(&url)
        .header("User-Agent", "EverEvo-Plugin-WebSearch/1.0")
        .call()
        .map_err(|e| format!("Search request failed: {e}"))?;

    let mut body_reader = response.into_body();
    let body = body_reader
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    // Extract result snippets from the HTML
    let results = parse_ddg_lite(&body);

    if results.is_empty() {
        Ok(format!(
            "No search results found for '{query}'. Try a different query."
        ))
    } else {
        let formatted: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, (title, snippet, link))| {
                format!("{}. {}\n   {}\n   {}", i + 1, title, snippet, link)
            })
            .collect();
        Ok(format!(
            "Web search results for '{}':\n\n{}",
            query,
            formatted.join("\n\n")
        ))
    }
}

/// Simple URL encoding (avoid adding a dependency for this).
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => format!("%{:02X}", other as u8),
        })
        .collect()
}

/// Parse DuckDuckGo Lite HTML result page.
fn parse_ddg_lite(html: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut current_title = String::new();
    let mut current_snippet = String::new();
    let mut current_link = String::new();
    let mut in_result = false;
    let mut in_link = false;

    for line in html.lines() {
        let trimmed = line.trim();

        // DDG Lite: result links have class="result-link"
        if trimmed.contains("result-link") || trimmed.contains("class=\"result-snippet\"") {
            // Extract link from <a href="...">
            if let Some(href_start) = trimmed.find("href=\"") {
                let after = &trimmed[href_start + 6..];
                if let Some(href_end) = after.find('"') {
                    current_link = after[..href_end].to_string();
                    // Resolve relative URLs
                    if current_link.starts_with("//") {
                        current_link = format!("https:{current_link}");
                    }
                }
                // Title is the text content
                if let Some(close) = trimmed.find('>') {
                    let after_tag = &trimmed[close + 1..];
                    if let Some(end) = after_tag.find("</a>") {
                        current_title = strip_html(&after_tag[..end]);
                    }
                }
            }
            in_result = true;
            in_link = true;
        } else if trimmed.contains("result-snippet") && in_result {
            // Extract snippet text
            if let Some(close) = trimmed.find('>') {
                let after_tag = &trimmed[close + 1..];
                if let Some(end) = after_tag.find("</") {
                    current_snippet = strip_html(&after_tag[..end]);
                }
            }
            // End of this result
            if !current_title.is_empty() {
                results.push((
                    std::mem::take(&mut current_title),
                    std::mem::take(&mut current_snippet),
                    std::mem::take(&mut current_link),
                ));
            }
            in_result = false;
            in_link = false;
        } else if in_link && (trimmed.starts_with("class=\"") || trimmed.is_empty()) {
            // Continuation of the result block
            continue;
        } else {
            in_link = false;
        }
    }

    results
}

/// Strip HTML tags from text.
fn strip_html(s: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .trim()
        .to_string()
}

// ── MCP Server ─────────────────────────────────────────────────────────

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
                    "tools": [{
                        "name": "web_search",
                        "description": "Search the web using DuckDuckGo Lite. Returns titles, snippets, and links.",
                        "inputSchema": serde_json::from_str::<serde_json::Value>(SEARCH_TOOL_SCHEMA).unwrap()
                    }]
                }
            }),

            "tools/call" => {
                let params = &req["params"];
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];

                let result = match tool_name {
                    "web_search" => match parse_search_args(arguments) {
                        Ok((query, max_results)) => execute_search(&query, max_results),
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
            },

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
