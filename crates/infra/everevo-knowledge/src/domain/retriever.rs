//! Domain retriever — keyword + vector hybrid search across all domain documents.
//!
//! Implements the unified `Retriever` trait from everevo_core.
//! Supports three search strategies:
//! - **Keyword**: filename + content grep (always available)
//! - **Vector**: ONNX embedding → HNSW ANN search (when embedder + store available)
//! - **Hybrid**: RRF fusion of keyword + vector (best quality, per BEIR/vstash)

use std::path::PathBuf;
use std::sync::Arc;

use super::manager::DomainManager;
use everevo_core::retrieval::{HybridFusion, SearchResult};
use everevo_vector::{EmbeddingModel, VectorStore};

/// A retriever that searches across all domain documents.
///
/// ## Search strategies (auto-selected based on available backends)
///
/// - **Keyword-only**: when no embedder/store, searches filenames + file content
///   via substring matching.
/// - **Vector**: when embedder + store are available, adds semantic search via
///   query embedding → HNSW ANN.
/// - **Hybrid**: when both are available, uses RRF (k=60) to fuse keyword + vector
///   results, which produces the best NDCG@10 per BEIR benchmarks.
pub struct DomainRetriever {
    domain_root: PathBuf,
    /// Optional embedder for vector search.
    embedder: Option<Arc<dyn EmbeddingModel>>,
    /// Optional vector store for ANN search.
    vector_store: Option<Arc<dyn VectorStore>>,
}

impl DomainRetriever {
    /// Create a keyword-only retriever (no vector backend).
    pub fn new(domain_root: impl Into<PathBuf>) -> Self {
        Self {
            domain_root: domain_root.into(),
            embedder: None,
            vector_store: None,
        }
    }

    /// Create a retriever with vector search support.
    pub fn with_vector(
        domain_root: impl Into<PathBuf>,
        embedder: Arc<dyn EmbeddingModel>,
        vector_store: Arc<dyn VectorStore>,
    ) -> Self {
        Self {
            domain_root: domain_root.into(),
            embedder: Some(embedder),
            vector_store: Some(vector_store),
        }
    }

    /// Check whether vector search is available.
    pub fn has_vector_search(&self) -> bool {
        self.embedder.is_some() && self.vector_store.is_some()
    }

    // ── Keyword Search (filename + content) ──────────────────────────────

    fn search_keyword(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        let mut results = Vec::new();
        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

        let Ok(mgr) = DomainManager::load(&self.domain_root) else {
            return results;
        };

        for (id, domain) in &mgr.registry.domains {
            if domain.merged_into.is_some() {
                continue;
            }
            let Ok(docs) = mgr.list_documents(id) else {
                continue;
            };
            let doc_dir = self.domain_root.join(id).join("documents");

            for doc in &docs {
                let mut score = 0.0_f32;
                let mut matched = false;

                // Exact filename match → high score
                if doc.filename.to_lowercase().contains(&query_lower) {
                    score = 0.9;
                    matched = true;
                }
                // Partial word match on filename
                else if query_terms
                    .iter()
                    .any(|w| doc.filename.to_lowercase().contains(w) && w.len() > 2)
                {
                    score = 0.6;
                    matched = true;
                }
                // Content search: read file and grep
                else {
                    let file_path = doc_dir.join(&doc.filename);
                    if let Ok(content) = std::fs::read_to_string(&file_path) {
                        let content_lower = content.to_lowercase();
                        // TF-like scoring: count occurrences
                        let hits = query_terms
                            .iter()
                            .filter(|t| t.len() > 1)
                            .filter(|t| content_lower.contains(*t))
                            .count();
                        if hits > 0 {
                            let hit_ratio = hits as f32 / query_terms.len().max(1) as f32;
                            score = 0.3 + 0.4 * hit_ratio; // 0.3 – 0.7 range
                            matched = true;
                        }
                    }
                }

                if matched {
                    // Read snippet for content-matched results
                    let snippet = if score < 0.7 {
                        let file_path = doc_dir.join(&doc.filename);
                        std::fs::read_to_string(&file_path)
                            .unwrap_or_default()
                            .chars()
                            .take(200)
                            .collect()
                    } else {
                        format!(
                            "[{}] {} | {}B | {}",
                            domain.name,
                            doc.filename,
                            doc.size_bytes,
                            doc.modified.format("%Y-%m-%d")
                        )
                    };

                    results.push(SearchResult {
                        id: format!("{}/{}", id, doc.filename),
                        label: format!("[{}] {}", domain.name, doc.filename),
                        snippet,
                        score,
                        source: "domain-keyword".into(),
                        metadata: serde_json::json!({
                            "domain_id": id,
                            "domain_name": domain.name,
                            "filename": doc.filename,
                            "size_bytes": doc.size_bytes,
                        }),
                    });
                }
            }
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        results
    }

    // ── Vector Search ────────────────────────────────────────────────────

    fn search_vector(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => return vec![],
        };
        let store = match &self.vector_store {
            Some(s) => s,
            None => return vec![],
        };

        let qvec = match embedder.encode(query) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let scored = match store.search(&qvec, top_k) {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        scored
            .into_iter()
            .map(|s| SearchResult {
                id: s.chunk.id.to_string(),
                label: s.chunk.content.chars().take(80).collect(),
                snippet: s.chunk.content.chars().take(200).collect(),
                score: s.score.clamp(0.0, 1.0),
                source: "domain-vector".into(),
                metadata: serde_json::json!({
                    "chunk_type": s.chunk.chunk_type.as_str(),
                    "retrieval_count": s.chunk.retrieval_count,
                }),
            })
            .collect()
    }

    // ── Hybrid Search (RRF fusion) ───────────────────────────────────────

    fn search_hybrid(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        let keyword_results = self.search_keyword(query, top_k);
        let vector_results = self.search_vector(query, top_k);

        if vector_results.is_empty() {
            return keyword_results;
        }
        if keyword_results.is_empty() {
            return vector_results;
        }

        let fusion = HybridFusion::default(); // RRF k=60
        fusion.fuse_rrf(&[keyword_results, vector_results], top_k)
    }
}

impl everevo_core::retrieval::Retriever for DomainRetriever {
    fn name(&self) -> &str {
        "domain"
    }

    fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        if self.has_vector_search() {
            self.search_hybrid(query, top_k)
        } else {
            self.search_keyword(query, top_k)
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::ProjectionMetadata;
    use everevo_core::retrieval::Retriever;
    use everevo_vector::{ChunkType, DummyEmbedder, HnswStore, MemoryChunk};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn setup_domain_with_docs(dir: &TempDir, docs: &[(&str, &str)]) {
        let mut mgr = DomainManager::load(dir.path()).unwrap();
        // Create a domain
        mgr.registry
            .create("test-domain".into(), "Test".into(), "Test domain".into());
        let doc_dir = dir.path().join("test-domain").join("documents");
        std::fs::create_dir_all(&doc_dir).unwrap();

        for (filename, content) in docs {
            std::fs::write(doc_dir.join(filename), content).unwrap();
            mgr.registry
                .add_document("test-domain", &vec![0.1_f32; 384])
                .unwrap();
        }
        mgr.save().unwrap();
    }

    #[test]
    fn test_keyword_search_finds_by_filename() {
        let dir = TempDir::new().unwrap();
        setup_domain_with_docs(
            &dir,
            &[(
                "rust-guide.md",
                "# Rust\n\nRust is a systems programming language.",
            )],
        );
        let retriever = DomainRetriever::new(dir.path());
        let results = retriever.search("rust-guide", 10);
        assert!(!results.is_empty(), "Should find by filename");
    }

    #[test]
    fn test_keyword_search_finds_by_content() {
        let dir = TempDir::new().unwrap();
        setup_domain_with_docs(
            &dir,
            &[(
                "readme.md",
                "# Getting Started\n\nThis guide covers async programming with Tokio.",
            )],
        );
        let retriever = DomainRetriever::new(dir.path());
        // Search for a term that's only in content, not filename
        let results = retriever.search("Tokio", 10);
        assert!(
            !results.is_empty(),
            "Content search should find 'Tokio' in document body"
        );
    }

    #[test]
    fn test_keyword_search_no_match() {
        let dir = TempDir::new().unwrap();
        setup_domain_with_docs(&dir, &[("notes.md", "Hello world")]);
        let retriever = DomainRetriever::new(dir.path());
        let results = retriever.search("nonexistent-xyz", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_vector_search_available() {
        let dir = TempDir::new().unwrap();
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(DummyEmbedder::new(16));
        let store = HnswStore::open(dir.path().join("vec-store"), 16).unwrap();
        let store: Arc<dyn VectorStore> = Arc::new(store);

        // Insert a chunk
        store
            .insert(vec![MemoryChunk {
                id: Uuid::new_v4(),
                content: "Rust async programming guide".into(),
                vector: vec![0.5_f32; 16],
                source_pointers: vec![],
                projection: ProjectionMetadata::new("test", "test", vec![], 1.0),
                chunk_type: ChunkType::Fact,
                created_at: chrono::Utc::now(),
                retrieval_count: 0,
            }])
            .unwrap();

        let retriever = DomainRetriever::with_vector(dir.path(), embedder, store);
        assert!(retriever.has_vector_search());
        // Hybrid search should work (keyword might miss but vector should find something)
        let results = retriever.search("async programming", 5);
        // DummyEmbedder returns all zeros → all distances are equal → results appear
        assert!(!results.is_empty(), "Hybrid search should return results");
    }

    #[test]
    fn test_retriever_trait_implementation() {
        let dir = TempDir::new().unwrap();
        setup_domain_with_docs(&dir, &[("guide.md", "Test content")]);
        let retriever = DomainRetriever::new(dir.path());
        assert_eq!(retriever.name(), "domain");
        assert!(retriever.available());
    }
}
