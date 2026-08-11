//! AgentCard builder — dynamic discovery document from EverEvo state.
//!
//! The AgentCard is served at `/.well-known/agent.json` and announces
//! EverEvo's capabilities to external agents. It is rebuilt on every
//! request so skill/tool changes take effect immediately.

use crate::types::{AgentCapabilities, AgentCard, AgentSkill};
use everevo_core::tool::ToolRegistry;
use std::collections::HashMap;

/// Build an AgentCard from the current server state.
///
/// Skills are derived from the `SkillRegistry`; tools are listed as
/// extensions so external agents know what EverEvo can do.
pub struct AgentCardBuilder {
    name: String,
    url: String,
    version: String,
    streaming: bool,
    push_notifications: bool,
    skills: Vec<SkillDef>,
    tools: Vec<ToolDef>,
}

/// A user-facing or LLM-created skill.
#[derive(Clone)]
pub struct SkillDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// A tool exposed to external agents.
#[derive(Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
}

impl AgentCardBuilder {
    pub fn new(base_url: &str) -> Self {
        Self {
            name: "EverEvo".into(),
            url: base_url.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            streaming: true,
            push_notifications: false, // Phase 2
            skills: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// Register skills from a list of (id, name, description, tags).
    pub fn with_skills(mut self, skills: &[SkillDef]) -> Self {
        self.skills = skills.to_vec();
        self
    }

    /// Register tools from the ToolRegistry.
    pub fn with_tools(mut self, registry: &ToolRegistry) -> Self {
        self.tools = registry
            .names()
            .iter()
            .filter_map(|name| {
                registry.get(name).map(|tool| ToolDef {
                    name: name.to_string(),
                    description: tool.description().to_string(),
                })
            })
            .collect();
        self
    }

    /// Build the final AgentCard.
    pub fn build(self) -> AgentCard {
        let skills: Vec<AgentSkill> = self
            .skills
            .into_iter()
            .map(|s| {
                let mut skill = AgentSkill::new(&s.id, &s.name, &s.description, s.tags);
                skill.examples = vec![];
                skill
            })
            .collect();

        let extensions = if self.tools.is_empty() {
            vec![]
        } else {
            let tool_list: Vec<String> = self
                .tools
                .iter()
                .map(|t| format!("{}: {}", t.name, t.description))
                .collect();
            vec![crate::types::AgentExtension {
                name: "everevo-tools".into(),
                description: format!("Internal tools: {}", tool_list.join(", ")),
                params: HashMap::new(),
            }]
        };

        AgentCard {
            name: self.name,
            description: "Self-evolving desktop AI agent with multi-tool orchestration, \
                          persistent memory, code search, and sub-agent delegation."
                .into(),
            url: self.url,
            version: self.version,
            protocol_version: "0.3.0".into(),
            capabilities: AgentCapabilities {
                streaming: self.streaming,
                push_notifications: self.push_notifications,
                state_transition_history: true,
                extensions,
            },
            skills,
            default_input_modes: vec!["text".into(), "file".into()],
            default_output_modes: vec!["text".into(), "file".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_minimal() {
        let builder = AgentCardBuilder::new("http://localhost:3000");
        let card = builder.build();
        assert_eq!(card.name, "EverEvo");
        assert_eq!(card.url, "http://localhost:3000");
        assert_eq!(card.protocol_version, "0.3.0");
        assert!(card.capabilities.streaming);
    }

    #[test]
    fn test_builder_with_skills() {
        let card = AgentCardBuilder::new("http://localhost:3000")
            .with_skills(&[SkillDef {
                id: "code-review".into(),
                name: "Code Review".into(),
                description: "Review code changes".into(),
                tags: vec!["code".into()],
            }])
            .build();
        assert_eq!(card.skills.len(), 1);
        assert_eq!(card.skills[0].id, "code-review");
    }
}
