//! plugin-list-dir — MCP server for directory listing.
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
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"list_dir","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"list_dir","description":"List files in a directory.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"Directory path (default: .)"}},"required":[]}}]}}),
            "tools/call" => {
                let dir = req["params"]["arguments"]["path"].as_str().unwrap_or(".");
                let result: Result<String,String> = (|| {
                    let mut lines = vec![];
                    for e in std::fs::read_dir(dir).map_err(|e| format!("read_dir: {e}"))? {
                        let e = e.map_err(|e| format!("entry: {e}"))?;
                        let meta = e.metadata().map_err(|e| format!("meta: {e}"))?;
                        let kind = if meta.is_dir() { "d" } else { "f" };
                        let size = if meta.is_file() { format!("{:>8}", meta.len()) } else { "       -".into() };
                        lines.push(format!("{kind} {size} {}", e.file_name().to_string_lossy()));
                    }
                    lines.sort();
                    Ok(if lines.is_empty() { format!("Directory '{dir}' is empty.") } else { lines.join("\n") })
                })();
                match result {
                    Ok(t) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":t}]}}),
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
