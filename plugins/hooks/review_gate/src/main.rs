//! plugin-hook-review-gate — MCP server for pre-execution tool review.
use std::io::{BufRead, BufReader, Write};
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"review_gate","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"review_pre_execute","description":"Review a tool call before execution. Returns: block reason (error) or ok.","inputSchema":{"type":"object","properties":{"tool_name":{"type":"string"},"params":{"type":"object"}},"required":["tool_name"]}}]}}),
            "tools/call" => {
                let args=&req["params"]["arguments"];
                let _tool=args["tool_name"].as_str().unwrap_or("");
                let result: Result<String,String> = (|| {
                    // Empty/broken params check
                    if let Some(obj)=args["params"].as_object() {
                        for (k,v) in obj { if let Some(s)=v.as_str() { if s.trim().is_empty() && is_required(k) { return Err(format!("Required param '{k}' is empty")); }}}
                    }
                    Ok("ok".into())
                })();
                match result {
                    Ok(_) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"allowed"}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
fn is_required(k:&str)->bool{ matches!(k,"command"|"file_path"|"query"|"url"|"content"|"message") }
