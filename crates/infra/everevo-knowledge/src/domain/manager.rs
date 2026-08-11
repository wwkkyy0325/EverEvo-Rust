//! Domain manager — high-level domain pipeline manager.
//! Wraps registry + classifier + watcher + optional embedder for real vectors.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::chunker::SemanticChunker;
use super::classifier::DomainClassifier;
use super::document::DocumentMeta;
use super::helpers::content_hash;
use super::parser::DocumentParser;
use super::registry::DomainRegistry;
use super::watcher::DomainWatcher;
use everevo_core::memory::ProjectionMetadata;
use everevo_core::EverEvoError;
use everevo_vector::{
    ChunkType as VectorChunkType, EmbeddingModel, MemoryChunk, RawChunk, VectorStore,
};

/// High-level domain pipeline manager.
pub struct DomainManager {
    pub registry: DomainRegistry,
    pub classifier: DomainClassifier,
    pub domain_root: PathBuf,
    /// Optional embedder for real vector generation.
    embedder: Option<Arc<dyn EmbeddingModel>>,
    /// Optional vector store for chunk storage.
    vector_store: Option<Arc<dyn VectorStore>>,
}

impl DomainManager {
    pub fn load(domain_root: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        Self::load_with_embedder(domain_root, None)
    }

    /// Load domain manager and auto-detect the ONNX embedder from `models_dir`.
    pub fn load_with_onnx(
        domain_root: impl Into<PathBuf>,
        models_dir: impl Into<PathBuf>,
    ) -> Result<Self, EverEvoError> {
        let models_dir: PathBuf = models_dir.into();
        let embedder: Option<Arc<dyn EmbeddingModel>> =
            everevo_vector::OnnxEmbedder::new("all-MiniLM-L6-v2", &models_dir)
                .ok()
                .filter(|e| e.is_loaded())
                .map(|e| Arc::new(e) as Arc<dyn EmbeddingModel>);
        if embedder.is_some() {
            tracing::info!("Domain manager loaded with ONNX embedder");
        }
        Self::load_with_embedder(domain_root, embedder)
    }

    pub fn load_with_embedder(
        domain_root: impl Into<PathBuf>,
        embedder: Option<Arc<dyn EmbeddingModel>>,
    ) -> Result<Self, EverEvoError> {
        let domain_root: PathBuf = domain_root.into();
        std::fs::create_dir_all(domain_root.join("inbox")).ok();
        let registry_path = domain_root.join("domains.json");
        let registry = DomainRegistry::load(&registry_path)?;
        Ok(Self {
            registry,
            classifier: DomainClassifier::default(),
            domain_root,
            embedder,
            vector_store: None,
        })
    }

    /// Attach a vector store for chunk persistence (LanceDB).
    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }

    /// Access the optional embedder for real vector generation.
    pub fn embedder(&self) -> Option<&Arc<dyn EmbeddingModel>> {
        self.embedder.as_ref()
    }

    pub fn save(&self) -> Result<(), EverEvoError> {
        self.registry.save(&self.domain_root.join("domains.json"))
    }

    /// List documents in a domain by scanning the documents/ directory.
    pub fn list_documents(&self, domain_id: &str) -> Result<Vec<DocumentMeta>, EverEvoError> {
        let doc_dir = self.domain_root.join(domain_id).join("documents");
        if !doc_dir.exists() {
            return Ok(vec![]);
        }
        let mut docs = Vec::new();
        for entry in std::fs::read_dir(&doc_dir)
            .map_err(|e| EverEvoError::Internal(format!("Read docs: {e}")))?
        {
            let entry = entry.map_err(|e| EverEvoError::Internal(format!("Entry: {e}")))?;
            let _path = entry.path();
            let meta = entry.metadata().ok();
            docs.push(DocumentMeta {
                filename: entry.file_name().to_string_lossy().to_string(),
                size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        chrono::DateTime::from_timestamp(
                            t.duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64,
                            0,
                        )
                        .unwrap_or_else(Utc::now)
                    })
                    .unwrap_or_else(Utc::now),
            });
        }
        Ok(docs)
    }

    /// Process the global inbox: classify + index all pending files.
    /// Returns a summary of what was done.
    pub async fn process_global_inbox(&mut self) -> Result<InboxResult, EverEvoError> {
        let inbox = self.domain_root.join("inbox");
        std::fs::create_dir_all(&inbox).ok();
        let reg_path = self.domain_root.join("domains.json");
        let mut watcher = DomainWatcher::new(&inbox, &reg_path)?;

        let files = watcher.scan()?;
        let mut result = InboxResult::default();

        for (filename, content) in &files {
            let text = DocumentParser::parse(filename, content).unwrap_or_default();
            let _hash = content_hash(&text);
            let chunker = SemanticChunker::default();
            let chunks = chunker.chunk(&text);

            // Generate real embedding if embedder is available
            let doc_vector: Vec<f32> = if let Some(ref emb) = self.embedder {
                emb.encode(&text)
                    .unwrap_or_else(|_| vec![0.1_f32; self.registry.embedding_dim])
            } else {
                vec![0.1_f32; self.registry.embedding_dim]
            };

            // Classify with real vector
            let classification =
                self.classifier
                    .classify(&self.registry, &doc_vector, text.len(), 1);

            let domain_id = if classification.is_new_domain {
                // Generate a slug from filename
                let slug = Path::new(filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_lowercase()
                    .replace(' ', "-");
                self.registry.create(
                    slug.clone(),
                    filename.clone(),
                    format!("Auto-created from {filename}"),
                );
                result.new_domains.push(slug.clone());
                self.save()?;
                slug
            } else {
                classification.domain_id.clone()
            };

            // Ensure domain exists
            if !self.registry.domains.contains_key(&domain_id) {
                self.registry
                    .create(domain_id.clone(), domain_id.clone(), String::new());
                self.save()?;
            }

            // Index document with real vector
            let _ = self.registry.add_document(&domain_id, &doc_vector);
            let doc_dir = self.domain_root.join(&domain_id).join("documents");
            std::fs::create_dir_all(&doc_dir).ok();
            let doc_id = Uuid::new_v4();
            std::fs::write(doc_dir.join(format!("{doc_id}.md")), content).ok();

            // Remove from inbox to prevent re-processing on next poll cycle.
            // The document now lives in the domain's documents/ directory.
            let original_path = inbox.join(filename);
            if let Err(e) = std::fs::remove_file(&original_path) {
                tracing::warn!(%filename, error = %e, "Failed to remove processed file from inbox");
            }

            // Store chunks in vector store if available
            if let (Some(ref emb), Some(ref store)) = (&self.embedder, &self.vector_store) {
                let raw_chunks: Vec<RawChunk> = chunks
                    .iter()
                    .map(|c| RawChunk {
                        id: uuid::Uuid::new_v4(),
                        content: c.clone(),
                        source_pointers: vec![],
                        projection: ProjectionMetadata::new("1.0", "domain", vec![], 1.0),
                        chunk_type: VectorChunkType::Fact,
                    })
                    .collect();
                // Embed and store (fire-and-forget best effort)
                if let Ok(vectors) = emb.encode_batch(
                    &raw_chunks
                        .iter()
                        .map(|c| c.content.clone())
                        .collect::<Vec<_>>(),
                ) {
                    let mchunks: Vec<MemoryChunk> = raw_chunks
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
                    let _ = store.insert(mchunks);
                }
            }

            result.processed += 1;
            tracing::info!(
                %filename,
                %domain_id,
                chunks = chunks.len(),
                "Global inbox: document classified"
            );
        }

        self.save()?;
        Ok(result)
    }

    /// Get domain coverage stats (for self-healing).
    pub fn coverage(&self) -> Vec<DomainCoverage> {
        self.registry
            .domains
            .iter()
            .filter(|(_, d)| d.merged_into.is_none())
            .map(|(id, d)| DomainCoverage {
                domain_id: id.clone(),
                name: d.name.clone(),
                document_count: d.document_count,
                has_relations: !d.related_ids.is_empty(),
                is_new: d.document_count < 3,
            })
            .collect()
    }
}

// ── Result Types ──────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct InboxResult {
    pub processed: usize,
    pub new_domains: Vec<String>,
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_vector::{DummyEmbedder, EmbeddingModel};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup_domain_root() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
        dir
    }

    #[test]
    fn test_process_empty_inbox() {
        let dir = setup_domain_root();
        // Empty inbox — scan should return nothing
        let inbox = dir.path().join("inbox");
        let reg_path = dir.path().join("domains.json");
        let mut watcher = DomainWatcher::new(&inbox, &reg_path).unwrap();
        let files = watcher.scan().unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_list_documents_empty_domain() {
        let dir = setup_domain_root();
        let mgr = DomainManager::load(dir.path()).unwrap();
        let docs = mgr.list_documents("nonexistent").unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn test_coverage_stats() {
        let dir = setup_domain_root();
        let mut mgr = DomainManager::load(dir.path()).unwrap();
        // Manually create a domain for coverage testing
        mgr.registry.create(
            "test-cov".into(),
            "Test Coverage".into(),
            "Coverage test".into(),
        );
        mgr.save().unwrap();

        let coverage = mgr.coverage();
        assert!(coverage.iter().any(|c| c.domain_id == "test-cov"));
        let cov = coverage.iter().find(|c| c.domain_id == "test-cov").unwrap();
        assert_eq!(cov.document_count, 0);
        assert!(cov.is_new); // < 3 docs
    }

    #[test]
    fn test_domain_manager_load_and_save() {
        let dir = setup_domain_root();
        {
            let mut mgr = DomainManager::load(dir.path()).unwrap();
            mgr.registry.create(
                "load-test".into(),
                "Load Test".into(),
                "Testing load".into(),
            );
            mgr.save().unwrap();
        }
        {
            let mgr = DomainManager::load(dir.path()).unwrap();
            assert!(mgr.registry.domains.contains_key("load-test"));
        }
    }

    #[test]
    fn test_domain_manager_with_dummy_embedder() {
        let dir = setup_domain_root();
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(DummyEmbedder::new(384));
        let mgr = DomainManager::load_with_embedder(dir.path(), Some(embedder)).unwrap();
        assert!(mgr.embedder().is_some());
    }

    #[test]
    fn test_domain_manager_without_embedder() {
        let dir = setup_domain_root();
        let mgr = DomainManager::load(dir.path()).unwrap();
        assert!(mgr.embedder().is_none());
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DomainCoverage {
    pub domain_id: String,
    pub name: String,
    pub document_count: usize,
    pub has_relations: bool,
    pub is_new: bool,
}
