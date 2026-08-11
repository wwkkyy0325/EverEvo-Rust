//! Knowledge layer — graph, domain, RAG, and wiki.
//!
//! Sub-modules:
//! - `graph`: Oxigraph knowledge graph (entity/relation storage + SPARQL)
//! - `domain`: Multi-domain document ingestion & retrieval
//! - `rag`: RAG pipeline (vector store wrapper)
//! - `wiki`: Project knowledge base (llmwiki)

pub mod domain;
pub mod graph;
pub mod metrics;

// Re-export key types at knowledge:: level for convenience
pub use graph::KnowledgeGraph;
pub use metrics::{mean_reciprocal_rank, ndcg_at_k, precision_at_k, recall_at_k, reciprocal_rank};
