//! Multi-collection vector store — namespace-isolated HNSW indexes.
//!
//! ## Design (industry-aligned)
//!
//! Each collection is a separate HNSW index file. This provides **physical
//! file isolation** rather than metadata-filtered logical isolation:
//! - `memory/`  — facts, conversation themes
//! - `code/`    — code symbols, functions (future)
//! - `domain/`  — uploaded documents (future)
//! - `wiki/`    — auto-generated wiki pages (future)
//!
//! This follows the same pattern as ChromaDB collections, Milvus partitions,
//! and LlamaIndex namespaces. Cross-collection search is supported via
//! `search_multi()` which merges results using RRF.
//!
//! ## Migration
//!
//! On first open, checks for old single-file path (`data/memory/vector/chunks.bin`)
//! and migrates it to the `memory` collection under `data/vector/`.
//!
//! ## Thread safety
//!
//! All methods take `&self`. Internal state is protected by `std::sync::Mutex`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::hnsw_store::HnswStore;
use super::store_trait::VectorStore;
use super::types::{MemoryChunk, ScoredChunk};
use everevo_core::EverEvoError;
use uuid::Uuid;

/// Known collection names. Each maps to a file under `base_dir/{name}.chunks.bin`.
pub const COLLECTION_MEMORY: &str = "memory";
pub const COLLECTION_CODE: &str = "code";
pub const COLLECTION_DOMAIN: &str = "domain";
pub const COLLECTION_WIKI: &str = "wiki";

/// All collection names in creation order.
pub const ALL_COLLECTIONS: &[&str] = &[
    COLLECTION_MEMORY,
    COLLECTION_CODE,
    COLLECTION_DOMAIN,
    COLLECTION_WIKI,
];

/// Inner state protected by Mutex.
struct Inner {
    stores: HashMap<String, HnswStore>,
    dim: usize,
    base_dir: PathBuf,
}

/// Manages multiple namespace-isolated HNSW vector stores.
///
/// All methods take `&self` — internal state is behind a Mutex.
pub struct MultiCollectionStore {
    inner: Mutex<Inner>,
}

impl MultiCollectionStore {
    /// Open all collections under `base_dir`.
    ///
    /// On first open, migrates old `data/memory/vector/chunks.bin` →
    /// `{base_dir}/memory.chunks.bin` if the old file exists.
    pub fn open(
        base_dir: impl Into<PathBuf>,
        dim: usize,
        old_path: Option<&Path>,
    ) -> Result<Self, EverEvoError> {
        let base_dir: PathBuf = base_dir.into();
        std::fs::create_dir_all(&base_dir).map_err(|e| {
            EverEvoError::Internal(format!("Create vector dir {}: {e}", base_dir.display()))
        })?;

        if let Some(old) = old_path {
            if old.exists() {
                Self::migrate_old_store(old, &base_dir, dim);
            }
        }

        let mut stores = HashMap::new();
        let memory_file = format!("memory-{}", dim);
        let memory_store = HnswStore::open(base_dir.join(&memory_file), dim)?;
        stores.insert(COLLECTION_MEMORY.to_string(), memory_store);

        tracing::info!(
            base_dir = %base_dir.display(),
            "MultiCollectionStore opened (memory + 3 lazy)"
        );

        Ok(Self {
            inner: Mutex::new(Inner {
                stores,
                dim,
                base_dir,
            }),
        })
    }

    /// Insert chunks into a specific collection. Creates the collection
    /// lazily if it doesn't exist. Files are named `{collection}-{dim}.bin`.
    pub fn insert(&self, collection: &str, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| EverEvoError::Internal(format!("Lock collections: {e}")))?;
        if !inner.stores.contains_key(collection) {
            let file_stem = format!("{}-{}", collection, inner.dim);
            let store = HnswStore::open(inner.base_dir.join(&file_stem), inner.dim)?;
            inner.stores.insert(collection.to_string(), store);
        }
        inner.stores[collection].insert(chunks)
    }

    /// Search within a single collection.
    pub fn search(
        &self,
        collection: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let inner = self
            .inner
            .lock()
            .map_err(|e| EverEvoError::Internal(format!("Lock collections: {e}")))?;
        if let Some(store) = inner.stores.get(collection) {
            store.search(query_vector, top_k)
        } else {
            Ok(vec![])
        }
    }

    /// Cross-collection search with RRF fusion.
    pub fn search_multi(
        &self,
        collections: &[&str],
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        if collections.is_empty() {
            return Ok(vec![]);
        }
        if collections.len() == 1 {
            return self.search(collections[0], query_vector, top_k);
        }

        let inner = self
            .inner
            .lock()
            .map_err(|e| EverEvoError::Internal(format!("Lock collections: {e}")))?;

        let mut all_results: Vec<Vec<ScoredChunk>> = Vec::new();
        for &col in collections {
            if let Some(store) = inner.stores.get(col) {
                if let Ok(results) = store.search(query_vector, top_k) {
                    all_results.push(results);
                }
            }
        }

        // RRF merge: score = Σ 1/(k + rank_i), k=60.
        let k: f32 = 60.0;
        let mut fused: HashMap<Uuid, (f32, ScoredChunk)> = HashMap::new();
        for results in &all_results {
            for (rank, chunk) in results.iter().enumerate() {
                let rrf_score = 1.0 / (k + (rank + 1) as f32);
                fused
                    .entry(chunk.chunk.id)
                    .and_modify(|(score, _)| *score += rrf_score)
                    .or_insert_with(|| (rrf_score, chunk.clone()));
            }
        }

        let mut merged: Vec<ScoredChunk> = fused
            .into_values()
            .map(|(score, mut chunk)| {
                chunk.score = score;
                chunk
            })
            .collect();
        merged.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        merged.truncate(top_k);
        Ok(merged)
    }

    /// Delete chunks from a collection.
    pub fn delete(&self, collection: &str, ids: &[Uuid]) -> Result<(), EverEvoError> {
        let inner = self
            .inner
            .lock()
            .map_err(|e| EverEvoError::Internal(format!("Lock collections: {e}")))?;
        if let Some(store) = inner.stores.get(collection) {
            store.delete(ids)
        } else {
            Ok(())
        }
    }

    /// Count in a collection.
    pub fn count(&self, collection: &str) -> usize {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        inner.stores.get(collection).map(|s| s.count()).unwrap_or(0)
    }

    /// Total count across all open collections.
    pub fn total_count(&self) -> usize {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        inner.stores.values().map(|s| s.count()).sum()
    }

    /// Get a chunk by UUID from a collection.
    pub fn get(&self, collection: &str, id: &Uuid) -> Option<MemoryChunk> {
        let inner = self.inner.lock().ok()?;
        inner.stores.get(collection).and_then(|s| s.get(id))
    }

    /// List names of currently open collections.
    pub fn collection_names(&self) -> Vec<String> {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        inner.stores.keys().cloned().collect()
    }

    // ── Internal ───────────────────────────────────────────────────────

    fn migrate_old_store(old_path: &Path, new_dir: &Path, dim: usize) {
        // Try both .bin and .json extensions.
        let old_bin = old_path.with_extension("bin");
        let old_json = old_path.with_extension("json");
        let actual_old = if old_bin.exists() {
            &old_bin
        } else {
            &old_json
        };
        if !actual_old.exists() {
            return;
        }
        let new_path = new_dir.join(format!("memory-{}.bin", dim));
        if new_path.exists() {
            return;
        }
        if let Err(e) = std::fs::rename(actual_old, &new_path) {
            if std::fs::copy(actual_old, &new_path).is_ok() {
                let _ = std::fs::remove_file(actual_old);
            }
            tracing::warn!(error = %e, "Old vector store migration failed — starting fresh");
        } else {
            tracing::info!(
                from = %actual_old.display(),
                to = %new_path.display(),
                "Migrated old vector store to MultiCollectionStore"
            );
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;
    use everevo_core::memory::ProjectionMetadata;
    use tempfile::TempDir;

    fn make_chunk(id: Uuid, v: Vec<f32>) -> MemoryChunk {
        MemoryChunk {
            id,
            content: String::new(),
            vector: v,
            source_pointers: vec![],
            projection: ProjectionMetadata::new("test", "test", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        }
    }

    #[test]
    fn test_namespace_isolation() {
        let dir = TempDir::new().unwrap();
        let store = MultiCollectionStore::open(dir.path().join("vector"), 4, None).unwrap();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        store
            .insert("memory", vec![make_chunk(id1, vec![1.0, 0.0, 0.0, 0.0])])
            .unwrap();
        store
            .insert("code", vec![make_chunk(id2, vec![0.0, 1.0, 0.0, 0.0])])
            .unwrap();

        let mem = store.search("memory", &[0.9, 0.1, 0.0, 0.0], 5).unwrap();
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].chunk.id, id1);

        let code = store.search("code", &[0.1, 0.9, 0.0, 0.0], 5).unwrap();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].chunk.id, id2);

        assert_eq!(store.count("memory"), 1);
        assert_eq!(store.count("code"), 1);
        assert_eq!(store.total_count(), 2);
    }

    #[test]
    fn test_lazy_collection_creation() {
        let dir = TempDir::new().unwrap();
        let store = MultiCollectionStore::open(dir.path().join("vector"), 3, None).unwrap();
        // Initially only "memory" exists.
        assert_eq!(store.count("memory"), 0);
        // Accessing "code" creates it lazily.
        store
            .insert(
                "code",
                vec![make_chunk(Uuid::new_v4(), vec![1.0, 0.0, 0.0])],
            )
            .unwrap();
        assert_eq!(store.count("code"), 1);
    }

    #[test]
    fn test_cross_collection_search() {
        let dir = TempDir::new().unwrap();
        let store = MultiCollectionStore::open(dir.path().join("vector"), 4, None).unwrap();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        store
            .insert("memory", vec![make_chunk(id1, vec![1.0, 0.0, 0.0, 0.0])])
            .unwrap();
        store
            .insert("code", vec![make_chunk(id2, vec![0.9, 0.1, 0.0, 0.0])])
            .unwrap();

        let results = store
            .search_multi(&["memory", "code"], &[1.0, 0.0, 0.0, 0.0], 5)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_migration() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("vector");
        // Create an old-format store using HnswStore and verify it's readable
        // through MultiCollectionStore after migration.
        let old_bin = dir.path().join("memory.bin");
        {
            let old = HnswStore::open(dir.path().join("memory"), 3).unwrap();
            old.insert(vec![make_chunk(Uuid::new_v4(), vec![1.0, 0.0, 0.0])])
                .unwrap();
        }
        assert!(old_bin.exists());

        // Open as MultiCollectionStore at a different base dir — migration
        // should move the old file.
        let store = MultiCollectionStore::open(&base, 3, Some(&old_bin)).unwrap();
        assert_eq!(store.count("memory"), 1);
    }
}
