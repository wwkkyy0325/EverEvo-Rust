//! Persona Stage — injects user communication style and thinking paradigm
//! into the LLM context. Reads from data/memory/persona/profile.json.

use std::path::{Path, PathBuf};

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;
use serde::{Deserialize, Serialize};

// ── Persona Profile ──────────────────────────────────────────────────────

/// User persona profile loaded from disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaProfile {
    pub communication_style: CommunicationStyle,
    pub thinking_paradigm: ThinkingParadigm,
    pub system_prompt_injection: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationStyle {
    pub language: String,  // "zh-CN" | "en"
    pub verbosity: String, // "concise" | "detailed"
    pub formality: String, // "casual" | "formal"
    #[serde(default)]
    pub code_first: bool, // whether user prefers code before explanation
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingParadigm {
    pub decomposition: String,      // "top-down" | "bottom-up"
    pub theory_vs_practice: String, // "theory" | "practice"
}

impl Default for PersonaProfile {
    fn default() -> Self {
        Self {
            communication_style: CommunicationStyle {
                language: "zh-CN".into(),
                verbosity: "concise".into(),
                formality: "casual".into(),
                code_first: true,
            },
            thinking_paradigm: ThinkingParadigm {
                decomposition: "top-down".into(),
                theory_vs_practice: "practice".into(),
            },
            system_prompt_injection: String::new(),
        }
    }
}

// ── PersonaStage ─────────────────────────────────────────────────────────

/// Injects user persona (communication style + thinking paradigm) into the
/// LLM context, right after the system prompt.
pub struct PersonaStage {
    profile_path: PathBuf,
}

impl PersonaStage {
    pub fn new(profile_path: PathBuf) -> Self {
        Self { profile_path }
    }
}

impl ContextStage for PersonaStage {
    fn priority(&self) -> i32 {
        1 // right after system prompt(0), before SkillStage(2)
    }

    fn name(&self) -> &str {
        "persona"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let profile = load_profile(&self.profile_path)?;

        let mut parts = Vec::new();

        if !profile.system_prompt_injection.is_empty() {
            parts.push(format!(
                "## User Persona\n{}",
                profile.system_prompt_injection
            ));
        }

        parts.push(format!(
            "Communication: {verbosity} {formality}, {lang}\n\
             Thinking: {decomp}, {theory}\n\
             Code-first: {code_first}",
            verbosity = profile.communication_style.verbosity,
            formality = profile.communication_style.formality,
            lang = profile.communication_style.language,
            decomp = profile.thinking_paradigm.decomposition,
            theory = profile.thinking_paradigm.theory_vs_practice,
            code_first = if profile.communication_style.code_first {
                "yes (show code before explanation)"
            } else {
                "no (explain before code)"
            },
        ));

        let content = parts.join("\n\n");

        Some(ContextFragment {
            label: "Persona Profile".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn load_profile(path: &Path) -> Option<PersonaProfile> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persona_profile_default() {
        let p = PersonaProfile::default();
        assert_eq!(p.communication_style.language, "zh-CN");
        assert_eq!(p.communication_style.verbosity, "concise");
        assert_eq!(p.thinking_paradigm.decomposition, "top-down");
    }

    #[test]
    fn test_load_profile_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            r#"{
                "communication_style": {
                    "language": "en",
                    "verbosity": "detailed",
                    "formality": "formal",
                    "code_first": false
                },
                "thinking_paradigm": {
                    "decomposition": "bottom-up",
                    "theory_vs_practice": "theory"
                },
                "system_prompt_injection": "The user is a senior Rust engineer."
            }"#,
        )
        .unwrap();

        let profile = load_profile(&path).unwrap();
        assert_eq!(profile.communication_style.language, "en");
        assert_eq!(profile.communication_style.verbosity, "detailed");
        assert_eq!(profile.communication_style.formality, "formal");
        assert_eq!(profile.communication_style.code_first, false);
        assert_eq!(profile.thinking_paradigm.decomposition, "bottom-up");
        assert_eq!(profile.thinking_paradigm.theory_vs_practice, "theory");
        assert_eq!(
            profile.system_prompt_injection,
            "The user is a senior Rust engineer."
        );
    }

    #[test]
    fn test_load_profile_missing_file_returns_none() {
        assert!(load_profile(Path::new("nonexistent.json")).is_none());
    }

    #[test]
    fn test_persona_stage_builds_context_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            r#"{
                "communication_style": {
                    "language": "zh-CN",
                    "verbosity": "concise",
                    "formality": "casual",
                    "code_first": true
                },
                "thinking_paradigm": {
                    "decomposition": "top-down",
                    "theory_vs_practice": "practice"
                },
                "system_prompt_injection": "Custom prompt here."
            }"#,
        )
        .unwrap();

        let stage = PersonaStage::new(path);
        let ctx = ContextBuildContext::default();

        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.label, "Persona Profile");
        assert_eq!(fragment.messages.len(), 1);
        let content = &fragment.messages[0].content;
        assert!(content.contains("Custom prompt here."));
        assert!(content.contains("concise casual"));
        assert!(content.contains("zh-CN"));
        assert!(content.contains("top-down"));
        assert!(content.contains("practice"));
        assert!(content.contains("Code-first: yes"));
    }

    #[test]
    fn test_persona_stage_no_system_prompt_injection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            r#"{
                "communication_style": {
                    "language": "en",
                    "verbosity": "detailed",
                    "formality": "formal",
                    "code_first": false
                },
                "thinking_paradigm": {
                    "decomposition": "bottom-up",
                    "theory_vs_practice": "theory"
                },
                "system_prompt_injection": ""
            }"#,
        )
        .unwrap();

        let stage = PersonaStage::new(path);
        let ctx = ContextBuildContext::default();

        let fragment = stage.build(&ctx).unwrap();
        let content = &fragment.messages[0].content;
        // Should NOT contain "User Persona" since system_prompt_injection is empty
        assert!(!content.contains("User Persona"));
        assert!(content.contains("detailed formal"));
    }
}
