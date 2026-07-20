//! EverEvo knowledge graph — Oxigraph-based entity/relation storage and SPARQL
//! query.
//!
//! ## Architecture
//!
//! Uses [Oxigraph](https://crates.io/crates/oxigraph) as an in-memory RDF quad
//! store with SPARQL 1.1 support.  Entities and relations are modelled as RDF
//! resources under `http://everevo.io/`.  Persisted as Turtle (.ttl) files under
//! `data/memory/graph/`.
//!
//! ## Design
//!
//! - **Entities**: nodes with type + properties — stored as named RDF resources
//! - **Relations**: labelled edges between entities — stored as RDF blank-node
//!   resources
//! - **Conflict handling**: contradicting edges marked invalid, not deleted
//! - **Source pointers**: every triple links back to raw conversation data

pub mod extraction;
pub mod graph;
pub mod resolver;
pub mod types;

// Re-export the public API surface
pub use extraction::build_extraction_prompt;
pub use graph::KnowledgeGraph;
pub use resolver::{EntityResolver, MatchResult, ResolveStats};
pub use types::{Entity, EntityType, Relation, RelationStatus, Triple};
