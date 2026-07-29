//! HNSW vector store — pure Rust ANN index with cosine distance.
//!
//! ## Storage format
//!
//! Single file per store: `{name}.bin` — bincode of `Vec<VectorEntry>`.
//! Each entry: `{ uuid: u128, data_id: u64, vector: Vec<f32> }`.
//!
//! At dim=384, each entry is ~1.55KB (vs ~4.3KB JSON). The HNSW graph is
//! rebuilt on load from the vector data (safe, no lifetimes, no unsafe).
//!
//! Old JSON format (`{name}.json`) is auto-migrated on first open, then deleted.
//!
//! ## Thread safety
//!
//! `hnsw_rs::Hnsw` uses internal `parking_lot` locks. Metadata uses `std::sync::Mutex`.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::Mutex;

use hnsw_rs::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store_trait::VectorStore;
use super::types::{ChunkType, MemoryChunk, ScoredChunk};
use everevo_core::memory::ProjectionMetadata;
use everevo_core::EverEvoError;

const M: usize = 32;
const MAX_LAYER: usize = 16;
const EF_CONSTRUCTION: usize = 200;
const INITIAL_CAPACITY: usize = 100_000;

struct Meta {
    id_map: HashMap<Uuid, DataId>,
    rev_map: HashMap<DataId, Uuid>,
    /// Track vectors by DataId for persistence round-trip.
    vectors: HashMap<DataId, Vec<f32>>,
    next_id: DataId,
}

/// Disk-backed HNSW vector store with bincode persistence.
pub struct HnswStore {
    hnsw: Hnsw<'static, f32, DistCosine>,
    meta: Mutex<Meta>,
    /// Path to the bincode file ({name}.bin).
    data_path: PathBuf,
}

impl HnswStore {
    /// Open or create a store. Loads from `base_path.bin` if it exists.
    /// Falls back to old JSON migration if `base_path.json` exists.
    pub fn open(base_path: impl Into<PathBuf>, dim: usize) -> Result<Self, EverEvoError> {
        let base: PathBuf = base_path.into();
        let bin_path = base.with_extension("bin");
        let json_path = base.with_extension("json");

        if bin_path.exists() {
            Self::load(&bin_path, dim)
        } else if json_path.exists() {
            Self::migrate_from_json(&json_path, &bin_path, dim)
        } else {
            Ok(Self::empty(bin_path, dim))
        }
    }

    fn empty(data_path: PathBuf, dim: usize) -> Self {
        let hnsw = Hnsw::<f32, DistCosine>::new(
            M, INITIAL_CAPACITY, MAX_LAYER, EF_CONSTRUCTION, DistCosine {},
        );
        if let Some(parent) = data_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        tracing::info!(dim, "HnswStore created (empty)");
        Self { hnsw, meta: Mutex::new(Meta { id_map: HashMap::new(), rev_map: HashMap::new(), vectors: HashMap::new(), next_id: 0 }), data_path }
    }

    fn load(bin_path: &std::path::Path, _dim: usize) -> Result<Self, EverEvoError> {
        let file = std::fs::File::open(bin_path).map_err(|e| {
            EverEvoError::Internal(format!("Open {}: {e}", bin_path.display()))
        })?;
        let reader = BufReader::new(file);
        let entries: Vec<VectorEntry> = bincode::deserialize_from(reader)
            .map_err(|e| EverEvoError::Internal(format!("Load {}: {e}", bin_path.display())))?;

        let count = entries.len();
        let capacity = (count + 1000).max(INITIAL_CAPACITY);
        let hnsw = Hnsw::<f32, DistCosine>::new(M, capacity, MAX_LAYER, EF_CONSTRUCTION, DistCosine {});

        let mut id_map = HashMap::with_capacity(count);
        let mut rev_map = HashMap::with_capacity(count);
        let mut vectors = HashMap::with_capacity(count);
        let mut max_id = 0usize;

        for entry in &entries {
            let uuid = Uuid::from_u128(entry.uuid);
            id_map.insert(uuid, entry.data_id);
            rev_map.insert(entry.data_id, uuid);
            vectors.insert(entry.data_id, entry.vector.clone());
            max_id = max_id.max(entry.data_id);
            hnsw.insert((entry.vector.as_slice(), entry.data_id));
        }

        tracing::info!(count, path = %bin_path.display(), "HnswStore loaded (bincode)");
        Ok(Self {
            hnsw,
            meta: Mutex::new(Meta { id_map, rev_map, vectors, next_id: max_id + 1 }),
            data_path: bin_path.to_path_buf(),
        })
    }

    fn migrate_from_json(json_path: &std::path::Path, bin_path: &std::path::Path, _dim: usize) -> Result<Self, EverEvoError> {
        let json = std::fs::read_to_string(json_path).map_err(|e| {
            EverEvoError::Internal(format!("Read JSON: {e}"))
        })?;
        let entries: Vec<JsonEntry> = serde_json::from_str(&json).unwrap_or_default();
        let count = entries.len();
        let capacity = (count + 1000).max(INITIAL_CAPACITY);
        let hnsw = Hnsw::<f32, DistCosine>::new(M, capacity, MAX_LAYER, EF_CONSTRUCTION, DistCosine {});

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
            hnsw.insert((entry.vector.as_slice(), entry.id));
        }

        let store = Self {
            hnsw,
            meta: Mutex::new(Meta { id_map, rev_map, vectors, next_id: max_id + 1 }),
            data_path: bin_path.to_path_buf(),
        };
        store.save()?;
        let _ = std::fs::remove_file(json_path);
        tracing::info!(count, "HnswStore migrated JSON → bincode");
        Ok(store)
    }

    fn save(&self) -> Result<(), EverEvoError> {
        let meta = self.meta.lock().map_err(|e| {
            EverEvoError::Internal(format!("Lock meta: {e}"))
        })?;

        // Collect entries: for each DataId, find its UUID.
        // We don't store the vector here because HNSW owns the vectors internally.
        // For full reconstruction, we'd need to store vectors too. But since we
        // only need UUID→DataId mapping on load, we store minimal data.
        //
        // Actually, we need vectors for reconstruction on load. HNSW doesn't
        // expose vectors by DataId. So we use the shadow-map approach:
        // store (uuid, data_id, vector) for all entries.
        //
        // We track vectors on insert via a secondary store.
        // For now, store the mapping only — full vector persistence requires
        // tracking vectors separately (see `vectors` field in Meta).

        // Reconstruct from id_map + vectors stored in hnsw.
        // Since hnsw doesn't expose vectors by data_id, we must track them.
        // For production: add a `Vec<VectorEntry>` on save, rebuilt from meta + a
        // separate vector store. Currently, `get()` returns placeholder vectors
        // and search works via HNSW internal vectors, so persistence works for
        // search but not for `get()` returning full vector data.

        let entries: Vec<VectorEntry> = meta.rev_map.iter().map(|(&data_id, &uuid)| {
            VectorEntry {
                uuid: uuid.as_u128(),
                data_id,
                vector: meta.vectors.get(&data_id).cloned().unwrap_or_default(),
            }
        }).collect();

        let file = std::fs::File::create(&self.data_path).map_err(|e| {
            EverEvoError::Internal(format!("Create {}: {e}", self.data_path.display()))
        })?;
        let writer = BufWriter::new(file);
        bincode::serialize_into(writer, &entries).map_err(|e| {
            EverEvoError::Internal(format!("Serialize {}: {e}", self.data_path.display()))
        })?;
        Ok(())
    }
}

impl VectorStore for HnswStore {
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        if chunks.is_empty() { return Ok(()); }
        {
            let mut meta = self.meta.lock().map_err(|e| EverEvoError::Internal(format!("Lock meta: {e}")))?;
            for chunk in &chunks {
                let id = meta.next_id;
                meta.next_id += 1;
                self.hnsw.insert((chunk.vector.as_slice(), id));
                meta.id_map.insert(chunk.id, id);
                meta.rev_map.insert(id, chunk.id);
                meta.vectors.insert(id, chunk.vector.clone());
            }
        }
        self.save()
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let ef = (top_k * 2).clamp(50, 512);
        let neighbors = self.hnsw.search(query_vector, top_k, ef);
        let meta = self.meta.lock().map_err(|e| EverEvoError::Internal(format!("Lock meta: {e}")))?;
        Ok(neighbors.iter().filter_map(|n| {
            let uuid = *meta.rev_map.get(&n.d_id)?;
            Some(ScoredChunk {
                chunk: MemoryChunk {
                    id: uuid, content: String::new(), vector: vec![],
                    source_pointers: vec![],
                    projection: ProjectionMetadata::new("2.0.0", "hnsw", vec![], 1.0),
                    chunk_type: ChunkType::Fact,
                    created_at: chrono::Utc::now(), retrieval_count: 0,
                },
                score: 1.0 - n.distance,
            })
        }).collect())
    }

    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        let mut meta = self.meta.lock().map_err(|e| EverEvoError::Internal(format!("Lock meta: {e}")))?;
        for uuid in ids {
            if let Some(internal_id) = meta.id_map.remove(uuid) {
                meta.rev_map.remove(&internal_id);
                meta.vectors.remove(&internal_id);
            }
        }
        drop(meta);
        self.save()
    }

    fn count(&self) -> usize { self.meta.lock().map(|m| m.id_map.len()).unwrap_or(0) }

    fn get(&self, id: &Uuid) -> Option<MemoryChunk> {
        let meta = self.meta.lock().ok()?;
        let _internal_id = meta.id_map.get(id)?;
        Some(MemoryChunk {
            id: *id, content: String::new(), vector: vec![],
            source_pointers: vec![],
            projection: ProjectionMetadata::new("2.0.0", "hnsw", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(), retrieval_count: 0,
        })
    }
}

// ── Serialization types ─────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct VectorEntry {
    uuid: u128,
    data_id: usize,
    vector: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
struct JsonEntry {
    id: usize,
    uuid: String,
    vector: Vec<f32>,
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_chunk(id: Uuid, v: Vec<f32>) -> MemoryChunk {
        MemoryChunk { id, content: String::new(), vector: v,
            source_pointers: vec![],
            projection: ProjectionMetadata::new("test", "test", vec![], 1.0),
            chunk_type: ChunkType::Fact, created_at: chrono::Utc::now(), retrieval_count: 0,
        }
    }

    #[test]
    fn test_insert_and_search() {
        let dir = TempDir::new().unwrap();
        let store = HnswStore::open(dir.path().join("test-store"), 4).unwrap();
        store.insert(vec![
            make_chunk(Uuid::new_v4(), vec![1.0, 0.0, 0.0, 0.0]),
            make_chunk(Uuid::new_v4(), vec![0.0, 1.0, 0.0, 0.0]),
        ]).unwrap();
        assert_eq!(store.count(), 2);
        let r = store.search(&[0.9, 0.1, 0.0, 0.0], 2).unwrap();
        assert_eq!(r.len(), 2);
        assert!(r[0].score > 0.9);
    }

    #[test]
    fn test_bincode_persistence_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("store");
        let id = Uuid::new_v4();
        {
            let store = HnswStore::open(&path, 3).unwrap();
            store.insert(vec![make_chunk(id, vec![1.0, 0.0, 0.0])]).unwrap();
        }
        assert!(path.with_extension("bin").exists());
        let store2 = HnswStore::open(&path, 3).unwrap();
        assert_eq!(store2.count(), 1);
        assert!(store2.get(&id).is_some());
    }

    #[test]
    fn test_json_migration() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("store.json");
        let bin_path = dir.path().join("store.bin");
        let entries = vec![JsonEntry { id: 0, uuid: Uuid::new_v4().to_string(), vector: vec![1.0, 0.0, 0.0] }];
        std::fs::write(&json_path, serde_json::to_string(&entries).unwrap()).unwrap();
        let store = HnswStore::open(dir.path().join("store"), 3).unwrap();
        assert_eq!(store.count(), 1);
        assert!(!json_path.exists());
        assert!(bin_path.exists());
    }
}
