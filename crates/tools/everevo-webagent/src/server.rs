//! MCP stdio server — reads JSON-RPC from stdin, writes to stdout.
//!
//! Registers three MCP tools:
//! - `web_search` — multi-engine HTTP search (Bing/DDG, fast, no browser)
//! - `web_fetch` — fetch a single URL (HTTP or CDP-browser for JS pages)
//! - `web_browse` — stealth CDP browser navigation + DOM extraction + CAPTCHA

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::captcha::solve::SimpleCaptchaSolver;
use crate::protect::{circuit::CircuitBreaker, rate_limit::RateLimiter};

// ── JSON-RPC types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize)]
struct ToolDef {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

// ── Tool registry ───────────────────────────────────────────────────────

type ToolFn = fn(&AppState, Value) -> Result<String, String>;

struct ToolEntry {
    def: ToolDef,
    handler: ToolFn,
}

struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

impl ToolRegistry {
    fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    fn register(&mut self, def: ToolDef, handler: ToolFn) {
        self.tools
            .insert(def.name.clone(), ToolEntry { def, handler });
    }

    fn list(&self) -> Vec<&ToolDef> {
        self.tools.values().map(|e| &e.def).collect()
    }

    fn call(&self, name: &str, args: Value, state: &AppState) -> Result<String, String> {
        let entry = self
            .tools
            .get(name)
            .ok_or_else(|| format!("Tool not found: {name}"))?;
        (entry.handler)(state, args)
    }
}

// ── Shared state ────────────────────────────────────────────────────────

pub struct AppState {
    rate_limiter: RateLimiter,
    circuit_breaker: CircuitBreaker,
}

// ── Server ──────────────────────────────────────────────────────────────

pub struct Server {
    registry: ToolRegistry,
    state: Arc<AppState>,
}

impl Server {
    pub fn new() -> Self {
        let state = Arc::new(AppState {
            rate_limiter: RateLimiter::new(5.0, 0.5), // 5 tokens, 0.5/sec refill
            circuit_breaker: CircuitBreaker::new(5, 120), // trip after 5 failures, 2min cooldown
        });

        let mut registry = ToolRegistry::new();

        // ── web_search ──────────────────────────────────────────────
        registry.register(
            ToolDef {
                name: "web_search".into(),
                description:
                    "Search the web using multiple search engines (Bing, DuckDuckGo) \
                     with automatic fallback. Fast — uses direct HTTP, no browser. \
                     Returns title, URL, and snippet for each result. \
                     Supports Bing (cn.bing.com, mainland-China friendly) and \
                     DuckDuckGo as backup. Parameters: query (required), limit (default 8, max 20)."
                        .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query keywords"},
                        "limit": {"type": "integer", "description": "Max results (default 8, max 20)", "default": 8}
                    },
                    "required": ["query"]
                }),
            },
            |_state, args| {
                let query = args["query"].as_str().ok_or("query required".to_string())?;
                let limit = args["limit"].as_u64().unwrap_or(8).min(20) as usize;
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
                rt.block_on(async {
                    // Tier 1: fast HTTP search (Bing → DDG)
                    match crate::search::engines::search(query, limit).await {
                        Ok(r) => Ok(r),
                        Err(http_err) => {
                            // Tier 2: CDP browser bridge fallback
                            let search_url = std::env::var("EVEREVO_SEARCH_BROWSER_URL")
                                .unwrap_or_else(|_| "https://cn.bing.com/search?q=".to_string());
                            let url = format!("{search_url}{}", crate::search::engines::encode_url_query(query));
                            match crate::browser::bridge::BrowserBridge::launch().await {
                                Ok(bridge) => {
                                    let solver = SimpleCaptchaSolver;
                                    match bridge.search(&url, limit, Some(&solver)).await {
                                        Ok(results) if !results.is_empty() => {
                                            let lines: Vec<String> = results.iter().enumerate()
                                                .map(|(i,r)| format!("{}. **{}**\n   {}\n   {}", i+1, r.title, r.url, r.snippet))
                                                .collect();
                                            Ok(format!("Browser search for '{query}':\n\n{}", lines.join("\n\n")))
                                        }
                                        Ok(_) => Err(format!("HTTP search failed ({http_err}). Browser bridge returned 0 results.")),
                                        Err(bridge_err) => Err(format!("HTTP search failed ({http_err}). Browser bridge also failed: {bridge_err}")),
                                    }
                                }
                                Err(launch_err) => Err(format!("HTTP search failed ({http_err}). Browser bridge unavailable: {launch_err}")),
                            }
                        }
                    }
                })
            },
        );

        // ── web_fetch ───────────────────────────────────────────────
        registry.register(
            ToolDef {
                name: "web_fetch".into(),
                description:
                    "Fetch the content of a single web page. Supports both direct HTTP \
                     (fast, for static pages) and CDP browser rendering (for JavaScript \
                     pages). Returns cleaned text or markdown. Also extracts OpenGraph \
                     metadata and JSON-LD structured data when available.\
                     Parameters: url (required), render_js (optional, default false), \
                     format (optional, 'text' or 'markdown', default 'text')."
                        .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Full URL to fetch (must be http/https)"},
                        "render_js": {"type": "boolean", "description": "Use browser to execute JS (default false)", "default": false},
                        "format": {"type": "string", "description": "Output format: 'text' or 'markdown' (default 'text')", "enum": ["text", "markdown"]}
                    },
                    "required": ["url"]
                }),
            },
            |state, args| {
                let url = args["url"].as_str().ok_or("url required".to_string())?;
                let render_js = args["render_js"].as_bool().unwrap_or(false);
                let use_md = args["format"].as_str() == Some("markdown");

                // Validate URL
                let url = crate::protect::sanitize::sanitize_url(url)?;

                // Rate limit check
                let domain = url.split('/').nth(2).unwrap_or("unknown");
                if let Err(wait_ms) = state.rate_limiter.check(domain) {
                    return Err(format!("Rate limited. Wait {wait_ms}ms before retrying {domain}"));
                }

                // Circuit breaker
                if let Err(cd_ms) = state.circuit_breaker.check(domain) {
                    return Err(format!("Circuit breaker open for {domain}. Cooldown: {cd_ms}ms"));
                }

                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;

                rt.block_on(async {
                    let (body, content_type) = if render_js {
                        // Use CDP browser for JS-rendered pages
                        let bridge = crate::browser::bridge::BrowserBridge::launch().await
                            .map_err(|e| {
                                state.circuit_breaker.failure(domain);
                                format!("Browser launch failed: {e}")
                            })?;
                        let text = bridge.fetch_page_text(&url).await.inspect_err(|_| {
                            state.circuit_breaker.failure(domain);
                        })?;
                        state.circuit_breaker.success(domain);
                        (text, "text/html; browser-rendered".to_string())
                    } else {
                        // Direct HTTP fetch
                        let client = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(30))
                            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
                            .build().map_err(|e| format!("client: {e}"))?;
                        let resp = client.get(&url).send().await.map_err(|e| {
                            state.circuit_breaker.failure(domain);
                            format!("HTTP error: {e}")
                        })?;
                        let status = resp.status();
                        let ct = resp.headers().get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("unknown")
                            .to_string();
                        let body = resp.text().await.map_err(|e| format!("body: {e}"))?;
                        if !status.is_success() {
                            state.circuit_breaker.failure(domain);
                            return Err(format!("HTTP {status}"));
                        }
                        state.circuit_breaker.success(domain);
                        (body, ct)
                    };

                    let is_html = content_type.contains("html");

                    // Extract metadata when available
                    let metadata = if is_html {
                        let og = crate::extract::structured::extract_meta_tags(&body);
                        let ld = crate::extract::structured::extract_json_ld(&body);
                        (og, ld)
                    } else {
                        (Vec::new(), Vec::new())
                    };

                    // Extract content
                    let text = if is_html {
                        if use_md {
                            crate::extract::html::html_to_markdown(&body)
                        } else {
                            crate::protect::sanitize::html_to_text(&body)
                        }
                    } else {
                        // Non-HTML: return as-is
                        body.clone()
                    };

                    let truncated: String = text.chars().take(16000).collect();

                    // Build rich output with metadata
                    let mut output = format!(
                        "Fetched {url} ({} {})\n\n",
                        if render_js { "browser" } else { "HTTP" },
                        if is_html { "HTML" } else { &content_type }
                    );

                    if !metadata.0.is_empty() {
                        output.push_str("## OpenGraph\n");
                        for (prop, val) in &metadata.0 {
                            output.push_str(&format!("- {prop}: {val}\n"));
                        }
                        output.push('\n');
                    }
                    if !metadata.1.is_empty() {
                        output.push_str("## Structured Data (JSON-LD)\n");
                        for ld in &metadata.1 {
                            let preview: String = ld.chars().take(500).collect();
                            output.push_str(&format!("```json\n{preview}\n```\n\n"));
                        }
                    }

                    output.push_str(&format!("## Content ({} chars)\n\n{truncated}", text.len()));

                    Ok(output)
                })
            },
        );

        // ── web_browse ──────────────────────────────────────────────
        registry.register(
            ToolDef {
                name: "web_browse".into(),
                description:
                    "Search the web using a real browser with anti-detection measures. \
                     Launches a stealth Chrome/Edge browser via CDP, navigates to the \
                     search engine, executes JavaScript to render the page, extracts \
                     structured results from the DOM, and handles simple CAPTCHAs \
                     (Turnstile, reCAPTCHA checkbox). \
                     Slower than web_search but can bypass anti-bot blocks that block \
                     direct HTTP. Parameters: query (required), limit (default 8, max 20)."
                        .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query keywords"},
                        "limit": {"type": "integer", "description": "Max results (default 8, max 20)", "default": 8}
                    },
                    "required": ["query"]
                }),
            },
            |state, args| {
                let query = args["query"].as_str().ok_or("query required".to_string())?;
                let limit = args["limit"].as_u64().unwrap_or(8).min(20) as usize;

                // Check if search engines are rate-limited or circuit-broken
                for domain in &["cn.bing.com", "duckduckgo.com"] {
                    if let Err(wait_ms) = state.rate_limiter.check(domain) {
                        return Err(format!("Rate limited on {domain}. Wait {wait_ms}ms"));
                    }
                    if let Err(cd_ms) = state.circuit_breaker.check(domain) {
                        return Err(format!("Circuit open for {domain}. Cooldown: {cd_ms}ms"));
                    }
                }

                let search_url = std::env::var("EVEREVO_SEARCH_BROWSER_URL")
                    .unwrap_or_else(|_| "https://cn.bing.com/search?q=".to_string());
                let url = format!("{search_url}{}", crate::search::engines::encode_url_query(query));

                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;

                rt.block_on(async {
                    let bridge = crate::browser::bridge::BrowserBridge::launch().await
                        .map_err(|e| {
                            // Mark search engines as failed
                            state.circuit_breaker.failure("cn.bing.com");
                            state.circuit_breaker.failure("duckduckgo.com");
                            format!("Browser launch failed: {e}")
                        })?;

                    let solver = SimpleCaptchaSolver;
                    let results = bridge.search(&url, limit, Some(&solver)).await.inspect_err(|_| {
                        state.circuit_breaker.failure("cn.bing.com");
                    })?;

                    if results.is_empty() {
                        return Err("No results extracted (page may be blocked or empty)".to_string());
                    }

                    state.circuit_breaker.success("cn.bing.com");
                    let lines: Vec<String> = results.iter().enumerate()
                        .map(|(i, r)| format!("{}. **{}**\n   {}\n   {}", i+1, r.title, r.url, r.snippet))
                        .collect();
                    Ok(format!("Browser search results for '{query}':\n\n{}", lines.join("\n\n")))
                })
            },
        );

        Self { registry, state }
    }

    /// Run the MCP stdio server loop. Blocks until stdin closes.
    pub fn run(&self) {
        let stdin = std::io::stdin();
        let reader = BufReader::new(stdin.lock());
        let mut stdout = std::io::stdout();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }

            let req: Request = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response {
                        jsonrpc: "2.0",
                        id: Value::Null,
                        result: None,
                        error: Some(RpcError {
                            code: -32700,
                            message: format!("Parse error: {e}"),
                        }),
                    };
                    writeln!(
                        stdout,
                        "{}",
                        serde_json::to_string(&resp).unwrap_or_default()
                    )
                    .ok();
                    continue;
                }
            };

            let result = match req.method.as_str() {
                "initialize" => Ok(serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "serverInfo": { "name": "everevo-webagent", "version": "0.1.0" },
                    "capabilities": { "tools": {} }
                })),
                "tools/list" => {
                    let tools: Vec<&ToolDef> = self.registry.list();
                    Ok(serde_json::json!({ "tools": tools }))
                }
                "tools/call" => {
                    let name = req.params["name"].as_str().unwrap_or("");
                    let args = req.params.get("arguments").cloned().unwrap_or(Value::Null);
                    match self.registry.call(name, args, &self.state) {
                        Ok(text) => Ok(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        })),
                        Err(e) => Ok(serde_json::json!({
                            "content": [{ "type": "text", "text": format!("Error: {e}") }],
                            "isError": true
                        })),
                    }
                }
                "ping" => Ok(serde_json::json!({})),
                "notifications/initialized" => continue,
                _ => Err(RpcError {
                    code: -32601,
                    message: format!("Method not found: {}", req.method),
                }),
            };

            let resp = match result {
                Ok(r) => Response {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(r),
                    error: None,
                },
                Err(e) => Response {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: None,
                    error: Some(e),
                },
            };
            writeln!(
                stdout,
                "{}",
                serde_json::to_string(&resp).unwrap_or_default()
            )
            .ok();
            stdout.flush().ok();
        }
    }
}
