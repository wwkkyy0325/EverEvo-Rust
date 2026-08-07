//! In-process Persona ContextStage for the context pipeline.
//!
//! Complemented by MCP stage plugin `plugin-stage-persona` which exposes MCP tools.
//! This in-process version implements ContextStage for automatic persona injection
//! into the LLM context — a capability MCP plugins cannot provide.
//! This in-process implementation is kept for backward compatibility.
//! New development should use the MCP plugin version.

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

        // ── Language directive (bilingual thinking/output separation) ──
        parts.push(language_directive(&profile));

        // ── Communication + thinking style ──
        parts.push(format!(
            "## Communication Style\n\
             Verbosity: {verbosity}\n\
             Formality: {formality}\n\
             Code-first: {code_first}",
            verbosity = profile.communication_style.verbosity,
            formality = profile.communication_style.formality,
            code_first = if profile.communication_style.code_first {
                "yes (show code before explanation)"
            } else {
                "no (explain before code)"
            },
        ));

        parts.push(format!(
            "## Thinking Paradigm\n\
             Decomposition: {decomp}\n\
             Theory vs Practice: {theory}",
            decomp = profile.thinking_paradigm.decomposition,
            theory = profile.thinking_paradigm.theory_vs_practice,
        ));

        let content = parts.join("\n\n");

        Some(ContextFragment {
            label: "Persona Profile".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}

// ── Language Directive ────────────────────────────────────────────────────

/// Generate a language behavior directive based on the profile's language.
///
/// When the user language is Chinese ("zh-CN"), this injects the bilingual
/// pattern: reasoning and code in English, conversation in Chinese. This is
/// the same approach Claude Code uses — pure system prompt engineering,
/// not an API feature.
///
/// For English-only profiles, the directive is a simple all-English instruction.
fn language_directive(profile: &PersonaProfile) -> String {
    match profile.communication_style.language.as_str() {
        "zh-CN" => "\
## Language\n\n\
- **Reasoning & thinking:** think through problems in English. Internal reasoning, \
code analysis, and planning happen in English even when the final reply is in Chinese.\n\
- **Code & artifacts:** identifiers, comments, commit messages, and all written docs \
(`design.md`, `changelog.md`, task docs) are in English.\n\
- **Conversation:** replies and Q&A are in Chinese by default. Follow the user's \
language if they switch mid-conversation.\n\
- **Markers:** keep status markers, headings, and structural labels in English \
regardless of reply language."
            .to_string(),

        _ => "\
## Language\n\n\
- All communication, code, and documentation in English."
            .to_string(),
    }
}

// ── Persona Auto-Update ───────────────────────────────────────────────────

/// Update the persona profile based on accumulated feedback facts and
/// conversation patterns. Called periodically by the dreaming scheduler
/// after DEEP phase (alongside wiki generation).
///
/// Currently updates:
/// - `verbosity` based on average message length preference
/// - `code_first` based on tool usage patterns
/// - `system_prompt_injection` from high-confidence feedback facts
pub fn update_persona_from_facts(
    profile_path: &Path,
    facts: &[everevo_core::memory::MemoryFact],
) {
    let mut profile = load_profile(profile_path).unwrap_or_default();
    let mut changed = false;

    // Collect high-confidence feedback facts
    let feedback: Vec<_> = facts
        .iter()
        .filter(|f| f.fact_type == everevo_core::memory::FactType::Feedback)
        .collect();

    if !feedback.is_empty() {
        // Build a persona injection from the top-3 feedback facts
        let top: Vec<_> = feedback
            .iter()
            .filter(|f| f.projection.confidence > 0.6)
            .take(3)
            .map(|f| format!("- {}", f.content))
            .collect();

        if !top.is_empty() {
            let injection = format!(
                "## Learned Preferences (auto-updated from interactions)\n\n{}",
                top.join("\n")
            );
            if profile.system_prompt_injection != injection {
                profile.system_prompt_injection = injection;
                changed = true;
            }
        }
    }

    // Infer code-first preference from fact patterns
    let code_facts = facts
        .iter()
        .filter(|f| {
            f.fact_type == everevo_core::memory::FactType::Project
                && (f.content.contains("fn ") || f.content.contains("struct ") || f.content.contains("import "))
        })
        .count();
    let text_facts = facts
        .iter()
        .filter(|f| {
            f.fact_type == everevo_core::memory::FactType::Project
                && !f.content.contains("fn ")
                && !f.content.contains("struct ")
        })
        .count();

    if code_facts + text_facts > 5 {
        let prefers_code = code_facts > text_facts;
        if profile.communication_style.code_first != prefers_code {
            profile.communication_style.code_first = prefers_code;
            changed = true;
        }
    }

    if changed {
        if let Some(parent) = profile_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&profile) {
            if let Err(e) = std::fs::write(profile_path, &json) {
                tracing::warn!(error = %e, "Failed to write updated persona profile");
            } else {
                tracing::info!(
                    code_first = profile.communication_style.code_first,
                    feedback_count = feedback.len(),
                    "Persona profile auto-updated"
                );
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn load_profile(path: &Path) -> Option<PersonaProfile> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).ok(),
        Err(_) => {
            // Auto-create default profile so Persona stage is never silent-missing.
            let default = PersonaProfile::default();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&default) {
                let _ = std::fs::write(path, &json);
                tracing::info!(
                    path = %path.display(),
                    "Created default persona profile"
                );
            }
            Some(default)
        }
    }
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
    fn test_language_directive_zh_cn() {
        let profile = PersonaProfile::default();
        let directive = language_directive(&profile);
        assert!(directive.contains("Reasoning & thinking"));
        assert!(directive.contains("think through problems in English"));
        assert!(directive.contains("replies and Q&A are in Chinese"));
        assert!(directive.contains("identifiers, comments, commit messages"));
    }

    #[test]
    fn test_language_directive_en() {
        let mut profile = PersonaProfile::default();
        profile.communication_style.language = "en".into();
        let directive = language_directive(&profile);
        assert!(directive.contains("All communication, code, and documentation in English"));
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
    fn test_load_profile_missing_file_creates_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        // File doesn't exist — should auto-create and return defaults.
        let profile = load_profile(&path).unwrap();
        assert_eq!(profile.communication_style.language, "zh-CN");
        assert!(path.exists()); // File was created on disk
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

        // System prompt injection
        assert!(content.contains("Custom prompt here."));

        // Language directive (zh-CN → bilingual thinking/output)
        assert!(content.contains("## Language"));
        assert!(content.contains("Reasoning & thinking"));
        assert!(content.contains("think through problems in English"));

        // Communication style (new structured format)
        assert!(content.contains("## Communication Style"));
        assert!(content.contains("Verbosity: concise"));
        assert!(content.contains("Formality: casual"));
        assert!(content.contains("Code-first: yes"));

        // Thinking paradigm
        assert!(content.contains("## Thinking Paradigm"));
        assert!(content.contains("Decomposition: top-down"));
        assert!(content.contains("Theory vs Practice: practice"));
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

        // Language directive (en → all-English)
        assert!(content.contains("All communication, code, and documentation in English"));

        // Communication style
        assert!(content.contains("## Communication Style"));
        assert!(content.contains("Verbosity: detailed"));
        assert!(content.contains("Formality: formal"));
        assert!(content.contains("Code-first: no"));

        // Thinking paradigm
        assert!(content.contains("Decomposition: bottom-up"));
    }
}
