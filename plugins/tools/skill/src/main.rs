//! plugin-skill — MCP server for skill discovery and authoring.
//!
//! Tools:
//! - skill_list    — list all available skill names
//! - skill_load    — load full body of a skill by name
//! - skill_search  — semantic retrieval across skills (name + desc + triggers)
//! - skill_compose — structured skill authoring with frontmatter validation
#![allow(clippy::possible_missing_else, clippy::manual_strip)] // Compact parser style
use std::io::{BufRead, BufReader, Write};
use std::fs;

/// Lightweight skill metadata parsed from SKILL.md frontmatter.
struct SkillMeta {
    name: String,
    description: String,
    when_to_use: Vec<String>,
    body_preview: String,
}

/// Parse a SKILL.md file into SkillMeta. Returns None on parse failure.
fn parse_skill_meta(content: &str) -> Option<SkillMeta> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let front = rest
        .split("\n---")
        .next()
        .or_else(|| rest.split("\r\n---").next())?;

    let mut name = String::new();
    let mut description = String::new();
    let mut when_to_use = Vec::new();
    let mut in_when = false;

    for line in front.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("name:") {
            name = trimmed["name:".len()..].trim().to_string();
        } else if trimmed.starts_with("description:") {
            description = trimmed["description:".len()..].trim().to_string();
        } else if trimmed == "when_to_use:" {
            in_when = true;
        } else if in_when && trimmed.starts_with("- ") {
            when_to_use.push(trimmed["- ".len()..].trim().to_string());
        } else if in_when && !trimmed.starts_with('-') && !trimmed.is_empty() {
            // continuation of previous trigger line
            if let Some(last) = when_to_use.last_mut() {
                last.push(' ');
                last.push_str(trimmed);
            }
        } else if trimmed.is_empty() {
            in_when = false;
        }
    }

    // Body preview: first 200 chars after frontmatter
    let body_start = content.find("\n---").map(|p| p + 4).unwrap_or(0);
    let body = &content[body_start..];
    let body_preview = body.chars().take(200).collect::<String>();

    if name.is_empty() {
        return None;
    }
    Some(SkillMeta { name, description, when_to_use, body_preview })
}

/// Compute a relevance score (0-100) for a skill against a query.
/// Simple token-overlap + bonus for name match.
fn relevance_score(meta: &SkillMeta, query: &str) -> u32 {
    let query_lower = query.to_lowercase();
    let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();
    if query_tokens.is_empty() { return 0; }

    let name_lower = meta.name.to_lowercase();
    let desc_lower = meta.description.to_lowercase();
    let triggers: String = meta.when_to_use.join(" ").to_lowercase();

    let mut score = 0u32;
    for token in &query_tokens {
        // Name match → high weight (3x)
        if name_lower.contains(token) { score += 30; }
        // Description match (2x)
        if desc_lower.contains(token) { score += 20; }
        // Trigger match (1x)
        if triggers.contains(token) { score += 10; }
        // Body preview match (1x)
        if meta.body_preview.to_lowercase().contains(token) { score += 10; }
    }
    // Exact name match bonus
    if name_lower == query_lower { score += 50; }
    // Cap at 100
    score.min(100)
}

/// Search across all skills, return top `limit` results sorted by relevance.
fn search_skills(dir: &str, query: &str, limit: usize) -> String {
    let mut results: Vec<(u32, SkillMeta)> = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            let md_path = skill_dir.join("SKILL.md");
            if !md_path.exists() { continue; }
            if let Ok(content) = fs::read_to_string(&md_path) {
                if let Some(meta) = parse_skill_meta(&content) {
                    let score = relevance_score(&meta, query);
                    if score > 0 {
                        results.push((score, meta));
                    }
                }
            }
        }
    }

    if results.is_empty() {
        return format!("No skills found matching '{query}'.");
    }

    // Sort by score descending, then take top-N
    results.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    results.truncate(limit);

    let lines: Vec<String> = results
        .into_iter()
        .map(|(score, meta)| {
            let triggers = if meta.when_to_use.is_empty() {
                String::new()
            } else {
                format!("\n  When: {}", meta.when_to_use.join("; "))
            };
            format!(
                "[{}%] **{}** — {}{}\n  Load: `skill_load name=\"{}\"`",
                score, meta.name, meta.description, triggers, meta.name
            )
        })
        .collect();

    format!(
        "Found {} skill(s) for '{query}':\n\n{}",
        lines.len(),
        lines.join("\n\n")
    )
}

/// Compose a new skill with proper frontmatter and save it.
fn compose_skill(dir: &str, name: &str, description: &str, triggers: &str, body: &str) -> Result<String, String> {
    // Validate name
    if name.is_empty() || name.len() > 64 {
        return Err("Name must be 1-64 characters.".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Name must be alphanumeric, dashes, or underscores.".into());
    }
    if body.trim().len() < 20 {
        return Err("Body must be at least 20 characters.".into());
    }

    let skill_dir = format!("{}/{}", dir, name);
    fs::create_dir_all(&skill_dir).map_err(|e| format!("mkdir: {e}"))?;

    // Build frontmatter
    let mut frontmatter = format!(
        "---\nname: {name}\ndescription: {}\n",
        description.trim()
    );
    let trigger_lines: Vec<&str> = triggers
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if !trigger_lines.is_empty() {
        frontmatter.push_str("when_to_use:\n");
        for t in &trigger_lines {
            frontmatter.push_str(&format!("  - {}\n", t.trim().trim_start_matches("- ")));
        }
    }
    frontmatter.push_str("---\n\n");
    frontmatter.push_str(body.trim());
    frontmatter.push('\n');

    let md_path = format!("{}/SKILL.md", skill_dir);
    let exists = std::path::Path::new(&md_path).exists();

    fs::write(&md_path, &frontmatter).map_err(|e| format!("write: {e}"))?;

    if exists {
        Ok(format!("Skill '{name}' updated. {} triggers, {} bytes.", trigger_lines.len(), frontmatter.len()))
    } else {
        Ok(format!("Skill '{name}' created. {} triggers, {} bytes.", trigger_lines.len(), frontmatter.len()))
    }
}

fn main() {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    for line in stdin.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(stdout, r#"{{"jsonrpc":"2.0","error":{{"code":-32700,"message":"{}"}},"id":null}}"#, e);
                let _ = stdout.flush();
                continue;
            }
        };
        let m = req["method"].as_str().unwrap_or("");
        let id = req["id"].clone();
        let resp = match m {
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","serverInfo":{"name":"skill","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}}}),
            "notifications/initialized" => continue,
            "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                {"name":"skill_list","description":"List all available skill names.","inputSchema":{"type":"object","properties":{},"required":[]}},
                {"name":"skill_load","description":"Load the full body of a skill by name.","inputSchema":{"type":"object","properties":{"name":{"type":"string","description":"Skill name (folder name)"}},"required":["name"]}},
                {"name":"skill_search","description":"Search skills by query. Matches against name, description, when_to_use triggers, and body content. Returns top-N most relevant skills. Use this when you have many skills and need to find the right one for the current task.","inputSchema":{"type":"object","properties":{"query":{"type":"string","description":"Search query describing what you need"},"limit":{"type":"integer","description":"Max results (default 5)"}},"required":["query"]}},
                {"name":"skill_compose","description":"Create or update a skill with proper frontmatter. Use this to write new skills or edit existing ones. The tool validates the name, generates YAML frontmatter with when_to_use triggers, and writes the SKILL.md file.","inputSchema":{"type":"object","properties":{"name":{"type":"string","description":"Skill name (folder slug, alphanumeric+dashes)"},"description":{"type":"string","description":"One-line summary of what the skill does"},"triggers":{"type":"string","description":"When to use this skill — one trigger per line"},"body":{"type":"string","description":"Full skill body content (markdown)"}},"required":["name","description","body"]}}
            ]}}),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let action = req["params"]["name"].as_str().unwrap_or("");
                let skill_dir = "data/skills";
                let result: Result<String, String> = (|| {
                    match action {
                    "skill_list" => {
                        let mut list = vec![];
                        if let Ok(entries) = fs::read_dir(skill_dir) {
                            for e in entries.flatten() {
                                if e.path().join("SKILL.md").exists() {
                                    list.push(e.file_name().to_string_lossy().into_owned());
                                }
                            }
                        }
                        if list.is_empty() {
                            Ok("No skills installed. Use skill_compose to create one.".into())
                        } else {
                            list.sort();
                            let count = list.len();
                            Ok(format!("{count} skill(s):\n{}", list.join("\n")))
                        }
                    }
                    "skill_load" => {
                        let name = args["name"].as_str().ok_or("Missing 'name' parameter")?;
                        let path = format!("{skill_dir}/{name}/SKILL.md");
                        fs::read_to_string(&path)
                            .map_err(|e| format!("Cannot load skill '{name}': {e}"))
                    }
                    "skill_search" => {
                        let query = args["query"].as_str().unwrap_or("");
                        let limit = args["limit"].as_u64().unwrap_or(5) as usize;
                        Ok(search_skills(skill_dir, query, limit))
                    }
                    "skill_compose" => {
                        let name = args["name"].as_str().ok_or("Missing 'name' parameter")?;
                        let desc = args["description"].as_str().unwrap_or("");
                        let triggers = args["triggers"].as_str().unwrap_or("");
                        let body = args["body"].as_str().unwrap_or("");
                        compose_skill(skill_dir, name, desc, triggers, body)
                    }
                    _ => Err(format!("Unknown action: {action}. Use skill_list, skill_load, skill_search, or skill_compose.")),
                }
                })();
                match result {
                    Ok(t) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":t}]}}),
                    Err(e) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
                }
            }
            "ping" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unknown: {m}")}}),
        };
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
        let _ = stdout.flush();
    }
}
