//! plugin-code-search — MCP server for codebase grep/pattern search.
#![allow(clippy::disallowed_methods)] // Plugin uses Command to spawn rg
use std::io::{BufRead, BufReader, Write};
use std::process::Command;

/// Try ripgrep first, then grep, then findstr (Windows), then basic Rust walk.
/// Returns (stdout_string, error_string).
fn run_search(pattern: &str, dir: &str) -> Result<String, String> {
    // ── Tier 1: ripgrep (fastest) ──────────────────────────────────
    match Command::new("rg")
        .args(["--no-heading", "-n", "--max-count=50", pattern, dir])
        .output()
    {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            return Ok(if text.trim().is_empty() { format!("No matches for '{pattern}'") } else { text });
        }
        Ok(o) => {
            // rg found but failed (bad pattern?) — fall through
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.is_empty() { eprintln!("[code_search] rg error: {stderr}"); }
        }
        Err(_) => { /* rg not found — try next */ }
    }

    // ── Tier 2: grep (Unix/Git Bash) ───────────────────────────────
    match Command::new("grep")
        .args(["-rn", "--max-count=50", pattern, dir])
        .output()
    {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            return Ok(if text.trim().is_empty() { format!("No matches for '{pattern}'") } else { text });
        }
        Ok(_) => {}
        Err(_) => {}
    }

    // ── Tier 3: findstr (Windows cmd built-in) ──────────────────────
    #[cfg(windows)]
    {
        match Command::new("findstr")
            .args(["/s", "/n", "/c:", pattern, &format!("{dir}\\*")])
            .output()
        {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                return Ok(if text.trim().is_empty() { format!("No matches for '{pattern}'") } else { text });
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }

    // ── Tier 4: built-in Rust walk (always available) ───────────────
    match basic_grep(pattern, dir) {
        Ok(text) => Ok(text),
        Err(e) => Err(format!(
            "Search failed: {e}\n\n\
             Install ripgrep for faster search: https://github.com/BurntSushi/ripgrep\n\
             Or use list_dir + read_file as an alternative."
        )),
    }
}

/// Recursive file search using only std::fs + string matching.
/// Slow but always available — last-resort fallback.
fn basic_grep(pattern: &str, dir: &str) -> Result<String, String> {
    let mut results = Vec::new();
    let base = std::path::Path::new(dir);
    if !base.exists() {
        return Err(format!("Directory not found: {dir}"));
    }
    walk_dir(base, pattern, &mut results, 50)
        .map_err(|e| format!("Walk error: {e}"))?;
    if results.is_empty() {
        Ok(format!("No matches for '{pattern}'"))
    } else {
        Ok(results.join("\n"))
    }
}

fn walk_dir(
    dir: &std::path::Path,
    pattern: &str,
    results: &mut Vec<String>,
    max: usize,
) -> Result<(), std::io::Error> {
    if results.len() >= max { return Ok(()); }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden dirs and common noise
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            walk_dir(&path, pattern, results, max)?;
        } else if path.is_file() {
            if results.len() >= max { return Ok(()); }
            // Skip binary files by extension
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "exe" | "dll" | "pdb" | "obj" | "rlib" | "png" | "jpg" | "gif" | "ico" | "zip" | "gz" | "tar") {
                    continue;
                }
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (line_no, line) in content.lines().enumerate() {
                    if results.len() >= max { return Ok(()); }
                    if line.contains(pattern) {
                        let rel = path.strip_prefix(std::env::current_dir().unwrap_or_default())
                            .unwrap_or(&path);
                        results.push(format!("{}:{}:{}", rel.display(), line_no + 1, line));
                    }
                }
            }
        }
    }
    Ok(())
}

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
                let result = run_search(pattern, dir);
                match result {
                    Ok(text) => {
                        if text.trim().is_empty() {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("No matches for '{pattern}'")}]}})
                        } else {
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}})
                        }
                    }
                    Err(e) => {
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}})
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
