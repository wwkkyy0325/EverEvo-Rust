//! Domain knowledge base — multi-domain document ingestion & retrieval.
//!
//! ## Architecture
//!
//! ```text
//! data/domain/inbox/   ← drop files here → auto-trigger pipeline
//!   → classifier (which domain?) → parser → chunker → dedup → index
//!   → LanceDB (vector) + Oxigraph (graph) + SQLite (FTS5)
//! ```
//!
//! ## Design References
//! - AutoGraph (ArangoDB): Corpus Graph + MegaGraph cross-domain linking
//! - Kamat et al. (KBS 2025): 96.5% embedding-based document classification
//! - AnythingLLM: workspace isolation, LanceDB vector store

pub mod chunker;
pub mod classifier;
pub mod document;
pub mod helpers;
pub mod manager;
pub mod parser;
pub mod registry;
pub mod retriever;
pub mod watcher;

// Re-export the public API surface
pub use chunker::SemanticChunker;
pub use classifier::{ClassificationResult, DomainClassifier};
pub use document::{ChunkType, DocumentMeta, DocumentSource, DomainChunk, DomainDocument};
pub use helpers::content_hash;
pub use manager::{DomainCoverage, DomainManager, InboxResult};
pub use parser::DocumentParser;
pub use registry::{Domain, DomainRegistry};
pub use retriever::DomainRetriever;
pub use watcher::DomainWatcher;
