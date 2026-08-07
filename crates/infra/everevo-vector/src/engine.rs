//! Vector engine — combines an embedder and a store, plus utility functions.

use everevo_core::EverEvoError;

use super::embedding::EmbeddingModel;
use super::store_trait::VectorStore;
use super::types::{MemoryChunk, RawChunk, ScoredChunk};

/// Compute cosine similarity between two float vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// High-level vector engine combining an embedder and a store.
pub struct VectorEngine<E: EmbeddingModel, S: VectorStore> {
    pub embedder: E,
    pub store: S,
}

impl<E: EmbeddingModel, S: VectorStore> VectorEngine<E, S> {
    pub fn new(embedder: E, store: S) -> Self {
        Self { embedder, store }
    }

    /// Insert raw text chunks (auto-embeds).
    pub fn insert_texts(&self, chunks: Vec<RawChunk>) -> Result<(), EverEvoError> {
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let vectors = self.embedder.encode_batch(&texts)?;
        let memory_chunks: Vec<MemoryChunk> = chunks
            .into_iter()
            .zip(vectors)
            .map(|(raw, vector)| MemoryChunk {
                id: raw.id,
                content: raw.content,
                vector,
                source_pointers: raw.source_pointers,
                projection: raw.projection,
                chunk_type: raw.chunk_type,
                created_at: chrono::Utc::now(),
                retrieval_count: 0,
            })
            .collect();
        self.store.insert(memory_chunks)
    }

    /// Search by raw text query (auto-embeds).
    pub fn search_text(&self, query: &str, top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let query_vector = self.embedder.encode(query)?;
        self.store.search(&query_vector, top_k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_different_length() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_both_zero() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![0.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_high_dim() {
        let a: Vec<f32> = (0..128).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..128).map(|i| i as f32).collect();
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
    }
}
