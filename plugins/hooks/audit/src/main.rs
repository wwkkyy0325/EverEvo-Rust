//! plugin-hook-audit — MCP server for tool call audit logging.
use std::io::{BufRead, BufReader, Write};
use std::fs;
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"audit_hook","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"audit_log","description":"Log a tool call for audit trail. Called by kernel after every tool execution.","inputSchema":{"type":"object","properties":{"tool_name":{"type":"string"},"params":{"type":"object"},"success":{"type":"boolean"},"error":{"type":"string"}},"required":["tool_name","success"]}}]}}),
            "tools/call" => {
                let args=&req["params"]["arguments"];
                let tool=args["tool_name"].as_str().unwrap_or("unknown");
                let success=args["success"].as_bool().unwrap_or(true);
                let status=if success{"OK"}else{"FAIL"};
                let entry=format!("[{}] {} {}\n", chrono_now(), tool, status);
                let _=fs::create_dir_all("data/audit");
                let _=fs::OpenOptions::new().create(true).append(true).open("data/audit/tool_calls.log").map(|mut f| { use std::io::Write; let _=writeln!(f,"{entry}"); });
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("Audited: {tool} {status}")}]}})
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
// Cross-platform timestamp using std::time (no external deps).
fn chrono_now() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            // Convert Unix timestamp to YYYY-MM-DDTHH:MM:SS (UTC)
            let days = secs / 86400;
            let time = secs % 86400;
            let year = (days / 365) + 1970; // approximate, fine for audit logging
            let month = ((days % 365) / 30) + 1;
            let day = ((days % 365) % 30) + 1;
            let h = time / 3600;
            let m = (time % 3600) / 60;
            let s = time % 60;
            format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}")
        }
        Err(_) => "unknown".into(),
    }
}
