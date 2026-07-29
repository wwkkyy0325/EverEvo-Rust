//! Domain retriever — searches across all domain documents.
//! Implements the unified `Retriever` trait from everevo_core.

use std::path::PathBuf;

use super::manager::DomainManager;
use everevo_core::retrieval::SearchResult;

/// A retriever that searches across all domain documents.
pub struct DomainRetriever {
    domain_root: PathBuf,
}

impl DomainRetriever {
    pub fn new(domain_root: impl Into<PathBuf>) -> Self {
        Self {
            domain_root: domain_root.into(),
        }
    }

    /// Search all domain documents by filename/content keyword match.
    /// Phase 3b: upgrade to vector similarity via LanceDB.
    fn search_documents(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        let mut results = Vec::new();
        let query_lower = query.to_lowercase();
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
            for doc in &docs {
                let score = if doc.filename.to_lowercase().contains(&query_lower) {
                    0.8
                } else if query_lower
                    .split_whitespace()
                    .any(|w| doc.filename.to_lowercase().contains(w))
                {
                    0.5
                } else {
                    continue;
                };
                results.push(SearchResult {
                    id: format!("{}/{}", id, doc.filename),
                    label: format!("[{}] {}", domain.name, doc.filename),
                    snippet: format!(
                        "[{}] {} | {}B | {}",
                        domain.name,
                        doc.filename,
                        doc.size_bytes,
                        doc.modified.format("%Y-%m-%d")
                    ),
                    score,
                    source: "domain".into(),
                    metadata: serde_json::json!({
                        "domain_id": id,
                        "domain_name": domain.name,
                    }),
                });
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
}

impl everevo_core::retrieval::Retriever for DomainRetriever {
    fn name(&self) -> &str {
        "domain"
    }

    fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        self.search_documents(query, top_k)
    }
}
