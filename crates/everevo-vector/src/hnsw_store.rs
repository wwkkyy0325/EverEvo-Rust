//! HNSW vector store — pure Rust ANN index with cosine distance.
//!
//! Replaces LanceDB (nested-runtime panics on Windows) and InMemory flat search
//! (O(N) scaling). HNSW provides O(log N) approximate nearest-neighbor search
//! with >99% recall at typical ef values.
//!
//! ## Thread safety
//!
//! `hnsw_rs::Hnsw` uses internal `parking_lot` locks — `insert()` and `search()`
//! take `&self`. Metadata (id_map, vectors) is protected by a `std::sync::Mutex`.
//!
//! ## Persistence
//!
//! Shadow-map approach (like `claw-vector`, `graphmind`):
//! - Save: lock metadata → serialize (id, uuid, vector) → JSON
//! - Load: deserialize JSON → parallel_insert into fresh HNSW
//!
//! This avoids the lifetime complexity of `hnsw_rs::HnswIo::load_hnsw()`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use hnsw_rs::prelude::*;
use uuid::Uuid;

use super::store_trait::VectorStore;
use super::types::{ChunkType, MemoryChunk, ScoredChunk};
use everevo_core::memory::ProjectionMetadata;
use everevo_core::EverEvoError;

/// HNSW parameter M: bi-directional links per node (12–48, default 32).
const M: usize = 32;
/// Max layers. 16 supports up to 65K elements; graph auto-scales beyond.
const MAX_LAYER: usize = 16;
/// ef_construction: dynamic candidate list during build. 200 = standard.
const EF_CONSTRUCTION: usize = 200;
/// Initial capacity hint.
const INITIAL_CAPACITY: usize = 100_000;

/// Mutable metadata guarded by a Mutex (HNSW graph itself is lock-free).
struct Meta {
    id_map: HashMap<Uuid, DataId>,
    rev_map: HashMap<DataId, Uuid>,
    vectors: HashMap<DataId, Vec<f32>>,
    next_id: DataId,
}

/// Disk-backed HNSW vector store with cosine distance.
pub struct HnswStore {
    /// The HNSW index — internally thread-safe (parking_lot), &self methods.
    hnsw: Hnsw<'static, f32, DistCosine>,
    /// Mutable metadata. Locked separately from the HNSW graph so search
    /// (which reads metadata) can run concurrently with other searches.
    meta: Mutex<Meta>,
    /// Path for JSON persistence.
    persist_path: PathBuf,
    /// Dimensionality (informational, used for external validation).
    #[allow(dead_code)]
    dim: usize,
}

impl HnswStore {
    /// Open or create an HNSW store.
    ///
    /// If `base_path.json` exists, loads from it. Otherwise creates empty.
    pub fn open(base_path: impl Into<PathBuf>, dim: usize) -> Result<Self, EverEvoError> {
        let base: PathBuf = base_path.into();
        let persist_path = base.with_extension("json");

        if persist_path.exists() {
            Self::load(persist_path, dim)
        } else {
            Ok(Self::empty(persist_path, dim))
        }
    }

    fn empty(persist_path: PathBuf, dim: usize) -> Self {
        let hnsw = Hnsw::<f32, DistCosine>::new(
            M,
            INITIAL_CAPACITY,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine {},
        );
        if let Some(parent) = persist_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        tracing::info!(dim, "HnswStore created (empty)");
        Self {
            hnsw,
            meta: Mutex::new(Meta {
                id_map: HashMap::new(),
                rev_map: HashMap::new(),
                vectors: HashMap::new(),
                next_id: 0,
            }),
            persist_path,
            dim,
        }
    }

    fn load(persist_path: PathBuf, _dim: usize) -> Result<Self, EverEvoError> {
        let json = std::fs::read_to_string(&persist_path).map_err(|e| {
            EverEvoError::Internal(format!("Read HNSW store {}: {e}", persist_path.display()))
        })?;
        let entries: Vec<PersistedEntry> =
            serde_json::from_str(&json).unwrap_or_default();
        let count = entries.len();
        let capacity = (count + 1000).max(INITIAL_CAPACITY);

        let hnsw = Hnsw::<f32, DistCosine>::new(
            M,
            capacity,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine {},
        );

        let mut id_map = HashMap::with_capacity(count);
        let mut rev_map = HashMap::with_capacity(count);
        let mut vectors = HashMap::with_capacity(count);
        let mut max_id = 0usize;

        for entry in &entries {
            let uuid: Uuid = entry.uuid.parse().unwrap_or_else(|_| Uuid::new_v4());
            id_map.insert(uuid, entry.id);
            rev_map.insert(entry.id, uuid);
            vectors.insert(entry.id, entry.vector.clone());
            max_id = max_id.max(entry.id);
        }

        // Insert all vectors into the HNSW graph.
        for entry in &entries {
            if let Some(vec) = vectors.get(&entry.id) {
                hnsw.insert((vec.as_slice(), entry.id));
            }
        }

        tracing::info!(count, "HnswStore loaded from disk");
        Ok(Self {
            hnsw,
            meta: Mutex::new(Meta {
                id_map,
                rev_map,
                vectors,
                next_id: max_id + 1,
            }),
            persist_path,
            dim: _dim,
        })
    }

    fn save(&self, meta: &Meta) -> Result<(), EverEvoError> {
        let entries: Vec<PersistedEntry> = meta
            .rev_map
            .iter()
            .map(|(id, uuid)| PersistedEntry {
                id: *id,
                uuid: uuid.to_string(),
                vector: meta.vectors.get(id).cloned().unwrap_or_default(),
            })
            .collect();
        let json = serde_json::to_string_pretty(&entries).map_err(|e| {
            EverEvoError::Internal(format!("Serialize HNSW store: {e}"))
        })?;
        std::fs::write(&self.persist_path, &json).map_err(|e| {
            EverEvoError::Internal(format!("Write HNSW store: {e}"))
        })?;
        Ok(())
    }
}

impl VectorStore for HnswStore {
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        let mut meta = self.meta.lock().map_err(|e| {
            EverEvoError::Internal(format!("Lock HNSW meta: {e}"))
        })?;

        for chunk in &chunks {
            let id = meta.next_id;
            meta.next_id += 1;
            // HNSW insert takes (&[T], DataId) via &self (uses internal lock).
            self.hnsw.insert((chunk.vector.as_slice(), id));
            meta.id_map.insert(chunk.id, id);
            meta.rev_map.insert(id, chunk.id);
            meta.vectors.insert(id, chunk.vector.clone());
        }

        self.save(&meta)
    }

    fn search(
        &self,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let ef = (top_k * 2).clamp(50, 512);
        let neighbors = self.hnsw.search(query_vector, top_k, ef);

        let meta = self.meta.lock().map_err(|e| {
            EverEvoError::Internal(format!("Lock HNSW meta: {e}"))
        })?;

        let results: Vec<ScoredChunk> = neighbors
            .iter()
            .filter_map(|n| {
                let id = n.d_id;
                let uuid = *meta.rev_map.get(&id)?;
                let vector = meta.vectors.get(&id)?;
                Some(ScoredChunk {
                    chunk: MemoryChunk {
                        id: uuid,
                        content: String::new(),
                        vector: vector.clone(),
                        source_pointers: vec![],
                        projection: ProjectionMetadata::new(
                            "2.0.0", "hnsw", vec![], 1.0,
                        ),
                        chunk_type: ChunkType::Fact,
                        created_at: chrono::Utc::now(),
                        retrieval_count: 0,
                    },
                    score: 1.0 - n.distance,
                })
            })
            .collect();
        Ok(results)
    }

    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        let mut meta = self.meta.lock().map_err(|e| {
            EverEvoError::Internal(format!("Lock HNSW meta: {e}"))
        })?;

        for uuid in ids {
            if let Some(internal_id) = meta.id_map.remove(uuid) {
                meta.rev_map.remove(&internal_id);
                meta.vectors.remove(&internal_id);
            }
        }
        self.save(&meta)
    }

    fn count(&self) -> usize {
        self.meta.lock().map(|m| m.id_map.len()).unwrap_or(0)
    }

    fn get(&self, id: &Uuid) -> Option<MemoryChunk> {
        let meta = self.meta.lock().ok()?;
        let internal_id = meta.id_map.get(id)?;
        let vector = meta.vectors.get(internal_id)?;
        Some(MemoryChunk {
            id: *id,
            content: String::new(),
            vector: vector.clone(),
            source_pointers: vec![],
            projection: ProjectionMetadata::new("2.0.0", "hnsw", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        })
    }
}

// ── Persistence ──────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedEntry {
    id: DataId,
    uuid: String,
    vector: Vec<f32>,
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_insert_and_search() {
        let dir = TempDir::new().unwrap();
        let store = HnswStore::open(dir.path().join("test"), 4).unwrap();
        assert_eq!(store.count(), 0);

        let c1 = make_chunk(Uuid::new_v4(), vec![1.0, 0.0, 0.0, 0.0]);
        let c2 = make_chunk(Uuid::new_v4(), vec![0.0, 1.0, 0.0, 0.0]);
        store.insert(vec![c1, c2]).unwrap();
        assert_eq!(store.count(), 2);

        let results = store.search(&[0.9, 0.1, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].score > 0.9);
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("store");

        let id = Uuid::new_v4();
        {
            let store = HnswStore::open(&path, 3).unwrap();
            store.insert(vec![make_chunk(id, vec![1.0, 0.0, 0.0])]).unwrap();
        }
        let store2 = HnswStore::open(&path, 3).unwrap();
        assert_eq!(store2.count(), 1);
        assert!(store2.get(&id).is_some());
    }

    #[test]
    fn test_delete() {
        let dir = TempDir::new().unwrap();
        let store = HnswStore::open(dir.path().join("del"), 3).unwrap();
        let id = Uuid::new_v4();
        store.insert(vec![make_chunk(id, vec![1.0, 0.0, 0.0])]).unwrap();
        assert_eq!(store.count(), 1);
        store.delete(&[id]).unwrap();
        assert_eq!(store.count(), 0);
    }
}
