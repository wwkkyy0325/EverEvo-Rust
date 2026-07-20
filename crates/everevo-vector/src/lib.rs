//! EverEvo vector store — embedding, chunking, and semantic search.
//!
//! ## Architecture
//!
//! ```text
//! EmbeddingModel (trait)
//!   ├── DummyEmbedder     ← fallback: returns zero vectors
//!   └── FastembedModel    ← Phase 2b: fastembed-rs (ONNX, CPU)
//!
//! VectorStore (trait)
//!   ├── InMemoryStore     ← MVP: flat cosine search, <100K chunks
//!   ├── LanceDBStore      ← Disk-backed: ANN index (feature = "lancedb")
//!   └── PersistentStore   ← Wraps LanceDBStore (or InMemory + JSON fallback)
//! ```
//!
//! ## Upgrade path
//!
//! The trait-based design means we start with the simple in-memory store
//! and swap to LanceDB later without changing any call sites.

mod embedding;
mod engine;
mod memory_store;
mod onnx_embedder;
mod persistent;
mod store_trait;
mod types;

#[cfg(feature = "lancedb")]
mod lancedb_store;

pub use embedding::{DummyEmbedder, EmbeddingModel};
pub use engine::{cosine_similarity, VectorEngine};
pub use onnx_embedder::{check_onnx_model, configure_ort_dylib, OnnxCheckResult, OnnxEmbedder};
pub use memory_store::InMemoryStore;
pub use persistent::PersistentStore;
pub use store_trait::VectorStore;
pub use types::{ChunkType, MemoryChunk, RawChunk, ScoredChunk};

#[cfg(feature = "lancedb")]
pub use lancedb_store::LanceDBStore;
