//! plugin-verify — MCP server for build/test verification.
#![allow(clippy::disallowed_methods)] // Plugin uses Command to run cargo check/test
use std::io::{BufRead, BufReader, Write};
use std::process::Command;
fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let method = req["method"].as_str().unwrap_or(""); let id = req["id"].clone();
        let resp = match method {
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"verify","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"verify","description":"Run cargo check or cargo test to verify code changes.","inputSchema":{"type":"object","properties":{"kind":{"type":"string","description":"'check' (fast) or 'test' (full)","enum":["check","test"]},"crate_name":{"type":"string","description":"Specific crate to test (optional)"}},"required":["kind"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let kind = args["kind"].as_str().unwrap_or("check");
                let crate_name = args["crate_name"].as_str();
                let mut cmd = Command::new("cargo");
                cmd.arg(kind).arg("--workspace");
                if let Some(c) = crate_name { cmd.args(["-p", c]); }
                let result = match cmd.output() {
                    Ok(o) => {
                        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                        (o.status.success(), text)
                    }
                    Err(e) => (false, format!("Failed to run cargo: {e}")),
                };
                if result.0 { serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("✅ Verification passed\n{}",result.1)}]}}) }
                else { serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("❌ Verification failed\n{}",result.1)}],"isError":true}}) }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()); let _ = stdout.flush();
    }
}
