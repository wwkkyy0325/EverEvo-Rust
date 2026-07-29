//! EverEvo vector store — embedding, chunking, and HNSW semantic search.
//!
//! ## Architecture
//!
//! ```text
//! EmbeddingModel (trait)
//!   ├── DummyEmbedder     ← fallback: returns zero vectors
//!   └── FastembedModel    ← fastembed-rs (ONNX, CPU)
//!
//! VectorStore (trait)
//!   └── HnswStore         ← Pure Rust HNSW ANN index with cosine distance.
//!                            Single backend — no fallbacks, no platform issues.
//! ```
//!
//! ## Why HNSW (not LanceDB, not Flat)
//!
//! - **LanceDB**: tokio nested-runtime panics on Windows. Requires separate
//!   process for reliable embedding. Overkill for desktop agents.
//! - **Flat search**: O(N×D) scaling. Fine up to ~50K vectors, then linear
//!   latency becomes noticeable compared to LLM inference time.
//! - **HNSW**: O(log N) search, >99% recall, zero FFI, no async runtime,
//!   disk persistence via bincode. Good from 100 vectors to 10M+.

mod embedding;
mod engine;
mod hnsw_store;
mod model_registry;
mod multi_collection;
mod onnx_embedder;
mod store_trait;
mod types;

pub use embedding::{DummyEmbedder, EmbeddingModel};
pub use engine::{cosine_similarity, VectorEngine};
pub use hnsw_store::HnswStore;
pub use model_registry::{ModelMeta, ModelRegistry};
pub use multi_collection::{MultiCollectionStore, ALL_COLLECTIONS, COLLECTION_CODE, COLLECTION_DOMAIN, COLLECTION_MEMORY, COLLECTION_WIKI};
pub use onnx_embedder::{check_onnx_model, configure_ort_dylib, OnnxCheckResult, OnnxEmbedder};
pub use store_trait::VectorStore;
pub use types::{make_chunk, make_chunk_with_sources, ChunkType, MemoryChunk, RawChunk, ScoredChunk};
