//! plugin-write-file — MCP server for file writing.
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
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"write_file","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"write_file","description":"Write content to a file.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"File path to write"},"content":{"type":"string","description":"Content to write"}},"required":["path","content"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let result = match (args["path"].as_str(), args["content"].as_str()) {
                    (Some(path), Some(content)) => {
                        if let Some(parent) = std::path::Path::new(path).parent() { let _ = std::fs::create_dir_all(parent); }
                        std::fs::write(path, content).map(|_| format!("Wrote {} bytes to '{path}'", content.len())).map_err(|e| format!("Failed to write '{path}': {e}"))
                    }
                    _ => Err("Missing 'path' or 'content'".into()),
                };
                match result {
                    Ok(text) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}}),
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
