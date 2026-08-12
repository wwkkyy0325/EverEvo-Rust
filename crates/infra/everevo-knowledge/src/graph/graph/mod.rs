//! Knowledge graph — HashMap primary storage + Oxigraph SPARQL layer.

use std::collections::HashMap;
use std::path::PathBuf;

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::store::Store;

use everevo_core::EverEvoError;

use super::types::{Entity, Relation};

mod persist;
mod storage;

// ── Knowledge Graph ─────────────────────────────────────────────────────────

/// Knowledge graph — HashMap primary storage + Oxigraph SPARQL layer.
pub struct KnowledgeGraph {
    /// Primary entity storage (fast HashMap CRUD).
    entities: HashMap<String, Entity>,
    /// Primary relation storage (fast Vec operations).
    relations: Vec<Relation>,
    /// Oxigraph store for SPARQL queries and Turtle persistence.
    store: Store,
    /// Path for file persistence.
    store_path: PathBuf,
}

// ── Public API ──────────────────────────────────────────────────────────────

impl KnowledgeGraph {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let store_path: PathBuf = dir.into();
        std::fs::create_dir_all(&store_path)
            .map_err(|e| EverEvoError::Internal(format!("Create kg dir: {e}")))?;
        let store =
            Store::new().map_err(|e| EverEvoError::KnowledgeGraph(format!("Create store: {e}")))?;
        let mut kg = Self {
            entities: HashMap::new(),
            relations: Vec::new(),
            store,
            store_path,
        };
        // Load persisted Turtle into the Oxigraph store, then sync to HashMap
        let turtle_path = kg.store_path.join("knowledge.ttl");
        if turtle_path.exists() {
            let file = std::fs::File::open(&turtle_path).map_err(EverEvoError::Io)?;
            let reader = std::io::BufReader::new(file);
            kg.store
                .load_from_reader(RdfParser::from_format(RdfFormat::Turtle), reader)
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Load Turtle: {e}")))?;
        }
        kg.sync_from_store();
        Ok(kg)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{EntityType, RelationStatus};
    use tempfile::TempDir;

    fn e(id: &str, label: &str, t: EntityType) -> Entity {
        Entity {
            id: id.into(),
            label: label.into(),
            entity_type: t,
            properties: HashMap::new(),
            sources: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            merged_into: None,
        }
    }

    #[test]
    fn test_crud() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("x", "X", EntityType::Project));
        assert_eq!(kg.entity_count(), 1);
        assert_eq!(kg.search("x").len(), 1);
        assert!(kg.get_entity("x").is_some());
    }

    #[test]
    fn test_relation() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let n = chrono::Utc::now();
        kg.add_relation(Relation {
            from: "a".into(),
            predicate: "lives_in".into(),
            to: "sf".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        kg.add_relation(Relation {
            from: "a".into(),
            predicate: "lives_in".into(),
            to: "ny".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        assert_eq!(kg.relation_count(), 1);
    }

    #[test]
    fn test_expand() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("a", "A", EntityType::Person));
        kg.upsert_entity(e("b", "B", EntityType::Project));
        kg.upsert_entity(e("c", "C", EntityType::Tool));
        let n = chrono::Utc::now();
        kg.add_relation(Relation {
            from: "a".into(),
            predicate: "w".into(),
            to: "b".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        kg.add_relation(Relation {
            from: "b".into(),
            predicate: "u".into(),
            to: "c".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        assert_eq!(kg.expand("a", 2).len(), 3);
    }

    // ── Enhanced tests ────────────────────────────────────────────────────

    #[test]
    fn test_expand_depth_zero() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("root", "Root", EntityType::Concept));
        kg.upsert_entity(e("child", "Child", EntityType::Concept));
        let n = chrono::Utc::now();
        kg.add_relation(Relation {
            from: "root".into(),
            predicate: "has".into(),
            to: "child".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        // depth=0 should only return the starting node
        let results = kg.expand("root", 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "root");
    }

    #[test]
    fn test_expand_nonexistent_start() {
        let d = TempDir::new().unwrap();
        let kg = KnowledgeGraph::open(d.path()).unwrap();
        let results = kg.expand("nonexistent", 2);
        assert!(results.is_empty());
    }

    #[test]
    fn test_expand_cycle() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("a", "A", EntityType::Concept));
        kg.upsert_entity(e("b", "B", EntityType::Concept));
        let n = chrono::Utc::now();
        // Create a cycle: a → b → a
        kg.add_relation(Relation {
            from: "a".into(),
            predicate: "links_to".into(),
            to: "b".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        kg.add_relation(Relation {
            from: "b".into(),
            predicate: "links_to".into(),
            to: "a".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        // Should terminate, not infinite loop
        let results = kg.expand("a", 5);
        assert_eq!(results.len(), 2); // a and b, no duplicates
    }

    #[test]
    fn test_merge_entity_properties() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let mut target = e("target", "Target", EntityType::Concept);
        target.properties.insert("key1".into(), "val1".into());
        let mut source = e("source", "Source", EntityType::Concept);
        source
            .properties
            .insert("key1".into(), "val1_original".into()); // should NOT overwrite
        source.properties.insert("key2".into(), "val2".into());

        kg.upsert_entity(target);
        kg.upsert_entity(source);
        kg.merge_entities("target", "source");

        let merged = kg.get_entity("target").unwrap();
        assert_eq!(merged.properties.get("key1").unwrap(), "val1"); // original preserved
        assert_eq!(merged.properties.get("key2").unwrap(), "val2"); // new key added
        assert!(kg.get_entity("source").unwrap().merged_into.is_some());
    }

    #[test]
    fn test_merge_entity_sources_dedup() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let msg_id = uuid::Uuid::new_v4();
        let sess_id = uuid::Uuid::new_v4();

        let mut target = e("target", "T", EntityType::Concept);
        target
            .sources
            .push(everevo_core::memory::SourcePointer::new(
                sess_id, msg_id, "shared",
            ));
        let mut source = e("source", "S", EntityType::Concept);
        source
            .sources
            .push(everevo_core::memory::SourcePointer::new(
                sess_id, msg_id, "shared", // same message_id
            ));

        kg.upsert_entity(target);
        kg.upsert_entity(source);
        kg.merge_entities("target", "source");

        let merged = kg.get_entity("target").unwrap();
        // Same message_id should be deduplicated
        assert_eq!(merged.sources.len(), 1);
    }

    #[test]
    fn test_merge_entity_self() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("x", "X", EntityType::Concept));
        kg.merge_entities("x", "x"); // should be no-op
        let entity = kg.get_entity("x").unwrap();
        assert!(entity.merged_into.is_none());
    }

    #[test]
    fn test_sparql_select() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("test1", "Test One", EntityType::Project));
        kg.upsert_entity(e("test2", "Test Two", EntityType::Tool));
        kg.save().unwrap();

        let rows = kg
            .query_sparql(
                "PREFIX evo: <http://everevo.io/> \
             SELECT ?label WHERE { ?e evo:label ?label } ORDER BY ?label",
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_sparql_ask() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("ask_test", "Ask Entity", EntityType::Concept));
        kg.save().unwrap();

        // ASK returns boolean result — no rows but succeeds
        let result = kg.query_sparql(
            "PREFIX evo: <http://everevo.io/> \
             ASK { ?e evo:label \"Ask Entity\" }",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_save_writes_turtle_file() {
        // Verifies that save() produces a valid Turtle file containing entity data.
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("persist", "Persist Me", EntityType::Project));
        kg.save().unwrap();

        let ttl_path = d.path().join("knowledge.ttl");
        assert!(ttl_path.exists(), "Turtle file should exist after save");
        let content = std::fs::read_to_string(&ttl_path).unwrap();
        assert!(
            content.contains("Persist Me"),
            "Turtle should contain entity label"
        );
        assert!(
            content.contains("http://everevo.io/"),
            "Turtle should use everevo namespace"
        );
    }

    #[test]
    fn test_persistence_roundtrip() {
        let d = TempDir::new().unwrap();
        let path = d.path();

        // Create, populate, save
        {
            let mut kg = KnowledgeGraph::open(path).unwrap();
            kg.upsert_entity(e("persist", "Persist Me", EntityType::Project));
            kg.save().unwrap();
            // Verify it's in the HashMap before close
            assert!(
                kg.get_entity("persist").is_some(),
                "Entity should be in memory before close"
            );
        }

        // Reopen and verify data survived
        {
            let kg = KnowledgeGraph::open(path).unwrap();
            let count = kg.entity_count();
            let entity = kg.get_entity("persist");
            assert!(
                entity.is_some(),
                "Entity should survive roundtrip. entity_count={count}"
            );
            assert_eq!(entity.unwrap().label, "Persist Me");
        }
    }

    #[test]
    fn test_seed_idempotent() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.seed_project_structure(&["crate-a", "crate-b"]);
        let entity_count_1 = kg.entity_count();
        let relation_count_1 = kg.relation_count();
        assert!(entity_count_1 > 0);

        // Second seed should be a no-op
        kg.seed_project_structure(&["crate-a", "crate-b"]);
        assert_eq!(kg.entity_count(), entity_count_1);
        assert_eq!(kg.relation_count(), relation_count_1);
    }

    #[test]
    fn test_relation_many_no_duplicate() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let n = chrono::Utc::now();
        let rel = Relation {
            from: "a".into(),
            predicate: "depends_on".into(),
            to: "b".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        };
        kg.add_relation_many(rel.clone());
        kg.add_relation_many(rel.clone());
        // Exact same relation should only appear once
        assert_eq!(kg.relation_count(), 1);
    }

    #[test]
    fn test_find_relations_by_predicate_any() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let n = chrono::Utc::now();
        kg.add_relation(Relation {
            from: "a".into(),
            predicate: "uses".into(),
            to: "x".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        kg.add_relation(Relation {
            from: "b".into(),
            predicate: "uses".into(),
            to: "y".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });
        kg.add_relation(Relation {
            from: "c".into(),
            predicate: "contains".into(),
            to: "z".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });

        let uses = kg.find_relations_by_predicate_any("uses");
        assert_eq!(uses.len(), 2);
    }

    #[test]
    fn test_outgoing_incoming() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        let n = chrono::Utc::now();
        kg.add_relation(Relation {
            from: "alice".into(),
            predicate: "knows".into(),
            to: "bob".into(),
            status: RelationStatus::Active,
            valid_from: n,
            valid_until: None,
            sources: vec![],
        });

        assert_eq!(kg.outgoing("alice").len(), 1);
        assert_eq!(kg.outgoing("bob").len(), 0);
        assert_eq!(kg.incoming("bob").len(), 1);
        assert_eq!(kg.incoming("alice").len(), 0);
    }

    #[test]
    fn test_active_entity_count() {
        let d = TempDir::new().unwrap();
        let mut kg = KnowledgeGraph::open(d.path()).unwrap();
        kg.upsert_entity(e("a", "A", EntityType::Concept));
        kg.upsert_entity(e("b", "B", EntityType::Concept));
        assert_eq!(kg.entity_count(), 2);
        assert_eq!(kg.active_entity_count(), 2);

        kg.merge_entities("a", "b");
        assert_eq!(kg.entity_count(), 2); // both still exist
        assert_eq!(kg.active_entity_count(), 1); // b is merged_into a
    }

    /// Regression: Turtle save→reopen roundtrip preserves entity data.
    /// Found and fixed sync_from_store SPARQL RDF_NS prefix bug (Aug 2026).
    #[test]
    fn test_persistence_roundtrip_save_reopen() {
        let d = TempDir::new().unwrap();
        let path = d.path();
        {
            let mut kg = KnowledgeGraph::open(path).unwrap();
            kg.upsert_entity(e("roundtrip-2", "Roundtrip Two", EntityType::Tool));
            kg.save().unwrap();
        }
        {
            let kg = KnowledgeGraph::open(path).unwrap();
            assert!(kg.get_entity("roundtrip-2").is_some());
            assert_eq!(kg.get_entity("roundtrip-2").unwrap().label, "Roundtrip Two");
        }
    }
}
