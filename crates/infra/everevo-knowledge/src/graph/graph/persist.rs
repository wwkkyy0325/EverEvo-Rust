// ── Persistence (Oxigraph RDF / SPARQL / Turtle) ────────────────────────────

use std::collections::HashMap;

use oxigraph::io::RdfFormat;
use oxigraph::model::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxigraph::sparql::QueryResults;

use everevo_core::EverEvoError;

use super::KnowledgeGraph;
use super::super::types::{Entity, EntityType};

// ── Namespace ───────────────────────────────────────────────────────────────

/// Base IRI for all EverEvo knowledge-graph resources.
const NS: &str = "http://everevo.io/";

/// Standard RDF type predicate IRI.
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
/// RDF namespace IRI (used for SPARQL prefixes).
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

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

// ── Oxigraph store layer ────────────────────────────────────────────────────

impl KnowledgeGraph {
    /// Populate HashMap entities from the Oxigraph Store (on load).
    pub(crate) fn sync_from_store(&mut self) {
        let sparql = format!(
            "PREFIX rdf: <{RDF_NS}> \
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
        match self.store.query(&sparql) {
            Ok(QueryResults::Solutions(solutions)) => {
                let mut count = 0usize;
                for sol in solutions.flatten() {
                    count += 1;
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
                tracing::debug!(
                    count,
                    inserted = self.entities.len(),
                    "sync_from_store: loaded entities from Turtle"
                );
            }
            Ok(_) => {
                tracing::debug!("sync_from_store: query returned non-Solutions result");
            }
            Err(e) => {
                tracing::warn!(error = %e, "sync_from_store: SPARQL query failed");
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
