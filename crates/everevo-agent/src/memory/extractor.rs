//! Turn-level memory extractor — auto-captures facts after each agent turn.
//!
//! ## Design (Mem0 extraction phase pattern)
//!
//! After each (user_msg, assistant_msg) pair, a lightweight LLM call
//! extracts candidate facts. These are saved via FactManager which
//! now includes real-time Jaccard dedup.
//!
//! This runs asynchronously — non-blocking, fire-and-forget. Failures
//! are logged but never surface to the user.

use everevo_core::llm::{LlmMessage, LlmProvider};

use crate::llm::HttpClient;
use crate::memory::facts::FactManager;

/// Attempt to extract and save facts from a conversation turn.
///
/// Called after the assistant response is persisted. Runs as a background
/// tokio task — failures are logged, never returned to the user.
pub async fn extract_from_turn(
    llm: &HttpClient,
    fact_manager: &FactManager,
    user_msg: &str,
    assistant_msg: &str,
) {
    if user_msg.len() < 20 || assistant_msg.len() < 20 {
        return; // too short to contain meaningful facts
    }

    let prompt = build_memory_extraction_prompt(user_msg, assistant_msg);
    let messages = vec![LlmMessage::user(&prompt)];

    match llm.chat(&messages, &[]).await {
        Ok(response) => {
            let text = response.content.unwrap_or_default();
            if text.is_empty() || text.contains("[NO_FACTS]") {
                return;
            }

            // Parse JSON array of candidate facts
            let facts = parse_extracted_facts(&text);
            if facts.is_empty() {
                return;
            }

            let mut saved = 0u32;
            for (name, description, content) in &facts {
                let fact = everevo_core::memory::MemoryFact {
                    name: name.clone(),
                    description: description.clone(),
                    content: content.clone(),
                    fact_type: everevo_core::memory::FactType::Project,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    projection: everevo_core::memory::ProjectionMetadata::new(
                        "turn-extractor",
                        "llm",
                        vec![],
                        0.7,
                    ),
                    links: vec![],
                };
                match fact_manager.save(&fact) {
                    Ok(()) => saved += 1,
                    Err(e) => {
                        tracing::debug!(name = %fact.name, error = %e, "Extracted fact dedup'd or rejected")
                    }
                }
            }
            if saved > 0 {
                tracing::info!(saved, "Turn-level memory extraction saved facts");
            }
        }
        Err(e) => tracing::debug!(error = %e, "Memory extraction LLM call failed"),
    }
}

/// Build the extraction prompt for turn-level memory capture.
fn build_memory_extraction_prompt(user_msg: &str, assistant_msg: &str) -> String {
    format!(
        "Extract durable facts from this conversation turn. Return ONLY a JSON array.\n\
         Each fact: {{\"name\": \"kebab-case-slug\", \"description\": \"one sentence\", \
         \"content\": \"detailed fact\"}}\n\n\
         Rules:\n\
         - Only extract facts likely useful in future sessions\n\
         - Skip greetings, confirmations, tool noise\n\
         - Skip transient/temporary information\n\
         - If nothing substantive, return []\n\n\
         User: {user_msg}\n\nAssistant: {assistant_msg}\n\nJSON:"
    )
}

/// Parse extracted facts from LLM response JSON.
pub(crate) fn parse_extracted_facts(text: &str) -> Vec<(String, String, String)> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let parsed: Vec<serde_json::Value> = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            // Fallback: try to find a JSON array anywhere in the text
            if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
                match serde_json::from_str(&trimmed[start..=end]) {
                    Ok(v) => v,
                    Err(_) => return vec![],
                }
            } else {
                return vec![];
            }
        }
    };

    parsed
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let desc = item.get("description")?.as_str()?.to_string();
            let content = item.get("content")?.as_str()?.to_string();
            if name.is_empty() || desc.is_empty() {
                return None;
            }
            Some((name, desc, content))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_response() {
        assert!(parse_extracted_facts("[]").is_empty());
    }

    #[test]
    fn test_parse_single_fact() {
        let json = r#"[{"name": "user-prefers-rust", "description": "User likes Rust", "content": "User prefers Rust for backend development"}]"#;
        let facts = parse_extracted_facts(json);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].0, "user-prefers-rust");
    }

    #[test]
    fn test_extraction_prompt_contains_content() {
        let prompt = build_memory_extraction_prompt("Hello", "Hi there");
        assert!(prompt.contains("Hello"));
        assert!(prompt.contains("Hi there"));
    }
}
