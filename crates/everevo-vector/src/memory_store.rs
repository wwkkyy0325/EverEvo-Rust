//! In-memory vector store — flat cosine similarity search, suitable for <100K chunks.

use std::collections::HashMap;
use std::sync::RwLock;

use uuid::Uuid;

use super::store_trait::VectorStore;
use super::types::{MemoryChunk, ScoredChunk};
use super::engine::cosine_similarity;
use everevo_core::EverEvoError;

/// Simple in-memory vector store with cosine similarity search.
pub struct InMemoryStore {
    pub(crate) chunks: RwLock<HashMap<Uuid, MemoryChunk>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            chunks: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for InMemoryStore {
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        let mut map = self.chunks.write().map_err(|e| {
            EverEvoError::Internal(format!("Vector store lock poisoned: {e}"))
        })?;
        for chunk in chunks {
            map.insert(chunk.id, chunk);
        }
        Ok(())
    }

    fn search(
        &self,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let map = self.chunks.read().map_err(|e| {
            EverEvoError::Internal(format!("Vector store lock poisoned: {e}"))
        })?;
        let mut scored: Vec<ScoredChunk> = map
            .values()
            .map(|chunk| {
                let score = cosine_similarity(query_vector, &chunk.vector);
                ScoredChunk {
                    chunk: chunk.clone(),
                    score,
                }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        let mut map = self.chunks.write().map_err(|e| {
            EverEvoError::Internal(format!("Vector store lock poisoned: {e}"))
        })?;
        for id in ids {
            map.remove(id);
        }
        Ok(())
    }

    fn count(&self) -> usize {
        self.chunks.read().map(|m| m.len()).unwrap_or(0)
    }

    fn get(&self, id: &Uuid) -> Option<MemoryChunk> {
        self.chunks.read().ok()?.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::ProjectionMetadata;

    fn make_chunk(content: &str, vector: &[f32]) -> MemoryChunk {
        MemoryChunk {
            id: Uuid::new_v4(),
            content: content.into(),
            vector: vector.to_vec(),
            source_pointers: vec![],
            projection: ProjectionMetadata::new("1.0.0", "none", vec![], 1.0),
            chunk_type: super::super::types::ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        }
    }

    #[test]
    fn test_in_memory_store_insert_search() {
        let store = InMemoryStore::new();
        let chunk = make_chunk("test", &[1.0, 0.0, 0.0]);
        store.insert(vec![chunk.clone()]).unwrap();
        assert_eq!(store.count(), 1);
        let results = store.search(&[1.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_in_memory_store_delete() {
        let store = InMemoryStore::new();
        let c1 = make_chunk("a", &[1.0, 0.0]);
        let c2 = make_chunk("b", &[0.0, 1.0]);
        store.insert(vec![c1.clone(), c2.clone()]).unwrap();
        assert_eq!(store.count(), 2);
        store.delete(&[c1.id]).unwrap();
        assert_eq!(store.count(), 1);
        assert!(store.get(&c1.id).is_none());
        assert!(store.get(&c2.id).is_some());
    }

    #[test]
    fn test_in_memory_store_get_nonexistent() {
        let store = InMemoryStore::new();
        assert!(store.get(&Uuid::new_v4()).is_none());
    }

    #[test]
    fn test_in_memory_store_count_empty() {
        let store = InMemoryStore::new();
        assert_eq!(store.count(), 0);
    }
}
