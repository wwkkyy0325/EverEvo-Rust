//! Vector store trait — the abstract interface for vector storage backends.

use uuid::Uuid;

use super::types::{MemoryChunk, ScoredChunk};
use everevo_core::EverEvoError;

/// Abstract vector store — plug in different backends.
pub trait VectorStore: Send + Sync {
    /// Insert chunks into the store.
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError>;
    /// Search for the top-k most similar chunks by cosine similarity.
    fn search(&self, query_vector: &[f32], top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError>;
    /// Delete chunks by ID.
    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError>;
    /// Total number of chunks in the store.
    fn count(&self) -> usize;
    /// Get a chunk by ID.
    fn get(&self, id: &Uuid) -> Option<MemoryChunk>;
}
