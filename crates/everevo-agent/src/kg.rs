//! Knowledge graph — wraps everevo-kg for entity/relation storage and SPARQL.
//!
//! Provides a high-level agent API on top of the Oxigraph-backed graph store.

use std::path::Path;

use everevo_core::EverEvoError;

// Re-export the full public API from everevo-kg
pub use everevo_kg::{
    build_extraction_prompt, Entity, EntityResolver, EntityType, KnowledgeGraph, Relation,
    RelationStatus, ResolveStats, Triple,
};

/// Agent-facing knowledge graph wrapper with convenience methods.
pub struct AgentKnowledgeGraph {
    inner: KnowledgeGraph,
}

impl AgentKnowledgeGraph {
    /// Open or create a knowledge graph at the given directory.
    pub fn open(data_dir: &Path) -> Result<Self, EverEvoError> {
        let graph_dir = data_dir.join("memory").join("graph");
        let kg = KnowledgeGraph::open(graph_dir)?;
        Ok(Self { inner: kg })
    }

    /// Access the underlying [`KnowledgeGraph`].
    pub fn inner(&self) -> &KnowledgeGraph {
        &self.inner
    }

    /// Mutable access for operations like upsert/merge.
    pub fn inner_mut(&mut self) -> &mut KnowledgeGraph {
        &mut self.inner
    }

    /// Upsert an entity and persist to Turtle.
    pub fn upsert(&mut self, entity: Entity) -> Result<bool, EverEvoError> {
        let is_new = self.inner.upsert_entity(entity);
        self.inner.save()?;
        Ok(is_new)
    }

    /// Add a relation and persist.
    pub fn add_relation(&mut self, relation: Relation) -> Result<(), EverEvoError> {
        self.inner.add_relation(relation);
        self.inner.save()
    }

    /// Run entity resolution and persist any merges.
    pub fn resolve_and_save(&mut self) -> Result<ResolveStats, EverEvoError> {
        let stats = self.inner.resolve_all();
        if stats.entities_merged > 0 {
            self.inner.save()?;
        }
        Ok(stats)
    }

    /// Save current state to Turtle.
    pub fn save(&self) -> Result<(), EverEvoError> {
        self.inner.save()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_agent_kg_open_and_upsert() {
        let dir = TempDir::new().unwrap();
        let mut kg = AgentKnowledgeGraph::open(dir.path()).unwrap();

        let entity = Entity {
            id: "test-agent".into(),
            label: "Test Agent Entity".into(),
            entity_type: EntityType::Tool,
            properties: std::collections::HashMap::new(),
            sources: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            merged_into: None,
        };

        let is_new = kg.upsert(entity).unwrap();
        assert!(is_new);

        // Roundtrip: reload and verify
        let kg2 = AgentKnowledgeGraph::open(dir.path()).unwrap();
        let found = kg2.inner().get_entity("test-agent");
        assert!(found.is_some());
        assert_eq!(found.unwrap().label, "Test Agent Entity");
    }

    #[test]
    fn test_extraction_prompt_contains_keywords() {
        let prompt = build_extraction_prompt("Alice works on EverEvo");
        assert!(prompt.contains("entities"));
        assert!(prompt.contains("relations"));
        assert!(prompt.contains("Alice"));
    }
}
