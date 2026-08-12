//! REM-phase theme domain — `Theme` extraction, parsing, and promotion helpers.
//!
//! Extracted verbatim from `engine.rs` during a pure structural split.

use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};

/// A theme extracted during the REM phase.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Theme {
    /// Short kebab-case identifier.
    pub name: String,
    /// One-sentence summary.
    pub description: String,
    /// Supporting quotes from diary entries.
    pub evidence: Vec<String>,
    /// LLM-assigned confidence [0.0, 1.0].
    pub confidence: f32,
}

/// Build a prompt for the LLM to extract themes from diary entries.
pub(crate) fn build_theme_extraction_prompt(recent: &[(String, String)]) -> String {
    let diary_text: String = recent
        .iter()
        .map(|(date, content)| format!("## {date}\n\n{content}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "You are a memory curator. Read the following diary entries and extract \
         recurring themes, facts, and patterns.\n\n\
         For each theme, provide:\n\
         - name: a short identifier (kebab-case, e.g. \"user-prefers-async\")\n\
         - description: one sentence summarizing the theme\n\
         - evidence: 1-3 supporting quotes from the diary\n\
         - confidence: 0.0 to 1.0 (how certain you are this is a real pattern)\n\n\
         Return ONLY a JSON array of theme objects. If nothing is found, return [].\n\
         Do not include any other text outside the JSON array.\n\n\
         === DIARY ENTRIES ===\n\n{diary_text}\n\n=== THEMES (JSON array) ==="
    )
}

/// Parse theme objects from an LLM response.
///
/// Attempts to find a JSON array in the response text, with fallback
/// to line-by-line JSON parsing. Returns an empty vec if nothing parses.
pub(crate) fn parse_themes_from_response(response: &str) -> Vec<Theme> {
    let trimmed = response.trim();

    // Find the first '[' and last ']' for a JSON array
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        let json_str = &trimmed[start..=end];
        if let Ok(themes) = serde_json::from_str::<Vec<Theme>>(json_str) {
            return themes;
        }
    }

    // Fallback: try parsing each non-empty line as a standalone theme object
    let mut themes = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line == "[" || line == "]" {
            continue;
        }
        // Strip trailing comma from JSON array entries
        let clean = line.strip_suffix(',').unwrap_or(line);
        if let Ok(theme) = serde_json::from_str::<Theme>(clean) {
            themes.push(theme);
        }
    }
    themes
}

/// Convert a Theme into a MemoryFact for the DEEP phase promotion.
pub(crate) fn theme_to_memory_fact(theme: &Theme) -> MemoryFact {
    MemoryFact {
        name: theme.name.clone(),
        description: theme.description.clone(),
        content: theme.evidence.join("; "),
        fact_type: FactType::Project,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        projection: ProjectionMetadata::new("dreaming-pipeline", "llm", vec![], theme.confidence),
        links: vec![],
        // DEEP-phase promoted themes are cross-session long-term memory.
        session: Some("global".into()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Theme parsing tests ───────────────────────────────────────────

    #[test]
    fn test_parse_themes_json_array() {
        let response = r#"[
            {"name": "rust-pref", "description": "User likes Rust", "evidence": ["uses Rust"], "confidence": 0.9},
            {"name": "async-pref", "description": "User prefers async", "evidence": ["async await"], "confidence": 0.8}
        ]"#;
        let themes = parse_themes_from_response(response);
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].name, "rust-pref");
        assert_eq!(themes[1].name, "async-pref");
    }

    #[test]
    fn test_parse_themes_empty() {
        assert!(parse_themes_from_response("[]").is_empty());
        assert!(parse_themes_from_response("No themes found.").is_empty());
    }

    #[test]
    fn test_parse_themes_line_by_line_fallback() {
        let response = "{\"name\":\"a\",\"description\":\"d\",\"evidence\":[],\"confidence\":0.5}\n{\"name\":\"b\",\"description\":\"d\",\"evidence\":[],\"confidence\":0.7}";
        let themes = parse_themes_from_response(response);
        assert_eq!(themes.len(), 2);
    }

    #[test]
    fn test_theme_to_memory_fact() {
        let theme = Theme {
            name: "test-theme".into(),
            description: "A test theme".into(),
            evidence: vec!["evidence 1".into(), "evidence 2".into()],
            confidence: 0.85,
        };
        let fact = theme_to_memory_fact(&theme);
        assert_eq!(fact.name, "test-theme");
        assert_eq!(fact.fact_type, FactType::Project);
        assert!(fact.content.contains("evidence 1"));
        assert_eq!(fact.projection.confidence, 0.85);
    }

    #[test]
    fn test_build_theme_extraction_prompt() {
        let recent = vec![
            (
                "2026-07-19".into(),
                "User discussed async Rust patterns.".into(),
            ),
            (
                "2026-07-18".into(),
                "User prefers small crates over monoliths.".into(),
            ),
        ];
        let prompt = build_theme_extraction_prompt(&recent);
        assert!(prompt.contains("2026-07-19"));
        assert!(prompt.contains("async Rust"));
        assert!(prompt.contains("THEMES"));
    }
}
