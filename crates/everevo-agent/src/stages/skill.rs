//! Skill context stage — injects available skill names + descriptions.
//!
//! Stage 1 only — lightweight metadata injection so the LLM knows what
//! skills exist. Full skill bodies are loaded on-demand when invoked.

use std::sync::Arc;

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

use crate::skill::SkillRegistry;

/// Injects available skill names + descriptions into the LLM context.
pub struct SkillStage {
    registry: Arc<SkillRegistry>,
}

impl SkillStage {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

impl ContextStage for SkillStage {
    fn priority(&self) -> i32 {
        2 // after PersonaStage(1), before MemoryStage(3)
    }

    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let metadata = self.registry.list_metadata();
        if metadata.is_empty() {
            return None;
        }
        let content: String = metadata
            .iter()
            .map(|(name, desc)| format!("- **{name}**: {desc}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(ContextFragment {
            label: "Available Skills".into(),
            messages: vec![LlmMessage::user(format!(
                "## Available Skills\n\n{content}\n\n\
                 To use a skill, say \"use the {{name}}\" skill or invoke it by name."
            ))],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{Skill, SkillRegistry};
    use std::path::PathBuf;

    #[test]
    fn test_skill_stage_builds_context_fragment() {
        let skills = vec![Skill {
            name: "test-skill".into(),
            description: "A test skill for testing".into(),
            body: "body".into(),
            tools: vec![],
            when_to_use: vec![],
            persona: None,
            path: PathBuf::from("test"),
        }];
        let registry = Arc::new(SkillRegistry {
            skills,
            skills_dir: PathBuf::from("test"),
        });
        let stage = SkillStage::new(registry);
        let ctx = ContextBuildContext::default();

        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.label, "Available Skills");
        assert_eq!(fragment.messages.len(), 1);
        let content = &fragment.messages[0].content;
        assert!(content.contains("Available Skills"));
        assert!(content.contains("**test-skill**"));
        assert!(content.contains("A test skill for testing"));
    }

    #[test]
    fn test_skill_stage_empty_registry_returns_none() {
        let registry = Arc::new(SkillRegistry {
            skills: vec![],
            skills_dir: PathBuf::from("test"),
        });
        let stage = SkillStage::new(registry);
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }
}
