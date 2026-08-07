//! plugin-stage-memory — MCP server for memory fact + paradigm context injection.
use std::io::{BufRead, BufReader, Write};
use std::fs;
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"memory_stage","version":env!("CARGO_PKG_VERSION")},"capabilities":{"prompts":{}}}}),
            "notifications/initialized"=>continue,
            "prompts/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"prompts":[{"name":"relevant_memory","description":"Relevant memory facts and paradigms for the current conversation.","arguments":[{"name":"query","description":"User message to match against","required":true}]}]}}),
            "prompts/get" => {
                let query = req["params"]["arguments"]["query"].as_str().unwrap_or("");
                let facts_dir = req["params"]["_meta"]["factsDir"].as_str().unwrap_or("data/memory/facts");
                let mut result = String::new();
                let q = query.to_lowercase();
                if let Ok(entries) = fs::read_dir(facts_dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.extension().map(|x| x == "md").unwrap_or(false) {
                            if let Ok(c) = fs::read_to_string(&p) {
                                if c.to_lowercase().contains(&q) {
                                    let name = p.file_stem().unwrap_or_default().to_string_lossy();
                                    let desc = c.lines().find(|l| l.starts_with("description:")).map(|l| l.trim_start_matches("description:").trim()).unwrap_or("");
                                    result.push_str(&format!("- [{name}](facts/{name}.md) — {desc}\n"));
                                }
                            }
                        }
                    }
                }
                let content = if result.is_empty() { String::new() } else { format!("## Relevant Memory\n\n{result}") };
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"description":"Memory context","messages":[{"role":"user","content":content}]}})
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
