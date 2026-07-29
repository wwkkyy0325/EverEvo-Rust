//! RAG pipeline — ONNX embedder + dim-aware multi-collection vector store.
//!
//! Uses `ModelRegistry` to auto-detect available embedding models and their
//! dimensions. Collections are named `{namespace}-{dim}.bin` so switching
//! models creates new collections without overwriting old data.

use std::path::Path;

use everevo_core::EverEvoError;
#[allow(unused_imports)]
use everevo_vector::{
    ChunkType, DummyEmbedder, EmbeddingModel, ModelRegistry, MultiCollectionStore,
    OnnxEmbedder, RawChunk, ScoredChunk,
};

/// The RAG pipeline — embedder + dim-aware vector collections.
pub struct RagPipeline {
    /// Text → vector embedding model.
    embedder: Box<dyn EmbeddingModel>,
    /// Dim-aware namespaced HNSW collections.
    pub collections: MultiCollectionStore,
    /// Whether real (ONNX) embeddings are in use.
    pub real_embeddings: bool,
    /// Active model name.
    pub model_name: String,
    /// Active embedding dimension.
    pub dim: usize,
}

impl RagPipeline {
    /// Create a RAG pipeline using the active model from the registry.
    ///
    /// Vectors are stored under `data_dir/vector/{name}-{dim}.bin`.
    pub fn new(data_dir: &Path, registry: &ModelRegistry) -> Result<Self, EverEvoError> {
        let active = registry.active();
        let vector_dir = data_dir.join("vector");
        let old_path = data_dir.join("memory").join("vector").join("chunks.json");

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

        let collections = MultiCollectionStore::open(&vector_dir, active.dim, Some(&old_path))?;

        tracing::info!(
            model = %active.name,
            dim = active.dim,
            real_embeddings = real,
            "RagPipeline initialized"
        );

        Ok(Self {
            embedder,
            collections,
            real_embeddings: real,
            model_name: active.name.clone(),
            dim: active.dim,
        })
    }

    /// Re-create the pipeline with a new active model (after registry.activate()).
    pub fn reload(&mut self, data_dir: &Path, registry: &ModelRegistry) -> Result<(), EverEvoError> {
        let active = registry.active();
        let models_dir = data_dir.join("models");
        let vector_dir = data_dir.join("vector");

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

        self.collections = MultiCollectionStore::open(&vector_dir, active.dim, None)?;
        self.embedder = embedder;
        self.real_embeddings = real;
        self.model_name = active.name.clone();
        self.dim = active.dim;

        tracing::info!(model = %active.name, dim = active.dim, "RagPipeline reloaded");
        Ok(())
    }

    /// Embed and ingest into a specific collection.
    pub fn ingest_into(&self, collection: &str, chunks: Vec<RawChunk>) -> Result<(), EverEvoError> {
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let vectors = self.embedder.encode_batch(&texts)?;
        let memory_chunks: Vec<everevo_vector::MemoryChunk> = chunks
            .into_iter()
            .zip(vectors)
            .map(|(raw, vector)| everevo_vector::MemoryChunk {
                id: raw.id, content: raw.content, vector,
                source_pointers: raw.source_pointers,
                projection: raw.projection, chunk_type: raw.chunk_type,
                created_at: chrono::Utc::now(), retrieval_count: 0,
            })
            .collect();
        self.collections.insert(collection, memory_chunks)
    }

    /// Semantic search within a single collection.
    pub fn search_in(&self, collection: &str, query: &str, top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let query_vector = self.embedder.encode(query)?;
        self.collections.search(collection, &query_vector, top_k)
    }

    /// Cross-collection search with RRF fusion.
    pub fn search_multi(&self, collections: &[&str], query: &str, top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError> {
        let query_vector = self.embedder.encode(query)?;
        self.collections.search_multi(collections, &query_vector, top_k)
    }

    /// Encode a query to vector for hybrid search RRF.
    pub fn encode_query(&self, query: &str) -> Result<Vec<f32>, EverEvoError> {
        self.embedder.encode(query)
    }

    /// Total vector count.
    pub fn total_count(&self) -> usize {
        self.collections.total_count()
    }
}

// ── Chunk constructors (re-exported from everevo-vector) ─────────────────

pub use everevo_vector::{make_chunk, make_chunk_with_sources};

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use everevo_vector::ModelRegistry;

    fn setup_registry(dir: &TempDir) -> ModelRegistry {
        let md = dir.path().join("test-model");
        std::fs::create_dir_all(&md).unwrap();
        std::fs::write(md.join("model_quantized.onnx"), b"fake").unwrap();
        std::fs::write(md.join("config.json"), r#"{"hidden_size": 384, "_name_or_path": "test"}"#).unwrap();
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
