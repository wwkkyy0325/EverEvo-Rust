//! plugin-memory — MCP server for persistent memory operations.
//! Communicates via JSON-RPC 2.0 over stdin/stdout.
//! Provides: memory_search, memory_save, memory_list tools.
//! Reads/writes facts from the filesystem directory passed via MCP initialize.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

static FACTS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn facts_dir() -> &'static PathBuf {
    FACTS_DIR.get().expect("facts_dir not initialized — MCP initialize must be called first")
}

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();

    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v, Err(e) => {
                let _ = writeln!(stdout, r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"Parse error: {}"}},"id":null}}"#, e);
                let _ = stdout.flush(); continue;
            }
        };

        let method = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();
        let response = match method {
            "initialize" => {
                // Store facts directory from initialize params
                if let Some(dir) = req["params"]["factsDir"].as_str() {
                    let _ = FACTS_DIR.set(PathBuf::from(dir));
                }
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion":"2025-03-26",
                        "serverInfo":{"name":"memory","version":env!("CARGO_PKG_VERSION")},
                        "capabilities":{"tools":{}}
                    }
                })
            }
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{"tools":[
                    {"name":"memory_search","description":"Search persistent memory facts by keyword.","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},
                    {"name":"memory_save","description":"Save a fact to persistent memory.","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"},"content":{"type":"string"}},"required":["name","description","content"]}},
                    {"name":"memory_list","description":"List all saved memory facts.","inputSchema":{"type":"object","properties":{},"required":[]}}
                ]}
            }),
            "tools/call" => {
                let params = &req["params"];
                let name = params["name"].as_str().unwrap_or("");
                let args = &params["arguments"];
                let result = match name {
                    "memory_search" => search_facts(args["query"].as_str().unwrap_or("")),
                    "memory_save" => save_fact(args["name"].as_str().unwrap_or(""), args["description"].as_str().unwrap_or(""), args["content"].as_str().unwrap_or("")),
                    "memory_list" => list_facts(),
                    _ => Err(format!("Unknown tool: {name}")),
                };
                match result {
                    Ok(content) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":content}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}

fn search_facts(query: &str) -> Result<String, String> {
    let dir = facts_dir();
    if !dir.exists() { return Ok("No facts stored yet.".into()); }
    let q = query.to_lowercase();
    let mut results = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let path = entry.path();
        if path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.to_lowercase().contains(&q) {
                    let name = path.file_stem().unwrap_or_default().to_string_lossy();
                    let desc = content.lines().find(|l| l.starts_with("description:"))
                        .map(|l| l.trim_start_matches("description:").trim())
                        .unwrap_or("");
                    results.push(format!("- {name}: {desc}"));
                }
            }
        }
    }
    if results.is_empty() { Ok(format!("No facts matching '{query}'.")) }
    else { Ok(format!("Found {} facts:\n{}", results.len(), results.join("\n"))) }
}

fn save_fact(name: &str, description: &str, content: &str) -> Result<String, String> {
    let dir = facts_dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    let path = dir.join(format!("{name}.md"));
    let md = format!("---\nname: {name}\ndescription: {description}\n---\n\n{content}\n");
    std::fs::write(&path, md).map_err(|e| format!("write: {e}"))?;
    Ok(format!("Saved fact '{name}': {description}"))
}

fn list_facts() -> Result<String, String> {
    let dir = facts_dir();
    if !dir.exists() { return Ok("No facts stored yet.".into()); }
    let mut facts = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        if entry.path().extension().map(|e| e == "md").unwrap_or(false) {
            facts.push(entry.path().file_stem().unwrap_or_default().to_string_lossy().into_owned());
        }
    }
    facts.sort();
    Ok(format!("{} facts: {}", facts.len(), facts.join(", ")))
}
