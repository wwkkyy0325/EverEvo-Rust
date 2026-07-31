//! Skill tool — matches Claude Code's skill discovery and invocation.
//!
//! Skills are SKILL.md files in data/skills/ that provide domain-specific
//! instructions. The LLM can search for and read skills to extend its
//! capabilities without modifying system prompts.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub struct SkillTool {
    skills_dir: PathBuf,
}

impl SkillTool {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    fn list_skills(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
        names.sort();
        names
    }

    fn read_skill(&self, name: &str) -> Option<String> {
        let path = self.skills_dir.join(name).join("SKILL.md");
        std::fs::read_to_string(&path).ok()
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        "Invoke a skill by name. Skills provide specialized instructions for \
         specific tasks. Use 'list' action to discover available skills, or \
         provide a skill name to load its instructions into context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Skill name to invoke, or 'list' to see available skills"
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments to pass to the skill"
                }
            },
            "required": ["skill"]
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
        let skill = params["skill"].as_str().unwrap_or("");

        if skill == "list" {
            let names = self.list_skills();
            if names.is_empty() {
                return Ok(ToolOutput {
                    content: "No skills found. Place SKILL.md files in data/skills/<name>/ to create skills.".into(),
                    is_error: false,
                 ..Default::default() });
            }
            return Ok(ToolOutput {
                content: format!(
                    "Available skills:\n{}",
                    names
                        .iter()
                        .map(|n| format!("- {n}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
                is_error: false,
                ..Default::default()
            });
        }

        match self.read_skill(skill) {
            Some(content) => {
                let args = params["args"].as_str().unwrap_or("");
                let header = if args.is_empty() {
                    format!("## Skill: {skill}\n\n{content}")
                } else {
                    format!("## Skill: {skill}\n\nArguments: {args}\n\n{content}")
                };
                Ok(ToolOutput {
                    content: header,
                    is_error: false,
                    ..Default::default()
                })
            }
            None => Ok(ToolOutput {
                content: format!(
                    "Skill '{skill}' not found. Use Skill(action='list') to see available skills."
                ),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
