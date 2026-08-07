//! plugin-stage-best-practices — MCP server returning verification/code-quality rules for context injection.
use std::io::{BufRead, BufReader, Write};
const RULES: &str = "\
## Verification Pipeline (Run After Every Change)\n\n\
- `cargo check --workspace` before committing\n\
- `cargo test --workspace` after meaningful changes\n\
- Never claim completion without fresh verification output\n\
- Fix code, never weaken tests\n\n\
## Code Conventions\n\n\
- Rust: `cargo fmt`, `cargo clippy`. Match existing style.\n\
- Commits: conventional commits (`feat:`, `fix:`, `chore:`)\n\
- Imports: remove imports YOUR changes made unused.\n\n\
## Critical Rules\n\n\
- 2-failure limit: same command fails twice → STOP, diagnose root cause\n\
- Verify before claiming done\n\
- Admit when stuck: 'I tried X,Y,Z. Here's what failed and what I need'";
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"best_practices","version":env!("CARGO_PKG_VERSION")},"capabilities":{"prompts":{}}}}),
            "notifications/initialized"=>continue,
            "prompts/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"prompts":[{"name":"best_practices","description":"Verification rules, code conventions, and critical rules for the agent to follow."}]}}),
            "prompts/get" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"description":"Best practices context","messages":[{"role":"user","content":RULES}]}}),
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
