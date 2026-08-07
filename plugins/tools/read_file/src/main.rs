//! plugin-read-file — MCP server for file reading.
use std::io::{BufRead, BufReader, Write};
fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }
        };
        let method = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();
        let resp = match method {
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"read_file","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"read_file","description":"Read a file from the filesystem.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"File path to read"}},"required":["path"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let name = req["params"]["name"].as_str().unwrap_or("");
                let result = match name {
                    "read_file" => match args["path"].as_str() {
                        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("Failed to read '{path}': {e}")),
                        None => Err("Missing 'path' parameter".into()),
                    },
                    _ => Err(format!("Unknown tool: {name}")),
                };
                match result {
                    Ok(content) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":content}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
        let _ = stdout.flush();
    }
}
