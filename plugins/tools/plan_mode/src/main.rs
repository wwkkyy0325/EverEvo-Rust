//! plugin-plan-mode — MCP server for plan mode (Enter/Exit read-only exploration).
use std::io::{BufRead, BufReader, Write};
use std::fs;
use std::sync::Mutex;
static PLAN_STATE: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn main() {
    let stdin = BufReader::new(std::io::stdin()); let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let m=req["method"].as_str().unwrap_or(""); let id=req["id"].clone();
        let resp=match m {
            "initialize"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"plan_mode","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized"=>continue,
            "tools/list"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                {"name":"enter_plan_mode","description":"Enter read-only exploration phase before implementation.","inputSchema":{"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}},
                {"name":"exit_plan_mode","description":"Submit plan for approval and exit plan mode.","inputSchema":{"type":"object","properties":{"plan":{"type":"string"},"session_id":{"type":"string"}},"required":["plan","session_id"]}}
            ]}}),
            "tools/call"=>{
                let args=&req["params"]["arguments"];
                let name=req["params"]["name"].as_str().unwrap_or("");
                let sid=args["session_id"].as_str().unwrap_or("");
                let result=match name {
                    "enter_plan_mode"=>{
                        let mut state=PLAN_STATE.lock().unwrap();
                        if state.contains(&sid.to_string()) { Err("Already in plan mode".into()) }
                        else { state.push(sid.to_string()); let _=fs::create_dir_all("data/plans"); Ok("Plan mode entered. Read-only exploration phase. Use exit_plan_mode to submit your plan.".into()) }
                    }
                    "exit_plan_mode"=>{
                        let plan=args["plan"].as_str().unwrap_or("");
                        let mut state=PLAN_STATE.lock().unwrap();
                        if let Some(pos)=state.iter().position(|s|s==sid) { state.remove(pos); }
                        let _=fs::create_dir_all("data/plans");
                        let _=fs::write(format!("data/plans/{}.md", slugify(plan)), format!("# Plan\n\n{plan}\n\nSession: {sid}"));
                        Ok("Plan submitted. Waiting for user approval.".to_string())
                    }
                    _=>Err(format!("Unknown: {name}")),
                };
                match result {
                    Ok(t)=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":t}]}}),
                    Err(e)=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping"=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _=writeln!(stdout,"{}",serde_json::to_string(&resp).unwrap()); let _=stdout.flush();
    }
}
fn slugify(s:&str)->String{s.chars().take(80).map(|c|if c.is_alphanumeric()||c==' '||c=='-'{c}else{' '}).collect::<String>().split_whitespace().take(6).collect::<Vec<_>>().join("-").to_lowercase()}
