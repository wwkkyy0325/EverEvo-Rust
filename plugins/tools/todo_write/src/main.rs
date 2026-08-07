//! plugin-todo-write — MCP server for task list management.
use std::io::{BufRead, BufReader, Write};
use std::fs;
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"todo_write","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"todo_write","description":"Create and manage a task list with checkboxes.","inputSchema":{"type":"object","properties":{"todos":{"type":"array","description":"Task items with content, status, activeForm"}},"required":["todos"]}}]}}),
            "tools/call"=>{
                let todos=&req["params"]["arguments"]["todos"];
                let text=serde_json::to_string_pretty(todos).unwrap_or_default();
                let _=fs::create_dir_all("data/todos");
                let _=fs::write("data/todos/current.json",&text);
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("Task list updated ({} items)",todos.as_array().map(|a|a.len()).unwrap_or(0))}]}})
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
