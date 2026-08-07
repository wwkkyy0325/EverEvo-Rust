//! Skill tool — matches Claude Code's skill discovery and invocation.
//!
//! Skills are SKILL.md files in data/skills/ that provide domain-specific
//! instructions. The LLM can search for and read skills to extend its
//! capabilities without modifying system prompts.
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
                    "enum": ["list", "load", "create"],
                    "description": "Action: 'list' available skills, 'load' a skill by name, 'create' a new skill"
                },
                "skill": {
                    "type": "string",
                    "description": "Skill name (required for 'load' action)"
                },
                "name": {
                    "type": "string",
                    "description": "Name for new skill (required for 'create', alphanumeric + hyphen)"
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
                        "No skills available. Use Skill(action='create', ...) to create one."
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
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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

            _ => Ok(ToolOutput {
                content: "Unknown action. Use 'list', 'load', or 'create'.".into(),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
