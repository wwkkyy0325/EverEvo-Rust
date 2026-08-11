//! plugin-memory — MCP server for persistent memory operations.
//! Communicates via JSON-RPC 2.0 over stdin/stdout.
//! Provides: memory_search, memory_save, memory_list tools.
//! Reads/writes facts from the filesystem directory passed via MCP initialize.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

static FACTS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn facts_dir() -> &'static PathBuf {
    FACTS_DIR.get_or_init(|| {
        // Priority: 1) MCP initialize factsDir param, 2) EVEREVO_FACTS_DIR env,
        // 3) fallback to ./data/memory-facts relative to cwd.
        if let Ok(dir) = std::env::var("EVEREVO_FACTS_DIR") {
            let p = PathBuf::from(&dir);
            let _ = std::fs::create_dir_all(&p);
            return p;
        }
        let default = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("data")
            .join("memory-facts");
        let _ = std::fs::create_dir_all(&default);
        default
    })
}

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();

    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v, Err(e) => {
                let _ = writeln!(stdout, r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"Parse error: {}"}},"id":null}}"#, e);
                let _ = stdout.flush(); continue;
            }
        };

        let method = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();
        let response = match method {
            "initialize" => {
                // Store facts directory from initialize params
                if let Some(dir) = req["params"]["factsDir"].as_str() {
                    let _ = FACTS_DIR.set(PathBuf::from(dir));
                }
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion":"2025-03-26",
                        "serverInfo":{"name":"memory","version":env!("CARGO_PKG_VERSION")},
                        "capabilities":{"tools":{}}
                    }
                })
            }
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{"tools":[
                    {"name":"memory_search","description":"Search persistent memory facts by keyword.","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},
                    {"name":"memory_save","description":"Save a fact to persistent memory.","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"},"content":{"type":"string"}},"required":["name","description","content"]}},
                    {"name":"memory_list","description":"List all saved memory facts.","inputSchema":{"type":"object","properties":{},"required":[]}}
                ]}
            }),
            "tools/call" => {
                let params = &req["params"];
                let name = params["name"].as_str().unwrap_or("");
                let args = &params["arguments"];
                let result = match name {
                    "memory_search" => search_facts(args["query"].as_str().unwrap_or("")),
                    "memory_save" => save_fact(args["name"].as_str().unwrap_or(""), args["description"].as_str().unwrap_or(""), args["content"].as_str().unwrap_or("")),
                    "memory_list" => list_facts(),
                    _ => Err(format!("Unknown tool: {name}")),
                };
                match result {
                    Ok(content) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":content}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {method}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}

fn search_facts(query: &str) -> Result<String, String> {
    let dir = facts_dir();
    if !dir.exists() { return Ok("No facts stored yet.".into()); }
    let q = query.to_lowercase();
    let mut results = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let path = entry.path();
        if path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.to_lowercase().contains(&q) {
                    let name = path.file_stem().unwrap_or_default().to_string_lossy();
                    let desc = content.lines().find(|l| l.starts_with("description:"))
                        .map(|l| l.trim_start_matches("description:").trim())
                        .unwrap_or("");
                    results.push(format!("- {name}: {desc}"));
                }
            }
        }
    }
    if results.is_empty() { Ok(format!("No facts matching '{query}'.")) }
    else { Ok(format!("Found {} facts:\n{}", results.len(), results.join("\n"))) }
}

fn save_fact(name: &str, description: &str, content: &str) -> Result<String, String> {
    let dir = facts_dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    let path = dir.join(format!("{name}.md"));
    let md = format!("---\nname: {name}\ndescription: {description}\n---\n\n{content}\n");
    std::fs::write(&path, md).map_err(|e| format!("write: {e}"))?;
    Ok(format!("Saved fact '{name}': {description}"))
}

fn list_facts() -> Result<String, String> {
    let dir = facts_dir();
    if !dir.exists() { return Ok("No facts stored yet.".into()); }
    let mut facts = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        if entry.path().extension().map(|e| e == "md").unwrap_or(false) {
            facts.push(entry.path().file_stem().unwrap_or_default().to_string_lossy().into_owned());
        }
    }
    facts.sort();
    Ok(format!("{} facts: {}", facts.len(), facts.join(", ")))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;
    use tempfile::TempDir;

    /// A single shared temp directory for all memory plugin tests.
    /// `FACTS_DIR` is a process-level `OnceLock` — once set, it never changes.
    /// We use a `LazyLock` to create one shared directory for all tests.
    static TEST_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

    fn init_test_env() {
        std::env::set_var(
            "EVEREVO_FACTS_DIR",
            TEST_DIR.path().to_string_lossy().as_ref(),
        );
        // Clear previous test artifacts
        let dir = TEST_DIR.path();
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
    }

    #[test]
    fn test_list_empty() {
        init_test_env();
        let result = list_facts().unwrap();
        // When dir exists but is empty, list returns "N facts: " with N=0
        assert!(result.contains("0 facts"), "Expected '0 facts' in: {result}");
    }

    #[test]
    fn test_save_and_list() {
        init_test_env();
        save_fact("test-fact", "A test description", "Some content here").unwrap();
        let result = list_facts().unwrap();
        assert!(result.contains("test-fact"), "Expected 'test-fact' in: {result}");
    }

    #[test]
    fn test_search_finds_match() {
        init_test_env();
        save_fact("unique-search", "Search test", "The quick brown fox jumps over the lazy dog").unwrap();
        let result = search_facts("brown fox").unwrap();
        assert!(result.contains("unique-search"), "Expected 'unique-search' in: {result}");
    }

    #[test]
    fn test_search_no_match() {
        init_test_env();
        save_fact("only-fact", "Only fact", "hello world").unwrap();
        let result = search_facts("nonexistent-xyz").unwrap();
        assert!(result.contains("No facts matching"), "Expected 'No facts matching' in: {result}");
    }

    #[test]
    fn test_duplicate_save_overwrites() {
        init_test_env();
        save_fact("dup", "First save", "content v1").unwrap();
        save_fact("dup", "Second save", "content v2").unwrap();

        let result = list_facts().unwrap();
        // Should only appear once in list
        let count = result.matches("dup").count();
        assert_eq!(count, 1, "Duplicate fact should only appear once, found {count}");

        // The file content should contain the second save's data
        let path = TEST_DIR.path().join("dup.md");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("content v2"), "File should contain second save's content");
    }

    #[test]
    fn test_save_creates_markdown_frontmatter() {
        init_test_env();
        save_fact("fmt-test", "Format test desc", "Body content here").unwrap();

        let path = TEST_DIR.path().join("fmt-test.md");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("---"));
        assert!(content.contains("name: fmt-test"));
        assert!(content.contains("description: Format test desc"));
        assert!(content.contains("Body content here"));
    }

    #[test]
    fn test_search_case_insensitive() {
        init_test_env();
        save_fact("case-test", "Case test", "UPPERCASE CONTENT lower").unwrap();
        let r1 = search_facts("uppercase").unwrap();
        let r2 = search_facts("UPPERCASE").unwrap();
        let r3 = search_facts("UpPeRcAsE").unwrap();
        assert!(r1.contains("case-test"));
        assert!(r2.contains("case-test"));
        assert!(r3.contains("case-test"));
    }
}
