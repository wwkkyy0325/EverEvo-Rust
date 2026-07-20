//! Persistent vector store — disk-backed with LanceDB or JSON fallback.

use std::path::PathBuf;

use uuid::Uuid;

use super::store_trait::VectorStore;
use super::types::{MemoryChunk, ScoredChunk};
use everevo_core::EverEvoError;

/// Disk-backed vector store.
///
/// When the `lancedb` feature is enabled, delegates to [`LanceDBStore`] for
/// ANN-powered disk persistence.
///
/// When LanceDB is unavailable, falls back to a JSON-backed [`InMemoryStore`]
/// that saves to disk on every mutation.
pub struct PersistentStore {
    #[cfg(feature = "lancedb")]
    inner: super::lancedb_store::LanceDBStore,
    #[cfg(not(feature = "lancedb"))]
    inner: super::memory_store::InMemoryStore,
    #[cfg(not(feature = "lancedb"))]
    save_path: PathBuf,
}

impl PersistentStore {
    #[cfg(feature = "lancedb")]
    pub fn open(path: impl Into<PathBuf>, dim: usize) -> Result<Self, EverEvoError> {
        let inner = super::lancedb_store::LanceDBStore::open(path, dim)?;
        Ok(Self { inner })
    }

    #[cfg(not(feature = "lancedb"))]
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let save_path: PathBuf = path.into();
        let inner = if save_path.exists() {
            let json = std::fs::read_to_string(&save_path).map_err(|e| {
                EverEvoError::Internal(format!("Read vector store: {e}"))
            })?;
            let chunks: Vec<MemoryChunk> =
                serde_json::from_str(&json).unwrap_or_default();
            let store = super::memory_store::InMemoryStore::new();
            store.insert(chunks)?;
            store
        } else {
            super::memory_store::InMemoryStore::new()
        };
        Ok(Self { inner, save_path })
    }

    #[cfg(not(feature = "lancedb"))]
    fn save_to_disk(&self) -> Result<(), EverEvoError> {
        let map = self
            .inner
            .chunks
            .read()
            .map_err(|e| EverEvoError::Internal(format!("Lock error: {e}")))?;
        let chunks: Vec<&MemoryChunk> = map.values().collect();
        let json = serde_json::to_string_pretty(&chunks).map_err(|e| {
            EverEvoError::Internal(format!("Serialize vector store: {e}"))
        })?;
        if let Some(parent) = self.save_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.save_path, &json).map_err(|e| {
            EverEvoError::Internal(format!("Write vector store: {e}"))
        })?;
        Ok(())
    }
}

impl VectorStore for PersistentStore {
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        #[cfg(feature = "lancedb")]
        {
            self.inner.insert(chunks)
        }
        #[cfg(not(feature = "lancedb"))]
        {
            self.inner.insert(chunks)?;
            self.save_to_disk()
        }
    }

    fn search(
        &self,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        self.inner.search(query_vector, top_k)
    }

    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        #[cfg(feature = "lancedb")]
        {
            self.inner.delete(ids)
        }
        #[cfg(not(feature = "lancedb"))]
        {
            self.inner.delete(ids)?;
            self.save_to_disk()
        }
    }

    fn count(&self) -> usize {
        self.inner.count()
    }

    fn get(&self, id: &Uuid) -> Option<MemoryChunk> {
        self.inner.get(id)
    }
}
