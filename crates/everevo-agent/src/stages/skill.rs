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

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        // Hot-reload: re-scan data/skills/ if changed since last check.
        self.registry.check_rescan();

        // Selective injection: only inject skills relevant to the user's message.
        // Falls back to full metadata list when the message is empty (e.g. first turn).
        if ctx.user_message.is_empty() {
            let metadata = self.registry.list_metadata();
            if metadata.is_empty() {
                return None;
            }
            let content: String = metadata
                .iter()
                .map(|(name, desc)| format!("- **{name}**: {desc}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(ContextFragment {
                label: "Available Skills".into(),
                messages: vec![LlmMessage::user(format!(
                    "## Available Skills\n\n{content}\n\n\
                     To use a skill, say \"use the {{name}}\" skill or invoke it by name."
                ))],
            });
        }

        // Selective: only top-matched skills
        let matched = self.registry.find_relevant(&ctx.user_message);
        if matched.is_empty() {
            return None; // no match → inject nothing, save tokens
        }

        let content: String = matched
            .iter()
            .map(|(s, score)| {
                format!(
                    "- **{name}** (match {pct:.0}%): {desc}",
                    name = s.name,
                    pct = (score / 8.0 * 100.0).min(100.0),
                    desc = s.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Some(ContextFragment {
            label: "Relevant Skills".into(),
            messages: vec![LlmMessage::user(format!(
                "## Relevant Skills\n\n{content}\n\n\
                 Use the Skill tool (action='load') to read a skill's full instructions."
            ))],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{Skill, SkillRegistry};
    use std::path::PathBuf;
    use std::sync::RwLock;
    use std::time::SystemTime;

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
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
        }];
        let registry = Arc::new(SkillRegistry {
            skills: RwLock::new(skills),
            skills_dir: PathBuf::from("test"),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
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
            skills: RwLock::new(vec![]),
            skills_dir: PathBuf::from("test"),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
        });
        let stage = SkillStage::new(registry);
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }
}
