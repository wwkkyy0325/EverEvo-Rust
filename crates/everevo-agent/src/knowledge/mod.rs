//! Knowledge layer — graph, domain, RAG, and wiki.
//!
//! Sub-modules:
//! - `graph`: Oxigraph knowledge graph (entity/relation storage + SPARQL)
//! - `domain`: Multi-domain document ingestion & retrieval
//! - `rag`: RAG pipeline (vector store wrapper)
//! - `wiki`: Project knowledge base (llmwiki)

pub mod domain;
pub mod graph;

// Re-export key types at knowledge:: level for convenience
pub use graph::KnowledgeGraph;
