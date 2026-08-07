//! plugin-download — MCP server for file downloading.
use std::io::{BufRead, BufReader, Write};
fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = writeln!(stdout,r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#,e); let _=stdout.flush(); continue; }};
        let method = req["method"].as_str().unwrap_or(""); let id = req["id"].clone();
        let resp = match method {
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"download","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"download","description":"Download a file from URL to local path.","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"dest":{"type":"string","description":"Destination file path"}},"required":["url","dest"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let result = (|| -> Result<String,String> {
                    let url = args["url"].as_str().ok_or("Missing 'url'")?;
                    let dest = args["dest"].as_str().ok_or("Missing 'dest'")?;
                    let resp = ureq::get(url).call().map_err(|e| format!("Download failed: {e}"))?;
                    let mut body = resp.into_body();
                    let buf = body.read_to_vec().map_err(|e| format!("Read: {e}"))?;
                    if let Some(parent) = std::path::Path::new(dest).parent() { let _ = std::fs::create_dir_all(parent); }
                    std::fs::write(dest, &buf).map_err(|e| format!("Write: {e}"))?;
                    Ok(format!("Downloaded {} bytes to '{dest}'", buf.len()))
                })();
                match result {
                    Ok(t) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":t}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()); let _ = stdout.flush();
    }
}
