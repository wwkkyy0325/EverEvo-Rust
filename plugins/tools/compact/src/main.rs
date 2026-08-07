//! plugin-compact — MCP server for context compaction trigger.
use std::io::{BufRead, BufReader, Write};
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"compact","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"compact","description":"Request context compaction to free token space.","inputSchema":{"type":"object","properties":{"focus":{"type":"string","description":"What to focus on after compaction"}},"required":[]}}]}}),
            "tools/call"=>{
                let focus = req["params"]["arguments"]["focus"].as_str().unwrap_or("general");
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{
                        "content":[{
                            "type":"text",
                            "text": format!("[COMPACT_SIGNAL] focus={focus} — Kernel should summarize conversation history, retaining key decisions and open issues. The in-process CompactTool handles the focus-channel integration with AgentLoop autocompact.")
                        }]
                    }
                })
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
