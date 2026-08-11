//! Chunk Extraction Pipeline — LLM entity/relation extraction + dual write.
//!
//! ## Flow
//!
//! ```text
//! candidate facts (from DEEP phase)
//!   → ChunkExtractor: LLM extracts entities + relations
//!   → EntityResolver: deduplicate entities
//!   → DualWrite: Vector Store + Knowledge Graph
//!   → WikiGenerator: update wiki/*.md
//! ```

use everevo_core::memory::{MemoryFact, SourcePointer};
use everevo_core::EverEvoError;

use super::consolidator::{ConsolidationAction, MemoryConsolidator};
use super::facts::FactManager;

// ── Chunk Extraction Result ───────────────────────────────────────────────

/// Output of the chunk extraction phase — ready for dual write.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    /// The original fact.
    pub fact: MemoryFact,
    /// Extracted entities with their types.
    pub entities: Vec<ExtractedEntity>,
    /// Extracted relationships between entities.
    pub relations: Vec<ExtractedRelation>,
    /// The consolidation action taken.
    pub action: ConsolidationAction,
    /// Source pointers to the original fact.
    pub source_pointers: Vec<SourcePointer>,
}

#[derive(Debug, Clone)]
pub struct ExtractedEntity {
    pub id: String,
    pub label: String,
    pub entity_type: String,
    pub properties: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ExtractedRelation {
    pub from: String,
    pub predicate: String,
    pub to: String,
}

// ── Chunk Extractor ───────────────────────────────────────────────────────

/// Extracts entities and relations from memory facts.
///
/// Phase 2c full implementation: calls LLM via `build_extraction_prompt()`.
/// Current MVP: simple keyword-based extraction.
pub struct ChunkExtractor;

impl ChunkExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Extract entities and relations from a memory fact.
    /// Phase 2c: replace with LLM call using knowledge::graph::build_extraction_prompt().
    pub fn extract(&self, fact: &MemoryFact) -> ExtractionResult {
        // Simple keyword-based extraction as MVP
        let entities = extract_entities_basic(fact);
        let relations = extract_relations_basic(fact, &entities);

        ExtractionResult {
            fact: fact.clone(),
            entities,
            relations,
            action: ConsolidationAction::Add,
            source_pointers: fact.projection.source_pointers.clone(),
        }
    }

    /// Run the full consolidation + extraction pipeline for a batch of facts.
    pub fn process_batch(
        &self,
        candidates: &[MemoryFact],
        existing: &[MemoryFact],
    ) -> Vec<ExtractionResult> {
        let consolidator = MemoryConsolidator::default();
        let mut results = Vec::new();

        for candidate in candidates {
            let action = consolidator.consolidate(candidate, existing);
            let mut result = self.extract(candidate);
            result.action = action;
            results.push(result);
        }

        results
    }

    /// Apply results with dual write to KG.
    pub async fn apply_with_kg(
        results: &[ExtractionResult],
        fact_manager: &FactManager,
        kg: Option<&mut everevo_knowledge::graph::KnowledgeGraph>,
    ) -> Result<ApplyStats, EverEvoError> {
        let stats = Self::apply(results, fact_manager).await?;

        // Write extracted entities and relations to KG
        if let Some(kg) = kg {
            for result in results {
                for entity in &result.entities {
                    use everevo_knowledge::graph::{Entity, EntityType};
                    let e = Entity {
                        id: entity.id.clone(),
                        label: entity.label.clone(),
                        entity_type: EntityType::Concept,
                        properties: std::collections::HashMap::new(),
                        sources: vec![],
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                        merged_into: None,
                    };
                    kg.upsert_entity(e);
                }
                for rel in &result.relations {
                    use everevo_knowledge::graph::{Relation, RelationStatus};
                    let r = Relation {
                        from: rel.from.clone(),
                        predicate: rel.predicate.clone(),
                        to: rel.to.clone(),
                        status: RelationStatus::Active,
                        valid_from: chrono::Utc::now(),
                        valid_until: None,
                        sources: vec![],
                    };
                    kg.add_relation(r);
                }
            }
            if let Err(e) = kg.save() {
                tracing::warn!(error = %e, "KG save failed during apply_with_kg");
            }
        }

        Ok(stats)
    }

    /// Apply extraction results: write facts and feed to vector/graph stores.
    pub async fn apply(
        results: &[ExtractionResult],
        fact_manager: &FactManager,
    ) -> Result<ApplyStats, EverEvoError> {
        let mut stats = ApplyStats::default();

        for result in results {
            match &result.action {
                ConsolidationAction::Add => {
                    fact_manager.save_async(result.fact.clone()).await?;
                    stats.added += 1;
                }
                ConsolidationAction::Update { existing_name, .. } => {
                    fact_manager.save_async(result.fact.clone()).await?;
                    // Delete old fact if name changed
                    if result.fact.name != *existing_name {
                        fact_manager.delete(existing_name)?;
                    }
                    stats.updated += 1;
                }
                ConsolidationAction::Delete { existing_name, .. } => {
                    fact_manager.delete(existing_name)?;
                    stats.deleted += 1;
                }
                ConsolidationAction::Noop { .. } => {
                    stats.skipped += 1;
                }
            }
        }

        Ok(stats)
    }
}

impl Default for ChunkExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct ApplyStats {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub skipped: usize,
}

// ── Basic Entity Extraction (keyword-based MVP) ───────────────────────────

fn extract_entities_basic(fact: &MemoryFact) -> Vec<ExtractedEntity> {
    let mut entities = Vec::new();

    // Extract capitalized proper nouns as potential entities
    let words: Vec<&str> = fact.content.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].trim_matches(|c: char| !c.is_alphanumeric());
        if word.len() > 2 && word.chars().next().is_some_and(|c| c.is_uppercase()) {
            let label = if i + 1 < words.len()
                && words[i + 1]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase())
            {
                // Multi-word proper noun
                let multi = format!(
                    "{} {}",
                    word,
                    words[i + 1].trim_matches(|c: char| !c.is_alphanumeric())
                );
                i += 1;
                multi
            } else {
                word.to_string()
            };

            let id = label.to_lowercase().replace(' ', "-");
            let entity_type = classify_entity(&label);

            if !entities.iter().any(|e: &ExtractedEntity| e.id == id) {
                entities.push(ExtractedEntity {
                    id,
                    label,
                    entity_type,
                    properties: vec![],
                });
            }
        }
        i += 1;
    }

    entities
}

fn classify_entity(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.contains("rust") || lower.contains("python") || lower.contains("sql") {
        "Tool".into()
    } else if lower.contains("project") || lower.contains("app") || lower.contains("crate") {
        "Project".into()
    } else if lower.contains(".rs") || lower.contains(".md") || lower.contains(".toml") {
        "File".into()
    } else {
        "Concept".into()
    }
}

fn extract_relations_basic(
    fact: &MemoryFact,
    entities: &[ExtractedEntity],
) -> Vec<ExtractedRelation> {
    let mut relations = Vec::new();

    // Look for common relation patterns in the content
    for (i, e1) in entities.iter().enumerate() {
        for e2 in entities.iter().skip(i + 1) {
            // Check if both entities appear in the same sentence with a connector
            let content_lower = fact.content.to_lowercase();
            if content_lower.contains(&e1.label.to_lowercase())
                && content_lower.contains(&e2.label.to_lowercase())
            {
                let predicate = infer_predicate(&fact.content, &e1.label, &e2.label);
                relations.push(ExtractedRelation {
                    from: e1.id.clone(),
                    predicate,
                    to: e2.id.clone(),
                });
            }
        }
    }

    relations
}

fn infer_predicate(content: &str, entity1: &str, entity2: &str) -> String {
    let lower = content.to_lowercase();
    let e1_lower = entity1.to_lowercase();
    let e2_lower = entity2.to_lowercase();

    // Simple pattern matching for common relations
    let patterns = [
        ("uses", "uses"),
        ("built with", "built_with"),
        ("depends on", "depends_on"),
        ("works on", "works_on"),
        ("created", "created"),
        ("contains", "contains"),
        ("references", "references"),
        ("prefers", "prefers"),
    ];

    for (pattern, rel) in &patterns {
        if lower.contains(pattern) {
            // Check which entity comes first
            if let (Some(pos1), Some(pos2)) = (lower.find(&e1_lower), lower.find(&e2_lower)) {
                if pos1 < pos2 {
                    return rel.to_string();
                } else {
                    return format!("inverse_{rel}");
                }
            }
        }
    }

    "related_to".into()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::{FactType, ProjectionMetadata};

    fn make_fact(name: &str, content: &str) -> MemoryFact {
        MemoryFact {
            name: name.into(),
            description: "test".into(),
            content: content.into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
            session: None,
        }
    }

    #[test]
    fn test_extract_entities() {
        let fact = make_fact(
            "test",
            "EverEvo uses Rust for the backend and Python for scripting",
        );
        let result = ChunkExtractor::new().extract(&fact);
        assert!(!result.entities.is_empty());
        let names: Vec<&str> = result.entities.iter().map(|e| e.label.as_str()).collect();
        // Should find "EverEvo" and "Rust"
        assert!(names.contains(&"EverEvo"));
    }

    #[test]
    fn test_extract_relations() {
        let fact = make_fact("test", "EverEvo uses Rust for sandbox isolation");
        let result = ChunkExtractor::new().extract(&fact);
        assert!(!result.relations.is_empty());
    }

    #[test]
    fn test_process_batch_add() {
        let cand = make_fact("new-fact", "Rust is used for memory management");
        let existing = vec![make_fact("old-fact", "Python is used for scripting")];
        let results = ChunkExtractor::new().process_batch(&[cand], &existing);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, ConsolidationAction::Add);
    }
}
