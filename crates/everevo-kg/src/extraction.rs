//! LLM entity/relation extraction prompt builder.

/// Build the extraction prompt for LLM-based entity/relation extraction.
pub fn build_extraction_prompt(content: &str) -> String {
    format!(
        "Extract entities and relations from the following text. Return ONLY a JSON object with two keys:\n\
         - \"entities\": array of {{\"id\": \"kebab-case-slug\", \"label\": \"Human name\", \"type\": \"Person|Project|Tool|Concept|File|Event\"}}\n\
         - \"relations\": array of {{\"from\": \"entity-id\", \"predicate\": \"VERB_IN_LOWERCASE\", \"to\": \"entity-id\"}}\n\n\
         Rules:\n\
         - Entity IDs must be kebab-case\n\
         - Only extract entities that seem durable\n\
         - Relations use simple present-tense verbs\n\
         - Do NOT extract pronouns as entities\n\n\
         Text:\n{content}\n\nJSON:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract() {
        let prompt = build_extraction_prompt("Alice works on EverEvo");
        assert!(prompt.contains("entities"));
    }
}
