//! DEEP-phase knowledge-graph write helper.
//!
//! Extracted verbatim from `engine.rs` during a pure structural split.

use everevo_knowledge::graph::{Entity, EntityType, KnowledgeGraph, Relation, RelationStatus};

/// Parse LLM-extracted entities/relations JSON and write them into the graph.
pub(crate) fn extract_and_write_to_kg(json_text: &str, _source: &str, kg: &mut KnowledgeGraph) {
    let cleaned = json_text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(cleaned) {
        if let Some(entities) = parsed.get("entities").and_then(|e| e.as_array()) {
            for e in entities {
                let id = e["id"].as_str().unwrap_or("unknown");
                let label = e["label"].as_str().unwrap_or(id);
                let etype = match e["type"].as_str().unwrap_or("Concept") {
                    "Person" => EntityType::Person,
                    "Project" => EntityType::Project,
                    "Tool" => EntityType::Tool,
                    "File" => EntityType::File,
                    "Event" => EntityType::Event,
                    _ => EntityType::Concept,
                };
                kg.upsert_entity(Entity {
                    id: id.to_string(),
                    label: label.to_string(),
                    entity_type: etype,
                    properties: std::collections::HashMap::new(),
                    sources: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    merged_into: None,
                });
            }
        }
        if let Some(relations) = parsed.get("relations").and_then(|r| r.as_array()) {
            for r in relations {
                kg.add_relation(Relation {
                    from: r["from"].as_str().unwrap_or("").into(),
                    predicate: r["predicate"].as_str().unwrap_or("related_to").into(),
                    to: r["to"].as_str().unwrap_or("").into(),
                    status: RelationStatus::Active,
                    valid_from: chrono::Utc::now(),
                    valid_until: None,
                    sources: vec![],
                });
            }
        }
        let _ = kg.save();
    }
}
