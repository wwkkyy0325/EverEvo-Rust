//! Knowledge graph — HashMap primary storage + Oxigraph SPARQL layer.

use std::collections::HashMap;
use std::path::PathBuf;

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;

use everevo_core::EverEvoError;

use super::resolver::{EntityResolver, ResolveStats};
use super::types::{Entity, EntityType, Relation, RelationStatus};

// ── Namespace ───────────────────────────────────────────────────────────────

/// Base IRI for all EverEvo knowledge-graph resources.
const NS: &str = "http://everevo.io/";

/// Standard RDF type predicate IRI.
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Build the full IRI for an entity node.
fn entity_iri(id: &str) -> NamedNode {
    NamedNode::new(format!("{NS}e/{id}")).expect("entity IRI must be valid")
}

/// Build a predicate IRI under the EverEvo namespace.
fn ns_pred(name: &str) -> NamedNode {
    NamedNode::new(format!("{NS}{name}")).expect("predicate IRI must be valid")
}

/// The standard `rdf:type` predicate.
fn rdf_type() -> NamedNode {
    NamedNode::new(RDF_TYPE).expect("rdf:type IRI is valid")
}

/// Extract the raw value from an RDF term — for literals, returns `value()`
/// (unescaped lexical form); for IRIs and blank nodes, falls back to `to_string()`.
fn term_value(t: &Term) -> String {
    match t {
        Term::Literal(lit) => lit.value().to_string(),
        _ => t.to_string(),
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Build a Quad with a NamedNode subject in the default graph.
fn quad_named(s: NamedNode, p: NamedNode, o: Term) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(s),
        p,
        o,
        GraphName::DefaultGraph,
    )
}

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

    // ── Entity CRUD (HashMap primary) ─────────────────────────────────
    pub fn upsert_entity(&mut self, entity: Entity) -> bool {
        let is_new = !self.entities.contains_key(&entity.id);
        self.entities.insert(entity.id.clone(), entity);
        is_new
    }
    pub fn get_entity(&self, id: &str) -> Option<Entity> {
        self.entities.get(id).cloned()
    }
    pub fn find_by_type(&self, entity_type: &EntityType) -> Vec<Entity> {
        self.entities
            .values()
            .filter(|e| &e.entity_type == entity_type)
            .cloned()
            .collect()
    }
    pub fn search(&self, query: &str) -> Vec<Entity> {
        let q = query.to_lowercase();
        self.entities
            .values()
            .filter(|e| e.label.to_lowercase().contains(&q) || e.id.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }
    pub fn all_entities(&self) -> Vec<Entity> {
        self.entities.values().cloned().collect()
    }
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }
    pub fn active_entity_count(&self) -> usize {
        self.entities
            .values()
            .filter(|e| e.merged_into.is_none())
            .count()
    }

    // ── Relation CRUD (Vec primary) ───────────────────────────────────
    pub fn add_relation(&mut self, relation: Relation) {
        for existing in self.relations.iter_mut() {
            if existing.from == relation.from
                && existing.predicate == relation.predicate
                && existing.to != relation.to
                && existing.status == RelationStatus::Active
            {
                existing.status = RelationStatus::Superseded;
                existing.valid_until = Some(relation.valid_from);
            }
        }
        self.relations.push(Relation {
            status: RelationStatus::Active,
            ..relation
        });
    }
    pub fn outgoing(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.from == entity_id && r.status == RelationStatus::Active)
            .collect()
    }
    pub fn incoming(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.to == entity_id && r.status == RelationStatus::Active)
            .collect()
    }
    pub fn relation_count(&self) -> usize {
        self.relations
            .iter()
            .filter(|r| r.status == RelationStatus::Active)
            .count()
    }

    /// Add a many-to-many relation (no supersede semantics).
    ///
    /// Unlike `add_relation()`, this does NOT supersede existing relations
    /// with the same `(from, predicate)` pair. Use for `DependsOn`,
    /// `HasCapability`, and other one-to-many predicates.
    pub fn add_relation_many(&mut self, relation: Relation) {
        // Only skip if the EXACT same relation already exists
        let already_exists = self.relations.iter().any(|r| {
            r.from == relation.from
                && r.predicate == relation.predicate
                && r.to == relation.to
                && r.status == RelationStatus::Active
        });
        if !already_exists {
            self.relations.push(Relation {
                status: RelationStatus::Active,
                ..relation
            });
        }
    }

    /// Find active relations matching a given predicate.
    ///
    /// Matches on the predicate string directly (both raw strings and
    /// `SymbolPredicate::as_uri_fragment()` values).
    pub fn find_relations_by_predicate(
        &self,
        from: &str,
        predicate: &str,
    ) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| {
                r.from == from
                    && r.predicate == predicate
                    && r.status == RelationStatus::Active
            })
            .collect()
    }

    /// Find all active relations where the predicate matches (any from/to).
    pub fn find_relations_by_predicate_any(&self, predicate: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.predicate == predicate && r.status == RelationStatus::Active)
            .collect()
    }

    /// Seed the knowledge graph with project structure entities so
    /// `memory kg_search` returns useful results immediately — no empty
    /// "no entities" response on first use.
    ///
    /// Only seeds if the graph is empty (idempotent). Call after `open()`.
    pub fn seed_project_structure(&mut self, crate_names: &[&str]) {
        if self.entity_count() > 0 {
            return; // already seeded
        }
        let now = chrono::Utc::now();
        let empty_props = std::collections::HashMap::new();
        let empty_sources = Vec::new();

        // Root project entity
        self.upsert_entity(Entity {
            id: "everevo".into(),
            label: "EverEvo".into(),
            entity_type: EntityType::Project,
            properties: empty_props.clone(),
            sources: empty_sources.clone(),
            created_at: now,
            updated_at: now,
            merged_into: None,
        });

        // Crate entities with relations
        for &name in crate_names {
            let id = format!("crate-{name}");
            self.upsert_entity(Entity {
                id: id.clone(),
                label: format!("Crate: {name}"),
                entity_type: EntityType::Other("Crate".into()),
                properties: empty_props.clone(),
                sources: empty_sources.clone(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });
            self.add_relation(Relation {
                from: "everevo".into(),
                predicate: "contains".into(),
                to: id,
                status: RelationStatus::Active,
                valid_from: now,
                valid_until: None,
                sources: empty_sources.clone(),
            });
        }

        // Tech stack entities
        let techs = [
            ("rust", "Rust", "Language"),
            ("typescript", "TypeScript", "Language"),
            ("react", "React", "Framework"),
            ("axum", "Axum", "Framework"),
            ("sqlite", "SQLite", "Database"),
            ("oxigraph", "Oxigraph", "GraphDB"),
            ("onnx", "ONNX", "Runtime"),
            ("tauri", "Tauri", "Framework"),
            ("tokio", "Tokio", "Runtime"),
            ("reqwest", "reqwest", "Library"),
            ("sqlx", "SQLx", "Library"),
        ];
        for (id, label, kind) in &techs {
            self.upsert_entity(Entity {
                id: id.to_string(),
                label: label.to_string(),
                entity_type: EntityType::Other(kind.to_string()),
                properties: empty_props.clone(),
                sources: empty_sources.clone(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });
            self.add_relation(Relation {
                from: "everevo".into(),
                predicate: "uses".into(),
                to: id.to_string(),
                status: RelationStatus::Active,
                valid_from: now,
                valid_until: None,
                sources: empty_sources.clone(),
            });
        }

        tracing::info!(
            entities = self.entity_count(),
            relations = self.relation_count(),
            "Knowledge graph seeded with project structure"
        );
    }

    // ── Graph Operations ──────────────────────────────────────────────
    pub fn expand(&self, start_id: &str, depth: usize) -> Vec<Entity> {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut frontier: Vec<String> = vec![start_id.to_string()];
        let mut result: Vec<Entity> = Vec::new();
        for _ in 0..=depth {
            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                if !visited.insert(id.clone()) {
                    continue;
                }
                if let Some(e) = self.entities.get(id) {
                    result.push(e.clone());
                }
                for r in self.outgoing(id) {
                    if !visited.contains(&r.to) {
                        next.push(r.to.clone());
                    }
                }
                for r in self.incoming(id) {
                    if !visited.contains(&r.from) {
                        next.push(r.from.clone());
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        result
    }
    pub fn merge_entities(&mut self, canonical_id: &str, merged_id: &str) {
        if canonical_id == merged_id {
            return;
        }
        let merged = match self.entities.get(merged_id) {
            Some(e) => e.clone(),
            None => return,
        };
        if let Some(canonical) = self.entities.get_mut(canonical_id) {
            for (k, v) in &merged.properties {
                canonical
                    .properties
                    .entry(k.clone())
                    .or_insert_with(|| v.clone());
            }
            for src in &merged.sources {
                if !canonical
                    .sources
                    .iter()
                    .any(|s| s.message_id == src.message_id)
                {
                    canonical.sources.push(src.clone());
                }
            }
            canonical.updated_at = chrono::Utc::now();
        }
        for rel in self.relations.iter_mut() {
            if rel.from == *merged_id {
                rel.from = canonical_id.to_string();
            }
            if rel.to == *merged_id {
                rel.to = canonical_id.to_string();
            }
        }
        if let Some(merged_entity) = self.entities.get_mut(merged_id) {
            merged_entity.merged_into = Some(canonical_id.to_string());
            merged_entity.updated_at = chrono::Utc::now();
        }
    }
    pub fn resolve_all(&mut self) -> ResolveStats {
        let resolver = EntityResolver::default();
        resolver.resolve(self)
    }

    /// Populate HashMap entities from the Oxigraph Store (on load).
    fn sync_from_store(&mut self) {
        let sparql = format!(
            "PREFIX rdf: <{RDF_TYPE}> \
             PREFIX evo: <{NS}> \
             SELECT ?id ?label ?type ?created ?updated ?merged ?props ?srcs \
             WHERE {{ \
               ?e rdf:type evo:Entity ; \
                  evo:label ?label ; \
                  evo:entityType ?type ; \
                  evo:createdAt ?created ; \
                  evo:updatedAt ?updated . \
               OPTIONAL {{ ?e evo:properties ?props }} \
               OPTIONAL {{ ?e evo:sources ?srcs }} \
               OPTIONAL {{ ?e evo:mergedInto ?merged }} \
               BIND(STRAFTER(STR(?e), \"{NS}e/\") AS ?id) \
             }}"
        );
        if let Ok(QueryResults::Solutions(solutions)) = self.store.query(&sparql) {
            for sol in solutions.flatten() {
                let id = sol.get("id").map(term_value).unwrap_or_default();
                let label = sol.get("label").map(term_value).unwrap_or_default();
                let etype = sol.get("type").map(term_value).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                self.entities.insert(
                    id.clone(),
                    Entity {
                        id,
                        label,
                        entity_type: match etype.as_str() {
                            "Person" => EntityType::Person,
                            "Project" => EntityType::Project,
                            "Tool" => EntityType::Tool,
                            "Concept" => EntityType::Concept,
                            "File" => EntityType::File,
                            "Event" => EntityType::Event,
                            "Capability" => EntityType::Capability,
                            "KnowledgeSource" => EntityType::KnowledgeSource,
                            "Constraint" => EntityType::Constraint,
                            other => EntityType::Other(other.into()),
                        },
                        properties: sol
                            .get("props")
                            .map(term_value)
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        sources: sol
                            .get("srcs")
                            .map(term_value)
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        created_at: sol
                            .get("created")
                            .map(term_value)
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(chrono::Utc::now),
                        updated_at: sol
                            .get("updated")
                            .map(term_value)
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(chrono::Utc::now),
                        merged_into: sol.get("merged").map(term_value),
                    },
                );
            }
        }
    }

    // ── SPARQL Query (Oxigraph) ───────────────────────────────────────
    pub fn query_sparql(&self, sparql: &str) -> Result<Vec<HashMap<String, String>>, EverEvoError> {
        let results = self
            .store
            .query(sparql)
            .map_err(|e| EverEvoError::KnowledgeGraph(format!("SPARQL: {e}")))?;
        if let QueryResults::Solutions(solutions) = results {
            let mut rows = Vec::new();
            for sol in solutions.flatten() {
                let mut row = HashMap::new();
                for (var, term) in sol.iter() {
                    row.insert(var.to_string(), term.to_string());
                }
                rows.push(row);
            }
            Ok(rows)
        } else {
            Ok(Vec::new())
        }
    }

    // ── Persistence (Oxigraph Turtle) ─────────────────────────────────
    pub fn save(&self) -> Result<(), EverEvoError> {
        let path = self.store_path.join("knowledge.ttl");
        let file = std::fs::File::create(&path).map_err(EverEvoError::Io)?;
        let mut writer = std::io::BufWriter::new(file);
        // Sync entities to store
        for entity in self.entities.values() {
            let s = entity_iri(&entity.id);
            self.store
                .insert(&quad_named(
                    s.clone(),
                    rdf_type(),
                    Term::NamedNode(ns_pred("Entity")),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert type: {e}")))?;
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("label"),
                    Term::Literal(Literal::new_simple_literal(&entity.label)),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert label: {e}")))?;
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("entityType"),
                    Term::Literal(Literal::new_simple_literal(entity.entity_type.as_str())),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert entityType: {e}")))?;
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("createdAt"),
                    Term::Literal(Literal::new_simple_literal(entity.created_at.to_rfc3339())),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert createdAt: {e}")))?;
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("updatedAt"),
                    Term::Literal(Literal::new_simple_literal(entity.updated_at.to_rfc3339())),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert updatedAt: {e}")))?;
            let props_json =
                serde_json::to_string(&entity.properties).unwrap_or_else(|_| "{}".into());
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("properties"),
                    Term::Literal(Literal::new_simple_literal(&props_json)),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert properties: {e}")))?;
            let sources_json =
                serde_json::to_string(&entity.sources).unwrap_or_else(|_| "[]".into());
            self.store
                .insert(&quad_named(
                    s.clone(),
                    ns_pred("sources"),
                    Term::Literal(Literal::new_simple_literal(&sources_json)),
                ))
                .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert sources: {e}")))?;
            if let Some(ref m) = entity.merged_into {
                self.store
                    .insert(&quad_named(
                        s,
                        ns_pred("mergedInto"),
                        Term::Literal(Literal::new_simple_literal(m)),
                    ))
                    .map_err(|e| EverEvoError::KnowledgeGraph(format!("Insert mergedInto: {e}")))?;
            }
        }
        self.store
            .dump_graph_to_writer(
                oxigraph::model::GraphNameRef::DefaultGraph,
                RdfFormat::Turtle,
                &mut writer,
            )
            .map_err(|e| EverEvoError::KnowledgeGraph(format!("Save Turtle: {e}")))?;
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
}
