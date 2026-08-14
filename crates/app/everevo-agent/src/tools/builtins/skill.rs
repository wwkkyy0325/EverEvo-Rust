//! Skill tool — matches Claude Code's skill discovery and invocation.
//!
//! Skills are SKILL.md files in data/skills/ that provide domain-specific
//! instructions. The LLM can search for and read skills to extend its
//! capabilities without modifying system prompts.
//!
//! Complemented by MCP plugin `plugin-skill`; this in-process version reads
//! the shared SkillRegistry (populated at boot, updated by promote_to_skill)
//! that the plugin cannot access directly.
//!
//! Now backed by `SkillRegistry` so built-in skills (embedded via `include_str!`)
//! are visible alongside user skills created with `PromoteSkillTool`.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use crate::skill::SkillRegistry;

pub struct SkillTool {
    registry: Arc<SkillRegistry>,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        "Invoke a skill by name, list available skills, or create a new skill. \
         Use action='list' to discover available skills, action='load' with a \
         skill name to read its full instructions, or action='create' to write \
         a new skill (name, description, body, when_to_use triggers)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "load", "create", "search", "compose"],
                    "description": "Action: 'list' available skills, 'load' a skill by name, 'search' for relevant skills by query, 'create'/'compose' a new skill"
                },
                "skill": {
                    "type": "string",
                    "description": "Skill name (required for 'load' action)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query for 'search' action — describes what you need"
                },
                "name": {
                    "type": "string",
                    "description": "Name for new skill (required for 'create'/'compose', alphanumeric + hyphen)"
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of what the skill does"
                },
                "when_to_use": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Trigger phrases that activate this skill"
                },
                "body": {
                    "type": "string",
                    "description": "Full skill instructions in Markdown"
                }
            },
            "required": ["action"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let action = params["action"].as_str().unwrap_or("list");

        match action {
            "list" => {
                let metadata = self.registry.list_metadata();
                if metadata.is_empty() {
                    return Ok(ToolOutput::text(
                        "No skills available. Use Skill(action='create', ...) to create one.",
                    ));
                }
                let lines: Vec<String> = metadata
                    .iter()
                    .map(|(name, desc)| format!("- **{name}**: {desc}"))
                    .collect();
                Ok(ToolOutput::text(format!(
                    "Available skills:\n{}",
                    lines.join("\n")
                )))
            }

            "load" => {
                let skill_name = params["skill"].as_str().unwrap_or("");
                if skill_name.is_empty() {
                    return Ok(ToolOutput {
                        content: "Provide a skill name with action='load'.".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }
                match self.registry.get(skill_name) {
                    Some(skill) => Ok(ToolOutput::text(format!(
                        "## Skill: {name}\n\n{body}",
                        name = skill.name,
                        body = skill.body
                    ))),
                    None => Ok(ToolOutput {
                        content: format!(
                            "Skill '{skill_name}' not found. Use Skill(action='list') to see available skills."
                        ),
                        is_error: true,
                        ..Default::default()
                    }),
                }
            }

            "create" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                let description = params["description"].as_str().unwrap_or("").to_string();
                let body = params["body"].as_str().unwrap_or("").to_string();
                let when_to_use: Vec<String> = params["when_to_use"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                if name.is_empty() || body.is_empty() {
                    return Ok(ToolOutput {
                        content: "Both 'name' and 'body' are required for action='create'.".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }

                match crate::skill::promote_to_skill(
                    &self.registry.skills_dir(),
                    &name,
                    &description,
                    &when_to_use,
                    &body,
                ) {
                    Ok(path) => {
                        // Trigger immediate rescan so the new skill is usable
                        let _ = self.registry.rescan();
                        Ok(ToolOutput::text(format!(
                            "Skill '{name}' created at {path}. Available immediately.",
                            path = path.display()
                        )))
                    }
                    Err(e) => Ok(ToolOutput {
                        content: format!("Failed to create skill: {e}"),
                        is_error: true,
                        ..Default::default()
                    }),
                }
            }

            "search" => {
                let query = params["query"].as_str().unwrap_or("");
                if query.is_empty() {
                    return Ok(ToolOutput {
                        content: "Provide a 'query' for action='search'.".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }
                let metadata = self.registry.list_metadata();
                if metadata.is_empty() {
                    return Ok(ToolOutput::text("No skills installed."));
                }
                // Score each skill against the query
                let mut scored: Vec<(u32, String, String)> = metadata
                    .iter()
                    .filter_map(|(name, desc)| {
                        let score = skill_relevance_score(name, desc, &[], query);
                        if score > 0 {
                            Some((score, name.clone(), desc.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                scored.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
                scored.truncate(5);
                if scored.is_empty() {
                    Ok(ToolOutput::text(format!(
                        "No skills matched '{query}'. Use Skill(action='list') to see all skills."
                    )))
                } else {
                    let lines: Vec<String> = scored
                        .iter()
                        .map(|(score, name, desc)| {
                            format!("[{}%] **{name}** — {desc}\n  Load: Skill(action='load', skill=\"{name}\")", score)
                        })
                        .collect();
                    Ok(ToolOutput::text(format!(
                        "Found {} skill(s) for '{query}':\n\n{}",
                        lines.len(),
                        lines.join("\n\n")
                    )))
                }
            }

            "compose" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                let description = params["description"].as_str().unwrap_or("").to_string();
                let body = params["body"].as_str().unwrap_or("").to_string();
                let when_to_use: Vec<String> = params["when_to_use"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // Validate
                if name.is_empty() || body.is_empty() {
                    return Ok(ToolOutput {
                        content: "Both 'name' and 'body' are required for action='compose'.".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }
                if body.trim().len() < 20 {
                    return Ok(ToolOutput {
                        content: "Body must be at least 20 characters for a useful skill.".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }

                match crate::skill::promote_to_skill(
                    &self.registry.skills_dir(),
                    &name,
                    &description,
                    &when_to_use,
                    &body,
                ) {
                    Ok(path) => {
                        let _ = self.registry.rescan();
                        let verb = if path.exists() { "updated" } else { "created" };
                        Ok(ToolOutput::text(format!(
                            "Skill '{name}' {verb} successfully.\nPath: {}\nTriggers: {}\nBody: {} chars",
                            path.display(),
                            when_to_use.len(),
                            body.len(),
                        )))
                    }
                    Err(e) => Ok(ToolOutput {
                        content: format!("Failed to compose skill: {e}"),
                        is_error: true,
                        ..Default::default()
                    }),
                }
            }

            _ => Ok(ToolOutput {
                content: "Unknown action. Use 'list', 'load', 'search', 'compose', or 'create'."
                    .into(),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

/// Simple token-overlap relevance score (0-100) for a skill against a query.
/// Name matches weighted 3x, description 2x, trigger 1x. Caps at 100.
fn skill_relevance_score(name: &str, description: &str, triggers: &[String], query: &str) -> u32 {
    let query_lower = query.to_lowercase();
    let tokens: Vec<&str> = query_lower.split_whitespace().collect();
    if tokens.is_empty() {
        return 0;
    }

    let name_lower = name.to_lowercase();
    let desc_lower = description.to_lowercase();
    let trigger_text = triggers.join(" ").to_lowercase();

    let mut score = 0u32;
    for token in &tokens {
        if name_lower.contains(token) {
            score += 30;
        }
        if desc_lower.contains(token) {
            score += 20;
        }
        if trigger_text.contains(token) {
            score += 10;
        }
    }
    if name_lower == query_lower {
        score += 50;
    }
    score.min(100)
}
