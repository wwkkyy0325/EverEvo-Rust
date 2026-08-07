//! plugin-code-search — MCP server for codebase grep/pattern search.
#![allow(clippy::disallowed_methods)] // Plugin uses Command to spawn rg
use std::io::{BufRead, BufReader, Write};
use std::process::Command;
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
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"code_search","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"code_search","description":"Search codebase with ripgrep.","inputSchema":{"type":"object","properties":{"pattern":{"type":"string","description":"Regex pattern to search"},"path":{"type":"string","description":"Directory to search (default: .)"}},"required":["pattern"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let pattern = args["pattern"].as_str().unwrap_or("");
                let dir = args["path"].as_str().unwrap_or(".");
                let result = Command::new("rg")
                    .args(["--no-heading","-n","--max-count=50", pattern, dir])
                    .output();
                match result {
                    Ok(o) => {
                        let text = String::from_utf8_lossy(&o.stdout);
                        if text.trim().is_empty() {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("No matches for '{pattern}'")}]}})
                        } else {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text.to_string()}]}})
                        }
                    }
                    Err(e) => {
                        // Check if rg is not installed
                        let hint = if e.kind() == std::io::ErrorKind::NotFound {
                            "ripgrep (rg) is not installed. Install it: https://github.com/BurntSushi/ripgrep\n\
                             Or use list_dir + read_file as an alternative for code search."
                        } else {
                            &format!("Search error: {e}")
                        };
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":hint}],"isError":true}})
                    }
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
        let _ = stdout.flush();
    }
}
