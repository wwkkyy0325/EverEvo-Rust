//! plugin-hook-reflect-gate — MCP server for post-execution reflection.
use std::io::{BufRead, BufReader, Write};
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"reflect_gate","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"reflect_post_execute","description":"Analyze tool execution result and return feedback hints.","inputSchema":{"type":"object","properties":{"tool_name":{"type":"string"},"success":{"type":"boolean"},"output":{"type":"string"},"error":{"type":"string"}},"required":["tool_name","success"]}}]}}),
            "tools/call" => {
                let args=&req["params"]["arguments"];
                let tool=args["tool_name"].as_str().unwrap_or("");
                let success=args["success"].as_bool().unwrap_or(true);
                let output=args["output"].as_str().unwrap_or("");
                let hint = if !success {
                    let lower = output.to_lowercase();
                    if lower.contains("command not found") { Some("Tip: run `which <command>` to check if it's installed.".into()) }
                    else if lower.contains("permission denied") { Some("Permission denied. Explain what you need — user can grant access.".into()) }
                    else if lower.contains("connection refused") { Some("Connection error. Try HTTPS instead of SSH for git operations.".into()) }
                    else { None }
                } else if output.trim().is_empty() { Some("Warning: tool returned empty output — may indicate silent failure.".into()) }
                else { None };
                let text = hint.unwrap_or_else(|| format!("Reflected: {tool} {}", if success {"OK"} else {"FAIL"}));
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}})
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
