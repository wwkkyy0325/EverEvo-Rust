//! plugin-stage-skill — MCP server listing available skills.
use std::io::{BufRead, BufReader, Write};
use std::fs;
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() { let line = match line { Ok(l) => l, Err(_) => break }; if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _=writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"skill_stage","version":env!("CARGO_PKG_VERSION")},"capabilities":{"prompts":{}}}}),
            "notifications/initialized"=>continue,
            "prompts/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"prompts":[{"name":"available_skills","description":"List of available skill names and descriptions for the agent to reference."}]}}),
            "prompts/get" => {
                let mut skills = String::from("## Available Skills\n\nUse `skill_load` to get full instructions for any skill.\n\n");
                if let Ok(entries) = fs::read_dir("data/skills") {
                    for e in entries.flatten() {
                        let path = e.path().join("SKILL.md");
                        if path.exists() {
                            if let Ok(content) = fs::read_to_string(&path) {
                                let name = e.file_name().to_string_lossy().into_owned();
                                let desc = content.lines().find(|l| l.starts_with("description:")).map(|l| l.trim_start_matches("description:").trim()).unwrap_or("");
                                skills.push_str(&format!("- **{name}**: {desc}\n"));
                            }
                        }
                    }
                }
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"description":"Available skills","messages":[{"role":"user","content":skills}]}})
            },
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
