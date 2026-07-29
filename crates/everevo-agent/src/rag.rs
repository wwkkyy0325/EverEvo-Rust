//! RAG pipeline — wraps everevo-vector's embedding + vector store.
//!
//! Provides high-level API for agent memory ingestion and semantic search.
//! Uses `OnnxEmbedder` when model files are available, falls back to `DummyEmbedder`.

use std::path::Path;

use everevo_core::memory::{ProjectionMetadata, SourcePointer};
use everevo_core::EverEvoError;
use everevo_vector::{
    ChunkType, DummyEmbedder, EmbeddingModel, HnswStore, OnnxEmbedder, RawChunk, ScoredChunk,
    VectorEngine, VectorStore,
};
use uuid::Uuid;

/// The RAG pipeline — combines an embedder and a vector store for ingestion
/// and semantic search.
///
/// Tries to load a real ONNX embedding model from `data/models/`.
/// Falls back to `DummyEmbedder` (zero vectors) if model files are unavailable.
pub struct RagPipeline {
    engine: VectorEngine<Box<dyn EmbeddingModel>, HnswStore>,
    /// Whether a real embedding model is loaded.
    pub real_embeddings: bool,
}

impl RagPipeline {
    /// Create a new RAG pipeline backed by the given data directory.
    ///
    /// Vectors are stored at `{data_dir}/memory/vector/`.
    /// Embedding models are loaded from `{data_dir}/models/`.
    pub fn new(data_dir: &Path) -> Result<Self, EverEvoError> {
        let store_dir = data_dir.join("memory").join("vector");
        std::fs::create_dir_all(&store_dir)
            .map_err(|e| EverEvoError::Internal(format!("Create vector dir: {e}")))?;

        let models_dir = data_dir.join("models");
        let (embedder, real): (Box<dyn EmbeddingModel>, bool) =
            if let Ok(onnx) = OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir) {
                if onnx.is_loaded() {
                    (Box::new(onnx), true)
                } else {
                    (Box::new(DummyEmbedder::new(384)), false)
                }
            } else {
                (Box::new(DummyEmbedder::new(384)), false)
            };

        let store = HnswStore::open(store_dir.join("chunks"), 384)?;
        let engine = VectorEngine::new(embedder, store);
        Ok(Self {
            engine,
            real_embeddings: real,
        })
    }

    /// Create a Chinese-optimized RAG pipeline (uses bge-small-zh model).
    pub fn new_zh(data_dir: &Path) -> Result<Self, EverEvoError> {
        let store_dir = data_dir.join("memory").join("vector");
        std::fs::create_dir_all(&store_dir)
            .map_err(|e| EverEvoError::Internal(format!("Create vector dir: {e}")))?;

        let models_dir = data_dir.join("models");
        let (embedder, real): (Box<dyn EmbeddingModel>, bool) =
            if let Ok(onnx) = OnnxEmbedder::new("bge-small-zh", &models_dir) {
                if onnx.is_loaded() {
                    (Box::new(onnx), true)
                } else {
                    (Box::new(DummyEmbedder::new(384)), false)
                }
            } else {
                (Box::new(DummyEmbedder::new(384)), false)
            };

        let store = HnswStore::open(store_dir.join("chunks"), 384)?;
        let engine = VectorEngine::new(embedder, store);
        Ok(Self {
            engine,
            real_embeddings: real,
        })
    }

    pub fn ingest(&self, chunks: Vec<RawChunk>) -> Result<(), EverEvoError> {
        self.engine.insert_texts(chunks)
    }

    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<ScoredChunk>, EverEvoError> {
        self.engine.search_text(query, top_k)
    }

    pub fn count(&self) -> usize {
        self.engine.store.count()
    }

    pub fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        self.engine.store.delete(ids)
    }
}

/// Convenience builder: create a [`RawChunk`] from agent data.
pub fn make_chunk(content: String, chunk_type: ChunkType, sources: Vec<SourcePointer>) -> RawChunk {
    RawChunk {
        id: Uuid::new_v4(),
        content,
        source_pointers: sources,
        projection: ProjectionMetadata::new(env!("CARGO_PKG_VERSION"), "agent", vec![], 0.5),
        chunk_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_rag_pipeline_create_and_ingest() {
        let dir = TempDir::new().unwrap();
        let rag = RagPipeline::new(dir.path()).unwrap();
        assert_eq!(rag.count(), 0);

        let chunk = make_chunk("Hello world".into(), ChunkType::Fact, vec![]);
        rag.ingest(vec![chunk]).unwrap();
        assert_eq!(rag.count(), 1);
    }

    #[test]
    fn test_rag_pipeline_fallback_when_no_models() {
        let dir = TempDir::new().unwrap();
        let rag = RagPipeline::new(dir.path()).unwrap();
        // Should still create successfully with DummyEmbedder fallback
        assert!(!rag.real_embeddings);
    }
}
