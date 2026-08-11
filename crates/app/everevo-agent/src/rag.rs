//! RAG pipeline — ONNX embedder + dim-aware multi-collection vector store.
//!
//! Uses `ModelRegistry` to auto-detect available embedding models and their
//! dimensions. Collections are named `{namespace}-{dim}.bin`.
//!
//! ## Vector storage layout (co-located with source data)
//!
//! ```text
//! data/memory/vector/   ← memory-{dim}.bin, wiki-{dim}.bin, code-{dim}.bin
//! data/domain/vector/   ← domain-{dim}.bin
//! ```
//!
//! Memory-related vectors live alongside facts/diary/persona under `data/memory/`.
//! Domain vectors live under `data/domain/` alongside ingested documents.
//! This follows the industry pattern (ChromaDB, LlamaIndex, Milvus) of co-locating
//! vector indexes with their source data rather than in a flat `data/vector/` dump.

use std::path::Path;

use everevo_core::EverEvoError;
#[allow(unused_imports)]
use everevo_vector::{
    ChunkType, DummyEmbedder, EmbeddingModel, ModelRegistry, MultiCollectionStore, OnnxEmbedder,
    RawChunk, ScoredChunk,
};

/// Collections that belong under `data/memory/vector/`.
const MEMORY_COLLECTIONS: &[&str] = &["memory", "wiki", "code"];
/// Collections that belong under `data/domain/vector/`.
const DOMAIN_COLLECTIONS: &[&str] = &["domain"];

/// The RAG pipeline — embedder + dim-aware vector collections.
pub struct RagPipeline {
    /// Text → vector embedding model.
    embedder: Box<dyn EmbeddingModel>,
    /// Memory-related collections (facts, wiki, code).
    pub memory_store: MultiCollectionStore,
    /// Domain-related collections (ingested documents).
    pub domain_store: MultiCollectionStore,
    /// Whether real (ONNX) embeddings are in use.
    pub real_embeddings: bool,
    /// Active model name.
    pub model_name: String,
    /// Active embedding dimension.
    pub dim: usize,
}

impl RagPipeline {
    /// Create a RAG pipeline using the active model from the registry.
    pub fn new(data_dir: &Path, registry: &ModelRegistry) -> Result<Self, EverEvoError> {
        let active = registry
            .active()
            .ok_or_else(|| EverEvoError::Config("No active embedding model available".into()))?;
        let memory_vector_dir = data_dir.join("memory").join("vector");
        let domain_vector_dir = data_dir.join("domain").join("vector");
        let old_vector_dir = data_dir.join("vector");
        let old_json = data_dir.join("memory").join("vector").join("chunks.json");

        let models_dir = data_dir.join("models");
        let (embedder, real): (Box<dyn EmbeddingModel>, bool) =
            if let Ok(onnx) = OnnxEmbedder::new(&active.name, &models_dir) {
                if onnx.is_loaded() {
                    (Box::new(onnx), true)
                } else {
                    (Box::new(DummyEmbedder::new(active.dim)), false)
                }
            } else {
                (Box::new(DummyEmbedder::new(active.dim)), false)
            };

        // Migrate old `data/vector/*.bin` → new layout on first start
        Self::migrate_old_vectors(
            &old_vector_dir,
            &memory_vector_dir,
            &domain_vector_dir,
            active.dim,
        );

        let memory_store =
            MultiCollectionStore::open(&memory_vector_dir, active.dim, Some(&old_json))?;
        let domain_store = MultiCollectionStore::open(&domain_vector_dir, active.dim, None)?;

        tracing::info!(
            model = %active.name,
            dim = active.dim,
            real_embeddings = real,
            memory_dir = %memory_vector_dir.display(),
            domain_dir = %domain_vector_dir.display(),
            "RagPipeline initialized"
        );

        Ok(Self {
            embedder,
            memory_store,
            domain_store,
            real_embeddings: real,
            model_name: active.name.clone(),
            dim: active.dim,
        })
    }

    /// Migrate old `data/vector/{name}-{dim}.bin` files to the new co-located
    /// layout. One-time operation — skips if old dir doesn't exist.
    fn migrate_old_vectors(old_dir: &Path, memory_dir: &Path, domain_dir: &Path, dim: usize) {
        if !old_dir.exists() {
            return;
        }
        std::fs::create_dir_all(memory_dir).ok();
        std::fs::create_dir_all(domain_dir).ok();

        let mut moved = 0u32;
        for &collection in MEMORY_COLLECTIONS {
            let old_file = old_dir.join(format!("{}-{}.bin", collection, dim));
            let new_file = memory_dir.join(format!("{}-{}.bin", collection, dim));
            if old_file.exists()
                && !new_file.exists()
                && std::fs::rename(&old_file, &new_file).is_ok()
            {
                moved += 1;
            }
        }
        for &collection in DOMAIN_COLLECTIONS {
            let old_file = old_dir.join(format!("{}-{}.bin", collection, dim));
            let new_file = domain_dir.join(format!("{}-{}.bin", collection, dim));
            if old_file.exists()
                && !new_file.exists()
                && std::fs::rename(&old_file, &new_file).is_ok()
            {
                moved += 1;
            }
        }
        if moved > 0 {
            tracing::info!(
                moved,
                from = %old_dir.display(),
                "Migrated vector files to new co-located layout"
            );
        }
    }

    /// Re-create the pipeline with a new active model.
    pub fn reload(
        &mut self,
        data_dir: &Path,
        registry: &ModelRegistry,
    ) -> Result<(), EverEvoError> {
        let active = registry
            .active()
            .ok_or_else(|| EverEvoError::Config("No active embedding model available".into()))?;
        let models_dir = data_dir.join("models");
        let memory_vector_dir = data_dir.join("memory").join("vector");
        let domain_vector_dir = data_dir.join("domain").join("vector");

        let (embedder, real): (Box<dyn EmbeddingModel>, bool) =
            if let Ok(onnx) = OnnxEmbedder::new(&active.name, &models_dir) {
                if onnx.is_loaded() {
                    (Box::new(onnx), true)
                } else {
                    (Box::new(DummyEmbedder::new(active.dim)), false)
                }
            } else {
                (Box::new(DummyEmbedder::new(active.dim)), false)
            };

        self.memory_store = MultiCollectionStore::open(&memory_vector_dir, active.dim, None)?;
        self.domain_store = MultiCollectionStore::open(&domain_vector_dir, active.dim, None)?;
        self.embedder = embedder;
        self.real_embeddings = real;
        self.model_name = active.name.clone();
        self.dim = active.dim;

        tracing::info!(model = %active.name, dim = active.dim, "RagPipeline reloaded");
        Ok(())
    }

    /// Route a collection name to the correct store.
    fn store_for(&self, collection: &str) -> &MultiCollectionStore {
        if DOMAIN_COLLECTIONS.contains(&collection) {
            &self.domain_store
        } else {
            &self.memory_store
        }
    }

    /// Embed and ingest into a specific collection.
    pub fn ingest_into(&self, collection: &str, chunks: Vec<RawChunk>) -> Result<(), EverEvoError> {
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let vectors = self.embedder.encode_batch(&texts)?;
        let memory_chunks: Vec<everevo_vector::MemoryChunk> = chunks
            .into_iter()
            .zip(vectors)
            .map(|(raw, vector)| everevo_vector::MemoryChunk {
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
        self.store_for(collection).insert(collection, memory_chunks)
    }

    /// Semantic search within a single collection.
    pub fn search_in(
        &self,
        collection: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let query_vector = self.embedder.encode(query)?;
        self.store_for(collection)
            .search(collection, &query_vector, top_k)
    }

    /// Cross-collection search with RRF fusion.
    /// NOTE: Cross-store RRF is not supported; all collections must be in the same store.
    /// Falls back to searching the memory store (which has most collections).
    pub fn search_multi(
        &self,
        collections: &[&str],
        query: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let query_vector = self.embedder.encode(query)?;
        // Search memory store (holds memory + wiki + code)
        let memory_cols: Vec<&str> = collections
            .iter()
            .filter(|c| MEMORY_COLLECTIONS.contains(c))
            .copied()
            .collect();
        let domain_cols: Vec<&str> = collections
            .iter()
            .filter(|c| DOMAIN_COLLECTIONS.contains(c))
            .copied()
            .collect();

        let mut all_results: Vec<ScoredChunk> = Vec::new();
        if !memory_cols.is_empty() {
            if let Ok(r) = self
                .memory_store
                .search_multi(&memory_cols, &query_vector, top_k)
            {
                all_results.extend(r);
            }
        }
        if !domain_cols.is_empty() {
            if let Ok(r) = self
                .domain_store
                .search_multi(&domain_cols, &query_vector, top_k)
            {
                all_results.extend(r);
            }
        }
        if all_results.is_empty() && !collections.is_empty() {
            // Fallback: single collection search in whichever store
            return self.search_in(collections[0], query, top_k);
        }
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(top_k);
        Ok(all_results)
    }

    /// Encode a query to vector for hybrid search RRF.
    pub fn encode_query(&self, query: &str) -> Result<Vec<f32>, EverEvoError> {
        self.embedder.encode(query)
    }

    /// Total vector count across all stores.
    pub fn total_count(&self) -> usize {
        self.memory_store.total_count() + self.domain_store.total_count()
    }
}

// ── Chunk constructors (re-exported from everevo-vector) ─────────────────

pub use everevo_vector::{make_chunk, make_chunk_with_sources};

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_vector::ModelRegistry;
    use tempfile::TempDir;

    fn setup_registry(dir: &TempDir) -> ModelRegistry {
        let md = dir.path().join("test-model");
        std::fs::create_dir_all(&md).unwrap();
        std::fs::write(md.join("model_quantized.onnx"), b"fake").unwrap();
        std::fs::write(
            md.join("config.json"),
            r#"{"hidden_size": 384, "_name_or_path": "test"}"#,
        )
        .unwrap();
        ModelRegistry::discover(dir.path(), None).unwrap()
    }

    #[test]
    fn test_rag_pipeline_create_and_ingest() {
        let dir = TempDir::new().unwrap();
        let reg = setup_registry(&dir);
        let rag = RagPipeline::new(dir.path(), &reg).unwrap();
        assert_eq!(rag.total_count(), 0);
        let chunk = make_chunk("Hello world".into(), ChunkType::Fact);
        rag.ingest_into("memory", vec![chunk]).unwrap();
        assert!(rag.total_count() > 0);
    }

    #[test]
    fn test_rag_pipeline_fallback_when_no_models() {
        let dir = TempDir::new().unwrap();
        let reg = setup_registry(&dir);
        let rag = RagPipeline::new(dir.path(), &reg).unwrap();
        assert!(!rag.real_embeddings); // .onnx is fake → falls back to Dummy
    }
}
