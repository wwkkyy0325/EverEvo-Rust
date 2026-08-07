//! plugin-stage-persona — MCP server for agent personality context.
use std::io::{BufRead, BufReader, Write};
const PERSONA: &str = "\
## Communication Style\n\n\
- Be direct and concise. Start with the answer, then explain if needed.\n\
- Use tools to DO things — never just describe what to do.\n\
- When a tool fails, diagnose the root cause before retrying.\n\
- Admit when stuck. Better honest than looping.";
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"persona","version":env!("CARGO_PKG_VERSION")},"capabilities":{"prompts":{}}}}),
            "notifications/initialized"=>continue,
            "prompts/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"prompts":[{"name":"persona","description":"Agent communication style and behavioral guidelines for context injection."}]}}),
            "prompts/get" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"description":"Persona context","messages":[{"role":"user","content":PERSONA}]}}),
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
