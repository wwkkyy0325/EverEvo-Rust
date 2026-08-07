//! plugin-web-fetch — MCP server for HTTP URL fetching with HTML-to-text conversion.
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
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"web_fetch","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"web_fetch","description":"Fetch a URL and return content as markdown/text.","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"max_chars":{"type":"integer","description":"Max characters (default: 10000)"}},"required":["url"]}}]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let url = args["url"].as_str().unwrap_or("");
                let max = args["max_chars"].as_u64().unwrap_or(10000) as usize;
                let result = (|| -> Result<String,String> {
                    let resp = ureq::get(url).header("User-Agent","EverEvo-Plugin-WebFetch/1.0").call().map_err(|e| format!("Request failed: {e}"))?;
                    let mut body = resp.into_body();
                    let html = body.read_to_string().map_err(|e| format!("Read: {e}"))?;
                    let text = strip_html(&html);
                    if text.len() > max { Ok(format!("{}…\n[truncated at {max} chars]", &text[..max])) }
                    else { Ok(text) }
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
fn strip_html(html: &str) -> String {
    let mut r = String::new(); let mut tag = false;
    for c in html.chars() { match c { '<' => tag = true, '>' => tag = false, _ if !tag => r.push(c), _ => {} } }
    r.replace("&amp;","&").replace("&lt;","<").replace("&gt;",">").replace("&quot;","\"").trim().to_string()
}
